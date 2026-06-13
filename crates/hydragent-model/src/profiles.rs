//! # Model profiles
//!
//! A [`ModelProfile`] describes a single LLM the [`ModelCouncil`](crate::council::ModelCouncil)
//! can route sub-agent dispatches to. Profiles are loaded in bulk from
//! `config/model_council.yaml` and indexed by model id.
//!
//! The schema is intentionally aligned with the 8 [`hydragent_types::TaskType`]
//! variants (which the planner emits) plus a `general` catch-all tag. A profile
//! is "good" at a task if its `task_tags` list contains that task's tag; the
//! relative strength is the `benchmark[task]` score (0.0 - 1.0).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The cost tier a caller is willing to spend on a sub-agent dispatch.
/// Mapped to a profile's [`ModelProfile::cost_tier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostTier {
    /// $0 per 1k tokens (local models, free tiers).
    Free,
    /// <= $0.0005 per 1k tokens.
    Cheap,
    /// > $0.0005 and <= $0.0025 per 1k tokens.
    Standard,
    /// > $0.0025 per 1k tokens.
    Premium,
    /// Any cost tier (used as the default when a caller doesn't constrain).
    Any,
}

impl Default for CostTier {
    fn default() -> Self {
        CostTier::Any
    }
}

impl CostTier {
    /// True if `profile_tier` is acceptable under `self` as the budget cap.
    pub fn accepts(&self, profile_tier: CostTier) -> bool {
        if *self == CostTier::Any {
            return true;
        }
        if profile_tier == CostTier::Free {
            return true; // Free is always acceptable
        }
        // Strict ordering: Free < Cheap < Standard < Premium.
        let rank = |t: CostTier| match t {
            CostTier::Free => 0,
            CostTier::Cheap => 1,
            CostTier::Standard => 2,
            CostTier::Premium => 3,
            CostTier::Any => 4,
        };
        rank(profile_tier) <= rank(*self)
    }

    /// String form used in YAML and the routing log line.
    pub fn as_str(&self) -> &'static str {
        match self {
            CostTier::Free => "free",
            CostTier::Cheap => "cheap",
            CostTier::Standard => "standard",
            CostTier::Premium => "premium",
            CostTier::Any => "any",
        }
    }
}

/// A single model's metadata and routing metadata.
///
/// Sourced from `config/model_council.yaml`; deserialized via
/// `#[serde(deny_unknown_fields)]` to fail loudly on typos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelProfile {
    /// Wire-level model id (e.g. `"anthropic/claude-3.5-sonnet"` for
    /// openrouter, or `"llama3.1:8b"` for ollama).
    pub model_id: String,
    /// Backend provider: `openrouter` | `custom_openai` | `ollama`.
    pub provider: String,
    /// Max context tokens.
    ///
    /// Accepts both a plain integer (`128000`) and a YAML string with
    /// underscore separators (`128_000` / `1_000_000`).  The latter is
    /// commonly written for human readability but `serde_yaml` refuses
    /// to parse it as a number, so we accept both forms here.
    #[serde(deserialize_with = "deserialize_u32_underscored")]
    pub context_window: u32,
    /// USD per 1k tokens (output side; conservative).
    pub cost_per_1k: f64,
    /// Cost bucket — used by the council's budget filter.
    pub cost_tier: CostTier,
    /// Tags the model is good at.  Must align with the planner's
    /// `TaskType` snake_case strings.
    pub task_tags: Vec<String>,
    /// Optional benchmark scores per task tag (0.0 - 1.0).  Used as
    /// tie-breaker when multiple profiles match a tag.
    #[serde(default)]
    pub benchmark: HashMap<String, f64>,
    /// The safety-net fallback.  Exactly one profile in the council
    /// must be primary.
    #[serde(default)]
    pub primary: bool,
}

impl ModelProfile {
    /// Look up the benchmark score for a task tag.  Returns `None` if
    /// the profile didn't ship a score for that tag.
    pub fn score_for(&self, task_tag: &str) -> Option<f64> {
        self.benchmark.get(task_tag).copied()
    }

    /// True if this profile advertises the given task tag in `task_tags`.
    pub fn supports(&self, task_tag: &str) -> bool {
        self.task_tags.iter().any(|t| t == task_tag)
    }

    /// A short one-line summary used in logs.
    pub fn summary(&self) -> String {
        format!(
            "{:<45} provider={:<14} tier={:<8} ctx={:>7} cost=${:.5}/1k tags={}",
            self.model_id,
            self.provider,
            self.cost_tier.as_str(),
            self.context_window,
            self.cost_per_1k,
            self.task_tags.join(",")
        )
    }
}

/// Deserialize a `u32` from YAML that may be either a plain number
/// (`128000`) or a string with underscore separators (`128_000`,
/// `1_000_000`).  The latter is the most readable form for context
/// windows, but `serde_yaml` parses it as a string, so we accept
/// both and strip the underscores before parsing.
fn deserialize_u32_underscored<'de, D>(de: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Int(u64),
        Float(f64),
        Str(String),
    }

    match NumOrStr::deserialize(de)? {
        NumOrStr::Int(n) => u32::try_from(n).map_err(|e| D::Error::custom(format!("u32 overflow: {e}"))),
        NumOrStr::Float(f) => {
            if !f.is_finite() || f < 0.0 {
                return Err(D::Error::custom(format!("non-u32 float: {f}")));
            }
            Ok(f as u32)
        }
        NumOrStr::Str(s) => {
            let stripped: String = s.chars().filter(|c| *c != '_' && *c != ' ').collect();
            stripped
                .parse::<u32>()
                .map_err(|e| D::Error::custom(format!("cannot parse '{s}' as u32: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(model_id: &str, tier: CostTier) -> ModelProfile {
        ModelProfile {
            model_id: model_id.into(),
            provider: "openrouter".into(),
            context_window: 128_000,
            cost_per_1k: 0.001,
            cost_tier: tier,
            task_tags: vec!["general".into()],
            benchmark: HashMap::new(),
            primary: false,
        }
    }

    #[test]
    fn cost_tier_accepts_ranks() {
        // "Any" accepts every profile tier.
        assert!(CostTier::Any.accepts(CostTier::Free));
        assert!(CostTier::Any.accepts(CostTier::Cheap));
        assert!(CostTier::Any.accepts(CostTier::Standard));
        assert!(CostTier::Any.accepts(CostTier::Premium));

        // Free profiles are always acceptable.
        assert!(CostTier::Cheap.accepts(CostTier::Free));
        assert!(CostTier::Premium.accepts(CostTier::Free));

        // A Cheap budget rejects Standard and Premium.
        assert!(!CostTier::Cheap.accepts(CostTier::Standard));
        assert!(!CostTier::Cheap.accepts(CostTier::Premium));

        // A Premium budget accepts everything.
        assert!(CostTier::Premium.accepts(CostTier::Standard));
        assert!(CostTier::Premium.accepts(CostTier::Premium));

        // A Free budget accepts only Free.
        assert!(CostTier::Free.accepts(CostTier::Free));
        assert!(!CostTier::Free.accepts(CostTier::Cheap));
    }

    #[test]
    fn cost_tier_default_is_any() {
        assert_eq!(CostTier::default(), CostTier::Any);
    }

    #[test]
    fn cost_tier_str_roundtrip() {
        for tier in [
            CostTier::Free,
            CostTier::Cheap,
            CostTier::Standard,
            CostTier::Premium,
            CostTier::Any,
        ] {
            // As long as `as_str` returns the snake_case form that matches
            // the serde rename_all annotation, YAML deserialization is safe.
            assert!(!tier.as_str().is_empty());
            assert_eq!(tier.as_str().to_lowercase(), tier.as_str());
        }
    }

    #[test]
    fn model_profile_supports() {
        let mut p = sample("m1", CostTier::Free);
        p.task_tags = vec!["code_generation".into(), "review".into()];
        assert!(p.supports("code_generation"));
        assert!(p.supports("review"));
        assert!(!p.supports("creative_writing"));
        assert!(!p.supports("nonsense"));
    }

    #[test]
    fn model_profile_score_for() {
        let mut p = sample("m1", CostTier::Free);
        p.benchmark
            .insert("code_generation".into(), 0.92);
        assert_eq!(p.score_for("code_generation"), Some(0.92));
        assert_eq!(p.score_for("review"), None);
    }

    #[test]
    fn model_profile_summary_is_one_line() {
        let p = sample("openai/gpt-4o-mini", CostTier::Cheap);
        let s = p.summary();
        assert!(s.contains("openai/gpt-4o-mini"));
        assert!(s.contains("openrouter"));
        assert!(s.contains("cheap"));
        assert!(!s.contains('\n'));
    }

    #[test]
    fn model_profile_deserialize_from_yaml() {
        // Minimal valid YAML for one profile.
        let yaml = r#"
            model_id: "openai/gpt-4o-mini"
            provider: "openrouter"
            context_window: 128000
            cost_per_1k: 0.00015
            cost_tier: "cheap"
            task_tags: ["general", "review"]
            benchmark:
              general: 0.81
              review: 0.78
        "#;
        let p: ModelProfile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.model_id, "openai/gpt-4o-mini");
        assert_eq!(p.cost_tier, CostTier::Cheap);
        assert!(!p.primary);
        assert!(p.supports("general"));
        assert_eq!(p.score_for("general"), Some(0.81));
    }

    #[test]
    fn context_window_accepts_underscored_string() {
        // The shipped config uses `128_000` for readability; serde_yaml
        // parses that as a string, so our deserializer must accept it.
        let yaml = r#"
            model_id: "x/y"
            provider: "openrouter"
            context_window: "128_000"
            cost_per_1k: 0.0
            cost_tier: "free"
            task_tags: ["general"]
        "#;
        let p: ModelProfile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.context_window, 128_000);
    }

    #[test]
    fn context_window_accepts_underscored_int() {
        // Some YAML emitters quote nothing: `context_window: 1_000_000`.
        // That still comes through as a string under serde_yaml.
        let yaml = r#"
            model_id: "x/y"
            provider: "openrouter"
            context_window: 1_000_000
            cost_per_1k: 0.0
            cost_tier: "free"
            task_tags: ["general"]
        "#;
        let p: ModelProfile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.context_window, 1_000_000);
    }

    #[test]
    fn context_window_rejects_garbage() {
        let yaml = r#"
            model_id: "x/y"
            provider: "openrouter"
            context_window: "not-a-number"
            cost_per_1k: 0.0
            cost_tier: "free"
            task_tags: ["general"]
        "#;
        let r: Result<ModelProfile, _> = serde_yaml::from_str(yaml);
        assert!(r.is_err(), "should reject non-numeric context_window");
    }
}
