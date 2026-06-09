use std::collections::HashMap;
use std::sync::Arc;
use hydragent_types::{ToolCall, ToolResult, ToolStatus};
use tracing::{info, warn};
use crate::tool_trait::Tool;

/// Thread-safe tool registry. Shared via Arc<ToolRegistry>.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        info!(tool = %name, "Tool registered");
        self.tools.insert(name, Arc::new(tool));
    }

    pub async fn invoke(&self, call: &ToolCall) -> ToolResult {
        match self.tools.get(&call.tool_id) {
            None => {
                warn!(tool_id = %call.tool_id, "Unknown tool invoked");
                ToolResult {
                    call_id: call.call_id.clone(),
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Tool '{}' not found", call.tool_id)),
                }
            }
            Some(tool) => tool.execute(&call.params_json).await,
        }
    }

    pub fn get_tier(&self, tool_id: &str) -> hydragent_types::PermissionTier {
        self.tools.get(tool_id)
            .map(|t| t.permission_tier())
            .unwrap_or(hydragent_types::PermissionTier::Deny)
    }

    /// Build the tool-descriptions block injected into the system prompt.
    pub fn build_system_prompt_block(&self) -> String {
        self.tools.values()
            .map(|t| format!("- **{}**: {}\n  Params (JSON Schema): {}\n",
                             t.name(), t.description(), t.params_schema()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
