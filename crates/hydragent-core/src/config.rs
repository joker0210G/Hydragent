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
#[derive(Deserialize, Clone)]
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

    /// Explicit provider type (e.g. "openai", "openrouter", "ollama").
    /// If empty, we auto-detect (e.g. Ollama if URL contains 11434).
    pub brain_provider: String,

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
            // Brain
            .set_default("brain_base", "")?
            .set_default("brain_key", "")?
            .set_default("brain_model", "")?
            .set_default("brain_provider", "")?
            .set_default("brain_fallbacks", "")?

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

            // Memory cap
            .set_default("max_semantic_memories", 1_000_000_u64)?

            // Add environment overrides
            .add_source(Environment::default())
            .build()?;

        let mut config: AppConfig = builder.try_deserialize()?;

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

    /// Effective provider type for the live brain. If not explicitly
    /// configured, auto-detects from the URL.
    pub fn effective_brain_provider(&self) -> String {
        if !self.brain_provider.is_empty() {
            return self.brain_provider.trim().to_lowercase();
        }
        let base = self.effective_brain_base();
        if base.contains("11434") || base.contains("ollama") {
            "ollama".to_string()
        } else if base.contains("openrouter.ai") {
            "openrouter".to_string()
        } else {
            "custom-openai".to_string()
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

    /// Redact a secret value for safe inclusion in logs.
    /// Public re-export so call sites (e.g. `info!("…{:?}", …)` in
    /// other modules) can use the same masking policy.
    pub fn mask_key(s: &str) -> String {
        mask_key_for_debug(s)
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
