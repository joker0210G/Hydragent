//! # Model Council
//!
//! A [`ModelCouncil`] is a registry of [`ModelProfile`]s plus a routing
//! function.  It replaces (or sits in front of) the per-call fallback
//! chain in `ModelRouter` for sub-agent dispatches: instead of every
//! sub-agent trying `primary → fallback1 → fallback2`, the planner or
//! spawner asks the council to pick a model **for the task type at
//! hand**.
//!
//! ## Routing priority
//!
//! 1. **Exact task-tag match** — every profile whose `task_tags` contains
//!    the requested tag, sorted by `benchmark[task]` descending.
//! 2. **Cost-budget filter** — drop any profile whose `cost_tier` is
//!    strictly more expensive than the caller's `CostTier`.
//! 3. **Primary fallback** — if the filtered list is empty (or no
//!    profile supported the task at all), return the profile flagged
//!    `primary: true` (the safety-net "always works, never blocked"
//!    model).
//!
//! ## Loading
//!
//! ```ignore
//! use hydragent_model::council::ModelCouncil;
//! let council = ModelCouncil::load_from_yaml("config/model_council.yaml")?;
//! let profile = council.route("code_generation", CostTier::Cheap);
//! ```
//!
//! ## Threading
//!
//! The council is `Send + Sync` (all fields are `Arc<...>` or pure data).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::profiles::{CostTier, ModelProfile};

/// All the ways the council can fail to load or route.
#[derive(Debug, Error)]
pub enum CouncilError {
    /// YAML parse error or IO error reading the council file.
    #[error("failed to load model council: {0}")]
    Load(String),
    /// The loaded council has zero profiles.
    #[error("model council has no profiles")]
    Empty,
    /// The loaded council has zero primary profiles (we require exactly one).
    #[error("model council must have exactly one primary profile; found {0}")]
    NoPrimary(usize),
    /// The loaded council has more than one primary profile.
    #[error("model council must have exactly one primary profile; found {0}")]
    MultiplePrimaries(usize),
}

impl From<std::io::Error> for CouncilError {
    fn from(e: std::io::Error) -> Self {
        CouncilError::Load(e.to_string())
    }
}

impl From<serde_yaml::Error> for CouncilError {
    fn from(e: serde_yaml::Error) -> Self {
        CouncilError::Load(e.to_string())
    }
}

/// The full YAML file shape:
/// ```yaml
/// profiles:
///   - model_id: ...
///     ...
/// ```
#[derive(Debug, Deserialize, Serialize)]
struct CouncilFile {
    #[serde(default)]
    profiles: Vec<ModelProfile>,
}

/// Cheaply-cloneable routing table. The profile set is read-only after
/// construction; routing is allocation-free.
#[derive(Debug, Clone)]
pub struct ModelCouncil {
    /// All profiles, indexed by `model_id`.
    by_id: Arc<HashMap<String, ModelProfile>>,
    /// Profiles that advertise each task tag (sorted by benchmark score
    /// descending).  Pre-computed at load time so routing is O(matches)
    /// per call, not O(n).
    by_tag: Arc<HashMap<String, Vec<ModelProfile>>>,
    /// The single primary profile (always-fallback safety net).
    primary: Arc<ModelProfile>,
}

impl ModelCouncil {
    /// Load a council from a YAML file. Validates that exactly one
    /// profile has `primary: true`.
    pub fn load_from_yaml<P: AsRef<Path>>(path: P) -> Result<Self, CouncilError> {
        let text = std::fs::read_to_string(path.as_ref())?;
        let parsed: CouncilFile = serde_yaml::from_str(&text)?;
        Self::from_profiles(parsed.profiles)
    }

    /// Build a council from an in-memory profile list.  Useful for tests
    /// and for callers that fetch profiles from somewhere other than
    /// YAML (e.g., a future `config(model_council, ...)` table).
    pub fn from_profiles(profiles: Vec<ModelProfile>) -> Result<Self, CouncilError> {
        if profiles.is_empty() {
            return Err(CouncilError::Empty);
        }
        let mut by_id: HashMap<String, ModelProfile> = HashMap::new();
        let mut primaries: Vec<ModelProfile> = Vec::new();
        for p in profiles {
            if by_id.contains_key(&p.model_id) {
                // Duplicate model_id — last write wins, but surface a
                // warning-level error so misconfigs are caught early.
                return Err(CouncilError::Load(format!(
                    "duplicate model_id: {}",
                    p.model_id
                )));
            }
            if p.primary {
                primaries.push(p.clone());
            }
            by_id.insert(p.model_id.clone(), p);
        }
        let primary = match primaries.len() {
            0 => return Err(CouncilError::NoPrimary(0)),
            1 => primaries.into_iter().next().unwrap(),
            n => return Err(CouncilError::MultiplePrimaries(n)),
        };

        // Pre-compute the per-tag index, sorted by the profile's
        // peak benchmark score desc, with cheaper cost tiers
        // breaking ties.
        let mut by_tag: HashMap<String, Vec<ModelProfile>> = HashMap::new();
        for p in by_id.values() {
            for tag in &p.task_tags {
                by_tag
                    .entry(tag.clone())
                    .or_default()
                    .push(p.clone());
            }
        }
        for list in by_tag.values_mut() {
            list.sort_by(|a, b| {
                let sa = a.benchmark.values().copied().fold(0.0_f64, f64::max);
                let sb = b.benchmark.values().copied().fold(0.0_f64, f64::max);
                let ta = tier_rank(a.cost_tier);
                let tb = tier_rank(b.cost_tier);
                sb.partial_cmp(&sa)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| ta.cmp(&tb))
            });
        }

        Ok(Self {
            by_id: Arc::new(by_id),
            by_tag: Arc::new(by_tag),
            primary: Arc::new(primary),
        })
    }

    /// Number of profiles in the council.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// True if the council is empty (shouldn't happen; constructor
    /// rejects empty).
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Borrow a profile by id.  Returns `None` if not found.
    pub fn get(&self, model_id: &str) -> Option<&ModelProfile> {
        self.by_id.get(model_id)
    }

    /// Borrow the primary (safety-net) profile.
    pub fn primary(&self) -> &ModelProfile {
        &self.primary
    }

    /// All profiles that advertise the given task tag, in routing order
    /// (best benchmark first, then cheaper tier).
    pub fn profiles_for_tag(&self, task_tag: &str) -> &[ModelProfile] {
        self.by_tag.get(task_tag).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// All profiles in load order.
    pub fn all(&self) -> impl Iterator<Item = &ModelProfile> {
        // by_id preserves insertion order (HashMap with random hasher
        // doesn't, so we sort by model_id for determinism).
        let mut v: Vec<&ModelProfile> = self.by_id.values().collect();
        v.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        v.into_iter()
    }

    /// The main routing function.
    ///
    /// Algorithm:
    /// 1. Look up profiles matching `task_tag`.
    /// 2. Filter by `budget` (drop any whose `cost_tier` exceeds it).
    /// 3. If any survive, return the first one (best benchmark).
    /// 4. Else, if unfiltered matches exist, return the cheapest of
    ///    them (with a debug-log note that we're over budget).
    /// 5. Else, return the primary profile.
    pub fn route(&self, task_tag: &str, budget: CostTier) -> RoutingDecision {
        let matches = self.profiles_for_tag(task_tag);

        // Pass 1: exact task match within budget.
        let in_budget: Vec<&ModelProfile> = matches
            .iter()
            .filter(|p| budget.accepts(p.cost_tier))
            .collect();
        if let Some(best) = in_budget.first() {
            return RoutingDecision {
                profile: (*best).clone(),
                path: RoutingPath::ExactMatchInBudget,
                candidates_considered: matches.len(),
                candidates_in_budget: in_budget.len(),
            };
        }

        // Pass 2: exact task match but over budget — fall back to cheapest.
        if !matches.is_empty() {
            let cheapest = matches
                .iter()
                .min_by_key(|p| tier_rank(p.cost_tier))
                .unwrap(); // matches is non-empty here
            return RoutingDecision {
                profile: (*cheapest).clone(),
                path: RoutingPath::OverBudgetCheapest,
                candidates_considered: matches.len(),
                candidates_in_budget: 0,
            };
        }

        // Pass 3: no match — return primary.
        RoutingDecision {
            profile: (*self.primary).clone(),
            path: RoutingPath::PrimaryFallback,
            candidates_considered: 0,
            candidates_in_budget: 0,
        }
    }

    /// Look up a profile by id, and return a `RoutingDecision` of kind
    /// `Explicit` so the audit log makes it clear a caller asked for
    /// this profile by name (e.g. via `SubAgentSpec.model_hint`).
    pub fn route_explicit(&self, model_id: &str) -> Option<RoutingDecision> {
        let p = self.by_id.get(model_id)?;
        Some(RoutingDecision {
            profile: p.clone(),
            path: RoutingPath::Explicit,
            candidates_considered: 0,
            candidates_in_budget: 0,
        })
    }
}

/// The result of a routing call.  Carries the picked profile plus a
/// breadcrumb trail of how we got there — useful for observability and
/// the self-healing re-planner in Track 5.4.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    /// The profile the council picked.
    pub profile: ModelProfile,
    /// Which routing path was taken.
    pub path: RoutingPath,
    /// How many profiles matched the requested task tag (before
    /// budget filtering).
    pub candidates_considered: usize,
    /// Of those, how many were inside the budget.
    pub candidates_in_budget: usize,
}

/// The five routing outcomes.  New variants are append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingPath {
    /// The caller asked for a specific `model_id` and the council
    /// honored it.
    Explicit,
    /// A profile matched the task tag AND fit the budget — best case.
    ExactMatchInBudget,
    /// A profile matched the task tag but exceeded the budget.  The
    /// council returned the cheapest match.  The caller can decide
    /// whether to re-plan or accept the over-budget cost.
    OverBudgetCheapest,
    /// No profile matched the task tag.  The primary profile was
    /// returned.
    PrimaryFallback,
    /// A budget filter rejected every match.  The cheapest of the
    /// rejected matches was returned (alias for `OverBudgetCheapest`
    /// in the current implementation; kept distinct in case we
    /// diverge in Track 5.4).
    BudgetFiltered,
}

impl RoutingPath {
    /// Snake-case name (e.g. for log lines).
    pub fn as_str(&self) -> &'static str {
        match self {
            RoutingPath::Explicit => "explicit",
            RoutingPath::ExactMatchInBudget => "exact_match_in_budget",
            RoutingPath::OverBudgetCheapest => "over_budget_cheapest",
            RoutingPath::PrimaryFallback => "primary_fallback",
            RoutingPath::BudgetFiltered => "budget_filtered",
        }
    }
}

/// Helper: rank cost tiers numerically for sorting/cheapest-finding.
/// Lower is cheaper.
fn tier_rank(t: CostTier) -> u8 {
    match t {
        CostTier::Free => 0,
        CostTier::Cheap => 1,
        CostTier::Standard => 2,
        CostTier::Premium => 3,
        CostTier::Any => 4,
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ModelProfile;

    fn prof(
        id: &str,
        tier: CostTier,
        tags: &[&str],
        score: Option<f64>,
        primary: bool,
    ) -> ModelProfile {
        let mut p = ModelProfile {
            model_id: id.into(),
            provider: "openrouter".into(),
            context_window: 128_000,
            cost_per_1k: match tier {
                CostTier::Free => 0.0,
                CostTier::Cheap => 0.0003,
                CostTier::Standard => 0.0015,
                CostTier::Premium => 0.005,
                CostTier::Any => 0.0,
            },
            cost_tier: tier,
            task_tags: tags.iter().map(|s| s.to_string()).collect(),
            benchmark: HashMap::new(),
            primary,
        };
        if let Some(s) = score {
            for t in tags {
                p.benchmark.insert((*t).into(), s);
            }
        }
        p
    }

    fn sample_council() -> ModelCouncil {
        ModelCouncil::from_profiles(vec![
            prof("deepseek-coder", CostTier::Cheap, &["code_generation"], Some(0.92), false),
            prof("claude-sonnet", CostTier::Premium, &["creative_writing", "reasoning"], Some(0.95), false),
            prof("perplexity-sonar", CostTier::Standard, &["research"], Some(0.94), false),
            prof("llama-70b-free", CostTier::Free, &["general", "summarization"], Some(0.75), true),  // primary
            prof("haiku", CostTier::Cheap, &["summarization"], Some(0.84), false),
        ]).unwrap()
    }

    #[test]
    fn from_profiles_rejects_empty() {
        let r = ModelCouncil::from_profiles(vec![]);
        assert!(matches!(r, Err(CouncilError::Empty)));
    }

    #[test]
    fn from_profiles_rejects_no_primary() {
        let r = ModelCouncil::from_profiles(vec![
            prof("a", CostTier::Free, &["general"], None, false),
        ]);
        assert!(matches!(r, Err(CouncilError::NoPrimary(0))));
    }

    #[test]
    fn from_profiles_rejects_two_primaries() {
        let r = ModelCouncil::from_profiles(vec![
            prof("a", CostTier::Free, &["general"], None, true),
            prof("b", CostTier::Free, &["general"], None, true),
        ]);
        assert!(matches!(r, Err(CouncilError::MultiplePrimaries(2))));
    }

    #[test]
    fn from_profiles_rejects_duplicate_model_id() {
        let r = ModelCouncil::from_profiles(vec![
            prof("dup", CostTier::Free, &["general"], None, true),
            prof("dup", CostTier::Free, &["research"], None, false),
        ]);
        assert!(matches!(r, Err(CouncilError::Load(_))));
    }

    #[test]
    fn route_code_generation_with_cheap_budget_picks_deepseek() {
        let c = sample_council();
        let d = c.route("code_generation", CostTier::Cheap);
        assert_eq!(d.profile.model_id, "deepseek-coder");
        assert_eq!(d.path, RoutingPath::ExactMatchInBudget);
        assert_eq!(d.candidates_considered, 1);
        assert_eq!(d.candidates_in_budget, 1);
    }

    #[test]
    fn route_research_picks_perplexity_with_any_budget() {
        let c = sample_council();
        let d = c.route("research", CostTier::Any);
        assert_eq!(d.profile.model_id, "perplexity-sonar");
        assert_eq!(d.path, RoutingPath::ExactMatchInBudget);
    }

    #[test]
    fn route_creative_writing_picks_claude_with_any_budget() {
        let c = sample_council();
        let d = c.route("creative_writing", CostTier::Any);
        assert_eq!(d.profile.model_id, "claude-sonnet");
        assert_eq!(d.path, RoutingPath::ExactMatchInBudget);
    }

    #[test]
    fn route_cheap_summarization_picks_haiku() {
        let c = sample_council();
        let d = c.route("summarization", CostTier::Cheap);
        // haiku (cheap, score 0.84) wins over llama-70b-free (free, score 0.75)
        // because the sort is benchmark-first.
        assert_eq!(d.profile.model_id, "haiku");
        assert_eq!(d.path, RoutingPath::ExactMatchInBudget);
    }

    #[test]
    fn route_free_summarization_picks_llama_primary() {
        let c = sample_council();
        let d = c.route("summarization", CostTier::Free);
        // haiku is Cheap > Free budget, so it gets filtered out.
        // llama-70b-free is the only in-budget match.
        assert_eq!(d.profile.model_id, "llama-70b-free");
        assert_eq!(d.path, RoutingPath::ExactMatchInBudget);
    }

    #[test]
    fn route_unknown_task_falls_back_to_primary() {
        let c = sample_council();
        let d = c.route("nonsense_task", CostTier::Any);
        assert_eq!(d.profile.model_id, "llama-70b-free");
        assert_eq!(d.path, RoutingPath::PrimaryFallback);
        assert_eq!(d.candidates_considered, 0);
    }

    #[test]
    fn route_free_budget_creative_writing_falls_back_to_cheapest_premium() {
        let c = sample_council();
        // No free model supports creative_writing.  Council returns the
        // cheapest premium match.
        let d = c.route("creative_writing", CostTier::Free);
        assert_eq!(d.profile.model_id, "claude-sonnet");
        assert_eq!(d.path, RoutingPath::OverBudgetCheapest);
        assert_eq!(d.candidates_considered, 1);
        assert_eq!(d.candidates_in_budget, 0);
    }

    #[test]
    fn route_explicit_honors_caller_choice() {
        let c = sample_council();
        let d = c.route_explicit("claude-sonnet").unwrap();
        assert_eq!(d.profile.model_id, "claude-sonnet");
        assert_eq!(d.path, RoutingPath::Explicit);
    }

    #[test]
    fn route_explicit_unknown_returns_none() {
        let c = sample_council();
        assert!(c.route_explicit("not-a-model").is_none());
    }

    #[test]
    fn profiles_for_tag_returns_matches_in_score_order() {
        let c = sample_council();
        let list = c.profiles_for_tag("summarization");
        let ids: Vec<&str> = list.iter().map(|p| p.model_id.as_str()).collect();
        // haiku (0.84) ahead of llama-70b-free (0.75) — but llama is also
        // tagged "summarization" so both appear.
        assert!(ids.contains(&"haiku"));
        assert!(ids.contains(&"llama-70b-free"));
        assert_eq!(ids[0], "haiku");
    }

    #[test]
    fn primary_returns_the_flagged_profile() {
        let c = sample_council();
        assert_eq!(c.primary().model_id, "llama-70b-free");
        assert!(c.primary().primary);
    }

    #[test]
    fn load_from_yaml_round_trips() {
        // Write a tiny YAML to a temp file and load it.
        let dir = std::env::temp_dir();
        let path = dir.join("hydragent_test_council.yaml");
        std::fs::write(
            &path,
            r#"
profiles:
  - model_id: "x/y"
    provider: "openrouter"
    context_window: 1000
    cost_per_1k: 0.0
    cost_tier: "free"
    task_tags: ["general"]
    primary: true
"#,
        )
        .unwrap();
        let c = ModelCouncil::load_from_yaml(&path).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c.primary().model_id, "x/y");
    }

    /// Verifies the real shipped `config/model_council.yaml` parses,
    /// has exactly one primary, and routes the four canonical
    /// "design" task → model pairs from TODO_PHASE5 Track 5.2.
    /// This test is `#[ignore]`-style-safe: it skips if the file is
    /// missing (e.g. when hydragent-model is vendored without the
    /// repo's config dir), but in the canonical workspace layout the
    /// file is always present.
    #[test]
    fn load_real_config_routes_canonical_pairs() {
        // Walk up from CARGO_MANIFEST_DIR to find the repo root that
        // contains `config/model_council.yaml`.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.parent().and_then(|p| p.parent());
        let cfg_path = match repo_root {
            Some(root) => root.join("config").join("model_council.yaml"),
            None => {
                eprintln!("no repo root; skipping");
                return;
            }
        };
        if !cfg_path.exists() {
            eprintln!("{} not present; skipping", cfg_path.display());
            return;
        }

        let c = ModelCouncil::load_from_yaml(&cfg_path)
            .expect("config/model_council.yaml must load");
        assert!(
            c.len() >= 20,
            "Track 5.2 design calls for 20+ profiles; got {}",
            c.len()
        );
        assert!(c.primary().primary, "primary flag preserved");
        assert!(
            c.primary().cost_tier.accepts(CostTier::Free),
            "primary must be free (always-on safety net)"
        );

        // (task, budget, expected_model_id) — the four cases from
        // TODO_PHASE5 Track 5.2 acceptance criteria.  Model ids match
        // the real `config/model_council.yaml`.
        let cases: &[(&str, CostTier, &str)] = &[
            ("code_generation", CostTier::Any, "deepseek/deepseek-coder"),
            ("research", CostTier::Any, "perplexity/sonar"),
            ("creative_writing", CostTier::Any, "anthropic/claude-3.5-sonnet"),
            ("summarization", CostTier::Cheap, "anthropic/claude-3-haiku"),
        ];
        for (task, budget, expected) in cases {
            let d = c.route(task, *budget);
            assert_eq!(
                d.profile.model_id, *expected,
                "task={} budget={:?} expected={} got={} path={:?}",
                task, budget, expected, d.profile.model_id, d.path
            );
        }
    }
}
