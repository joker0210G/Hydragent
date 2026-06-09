use std::sync::Arc;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use hydragent_types::{ToolResult, ToolStatus};
use hydragent_memory::SessionStore;
use crate::tool_trait::Tool;

pub struct MemoryForgetTool {
    store: Arc<SessionStore>,
}

impl MemoryForgetTool {
    pub fn new(store: Arc<SessionStore>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct MemoryForgetParams {
    memory_id: String,
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn description(&self) -> &str {
        "Deletes a specific fact from long-term memory using its unique memory_id."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "memory_id": {
                    "type": "string",
                    "description": "The unique ID of the memory/fact to delete."
                }
            },
            "required": ["memory_id"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start_time = std::time::Instant::now();
        let call_id = uuid::Uuid::new_v4().to_string();

        let params: MemoryForgetParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: start_time.elapsed().as_millis() as u32,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        match self.store.delete_memory(&params.memory_id).await {
            Ok(_) => ToolResult {
                call_id,
                output_json: json!({
                    "status": "success",
                    "message": format!("Memory '{}' has been forgotten.", params.memory_id)
                }).to_string(),
                status: ToolStatus::Success,
                execution_ms: start_time.elapsed().as_millis() as u32,
                error_message: None,
            },
            Err(e) => ToolResult {
                call_id,
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start_time.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to delete memory: {}", e)),
            },
        }
    }
}
