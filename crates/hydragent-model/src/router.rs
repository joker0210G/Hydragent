use std::sync::Arc;
use tokio::sync::mpsc;
use crate::model_trait::ModelProvider;
use crate::openrouter::{LLMRequest, ChatMessage};
use anyhow::Result;
use tracing::{info, warn};

pub struct ModelRouter {
    provider: Arc<dyn ModelProvider>,
    primary: String,
    fallbacks: Vec<String>,
}

impl ModelRouter {
    pub fn new(provider: Arc<dyn ModelProvider>, primary: String, fallbacks: Vec<String>) -> Self {
        Self {
            provider,
            primary,
            fallbacks,
        }
    }

    pub async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        token_tx: mpsc::Sender<String>,
    ) -> Result<(String, String)> {
        // Attempt primary model first
        info!("Attempting primary model: {}", self.primary);
        let mut request = LLMRequest {
            model: self.primary.clone(),
            messages: messages.clone(),
            stream: true,
            max_tokens: None,
        };

        match self.provider.chat_stream(&request, token_tx.clone()).await {
            Ok(content) => return Ok((content, self.primary.clone())),
            Err(e) => {
                warn!("Primary model {} failed: {}. Initiating fallbacks...", self.primary, e);
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

        anyhow::bail!("All models (primary and fallback) failed to execute completion.")
    }
}
