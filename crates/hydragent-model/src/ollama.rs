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

        Self {
            base_url,
            default_model,
            timeout: Duration::from_secs(timeout_secs),
            default_num_ctx,
            keep_alive,
            num_thread,
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

        let mut line_buffer = String::new();
        info!("Ollama streaming started for model: {}", model);
        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("Ollama chunk error")?;
            let text = std::str::from_utf8(&bytes)?;
            line_buffer.push_str(text);

            while let Some(newline_idx) = line_buffer.find('\n') {
                let line = line_buffer[..newline_idx].to_string();
                line_buffer.drain(..=newline_idx);

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
                            break;
                        }
                    }
                    Err(e) => {
                        info!("Ollama parse skip: {} | {}", e, &line[..line.len().min(120)]);
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
