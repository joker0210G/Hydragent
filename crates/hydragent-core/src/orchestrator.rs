use async_trait::async_trait;
use tokio::sync::mpsc;
use serde_json::json;
use hydragent_bus::router::MethodHandler;
use hydragent_bus::message::{JsonRpcRequest, JsonRpcResponse};
use hydragent_types::{IntentEvent, AgentResponse, ResponseFormat, MessageRole};
use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_model::openrouter::ChatMessage as ModelChatMessage;
use std::sync::Arc;
use tracing::{info, error};

pub struct IntentSubmitHandler {
    pub store: Arc<SessionStore>,
    pub model_router: Arc<ModelRouter>,
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

        // Create or verify session and log user query
        if let Err(e) = self.store.create_session(&intent.session_id).await {
            error!("Failed to create/load session meta: {}", e);
        }
        if let Err(e) = self.store.append_message(&intent.session_id, MessageRole::User, &intent.content).await {
            error!("Failed to append user query: {}", e);
        }

        // Initialize message list starting with system prompt
        let system_prompt = "You are Hydra, a helpful and precise AI agent. Keep your responses concise and format them as Markdown.";
        let mut llm_messages = vec![ModelChatMessage {
            role: "system".to_string(),
            content: system_prompt.to_string(),
        }];

        // Try to load context history and log it
        let mut history_recalled = false;
        let mut history_count = 0;
        match self.store.load_recent(&intent.session_id, 20).await {
            Ok(history) => {
                history_count = history.len();
                info!("Loaded {} history messages for session {}", history_count, intent.session_id);
                
                // If there's previous messages (excluding the query we just appended), notify the client
                if history_count > 1 {
                    history_recalled = true;
                }

                // Add all historical messages to the LLM message history
                for msg in history {
                    let role = match msg.role {
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                        MessageRole::System => "system",
                        MessageRole::Tool => "tool",
                    };
                    llm_messages.push(ModelChatMessage {
                        role: role.to_string(),
                        content: msg.content,
                    });
                }
            }
            Err(e) => {
                error!("Failed to load session history: {}", e);
                // Fallback to adding the current query if DB fetch failed
                llm_messages.push(ModelChatMessage {
                    role: "user".to_string(),
                    content: intent.content.clone(),
                });
            }
        }

        // Send memory recall notification to user if applicable
        if history_recalled {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "response.token",
                "params": {
                    "token": format!("[Recalled {} past messages from SQLite history]\n\n", history_count - 1)
                }
            });
            let _ = response_tx.send(serde_json::to_string(&notification).unwrap()).await;
        }

        // Create streaming channels for ModelRouter to pass tokens
        let (token_tx, mut token_rx) = mpsc::channel(100);
        let model_router = self.model_router.clone();

        // Spawn LLM call in a background task
        let handle = tokio::spawn(async move {
            model_router.chat_stream(llm_messages, token_tx).await
        });

        // Forward tokens from token_rx down to the event bus client (response_tx)
        while let Some(token) = token_rx.recv().await {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "response.token",
                "params": {
                    "token": token
                }
            });
            let _ = response_tx.send(serde_json::to_string(&notification).unwrap()).await;
        }

        // Resolve LLM completion output
        let reply_text = match handle.await {
            Ok(Ok((content, model_used))) => {
                info!("Successfully generated response using model: {}", model_used);
                content
            }
            Ok(Err(e)) => {
                error!("Model router error: {}", e);
                format!("Error: Failed to generate response from AI models. Details: {}", e)
            }
            Err(e) => {
                error!("Model router task panicked: {}", e);
                format!("Error: AI model completion task panicked.")
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

        let agent_response = AgentResponse {
            session_id: intent.session_id,
            content: reply_text,
            format: ResponseFormat::Markdown,
            consent_requests: vec![],
            tool_calls_executed: vec![],
        };

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::to_value(agent_response).unwrap()),
            error: None,
            id: request.id,
        }
    }
}
