use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Duration;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;
use tracing::info;

use crate::model_trait::ModelProvider;
use crate::openrouter::{LLMRequest, ChatMessage};

#[derive(Debug, Clone)]
pub struct OllamaProviderConfig {
    pub base_url: String,
    pub default_model: String,
    pub timeout: Duration,
    pub default_num_ctx: u32,
    pub keep_alive: Option<String>,
    pub num_thread: Option<u32>,
    pub temperature: Option<f32>,
    pub repeat_penalty: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
}

impl OllamaProviderConfig {
    pub fn from_env() -> Self {
        let base_url = std::env::var("OLLAMA_API_BASE")
            .or_else(|_| std::env::var("BRAIN_BASE"))
            .unwrap_or_else(|_| "http://localhost:11434".to_string())
            .trim_end_matches('/')
            .replace("/v1", "");

        let default_model = std::env::var("OLLAMA_MODEL")
            .or_else(|_| std::env::var("BRAIN_MODEL"))
            .unwrap_or_else(|_| "llama3.1:8b".to_string());

        let timeout_secs = std::env::var("OLLAMA_API_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(300);

        let default_num_ctx = std::env::var("OLLAMA_NUM_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(8192);

        let keep_alive = std::env::var("OLLAMA_KEEP_ALIVE")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let num_thread = std::env::var("OLLAMA_NUM_THREAD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok());

        let temperature = std::env::var("OLLAMA_TEMPERATURE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok());

        let repeat_penalty = std::env::var("OLLAMA_REPEAT_PENALTY")
            .ok()
            .and_then(|s| s.parse::<f32>().ok());

        let top_p = std::env::var("OLLAMA_TOP_P")
            .ok()
            .and_then(|s| s.parse::<f32>().ok());

        let top_k = std::env::var("OLLAMA_TOP_K")
            .ok()
            .and_then(|s| s.parse::<u32>().ok());

        Self {
            base_url,
            default_model,
            timeout: Duration::from_secs(timeout_secs),
            default_num_ctx,
            keep_alive,
            num_thread,
            temperature,
            repeat_penalty,
            top_p,
            top_k,
        }
    }
}

pub struct OllamaClient {
    config: OllamaProviderConfig,
    client: Client,
    // Cache for model context windows: model_name -> context_limit
    context_cache: RwLock<HashMap<String, u32>>,
}

#[derive(Debug, Serialize)]
struct OllamaChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    num_ctx: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_thread: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    options: OllamaChatOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<String>,
    /// Enable native thinking for supported models.
    /// NEVER combine with format:"json" — known Ollama bug that kills content output.
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
}

/// Each streamed chunk from Ollama /api/chat.
#[derive(Debug, Deserialize)]
struct OllamaChatResponseChunk {
    message: Option<OllamaMessageChunk>,
    #[serde(default)]
    done: bool,
    prompt_eval_count: Option<u32>,
    eval_count: Option<u32>,
    total_duration: Option<u64>,
}

/// Fields default to empty string so we can use .is_empty() safely.
#[derive(Debug, Deserialize, Default)]
struct OllamaMessageChunk {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: String,
}

#[derive(Debug, Serialize)]
struct OllamaShowRequest<'a> {
    model: &'a str,
}

#[derive(Debug, Deserialize)]
struct OllamaShowResponse {
    model_info: Option<HashMap<String, serde_json::Value>>,
}

impl OllamaClient {
    pub fn new(config: OllamaProviderConfig) -> Self {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            config,
            client,
            context_cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        Self::new(OllamaProviderConfig::from_env())
    }

    fn resolve_model<'a>(&'a self, requested: &'a str) -> &'a str {
        if requested.is_empty() {
            self.config.default_model.as_str()
        } else {
            requested
        }
    }

    /// Returns true if the model name suggests native thinking capability.
    fn model_supports_thinking(model: &str) -> bool {
        let m = model.to_lowercase();
        m.contains("deepseek-r1")
            || m.contains("qwq")
            || m.contains("qwen3")
            || m.contains("marco-o1")
            || m.contains("cogito")
            || m.contains("exaone-deep")
    }

    /// Query Ollama's /api/show endpoint to find the native context length limit of the model.
    async fn fetch_model_context_limit(&self, model: &str) -> u32 {
        // Check cache first
        if let Ok(cache) = self.context_cache.read() {
            if let Some(&limit) = cache.get(model) {
                return limit;
            }
        }

        let url = format!("{}/api/show", self.config.base_url);
        let req_body = OllamaShowRequest { model };

        let limit = match self.client.post(&url)
            .timeout(Duration::from_secs(2))
            .json(&req_body)
            .send()
            .await {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(show_info) = resp.json::<OllamaShowResponse>().await {
                        if let Some(info) = show_info.model_info {
                            // Look for any key ending in ".context_length"
                            info.iter()
                                .find(|(k, _)| k.ends_with(".context_length"))
                                .and_then(|(_, val)| val.as_u64())
                                .map(|v| v as u32)
                                .unwrap_or(self.config.default_num_ctx)
                        } else {
                            self.config.default_num_ctx
                        }
                    } else {
                        self.config.default_num_ctx
                    }
                } else {
                    self.config.default_num_ctx
                }
            }
            Err(_) => self.config.default_num_ctx,
        };

        // Populate cache
        if let Ok(mut cache) = self.context_cache.write() {
            cache.insert(model.to_string(), limit);
            info!("Queried Ollama for model '{}' context limit: {} tokens", model, limit);
        }

        limit
    }

    async fn stream_completion(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
    ) -> Result<String> {
        let model = self.resolve_model(&request.model);

        // Dynamically get the context limit of the model (fast, cached, short timeout)
        let num_ctx = self.fetch_model_context_limit(model).await;

        // Only enable native thinking for known reasoning models.
        // IMPORTANT: Do NOT pass format:"json" when think is set —
        // combining format+think is a known Ollama bug that produces zero content.
        let think = if Self::model_supports_thinking(model) {
            Some(true)
        } else {
            None
        };

        let body = OllamaChatRequest {
            model,
            messages: &request.messages,
            stream: true, // always stream through tx channel
            options: OllamaChatOptions {
                num_ctx: Some(num_ctx),
                num_thread: self.config.num_thread,
                num_predict: request.max_tokens.map(|t| t as i32),
                temperature: self.config.temperature,
                repeat_penalty: self.config.repeat_penalty,
                top_p: self.config.top_p,
                top_k: self.config.top_k,
            },
            keep_alive: self.config.keep_alive.clone(),
            think,
        };

        let url = format!("{}/api/chat", self.config.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Ollama provider request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Ollama provider error response ({}): {}",
                status,
                error_text
            );
        }

        let mut full_content = String::new();
        let mut stream = resp.bytes_stream();
        let mut in_thinking = false;

        let mut byte_buffer: Vec<u8> = Vec::new();
        info!("Ollama streaming started for model: {}", model);
        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("Ollama chunk error")?;
            byte_buffer.extend_from_slice(&bytes);

            while let Some(newline_idx) = byte_buffer.iter().position(|&b| b == b'\n') {
                let line_bytes = byte_buffer[..newline_idx].to_vec();
                byte_buffer.drain(..=newline_idx);

                let line = String::from_utf8(line_bytes)?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<OllamaChatResponseChunk>(&line) {
                    Ok(chunk_data) => {
                        if let Some(msg) = chunk_data.message {
                            // Native thinking field (deepseek-r1, qwq, qwen3 etc.)
                            if !msg.thinking.is_empty() {
                                if !in_thinking {
                                    in_thinking = true;
                                    let _ = tx.send("<think>".to_string()).await;
                                }
                                full_content.push_str(&msg.thinking);
                                let _ = tx.send(msg.thinking).await;
                            }

                            // Regular content
                            if !msg.content.is_empty() {
                                if in_thinking {
                                    in_thinking = false;
                                    let _ = tx.send("</think>".to_string()).await;
                                }
                                full_content.push_str(&msg.content);
                                let _ = tx.send(msg.content).await;
                            }
                        }
                        if chunk_data.done {
                            if in_thinking {
                                let _ = tx.send("</think>".to_string()).await;
                            }
                            let prompt_tokens = chunk_data.prompt_eval_count.unwrap_or(0);
                            let completion_tokens = chunk_data.eval_count.unwrap_or(0);
                            let duration_ms = chunk_data.total_duration.map(|ns| ns / 1_000_000).unwrap_or(0);
                            info!(
                                model = %model,
                                prompt_tokens = %prompt_tokens,
                                completion_tokens = %completion_tokens,
                                duration_ms = %duration_ms,
                                "Ollama generation completed"
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        info!("Ollama parse skip: {} | {}", e, &line[..line.len().min(120)]);
                    }
                }
            }
        }

        // Process any trailing line without newline
        if !byte_buffer.is_empty() {
            if let Ok(line) = String::from_utf8(byte_buffer) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if let Ok(chunk_data) = serde_json::from_str::<OllamaChatResponseChunk>(&line) {
                        if let Some(msg) = chunk_data.message {
                            if !msg.content.is_empty() {
                                full_content.push_str(&msg.content);
                                let _ = tx.send(msg.content).await;
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
impl ModelProvider for OllamaClient {
    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn is_available(&self) -> bool {
        !self.config.base_url.is_empty()
    }

    async fn chat_stream(
        &self,
        request: &LLMRequest,
        token_tx: mpsc::Sender<String>,
    ) -> Result<String> {
        self.stream_completion(request, token_tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_supports_thinking() {
        assert!(OllamaClient::model_supports_thinking("deepseek-r1:8b"));
        assert!(OllamaClient::model_supports_thinking("deepseek-r1:32b"));
        assert!(OllamaClient::model_supports_thinking("qwq:latest"));
        assert!(OllamaClient::model_supports_thinking("qwen3-coder"));
        assert!(OllamaClient::model_supports_thinking("marco-o1"));
        assert!(!OllamaClient::model_supports_thinking("llama3.1:8b"));
        assert!(!OllamaClient::model_supports_thinking("mistral:latest"));
    }

    #[test]
    fn test_ollama_provider_config_env_suite() {
        // Clear env vars to test defaults
        std::env::remove_var("OLLAMA_API_BASE");
        std::env::remove_var("BRAIN_BASE");
        std::env::remove_var("OLLAMA_MODEL");
        std::env::remove_var("BRAIN_MODEL");
        std::env::remove_var("OLLAMA_API_TIMEOUT_SEC");
        std::env::remove_var("OLLAMA_NUM_CTX");
        std::env::remove_var("OLLAMA_KEEP_ALIVE");
        std::env::remove_var("OLLAMA_NUM_THREAD");
        std::env::remove_var("OLLAMA_TEMPERATURE");
        std::env::remove_var("OLLAMA_REPEAT_PENALTY");
        std::env::remove_var("OLLAMA_TOP_P");
        std::env::remove_var("OLLAMA_TOP_K");

        let cfg = OllamaProviderConfig::from_env();
        assert_eq!(cfg.base_url, "http://localhost:11434");
        assert_eq!(cfg.default_model, "llama3.1:8b");
        assert_eq!(cfg.timeout, Duration::from_secs(300));
        assert_eq!(cfg.default_num_ctx, 8192);
        assert!(cfg.keep_alive.is_none());
        assert!(cfg.num_thread.is_none());
        assert!(cfg.temperature.is_none());
        assert!(cfg.repeat_penalty.is_none());
        assert!(cfg.top_p.is_none());
        assert!(cfg.top_k.is_none());

        // Test overrides
        std::env::set_var("OLLAMA_API_BASE", "http://ollama-host:11434/");
        std::env::set_var("OLLAMA_MODEL", "custom-model:latest");
        std::env::set_var("OLLAMA_API_TIMEOUT_SEC", "120");
        std::env::set_var("OLLAMA_NUM_CTX", "16384");
        std::env::set_var("OLLAMA_KEEP_ALIVE", "15m");
        std::env::set_var("OLLAMA_NUM_THREAD", "4");
        std::env::set_var("OLLAMA_TEMPERATURE", "0.7");
        std::env::set_var("OLLAMA_REPEAT_PENALTY", "1.1");
        std::env::set_var("OLLAMA_TOP_P", "0.9");
        std::env::set_var("OLLAMA_TOP_K", "40");

        let cfg = OllamaProviderConfig::from_env();
        // trailing slash should be trimmed by config constructor
        assert_eq!(cfg.base_url, "http://ollama-host:11434");
        assert_eq!(cfg.default_model, "custom-model:latest");
        assert_eq!(cfg.timeout, Duration::from_secs(120));
        assert_eq!(cfg.default_num_ctx, 16384);
        assert_eq!(cfg.keep_alive, Some("15m".to_string()));
        assert_eq!(cfg.num_thread, Some(4));
        assert_eq!(cfg.temperature, Some(0.7));
        assert_eq!(cfg.repeat_penalty, Some(1.1));
        assert_eq!(cfg.top_p, Some(0.9));
        assert_eq!(cfg.top_k, Some(40));

        // Clean up env vars after test
        std::env::remove_var("OLLAMA_API_BASE");
        std::env::remove_var("OLLAMA_MODEL");
        std::env::remove_var("OLLAMA_API_TIMEOUT_SEC");
        std::env::remove_var("OLLAMA_NUM_CTX");
        std::env::remove_var("OLLAMA_KEEP_ALIVE");
        std::env::remove_var("OLLAMA_NUM_THREAD");
        std::env::remove_var("OLLAMA_TEMPERATURE");
        std::env::remove_var("OLLAMA_REPEAT_PENALTY");
        std::env::remove_var("OLLAMA_TOP_P");
        std::env::remove_var("OLLAMA_TOP_K");
    }

    #[test]
    fn test_parse_response_chunks() {
        let chunk_json = r#"{"message":{"role":"assistant","content":"hello"},"done":false}"#;
        let chunk: OllamaChatResponseChunk = serde_json::from_str(chunk_json).unwrap();
        assert!(!chunk.done);
        assert_eq!(chunk.message.unwrap().content, "hello");

        let done_json = r#"{"done":true,"prompt_eval_count":20,"eval_count":10,"total_duration":10000000}"#;
        let chunk: OllamaChatResponseChunk = serde_json::from_str(done_json).unwrap();
        assert!(chunk.done);
        assert_eq!(chunk.prompt_eval_count, Some(20));
        assert_eq!(chunk.eval_count, Some(10));
        assert_eq!(chunk.total_duration, Some(10000000));
    }
}

