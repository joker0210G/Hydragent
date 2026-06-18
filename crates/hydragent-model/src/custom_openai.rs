// crates/hydragent-model/src/custom_openai.rs
//
// Generic OpenAI-compatible chat completions client.
//
// Most LLM providers in 2026 ship an OpenAI-compatible `/v1/chat/completions`
// endpoint (OpenAI itself, OpenRouter, Together AI, Groq, Fireworks, Anyscale,
// vLLM, LM Studio, llama.cpp, Ollama in OpenAI mode, Mistral, DeepSeek,
// Perplexity, etc.). This provider speaks that wire format, so any of them can
// be plugged in by setting three environment variables:
//
//   CUSTOM_API_BASE = "https://api.together.xyz/v1"   (no trailing slash)
//   CUSTOM_API_KEY  = "sk-..."
//   CUSTOM_MODEL    = "meta-llama/Llama-3-70b-chat-hf"
//
// Streaming is the same `data: {json}` SSE format that the OpenRouter client
// already parses, so this file is a slim fork of the OpenRouter client without
// the multi-key rotation / vault injection logic (those are still the main
// primary-model concerns).
//
// The "librarian" role in Hydragent (memory extraction / consolidation in
// `hydragent-core::dream`) is wired to this provider when its key is set,
// giving the user a place to mount a cheaper / limited-time promotional model.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::{warn, error};
use async_trait::async_trait;

use crate::model_trait::ModelProvider;
use crate::openrouter::{LLMRequest, ChatMessage};
use zeroize::Zeroize;

// `CustomProviderConfig` deliberately does **not** derive `Debug` because it
// carries the `api_key` bearer token. The manual `Debug` impl below
// redacts `api_key` with `mask_api_key_for_debug` so that no future
// `format!("{:?}", cfg)` call site can accidentally leak a secret. (See
// regression test `custom_provider_config_debug_redacts_api_key`.)
#[derive(Clone)]
pub struct CustomProviderConfig {
    /// Base URL of the OpenAI-compatible API, e.g. "https://api.together.xyz/v1"
    pub base_url: String,
    /// API key / bearer token
    pub api_key: String,
    /// Default model identifier (overridable per-request by the router)
    pub default_model: String,
    /// Optional provider tag surfaced in logs / headers
    pub provider_label: String,
    /// Total request timeout
    pub timeout: Duration,
    /// Number of retry attempts on transient failure (429 / 5xx / network)
    pub max_retries: u8,
}

/// Redact a secret string for log output. Same policy as
/// `hydragent_core::config::AppConfig::mask_key` — kept local so this
/// module has no cross-crate dependency for diagnostics.
fn mask_api_key_for_debug(s: &str) -> String {
    if s.is_empty() {
        return "<empty>".to_string();
    }
    let n = s.chars().count();
    if n <= 12 {
        return format!("<set> ({} chars)", n);
    }
    let head: String = s.chars().take(4).collect();
    let tail_rev: String = s
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{}…{} ({} chars)", head, tail_rev, n)
}

impl std::fmt::Debug for CustomProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomProviderConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &mask_api_key_for_debug(&self.api_key))
            .field("default_model", &self.default_model)
            .field("provider_label", &self.provider_label)
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

impl CustomProviderConfig {
    /// Build from the env vars the user sets. Returns `None` if no key is
    /// present (the caller should then fall back to the primary provider).
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("CUSTOM_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;

        let base_url = std::env::var("CUSTOM_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string())
            .trim_end_matches('/')
            .to_string();

        let default_model = std::env::var("CUSTOM_MODEL")
            .unwrap_or_else(|_| "gpt-4o-mini".to_string());

        let provider_label = std::env::var("CUSTOM_PROVIDER_LABEL")
            .unwrap_or_else(|_| "custom-openai".to_string());

        let timeout_secs = std::env::var("CUSTOM_API_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60);

        let max_retries = std::env::var("CUSTOM_API_MAX_RETRIES")
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(3);

        Some(Self {
            base_url,
            api_key,
            default_model,
            provider_label,
            timeout: Duration::from_secs(timeout_secs),
            max_retries,
        })
    }
}

pub struct CustomOpenAIClient {
    config: CustomProviderConfig,
    client: Client,
}

/// Wire body for OpenAI-compatible `/v1/chat/completions`.
///
/// We intentionally do **not** import the upstream `openai_dive` crate — the
/// endpoint only needs three fields in practice and rolling our own keeps the
/// dependency surface minimal.
#[derive(Debug, Serialize)]
struct OpenAIChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

impl CustomOpenAIClient {
    pub fn new(config: CustomProviderConfig) -> Self {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { config, client }
    }

    /// Build from environment. Returns `None` if `CUSTOM_API_KEY` is unset —
    /// callers can then skip wiring the librarian router.
    pub fn from_env() -> Option<Self> {
        CustomProviderConfig::from_env().map(Self::new)
    }

    pub fn config(&self) -> &CustomProviderConfig {
        &self.config
    }

    /// Apply a few aliases so `generate_non_streaming` and short model names
    /// work transparently with whichever provider the user plugged in.
    fn resolve_model<'a>(&'a self, requested: &'a str) -> &'a str {
        if requested.is_empty() {
            self.config.default_model.as_str()
        } else {
            requested
        }
    }

    async fn stream_completion(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
    ) -> Result<String> {
        let model = self.resolve_model(&request.model);
        let body = OpenAIChatRequest {
            model,
            messages: &request.messages,
            stream: request.stream,
            max_tokens: request.max_tokens,
            temperature: None,
        };
        let mut json_body = serde_json::to_string(&body)?;
        // We are about to put the body on the wire; zeroize the buffer after.
        let mut tainted_body = hydragent_vault::TaintedString::new(json_body.clone());
        json_body.zeroize();

        let url = format!("{}/chat/completions", self.config.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .header("Content-Type", "application/json")
            .header("X-Provider", &self.config.provider_label)
            .body(tainted_body.expose_secret().to_string())
            .send()
            .await
            .context("Custom provider request failed")?;
        tainted_body.zeroize();

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!("HTTP 429: Rate limited");
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let error_text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Custom provider ({}) error response ({}): {}",
                self.config.provider_label,
                status,
                error_text
            );
        }

        let mut full_content = String::new();
        let mut stream = resp.bytes_stream();

        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("SSE chunk error")?;
            let text = std::str::from_utf8(&bytes)?;

            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    let trimmed = data.trim();
                    if trimmed == "[DONE]" {
                        break;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                        if let Some(err) = v.get("error") {
                            let msg = err
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("Unknown error");
                            anyhow::bail!(
                                "Custom provider ({}) API level error: {}",
                                self.config.provider_label,
                                msg
                            );
                        }
                        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                            if !choices.is_empty() {
                                if let Some(delta) = choices[0].get("delta") {
                                    if let Some(token) =
                                        delta.get("content").and_then(|t| t.as_str())
                                    {
                                        full_content.push_str(token);
                                        // The `chat_stream` trait contract
                                        // is `Sender<String>` where each
                                        // String is a plain token fragment
                                        // (may contain raw newlines, code
                                        // spans, etc.). The router / bus
                                        // adapter is responsible for any
                                        // wire framing (newline-delimited
                                        // JSON, length-prefixed, etc.) on
                                        // the *external* bus connection.
                                        // On this in-process mpsc channel
                                        // we send the token as-is.
                                        let _ = tx.send(token.to_string()).await;
                                    }
                                }
                                // Some providers (OpenRouter compatible) also
                                // return a final non-delta `message` object on
                                // the last chunk.
                                if let Some(message) = choices[0].get("message") {
                                    if let Some(token) =
                                        message.get("content").and_then(|t| t.as_str())
                                    {
                                        if !token.is_empty() && full_content.is_empty() {
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
        Ok(full_content)
    }
}

#[async_trait]
impl ModelProvider for CustomOpenAIClient {
    fn provider_name(&self) -> &str {
        &self.config.provider_label
    }

    fn is_available(&self) -> bool {
        !self.config.api_key.is_empty() && !self.config.base_url.is_empty()
    }

    async fn chat_stream(
        &self,
        request: &LLMRequest,
        token_tx: mpsc::Sender<String>,
    ) -> Result<String> {
        let mut attempt: u8 = 0;
        loop {
            match self.stream_completion(request, token_tx.clone()).await {
                Ok(content) => return Ok(content),
                Err(e) => {
                    attempt += 1;
                    if attempt > self.config.max_retries {
                        error!(
                            attempt,
                            provider = %self.config.provider_label,
                            error = %e,
                            "Custom provider: max retries exceeded"
                        );
                        return Err(e);
                    }
                    let delay = Duration::from_millis(150u64 * (1u64 << attempt));
                    warn!(
                        attempt,
                        provider = %self.config.provider_label,
                        delay_ms = delay.as_millis(),
                        error = %e,
                        "Custom provider: retrying after backoff"
                    );
                    sleep(delay).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrouter::ChatMessage;

    #[test]
    fn test_resolve_model_uses_default_when_empty() {
        let cfg = CustomProviderConfig {
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            default_model: "llama-3".to_string(),
            provider_label: "test".to_string(),
            timeout: Duration::from_secs(10),
            max_retries: 1,
        };
        let c = CustomOpenAIClient::new(cfg);
        assert_eq!(c.resolve_model(""), "llama-3");
        assert_eq!(c.resolve_model("explicit-model"), "explicit-model");
    }

    #[test]
    fn test_provider_name_and_availability() {
        let cfg = CustomProviderConfig {
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            default_model: "llama-3".to_string(),
            provider_label: "together".to_string(),
            timeout: Duration::from_secs(10),
            max_retries: 1,
        };
        let c = CustomOpenAIClient::new(cfg);
        assert_eq!(c.provider_name(), "together");
        assert!(c.is_available());

        let bad = CustomOpenAIClient::new(CustomProviderConfig {
            base_url: "".to_string(),
            api_key: "sk-test".to_string(),
            default_model: "x".to_string(),
            provider_label: "x".to_string(),
            timeout: Duration::from_secs(1),
            max_retries: 0,
        });
        assert!(!bad.is_available());
    }

    #[test]
    fn test_request_serialization_matches_openai_wire_format() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "hi".to_string(),
        }];
        let req = OpenAIChatRequest {
            model: "gpt-4o-mini",
            messages: &messages,
            stream: true,
            max_tokens: Some(64),
            temperature: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        // Must NOT include the optional `temperature` field at all.
        assert!(!s.contains("temperature"));
        assert!(s.contains("\"model\":\"gpt-4o-mini\""));
        assert!(s.contains("\"stream\":true"));
        assert!(s.contains("\"max_tokens\":64"));
        assert!(s.contains("\"messages\""));
    }

    // ── P0: API-key leak prevention ────────────────────────────────────
    //
    // Regression: the old code derived `Debug` on `CustomProviderConfig`,
    // so any `format!("{:?}", cfg)` would print the `api_key` in
    // plaintext. The manual `Debug` impl above redacts the field. These
    // tests pin the redaction so a future refactor can't quietly
    // re-introduce the leak.

    fn cfg_with_realistic_key() -> CustomProviderConfig {
        CustomProviderConfig {
            base_url: "https://api.together.xyz/v1".to_string(),
            // 32-char secret — should be redacted.
            api_key: "sk-together-ABCDefgh1234567890WXYZabcd".to_string(),
            default_model: "meta-llama/Llama-3-70b-chat-hf".to_string(),
            provider_label: "together".to_string(),
            timeout: Duration::from_secs(60),
            max_retries: 3,
        }
    }

    #[test]
    fn custom_provider_config_debug_redacts_api_key() {
        let cfg = cfg_with_realistic_key();
        let s = format!("{:?}", cfg);
        // The raw secret must NEVER appear in the Debug output.
        assert!(
            !s.contains("sk-together-ABCDefgh1234567890WXYZabcd"),
            "api_key leaked through Debug! output was: {s}"
        );
        // We should see the redaction sentinel.
        assert!(
            s.contains("…") && s.contains("chars"),
            "expected redaction marker (… + chars) in Debug output, got: {s}"
        );
    }

    #[test]
    fn custom_provider_config_debug_handles_empty_key() {
        let mut cfg = cfg_with_realistic_key();
        cfg.api_key = String::new();
        let s = format!("{:?}", cfg);
        assert!(s.contains("<empty>"), "empty sentinel missing from: {s}");
    }

    #[test]
    fn custom_provider_config_debug_handles_short_key() {
        let mut cfg = cfg_with_realistic_key();
        cfg.api_key = "short-key".to_string();
        let s = format!("{:?}", cfg);
        assert!(
            !s.contains("short-key"),
            "short key leaked through Debug! output was: {s}"
        );
        assert!(
            s.contains("<set>") && s.contains("9 chars"),
            "expected '<set> (9 chars)' redaction, got: {s}"
        );
    }

    #[test]
    fn custom_provider_config_debug_keeps_non_secret_fields_visible() {
        // Sanity check: base_url / model / label are still visible so
        // the log line remains useful for debugging.
        let cfg = cfg_with_realistic_key();
        let s = format!("{:?}", cfg);
        assert!(s.contains("https://api.together.xyz/v1"), "base_url missing");
        assert!(s.contains("meta-llama/Llama-3-70b-chat-hf"), "default_model missing");
        assert!(s.contains("together"), "provider_label missing");
        assert!(s.contains("CustomProviderConfig"), "struct name missing");
    }

    #[test]
    fn custom_openai_client_debug_does_not_leak() {
        // `CustomOpenAIClient` itself does not derive `Debug`, but a
        // user might write `println!("{:?}", client.config)`. Make
        // sure that round-trip stays redacted.
        let client = CustomOpenAIClient::new(cfg_with_realistic_key());
        let s = format!("{:?}", client.config);
        assert!(
            !s.contains("sk-together-ABCDefgh1234567890WXYZabcd"),
            "api_key leaked through client.config Debug! output was: {s}"
        );
    }
}
