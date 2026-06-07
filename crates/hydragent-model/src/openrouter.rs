use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::{warn, error};

use std::sync::atomic::{AtomicUsize, Ordering};
use async_trait::async_trait;
use crate::model_trait::ModelProvider;

pub struct OpenRouterClient {
    api_keys: Vec<String>,
    active_key_index: AtomicUsize,
    client: Client,
    base_url: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct LLMRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,   // "system" | "user" | "assistant" | "tool"
    pub content: String,
}

impl OpenRouterClient {
    pub fn new(api_keys: Vec<String>) -> Self {
        Self {
            api_keys,
            active_key_index: AtomicUsize::new(0),
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
            base_url: "https://openrouter.ai/api/v1".into(),
        }
    }


    /// Retrieve the current active API key.
    fn get_active_key(&self) -> Option<&String> {
        if self.api_keys.is_empty() {
            return None;
        }
        let index = self.active_key_index.load(Ordering::Relaxed);
        Some(&self.api_keys[index % self.api_keys.len()])
    }

    /// Increment active key index to rotate to the next key.
    fn rotate_key(&self) {
        if self.api_keys.len() > 1 {
            let old_idx = self.active_key_index.fetch_add(1, Ordering::Relaxed);
            let new_idx = (old_idx + 1) % self.api_keys.len();
            warn!("Rotating OpenRouter API key from index {} to {}", old_idx % self.api_keys.len(), new_idx);
        }
    }

    /// Stream a chat completion. Sends tokens to `tx` as they arrive.
    /// Returns the full concatenated response when the stream ends.
    pub async fn chat_stream_internal(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
    ) -> Result<String> {
        let api_key = self.get_active_key()
            .context("No OpenRouter API keys available in configuration")?;

        let resp = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(api_key)
            .header("HTTP-Referer", "https://github.com/joker0210G/Hydragent")
            .header("X-Title", "Hydragent")
            .json(request)
            .send()
            .await
            .context("OpenRouter request failed")?;

        // If rate limited, throw rate limit error so the caller retry loop rotates the key
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!("HTTP 429: Rate limited");
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let error_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter error response ({}): {}", status, error_text);
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
                        // Check if response contains an API level error (like billing/rate limits inside JSON)
                        if let Some(err) = v.get("error") {
                            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
                            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                            if code == 429 || msg.contains("rate limit") || msg.contains("credits") {
                                anyhow::bail!("OpenRouter API level rate limit/credit error: {}", msg);
                            }
                        }

                        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                            if !choices.is_empty() {
                                if let Some(delta) = choices[0].get("delta") {
                                    if let Some(token) = delta.get("content").and_then(|t| t.as_str()) {
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
        Ok(full_content)
    }

    /// Outer wrapper that manages retries and key rotations.
    pub async fn chat_stream_with_retry(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
        max_retries: u8,
    ) -> Result<String> {
        let mut attempt = 0;
        loop {
            match self.chat_stream_internal(request, tx.clone()).await {
                Ok(content) => return Ok(content),
                Err(e) => {
                    attempt += 1;
                    if attempt >= max_retries {
                        error!("Max retries ({}) exceeded for OpenRouter: {}", max_retries, e);
                        return Err(e);
                    }

                    // Rotate the API key if rate limited or if we suspect credential issues
                    let err_msg = e.to_string();
                    if err_msg.contains("429") || err_msg.contains("rate limit") || err_msg.contains("credits") || err_msg.contains("401") {
                        self.rotate_key();
                    }

                    let delay = Duration::from_millis(100u64 << attempt);
                    warn!(attempt, delay_ms = delay.as_millis(), error = %e, "Retrying request...");
                    sleep(delay).await;
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
