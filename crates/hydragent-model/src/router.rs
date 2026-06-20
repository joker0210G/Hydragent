use std::sync::Arc;
use tokio::sync::mpsc;
use crate::model_trait::ModelProvider;
use crate::openrouter::{LLMRequest, ChatMessage};
use anyhow::Result;
use tracing::{info, warn};

pub struct ModelRouter {
    provider: Arc<dyn ModelProvider>,
    primary: std::sync::RwLock<String>,
    fallbacks: Vec<String>,
}

impl ModelRouter {
    pub fn new(provider: Arc<dyn ModelProvider>, primary: String, fallbacks: Vec<String>) -> Self {
        Self {
            provider,
            primary: std::sync::RwLock::new(primary),
            fallbacks,
        }
    }

    /// Update the primary model at runtime (e.g. from the REPL `/model` command).
    pub fn set_primary_model(&self, model: String) {
        if let Ok(mut guard) = self.primary.write() {
            *guard = model;
        }
    }

    /// Read the current primary model name.
    pub fn primary_model(&self) -> String {
        self.primary.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Human-readable name of the underlying provider (e.g. "openrouter",
    /// "custom-openai", "ollama"). Useful for logging when a "librarian"
    /// role is routed through a different provider than the primary.
    pub fn provider_label(&self) -> &str {
        self.provider.provider_name()
    }

    /// Direct access to the underlying `ModelProvider`. Use this when
    /// you need to bypass the router's primary+fallback chain and
    /// call the provider with a fully-formed `LLMRequest` of your
    /// own (e.g. the swarm supervisor's synthesis call).
    pub fn provider(&self) -> Arc<dyn ModelProvider> {
        self.provider.clone()
    }

    /// Stream a chat completion to `token_tx`, trying `primary` first
    /// and falling back to each entry in `fallbacks` in order.
    ///
    /// If `override_model` is `Some(model_id)`, that model is tried
    /// first **and** the fallbacks are skipped (we treat caller
    /// override as an explicit, non-negotiable pick).  An override is
    /// what the [`crate::council::ModelCouncil`] returns from
    /// `route(...)` — the council has already done the matching work,
    /// and silently swapping in a fallback would defeat the point.
    ///
    /// Returns `(content, model_used)`.
    pub async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        token_tx: mpsc::Sender<String>,
        override_model: Option<&str>,
    ) -> Result<(String, String)> {
        // Override path: try the caller-specified model, no fallback.
        if let Some(model) = override_model {
            info!("Routing to override model: {}", model);
            let request = LLMRequest {
                model: model.to_string(),
                messages: messages.clone(),
                stream: true,
                max_tokens: None,
            };
            let content = self.provider.chat_stream(&request, token_tx).await?;
            return Ok((content, model.to_string()));
        }

        // Primary + fallback path.
        let primary = self.primary_model();
        info!("Attempting primary model: {}", primary);
        let mut request = LLMRequest {
            model: primary.clone(),
            messages: messages.clone(),
            stream: true,
            max_tokens: None,
        };

        match self.provider.chat_stream(&request, token_tx.clone()).await {
            Ok(content) => return Ok((content, primary.clone())),
            Err(e) => {
                warn!("Primary model {} failed: {}. Initiating fallbacks...", primary, e);
            }
        }

        // Loop through fallbacks
        for fallback in &self.fallbacks {
            info!("Attempting fallback model: {}", fallback);
            request.model = fallback.clone();
            match self.provider.chat_stream(&request, token_tx.clone()).await {
                Ok(content) => return Ok((content, fallback.clone())),
                Err(e) => {
                    warn!("Fallback model {} failed: {}", fallback, e);
                }
            }
        }

        anyhow::bail!("All models (primary: {}, fallbacks: {:?}) failed to execute completion.", primary, self.fallbacks)
    }

    /// Non-streaming convenience wrapper.  See [`Self::chat_stream`]
    /// for the `override_model` semantics.
    pub async fn generate_non_streaming(
        &self,
        prompt: &str,
        override_model: Option<&str>,
    ) -> Result<String> {
        let (tx, _rx) = mpsc::channel(100);
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        }];
        let (content, _) = self.chat_stream(messages, tx, override_model).await?;
        Ok(content)
    }
}
