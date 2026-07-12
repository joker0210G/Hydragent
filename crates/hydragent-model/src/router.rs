use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use crate::model_trait::ModelProvider;
use crate::openrouter::{LLMRequest, ChatMessage};
use crate::registry::ProviderRegistry;
use anyhow::Result;
use tracing::{info, warn};

pub struct ModelRouter {
    provider: RwLock<Arc<dyn ModelProvider>>,
    registry: Option<Arc<ProviderRegistry>>,
    provider_cache: RwLock<std::collections::HashMap<String, Arc<dyn ModelProvider>>>,
    primary: std::sync::RwLock<String>,
    fallbacks: Vec<String>,
}

impl ModelRouter {
    pub fn new(provider: Arc<dyn ModelProvider>, primary: String, fallbacks: Vec<String>) -> Self {
        Self {
            provider: RwLock::new(provider),
            registry: None,
            provider_cache: RwLock::new(std::collections::HashMap::new()),
            primary: std::sync::RwLock::new(primary),
            fallbacks,
        }
    }

    pub fn new_with_registry(
        provider: Arc<dyn ModelProvider>,
        registry: Arc<ProviderRegistry>,
        primary: String,
        fallbacks: Vec<String>,
    ) -> Self {
        Self {
            provider: RwLock::new(provider),
            registry: Some(registry),
            provider_cache: RwLock::new(std::collections::HashMap::new()),
            primary: std::sync::RwLock::new(primary),
            fallbacks,
        }
    }

    /// Resolve and retrieve the model provider client dynamically, building and
    /// caching it from settings/environment keys when needed.
    pub fn get_or_build_provider(&self, provider_id: &str) -> Arc<dyn ModelProvider> {
        let norm_id = ProviderRegistry::normalize_provider_id(provider_id);
        
        let primary_provider = self.provider();
        if norm_id == primary_provider.provider_name() {
            return primary_provider;
        }

        // If no registry is available, fallback to the default provider
        let registry = match &self.registry {
            Some(r) => r,
            None => return primary_provider,
        };

        if let Ok(guard) = self.provider_cache.read() {
            if let Some(client) = guard.get(norm_id) {
                return client.clone();
            }
        }

        let provider_def = match registry.provider(norm_id) {
            Some(def) => def,
            None => return primary_provider,
        };

        let env_base_key = format!("BRAIN_{}_BASE", norm_id.to_uppercase().replace('-', "_"));
        let env_key_key = format!("BRAIN_{}_KEY", norm_id.to_uppercase().replace('-', "_"));
        
        let mut base_url = std::env::var(&env_base_key)
            .ok()
            .unwrap_or_else(|| provider_def.default_base_url.clone());
            
        let api_key = std::env::var(&env_key_key)
            .ok()
            .unwrap_or_else(|| std::env::var("BRAIN_KEY").unwrap_or_default());

        if norm_id == "ollama" && base_url.is_empty() {
            base_url = std::env::var("OLLAMA_API_BASE")
                .unwrap_or_else(|_| "http://localhost:11434".to_string());
        }

        let timeout_secs = if norm_id == "ollama" {
            std::env::var("OLLAMA_API_TIMEOUT_SEC")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(provider_def.timeout_secs)
        } else {
            provider_def.timeout_secs
        };

        let client = registry.build_provider(
            norm_id,
            &base_url,
            &api_key,
            "",
            timeout_secs,
        );

        if let Ok(mut guard) = self.provider_cache.write() {
            guard.insert(norm_id.to_string(), client.clone());
            client
        } else {
            client
        }
    }

    /// Update the underlying provider client dynamically at runtime.
    pub fn set_provider(&self, new_provider: Arc<dyn ModelProvider>) {
        if let Ok(mut guard) = self.provider.write() {
            *guard = new_provider;
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
    pub fn provider_label(&self) -> String {
        if let Ok(guard) = self.provider.read() {
            guard.provider_name().to_string()
        } else {
            "unknown".to_string()
        }
    }

    /// Direct access to the underlying `ModelProvider`. Use this when
    /// you need to bypass the router's primary + fallback chain and
    /// call the provider with a fully-formed `LLMRequest` of your
    /// own (e.g. the swarm supervisor's synthesis call).
    pub fn provider(&self) -> Arc<dyn ModelProvider> {
        self.provider.read().map(|g| g.clone()).unwrap_or_else(|_| {
            panic!("ModelRouter provider lock poisoned");
        })
    }

    /// Stream a chat completion to `token_tx`, trying `primary` first
    /// and falling back to each entry in `fallbacks` in order.
    ///
    /// If `override_model` is `Some(model_id)`, that model is tried
    /// first **and** the fallbacks are skipped (we treat caller
    /// override as an explicit, non-negotiable pick).
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
            
            let (provider_id, api_model_id) = if let Some(registry) = &self.registry {
                if let Some(resolved) = registry.resolve(model, None) {
                    (resolved.provider_id, resolved.api_model_id)
                } else {
                    let (p_id, m_id) = model.split_once('/').unwrap_or(("custom", model));
                    (p_id.to_string(), m_id.to_string())
                }
            } else {
                let (p_id, m_id) = model.split_once('/').unwrap_or(("custom", model));
                (p_id.to_string(), m_id.to_string())
            };

            let provider_client = self.get_or_build_provider(&provider_id);

            let request = LLMRequest {
                model: api_model_id.clone(),
                messages: messages.clone(),
                stream: true,
                max_tokens: None,
                models: vec![],
                reasoning_effort: None,
            };
            let content = provider_client.chat_stream(&request, token_tx).await?;
            return Ok((content, model.to_string()));
        }

        // Primary + fallback path.
        let primary = self.primary_model();
        info!("Attempting primary model: {}", primary);

        let (primary_provider_id, primary_api_model_id) = if let Some(registry) = &self.registry {
            if let Some(resolved) = registry.resolve(&primary, None) {
                (resolved.provider_id, resolved.api_model_id)
            } else {
                let (p_id, m_id) = primary.split_once('/').unwrap_or(("custom", &primary));
                (p_id.to_string(), m_id.to_string())
            }
        } else {
            let (p_id, m_id) = primary.split_once('/').unwrap_or(("custom", &primary));
            (p_id.to_string(), m_id.to_string())
        };

        let primary_client = self.get_or_build_provider(&primary_provider_id);

        let mut request = LLMRequest {
            model: primary_api_model_id.clone(),
            messages: messages.clone(),
            stream: true,
            max_tokens: None,
            // Include primary + all fallbacks — OpenRouter tries them in order
            models: std::iter::once(primary_api_model_id.clone())
                .chain(self.fallbacks.iter().cloned())
                .collect(),
            reasoning_effort: None,
        };

        match primary_client.chat_stream(&request, token_tx.clone()).await {
            Ok(content) => return Ok((content, primary.clone())),
            Err(e) => {
                warn!("Primary model {} failed: {}. Initiating fallbacks...", primary, e);
            }
        }

        // Loop through fallbacks
        for fallback in &self.fallbacks {
            info!("Attempting fallback model: {}", fallback);
            
            let (fb_provider_id, fb_api_model_id) = if let Some(registry) = &self.registry {
                if let Some(resolved) = registry.resolve(fallback, None) {
                    (resolved.provider_id, resolved.api_model_id)
                } else {
                    let (p_id, m_id) = fallback.split_once('/').unwrap_or(("custom", fallback));
                    (p_id.to_string(), m_id.to_string())
                }
            } else {
                let (p_id, m_id) = fallback.split_once('/').unwrap_or(("custom", fallback));
                (p_id.to_string(), m_id.to_string())
            };

            let fallback_client = self.get_or_build_provider(&fb_provider_id);
            request.model = fb_api_model_id.clone();
            
            match fallback_client.chat_stream(&request, token_tx.clone()).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ProviderRegistry;

    #[test]
    fn test_model_router_with_registry() {
        let registry = Arc::new(ProviderRegistry::builtin_default());
        
        let primary = registry.build_provider("ollama", "http://localhost:11434/v1", "", "llama3.1", 180);
        let router = ModelRouter::new_with_registry(
            primary,
            registry,
            "ollama/llama3.1".to_string(),
            vec!["openrouter/openai-gpt-4o-mini".to_string()],
        );

        assert_eq!(router.primary_model(), "ollama/llama3.1");

        // Verify dynamic builder lookup gets custom keys
        std::env::set_var("BRAIN_OPENAI_KEY", "sk-proj-testkey");
        std::env::set_var("BRAIN_OPENAI_BASE", "https://api.openai.com/v2");

        let openai_client = router.get_or_build_provider("openai");
        assert_eq!(openai_client.provider_name(), "openai");

        std::env::remove_var("BRAIN_OPENAI_KEY");
        std::env::remove_var("BRAIN_OPENAI_BASE");
    }
}


