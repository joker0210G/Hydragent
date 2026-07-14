use serde::Deserialize;
use config::{Config as ConfigBuilder, ConfigError, Environment};

use crate::paths;

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

// `AppConfig` deliberately does **not** derive `Debug` because it carries
// bearer tokens (brain_key, openrouter_api_keys). The manual `Debug` impl
// below redacts those fields with `mask_key_for_debug` so that no future
// `format!("{:?}", cfg)` call site can accidentally leak a secret to the
// log file. (See regression test `appconfig_debug_redacts_keys`.)
#[derive(Deserialize, Clone, Default)]
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
    #[serde(skip)]
    pub brain_base: String,

    #[serde(skip)]
    pub brain_key: String,

    #[serde(skip)]
    pub brain_model: String,

    #[serde(skip)]
    pub brain_provider: String,

    #[serde(skip)]
    pub brain_fallbacks: String,

    // ── Registry-backed provider/model selection (new) ─────────────────
    /// Path to `model_providers.yaml`. If empty, the built-in registry is used
    /// and the repo's `config/model_providers.yaml` is tried first.
    pub model_providers_path: String,
    /// Active provider id from the registry (e.g. `openrouter`, `ollama`).
    /// Falls back to `effective_brain_provider()` when empty.
    pub active_provider: String,
    /// Active model ref from the registry (e.g. `openai-gpt-4o-mini`).
    /// Falls back to `effective_brain_model()` when empty.
    pub active_model: String,

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
    pub dreaming_mode: DreamingMode,

    // ── Curator (skill curation) ─────────────────────────────────────
    pub enable_curator: bool,
    pub curator_interval_sec: u64,

    // ── Memory cap (LRU eviction) ────────────────────────────────────
    /// Maximum number of rows allowed in `semantic_memories`. When the
    /// count exceeds this after an insert, the oldest + lowest-importance
    /// rows are deleted. Default 1_000_000 (effectively unbounded for
    /// small tests); lower this in production to bound disk usage.
    pub max_semantic_memories: usize,
}

/// Redact a secret string for log output.
///
/// * `""`           → `<empty>`
/// * `len <= 12`    → `<set> (N chars)` (still redacted; never reveal
///                    the raw value, no matter how short)
/// * `len > 12`     → `first4…last4 (N chars)`
fn mask_key_for_debug(s: &str) -> String {
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

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConfig")
            .field("brain_base", &self.brain_base)
            .field("brain_key", &mask_key_for_debug(&self.brain_key))
            .field("brain_model", &self.brain_model)
            .field("brain_provider", &self.brain_provider)
            .field("brain_fallbacks", &self.brain_fallbacks)
            .field("model_providers_path", &self.model_providers_path)
            .field("active_provider", &self.active_provider)
            .field("active_model", &self.active_model)
            .field("log_format", &self.log_format)
            .field("log_level", &self.log_level)
            .field("data_dir", &self.data_dir)
            .field("max_react_steps", &self.max_react_steps)
            .field("bus_port", &self.bus_port)
            .field(
                "openrouter_api_keys",
                &mask_key_for_debug(&self.openrouter_api_keys),
            )
            .field("enable_dreaming", &self.enable_dreaming)
            .field("dreaming_interval_sec", &self.dreaming_interval_sec)
            .field("dreaming_mode", &self.dreaming_mode)
            .field("enable_curator", &self.enable_curator)
            .field("curator_interval_sec", &self.curator_interval_sec)
            .field("max_semantic_memories", &self.max_semantic_memories)
            .finish()
    }
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        // Load .env from the resolved install root (~/.hydragent/.env on
        // Unix, %USERPROFILE%\.hydragent\.env on Windows). We deliberately
        // do NOT use `dotenvy::dotenv()` here — that helper walks the
        // current directory and would pick up the wrong .env if the
        // user happens to `cd` somewhere else.
        let _ = paths::load_dotenv();

        // If the user has neither set HYDRAGENT_HOME nor has a HOME /
        // USERPROFILE variable, paths::hydragent_home() falls back to a
        // relative `./.hydragent`. In that case we still want the
        // binary to be useful, so we make sure the directory exists
        // before any other code tries to write into it.
        let _ = paths::ensure_dirs();

        let builder = ConfigBuilder::builder()
            // Brain defaults removed from configuration files and env

            // Registry-backed provider/model selection
            .set_default("model_providers_path", "")?
            .set_default("active_provider", "")?
            .set_default("active_model", "")?

            // Runtime
            .set_default("log_format", "terminal")?
            .set_default("log_level", "info")?
            // Default data_dir is now anchored at the resolved install
            // root (e.g. `/home/me/.hydragent/data`), not `./data` in
            // cwd. The post-processing below makes the path absolute
            // regardless of what the env override was.
            .set_default("data_dir", paths::data_dir().to_string_lossy().to_string())?
            .set_default("max_react_steps", 10_u64)?
            .set_default("bus_port", 5000_u64)?

            // Legacy
            .set_default("openrouter_api_keys", "")?

            // Dreaming
            .set_default("enable_dreaming", true)?
            .set_default("dreaming_interval_sec", 60_u64)?
            .set_default("dreaming_mode", "balanced")?

            // Curator
            .set_default("enable_curator", true)?
            .set_default("curator_interval_sec", 86400_u64)?

            // Memory cap
            .set_default("max_semantic_memories", 1_000_000_u64)?

            // Add environment overrides
            .add_source(Environment::default())
            .build()?;

        let mut config: AppConfig = builder.try_deserialize()?;

        // Support DREAM_BUDGET_MODE env variable fallback/override
        if let Ok(mode_str) = std::env::var("DREAM_BUDGET_MODE") {
            if let Ok(mode) = serde_json::from_value::<DreamingMode>(serde_json::json!(mode_str.to_lowercase())) {
                config.dreaming_mode = mode;
            }
        }
 
        // Resolve active provider and model defaults from model_providers.yaml
        let mut yaml_path = if !config.model_providers_path.trim().is_empty() {
            std::path::PathBuf::from(&config.model_providers_path)
        } else {
            paths::config_dir().join("model_providers.yaml")
        };
        if !yaml_path.exists() {
            let cwd_fallback = std::env::current_dir().unwrap_or_default().join("config/model_providers.yaml");
            if cwd_fallback.exists() {
                yaml_path = cwd_fallback;
            }
        }

        if yaml_path.exists() {
            if let Ok(reg) = hydragent_model::ProviderRegistry::load_from_yaml(&yaml_path) {
                if let Some(chat_default) = reg.role_default("chat") {
                    let (prov, mod_id) = chat_default.split_once('/').unwrap_or(("custom", chat_default));
                    
                    if config.active_provider.is_empty() {
                        config.active_provider = prov.to_string();
                    }
                    if config.active_model.is_empty() {
                        config.active_model = mod_id.to_string();
                    }
                }
            }
        }

        // Resolve relative `data_dir` settings so every downstream
        // `format!("{}/sessions.db", cfg.data_dir)` produces a stable
        // absolute path regardless of cwd. We anchor at the resolved
        // install root (NOT cwd) so a config file like `data_dir=./data`
        // lands at `<home>/data` rather than `<cwd>/data`.
        let data_dir_path = std::path::PathBuf::from(&config.data_dir);
        if data_dir_path.is_relative() {
            config.data_dir = paths::absolutize(&data_dir_path)
                .to_string_lossy()
                .to_string();
        }
        Ok(config)
    }

    pub fn effective_brain_base(&self) -> String {
        if let Ok(base) = std::env::var("BRAIN_BASE") {
            if !base.trim().is_empty() {
                return base.trim().to_string();
            }
        }

        let path = self.effective_model_providers_path();
        let yaml_path = std::path::PathBuf::from(&path);
        let reg = if yaml_path.exists() {
            hydragent_model::ProviderRegistry::load_from_yaml(&yaml_path).ok()
        } else {
            Some(hydragent_model::ProviderRegistry::builtin_default())
        };

        if let Some(r) = reg {
            let active = self.effective_active_provider();
            if let Some(prov) = r.provider(&active) {
                let env_base = format!("BRAIN_{}_BASE", active.to_uppercase().replace('-', "_"));
                if let Ok(url) = std::env::var(&env_base) {
                    if !url.trim().is_empty() {
                        return url.trim().to_string();
                    }
                }
                return prov.default_base_url.clone();
            }
        }
        String::new()
    }

    pub fn effective_brain_provider(&self) -> String {
        self.effective_active_provider()
    }

    pub fn effective_brain_key(&self) -> String {
        if let Ok(key) = std::env::var("BRAIN_KEY") {
            if !key.is_empty() {
                return key;
            }
        }

        let active = self.effective_active_provider();
        let env_key = format!("BRAIN_{}_KEY", active.to_uppercase().replace('-', "_"));
        if let Ok(key) = std::env::var(&env_key) {
            if !key.is_empty() {
                return key;
            }
        }
        
        let path = self.effective_model_providers_path();
        let yaml_path = std::path::PathBuf::from(&path);
        let reg = if yaml_path.exists() {
            hydragent_model::ProviderRegistry::load_from_yaml(&yaml_path).ok()
        } else {
            Some(hydragent_model::ProviderRegistry::builtin_default())
        };

        if let Some(r) = reg {
            if let Some(prov) = r.provider(&active) {
                if let Some(ref raw_key) = prov.api_key {
                    let trimmed = raw_key.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with("${input:") {
                        if trimmed.starts_with("${env:") && trimmed.ends_with('}') {
                            let var_name = &trimmed[6..trimmed.len() - 1];
                            if let Ok(val) = std::env::var(var_name) {
                                return val;
                            }
                        } else {
                            return trimmed.to_string();
                        }
                    }
                }
            }
        }
        
        String::new()
    }

    pub fn effective_brain_model(&self) -> String {
        self.effective_active_model()
    }

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

    pub fn effective_active_provider(&self) -> String {
        if !self.active_provider.is_empty() {
            self.active_provider.trim().to_lowercase()
        } else {
            "openrouter".to_string()
        }
    }

    pub fn effective_active_model(&self) -> String {
        if !self.active_model.is_empty() {
            self.active_model.trim().to_string()
        } else {
            "gpt-4o-mini".to_string()
        }
    }

    pub fn mask_key(s: &str) -> String {
        mask_key_for_debug(s)
    }

    /// Effective registry file path.
    ///
    /// Production default: `<hydragent_home>/config/model_providers.yaml`.
    /// Source checkout fallback: `config/model_providers.yaml` is still
    /// tried by the runtime loader when this file does not exist.
    pub fn effective_model_providers_path(&self) -> String {
        if !self.model_providers_path.trim().is_empty() {
            self.model_providers_path.trim().to_string()
        } else {
            paths::config_dir()
                .join("model_providers.yaml")
                .to_string_lossy()
                .to_string()
        }
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
    use super::DreamingMode;
    use super::mask_key_for_debug;

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
            brain_provider: String::new(),
            brain_fallbacks: brain_fallbacks.to_string(),
            model_providers_path: String::new(),
            active_provider: String::new(),
            active_model: String::new(),
            log_format: "terminal".to_string(),
            log_level: "info".to_string(),
            data_dir: "./data".to_string(),
            max_react_steps: 10,
            bus_port: 5000,
            openrouter_api_keys: openrouter_api_keys.to_string(),
            enable_dreaming: false,
            dreaming_interval_sec: 60,
            dreaming_mode: DreamingMode::Balanced,
            max_semantic_memories: 1_000_000,
            enable_curator: true,
            curator_interval_sec: 86400,
        }
    }

    fn cfg_active(active_provider: &str, active_model: &str) -> AppConfig {
        cfg_active_with_fallbacks(active_provider, active_model, "")
    }

    fn cfg_active_with_fallbacks(active_provider: &str, active_model: &str, fallbacks: &str) -> AppConfig {
        AppConfig {
            brain_base: String::new(),
            brain_key: String::new(),
            brain_model: String::new(),
            brain_provider: String::new(),
            brain_fallbacks: fallbacks.to_string(),
            model_providers_path: String::new(),
            active_provider: active_provider.to_string(),
            active_model: active_model.to_string(),
            log_format: "terminal".to_string(),
            log_level: "info".to_string(),
            data_dir: "./data".to_string(),
            max_react_steps: 10,
            bus_port: 5000,
            openrouter_api_keys: String::new(),
            enable_dreaming: false,
            dreaming_interval_sec: 60,
            dreaming_mode: DreamingMode::Balanced,
            max_semantic_memories: 1_000_000,
            enable_curator: true,
            curator_interval_sec: 86400,
        }
    }

    #[test]
    fn effective_brain_base_reads_env_override() {
        std::env::set_var("BRAIN_OPENROUTER_BASE", "https://override.openrouter.ai");
        let c = cfg_active("openrouter", "gpt-4o-mini");
        assert_eq!(c.effective_brain_base(), "https://override.openrouter.ai");
        std::env::remove_var("BRAIN_OPENROUTER_BASE");
    }

    #[test]
    fn effective_brain_key_reads_env_override() {
        std::env::set_var("BRAIN_OPENROUTER_KEY", "sk-or-override");
        let c = cfg_active("openrouter", "gpt-4o-mini");
        assert_eq!(c.effective_brain_key(), "sk-or-override");
        std::env::remove_var("BRAIN_OPENROUTER_KEY");
    }

    #[test]
    fn effective_brain_model_returns_active_model() {
        let c = cfg_active("ollama", "llama3.1");
        assert_eq!(c.effective_brain_model(), "llama3.1");
    }

    #[test]
    fn effective_brain_provider_returns_active_provider() {
        let c = cfg_active("ollama", "llama3.1");
        assert_eq!(c.effective_brain_provider(), "ollama");
    }

    #[test]
    fn effective_brain_fallbacks_splits_comma_list() {
        let c = cfg_active_with_fallbacks("openrouter", "gpt-4o-mini", "a, b ,c");
        assert_eq!(
            c.effective_brain_fallbacks(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn effective_brain_fallbacks_filters_empty_entries() {
        let c = cfg_active_with_fallbacks("openrouter", "gpt-4o-mini", ",a,,b,");
        assert_eq!(
            c.effective_brain_fallbacks(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn effective_brain_fallbacks_empty_when_nothing_set() {
        let c = cfg_active("openrouter", "gpt-4o-mini");
        assert!(c.effective_brain_fallbacks().is_empty());
    }

    #[test]
    fn effective_brain_fallbacks_single_value() {
        let c = cfg_active_with_fallbacks("openrouter", "gpt-4o-mini", "only-one");
        assert_eq!(c.effective_brain_fallbacks(), vec!["only-one".to_string()]);
    }

    #[test]
    fn effective_model_providers_path_prefers_explicit_override() {
        let mut c = cfg("", "", "", "", "");
        c.model_providers_path = "D:/custom/model_providers.yaml".to_string();
        assert_eq!(c.effective_model_providers_path(), "D:/custom/model_providers.yaml");
    }

    #[test]
    fn effective_model_providers_path_has_default_filename() {
        let c = cfg("", "", "", "", "");
        assert!(
            c.effective_model_providers_path().ends_with("model_providers.yaml"),
            "unexpected default registry path: {}",
            c.effective_model_providers_path()
        );
    }

    // ── P0: API-key leak prevention ────────────────────────────────────
    //
    // Regression: the old code derived `Debug` on `AppConfig`, so a single
    // `info!("starting with {:?}", app_config)` in main.rs printed the
    // `brain_key` and `openrouter_api_keys` in plaintext to the chat log
    // (`data/logs/chat.jsonl`). The manual `Debug` impl above redacts
    // both fields. These tests pin the redaction so a future refactor
    // can't quietly re-introduce the leak.

    fn cfg_with_realistic_secrets() -> AppConfig {
        cfg(
            "https://api.together.xyz/v1",
            // 32-char secret that should be redacted (longer than the
            // 12-char threshold, so we should see first-4…last-4 only).
            "sk-together-ABCDefgh1234567890WXYZabcd",
            "meta-llama/Llama-3-70b-chat-hf",
            "openai/gpt-4o-mini",
            // Legacy multi-key, also 32+ chars. Must also be redacted.
            "sk-or-v1-aaaaaaaaaaaaaaa, sk-or-v1-bbbbbbbbbbbbbb",
        )
    }

    #[test]
    fn appconfig_debug_redacts_brain_key() {
        let c = cfg_with_realistic_secrets();
        let s = format!("{:?}", c);
        // The raw secret must NEVER appear in the Debug output.
        assert!(
            !s.contains("sk-together-ABCDefgh1234567890WXYZabcd"),
            "brain_key leaked through Debug! output was: {s}"
        );
        // We should see the redaction sentinel instead.
        assert!(
            s.contains("sk-") && s.contains("…") && s.contains("chars"),
            "expected redaction marker (… + chars) in Debug output, got: {s}"
        );
    }

    #[test]
    fn appconfig_debug_redacts_openrouter_api_keys() {
        let c = cfg_with_realistic_secrets();
        let s = format!("{:?}", c);
        // Each legacy key prefix should not appear verbatim.
        assert!(
            !s.contains("sk-or-v1-aaaaaaaaaaaaaaa"),
            "openrouter key #1 leaked through Debug! output was: {s}"
        );
        assert!(
            !s.contains("sk-or-v1-bbbbbbbbbbbbbb"),
            "openrouter key #2 leaked through Debug! output was: {s}"
        );
        // And the redaction sentinel should be present.
        assert!(
            s.contains("…") && s.contains("chars"),
            "expected redaction marker in Debug output, got: {s}"
        );
    }

    #[test]
    fn appconfig_debug_handles_empty_keys() {
        // No secrets set — Debug should still work and the redaction
        // should print the `<empty>` sentinel.
        let c = cfg("", "", "", "", "");
        let s = format!("{:?}", c);
        assert!(s.contains("<empty>"), "empty sentinel missing from: {s}");
    }

    #[test]
    fn appconfig_debug_handles_short_keys() {
        // A 12-char or shorter key should be redacted with
        // `<set> (N chars)`, never with the raw value.
        let c = cfg("", "short-12char", "", "", "");
        let s = format!("{:?}", c);
        assert!(
            !s.contains("short-12char"),
            "short key leaked through Debug! output was: {s}"
        );
        assert!(
            s.contains("<set>") && s.contains("12 chars"),
            "expected '<set> (12 chars)' redaction, got: {s}"
        );
    }

    #[test]
    fn appconfig_debug_keeps_non_secret_fields_visible() {
        // Sanity check: non-secret fields are still visible so the
        // log line remains useful for debugging.
        let c = cfg("https://api.openai.com/v1", "", "gpt-4o", "", "");
        let s = format!("{:?}", c);
        assert!(s.contains("https://api.openai.com/v1"), "brain_base missing");
        assert!(s.contains("gpt-4o"), "brain_model missing");
        assert!(s.contains("AppConfig"), "struct name missing");
    }

    #[test]
    fn appconfig_mask_key_helper_is_consistent_with_debug() {
        // `AppConfig::mask_key` is the public re-export of the same
        // masking used by Debug. They must agree, otherwise some call
        // site might be using the wrong policy.
        let s = "sk-1234567890abcdefABCDEFGH";
        assert_eq!(
            AppConfig::mask_key(s),
            mask_key_for_debug(s),
            "public mask_key diverges from internal Debug masking"
        );
        assert_eq!(AppConfig::mask_key(""), "<empty>");
        assert_eq!(AppConfig::mask_key("short"), "<set> (5 chars)");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamingMode {
    Balanced,
    Turbo,
    Scholar,
    Custom,
}

impl Default for DreamingMode {
    fn default() -> Self {
        Self::Balanced
    }
}
