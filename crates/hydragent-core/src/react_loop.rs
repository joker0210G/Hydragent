use std::sync::Arc;
use tokio::sync::mpsc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn, error};

use hydragent_types::{Message, MessageRole, ToolCall, ToolResult, ToolStatus};
use hydragent_model::router::ModelRouter;
use hydragent_model::openrouter::ChatMessage as ModelChatMessage;
use hydragent_tools::registry::ToolRegistry;

#[derive(Serialize, Deserialize, Debug)]
pub struct ReActStepResponse {
    pub thought: Option<String>,
    pub tool: Option<String>,
    pub params: Option<Value>,
    pub answer: Option<String>,
}

async fn send_status(tx: &mpsc::Sender<String>, status: String) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "response.status",
        "params": {
            "status": status
        }
    });
    let _ = tx.send(msg.to_string()).await;
}

async fn send_token(tx: &mpsc::Sender<String>, token: String) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "response.token",
        "params": {
            "token": token
        }
    });
    let _ = tx.send(msg.to_string()).await;
}

pub async fn run_react_loop(
    session_id: &str,
    user_query: &str,
    history: Vec<Message>,
    retrieved_memories: Vec<hydragent_types::MemoryDocument>,
    standing_orders: Option<String>,
    model_router: Arc<ModelRouter>,
    registry: Arc<ToolRegistry>,
    max_steps: u8,
    response_tx: mpsc::Sender<String>,
    active_permissions: crate::orchestrator::ActivePermissions,
) -> anyhow::Result<(String, Vec<ToolResult>)> {
    let mut system_prompt = format!(
        "You are Hydra, an advanced agentic AI assistant. You solve problems step-by-step using a ReAct loop.\n\
        You must respond with a single JSON object. DO NOT wrap it in markdown block unless required, and DO NOT output anything else.\n\n\
        Your JSON response must follow one of these two schemas:\n\n\
        1. To call a tool:\n\
        {{\n\
          \"thought\": \"your step-by-step reasoning about what to do next\",\n\
          \"tool\": \"tool_name\",\n\
          \"params\": {{ ... key-value parameters for the tool ... }}\n\
        }}\n\n\
        2. To provide the final answer to the user:\n\
        {{\n\
          \"thought\": \"your final reasoning summary\",\n\
          \"answer\": \"your detailed markdown response to the user\"\n\
        }}\n\n\
        Available Tools:\n\
        {}\n\n\
        IMPORTANT: Only use the tools listed above. Always output valid JSON.",
        registry.build_system_prompt_block()
    );

    // Prepend persistent Standing Orders if present
    if let Some(so) = standing_orders {
        if !so.trim().is_empty() {
            system_prompt = format!(
                "# Persistent Standing Orders\n{}\n\n{}",
                so, system_prompt
            );
        }
    }

    // Apply persistent memory context injection if available
    let max_memory_tokens = std::env::var("MEMORY_CONTEXT_TOKEN_LIMIT")
        .unwrap_or_else(|_| "1000".to_string())
        .parse::<usize>()
        .unwrap_or(1000);

    system_prompt = hydragent_memory::build_system_prompt_with_memory(
        &system_prompt,
        &retrieved_memories,
        max_memory_tokens,
    );

    // Initial message stream starts with system prompt and history
    let mut messages = vec![ModelChatMessage {
        role: "system".to_string(),
        content: system_prompt,
    }];

    for msg in history {
        let role = match msg.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        };
        messages.push(ModelChatMessage {
            role: role.to_string(),
            content: msg.content,
        });
    }

    // Add current user query if it's not already at the end of history
    let last_content_is_query = messages.last().map(|m| m.content == user_query).unwrap_or(false);
    if !last_content_is_query {
        messages.push(ModelChatMessage {
            role: "user".to_string(),
            content: user_query.to_string(),
        });
    }

    let mut executed_tools = Vec::new();
    let mut step = 0;

    while step < max_steps {
        step += 1;
        info!(step, "Starting ReAct step");

        // Send a token indicating thinking is in progress
        send_status(&response_tx, format!("\n`[Thinking (Step {}/{})]`...\n", step, max_steps)).await;

        let (token_tx, mut token_rx) = mpsc::channel(100);
        let model_router_clone = model_router.clone();
        let messages_clone = messages.clone();

        let handle = tokio::spawn(async move {
            model_router_clone.chat_stream(messages_clone, token_tx).await
        });

        // We can optionally stream the thinking tokens or read them
        let mut raw_response = String::new();
        while let Some(token) = token_rx.recv().await {
            raw_response.push_str(&token);
        }

        let model_res = match handle.await {
            Ok(Ok((content, _model))) => content,
            Ok(Err(e)) => {
                error!("ReAct step LLM error: {}", e);
                return Err(e);
            }
            Err(e) => {
                error!("ReAct step LLM panic: {}", e);
                return Err(anyhow::anyhow!("ReAct step LLM task panicked: {}", e));
            }
        };

        if model_res.trim().is_empty() {
            warn!("LLM returned empty completion response.");
            return Err(anyhow::anyhow!("LLM returned empty completion response"));
        }

        info!(?model_res, "LLM raw response received");

        // Parse JSON step response
        let parsed = match parse_react_response(&model_res) {
            Ok(p) => p,
            Err(e) => {
                // If it's not JSON, but has no curly braces, we fallback to treating the entire response as the final answer.
                if !model_res.contains('{') && !model_res.contains('}') {
                    info!("LLM returned raw text instead of JSON. Fallback: treating as final answer.");
                    send_token(&response_tx, format!("\n{}", model_res)).await;
                    return Ok((model_res, executed_tools));
                }

                warn!("Failed to parse ReAct step response: {}. Raw was: {}. Retrying step.", e, model_res);
                // Prompt model to correct format
                messages.push(ModelChatMessage {
                    role: "assistant".to_string(),
                    content: model_res.clone(),
                });
                messages.push(ModelChatMessage {
                    role: "user".to_string(),
                    content: format!("Your response was not valid JSON: {}. Please retry and output only the valid JSON structure.", e),
                });
                continue;
            }
        };

        if let Some(thought) = &parsed.thought {
            info!(?thought, "Step thought");
            send_status(&response_tx, format!("\n`[Thought]` {}\n", thought)).await;
        }

        if let Some(tool_name) = &parsed.tool {
            let params = parsed.params.unwrap_or(json!({}));
            let params_str = serde_json::to_string(&params).unwrap_or_default();
            
            send_status(&response_tx, format!("`[Calling tool]` **{}** with params `{}`\n", tool_name, params_str)).await;

            let call_id = uuid::Uuid::new_v4().to_string();
            let tier = registry.get_tier(tool_name);
            
            let tool_call = ToolCall {
                call_id: call_id.clone(),
                tool_id: tool_name.clone(),
                params_json: params_str.clone(),
                tier,
            };

            let tool_result = match tier {
                hydragent_types::PermissionTier::AutoApprove => {
                    registry.invoke(&tool_call).await
                }
                hydragent_types::PermissionTier::Deny => {
                    ToolResult {
                        call_id: call_id.clone(),
                        output_json: "{}".to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: 0,
                        error_message: Some("Permission denied by static policy".to_string()),
                    }
                }
                hydragent_types::PermissionTier::Prompt => {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    let expires_at_ms = chrono::Utc::now().timestamp_millis() + 30000;
                    
                    let permission_request = hydragent_types::PermissionRequest {
                        request_id: request_id.clone(),
                        session_id: session_id.to_string(),
                        tool_id: tool_name.clone(),
                        params_summary: format!("Executing tool '{}' with parameters: {}", tool_name, params_str),
                        tier,
                        expires_at_ms,
                    };
                    
                    let request_msg = json!({
                        "jsonrpc": "2.0",
                        "method": "response.permission_request",
                        "params": permission_request
                    });
                    
                    let _ = response_tx.send(request_msg.to_string()).await;
                    
                    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
                    {
                        let mut pending = active_permissions.pending.lock().await;
                        pending.insert(request_id.clone(), tx);
                    }
                    
                    let approved = match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
                        Ok(Ok(approved)) => approved,
                        Ok(Err(_)) => false,
                        Err(_) => {
                            let mut pending = active_permissions.pending.lock().await;
                            pending.remove(&request_id);
                            false
                        }
                    };
                    
                    if approved {
                        registry.invoke(&tool_call).await
                    } else {
                        ToolResult {
                            call_id: call_id.clone(),
                            output_json: "{}".to_string(),
                            status: ToolStatus::Failure,
                            execution_ms: 0,
                            error_message: Some("Permission denied by user".to_string()),
                        }
                    }
                }
            };

            info!(?tool_result, "Tool result");

            send_status(&response_tx, format!("`[Tool Result]` Status: {:?}\n", tool_result.status)).await;

            executed_tools.push(tool_result.clone());

            // Add the assistant's turn and the tool's result to the message log
            messages.push(ModelChatMessage {
                role: "assistant".to_string(),
                content: model_res,
            });

            // Feed observation back to LLM
            messages.push(ModelChatMessage {
                role: "user".to_string(),
                content: format!(
                    "Observation from tool '{}': {}",
                    tool_name,
                    if tool_result.status == ToolStatus::Success {
                        tool_result.output_json.clone()
                    } else {
                        tool_result.error_message.clone().unwrap_or_else(|| "Unknown tool failure".to_string())
                    }
                ),
            });

        } else if let Some(answer) = parsed.answer {
            info!("Final answer found");
            // Stream the final answer content so adapter receives it
            send_token(&response_tx, format!("\n{}", answer)).await;
            return Ok((answer, executed_tools));
        } else {
            // Neither tool nor answer, prompt model to make a decision
            messages.push(ModelChatMessage {
                role: "assistant".to_string(),
                content: model_res,
            });
            messages.push(ModelChatMessage {
                role: "user".to_string(),
                content: "You did not specify a 'tool' to call or an 'answer' to finish. Please choose one and respond.".to_string(),
            });
        }
    }

    Err(anyhow::anyhow!("ReAct loop exceeded maximum steps ({}) without generating a final answer.", max_steps))
}

fn parse_react_response(raw: &str) -> anyhow::Result<ReActStepResponse> {
    let mut cleaned = raw.trim();
    if cleaned.starts_with("```json") {
        cleaned = cleaned.strip_prefix("```json").unwrap_or(cleaned);
    } else if cleaned.starts_with("```") {
        cleaned = cleaned.strip_prefix("```").unwrap_or(cleaned);
    }
    if cleaned.ends_with("```") {
        cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned);
    }
    let cleaned = cleaned.trim();

    let start_idx = cleaned.find('{').ok_or_else(|| anyhow::anyhow!("No JSON object found (missing '{{')"))?;
    let end_idx = cleaned.rfind('}').ok_or_else(|| anyhow::anyhow!("No JSON object found (missing '}}')"))?;
    let json_sub = &cleaned[start_idx..=end_idx];

    let parsed: ReActStepResponse = serde_json::from_str(json_sub)?;
    Ok(parsed)
}
