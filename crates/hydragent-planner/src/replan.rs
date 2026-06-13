//! # Self-Healing Replanner
//!
//! Phase 5 / Track 5.4. When a DAG node fails, the
//! `SelfHealingReplanner` decides how to recover. It does NOT
//! re-run the engine — the engine is still pull-based, so a
//! strategy "applies" by mutating the spec so the engine's next
//! ready-queue pass picks up the recovered node.
//!
//! ## Strategies
//!
//! 1. **`Retry`** — increment `retry_count` and reset status to
//!    `Pending`. Cheapest. Best for transient failures (timeout,
//!    5xx, OOM).
//! 2. **`Reroute`** — change `model_hint` to a fallback model, reset
//!    to `Pending`. Best for model-specific failures (rate-limit,
//!    content-filter, 401).
//! 3. **`Decompose`** — lower the task's complexity (simpler
//!    `TaskType`, fewer `allowed_tools`). Best when the failure
//!    indicates the task was too big for the assigned role.
//! 4. **`Escalate`** — mark `Skipped` and signal unrecoverable. Last
//!    resort. Caller should surface to the user.
//!
//! ## Why mutation, not a separate "plan" object?
//!
//! Keeping the recovery as a *spec mutation* (not a side-tree) lets
//! the engine stay simple: the same `ReadyQueue` that processed
//! the original graph will pick up the recovered node on the next
//! tick. No engine-level reentry logic needed.
//!
//! ## Determinism
//!
//! `decide_and_apply` is stochastic (uses `rand`). For tests and
//! audit purposes, `force_strategy` lets callers pin a specific
//! strategy and exercise each path deterministically.
//!
//! ## Example
//!
//! ```no_run
//! use hydragent_planner::dag::{DagNode, DagSpec, NodeStatus, TaskType};
//! use hydragent_planner::replan::{SelfHealingReplanner, FailureInfo};
//!
//! # fn build_spec() -> DagSpec {
//! #     DagSpec {
//! #         swarm_id: "s".into(), page_id: "p".into(),
//! #         original_task: "t".into(),
//! #         nodes: vec![DagNode {
//! #             id: "A".into(), name: "root".into(),
//! #             description: "do the thing".into(),
//! #             task_type: TaskType::General,
//! #             allowed_tools: vec!["echo".into()],
//! #             model_hint: None,
//! #             token_budget: 0, timeout_ms: 0,
//! #             retry_count: 0, max_retries: 2,
//! #             status: NodeStatus::Failed,
//! #             result: None,
//! #         }],
//! #         edges: vec![],
//! #         created_at: 0,
//! #     }
//! # }
//! let mut spec = build_spec();
//! let planner = SelfHealingReplanner::new(2, vec!["openai/gpt-4o-mini".into()]);
//! let failure = FailureInfo {
//!     node_id: "A".into(),
//!     error_message: "rate limit".into(),
//!     attempt_count: 0,
//! };
//! let outcome = planner.decide_and_apply(&mut spec, &failure);
//! // outcome is ReplanOutcome::Applied(ReplanStrategy::Retry{..}) most of the time.
//! ```

use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dag::{DagNode, DagSpec, NodeStatus, TaskType};

/// Replanner-level errors.
#[derive(Debug, Error)]
pub enum ReplanError {
    #[error("replan: node {0} not found in spec")]
    NodeNotFound(String),
    #[error("replan: invalid strategy: {0}")]
    InvalidStrategy(String),
}

/// Which recovery action the replanner took (or would have taken
/// for `force_strategy`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "strategy")]
pub enum ReplanStrategy {
    /// Re-attempt the same node, same model, same inputs.
    Retry { attempt: u32, max_attempts: u32 },
    /// Re-attempt with a different model from the fallback list.
    Reroute {
        from_model: Option<String>,
        to_model: String,
    },
    /// Lower task complexity (simpler TaskType + minimal tools).
    /// (Real "split into N sub-nodes" is reserved for a future
    /// Track — this implementation does the lighter "simplify" form,
    /// which is what most production self-healers do on the first
    /// failure.)
    Decompose { new_task_type: TaskType, subtask_count: u8 },
    /// Mark unrecoverable; caller should escalate to user.
    Escalate { reason: String },
    /// No recovery possible (e.g. caller asked for a strategy the
    /// replanner doesn't know). Logged but not applied.
    NoOp { reason: String },
}

impl ReplanStrategy {
    pub fn name(&self) -> &'static str {
        match self {
            ReplanStrategy::Retry { .. } => "retry",
            ReplanStrategy::Reroute { .. } => "reroute",
            ReplanStrategy::Decompose { .. } => "decompose",
            ReplanStrategy::Escalate { .. } => "escalate",
            ReplanStrategy::NoOp { .. } => "noop",
        }
    }
}

/// The result of running the replanner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplanOutcome {
    /// A strategy was applied (and the spec mutated) or queued for
    /// the caller to apply.
    Applied(ReplanStrategy),
    /// Nothing was done (e.g. retries exhausted and Escalate is the
    /// wrong move, or caller asked for an unknown strategy).
    NoAction(String),
}

/// What failed. Produced by the engine (or by tests) when a node
/// ends in `Failed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureInfo {
    pub node_id: String,
    pub error_message: String,
    /// How many times this node has already been retried (0 on the
    /// first failure). Bumped by `apply_retry` and `apply_reroute`.
    pub attempt_count: u32,
}

impl FailureInfo {
    pub fn new(node_id: impl Into<String>, error_message: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            error_message: error_message.into(),
            attempt_count: 0,
        }
    }
}

/// Bias knobs for stochastic strategy selection. The defaults
/// represent a reasonable "fail-soft" bias: prefer cheap retry
/// first, fall back to model swap, then decompose, escalate last.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplanBias {
    /// Weight for `Retry` (0.0–1.0).
    pub retry: f64,
    /// Weight for `Reroute` (0.0–1.0).
    pub reroute: f64,
    /// Weight for `Decompose` (0.0–1.0).
    pub decompose: f64,
    /// Weight for `Escalate` (0.0–1.0).
    pub escalate: f64,
}

impl Default for ReplanBias {
    fn default() -> Self {
        Self {
            retry: 0.60,
            reroute: 0.20,
            decompose: 0.10,
            escalate: 0.10,
        }
    }
}

/// The self-healing replanner. Cheap to construct (`Clone`).
#[derive(Debug, Clone)]
pub struct SelfHealingReplanner {
    max_retries: u32,
    fallback_models: Vec<String>,
    bias: ReplanBias,
}

impl SelfHealingReplanner {
    /// Build a replanner with the given `max_retries` and
    /// `fallback_models` (used by `Reroute`).
    pub fn new(max_retries: u32, fallback_models: Vec<String>) -> Self {
        Self {
            max_retries,
            fallback_models,
            bias: ReplanBias::default(),
        }
    }

    /// Override the stochastic bias.
    pub fn with_bias(mut self, bias: ReplanBias) -> Self {
        self.bias = bias;
        self
    }

    /// The configured max-retries. Convenience accessor for tests
    /// and observability.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// The configured fallback models.
    pub fn fallback_models(&self) -> &[String] {
        &self.fallback_models
    }

    /// Decide on a strategy and apply it. Stochastic (driven by
    /// `rand`) unless the bias assigns zero weight to every option
    /// (in which case the first strategy with non-zero weight wins).
    /// If `max_retries` is already exceeded, we always escalate
    /// regardless of bias.
    pub fn decide_and_apply(
        &self,
        spec: &mut DagSpec,
        failure: &FailureInfo,
    ) -> ReplanOutcome {
        // 1. Hard ceiling: if retries are exhausted, always Escalate.
        if failure.attempt_count >= self.max_retries {
            let strategy = self.apply_escalate(spec, failure);
            return ReplanOutcome::Applied(strategy);
        }

        // 2. Pick a strategy by weighted roll.
        let mut rng = rand::thread_rng();
        let strategy = self.pick_strategy(&mut rng);
        self.apply(spec, failure, &strategy)
    }

    /// Force a specific strategy. Returns `NoAction` if the strategy
    /// is not applicable (e.g. `Retry` past max_retries,
    /// `Reroute` with empty fallback list).
    pub fn force_strategy(
        &self,
        spec: &mut DagSpec,
        failure: &FailureInfo,
        strategy_name: &str,
    ) -> ReplanOutcome {
        let applied = match strategy_name {
            "retry" => {
                if failure.attempt_count >= self.max_retries {
                    ReplanStrategy::NoOp {
                        reason: format!(
                            "retry requested but attempt_count={} >= max_retries={}",
                            failure.attempt_count, self.max_retries
                        ),
                    }
                } else {
                    self.apply_retry(spec, failure)
                }
            }
            "reroute" => {
                if self.fallback_models.is_empty() {
                    ReplanStrategy::NoOp {
                        reason: "reroute requested but fallback_models is empty".to_string(),
                    }
                } else {
                    let mut rng = rand::thread_rng();
                    self.apply_reroute(spec, failure, &mut rng)
                }
            }
            "decompose" => self.apply_decompose(spec, failure),
            "escalate" => self.apply_escalate(spec, failure),
            other => ReplanStrategy::NoOp {
                reason: format!("unknown strategy: {other}"),
            },
        };
        match applied {
            ReplanStrategy::NoOp { .. } => ReplanOutcome::NoAction(applied.name().to_string()),
            _ => ReplanOutcome::Applied(applied),
        }
    }

    /// Apply a pre-decided strategy. Internal helper.
    fn apply(
        &self,
        spec: &mut DagSpec,
        failure: &FailureInfo,
        strategy: &ReplanStrategy,
    ) -> ReplanOutcome {
        let applied = match strategy {
            ReplanStrategy::Retry { .. } => self.apply_retry(spec, failure),
            ReplanStrategy::Reroute { .. } => {
                let mut rng = rand::thread_rng();
                self.apply_reroute(spec, failure, &mut rng)
            }
            ReplanStrategy::Decompose { .. } => self.apply_decompose(spec, failure),
            ReplanStrategy::Escalate { .. } => self.apply_escalate(spec, failure),
            ReplanStrategy::NoOp { reason } => ReplanStrategy::NoOp { reason: reason.clone() },
        };
        match applied {
            ReplanStrategy::NoOp { .. } => ReplanOutcome::NoAction(applied.name().to_string()),
            _ => ReplanOutcome::Applied(applied),
        }
    }

    /// Weighted pick from the bias.
    fn pick_strategy<R: Rng>(&self, rng: &mut R) -> ReplanStrategy {
        let total = self.bias.retry
            + self.bias.reroute
            + self.bias.decompose
            + self.bias.escalate;
        if total <= 0.0 {
            // All-zero bias → fall back to Retry.
            return ReplanStrategy::Retry {
                attempt: 0,
                max_attempts: self.max_retries,
            };
        }
        let roll: f64 = rng.gen_range(0.0..total);
        if roll < self.bias.retry {
            ReplanStrategy::Retry {
                attempt: 0,
                max_attempts: self.max_retries,
            }
        } else if roll < self.bias.retry + self.bias.reroute {
            // Pick a model now; the actual swap happens in apply_reroute.
            let to_model = self
                .fallback_models
                .first()
                .cloned()
                .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
            ReplanStrategy::Reroute {
                from_model: None,
                to_model,
            }
        } else if roll < self.bias.retry + self.bias.reroute + self.bias.decompose {
            ReplanStrategy::Decompose {
                new_task_type: TaskType::General,
                subtask_count: 1,
            }
        } else {
            ReplanStrategy::Escalate {
                reason: "bias roll landed on escalate".into(),
            }
        }
    }

    /// Reset the node to `Pending` and bump its retry counter.
    fn apply_retry(&self, spec: &mut DagSpec, failure: &FailureInfo) -> ReplanStrategy {
        let attempt = match find_node_mut(spec, &failure.node_id) {
            Some(n) => {
                n.retry_count = n.retry_count.saturating_add(1);
                n.status = NodeStatus::Pending;
                n.result = None;
                n.retry_count
            }
            None => return ReplanStrategy::NoOp { reason: format!("node {} not found", failure.node_id) },
        };
        ReplanStrategy::Retry {
            attempt,
            max_attempts: self.max_retries,
        }
    }

    /// Swap the node's `model_hint` to a random fallback and reset.
    fn apply_reroute<R: Rng>(
        &self,
        spec: &mut DagSpec,
        failure: &FailureInfo,
        rng: &mut R,
    ) -> ReplanStrategy {
        if self.fallback_models.is_empty() {
            return ReplanStrategy::NoOp {
                reason: "fallback_models is empty".to_string(),
            };
        }
        let to_model = self
            .fallback_models
            .choose(rng)
            .cloned()
            .expect("non-empty: checked above");
        let from = match find_node_mut(spec, &failure.node_id) {
            Some(n) => {
                let from = n.model_hint.clone();
                n.model_hint = Some(to_model.clone());
                n.retry_count = n.retry_count.saturating_add(1);
                n.status = NodeStatus::Pending;
                n.result = None;
                from
            }
            None => return ReplanStrategy::NoOp { reason: format!("node {} not found", failure.node_id) },
        };
        ReplanStrategy::Reroute {
            from_model: from,
            to_model,
        }
    }

    /// Lower the task complexity: simpler `TaskType` + minimal
    /// `allowed_tools` + reset to `Pending`.
    fn apply_decompose(&self, spec: &mut DagSpec, failure: &FailureInfo) -> ReplanStrategy {
        let new_type = match find_node_mut(spec, &failure.node_id) {
            Some(n) => {
                let new = simplify_task_type(&n.task_type);
                n.task_type = new.clone();
                n.allowed_tools = vec!["echo".to_string()];
                n.retry_count = n.retry_count.saturating_add(1);
                n.status = NodeStatus::Pending;
                n.result = None;
                new
            }
            None => return ReplanStrategy::NoOp { reason: format!("node {} not found", failure.node_id) },
        };
        ReplanStrategy::Decompose {
            new_task_type: new_type,
            subtask_count: 1,
        }
    }

    /// Mark the node `Skipped`. Caller should treat this as a
    /// terminal failure for the whole run (the engine's existing
    /// `skip_downstream` will then propagate to descendants).
    fn apply_escalate(&self, spec: &mut DagSpec, failure: &FailureInfo) -> ReplanStrategy {
        match find_node_mut(spec, &failure.node_id) {
            Some(n) => {
                n.status = NodeStatus::Skipped;
            }
            None => return ReplanStrategy::NoOp { reason: format!("node {} not found", failure.node_id) },
        }
        ReplanStrategy::Escalate {
            reason: failure.error_message.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find a node by id and return a mutable reference. Internal.
fn find_node_mut<'a>(spec: &'a mut DagSpec, node_id: &str) -> Option<&'a mut DagNode> {
    spec.nodes.iter_mut().find(|n| n.id == node_id)
}

/// Map a complex `TaskType` to a simpler one for `Decompose`.
fn simplify_task_type(t: &TaskType) -> TaskType {
    match t {
        // Already simple.
        TaskType::General | TaskType::Summarization | TaskType::DataExtraction => t.clone(),
        // Demote complex task types to General.
        _ => TaskType::General,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, task: TaskType) -> DagNode {
        DagNode {
            id: id.into(),
            name: id.into(),
            description: format!("node {id}"),
            task_type: task,
            allowed_tools: vec!["file_read".into(), "echo".into()],
            model_hint: Some("openai/gpt-4o".into()),
            token_budget: 0,
            timeout_ms: 0,
            retry_count: 0,
            max_retries: 3,
            status: NodeStatus::Failed,
            result: None,
        }
    }

    fn make_spec() -> DagSpec {
        DagSpec {
            swarm_id: "s1".into(),
            page_id: "p1".into(),
            original_task: "test".into(),
            nodes: vec![make_node("A", TaskType::CodeGeneration)],
            edges: vec![],
            created_at: 0,
        }
    }

    fn failure(node_id: &str, attempt: u32) -> FailureInfo {
        FailureInfo {
            node_id: node_id.into(),
            error_message: "synthetic failure".into(),
            attempt_count: attempt,
        }
    }

    #[test]
    fn force_retry_resets_status_and_bumps_count() {
        let planner = SelfHealingReplanner::new(3, vec!["openai/gpt-4o-mini".into()]);
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("A", 0), "retry");
        assert!(matches!(outcome, ReplanOutcome::Applied(ReplanStrategy::Retry { attempt: 1, .. })));
        let node = spec.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(node.status, NodeStatus::Pending);
        assert_eq!(node.retry_count, 1);
        assert!(node.result.is_none());
    }

    #[test]
    fn force_retry_refuses_past_max() {
        let planner = SelfHealingReplanner::new(2, vec![]);
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("A", 2), "retry");
        assert!(matches!(outcome, ReplanOutcome::NoAction(_)));
        // Spec must not have been mutated.
        let node = spec.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(node.status, NodeStatus::Failed);
        assert_eq!(node.retry_count, 0);
    }

    #[test]
    fn force_reroute_swaps_model_hint() {
        let planner = SelfHealingReplanner::new(
            3,
            vec!["anthropic/claude-3.5-sonnet".into(), "openai/gpt-4o-mini".into()],
        );
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("A", 0), "reroute");
        match outcome {
            ReplanOutcome::Applied(ReplanStrategy::Reroute { from_model, to_model }) => {
                assert_eq!(from_model, Some("openai/gpt-4o".into()));
                assert!(to_model == "anthropic/claude-3.5-sonnet"
                    || to_model == "openai/gpt-4o-mini");
            }
            _ => panic!("expected reroute outcome, got {outcome:?}"),
        }
        let node = spec.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(node.status, NodeStatus::Pending);
        assert_eq!(node.retry_count, 1);
        // The new model_hint must be one of the fallbacks.
        assert!(node.model_hint.is_some());
        assert_ne!(node.model_hint.as_deref(), Some("openai/gpt-4o"));
    }

    #[test]
    fn force_reroute_with_empty_fallbacks_returns_no_action() {
        let planner = SelfHealingReplanner::new(3, vec![]);
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("A", 0), "reroute");
        assert!(matches!(outcome, ReplanOutcome::NoAction(_)));
    }

    #[test]
    fn force_decompose_simplifies_task_type_and_tools() {
        let planner = SelfHealingReplanner::new(3, vec![]);
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("A", 0), "decompose");
        match outcome {
            ReplanOutcome::Applied(ReplanStrategy::Decompose { new_task_type, .. }) => {
                assert_eq!(new_task_type, TaskType::General);
            }
            _ => panic!("expected decompose outcome, got {outcome:?}"),
        }
        let node = spec.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(node.task_type, TaskType::General);
        assert_eq!(node.allowed_tools, vec!["echo".to_string()]);
        assert_eq!(node.status, NodeStatus::Pending);
        assert_eq!(node.retry_count, 1);
    }

    #[test]
    fn force_escalate_marks_node_skipped() {
        let planner = SelfHealingReplanner::new(3, vec![]);
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("A", 0), "escalate");
        match outcome {
            ReplanOutcome::Applied(ReplanStrategy::Escalate { reason }) => {
                assert!(reason.contains("synthetic failure"));
            }
            _ => panic!("expected escalate outcome, got {outcome:?}"),
        }
        let node = spec.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(node.status, NodeStatus::Skipped);
    }

    #[test]
    fn decide_and_apply_always_escalates_at_max_retries() {
        let planner = SelfHealingReplanner::new(1, vec!["x".into()]);
        let mut spec = make_spec();
        // attempt_count == max_retries → must escalate no matter the bias.
        let outcome = planner.decide_and_apply(&mut spec, &failure("A", 1));
        assert!(matches!(outcome, ReplanOutcome::Applied(ReplanStrategy::Escalate { .. })));
        let node = spec.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(node.status, NodeStatus::Skipped);
    }

    #[test]
    fn decide_and_apply_respects_zero_bias_falls_back_to_retry() {
        let planner = SelfHealingReplanner::new(3, vec!["x".into()])
            .with_bias(ReplanBias { retry: 0.0, reroute: 0.0, decompose: 0.0, escalate: 0.0 });
        let mut spec = make_spec();
        let outcome = planner.decide_and_apply(&mut spec, &failure("A", 0));
        // All-zero bias → first strategy with non-zero weight (here
        // none) → fall back to Retry.
        assert!(matches!(outcome, ReplanOutcome::Applied(ReplanStrategy::Retry { .. })));
    }

    #[test]
    fn decide_and_apply_pure_retry_bias_picks_retry() {
        let planner = SelfHealingReplanner::new(3, vec!["x".into()])
            .with_bias(ReplanBias { retry: 1.0, reroute: 0.0, decompose: 0.0, escalate: 0.0 });
        let mut spec = make_spec();
        for _ in 0..10 {
            spec.nodes[0] = make_node("A", TaskType::CodeGeneration);
            let outcome = planner.decide_and_apply(&mut spec, &failure("A", 0));
            assert!(matches!(outcome, ReplanOutcome::Applied(ReplanStrategy::Retry { .. })));
        }
    }

    #[test]
    fn decide_and_apply_pure_escalate_bias_picks_escalate() {
        let planner = SelfHealingReplanner::new(3, vec!["x".into()])
            .with_bias(ReplanBias { retry: 0.0, reroute: 0.0, decompose: 0.0, escalate: 1.0 });
        let mut spec = make_spec();
        let outcome = planner.decide_and_apply(&mut spec, &failure("A", 0));
        assert!(matches!(outcome, ReplanOutcome::Applied(ReplanStrategy::Escalate { .. })));
    }

    #[test]
    fn force_unknown_strategy_returns_no_action() {
        let planner = SelfHealingReplanner::new(3, vec![]);
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("A", 0), "unicorn");
        assert!(matches!(outcome, ReplanOutcome::NoAction(_)));
    }

    #[test]
    fn force_strategy_for_missing_node_is_no_op() {
        let planner = SelfHealingReplanner::new(3, vec!["x".into()]);
        let mut spec = make_spec();
        let outcome = planner.force_strategy(&mut spec, &failure("DOES_NOT_EXIST", 0), "retry");
        assert!(matches!(outcome, ReplanOutcome::NoAction(_)));
    }

    #[test]
    fn simplify_task_type_demotes_complex_to_general() {
        assert_eq!(simplify_task_type(&TaskType::CodeGeneration), TaskType::General);
        assert_eq!(simplify_task_type(&TaskType::Research), TaskType::General);
        assert_eq!(simplify_task_type(&TaskType::Reasoning), TaskType::General);
        assert_eq!(simplify_task_type(&TaskType::Planning), TaskType::General);
        // Already simple → unchanged.
        assert_eq!(simplify_task_type(&TaskType::General), TaskType::General);
        assert_eq!(simplify_task_type(&TaskType::Summarization), TaskType::Summarization);
    }

    #[test]
    fn replan_strategy_name_is_stable() {
        assert_eq!(
            ReplanStrategy::Retry { attempt: 1, max_attempts: 3 }.name(),
            "retry"
        );
        assert_eq!(
            ReplanStrategy::Reroute { from_model: None, to_model: "x".into() }.name(),
            "reroute"
        );
        assert_eq!(
            ReplanStrategy::Decompose {
                new_task_type: TaskType::General,
                subtask_count: 1
            }
            .name(),
            "decompose"
        );
        assert_eq!(
            ReplanStrategy::Escalate { reason: "x".into() }.name(),
            "escalate"
        );
        assert_eq!(
            ReplanStrategy::NoOp { reason: "x".into() }.name(),
            "noop"
        );
    }

    #[test]
    fn replan_strategy_serialization_round_trip() {
        let s = ReplanStrategy::Reroute {
            from_model: Some("a".into()),
            to_model: "b".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ReplanStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
