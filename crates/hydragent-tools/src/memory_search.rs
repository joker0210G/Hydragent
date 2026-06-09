use std::sync::Arc;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use hydragent_types::{ToolResult, ToolStatus};
use hydragent_memory::SessionStore;
use crate::tool_trait::Tool;

pub struct MemorySearchTool {
    store: Arc<SessionStore>,
}

impl MemorySearchTool {
    pub fn new(store: Arc<SessionStore>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct MemorySearchParams {
    query: String,
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Searches long-term memory for relevant facts, concepts, or preferences using keywords."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query keywords."
                }
            },
            "required": ["query"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start_time = std::time::Instant::now();
        let call_id = uuid::Uuid::new_v4().to_string();

        let params: MemorySearchParams = match serde_json::from_str(params_json) {
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

        // Perform hybrid search (BM25 + vector via RRF). Returns up to 5 results.
        match hydragent_memory::hybrid_search(&params.query, 5, &self.store).await {
            Ok(results) => {
                ToolResult {
                    call_id,
                    output_json: json!({
                        "status": "success",
                        "results": results
                    }).to_string(),
                    status: ToolStatus::Success,
                    execution_ms: start_time.elapsed().as_millis() as u32,
                    error_message: None,
                }
            }
            Err(e) => ToolResult {
                call_id,
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start_time.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to search memories: {}", e)),
            },
        }
    }
}
