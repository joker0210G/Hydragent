use async_trait::async_trait;
use hydragent_types::ToolResult;

/// Every tool implements this trait. Boxed as `Box<dyn Tool + Send + Sync>`.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn params_schema(&self) -> &str;  // JSON Schema string (shown to LLM)
    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::AutoApprove
    }
    async fn execute(&self, params_json: &str) -> ToolResult;
}
