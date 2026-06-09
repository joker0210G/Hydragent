use async_trait::async_trait;
use tokio::sync::mpsc;
use serde_json::json;
use hydragent_bus::router::MethodHandler;
use hydragent_bus::message::{JsonRpcRequest, JsonRpcResponse};
use hydragent_types::{IntentEvent, AgentResponse, ResponseFormat, MessageRole, ToolCallRecord};
use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_tools::registry::ToolRegistry;
use std::sync::Arc;
use tracing::{info, error};

use tokio::sync::oneshot;
use std::collections::HashMap;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct ActivePermissions {
    pub pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

pub struct IntentSubmitHandler {
    pub store: Arc<SessionStore>,
    pub model_router: Arc<ModelRouter>,
    pub registry: Arc<ToolRegistry>,
    pub max_react_steps: u8,
    pub active_permissions: ActivePermissions,
    pub gateway_router: Arc<hydragent_gateway::GatewayRouter>,
}

#[async_trait]
impl MethodHandler for IntentSubmitHandler {
    async fn handle(&self, request: JsonRpcRequest, response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let params_val = request.params.clone();
        let intent: IntentEvent = match serde_json::from_value(params_val) {
            Ok(evt) => evt,
            Err(e) => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: None,
                    error: Some(hydragent_bus::message::JsonRpcError {
                        code: hydragent_bus::message::ERR_INTERNAL,
                        message: format!("Invalid IntentEvent params: {}", e),
                        data: None,
                    }),
                    id: request.id,
                };
            }
        };

        // Check duplicate and rate limit via GatewayRouter
        if !self.gateway_router.inbound_check(&intent) {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_CONSENT_DENIED,
                    message: "Rate limit exceeded or duplicate message dropped".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        // Create or verify session and log user query
        if let Err(e) = self.store.create_session(&intent.session_id).await {
            error!("Failed to create/load session meta: {}", e);
        }
        if let Err(e) = self.store.append_message(&intent.session_id, MessageRole::User, &intent.content).await {
            error!("Failed to append user query: {}", e);
        }

        // Try to load context history
        let mut history_recalled = false;
        let mut history_count = 0;
        let mut history_messages = vec![];
        match self.store.load_recent(&intent.session_id, 20).await {
            Ok(history) => {
                history_count = history.len();
                info!("Loaded {} history messages for session {}", history_count, intent.session_id);
                
                // If there's previous messages (excluding the query we just appended), notify the client
                if history_count > 1 {
                    history_recalled = true;
                }
                history_messages = history;
            }
            Err(e) => {
                error!("Failed to load session history: {}", e);
            }
        }

        // Send memory recall notification to user if applicable
        if history_recalled {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "response.token",
                "params": {
                    "token": format!("[Recalled {} past messages from this Page's history]\n\n", history_count - 1)
                }
            });
            let _ = response_tx.send(serde_json::to_string(&notification).unwrap()).await;
        }

        // 1. Run hybrid search silently
        let retrieved_memories = match hydragent_memory::hybrid_search(&intent.content, 10, &self.store).await {
            Ok(mems) => {
                info!("Silently retrieved {} persistent semantic memories via hybrid search", mems.len());
                mems
            }
            Err(e) => {
                error!("Hybrid search error: {}", e);
                vec![]
            }
        };

        // Notify client if memories were retrieved silently
        if !retrieved_memories.is_empty() {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "response.status",
                "params": {
                    "status": format!("\n`[Injected {} facts from the Library's memory]`\n", retrieved_memories.len())
                }
            });
            let _ = response_tx.send(serde_json::to_string(&notification).unwrap()).await;
        }

        // Load standing orders silently
        let standing_orders = std::fs::read_to_string("./config/standing_orders.md").ok();

        let model_router = self.model_router.clone();
        let registry = self.registry.clone();
        let max_react_steps = self.max_react_steps;
        let session_id = intent.session_id.clone();
        let user_query = intent.content.clone();
        let response_tx_clone = response_tx.clone();

        let active_permissions = self.active_permissions.clone();

        // Spawn ReAct reasoning loop task
        let handle = tokio::spawn(async move {
            crate::react_loop::run_react_loop(
                &session_id,
                &user_query,
                history_messages,
                retrieved_memories,
                standing_orders,
                model_router,
                registry,
                max_react_steps,
                response_tx_clone,
                active_permissions,
            ).await
        });

        // Resolve ReAct reasoning loop completion output
        let (reply_text, executed_tools) = match handle.await {
            Ok(Ok((content, tools))) => {
                info!("Successfully completed ReAct reasoning loop");
                (content, tools)
            }
            Ok(Err(e)) => {
                error!("ReAct loop error: {}", e);
                (format!("Error: Failed to process request in reasoning loop. Details: {}", e), vec![])
            }
            Err(e) => {
                error!("ReAct loop task panicked: {}", e);
                (format!("Error: Reasoning loop task panicked."), vec![])
            }
        };

        // Send a complete message to signal completion
        let completion = json!({
            "jsonrpc": "2.0",
            "method": "response.complete",
            "params": {}
        });
        let _ = response_tx.send(serde_json::to_string(&completion).unwrap()).await;

        // Save assistant reply to SQLite memory
        if let Err(e) = self.store.append_message(&intent.session_id, MessageRole::Assistant, &reply_text).await {
            error!("Failed to save assistant response: {}", e);
        }

        // Convert ToolResults to ToolCallRecords for response
        let tool_records = executed_tools.into_iter().map(|tr| {
            ToolCallRecord {
                call_id: tr.call_id,
                tool_id: "".to_string(), // In ReAct loop we can populate tool_id or find it. Wait, tr has no tool_id field in ToolResult but we can pass it or use a default.
                params_hash: "".to_string(),
                status: tr.status,
                execution_ms: tr.execution_ms,
                timestamp: chrono::Utc::now().timestamp_millis(),
            }
        }).collect();

        let agent_response = AgentResponse {
            session_id: intent.session_id,
            content: reply_text,
            format: ResponseFormat::Markdown,
            consent_requests: vec![],
            tool_calls_executed: tool_records,
        };

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::to_value(agent_response).unwrap()),
            error: None,
            id: request.id,
        }
    }
}

pub struct PermissionRespondHandler {
    pub active_permissions: ActivePermissions,
}

#[async_trait]
impl MethodHandler for PermissionRespondHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let params_val = request.params.clone();
        let resp: hydragent_types::PermissionResponse = match serde_json::from_value(params_val) {
            Ok(r) => r,
            Err(e) => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: None,
                    error: Some(hydragent_bus::message::JsonRpcError {
                        code: hydragent_bus::message::ERR_INTERNAL,
                        message: format!("Invalid PermissionResponse: {}", e),
                        data: None,
                    }),
                    id: request.id,
                };
            }
        };
        let mut pending = self.active_permissions.pending.lock().await;
        if let Some(tx) = pending.remove(&resp.request_id) {
            let _ = tx.send(resp.approved);
        }
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(json!({"status": "ok"})),
            error: None,
            id: request.id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_active_permissions_flow() {
        let active_perms = ActivePermissions::default();
        let (tx, rx) = oneshot::channel::<bool>();
        let request_id = "test-req-id".to_string();

        {
            let mut pending = active_perms.pending.lock().await;
            pending.insert(request_id.clone(), tx);
        }

        let handler = PermissionRespondHandler {
            active_permissions: active_perms.clone(),
        };

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "permission.respond".to_string(),
            params: serde_json::json!({
                "request_id": request_id,
                "approved": true
            }),
            id: "1".to_string(),
        };

        let (resp_tx, _resp_rx) = mpsc::channel(1);
        let rpc_res = handler.handle(request, resp_tx).await;
        assert!(rpc_res.error.is_none());

        let approved = rx.await.unwrap();
        assert!(approved);
    }
}

pub struct EventBusChannelBridge {
    pub tx: mpsc::Sender<String>,
}

#[async_trait]
impl hydragent_gateway::ChannelAdapterBridge for EventBusChannelBridge {
    async fn send_response(&self, response: hydragent_types::AgentResponse) -> anyhow::Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "result": response,
            "id": "null"
        });
        self.tx.send(msg.to_string()).await.map_err(|e| anyhow::anyhow!("Failed to send outbound response: {}", e))
    }

    async fn send_push(&self, push: hydragent_types::PushMessage) -> anyhow::Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "gateway.push",
            "params": push
        });
        self.tx.send(msg.to_string()).await.map_err(|e| anyhow::anyhow!("Failed to send outbound push: {}", e))
    }
}

pub struct GatewayRegisterHandler {
    pub gateway_router: Arc<hydragent_gateway::GatewayRouter>,
}

#[async_trait]
impl MethodHandler for GatewayRegisterHandler {
    async fn handle(&self, request: JsonRpcRequest, response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let channel_id = request.params.get("channel_id").and_then(|c| c.as_str()).unwrap_or("").to_string();
        if channel_id.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing channel_id".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        let bridge = Arc::new(EventBusChannelBridge { tx: response_tx.clone() });
        self.gateway_router.register_adapter(channel_id, bridge);

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::json!({"status": "registered"})),
            error: None,
            id: request.id,
        }
    }
}

pub struct MemoryListHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for MemoryListHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        match self.store.list_memories().await {
            Ok(memories) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::to_value(memories).unwrap()),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to read memories: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct MemoryDeleteHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for MemoryDeleteHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let memory_id = request.params.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        if memory_id.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing id".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        match self.store.delete_memory(&memory_id).await {
            Ok(_) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "deleted"})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to delete memory: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct MemoryClearHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for MemoryClearHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        match self.store.clear_all_memories().await {
            Ok(_) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "cleared"})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to clear memories: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct LibraryNodeCreateHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for LibraryNodeCreateHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let id = request.params.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        let node_type = request.params.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let label = request.params.get("label").and_then(|l| l.as_str()).unwrap_or("").to_string();
        let properties = request.params.get("properties").map(|p| p.to_string());

        if id.is_empty() || node_type.is_empty() || label.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing id, type, or label".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        match self.store.create_node(&id, &node_type, &label, properties.as_deref()).await {
            Ok(_) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "created", "id": id})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to create node: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct LibraryLinkHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for LibraryLinkHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let edge_id = request.params.get("edge_id").and_then(|i| i.as_str()).map(|s| s.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let source = request.params.get("source").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let target = request.params.get("target").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let relation = request.params.get("relation").and_then(|r| r.as_str()).unwrap_or("").to_string();
        let weight = request.params.get("weight").and_then(|w| w.as_f64()).unwrap_or(1.0);

        if source.is_empty() || target.is_empty() || relation.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing source, target, or relation".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        match self.store.link_nodes(&edge_id, &source, &target, &relation, weight).await {
            Ok(_) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "linked", "edge_id": edge_id})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to link nodes: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct LibrarySearchHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for LibrarySearchHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let start_node = request.params.get("start_node").and_then(|s| s.as_str()).unwrap_or("").to_string();
        if start_node.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing start_node".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        match self.store.search_graph(&start_node).await {
            Ok(graph_data) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(graph_data),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to search graph: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct LibraryNodeListHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for LibraryNodeListHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let node_type = request.params.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if node_type.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing type".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        match self.store.list_nodes_by_type(&node_type).await {
            Ok(nodes_data) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(nodes_data),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to list nodes: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct LibraryNodeDeleteHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for LibraryNodeDeleteHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let id = request.params.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing id".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }

        match self.store.delete_node(&id).await {
            Ok(_) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "deleted", "id": id})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to delete node: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

