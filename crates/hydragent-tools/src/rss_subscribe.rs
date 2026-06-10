use async_trait::async_trait;
use hydragent_types::{PermissionTier, ToolResult, ToolStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;

#[derive(Serialize, Deserialize)]
struct RssSubscribeParams {
    url: String,
    name: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default = "default_digest_channel")]
    digest_channel: String,
    #[serde(default = "default_digest_cron")]
    digest_cron: String,
}

fn default_digest_channel() -> String {
    "current".to_string()
}

fn default_digest_cron() -> String {
    "0 8 * * *".to_string()
}

pub struct RssSubscribeTool {
    subscribe_fn: Arc<
        dyn Fn(String, String, String, String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync
    >,
}

impl RssSubscribeTool {
    pub fn new<F>(subscribe_fn: F) -> Self
    where
        F: Fn(String, String, String, String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync + 'static,
    {
        Self {
            subscribe_fn: Arc::new(subscribe_fn),
        }
    }
}

#[async_trait]
impl crate::tool_trait::Tool for RssSubscribeTool {
    fn name(&self) -> &str {
        "rss_subscribe"
    }

    fn description(&self) -> &str {
        "Add an RSS or Atom feed to the Work IQ monitor. The agent will check this feed periodically and alert you if matching keywords are found."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "RSS or Atom feed URL"
                },
                "name": {
                    "type": "string",
                    "description": "Friendly name for this feed (e.g., 'Rust Blog')"
                },
                "keywords": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Keywords that trigger an immediate alert if found in any entry title or summary"
                },
                "digest_channel": {
                    "type": "string",
                    "description": "Channel/Page ID to push daily digests to (e.g. 'telegram:123456789')"
                },
                "digest_cron": {
                    "type": "string",
                    "description": "Cron schedule for digest delivery. Defaults to '0 8 * * *' (daily 8 AM)"
                }
            },
            "required": ["url", "name"]
        }"#
    }

    fn permission_tier(&self) -> PermissionTier {
        PermissionTier::Prompt
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let params: RssSubscribeParams = match serde_json::from_str(params_json) {
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

        // If digest_channel is "current" or default, we should fall back or let core handle it.
        // Convert keywords array to comma-separated string.
        let keywords_str = params.keywords.join(", ");

        match (self.subscribe_fn)(
            params.url.clone(),
            params.name.clone(),
            keywords_str,
            params.digest_channel,
            params.digest_cron,
        ).await {
            Ok(_) => ToolResult {
                call_id: "".to_string(),
                output_json: format!(r#"{{"subscribed":true,"feed_name":"{}"}}"#, params.name),
                status: ToolStatus::Success,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: None,
            },
            Err(e) => ToolResult {
                call_id: "".to_string(),
                output_json: "".to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to subscribe to feed: {}", e)),
            },
        }
    }
}
