use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus};
use crate::tool_trait::Tool;
use serde_json::Value;

pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes the input message back. Used for testing."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message to echo back"
                }
            },
            "required": ["message"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let val: Value = match serde_json::from_str(params_json) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        let message = val.get("message").and_then(|m| m.as_str()).unwrap_or("");
        
        ToolResult {
            call_id: "".to_string(),
            output_json: serde_json::to_string(&serde_json::json!({ "message": message })).unwrap_or_default(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}
