use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::Duration;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

use crate::model_trait::ModelProvider;
use crate::openrouter::LLMRequest;
use crate::ollama_native::{
    OllamaNativeClient, OllamaNativeConfig, ThinkingMode,
    OllamaModelTag, OllamaModelInfo,
    fetch_model_info, resolve_thinking_param,
    OllamaServerStatus, check_ollama_server,
};

// ============================================================================
// Backward-Compatible Provider Config (now delegates to OllamaNativeConfig)
// ============================================================================

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
    // New fields for native support
    pub thinking_mode: ThinkingMode,
    pub auto_serve: bool,
    pub auto_discover: bool,
    pub structured_output: bool,
    pub native_tool_calling: bool,
    pub stream_buffer_ms: u64,
    pub context_length_override: Option<u32>,
    pub model_metadata_ttl_sec: u64,
}

impl OllamaProviderConfig {
    pub fn from_env() -> Self {
        let native = OllamaNativeConfig::from_env();
        Self::from_native(native)
    }

    pub fn from_native(native: OllamaNativeConfig) -> Self {
        Self {
            base_url: native.base_url.clone(),
            default_model: native.default_model.clone(),
            timeout: native.timeout,
            default_num_ctx: native.default_num_ctx,
            keep_alive: native.keep_alive.clone(),
            num_thread: native.num_thread,
            temperature: native.temperature,
            repeat_penalty: native.repeat_penalty,
            top_p: native.top_p,
            top_k: native.top_k,
            thinking_mode: native.thinking_mode,
            auto_serve: native.auto_serve,
            auto_discover: native.auto_discover,
            structured_output: native.structured_output,
            native_tool_calling: native.native_tool_calling,
            stream_buffer_ms: native.stream_buffer_ms,
            context_length_override: native.context_length_override,
            model_metadata_ttl_sec: native.model_metadata_ttl_sec,
        }
    }

    pub fn to_native(&self) -> OllamaNativeConfig {
        OllamaNativeConfig {
            base_url: self.base_url.clone(),
            default_model: self.default_model.clone(),
            timeout: self.timeout,
            default_num_ctx: self.default_num_ctx,
            keep_alive: self.keep_alive.clone(),
            num_thread: self.num_thread,
            temperature: self.temperature,
            repeat_penalty: self.repeat_penalty,
            top_p: self.top_p,
            top_k: self.top_k,
            thinking_mode: self.thinking_mode,
            auto_serve: self.auto_serve,
            auto_discover: self.auto_discover,
            structured_output: self.structured_output,
            native_tool_calling: self.native_tool_calling,
            stream_buffer_ms: self.stream_buffer_ms,
            context_length_override: self.context_length_override,
            model_metadata_ttl_sec: self.model_metadata_ttl_sec,
        }
    }
}

// ============================================================================
// Ollama Client (now delegates to OllamaNativeClient)
// ============================================================================

pub struct OllamaClient {
    native: OllamaNativeClient,
    // Keep context_cache for backward compat with external code that may read it
    context_cache: RwLock<HashMap<String, u32>>,
}

impl OllamaClient {
    pub fn new(config: OllamaProviderConfig) -> Self {
        let native = OllamaNativeClient::new(config.to_native());
        Self {
            native,
            context_cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        Self::new(OllamaProviderConfig::from_env())
    }

    pub fn native_client(&self) -> &OllamaNativeClient {
        &self.native
    }

    pub fn config(&self) -> &OllamaNativeConfig {
        self.native.config()
    }

    fn resolve_model<'a>(&'a self, requested: &'a str) -> &'a str {
        if requested.is_empty() {
            self.native.config().default_model.as_str()
        } else {
            requested
        }
    }

    /// Returns true if the model name suggests native thinking capability.
    pub fn model_supports_thinking(model: &str) -> bool {
        crate::ollama_native::model_supports_thinking(model)
    }

    /// Query Ollama's /api/show endpoint to find the native context length limit of the model.
    pub async fn fetch_model_context_limit(&self, model: &str) -> u32 {
        match self.native.get_model_info(model).await {
            Ok(info) => info.context_length,
            Err(_) => self.native.config().default_num_ctx,
        }
    }

    /// Discover available models from the Ollama server.
    pub async fn discover_models(&self) -> Result<Vec<OllamaModelTag>> {
        self.native.discover_models().await
    }

    /// Fetch detailed info about a specific model.
    pub async fn fetch_model_info(&self, model: &str) -> Result<OllamaModelInfo> {
        fetch_model_info(&self.native.config().base_url, model).await
    }

    /// Warm up a model by sending a minimal request.
    pub async fn warmup_model(&self, model: &str) -> Result<bool> {
        self.native.warmup_model(model).await
    }

    /// Check if the Ollama server is running.
    pub async fn check_server(&self) -> OllamaServerStatus {
        check_ollama_server(&self.native.config().base_url).await
    }

    /// Ensure the Ollama server is running (auto-start if configured).
    pub async fn ensure_server(&self) -> Result<bool> {
        self.native.ensure_server().await
    }

    async fn stream_completion(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
    ) -> Result<String> {
        self.native.stream_completion(request, tx).await
    }
}

#[async_trait]
impl ModelProvider for OllamaClient {
    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn is_available(&self) -> bool {
        !self.native.config().base_url.is_empty()
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
        assert!(OllamaClient::model_supports_thinking("gpt-oss:120b"));
        assert!(!OllamaClient::model_supports_thinking("llama3.1:8b"));
        assert!(!OllamaClient::model_supports_thinking("mistral:latest"));
    }

    #[test]
    fn test_ollama_provider_config_env_suite() {
        // Clear env vars to test defaults
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
        std::env::remove_var("OLLAMA_THINKING");
        std::env::remove_var("OLLAMA_AUTO_SERVE");
        std::env::remove_var("OLLAMA_AUTO_DISCOVER");
        std::env::remove_var("OLLAMA_STRUCTURED_OUTPUT");
        std::env::remove_var("OLLAMA_NATIVE_TOOL_CALLING");
        std::env::remove_var("OLLAMA_STREAM_BUFFER_MS");
        std::env::remove_var("OLLAMA_CONTEXT_LENGTH");
        std::env::remove_var("OLLAMA_METADATA_TTL_SEC");

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
        assert!(matches!(cfg.thinking_mode, ThinkingMode::Auto));
        assert!(cfg.auto_serve);
        assert!(cfg.auto_discover);
        assert!(cfg.structured_output);
        assert!(cfg.native_tool_calling);
        assert_eq!(cfg.stream_buffer_ms, 10);
        assert!(cfg.context_length_override.is_none());
        assert_eq!(cfg.model_metadata_ttl_sec, 300);

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
        std::env::set_var("OLLAMA_THINKING", "high");
        std::env::set_var("OLLAMA_AUTO_SERVE", "false");
        std::env::set_var("OLLAMA_AUTO_DISCOVER", "false");
        std::env::set_var("OLLAMA_STRUCTURED_OUTPUT", "false");
        std::env::set_var("OLLAMA_NATIVE_TOOL_CALLING", "false");
        std::env::set_var("OLLAMA_STREAM_BUFFER_MS", "50");
        std::env::set_var("OLLAMA_CONTEXT_LENGTH", "64000");
        std::env::set_var("OLLAMA_METADATA_TTL_SEC", "600");

        let cfg = OllamaProviderConfig::from_env();
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
        assert!(matches!(cfg.thinking_mode, ThinkingMode::High));
        assert!(!cfg.auto_serve);
        assert!(!cfg.auto_discover);
        assert!(!cfg.structured_output);
        assert!(!cfg.native_tool_calling);
        assert_eq!(cfg.stream_buffer_ms, 50);
        assert_eq!(cfg.context_length_override, Some(64000));
        assert_eq!(cfg.model_metadata_ttl_sec, 600);

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
        std::env::remove_var("OLLAMA_THINKING");
        std::env::remove_var("OLLAMA_AUTO_SERVE");
        std::env::remove_var("OLLAMA_AUTO_DISCOVER");
        std::env::remove_var("OLLAMA_STRUCTURED_OUTPUT");
        std::env::remove_var("OLLAMA_NATIVE_TOOL_CALLING");
        std::env::remove_var("OLLAMA_STREAM_BUFFER_MS");
        std::env::remove_var("OLLAMA_CONTEXT_LENGTH");
        std::env::remove_var("OLLAMA_METADATA_TTL_SEC");
    }

    #[test]
    fn test_config_roundtrip() {
        let native = OllamaNativeConfig::from_env();
        let cfg = OllamaProviderConfig::from_native(native.clone());
        let back = cfg.to_native();
        assert_eq!(back.base_url, native.base_url);
        assert_eq!(back.default_model, native.default_model);
        assert_eq!(back.default_num_ctx, native.default_num_ctx);
    }

    #[test]
    fn test_resolve_thinking_param() {
        assert!(resolve_thinking_param("deepseek-r1", ThinkingMode::Auto).is_some());
        assert!(resolve_thinking_param("llama3.1", ThinkingMode::Auto).is_none());
        assert!(resolve_thinking_param("llama3.1", ThinkingMode::Disabled).is_none());
        assert!(resolve_thinking_param("llama3.1", ThinkingMode::Enabled).is_some());
    }
}
