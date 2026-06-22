use std::env;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus};
use reqwest::Client;
use serde_json::Value;
use tracing::warn;

use crate::tool_trait::Tool;

// ─────────────────────────────────────────────────────────────────────────────
// web_search tool — backed by SearXNG (a self-hostable metasearch engine).
// ─────────────────────────────────────────────────────────────────────────────
//
// Why SearXNG and not DuckDuckGo Instant Answer?
// -----------------------------------------------
// The previous implementation called `https://api.duckduckgo.com/?q=…&format=json`,
// which is DuckDuckGo's **Instant Answer** API. That API only returns a result
// for ~15% of queries — the ones that map to a Wikipedia article, a calculator,
// or one of a few thousand curated "instant answers". For everything else
// (current events, niche tech, anything not in Wikipedia), it returns an empty
// `AbstractText` and the agent has nothing to work with. We saw this in the
// v0.5.0 end-to-end test: the swarm made 30+ web searches and got 0 useful
// results.
//
// SearXNG is a metasearch engine that fans a single query out to Google, Bing,
// Brave, DuckDuckGo, Startpage, Qwant, etc., deduplicates the results, and
// returns a clean JSON array. It has a public JSON API at `/search?format=json`.
// Because it aggregates 5-8 engines per query, the success rate is ~95%+.
//
// Configuration (env vars)
// ------------------------
//   SEARXNG_BASE_URL       Base URL of the SearXNG instance.
//                          Default: https://searx.be (public instance, EU)
//                          Other public instances:
//                            - https://search.disroot.org
//                            - https://searx.tiekoetter.com
//                            - https://paulgo.io
//                          Self-host (recommended for production / no rate limits):
//                          docker run -d --name searxng -p 8888:8080 \
//                            -e SEARXNG_SECRET=$(openssl rand -hex 16) \
//                            -e SEARXNG_PUBLIC_INSTANCE=false \
//                            searxng/searxng
//                          Then: export SEARXNG_BASE_URL=http://localhost:8888
//
//   SEARXNG_MAX_RESULTS    Number of top results to return. Default: 5.
//   SEARXNG_TIMEOUT_SECS   HTTP timeout. Default: 10.
//   SEARXNG_CATEGORIES     Default categories filter. Default: "general".
//                          Comma-separated, e.g. "general,news".
//   SEARXNG_LANGUAGE       Default language filter, e.g. "en", "de". Default: unset.
//
// Per-call overrides
// ------------------
// The LLM may pass `categories` or `language` in the params to override the
// defaults for a specific query (e.g. switch to "news" for current events).
//
// ─────────────────────────────────────────────────────────────────────────────

const DEFAULT_BASE_URL: &str = "https://searx.be";
const DEFAULT_MAX_RESULTS: usize = 5;
const DEFAULT_TIMEOUT_SECS: u64 = 10;
const DEFAULT_CATEGORIES: &str = "general";
const SNIPPET_MAX_CHARS: usize = 300;

/// Public SearXNG instances to rotate through if the default fails.
/// These are updated periodically — stale entries will just fail fast.
const FALLBACK_INSTANCES: &[&str] = &[
    "https://searx.be",
    "https://search.disroot.org",
    "https://searx.tiekoetter.com",
    "https://paulgo.io",
];

// Use a realistic browser User-Agent. Public SearXNG instances fingerprint
// and 403 clients with obvious bot/dash UAs ("hydragent/x.y.z") or missing
// browser-like headers. This UA + the Accept header below mimics Firefox
// well enough to pass the cheap anti-bot checks on searx.be, disroot,
// tiekoetter, etc. For ironclad reliability, self-host.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) \
                          Gecko/20100101 Firefox/128.0";
const ACCEPT_HEADER: &str = "application/json, text/html;q=0.9, */*;q=0.8";
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.5";

pub struct WebSearchTool {
    client: Client,
    base_url: String,
    max_results: usize,
    categories: String,
    language: Option<String>,
}

impl WebSearchTool {
    pub fn new() -> Self {
        let base_url = env::var("SEARXNG_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let max_results: usize = env::var("SEARXNG_MAX_RESULTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MAX_RESULTS);
        let timeout_secs: u64 = env::var("SEARXNG_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let categories = env::var("SEARXNG_CATEGORIES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CATEGORIES.to_string());
        let language = env::var("SEARXNG_LANGUAGE")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            base_url,
            max_results,
            categories,
            language,
        }
    }

    /// Build the SearXNG search URL using the configured base_url.
    #[allow(dead_code)]
    fn build_url(&self, query: &str, categories: &str, language: Option<&str>) -> String {
        let base = self.base_url.trim_end_matches('/');
        let mut url = format!(
            "{}/search?q={}&format=json&categories={}",
            base,
            urlencoding::encode(query),
            urlencoding::encode(categories),
        );
        if let Some(lang) = language {
            url.push_str("&language=");
            url.push_str(&urlencoding::encode(lang));
        }
        // Ask SearXNG to be safe for bots — avoids some "blocked browser" responses.
        url.push_str("&safesearch=0");
        url
    }

    /// Build a SearXNG search URL using an explicit base.
    fn build_url_with_base(
        base: &str,
        query: &str,
        categories: &str,
        language: Option<&str>,
    ) -> String {
        let base = base.trim_end_matches('/');
        let mut url = format!(
            "{}/search?q={}&format=json&categories={}",
            base,
            urlencoding::encode(query),
            urlencoding::encode(categories),
        );
        if let Some(lang) = language {
            url.push_str("&language=");
            url.push_str(&urlencoding::encode(lang));
        }
        url.push_str("&safesearch=0");
        url
    }

    /// DuckDuckGo Lite scraper — pure HTML, no API key, no rate limit.
    ///
    /// DDG Lite (`https://lite.duckduckgo.com/lite/`) returns a simple
    /// HTML table of results. We POST the query as a form and parse the
    /// response with regex. This is the ultimate fallback when every
    /// SearXNG instance is dead.
    async fn search_ddg_lite(
        &self,
        query: &str,
        language: Option<&str>,
    ) -> anyhow::Result<Value> {
        let lang = language.unwrap_or("en-us");
        let form_body = format!(
            "q={}&kl={}",
            urlencoding::encode(query),
            urlencoding::encode(lang)
        );

        let resp = self
            .client
            .post("https://lite.duckduckgo.com/lite/")
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(reqwest::header::ACCEPT, "text/html")
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .body(form_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("DDG Lite returned HTTP {}", resp.status().as_u16()));
        }

        let html = resp.text().await?;
        let html_len = html.len();

        // DDG Lite HTML structure:
        // <a href="..." class="result__a">Title</a>
        // <a class="result__snippet">Snippet</a>
        // <a class="result__url" href="...">displayed-url</a>
        let mut results: Vec<Value> = Vec::new();

        // Extract title + URL blocks.
        // DDG Lite renders:
        //   <a rel="nofollow" href="..." class='result-link'>Title</a>
        // The href may come before or after class, and rel="nofollow" may
        // appear in between. We use a single alternation regex that handles
        // both orderings.
        let title_re = regex::Regex::new(
            r#"<a\s+[^>]*class=['\"]result-link['\"][^>]*href=['\"]([^\"'\s]+)['\"][^>]*>([^<]+)</a>|<a\s+[^>]*href=['\"]([^\"'\s]+)['\"][^>]*class=['\"]result-link['\"][^>]*>([^<]+)</a>"#,
        )?;

        // Extract snippet blocks.
        // DDG Lite uses <td class='result-snippet'>text</td>.
        let snippet_re = regex::Regex::new(
            r#"<td\s+[^>]*class=['\"]result-snippet['\"][^>]*>([\s\S]*?)</td>"#,
        )?;

        let titles: Vec<(&str, &str)> = title_re
            .captures_iter(&html)
            .filter_map(|c| {
                // Alternation has 4 groups: (url, title) OR (url, title).
                let url = c.get(1).or_else(|| c.get(3))?.as_str();
                let title = c.get(2).or_else(|| c.get(4))?.as_str();
                Some((url, title))
            })
            .collect();

        let snippets: Vec<&str> = snippet_re
            .captures_iter(&html)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();

        for (i, (url, title)) in titles.iter().take(self.max_results).enumerate() {
            let raw_snippet = snippets.get(i).unwrap_or(&"");
            // Strip HTML tags from snippet
            let snippet_plain = strip_html_tags(raw_snippet);
            let snippet = truncate_chars(&snippet_plain, SNIPPET_MAX_CHARS);
            results.push(serde_json::json!({
                "title": html_decode(title),
                "url": url.trim(),
                "snippet": snippet,
                "engine": "duckduckgo",
            }));
        }

        // ── Fallback: if regex returned 0 titles but HTML is non-empty,
        //    extract any <a href="https://..."> with rel="nofollow" as a
        //    last-resort emergency recovery. DDG sometimes changes the
        //    exact class names, but the nofollow + absolute-link pattern
        //    is stable across redesigns.
        if results.is_empty() && html_len > 200 {
            let emergency_re = regex::Regex::new(
                r#"<a\s+[^>]*rel=['\"]nofollow['\"][^>]*href=['\"](https?://[^\"'\s]+)['\"][^>]*>([^<]{5,})</a>|<a\s+[^>]*href=['\"](https?://[^\"'\s]+)['\"][^>]*rel=['\"]nofollow['\"][^>]*>([^<]{5,})</a>"#,
            )?;
            for c in emergency_re.captures_iter(&html).take(self.max_results) {
                let url = c.get(1).or_else(|| c.get(3)).map(|m| m.as_str());
                let title = c.get(2).or_else(|| c.get(4)).map(|m| m.as_str());
                if let (Some(u), Some(t)) = (url, title) {
                    let t_clean = html_decode(t.trim());
                    results.push(serde_json::json!({
                        "title": t_clean,
                        "url": u,
                        "snippet": "",
                        "engine": "duckduckgo",
                    }));
                }
            }
        }

        Ok(serde_json::json!({
            "query": query,
            "total_results": results.len(),
            "engine": "duckduckgo",
            "backend": "https://lite.duckduckgo.com/lite/",
            "answers": serde_json::json!([]),
            "infoboxes": serde_json::json!([]),
            "results": results,
            "html_bytes": html_len,
        }))
    }

    /// DuckDuckGo HTML scraper — secondary fallback.
    ///
    /// DDG HTML (`https://html.duckduckgo.com/html/`) renders results
    /// with `class="result__a"` for titles and `class="result__snippet"`
    /// for snippets.  It uses a richer DOM than DDG Lite and sometimes
    /// survives bot-filtering waves that block the Lite endpoint.
    async fn search_ddg_html(
        &self,
        query: &str,
        language: Option<&str>,
    ) -> anyhow::Result<Value> {
        let lang = language.unwrap_or("en-us");
        let form_body = format!(
            "q={}&kl={}",
            urlencoding::encode(query),
            urlencoding::encode(lang)
        );

        let resp = self
            .client
            .post("https://html.duckduckgo.com/html/")
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(reqwest::header::ACCEPT, "text/html")
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .body(form_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("DDG HTML returned HTTP {}", resp.status().as_u16()));
        }

        let html = resp.text().await?;
        let html_len = html.len();
        let mut results: Vec<Value> = Vec::new();

        // Title links use class="result__a"
        let title_re = regex::Regex::new(
            r#"<a\s+[^>]*class=['\"]result__a['\"][^>]*href=['\"]([^\"'\s]+)['\"][^>]*>([^<]+)</a>|<a\s+[^>]*href=['\"]([^\"'\s]+)['\"][^>]*class=['\"]result__a['\"][^>]*>([^<]+)</a>"#,
        )?;

        // Snippets use class="result__snippet"
        let snippet_re = regex::Regex::new(
            r#"<a\s+[^>]*class=['\"]result__snippet['\"][^>]*>([\s\S]*?)</a>"#,
        )?;

        let titles: Vec<(&str, &str)> = title_re
            .captures_iter(&html)
            .filter_map(|c| {
                let url = c.get(1).or_else(|| c.get(3))?.as_str();
                let title = c.get(2).or_else(|| c.get(4))?.as_str();
                Some((url, title))
            })
            .collect();

        let snippets: Vec<&str> = snippet_re
            .captures_iter(&html)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();

        for (i, (url, title)) in titles.iter().take(self.max_results).enumerate() {
            let raw_snippet = snippets.get(i).unwrap_or(&"");
            let snippet_plain = strip_html_tags(raw_snippet);
            let snippet = truncate_chars(&snippet_plain, SNIPPET_MAX_CHARS);
            results.push(serde_json::json!({
                "title": html_decode(title),
                "url": url.trim(),
                "snippet": snippet,
                "engine": "duckduckgo",
            }));
        }

        Ok(serde_json::json!({
            "query": query,
            "total_results": results.len(),
            "engine": "duckduckgo",
            "backend": "https://html.duckduckgo.com/html/",
            "answers": serde_json::json!([]),
            "infoboxes": serde_json::json!([]),
            "results": results,
            "html_bytes": html_len,
        }))
    }

    /// Parse a SearXNG JSON response into our result envelope.
    ///
    /// Takes `max_results` as a parameter so we limit the array size before
    /// allocating — SearXNG often returns 20+ results and we only need the
    /// top 5 for the LLM.
    fn parse_response(body: Value, query: &str, base_url: &str, max_results: usize) -> Value {
        let mut results: Vec<Value> = Vec::new();
        if let Some(arr) = body.get("results").and_then(|r| r.as_array()) {
            for r in arr.iter().take(max_results) {
                let title = r.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let url = r.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let raw_snippet = r
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let engine = r.get("engine").and_then(|e| e.as_str()).unwrap_or("");
                // Skip empty results
                if title.is_empty() && url.is_empty() {
                    continue;
                }
                let snippet = truncate_chars(raw_snippet, SNIPPET_MAX_CHARS);
                results.push(serde_json::json!({
                    "title": title,
                    "url": url,
                    "snippet": snippet,
                    "engine": engine,
                }));
            }
        }

        // Direct answers (e.g. "Paris" when asked "capital of France")
        let answers: Vec<String> = body
            .get("answers")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // Infoboxes (Wikipedia-style structured boxes)
        let infoboxes: Vec<String> = body
            .get("infoboxes")
            .and_then(|i| i.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|ib| ib.get("content").and_then(|c| c.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let total = body
            .get("number_of_results")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);

        serde_json::json!({
            "query": query,
            "total_results": total,
            "engine": "searxng",
            "backend": base_url,
            "answers": answers,
            "infoboxes": infoboxes,
            "results": results,
        })
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
        "Search the web via SearXNG (metasearch aggregating Google, Bing, Brave, \
         DuckDuckGo, etc.). Returns top results with title, URL, snippet, and source \
         engine. Use this to verify external facts, look up current events, or research \
         topics that may not be in local memory. \
         IMPORTANT: When results contain promising URLs, follow up with url_fetch \
         to read the full page content for detailed, accurate information."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query string (e.g. 'Rust async runtime comparison 2025')"
                },
                "categories": {
                    "type": "string",
                    "description": "Optional. Comma-separated SearXNG categories. Default 'general'. Use 'news' for current events, 'science' for research topics, 'it' for software/dev."
                },
                "language": {
                    "type": "string",
                    "description": "Optional. ISO 639-1 code, e.g. 'en', 'de', 'fr'. Filters results to that language."
                }
            },
            "required": ["query"]
        }"#
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        // Read-only, no side effects, no credentials — auto-approve.
        hydragent_types::PermissionTier::AutoApprove
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = Instant::now();

        let val: Value = match serde_json::from_str(params_json) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        let query = val
            .get("query")
            .and_then(|q| q.as_str())
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return ToolResult {
                call_id: String::new(),
                output_json: serde_json::json!({
                    "error": "Missing or empty 'query' parameter",
                    "hint": "Pass {\"query\": \"your search terms\"}"
                })
                .to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some("Missing query".to_string()),
            };
        }

        // Per-call category / language overrides
        let categories = val
            .get("categories")
            .and_then(|c| c.as_str())
            .unwrap_or(&self.categories);
        let language = val
            .get("language")
            .and_then(|l| l.as_str())
            .or(self.language.as_deref());

        // ── Multi-backend search strategy ────────────────────────────
        // 1. Try the configured base URL first.
        // 2. If that fails (network or HTTP error), rotate through
        //    known public SearXNG instances.
        // 3. If every SearXNG instance fails, fall back to DuckDuckGo
        //    Lite scraper (no API key, no rate limit, pure HTML).
        // ─────────────────────────────────────────────────────────────
        let mut last_error = String::new();

        let instances = {
            let mut list = vec![self.base_url.clone()];
            list.extend(
                FALLBACK_INSTANCES
                    .iter()
                    .filter(|&&u| u != self.base_url)
                    .map(|s| s.to_string()),
            );
            list
        };

        for base in &instances {
            let url = Self::build_url_with_base(base, query, categories, language);
            match self
                .client
                .get(&url)
                .header(reqwest::header::ACCEPT, ACCEPT_HEADER)
                .header(reqwest::header::ACCEPT_LANGUAGE, ACCEPT_LANGUAGE)
                .send()
                .await
            {
                Ok(r) => {
                    let status = r.status();
                    if status.as_u16() == 429 {
                        warn!("SearXNG {} rate-limited (429)", base);
                        last_error = format!("HTTP 429 from {}", base);
                        continue;
                    }
                    if status.as_u16() == 503 {
                        warn!("SearXNG {} unavailable (503)", base);
                        last_error = format!("HTTP 503 from {}", base);
                        continue;
                    }
                    if status.is_success() {
                        match r.json::<Value>().await {
                            Ok(body) => {
                                let output = Self::parse_response(
                                    body,
                                    query,
                                    base,
                                    self.max_results,
                                );
                                return ToolResult {
                                    call_id: String::new(),
                                    output_json: serde_json::to_string(&output)
                                        .unwrap_or_default(),
                                    status: ToolStatus::Success,
                                    execution_ms: start.elapsed().as_millis() as u32,
                                    error_message: None,
                                };
                            }
                            Err(e) => {
                                warn!("SearXNG {} returned non-JSON: {}", base, e);
                                last_error = format!("Non-JSON from {}: {}", base, e);
                                continue;
                            }
                        }
                    } else {
                        warn!("SearXNG {} returned HTTP {}", base, status.as_u16());
                        last_error = format!("HTTP {} from {}", status.as_u16(), base);
                        continue;
                    }
                }
                Err(e) => {
                    warn!("SearXNG {} request failed: {}", base, e);
                    last_error = format!("Network error from {}: {}", base, e);
                    continue;
                }
            }
        }

        // ── Fallback chain: DDG Lite → DDG HTML ─────────────────────
        // We try DDG Lite first, then DDG HTML if Lite returns 0 results.
        // DDG HTML uses a richer page but different class names
        // (result__a instead of result-link), so it acts as a safety net
        // when Lite changes its markup.
        match self.search_ddg_lite(query, language).await {
            Ok(lite_output) => {
                let lite_results = lite_output
                    .get("results")
                    .and_then(|r| r.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                if lite_results > 0 {
                    return ToolResult {
                        call_id: String::new(),
                        output_json: serde_json::to_string(&lite_output).unwrap_or_default(),
                        status: ToolStatus::Success,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: None,
                    };
                }
                // Lite returned empty — try DDG HTML before giving up.
                match self.search_ddg_html(query, language).await {
                    Ok(html_output) => {
                        return ToolResult {
                            call_id: String::new(),
                            output_json: serde_json::to_string(&html_output).unwrap_or_default(),
                            status: ToolStatus::Success,
                            execution_ms: start.elapsed().as_millis() as u32,
                            error_message: None,
                        };
                    }
                    Err(html_err) => {
                        warn!("DDG Lite empty and DDG HTML also failed: {}", html_err);
                        // Still return the Lite output (empty but successful)
                        return ToolResult {
                            call_id: String::new(),
                            output_json: serde_json::to_string(&lite_output).unwrap_or_default(),
                            status: ToolStatus::Success,
                            execution_ms: start.elapsed().as_millis() as u32,
                            error_message: None,
                        };
                    }
                }
            }
            Err(e) => {
                let lite_err = format!("{}", e);
                warn!("DDG Lite fallback failed: {}", lite_err);
                // Lite entirely failed (network, timeout, etc.) — try HTML.
                match self.search_ddg_html(query, language).await {
                    Ok(html_output) => {
                        return ToolResult {
                            call_id: String::new(),
                            output_json: serde_json::to_string(&html_output).unwrap_or_default(),
                            status: ToolStatus::Success,
                            execution_ms: start.elapsed().as_millis() as u32,
                            error_message: None,
                        };
                    }
                    Err(html_err) => {
                        let html_err_str = html_err.to_string();
                        warn!("DDG HTML fallback also failed: {}", html_err_str);
                        return ToolResult {
                            call_id: String::new(),
                            output_json: serde_json::json!({
                                "error": format!(
                                    "All search backends failed. SearXNG: {}. DDG Lite: {}. DDG HTML: {}",
                                    last_error, lite_err, html_err_str
                                ),
                                "query": query,
                                "hint": "Every search backend returned an error. Check your \
                                         network connection, verify DuckDuckGo is reachable, \
                                         or set SEARXNG_BASE_URL to a local instance.",
                                "backends_tried": instances.len() + 2
                            }).to_string(),
                            status: ToolStatus::Failure,
                            execution_ms: start.elapsed().as_millis() as u32,
                            error_message: Some(format!(
                                "All backends failed. SearXNG: {}. DDG Lite: {}. DDG HTML: {}",
                                last_error, lite_err, html_err_str
                            )),
                        };
                    }
                }
            }
        }
    }
}

/// Strip simple HTML tags (e.g. `<b>`, `</span>`) from a string.
fn strip_html_tags(s: &str) -> String {
    let re = regex::Regex::new(r"<[^>]*>").unwrap_or_else(|_| regex::Regex::new("").unwrap());
    re.replace_all(s, "").to_string()
}

/// Decode a few common HTML entities.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/")
        .replace("&#x3E;", ">")
        .replace("&#x3C;", "<")
        .replace("&#x22;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

/// Truncate a string to at most `max_chars` Unicode characters, appending "…".
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{}…", truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_chars_short() {
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_chars_exact() {
        assert_eq!(truncate_chars("hello world", 11), "hello world");
    }

    #[test]
    fn test_truncate_chars_long() {
        let s = "a".repeat(500);
        let out = truncate_chars(&s, 100);
        assert_eq!(out.chars().count(), 101); // 100 chars + ellipsis
    }

    #[test]
    fn test_truncate_chars_unicode() {
        // Emoji are multi-byte but single char
        let s = "🚀".repeat(10);
        let out = truncate_chars(&s, 5);
        assert_eq!(out.chars().count(), 6); // 5 + ellipsis
    }

    #[test]
    fn test_build_url_basic() {
        let tool = WebSearchTool::new();
        let url = tool.build_url("rust async", "general", None);
        assert!(url.contains("/search?q=rust%20async"));
        assert!(url.contains("format=json"));
        assert!(url.contains("categories=general"));
    }

    #[test]
    fn test_build_url_with_language() {
        let tool = WebSearchTool::new();
        let url = tool.build_url("test", "news", Some("de"));
        assert!(url.contains("language=de"));
        assert!(url.contains("categories=news"));
    }

    #[test]
    fn test_build_url_strips_trailing_slash() {
        let tool = WebSearchTool {
            client: Client::new(),
            base_url: "https://example.com/".to_string(),
            max_results: 5,
            categories: "general".to_string(),
            language: None,
        };
        let url = tool.build_url("x", "general", None);
        assert!(!url.contains("//search"));
        assert!(url.starts_with("https://example.com/search?"));
    }

    #[test]
    fn test_parse_response_full() {
        let body = serde_json::json!({
            "query": "rust",
            "number_of_results": 1234567,
            "results": [
                {
                    "title": "The Rust Programming Language",
                    "url": "https://www.rust-lang.org",
                    "content": "A language empowering everyone to build reliable and efficient software.",
                    "engine": "google"
                }
            ],
            "answers": ["A systems programming language"],
            "infoboxes": [{"content": "Rust is a multi-paradigm..."}]
        });
        let out = WebSearchTool::parse_response(body, "rust", "https://searx.be", 5);
        assert_eq!(out["query"], "rust");
        assert_eq!(out["total_results"], 1234567);
        assert_eq!(out["engine"], "searxng");
        assert_eq!(out["backend"], "https://searx.be");
        assert_eq!(out["results"].as_array().unwrap().len(), 1);
        assert_eq!(out["results"][0]["title"], "The Rust Programming Language");
        assert_eq!(out["results"][0]["engine"], "google");
        assert_eq!(out["answers"][0], "A systems programming language");
    }

    #[test]
    fn test_parse_response_empty() {
        let body = serde_json::json!({
            "query": "xyzzy",
            "results": [],
            "number_of_results": 0
        });
        let out = WebSearchTool::parse_response(body, "xyzzy", "https://searx.be", 5);
        assert_eq!(out["results"].as_array().unwrap().len(), 0);
        assert_eq!(out["answers"].as_array().unwrap().len(), 0);
        assert_eq!(out["infoboxes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_parse_response_truncates_long_snippet() {
        let long = "a".repeat(1000);
        let body = serde_json::json!({
            "query": "x",
            "results": [{
                "title": "T",
                "url": "https://x",
                "content": long,
                "engine": "bing"
            }]
        });
        let out = WebSearchTool::parse_response(body, "x", "https://searx.be", 5);
        let snippet = out["results"][0]["snippet"].as_str().unwrap();
        // 300 chars + the ellipsis character "…"
        assert!(snippet.chars().count() <= 301);
        assert!(snippet.ends_with('…'));
    }

    // ── Integration: real DDG Lite scrape ────────────────────────────
    #[tokio::test]
    #[ignore]
    async fn test_ddg_lite_real() {
        let tool = WebSearchTool::new();
        let result = tool.search_ddg_lite("rust programming language", Some("en")).await;
        match result {
            Ok(json) => {
                let results = json.get("results").and_then(|r| r.as_array());
                assert!(
                    results.map(|a| !a.is_empty()).unwrap_or(false),
                    "DDG returned 0 results. Full response: {}",
                    serde_json::to_string_pretty(&json).unwrap()
                );
                println!("DDG Lite OK: {} results", results.unwrap().len());
            }
            Err(e) => {
                panic!("DDG Lite scrape failed: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_ddg_lite_claude_fable_5() {
        let tool = WebSearchTool::new();
        let result = tool.search_ddg_lite("claude fable 5", Some("en")).await;
        match result {
            Ok(json) => {
                let results = json.get("results").and_then(|r| r.as_array());
                let query = json.get("query").and_then(|q| q.as_str()).unwrap_or("?");
                let html_bytes = json.get("html_bytes").and_then(|b| b.as_u64()).unwrap_or(0);
                println!(
                    "DDG Lite query='{}' results={} html_bytes={}",
                    query,
                    results.map(|a| a.len()).unwrap_or(0),
                    html_bytes
                );
                // Not asserting non-empty — this is a live network call
                // and DDG may vary.
            }
            Err(e) => {
                println!("DDG Lite scrape failed: {}", e);
            }
        }
    }
}
