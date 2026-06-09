use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus};
use crate::tool_trait::Tool;
use serde_json::Value;
use reqwest::Client;
use tracing::warn;

pub struct WebSearchTool {
    client: Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for current information. Returns a summary of findings."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query string"
                }
            },
            "required": ["query"]
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

        let query = val.get("query").and_then(|q| q.as_str()).unwrap_or("");
        let url = format!("https://api.duckduckgo.com/?q={}&format=json&no_html=1", urlencoding::encode(query));

        match self.client.get(&url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    let body_json: Value = resp.json().await.unwrap_or(Value::Null);
                    let abstract_text = body_json.get("AbstractText")
                        .and_then(|a| a.as_str())
                        .unwrap_or("");
                    
                    let mut results = vec![];
                    if !abstract_text.is_empty() {
                        results.push(abstract_text.to_string());
                    }

                    // Parse related topics if abstract is empty
                    if results.is_empty() {
                        if let Some(related) = body_json.get("RelatedTopics").and_then(|r| r.as_array()) {
                            for topic in related.iter().take(3) {
                                if let Some(text) = topic.get("Text").and_then(|t| t.as_str()) {
                                    results.push(text.to_string());
                                }
                            }
                        }
                    }

                    if results.is_empty() {
                        results.push("No direct instant answer matches found. Try refining your query.".to_string());
                    }

                    let output = serde_json::json!({
                        "results": results,
                        "query": query
                    });

                    ToolResult {
                        call_id: "".to_string(),
                        output_json: serde_json::to_string(&output).unwrap_or_default(),
                        status: ToolStatus::Success,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: None,
                    }
                } else {
                    ToolResult {
                        call_id: "".to_string(),
                        output_json: "{}".to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(format!("HTTP error response: {}", resp.status())),
                    }
                }
            }
            Err(e) => {
                warn!("Search tool failed to execute: {}", e);
                ToolResult {
                    call_id: "".to_string(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("Network query failed: {}", e)),
                }
            }
        }
    }
}
