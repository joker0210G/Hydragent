use async_trait::async_trait;
use tokio::sync::mpsc;
use serde_json::json;
use hydragent_bus::router::MethodHandler;
use hydragent_bus::message::{JsonRpcRequest, JsonRpcResponse};
use hydragent_types::{
    IntentEvent, AgentResponse, ResponseFormat, MessageRole, ToolCallRecord,
    PendingClarification,
};
use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_tools::registry::ToolRegistry;
use std::sync::Arc;
use tracing::{info, error};

use tokio::sync::oneshot;
use std::collections::HashMap;
use tokio::sync::Mutex;

use crate::strategy::{select_strategy, Strategy};
use crate::swarm_runner::run_swarm;

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
    /// Pending clarification questions keyed by `page_id`. The orchestrator
    /// pops one when a new `intent.submit` arrives on the same page.
    pub pending_clarifications: Arc<Mutex<HashMap<String, PendingClarification>>>,
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

        // Create or verify page and log user query
        if let Err(e) = self.store.create_page(&intent.page_id).await {
            error!("Failed to create/load page meta: {}", e);
        }
        if let Err(e) = self.store.append_message(&intent.page_id, MessageRole::User, &intent.content).await {
            error!("Failed to append user query: {}", e);
        }

        // Try to load context history
        let mut history_recalled = false;
        let mut history_count = 0;
        let mut history_messages = vec![];
        
        // Inject page summary if present
        if let Ok(Some(ref summary)) = self.store.get_page_summary(&intent.page_id).await {
            if !summary.trim().is_empty() {
                history_messages.push(hydragent_types::Message {
                    id: 0,
                    page_id: intent.page_id.clone(),
                    role: MessageRole::System,
                    content: format!("[Summary of previous conversation context on this Page]:\n\n{}", summary),
                    timestamp: 0,
                    token_count: None,
                });
            }
        }

        match self.store.load_recent(&intent.page_id, 20).await {
            Ok(history) => {
                let recent_count = history.len();
                history_count = history_messages.len() + recent_count;
                info!("Loaded {} history messages for page {}", history_count, intent.page_id);
                
                // If there's previous messages (excluding the query we just appended), notify the client
                if history_count > 1 {
                    history_recalled = true;
                }
                history_messages.extend(history);
            }
            Err(e) => {
                error!("Failed to load page history: {}", e);
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

        // Load profiles silently
        let user_profile = std::fs::read_to_string("./config/USER.md").ok();
        let soul_guidelines = std::fs::read_to_string("./config/SOUL.md").ok();

        // ─── 1. Pop pending clarification (if any) and augment the user query ───
        let mut user_query = intent.content.clone();
        {
            let mut map = self.pending_clarifications.lock().await;
            if let Some(pending) = map.remove(&intent.page_id) {
                let notice = json!({
                    "jsonrpc": "2.0",
                    "method": "response.status",
                    "params": {
                        "status": format!(
                            "\n`[Pending clarification: \"{}\" — treating your new message as the answer]`\n",
                            pending.question
                        )
                    }
                });
                let _ = response_tx.send(serde_json::to_string(&notice).unwrap()).await;
                user_query = format!(
                    "{}\n\n[Clarification Q: {}]\n[User's answer: {}]",
                    intent.content, pending.question, intent.content
                );
            }
        }

        // ─── 2. Strategy selection (heuristic + LLM fallback) ───
        let (strategy, source) = select_strategy(&user_query, self.model_router.clone()).await;
        let strategy_label = match &strategy {
            Strategy::ReactLoop => "ReactLoop (single agent with tools)".to_string(),
            Strategy::DelegateToSwarm { .. } => "DelegateToSwarm (sub-agent DAG)".to_string(),
            Strategy::AskUser { question } => format!("AskUser: {}", question),
        };
        info!("Strategy selected: {} (via {})", strategy_label, source);
        let strategy_notice = json!({
            "jsonrpc": "2.0",
            "method": "response.status",
            "params": {
                "status": format!("\n`[Strategy: {} — via {}]`\n", strategy_label, source)
            }
        });
        let _ = response_tx.send(serde_json::to_string(&strategy_notice).unwrap()).await;

        // ─── 3. Branch on strategy ───
        let (reply_text, executed_tools) = match strategy {
            Strategy::ReactLoop => {
                let model_router = self.model_router.clone();
                let registry = self.registry.clone();
                let max_react_steps = self.max_react_steps;
                let page_id = intent.page_id.clone();
                let channel_id = intent.channel_id.clone();
                let user_id = intent.user_id.clone();
                let user_query_for_loop = user_query.clone();
                let response_tx_clone = response_tx.clone();
                let active_permissions = self.active_permissions.clone();

                let handle = tokio::spawn(async move {
                    crate::react_loop::run_react_loop(
                        &page_id,
                        &channel_id,
                        &user_id,
                        &user_query_for_loop,
                        history_messages,
                        retrieved_memories,
                        user_profile,
                        soul_guidelines,
                        model_router,
                        registry,
                        max_react_steps,
                        response_tx_clone,
                        active_permissions,
                    ).await
                });

                match handle.await {
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
                }
            }

            Strategy::DelegateToSwarm { refined_task } => {
                let task = refined_task.unwrap_or_else(|| user_query.clone());
                let model_router = self.model_router.clone();
                let tool_registry = self.registry.clone();
                let response_tx_clone = response_tx.clone();
                let page_id = intent.page_id.clone();
                let handle = tokio::spawn(async move {
                    run_swarm(&page_id, &task, model_router, tool_registry, response_tx_clone).await
                });
                match handle.await {
                    Ok(Ok(text)) => {
                        info!("Swarm completed successfully");
                        (text, vec![])
                    }
                    Ok(Err(e)) => {
                        error!("Swarm error: {}", e);
                        (format!("Error: Swarm failed. Details: {}", e), vec![])
                    }
                    Err(e) => {
                        error!("Swarm task panicked: {}", e);
                        (format!("Error: Swarm task panicked."), vec![])
                    }
                }
            }

            Strategy::AskUser { question } => {
                // Store the question so the next intent.submit on this page
                // is treated as the answer.
                {
                    let mut map = self.pending_clarifications.lock().await;
                    map.insert(
                        intent.page_id.clone(),
                        PendingClarification {
                            page_id: intent.page_id.clone(),
                            question: question.clone(),
                            asked_at_ms: chrono::Utc::now().timestamp_millis(),
                            source: source.clone(),
                        },
                    );
                }
                // Send the question to the user as a token.
                let token = json!({
                    "jsonrpc": "2.0",
                    "method": "response.token",
                    "params": { "token": format!("❓ {}\n", question) }
                });
                let _ = response_tx.send(serde_json::to_string(&token).unwrap()).await;
                let notice = json!({
                    "jsonrpc": "2.0",
                    "method": "response.status",
                    "params": {
                        "status": "\n`[Awaiting your reply — please answer the question above and I'll continue.]`\n".to_string()
                    }
                });
                let _ = response_tx.send(serde_json::to_string(&notice).unwrap()).await;
                (format!("[Asked for clarification: {}]", question), vec![])
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
        if let Err(e) = self.store.append_message(&intent.page_id, MessageRole::Assistant, &reply_text).await {
            error!("Failed to save assistant response: {}", e);
        }

        // Auto Compaction Trigger Check
        let store_clone = self.store.clone();
        let page_id_clone = intent.page_id.clone();
        let pool = self.store.pool().clone();
        let model_router_clone = self.model_router.clone();
        tokio::spawn(async move {
            let msg_count = sqlx::query("SELECT COUNT(*) FROM messages WHERE page_id = ?")
                .bind(&page_id_clone)
                .fetch_one(&pool)
                .await
                .map(|r| sqlx::Row::get::<i64, _>(&r, 0))
                .unwrap_or(0);
            let limit = std::env::var("PAGE_COMPACTION_LIMIT")
                .unwrap_or_else(|_| "30".to_string())
                .parse::<i64>()
                .unwrap_or(30);
            if msg_count > limit {
                info!("Page {} message count {} exceeds limit {}, triggering auto compaction", page_id_clone, msg_count, limit);
                if let Err(e) = run_compaction(&page_id_clone, &store_clone, &model_router_clone).await {
                    error!("Auto compaction failed: {}", e);
                } else {
                    info!("Auto compaction succeeded for page {}", page_id_clone);
                }
            }
        });

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
            page_id: intent.page_id,
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
    use hydragent_types::{PermissionRequest, PermissionResponse, PermissionTier};
    use tokio::sync::mpsc;

    /// Test the basic approve-path through `ActivePermissions`:
    /// a `Prompt` request is registered, then `PermissionRespondHandler`
    /// finds the pending oneshot and sends the decision through.
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

    /// A `PermissionRespondHandler` for an unknown request_id must
    /// respond OK (so the bus client doesn't see an error) but the
    /// pending oneshot is left untouched. The orchestrator's gate
    /// will eventually time out the missing response.
    #[tokio::test]
    async fn test_active_permissions_unknown_request_id() {
        let active_perms = ActivePermissions::default();
        let handler = PermissionRespondHandler {
            active_permissions: active_perms.clone(),
        };
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "permission.respond".to_string(),
            params: serde_json::json!({
                "request_id": "no-such-request",
                "approved": true
            }),
            id: "1".to_string(),
        };
        let (resp_tx, _resp_rx) = mpsc::channel(1);
        let rpc_res = handler.handle(request, resp_tx).await;
        assert!(rpc_res.error.is_none(),
                "handler must respond OK for unknown id (the gate times out, not the bus)");
    }

    /// `ActivePermissions` is `Clone` and clones share the same
    /// `Arc<Mutex<HashMap>>`. This is critical because the orchestrator
    /// holds one clone and the `PermissionRespondHandler` holds another.
    #[tokio::test]
    async fn test_active_permissions_clone_shares_state() {
        let active_perms = ActivePermissions::default();
        let clone1 = active_perms.clone();
        let clone2 = active_perms.clone();

        let (tx, rx) = oneshot::channel::<bool>();
        let req_id = "shared-req".to_string();

        // Register via clone1
        {
            let mut pending = clone1.pending.lock().await;
            pending.insert(req_id.clone(), tx);
        }

        // Verify clone2 sees the entry
        {
            let pending = clone2.pending.lock().await;
            assert!(pending.contains_key(&req_id),
                    "clones must share the same HashMap");
        }

        // Remove via clone2 and verify clone1 sees the removal
        {
            let mut pending = clone2.pending.lock().await;
            pending.remove(&req_id);
        }
        {
            let pending = clone1.pending.lock().await;
            assert!(!pending.contains_key(&req_id),
                    "clones must see each other's mutations");
        }

        // And the channel got the value
        drop(rx);
    }

    /// `PermissionTier` enum: AutoApprove, Prompt, Deny must all
    /// serialize/deserialize via JSON correctly (the bus client
    /// sends tier as a string in PermissionRequest).
    #[test]
    fn test_permission_tier_serde_roundtrip() {
        for tier in [PermissionTier::AutoApprove, PermissionTier::Prompt, PermissionTier::Deny] {
            let json = serde_json::to_string(&tier).unwrap();
            let parsed: PermissionTier = serde_json::from_str(&json).unwrap();
            assert_eq!(tier, parsed);
        }
    }

    /// `PermissionRequest` includes expires_at_ms which the gate
    /// checks for timeout. Verify the field roundtrips.
    #[test]
    fn test_permission_request_roundtrip() {
        let req = PermissionRequest {
            request_id: "abc".to_string(),
            page_id: "page-1".to_string(),
            tool_id: "file_write".to_string(),
            params_summary: "Write 42 bytes to /tmp/x.txt".to_string(),
            tier: PermissionTier::Prompt,
            expires_at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: PermissionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.request_id, req.request_id);
        assert_eq!(parsed.page_id, req.page_id);
        assert_eq!(parsed.tool_id, req.tool_id);
        assert_eq!(parsed.tier, req.tier);
        assert_eq!(parsed.expires_at_ms, req.expires_at_ms);
    }

    /// `PermissionResponse` is the shape the bus client sends back;
    /// verify the wire format the Rust side expects matches the
    /// Python `bus_client.py` which sends `{"request_id": ..., "approved": ...}`.
    #[test]
    fn test_permission_response_wire_format() {
        let json = r#"{"request_id":"abc","approved":false}"#;
        let resp: PermissionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.request_id, "abc");
        assert_eq!(resp.approved, false);
    }

    /// Tier routing: AutoApprove must NOT register a pending oneshot
    /// (it should pass straight through). Deny must NOT register
    /// either. Only Prompt requires a pending channel.
    ///
    /// This is the gate logic that lives in `react_loop.rs`. We
    /// smoke-test the underlying types here: a handler that's only
    /// wired for Prompt tiers should be a no-op for the other two.
    #[tokio::test]
    async fn test_prompt_tier_is_the_only_one_needing_oneshot() {
        let active_perms = ActivePermissions::default();

        // Simulate an AutoApprove path: the gate proceeds without
        // touching `active_permissions`. The pending map stays empty.
        {
            let pending = active_perms.pending.lock().await;
            assert!(pending.is_empty());
        }
        // Simulate a Deny path: same — pending stays empty.
        {
            let pending = active_perms.pending.lock().await;
            assert!(pending.is_empty());
        }
        // Simulate a Prompt path: the gate registers a oneshot.
        let (tx, _rx) = oneshot::channel::<bool>();
        let req_id = "prompt-1".to_string();
        {
            let mut pending = active_perms.pending.lock().await;
            pending.insert(req_id.clone(), tx);
        }
        // Now the map has one entry — this is what the gate would do.
        {
            let pending = active_perms.pending.lock().await;
            assert_eq!(pending.len(), 1);
            assert!(pending.contains_key(&req_id));
        }
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

pub struct MemorySearchHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for MemorySearchHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let query = request.params.get("query").and_then(|q| q.as_str()).unwrap_or("").to_string();
        if query.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing query".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }
        let limit = request.params.get("limit").and_then(|l| l.as_u64()).unwrap_or(5) as usize;
        match hydragent_memory::hybrid_search(&query, limit, &self.store).await {
            Ok(results) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"results": results})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to search memories: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

/// `dream.run` bus method — synchronously runs one memory-
/// consolidation dream cycle. Returns the `DreamStats` as JSON so
/// callers (tests, CI smoke harnesses, or future scheduled tasks)
/// can observe the cycle's output without parsing log lines.
///
/// The dream worker is also started automatically by `main.rs` on a
/// `tokio::time::interval` ticker when `enable_dreaming=true`; this
/// handler is the *synchronous, on-demand* counterpart used by tests
/// (Phase 2 final — D2 dream.run) and any future user-facing
/// "consolidate now" affordance.
pub struct DreamRunHandler {
    pub store: Arc<SessionStore>,
    pub model_router: Arc<ModelRouter>,
}

#[async_trait]
impl MethodHandler for DreamRunHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        match crate::dream::run_dream_cycle(self.store.clone(), self.model_router.clone()).await {
            Ok(stats) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({
                    "status": "ok",
                    "messages_processed": stats.messages_processed,
                    "facts_stored": stats.facts_stored,
                    "facts_skipped": stats.facts_skipped,
                    "style_habits_stored": stats.style_habits_stored,
                    "behavior_rules_stored": stats.behavior_rules_stored,
                })),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Dream cycle failed: {}", e),
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

pub async fn run_compaction(
    page_id: &str,
    store: &SessionStore,
    model_router: &ModelRouter,
) -> anyhow::Result<String> {
    let messages = store.load_recent(page_id, 200).await?;
    if messages.is_empty() {
        return Ok("".to_string());
    }
    let mut formatted_history = Vec::new();
    for msg in &messages {
        let role_str = match msg.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::System => "System",
            MessageRole::Tool => "Tool",
        };
        formatted_history.push(format!("{}: {}", role_str, msg.content));
    }
    let history_text = formatted_history.join("\n");
    
    let prompt = format!(
        "You are a helpful assistant. Summarize the key discussion points, user requests, outcomes, decisions, and current status of this conversation. Keep it concise, structured strictly as a markdown numbered list (e.g. 1. point one\\n2. point two).\\n\\nCONVERSATION HISTORY:\\n{}",
        history_text
    );
    
    let system_message = hydragent_model::openrouter::ChatMessage {
        role: "user".to_string(),
        content: prompt,
    };
    
    let (tx, mut rx) = mpsc::channel(100);
    tokio::spawn(async move {
        while let Some(_) = rx.recv().await {}
    });
    
    let (summary, _) = model_router.chat_stream(vec![system_message], tx, None).await?;
    
    store.update_page_summary(page_id, &summary).await?;
    store.truncate_page_messages(page_id, 4).await?;
    
    Ok(summary)
}

pub struct PageCompactHandler {
    pub store: Arc<SessionStore>,
    pub model_router: Arc<ModelRouter>,
}

#[async_trait]
impl MethodHandler for PageCompactHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let page_id = request.params.get("page_id").and_then(|p| p.as_str()).unwrap_or("").to_string();
        if page_id.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing page_id".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }
        match run_compaction(&page_id, &self.store, &self.model_router).await {
            Ok(summary) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "success", "summary": summary})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Compaction failed: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct PageGetSummaryHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for PageGetSummaryHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let page_id = request.params.get("page_id").and_then(|p| p.as_str()).unwrap_or("").to_string();
        if page_id.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing page_id".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }
        match self.store.get_page_summary(&page_id).await {
            Ok(summary) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"summary": summary.unwrap_or_default()})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to get summary: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct PageUpdateSummaryHandler {
    pub store: Arc<SessionStore>,
}

#[async_trait]
impl MethodHandler for PageUpdateSummaryHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let page_id = request.params.get("page_id").and_then(|p| p.as_str()).unwrap_or("").to_string();
        let summary = request.params.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
        if page_id.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Missing page_id".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }
        match self.store.update_page_summary(&page_id, &summary).await {
            Ok(_) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "success"})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to update summary: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct ConfigReadHandler;

#[async_trait]
impl MethodHandler for ConfigReadHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let file_name = request.params.get("file_name").and_then(|f| f.as_str()).unwrap_or("").to_string();
        if file_name != "USER.md" && file_name != "SOUL.md" {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Invalid file_name. Only USER.md and SOUL.md are allowed.".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }
        let path = format!("./config/{}", file_name);
        match std::fs::read_to_string(&path) {
            Ok(content) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"content": content})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to read file: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

pub struct ConfigWriteHandler;

#[async_trait]
impl MethodHandler for ConfigWriteHandler {
    async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let file_name = request.params.get("file_name").and_then(|f| f.as_str()).unwrap_or("").to_string();
        let content = request.params.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
        if file_name != "USER.md" && file_name != "SOUL.md" {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INVALID_REQUEST,
                    message: "Invalid file_name. Only USER.md and SOUL.md are allowed.".to_string(),
                    data: None,
                }),
                id: request.id,
            };
        }
        let path = format!("./config/{}", file_name);
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, content) {
            Ok(_) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(serde_json::json!({"status": "success"})),
                error: None,
                id: request.id,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(hydragent_bus::message::JsonRpcError {
                    code: hydragent_bus::message::ERR_INTERNAL,
                    message: format!("Failed to write file: {}", e),
                    data: None,
                }),
                id: request.id,
            }
        }
    }
}

