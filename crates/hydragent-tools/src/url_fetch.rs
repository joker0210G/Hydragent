use std::time::{Duration, Instant};
use std::net::IpAddr;

use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus};
use reqwest::Client;
use serde_json::Value;
use tracing::warn;

use crate::tool_trait::Tool;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_CHARS: usize = 12000;
const MAX_BODY_BYTES: u64 = 25 * 1024 * 1024; // 25 MB hard cap
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) \
                          Gecko/20100101 Firefox/128.0";

pub struct UrlFetchTool {
    client: Client,
}

impl UrlFetchTool {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self { client }
    }
}

impl Default for UrlFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate that a URL is safe to fetch (SSRF protection).
///
/// Blocks:
/// - Non-http(s) schemes (file://, ftp://, gopher://, etc.)
/// - Private IP ranges (RFC 1918)
/// - Loopback (127.0.0.0/8, ::1)
/// - Link-local (169.254.0.0/16)
/// - Multicast
fn validate_url(raw: &str) -> Result<(), String> {
    let parsed = match url::Url::parse(raw) {
        Ok(u) => u,
        Err(e) => return Err(format!("Invalid URL: {}", e)),
    };

    // 1. Scheme check
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("Scheme '{}' is not allowed. Only http:// and https:// are supported.", s)),
    }

    // 2. Host check — block IP literals in dangerous ranges
    if let Some(host) = parsed.host_str() {
        if let Ok(addr) = host.parse::<IpAddr>() {
            let blocked = match addr {
                IpAddr::V4(v4) => {
                    v4.is_private()
                        || v4.is_loopback()
                        || v4.is_multicast()
                        || v4.is_unspecified()
                        || (v4.octets()[0] == 169 && v4.octets()[1] == 254)
                }
                IpAddr::V6(v6) => {
                    v6.is_loopback()
                        || v6.is_multicast()
                        || v6.is_unspecified()
                        || v6.is_unicast_link_local()
                }
            };
            if blocked {
                return Err(format!(
                    "Blocked: URL resolves to internal/private address {}",
                    addr
                ));
            }
        }
        // Block bare IPs that look like AWS metadata or Docker bridge
        if host == "169.254.169.254"
            || host.starts_with("10.")
            || host.starts_with("192.168.")
            || host.starts_with("172.")
            || host == "localhost"
            || host == "127.0.0.1"
        {
            return Err(format!("Blocked: URL points to internal/private host '{}'", host));
        }
    } else {
        return Err("URL has no host".to_string());
    }

    Ok(())
}

/// Strip simple HTML tags from a string.
fn strip_html_tags(s: &str) -> String {
    let re = match regex::Regex::new(r"<[^>]*>") {
        Ok(r) => r,
        Err(_) => return s.to_string(),
    };
    re.replace_all(s, "").to_string()
}

/// Truncate a string to at most `max_chars` Unicode characters, appending "…".
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{}…", truncated)
}

#[async_trait]
impl Tool for UrlFetchTool {
    fn name(&self) -> &str {
        "url_fetch"
    }

    fn description(&self) -> &str {
        "Fetch any URL on the web and return its full text content. \
         Uses Jina AI Reader (https://r.jina.ai/) under the hood to convert \
         cluttered HTML into clean, LLM-friendly plain text automatically. \
         Use this when you have a specific URL you want to read, or when \
         other fetch tools fail. No API key required."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (e.g. 'https://docs.rs/tokio/latest/tokio/')"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return. Default 12000."
                }
            },
            "required": ["url"]
        }"#
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::AutoApprove
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = Instant::now();

        let val: Value = match serde_json::from_str(params_json) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: serde_json::json!({"error": format!("Invalid parameters: {}", e)}).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid JSON: {}", e)),
                };
            }
        };

        let url = val.get("url").and_then(|u| u.as_str()).unwrap_or("").trim();
        if url.is_empty() {
            return ToolResult {
                call_id: String::new(),
                output_json: serde_json::json!({
                    "error": "Missing or empty 'url' parameter",
                    "hint": "Pass {\"url\": \"https://example.com\"}"
                }).to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some("Missing URL".to_string()),
            };
        }

        // SSRF guard — block internal/private URLs before any network call.
        if let Err(e) = validate_url(url) {
            return ToolResult {
                call_id: String::new(),
                output_json: serde_json::json!({
                    "error": e,
                    "url": url,
                    "hint": "Only public http:// and https:// URLs are allowed. \
                              Internal IPs, localhost, and private networks are blocked \
                              for security. Set URL_FETCH_ALLOW_PRIVATE=1 to disable \
                              this check (not recommended)."
                }).to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some("SSRF guard blocked URL".to_string()),
            };
        }

        let max_chars = val
            .get("max_chars")
            .and_then(|n| n.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_CHARS);

        // Route through Jina AI Reader to get clean, LLM-friendly text.
        // Jina prepends https://r.jina.ai/ before any URL and returns
        // extracted article text (no ads, nav, or bloated HTML).
        let jina_url = format!("https://r.jina.ai/{}", url);

        let resp = match tokio::time::timeout(
            Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            self.client.get(&jina_url).header(reqwest::header::ACCEPT, "text/html, text/plain, */*;q=0.8").send()
        ).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                warn!("url_fetch failed: {}", e);
                return ToolResult {
                    call_id: String::new(),
                    output_json: serde_json::json!({
                        "error": format!("Request failed: {}", e),
                        "url": url,
                        "hint": "Check the URL is valid and reachable."
                    }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("Network error: {}", e)),
                };
            }
            Err(_) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: serde_json::json!({
                        "error": "Request timed out",
                        "url": url,
                        "hint": "The server took too long to respond. Try a different URL or increase timeout."
                    }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some("Timeout".to_string()),
                };
            }
        };

        let http_status = resp.status();
        if !http_status.is_success() {
            return ToolResult {
                call_id: String::new(),
                output_json: serde_json::json!({
                    "error": format!("HTTP {} from {}", http_status.as_u16(), url),
                    "url": url,
                    "hint": "The server returned an error. The page may be behind authentication or no longer exist."
                }).to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("HTTP {}", http_status.as_u16())),
            };
        }

        // Stream the body with a size cap to avoid unbounded memory use.
        let content_length = resp.content_length();
        if let Some(len) = content_length {
            if len > MAX_BODY_BYTES {
                return ToolResult {
                    call_id: String::new(),
                    output_json: serde_json::json!({
                        "error": format!("Response body is too large ({} bytes, max {})", len, MAX_BODY_BYTES),
                        "url": url,
                        "hint": "The page is larger than 25 MB. Try fetching a specific section or a different URL."
                    }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some("Body too large".to_string()),
                };
            }
        }

        let body = match resp.bytes().await {
            Ok(b) => {
                if b.len() as u64 > MAX_BODY_BYTES {
                    return ToolResult {
                        call_id: String::new(),
                        output_json: serde_json::json!({
                            "error": format!("Response body is too large ({} bytes, max {})", b.len(), MAX_BODY_BYTES),
                            "url": url,
                            "hint": "The page is larger than 25 MB. Try fetching a specific section or a different URL."
                        }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some("Body too large".to_string()),
                    };
                }
                String::from_utf8_lossy(&b).to_string()
            }
            Err(e) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: serde_json::json!({
                        "error": format!("Failed to read response body: {}", e),
                        "url": url
                    }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("Read error: {}", e)),
                };
            }
        };

        // Strip HTML tags so the LLM doesn't waste tokens on `<div class="...">`.
        let plain = strip_html_tags(&body);
        let content = truncate_chars(&plain, max_chars);
        let was_truncated = content.chars().count() < plain.chars().count();

        let output = serde_json::json!({
            "url": url,
            "content": content,
            "content_length": content.chars().count(),
            "truncated": was_truncated,
            "html_tags_removed": plain.len() < body.len(),
            "via_jina": true,
            "jina_url": jina_url,
        });

        ToolResult {
            call_id: String::new(),
            output_json: serde_json::to_string(&output).unwrap_or_default(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_fetch_tool_name() {
        let tool = UrlFetchTool::new();
        assert_eq!(tool.name(), "url_fetch");
        assert!(tool.description().contains("Jina AI Reader"));
    }

    #[test]
    fn test_params_schema_has_url() {
        let tool = UrlFetchTool::new();
        let schema = tool.params_schema();
        assert!(schema.contains("\"url\""));
        assert!(schema.contains("\"max_chars\""));
        assert!(schema.contains("\"required\""));
    }

    #[test]
    fn test_validate_url_allows_public_https() {
        assert!(validate_url("https://docs.rs/tokio").is_ok());
        assert!(validate_url("https://example.com/path?q=1").is_ok());
    }

    #[test]
    fn test_validate_url_blocks_loopback() {
        assert!(validate_url("https://127.0.0.1/").is_err());
        assert!(validate_url("http://localhost:8080/").is_err());
        assert!(validate_url("http://127.0.0.1:5984/_all_dbs").is_err());
    }

    #[test]
    fn test_validate_url_blocks_private() {
        assert!(validate_url("http://10.0.0.1/").is_err());
        assert!(validate_url("http://192.168.1.1/").is_err());
        assert!(validate_url("http://172.16.0.1/").is_err());
    }

    #[test]
    fn test_validate_url_blocks_link_local() {
        assert!(validate_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_url("http://169.254.1.1/").is_err());
    }

    #[test]
    fn test_validate_url_blocks_non_http() {
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("ftp://example.com/").is_err());
        assert!(validate_url("gopher://example.com/").is_err());
    }

    #[test]
    fn test_validate_url_blocks_multicast() {
        assert!(validate_url("http://224.0.0.1/").is_err());
    }

    #[test]
    fn test_validate_url_blocks_unspecified() {
        assert!(validate_url("http://0.0.0.0/").is_err());
    }

    #[test]
    fn test_strip_html_tags_basic() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_html_tags("<div class='x'>a</div>"), "a");
    }

    #[test]
    fn test_truncate_chars_unicode_safe() {
        let s = "🚀".repeat(10);
        let out = truncate_chars(&s, 5);
        assert_eq!(out.chars().count(), 6); // 5 + ellipsis
    }
}
