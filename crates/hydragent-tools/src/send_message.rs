use async_trait::async_trait;
use hydragent_types::{PermissionTier, ToolResult, ToolStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;

#[derive(Serialize, Deserialize)]
struct SendMessageParams {
    channel_id: String,
    page_id: Option<String>,
    content: String,
}

pub struct SendMessageTool {
    push_fn: Arc<dyn Fn(String, String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>,
}

impl SendMessageTool {
    pub fn new<F>(push_fn: F) -> Self 
    where
        F: Fn(String, String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync + 'static,
    {
        Self {
            push_fn: Arc::new(push_fn),
        }
    }
}

#[async_trait]
impl crate::tool_trait::Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Push a proactive notification to an EXTERNAL channel or user (e.g. Telegram, webhook). \
         Use ONLY when you need to notify someone on a DIFFERENT channel after completing a background task. \
         DO NOT use this to reply to the user you are currently talking to — \
         simply provide your answer in the 'answer' field of your JSON response instead. \
         Requires a valid channel_id like 'telegram:123456789'."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "channel_id": {
                    "type": "string",
                    "description": "Target channel/user identifier (e.g. 'telegram:123456789')"
                },
                "page_id": {
                    "type": "string",
                    "description": "Optional page ID context"
                },
                "content": {
                    "type": "string",
                    "description": "Content of the message"
                }
            },
            "required": ["channel_id", "content"]
        }"#
    }

    fn permission_tier(&self) -> PermissionTier {
        PermissionTier::AutoApprove
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let params: SendMessageParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        let page_id = params.page_id.unwrap_or_else(|| "general-page".to_string());
        match (self.push_fn)(params.channel_id, page_id, params.content).await {
            Ok(_) => ToolResult {
                call_id: "".to_string(),
                output_json: r#"{"status":"delivered"}"#.to_string(),
                status: ToolStatus::Success,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: None,
            },
            Err(e) => ToolResult {
                call_id: "".to_string(),
                output_json: "".to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to deliver message: {}", e)),
            },
        }
    }
}
