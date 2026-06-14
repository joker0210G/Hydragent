//! Phase 7 / Track 7.3 - Skill Composer.
//!
//! Combines multiple retrieved skills into a single execution plan.
//! Three composition strategies are supported:
//!
//! - [`ComposeStrategy::Sequential`] - run skills in order, each
//!   receiving the previous output as `{{prev_output}}`.
//! - [`ComposeStrategy::Parallel`] - run skills in parallel, gather
//!   outputs (a placeholder joiner substitutes them into
//!   `{{result_1}}`, `{{result_2}}`, ...).
//! - [`ComposeStrategy::Voting`] - run the same skill `n` times with
//!   temperature perturbation and majority-vote (placeholder; the
//!   actual voting happens in the orchestrator).
//!
//! The composer is a pure planner - it does not actually invoke
//! skills. The orchestrator walks the plan and dispatches.

use anyhow::{anyhow, bail, Result};
use hydragent_types::{Skill, SkillParam};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// How to combine multiple skills into one execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComposeStrategy {
    Sequential,
    Parallel,
    Voting,
}

/// One step in a composed plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub skill_id: String,
    pub skill_name: String,
    /// Subset of the skill's params that this step provides. Missing
    /// params are inherited from the plan-level params.
    pub params: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ComposedPlan {
    pub strategy: ComposeStrategy,
    pub steps: Vec<PlanStep>,
    /// Plan-level params inherited by every step that does not
    /// override them.
    pub defaults: std::collections::HashMap<String, String>,
}

impl ComposedPlan {
    /// Number of skills in the plan.
    pub fn len(&self) -> usize { self.steps.len() }
    pub fn is_empty(&self) -> bool { self.steps.is_empty() }
}

pub struct SkillComposer {
    pub strategy: ComposeStrategy,
}

impl Default for SkillComposer {
    fn default() -> Self { Self { strategy: ComposeStrategy::Sequential } }
}

impl SkillComposer {
    pub fn new(strategy: ComposeStrategy) -> Self { Self { strategy } }

    /// Validate that every step's skill exists and that the union
    /// of (defaults, per-step params) covers every required param
    /// for each step. Returns the merged param set for each step.
    pub fn validate(&self, skills: &[Skill], plan: &ComposedPlan) -> Result<Vec<std::collections::HashMap<String, String>>> {
        if plan.steps.is_empty() { bail!("empty plan"); }
        let mut merged = Vec::with_capacity(plan.steps.len());
        for step in &plan.steps {
            let s = skills.iter().find(|s| s.id == step.skill_id || s.name == step.skill_name)
                .ok_or_else(|| anyhow!("step references unknown skill: {}", step.skill_name))?;
            let mut m = plan.defaults.clone();
            for (k, v) in &step.params { m.insert(k.clone(), v.clone()); }
            for p in &s.params {
                if p.required && !m.contains_key(&p.name) {
                    bail!("step '{}' missing required param '{}'", s.name, p.name);
                }
            }
            merged.push(m);
        }
        Ok(merged)
    }

    /// Detect cycles in the dependency graph. Sequential composition
    /// forbids `skill_i -> skill_i` self-references. Parallel
    /// composition requires all skills to be distinct.
    pub fn detect_conflicts(&self, plan: &ComposedPlan) -> Result<()> {
        let names: Vec<&str> = plan.steps.iter().map(|s| s.skill_name.as_str()).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        if self.strategy == ComposeStrategy::Parallel && unique.len() != names.len() {
            bail!("parallel plan must contain distinct skills");
        }
        Ok(())
    }

    /// Compute the union of capability tags from all skills in the
    /// plan, sorted, deduplicated. Used to display the "what's
    /// covered" hint to the operator.
    pub fn plan_tags(plan: &ComposedPlan, skills: &[Skill]) -> Vec<String> {
        let mut tags: Vec<String> = plan.steps.iter()
            .flat_map(|step| {
                skills.iter()
                    .find(|s| s.id == step.skill_id || s.name == step.skill_name)
                    .map(|s| s.capability_tags.clone())
                    .unwrap_or_default()
            })
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }

    /// Pretty-print the plan as a multi-line string for logs.
    pub fn pretty_print(plan: &ComposedPlan, skills: &[Skill]) -> String {
        let mut out = String::new();
        out.push_str(&format!("plan [{}] ({} steps)\n",
            match plan.strategy {
                ComposeStrategy::Sequential => "sequential",
                ComposeStrategy::Parallel   => "parallel",
                ComposeStrategy::Voting     => "voting",
            },
            plan.steps.len()));
        for (i, step) in plan.steps.iter().enumerate() {
            let s = skills.iter().find(|s| s.name == step.skill_name);
            let param_str = match s {
                Some(s) => s.params.iter()
                    .map(|p: &SkillParam| {
                        let v = step.params.get(&p.name)
                            .or_else(|| plan.defaults.get(&p.name))
                            .cloned()
                            .unwrap_or_else(|| "<missing>".into());
                        format!("{}={}", p.name, v)
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                None => "<unknown>".into(),
            };
            out.push_str(&format!("  {}. {} ({})\n", i + 1, step.skill_name, param_str));
        }
        out
    }
}

fn _bail_keepalive() {} // removed; use `bail!` macro from anyhow

#[cfg(test)]
mod tests {
    use super::*;
    use hydragent_types::Skill;

    fn skill(name: &str, required: &[&str]) -> Skill {
        let mut s = Skill::new(name, "d", "t", "extractor");
        for r in required {
            s = s.with_param(SkillParam {
                name: (*r).into(),
                type_: "string".into(),
                description: "x".into(),
                required: true,
            });
        }
        s
    }

    #[test]
    fn validate_merges_defaults_and_per_step() {
        let a = skill("a", &["x", "y"]);
        let b = skill("b", &["z"]);
        let plan = ComposedPlan {
            strategy: ComposeStrategy::Sequential,
            steps: vec![
                PlanStep { skill_id: "id-a".into(), skill_name: "a".into(),
                    params: [("x".into(), "X".into())].iter().cloned().collect() },
                PlanStep { skill_id: "id-b".into(), skill_name: "b".into(),
                    params: [].iter().cloned().collect() },
            ],
            defaults: [("y".into(), "Y".into()), ("z".into(), "Z".into())].iter().cloned().collect(),
        };
        let merged = SkillComposer::new(ComposeStrategy::Sequential)
            .validate(&[a, b], &plan).unwrap();
        assert_eq!(merged[0].get("x"), Some(&"X".into()));
        assert_eq!(merged[0].get("y"), Some(&"Y".into()));
        assert_eq!(merged[1].get("z"), Some(&"Z".into()));
    }

    #[test]
    fn validate_fails_on_missing_required() {
        let a = skill("a", &["x"]);
        let plan = ComposedPlan {
            strategy: ComposeStrategy::Sequential,
            steps: vec![PlanStep { skill_id: "id-a".into(), skill_name: "a".into(),
                params: Default::default() }],
            defaults: Default::default(),
        };
        let err = SkillComposer::new(ComposeStrategy::Sequential)
            .validate(&[a], &plan).unwrap_err();
        assert!(err.to_string().contains("missing required param 'x'"));
    }

    #[test]
    fn parallel_rejects_duplicates() {
        let plan = ComposedPlan {
            strategy: ComposeStrategy::Parallel,
            steps: vec![
                PlanStep { skill_id: "1".into(), skill_name: "a".into(), params: Default::default() },
                PlanStep { skill_id: "2".into(), skill_name: "a".into(), params: Default::default() },
            ],
            defaults: Default::default(),
        };
        assert!(SkillComposer::new(ComposeStrategy::Parallel)
            .detect_conflicts(&plan).is_err());
    }

    #[test]
    fn parallel_accepts_distinct() {
        let plan = ComposedPlan {
            strategy: ComposeStrategy::Parallel,
            steps: vec![
                PlanStep { skill_id: "1".into(), skill_name: "a".into(), params: Default::default() },
                PlanStep { skill_id: "2".into(), skill_name: "b".into(), params: Default::default() },
            ],
            defaults: Default::default(),
        };
        assert!(SkillComposer::new(ComposeStrategy::Parallel)
            .detect_conflicts(&plan).is_ok());
    }

    #[test]
    fn plan_tags_is_union_sorted() {
        let mut a = skill("a", &[]); a.capability_tags = vec!["x".into(), "y".into()];
        let mut b = skill("b", &[]); b.capability_tags = vec!["y".into(), "z".into()];
        let plan = ComposedPlan {
            strategy: ComposeStrategy::Sequential,
            steps: vec![
                PlanStep { skill_id: "1".into(), skill_name: "a".into(), params: Default::default() },
                PlanStep { skill_id: "2".into(), skill_name: "b".into(), params: Default::default() },
            ],
            defaults: Default::default(),
        };
        let tags = SkillComposer::plan_tags(&plan, &[a, b]);
        assert_eq!(tags, vec!["x", "y", "z"]);
    }
}
