use serde::Deserialize;
use config::{Config as ConfigBuilder, ConfigError, Environment};

/// Standard provider name constants used throughout the registry.
pub mod provider_names {
    /// Live "brain" — any OpenAI-compatible endpoint the user wants to use.
    /// Identified by its base URL, not by a hard-coded name.
    pub const BRAIN: &str = "brain";

    /// True if the provider name is something we know how to build.
    pub fn is_known(name: &str) -> bool {
        matches!(name, BRAIN)
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    // ── The "brain" (single live provider) ────────────────────────────
    /// Base URL of the OpenAI-compatible `/v1/chat/completions` endpoint.
    /// Examples:
    ///   - `https://openrouter.ai/api/v1`
    ///   - `https://api.openai.com/v1`
    ///   - `https://api.together.xyz/v1`
    ///   - `http://localhost:11434/v1` (Ollama in OpenAI-compat mode)
    ///
    /// If unset, falls back to legacy `OPENROUTER_API_KEYS` (backward compat).
    pub brain_base: String,

    /// API key / bearer token for the brain. May be empty for local
    /// providers (Ollama, LM Studio) that don't require auth.
    pub brain_key: String,

    /// Primary model to call on the brain. If unset, falls back to legacy
    /// `PRIMARY_MODEL`.
    pub brain_model: String,

    /// Comma-separated fallback model list, all served by the same brain.
    /// Tried in order if the primary model errors out.
    pub brain_fallbacks: String,

    // ── Runtime ────────────────────────────────────────────────────────
    pub log_format: String,
    pub log_level: String,
    pub data_dir: String,
    pub max_react_steps: u8,
    pub bus_port: u16,

    // ── Legacy OpenRouter (back-compat) ────────────────────────────────
    /// Kept for users with old `.env` files. If `brain_base` is empty but
    /// this is set, we auto-map to `brain_base = "https://openrouter.ai/api/v1"`.
    pub openrouter_api_keys: String,

    // ── Dreaming (memory consolidation) ──────────────────────────────
    pub enable_dreaming: bool,
    pub dreaming_interval_sec: u64,

    // ── Memory cap (LRU eviction) ────────────────────────────────────
    /// Maximum number of rows allowed in `semantic_memories`. When the
    /// count exceeds this after an insert, the oldest + lowest-importance
    /// rows are deleted. Default 1_000_000 (effectively unbounded for
    /// small tests); lower this in production to bound disk usage.
    pub max_semantic_memories: usize,
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        // Load .env file if it exists
        let _ = dotenvy::dotenv();

        let builder = ConfigBuilder::builder()
            // Brain
            .set_default("brain_base", "")?
            .set_default("brain_key", "")?
            .set_default("brain_model", "")?
            .set_default("brain_fallbacks", "")?

            // Runtime
            .set_default("log_format", "terminal")?
            .set_default("log_level", "info")?
            .set_default("data_dir", "./data")?
            .set_default("max_react_steps", 10_u64)?
            .set_default("bus_port", 5000_u64)?

            // Legacy
            .set_default("openrouter_api_keys", "")?

            // Dreaming
            .set_default("enable_dreaming", true)?
            .set_default("dreaming_interval_sec", 60_u64)?

            // Memory cap
            .set_default("max_semantic_memories", 1_000_000_u64)?

            // Add environment overrides
            .add_source(Environment::default())
            .build()?;

        let mut config: AppConfig = builder.try_deserialize()?;
        let data_dir_path = std::path::PathBuf::from(&config.data_dir);
        if data_dir_path.is_relative() {
            config.data_dir = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(data_dir_path)
                .to_string_lossy()
                .to_string();
        }
        Ok(config)
    }

    /// Effective base URL of the live brain. Applies the OpenRouter
    /// back-compat default if `BRAIN_BASE` wasn't set but
    /// `OPENROUTER_API_KEYS` was.
    pub fn effective_brain_base(&self) -> String {
        if !self.brain_base.is_empty() {
            self.brain_base.trim_end_matches('/').to_string()
        } else if !self.openrouter_api_keys.is_empty() {
            "https://openrouter.ai/api/v1".to_string()
        } else {
            String::new()
        }
    }

    /// Effective API key for the live brain. Falls back to
    /// `OPENROUTER_API_KEYS` (first key in the comma-separated list) for
    /// back-compat.
    pub fn effective_brain_key(&self) -> String {
        if !self.brain_key.is_empty() {
            self.brain_key.clone()
        } else {
            // Take the first non-empty key from the legacy comma-separated list
            self.openrouter_api_keys
                .split(',')
                .map(|s| s.trim().to_string())
                .find(|s| !s.is_empty())
                .unwrap_or_default()
        }
    }

    /// Effective primary model. Falls back to `PRIMARY_MODEL` env for
    /// back-compat, then to a sane default.
    pub fn effective_brain_model(&self) -> String {
        if !self.brain_model.is_empty() {
            return self.brain_model.clone();
        }
        if let Ok(p) = std::env::var("PRIMARY_MODEL") {
            if !p.is_empty() {
                return p;
            }
        }
        "anthropic/claude-sonnet-4".to_string()
    }

    /// Effective fallback list. Falls back to `FALLBACK_MODELS` env for
    /// back-compat.
    pub fn effective_brain_fallbacks(&self) -> Vec<String> {
        let raw = if !self.brain_fallbacks.is_empty() {
            self.brain_fallbacks.clone()
        } else {
            std::env::var("FALLBACK_MODELS").unwrap_or_default()
        };
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the swappable "brain" provider config (Plan v4).
    //!
    //! These tests pin the behavior of the `effective_brain_*` helpers, which is
    //! what makes the new 4 env-var interface
    //! (`BRAIN_BASE`, `BRAIN_KEY`, `BRAIN_MODEL`, `BRAIN_FALLBACKS`)
    //! backward-compatible with the legacy
    //! `OPENROUTER_API_KEYS` / `PRIMARY_MODEL` / `FALLBACK_MODELS` env vars.
    //!
    //! We construct `AppConfig` directly with hand-picked field values rather than
    //! loading from the environment so each test is fully isolated and
    //! deterministic.

    use super::AppConfig;

    fn cfg(
        brain_base: &str,
        brain_key: &str,
        brain_model: &str,
        brain_fallbacks: &str,
        openrouter_api_keys: &str,
    ) -> AppConfig {
        AppConfig {
            brain_base: brain_base.to_string(),
            brain_key: brain_key.to_string(),
            brain_model: brain_model.to_string(),
            brain_fallbacks: brain_fallbacks.to_string(),
            log_format: "terminal".to_string(),
            log_level: "info".to_string(),
            data_dir: "./data".to_string(),
            max_react_steps: 10,
            bus_port: 5000,
            openrouter_api_keys: openrouter_api_keys.to_string(),
            enable_dreaming: false,
            dreaming_interval_sec: 60,
            max_semantic_memories: 1_000_000,
        }
    }

    #[test]
    fn effective_brain_base_prefers_brain_base() {
        let c = cfg("https://api.together.xyz/v1", "", "", "", "");
        assert_eq!(c.effective_brain_base(), "https://api.together.xyz/v1");
    }

    #[test]
    fn effective_brain_base_strips_trailing_slash() {
        let c = cfg("https://api.together.xyz/v1/", "", "", "", "");
        assert_eq!(c.effective_brain_base(), "https://api.together.xyz/v1");
    }

    #[test]
    fn effective_brain_base_falls_back_to_openrouter() {
        // Empty BRAIN_BASE but legacy OPENROUTER_API_KEYS set
        // -> auto-maps to openrouter.ai for back-compat
        let c = cfg("", "", "", "", "sk-or-v1-abc");
        assert_eq!(c.effective_brain_base(), "https://openrouter.ai/api/v1");
    }

    #[test]
    fn effective_brain_base_empty_when_nothing_set() {
        let c = cfg("", "", "", "", "");
        assert_eq!(c.effective_brain_base(), "");
    }

    #[test]
    fn effective_brain_key_prefers_brain_key() {
        let c = cfg("", "together-xyz", "", "", "sk-or-v1-legacy");
        assert_eq!(c.effective_brain_key(), "together-xyz");
    }

    #[test]
    fn effective_brain_key_uses_first_legacy_key() {
        // Empty BRAIN_KEY, multiple legacy keys
        // -> take the first non-empty one
        let c = cfg("", "", "", "", "sk-or-v1-first, sk-or-v1-second");
        assert_eq!(c.effective_brain_key(), "sk-or-v1-first");
    }

    #[test]
    fn effective_brain_key_handles_empty_legacy_entries() {
        // Legacy list with leading/trailing whitespace + empties
        let c = cfg("", "", "", "", " , sk-or-v1-real, ");
        assert_eq!(c.effective_brain_key(), "sk-or-v1-real");
    }

    #[test]
    fn effective_brain_key_empty_when_nothing_set() {
        let c = cfg("", "", "", "", "");
        assert_eq!(c.effective_brain_key(), "");
    }

    #[test]
    fn effective_brain_fallbacks_splits_comma_list() {
        let c = cfg("", "", "", "a, b ,c", "");
        assert_eq!(
            c.effective_brain_fallbacks(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn effective_brain_fallbacks_filters_empty_entries() {
        let c = cfg("", "", "", ",a,,b,", "");
        assert_eq!(
            c.effective_brain_fallbacks(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn effective_brain_fallbacks_empty_when_nothing_set() {
        let c = cfg("", "", "", "", "");
        assert!(c.effective_brain_fallbacks().is_empty());
    }

    #[test]
    fn effective_brain_fallbacks_single_value() {
        let c = cfg("", "", "", "only-one", "");
        assert_eq!(c.effective_brain_fallbacks(), vec!["only-one".to_string()]);
    }

    #[test]
    fn full_swappable_brain_scenario() {
        // Realistic 4-var user setup
        let c = cfg(
            "https://api.openai.com/v1",
            "sk-openai-xxx",
            "gpt-4o",
            "gpt-4o-mini,gpt-3.5-turbo",
            "",
        );
        assert_eq!(c.effective_brain_base(), "https://api.openai.com/v1");
        assert_eq!(c.effective_brain_key(), "sk-openai-xxx");
        assert_eq!(c.effective_brain_model(), "gpt-4o");
        assert_eq!(
            c.effective_brain_fallbacks(),
            vec!["gpt-4o-mini".to_string(), "gpt-3.5-turbo".to_string()]
        );
    }

    #[test]
    fn ollama_local_url_preserved() {
        // Local Ollama with no auth: BRAIN_KEY is empty
        let c = cfg("http://localhost:11434/v1", "", "llama3.1", "", "");
        assert_eq!(c.effective_brain_base(), "http://localhost:11434/v1");
        assert_eq!(c.effective_brain_key(), "");
        assert_eq!(c.effective_brain_model(), "llama3.1");
    }
}
