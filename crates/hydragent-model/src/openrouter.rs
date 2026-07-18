use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{OnceLock, RwLock};
use async_trait::async_trait;
use crate::model_trait::ModelProvider;
use zeroize::Zeroize;

// ---------------------------------------------------------------------------
// Live model capability cache (fetched once per session from /api/v1/models)
// ---------------------------------------------------------------------------

/// Capabilities and pricing for a single model returned by GET /api/v1/models.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    /// Cost in USD per million prompt tokens (0.0 = free)
    pub pricing_prompt: f64,
    /// Cost in USD per million completion tokens
    pub pricing_completion: f64,
    pub supports_reasoning: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub context_length: u64,
}

impl ModelInfo {
    pub fn is_free(&self) -> bool {
        self.pricing_prompt == 0.0 && self.pricing_completion == 0.0
    }
}

// ---------------------------------------------------------------------------
// Provider preferences (maps to OpenRouter's `provider` request field)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Default, Clone)]
pub struct ProviderPreferences {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quantizations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
}

impl ProviderPreferences {
    /// Build from environment variables. Returns None if all fields are default.
    pub fn from_env() -> Option<Self> {
        let order = csv_env("OPENROUTER_PROVIDER_ORDER");
        let ignore = csv_env("OPENROUTER_PROVIDER_IGNORE");
        let only = csv_env("OPENROUTER_PROVIDER_ONLY");
        let quantizations = csv_env("OPENROUTER_QUANTIZATIONS");
        let sort = std::env::var("OPENROUTER_SORT").ok().filter(|s| !s.is_empty());
        let data_collection = std::env::var("OPENROUTER_DATA_COLLECTION")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| Some("deny".to_string())); // default: privacy-preserving
        let allow_fallbacks = std::env::var("OPENROUTER_ALLOW_FALLBACKS")
            .ok()
            .and_then(|s| s.parse::<bool>().ok());
        let require_parameters = std::env::var("OPENROUTER_REQUIRE_PARAMETERS")
            .ok()
            .and_then(|s| s.parse::<bool>().ok());

        let prefs = ProviderPreferences {
            order, ignore, only, quantizations, sort, data_collection,
            allow_fallbacks, require_parameters,
        };

        // Return None only if absolutely everything is default
        Some(prefs)
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
            && self.ignore.is_empty()
            && self.only.is_empty()
            && self.quantizations.is_empty()
            && self.sort.is_none()
            && self.data_collection.as_deref() == Some("deny")
            && self.allow_fallbacks.is_none()
            && self.require_parameters.is_none()
    }
}

/// Read a comma-separated env var into a Vec<String>, filtering empty strings.
fn csv_env(key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// Reasoning config (maps to OpenRouter's `reasoning` request field)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub struct ReasoningConfig {
    /// "low" | "medium" | "high" | "auto"
    pub effort: String,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// The public request type shared across the whole model crate.
/// `models` carries the server-side fallback list for OpenRouter.
/// `reasoning_effort` sets thinking level for reasoning-capable models.
#[derive(Debug, Serialize, Clone)]
pub struct LLMRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// OpenRouter server-side fallback model list (empty = no fallback)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Reasoning effort level for thinking-capable models: "low", "medium", "high"
    /// Set to None to let the auto-detection decide.
    #[serde(skip)]
    pub reasoning_effort: Option<String>,
}

/// The wire format sent to OpenRouter's /chat/completions endpoint.
/// Kept private — callers use LLMRequest.
#[derive(Debug, Serialize)]
struct OpenRouterRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<ProviderPreferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,   // "system" | "user" | "assistant" | "tool"
    pub content: String,
}

// ---------------------------------------------------------------------------
// OpenRouterClient
// ---------------------------------------------------------------------------

pub struct OpenRouterClient {
    api_keys: Vec<String>,
    invalid_keys: RwLock<std::collections::HashSet<String>>,
    active_key_index: AtomicUsize,
    client: Client,
    base_url: String,
    /// Session-level capability cache, populated on first Ctrl+P or first request.
    model_cache: OnceLock<Vec<ModelInfo>>,
}

fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let without_suffix = trimmed.strip_suffix("/chat/completions").unwrap_or(trimmed);
    without_suffix.to_string()
}

fn resolve_vault_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("HYDRAGENT_DATA_DIR") {
        if !p.trim().is_empty() {
            return std::path::PathBuf::from(p.trim()).join("vault/.hydravault");
        }
    }
    
    let home = if let Ok(p) = std::env::var("HYDRAGENT_HOME") {
        std::path::PathBuf::from(p.trim())
    } else {
        #[cfg(target_os = "windows")]
        {
            std::env::var("USERPROFILE")
                .ok()
                .or_else(|| {
                    let drive = std::env::var("HOMEDRIVE").ok()?;
                    let path = std::env::var("HOMEPATH").ok()?;
                    Some(format!("{drive}{path}"))
                })
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".hydragent")
        }
        #[cfg(not(target_os = "windows"))]
        {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
        }
    };
    home.join("data/vault/.hydravault")
}

impl OpenRouterClient {
    pub fn new(api_keys: Vec<String>) -> Self {
        let timeout_secs = std::env::var("OPENROUTER_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(120);

        let base_url = std::env::var("OPENROUTER_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| normalize_base_url(&s))
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());

        Self {
            api_keys,
            invalid_keys: RwLock::new(std::collections::HashSet::new()),
            active_key_index: AtomicUsize::new(0),
            client: Client::builder()
                .timeout(Duration::from_secs(timeout_secs))
                .build()
                .unwrap_or_else(|_| Client::new()),
            base_url,
            model_cache: OnceLock::new(),
        }
    }

    // -----------------------------------------------------------------------
    // API key management
    // -----------------------------------------------------------------------

    fn get_active_key(&self) -> Option<String> {
        if self.api_keys.is_empty() {
            return None;
        }

        let mut vault_secrets = None;
        if let Ok(passphrase) = std::env::var("HYDRAGENT_VAULT_PASSPHRASE") {
            let vault_path = resolve_vault_path();
            let vault = hydragent_vault::Vault::new(vault_path);
            if vault.exists() {
                if let Ok(secrets) = vault.load(&passphrase) {
                    vault_secrets = Some(secrets);
                }
            }
        }

        let mut all_keys = Vec::new();
        for key in &self.api_keys {
            if key.contains("{{") && key.contains("}}") {
                if let Some(ref secrets) = vault_secrets {
                    let injected = hydragent_vault::inject_str(key, secrets);
                    for sub_key in injected.split(',') {
                        let trimmed = sub_key.trim().to_string();
                        if !trimmed.is_empty() {
                            all_keys.push(trimmed);
                        }
                    }
                } else {
                    all_keys.push(key.clone());
                }
            } else {
                all_keys.push(key.clone());
            }
        }

        let valid_keys: Vec<String> = {
            if let Ok(invalid) = self.invalid_keys.read() {
                all_keys.into_iter().filter(|k| !invalid.contains(k)).collect()
            } else {
                all_keys
            }
        };

        if valid_keys.is_empty() {
            return None;
        }

        let start_index = self.active_key_index.load(Ordering::Relaxed);
        let idx = start_index % valid_keys.len();
        Some(valid_keys[idx].clone())
    }

    fn mark_key_invalid(&self, key: &str) {
        if let Ok(mut invalid) = self.invalid_keys.write() {
            if invalid.insert(key.to_string()) {
                let mask_len = std::cmp::min(12, key.len());
                warn!("Marking OpenRouter API key as invalid: {}... (returned 401 Unauthorized)", &key[..mask_len]);
            }
        }
    }

    fn rotate_key(&self) {
        let old_idx = self.active_key_index.fetch_add(1, Ordering::Relaxed);
        warn!("Rotating OpenRouter API key from index {}", old_idx);
    }

    fn total_valid_keys(&self) -> usize {
        if self.api_keys.is_empty() {
            return 0;
        }

        let mut vault_secrets = None;
        if let Ok(passphrase) = std::env::var("HYDRAGENT_VAULT_PASSPHRASE") {
            let vault_path = resolve_vault_path();
            let vault = hydragent_vault::Vault::new(vault_path);
            if vault.exists() {
                if let Ok(secrets) = vault.load(&passphrase) {
                    vault_secrets = Some(secrets);
                }
            }
        }

        let mut all_keys = Vec::new();
        for key in &self.api_keys {
            if key.contains("{{") && key.contains("}}") {
                if let Some(ref secrets) = vault_secrets {
                    let injected = hydragent_vault::inject_str(key, secrets);
                    for sub_key in injected.split(',') {
                        let trimmed = sub_key.trim().to_string();
                        if !trimmed.is_empty() {
                            all_keys.push(trimmed);
                        }
                    }
                } else {
                    all_keys.push(key.clone());
                }
            } else {
                all_keys.push(key.clone());
            }
        }

        if let Ok(invalid) = self.invalid_keys.read() {
            all_keys.into_iter().filter(|k| !invalid.contains(k)).count()
        } else {
            all_keys.len()
        }
    }

    // -----------------------------------------------------------------------
    // Live model capability cache
    // -----------------------------------------------------------------------

    /// Fetch the full model list from OpenRouter and cache it for the session.
    /// Safe to call multiple times — fetches only once.
    pub async fn fetch_models(&self) -> &[ModelInfo] {
        if let Some(cached) = self.model_cache.get() {
            return cached;
        }

        let fetched = self.fetch_models_inner().await.unwrap_or_default();
        // OnceLock::set can fail if another thread races — that's fine, we just discard
        let _ = self.model_cache.set(fetched);
        self.model_cache.get().map(|v| v.as_slice()).unwrap_or(&[])
    }

    async fn fetch_models_inner(&self) -> Result<Vec<ModelInfo>> {
        let api_key = self.get_active_key()
            .context("No OpenRouter API keys available")?;

        let resp = self.client
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&api_key)
            .header("HTTP-Referer", "https://github.com/joker0210G/Hydragent")
            .header("X-Title", "Hydragent")
            .send()
            .await
            .context("Failed to fetch OpenRouter model list")?;

        if !resp.status().is_success() {
            anyhow::bail!("OpenRouter /models returned {}", resp.status());
        }

        let body: Value = resp.json().await.context("Failed to parse /models response")?;
        let data = body.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();

        let mut models = Vec::with_capacity(data.len());
        for m in &data {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if id.is_empty() { continue; }

            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(&id).to_string();
            let context_length = m.get("context_length").and_then(|v| v.as_u64()).unwrap_or(0);

            // Pricing: OpenRouter returns strings like "0.000001" (per token)
            // We normalise to USD per million tokens for display
            let pricing_prompt = m.get("pricing")
                .and_then(|p| p.get("prompt"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0) * 1_000_000.0;

            let pricing_completion = m.get("pricing")
                .and_then(|p| p.get("completion"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0) * 1_000_000.0;

            // Supported parameters is an array of strings
            let supported: Vec<&str> = m.get("supported_parameters")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();

            models.push(ModelInfo {
                id,
                name,
                pricing_prompt,
                pricing_completion,
                supports_reasoning: supported.contains(&"reasoning") || supported.contains(&"include_reasoning"),
                supports_tools: supported.contains(&"tools"),
                supports_vision: supported.contains(&"image"),
                context_length,
            });
        }

        info!("Fetched {} models from OpenRouter", models.len());
        Ok(models)
    }

    fn resolve_reasoning_options(&self, request: &LLMRequest, supports_reasoning: bool) -> (Option<bool>, Option<ReasoningConfig>) {
        if !supports_reasoning {
            return (None, None);
        }

        let effort = request.reasoning_effort.clone().unwrap_or_else(|| "auto".to_string());
        if effort == "auto" {
            (Some(true), None)
        } else {
            (None, Some(ReasoningConfig { effort }))
        }
    }

    /// Check if a model supports reasoning (from the capability cache).
    /// Falls back to false if cache hasn't been populated.
    pub fn model_supports_reasoning(&self, model_id: &str) -> bool {
        self.model_cache
            .get()
            .and_then(|cache| cache.iter().find(|m| m.id == model_id))
            .map(|m| m.supports_reasoning)
            .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // Streaming chat
    // -----------------------------------------------------------------------

    pub async fn chat_stream_internal(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
    ) -> Result<String> {
        let mut request = request.clone();
        let mut injected_scopes = Vec::new();

        if let Ok(passphrase) = std::env::var("HYDRAGENT_VAULT_PASSPHRASE") {
            let vault_path = resolve_vault_path();
            let vault = hydragent_vault::Vault::new(vault_path);
            if vault.exists() {
                if let Ok(secrets) = vault.load(&passphrase) {
                    let injector = hydragent_vault::KeyInjector::new(secrets);
                    for msg in &mut request.messages {
                        let (injected, scopes) = injector.inject_message(&msg.role, &msg.content);
                        msg.content = injected.expose_secret().to_string();
                        injected_scopes.extend(scopes);
                    }
                }
            }
        }

        // --- Auto-detect reasoning support from capability cache ---
        let supports_reasoning = self.model_supports_reasoning(&request.model);
        let (include_reasoning, reasoning_config) = self.resolve_reasoning_options(&request, supports_reasoning);

        // --- Build provider preferences from env ---
        let provider = ProviderPreferences::from_env().filter(|p| !p.is_empty());

        // --- Build the wire request ---
        let wire_request = OpenRouterRequest {
            model: &request.model,
            messages: &request.messages,
            stream: true,
            max_tokens: request.max_tokens,
            models: request.models.clone(),
            provider,
            include_reasoning,
            reasoning: reasoning_config,
        };

        let json_body = serde_json::to_string(&wire_request)?;
        let mut tainted_body = hydragent_vault::TaintedString::new(json_body);

        if !injected_scopes.is_empty() {
            tracing::info!(scopes = ?injected_scopes, "Performing key injection");
        }

        let api_key = self.get_active_key()
            .context("No OpenRouter API keys available in configuration")?;

        let resp = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(api_key)
            .header("HTTP-Referer", "https://github.com/joker0210G/Hydragent")
            .header("X-Title", "Hydragent")
            .header("Content-Type", "application/json")
            .body(tainted_body.expose_secret().to_string())
            .send()
            .await
            .context("OpenRouter request failed")?;

        // Zeroize sensitive materials immediately after sending request
        tainted_body.zeroize();
        for msg in &mut request.messages {
            msg.content.zeroize();
        }

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!("HTTP 429: Rate limited");
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let error_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter error response ({}): {}", status, error_text);
        }

        let mut full_content = String::new();
        let mut full_reasoning = String::new();
        let mut in_reasoning = false;
        let mut line_buffer = String::new();
        let mut stream = resp.bytes_stream();
        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("SSE chunk error")?;
            let text = std::str::from_utf8(&bytes)?;
            line_buffer.push_str(text);

            while let Some(pos) = line_buffer.find('\n') {
                let line = line_buffer[..pos].to_string();
                line_buffer.drain(..pos + 1);

                // Ignore SSE keep-alive comments like ": OPENROUTER PROCESSING"
                if line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    let trimmed = data.trim();
                    if trimmed == "[DONE]" {
                        break;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                        // --- API-level errors embedded in the stream ---
                        if let Some(err) = v.get("error") {
                            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
                            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                            if code == 429 || msg.contains("rate limit") || msg.contains("credits") {
                                anyhow::bail!("OpenRouter API level rate limit/credit error: {}", msg);
                            }
                        }

                        // --- Usage stats from the final chunk ---
                        if let Some(usage) = v.get("usage") {
                            let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let cost = usage.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let cached_tokens = usage.get("prompt_tokens_details")
                                .and_then(|d| d.get("cached_tokens"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let reasoning_tokens = usage.get("completion_tokens_details")
                                .and_then(|d| d.get("reasoning_tokens"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            info!(
                                model = %request.model,
                                prompt_tokens,
                                completion_tokens,
                                reasoning_tokens,
                                cached_tokens,
                                cost_usd = format!("${:.6}", cost),
                                "OpenRouter usage"
                            );
                        }

                        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                            if !choices.is_empty() {
                                if let Some(delta) = choices[0].get("delta") {

                                    // --- Reasoning/thinking tokens ---
                                    if let Some(reasoning) = delta.get("reasoning").and_then(|t| t.as_str()) {
                                        if !reasoning.is_empty() {
                                            if !in_reasoning {
                                                // Signal start of thinking block
                                                let _ = tx.send("<think>".to_string()).await;
                                                in_reasoning = true;
                                            }
                                            full_reasoning.push_str(reasoning);
                                            let _ = tx.send(reasoning.to_string()).await;
                                        }
                                    }

                                    // --- Normal content tokens ---
                                    if let Some(token) = delta.get("content").and_then(|t| t.as_str()) {
                                        if !token.is_empty() {
                                            if in_reasoning {
                                                // Close thinking block before first content token
                                                let _ = tx.send("</think>".to_string()).await;
                                                in_reasoning = false;
                                            }
                                            full_content.push_str(token);
                                            let _ = tx.send(token.to_string()).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Close reasoning block if stream ended while still in reasoning
        if in_reasoning {
            let _ = tx.send("</think>".to_string()).await;
        }

        Ok(full_content)
    }

    /// Outer wrapper that manages retries and key rotations.
    pub async fn chat_stream_with_retry(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
        _max_retries: u8,
    ) -> Result<String> {
        let mut attempt = 0;

        loop {
            match self.chat_stream_internal(request, tx.clone()).await {
                Ok(content) => return Ok(content),
                Err(e) => {
                    attempt += 1;
                    let err_msg = e.to_string();
                    let is_rate_limited = err_msg.contains("429") || err_msg.contains("rate limit");
                    let total_valid_keys = self.total_valid_keys();

                    let max_attempts = if is_rate_limited {
                        std::cmp::max(4, std::cmp::max(2, total_valid_keys))
                    } else {
                        std::cmp::max(2, total_valid_keys)
                    };

                    if attempt >= max_attempts {
                        error!("Max attempts ({}) exceeded on this model for OpenRouter. Swapping model. Error: {}", max_attempts, e);
                        return Err(e);
                    }

                    if err_msg.contains("401") || err_msg.contains("403") || err_msg.contains("Unauthorized") || err_msg.contains("Forbidden") {
                        if let Some(active_key) = self.get_active_key() {
                            self.mark_key_invalid(&active_key);
                        }
                    }

                    self.rotate_key();

                    let current_valid_keys = self.total_valid_keys();
                    if current_valid_keys <= 1 || attempt >= current_valid_keys {
                        let delay = if is_rate_limited {
                            Duration::from_millis(1500u64 << (attempt - 1))
                        } else {
                            Duration::from_millis(100u64 << attempt)
                        };
                        warn!(attempt, delay_ms = delay.as_millis(), error = %e, "Retrying same key(s) with backoff...");
                        sleep(delay).await;
                    } else {
                        warn!(attempt, error = %e, "Rotating to next API key immediately...");
                    }
                }
            }
        }
    }
}

#[async_trait]
impl ModelProvider for OpenRouterClient {
    fn provider_name(&self) -> &str {
        "openrouter"
    }

    fn is_available(&self) -> bool {
        self.get_active_key().is_some()
    }

    async fn chat_stream(
        &self,
        request: &LLMRequest,
        token_tx: mpsc::Sender<String>,
    ) -> Result<String> {
        self.chat_stream_with_retry(request, token_tx, 3).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_url_trims_chat_completions_suffix() {
        assert_eq!(
            normalize_base_url("https://openrouter.ai/api/v1/chat/completions"),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            normalize_base_url("https://openrouter.ai/api/v1/chat/completions/"),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(normalize_base_url("https://openrouter.ai/api/v1/"), "https://openrouter.ai/api/v1");
    }

    #[test]
    fn resolve_reasoning_options_uses_explicit_effort_when_provided() {
        let client = OpenRouterClient::new(vec!["test-key".to_string()]);
        let request = LLMRequest {
            model: "openai/gpt-4o-mini".to_string(),
            messages: vec![],
            stream: true,
            max_tokens: None,
            models: vec![],
            reasoning_effort: Some("high".to_string()),
        };

        let (include_reasoning, reasoning) = client.resolve_reasoning_options(&request, true);
        assert_eq!(include_reasoning, None);
        assert_eq!(reasoning.as_ref().map(|cfg| cfg.effort.as_str()), Some("high"));
    }
}
