use std::sync::Arc;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use hydragent_types::{ToolResult, ToolStatus};
use hydragent_memory::SessionStore;
use crate::tool_trait::Tool;

pub struct MemoryStoreTool {
    store: Arc<SessionStore>,
}

impl MemoryStoreTool {
    pub fn new(store: Arc<SessionStore>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct MemoryStoreParams {
    content: String,
    importance: Option<i64>,
    tags: Option<Vec<String>>,
    page_id: Option<String>,
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn description(&self) -> &str {
        "Explicitly stores a text fact (concept, fact, or user preference) in long-term memory."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The exact fact, concept, or preference to remember."
                },
                "importance": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 5,
                    "description": "How critical this fact is from 1 (minor details) to 5 (user names, core settings)."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Topic tags for organizing this memory (e.g. ['preference', 'user_info'])."
                },
                "page_id": {
                    "type": "string",
                    "description": "Optional Page identifier to scope this memory."
                }
            },
            "required": ["content"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start_time = std::time::Instant::now();
        let call_id = uuid::Uuid::new_v4().to_string();

        let params: MemoryStoreParams = match serde_json::from_str(params_json) {
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

        let memory_id = uuid::Uuid::new_v4().to_string();
        let importance = params.importance.unwrap_or(1);
        let tags = params.tags.unwrap_or_default();
        let page_id = params.page_id.as_deref();

        match self.store.insert_memory(&memory_id, page_id, &params.content, importance, &tags).await {
            Ok(_) => ToolResult {
                call_id,
                output_json: json!({
                    "status": "success",
                    "memory_id": memory_id,
                    "message": "Fact stored successfully in long-term memory."
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
                error_message: Some(format!("Failed to write memory to database: {}", e)),
            },
        }
    }
}
