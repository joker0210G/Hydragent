//! # DAG Execution Engine
//!
//! Phase 5 / Track 5.3. This is the runtime that consumes a `DagSpec`
//! and drives it to completion by spawning sub-agents in topological
//! order while respecting the per-node "ready" predicate.
//!
//! ## Loop
//!
//! ```text
//! loop {
//!     let ready = ReadyQueue::new(&mut spec).get_ready_nodes();
//!     if ready.is_empty() { break; }            // nothing to do
//!     for node in ready {
//!         mark Running
//!         spawn(node) -> JoinHandle<SubAgentStatus>
//!     }
//!     await_one_completion(&mut join_set)        // semaphore-bounded
//!     mark Completed/Failed on the resolved node
//!     if any sibling failed and that should fail the run, return
//! }
//! ```
//!
//! ## Why a fresh `ReadyQueue` each loop iteration
//!
//! `ReadyQueue` borrows the `DagSpec` mutably to update status. We
//! snapshot the ready list and release the borrow before spawning
//! (the spawner is `Clone`, the spec is owned by the engine), so we
//! can re-build a fresh `ReadyQueue` after each `JoinHandle`
//! resolves.
//!
//! ## Concurrency
//!
//! `DagExecutionEngine` holds a `tokio::sync::Semaphore` with the
//! requested `max_concurrent` permits. Every spawn acquires a permit
//! (waiting if at the cap) and releases it on completion. The default
//! is **unbounded** (`max_concurrent = 0`) for tests and
//! single-machine development; production callers should set it to a
//! sane cap (e.g. 4–8) to avoid hammering downstream LLM providers.
//!
//! ## Failure policy
//!
//! The default behavior is "fail fast": if any node in the run fails,
//! the engine marks every still-Pending descendant as `Skipped` (since
//! its dependency will never complete) and exits with an
//! `EngineError::NodeFailed`. `ExecutionReport.failed_node_ids` lists
//! the seed failures.
//!
//! Callers wanting graceful degradation (e.g. allow one branch to
//! fail and continue with the other) can post-process the report and
//! rely on `EngineError::NodeFailed` carrying the list of failures.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use hydragent_swarm::SubAgentSpawner;
use hydragent_types::{AgentState, SubAgentRole, SubAgentSpec, SubAgentStatus};

use crate::dag::{DagNode, DagSpec, NodeResult, NodeStatus, TaskType};
use crate::scheduler::ReadyQueue;

/// Engine-level errors.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The input spec is malformed (missing nodes, unknown edges, ...).
    #[error("invalid spec: {0}")]
    InvalidSpec(String),
    /// At least one node failed. The string lists the failed node ids.
    #[error("node(s) failed: {0}")]
    NodeFailed(String),
    /// The internal join task panicked.
    #[error("internal join error: {0}")]
    Join(#[from] tokio::task::JoinError),
    /// Spec validation / toposort failed.
    #[error("spec validation: {0}")]
    Spec(String),
}

/// Per-node outcome recorded in `ExecutionReport.node_results`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeOutcome {
    pub node_id: String,
    pub status: NodeStatus,
    /// Wall-clock duration of the spawn → completion path (ms).
    pub execution_ms: u64,
    /// Model that produced the result, if any.
    pub model_used: String,
    /// Tokens used by the sub-agent (LLM-side accounting).
    pub tokens_used: u32,
    /// Final assistant content (empty on failure/cancellation).
    pub output: String,
    /// Error message if the node ended in `Failed` or `Cancelled`.
    pub error: Option<String>,
    /// When the node was first dispatched (epoch ms).
    pub started_at_ms: i64,
    /// When the node finished (epoch ms).
    pub finished_at_ms: i64,
}

/// Full report produced by `DagExecutionEngine::run`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub swarm_id: String,
    pub page_id: String,
    pub original_task: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub total_execution_ms: u64,
    /// Counts by terminal status.
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub skipped: usize,
    /// Per-node outcomes, keyed by node id. Always populated for
    /// every node in the spec, even if the node was skipped (in which
    /// case `status == Skipped` and `output == ""`).
    pub node_results: HashMap<String, NodeOutcome>,
    /// Final on-disk spec after status updates. Useful for the
    /// supervisor (Track 5.4) and for tests.
    pub final_spec: DagSpec,
}

impl ExecutionReport {
    /// True if every node ended in `Completed`.
    pub fn is_success(&self) -> bool {
        self.failed == 0 && self.cancelled == 0
    }
}

/// Maps a planner `TaskType` to a sub-agent `SubAgentRole`. The
/// mapping is intentionally lossy — TaskType is about **what** the
/// node does, Role is about **how** the sub-agent behaves.
pub fn task_type_to_role(t: TaskType) -> SubAgentRole {
    match t {
        TaskType::CodeGeneration   => SubAgentRole::Build,
        TaskType::Research         => SubAgentRole::Explore,
        TaskType::CreativeWriting  => SubAgentRole::General,
        TaskType::Reasoning        => SubAgentRole::Plan,
        TaskType::Summarization    => SubAgentRole::Scout,
        TaskType::DataExtraction   => SubAgentRole::Scout,
        TaskType::Planning         => SubAgentRole::Plan,
        TaskType::Review           => SubAgentRole::Review,
        TaskType::General          => SubAgentRole::General,
    }
}

/// Convert a `DagNode` into a `SubAgentSpec` ready to be handed to
/// the `SubAgentSpawner`. The `swarm_id` and `parent_page_id` come
/// from the surrounding `DagSpec` (not the node), since they apply
/// to the whole graph.
pub fn dag_node_to_spec(node: &DagNode, swarm_id: &str, page_id: &str) -> SubAgentSpec {
    let role = task_type_to_role(node.task_type.clone());
    let mut spec = SubAgentSpec::new(node.name.clone(), role, node.description.clone());
    spec.id = node.id.clone();
    spec.system_prompt = String::new();
    spec.allowed_tools = if node.allowed_tools.is_empty() {
        role.default_tools().iter().map(|s| s.to_string()).collect()
    } else {
        node.allowed_tools.clone()
    };
    spec.model_hint = node.model_hint.clone();
    spec.token_budget = if node.token_budget == 0 {
        role.default_token_budget()
    } else {
        node.token_budget
    };
    spec.timeout_ms = if node.timeout_ms == 0 {
        role.default_timeout_ms()
    } else {
        node.timeout_ms
    };
    spec.swarm_id = swarm_id.to_string();
    spec.parent_page_id = page_id.to_string();
    spec
}

/// The engine. Cheap to construct; holds cheap handles.
pub struct DagExecutionEngine {
    spawner: SubAgentSpawner,
    /// 0 = unbounded. Otherwise a hard cap on simultaneously running
    /// sub-agents.
    max_concurrent: usize,
}

impl DagExecutionEngine {
    /// Build a new engine. `max_concurrent` = 0 means unbounded.
    pub fn new(spawner: SubAgentSpawner, max_concurrent: usize) -> Arc<Self> {
        Arc::new(Self {
            spawner,
            max_concurrent,
        })
    }

    /// Run the spec to completion. Returns the populated
    /// `ExecutionReport` on success or `EngineError::NodeFailed` if
    /// at least one node failed (the report is dropped in that
    /// case). Use `run_with_outcome` if you need the report on
    /// failure as well.
    pub async fn run(&self, spec: DagSpec) -> Result<ExecutionReport, EngineError> {
        match self.run_with_outcome(spec, None).await? {
            RunOutcome::Success(r) => Ok(r),
            RunOutcome::Failed(r, _err) => {
                // Drop the report, return just the error.
                Err(EngineError::NodeFailed(
                    r.node_results
                        .values()
                        .filter(|n| n.status == NodeStatus::Failed)
                        .map(|n| n.node_id.clone())
                        .collect::<Vec<_>>()
                        .join(","),
                ))
            }
        }
    }

    /// Like `run`, but returns the full report even on failure. The
    /// `EngineError` is wrapped in `RunOutcome::Failed` alongside the
    /// report so callers can inspect partial state.
    pub async fn run_with_outcome(
        &self,
        mut spec: DagSpec,
        cancel: Option<CancellationToken>,
    ) -> Result<RunOutcome, EngineError> {
        // Validate the spec before doing any work.
        spec.validate().map_err(|e| EngineError::Spec(e.to_string()))?;
        let started_at = Instant::now();
        let started_at_ms = chrono::Utc::now().timestamp_millis();

        let semaphore = Arc::new(Semaphore::new(if self.max_concurrent == 0 {
            // Effectively unbounded. tokio's `Semaphore::new` rejects
            // permits above `usize::MAX >> 3` (its `MAX_PERMITS`,
            // which is `pub(crate)` and not exposed), so we cap at
            // exactly that value. For a DAG engine that means a
            // single swarm can run `2^61 - 1` sub-agents
            // concurrently, which is effectively unbounded.
            usize::MAX >> 3
        } else {
            self.max_concurrent
        }));
        let in_flight: Arc<
            Mutex<HashMap<String, JoinHandle<Result<SubAgentStatus, tokio::task::JoinError>>>>,
        > = Arc::new(Mutex::new(HashMap::new()));

        let mut report = ExecutionReport {
            swarm_id: spec.swarm_id.clone(),
            page_id: spec.page_id.clone(),
            original_task: spec.original_task.clone(),
            started_at_ms,
            finished_at_ms: 0,
            total_execution_ms: 0,
            completed: 0,
            failed: 0,
            cancelled: 0,
            skipped: 0,
            node_results: HashMap::new(),
            final_spec: clone_empty_spec(&spec),
        };

        // Drive the loop. We break when:
        //   1. there's nothing ready AND nothing in flight (done)
        //   2. a node failed (fail-fast — mark downstream skipped, return)
        //   3. cancellation token fires (mark all running as cancelled)
        let mut had_failure = false;
        let mut cancel_message: Option<String> = None;
        'outer: loop {
            // 1. Snapshot ready nodes. Don't hold the queue borrow
            //    while spawning (the queue borrows spec mutably).
            let ready: Vec<String> = {
                let q = ReadyQueue::new(&mut spec);
                q.get_ready_nodes()
            };

            // 2. Spawn them.
            for node_id in &ready {
                // Mark Running so the next iteration's ReadyQueue
                // doesn't re-suggest this node. We re-look-up the
                // node to build the spec.
                let node_idx = spec
                    .nodes
                    .iter()
                    .position(|n| n.id == *node_id)
                    .ok_or_else(|| {
                        EngineError::InvalidSpec(format!("ready node not in spec: {node_id}"))
                    })?;
                let node = spec.nodes[node_idx].clone();
                {
                    let mut q = ReadyQueue::new(&mut spec);
                    q.update_status(node_id, NodeStatus::Running);
                }
                let sub_spec = dag_node_to_spec(&node, &spec.swarm_id, &spec.page_id);
                debug!(
                    node_id = %node.id,
                    role = ?sub_spec.role,
                    "DagExecutionEngine spawning sub-agent"
                );
                // Acquire a permit before spawning so the cap is
                // enforced even if `spawn_with_council` is fast.
                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("semaphore was closed");
                let handle = self
                    .spawner
                    .spawn_with_council(sub_spec)
                    .map_err(|e| EngineError::Spec(format!("spawn failed: {e}")))?;
                // Wrap the handle so we drop the permit when the
                // task completes — but we need the handle for the
                // join set, so we move the permit into the
                // JoinHandle wrapper.
                let wrapped = tokio::spawn(async move {
                    let r = handle.await;
                    drop(permit);
                    r
                });
                in_flight.lock().await.insert(node.id.clone(), wrapped);
            }

            // 3. Wait for at least one in-flight task to finish (or
            //    for cancellation, or for all of them to finish).
            if in_flight.lock().await.is_empty() {
                // Nothing left to do and nothing running.
                break 'outer;
            }
            let resolved_id = wait_for_any_or_cancel(&in_flight, cancel.clone()).await;
            match resolved_id {
                ResolveResult::Node(id) => {
                    let handle = in_flight.lock().await.remove(&id).unwrap();
                    let status_result = handle.await?;
                    let status = match status_result {
                        Ok(s) => s,
                        Err(e) => {
                            error!(node_id = %id, error = %e, "in-flight task join failed");
                            return Err(EngineError::Join(e));
                        }
                    };
                    // Apply the status to the spec.
                    apply_status(&mut spec, &id, &status);
                    // Record in the report.
                    let outcome = status_to_outcome(&id, &status);
                    let terminal = status.state;
                    if matches!(terminal, AgentState::Completed) {
                        report.completed += 1;
                    } else if matches!(terminal, AgentState::Failed) {
                        report.failed += 1;
                        had_failure = true;
                    } else if matches!(terminal, AgentState::Cancelled) {
                        report.cancelled += 1;
                        had_failure = true;
                    }
                    report.node_results.insert(id.clone(), outcome);
                    if had_failure {
                        // Mark downstream pending nodes as Skipped.
                        let skipped = skip_downstream(&mut spec, &id);
                        for sid in skipped {
                            let outcome = NodeOutcome {
                                node_id: sid.clone(),
                                status: NodeStatus::Skipped,
                                execution_ms: 0,
                                model_used: String::new(),
                                tokens_used: 0,
                                output: String::new(),
                                error: Some("skipped due to upstream failure".into()),
                                started_at_ms: chrono::Utc::now().timestamp_millis(),
                                finished_at_ms: chrono::Utc::now().timestamp_millis(),
                            };
                            report.skipped += 1;
                            report.node_results.insert(sid, outcome);
                        }
                        break 'outer;
                    }
                }
                ResolveResult::Cancelled => {
                    cancel_message = Some("cancelled by token".into());
                    // Mark all in-flight as cancelled by interrupting
                    // their tasks (best-effort — we can't cancel a
                    // spawned sub-agent directly; we just stop
                    // awaiting new ones and report).
                    break 'outer;
                }
                ResolveResult::AllDone => break 'outer,
            }
        }

        // Drain remaining in-flight tasks (we may have broken out
        // because of a failure, but other branches could still be
        // running). Don't await them indefinitely; record whatever
        // we can.
        //
        // The wrapped handles have type
        // `JoinHandle<Result<SubAgentStatus, JoinError>>`, so
        // `handle.await` is
        // `Result<Result<SubAgentStatus, JoinError>, JoinError>` and
        // `timeout(d, handle).await` is
        // `Result<Result<Result<SubAgentStatus, JoinError>, JoinError>, Elapsed>`.
        let in_flight_map = in_flight.lock().await.drain().collect::<Vec<_>>();
        for (id, handle) in in_flight_map {
            // Best-effort join — if the task is hung, we abandon it.
            let join_result: Result<
                Result<SubAgentStatus, tokio::task::JoinError>,
                tokio::task::JoinError,
            > = match tokio::time::timeout(
                std::time::Duration::from_millis(2_000),
                handle,
            )
            .await
            {
                Ok(r) => r,
                Err(_) => {
                    warn!(node_id = %id, "in-flight sub-agent join timed out — abandoning");
                    continue;
                }
            };
            let status_result: Result<SubAgentStatus, tokio::task::JoinError> =
                match join_result {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(node_id = %id, error = %e, "in-flight sub-agent join error");
                        continue;
                    }
                };
            let status: SubAgentStatus = match status_result {
                Ok(s) => s,
                Err(e) => {
                    warn!(node_id = %id, error = %e, "in-flight sub-agent returned Err");
                    continue;
                }
            };
            apply_status(&mut spec, &id, &status);
            let outcome = status_to_outcome(&id, &status);
            match status.state {
                AgentState::Completed => report.completed += 1,
                AgentState::Failed => {
                    report.failed += 1;
                    had_failure = true;
                }
                AgentState::Cancelled => {
                    report.cancelled += 1;
                    had_failure = true;
                }
                _ => {}
            }
            report.node_results.insert(id, outcome);
        }

        // Any node still Pending in the spec at this point (e.g.
        // cancellation killed the loop early) gets marked Skipped.
        for n in &mut spec.nodes {
            if n.status == NodeStatus::Pending {
                n.status = NodeStatus::Skipped;
                let outcome = NodeOutcome {
                    node_id: n.id.clone(),
                    status: NodeStatus::Skipped,
                    execution_ms: 0,
                    model_used: String::new(),
                    tokens_used: 0,
                    output: String::new(),
                    error: Some(
                        cancel_message
                            .clone()
                            .unwrap_or_else(|| "skipped (loop exited early)".into()),
                    ),
                    started_at_ms: started_at_ms,
                    finished_at_ms: chrono::Utc::now().timestamp_millis(),
                };
                report.skipped += 1;
                report.node_results.insert(n.id.clone(), outcome);
            }
        }

        let finished_at_ms = chrono::Utc::now().timestamp_millis();
        let total_ms = started_at.elapsed().as_millis() as u64;
        report.finished_at_ms = finished_at_ms;
        report.total_execution_ms = total_ms;
        report.final_spec = spec;

        if had_failure {
            let failed_ids: Vec<String> = report
                .node_results
                .values()
                .filter(|n| n.status == NodeStatus::Failed)
                .map(|n| n.node_id.clone())
                .collect();
            Ok(RunOutcome::Failed(
                report,
                EngineError::NodeFailed(failed_ids.join(",")),
            ))
        } else {
            Ok(RunOutcome::Success(report))
        }
    }
}

/// Outcome enum for `run_with_outcome`. Lets callers distinguish
/// "all green" from "some nodes failed" without losing the report.
#[derive(Debug)]
pub enum RunOutcome {
    Success(ExecutionReport),
    Failed(ExecutionReport, EngineError),
}

impl RunOutcome {
    pub fn report(&self) -> &ExecutionReport {
        match self {
            RunOutcome::Success(r) => r,
            RunOutcome::Failed(r, _) => r,
        }
    }
    pub fn into_report(self) -> ExecutionReport {
        match self {
            RunOutcome::Success(r) => r,
            RunOutcome::Failed(r, _) => r,
        }
    }
    pub fn is_success(&self) -> bool {
        matches!(self, RunOutcome::Success(_))
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

#[derive(Debug)]
enum ResolveResult {
    Node(String),
    Cancelled,
    AllDone,
}

/// Wait for any one in-flight task to finish, or for cancellation,
/// or for the in-flight set to become empty. Polling-based so we
/// don't need a `JoinSet` (we want a stable map id → handle for
/// inspection).
async fn wait_for_any_or_cancel(
    in_flight: &Arc<
        Mutex<HashMap<String, JoinHandle<Result<SubAgentStatus, tokio::task::JoinError>>>>,
    >,
    cancel: Option<CancellationToken>,
) -> ResolveResult {
    loop {
        // Snapshot the keys under the lock.
        {
            let g = in_flight.lock().await;
            if g.is_empty() {
                return ResolveResult::AllDone;
            }
        }
        if let Some(ref tok) = cancel {
            if tok.is_cancelled() {
                return ResolveResult::Cancelled;
            }
        }
        // Race a short sleep against the cancel token. When the
        // sleep finishes, scan the map for handles that have
        // completed. (We can't `await` a `JoinHandle` without
        // taking it out of the map; we use `is_finished()` as a
        // cheap poll.)
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                let g = in_flight.lock().await;
                let mut done: Option<String> = None;
                for (id, h) in g.iter() {
                    if h.is_finished() {
                        done = Some(id.clone());
                        break;
                    }
                }
                drop(g);
                if let Some(id) = done {
                    return ResolveResult::Node(id);
                }
                // Nothing ready; loop and re-check.
                continue;
            }
            _ = async {
                if let Some(tok) = cancel.as_ref() {
                    tok.cancelled().await;
                } else {
                    // No token — sleep forever (will never resolve).
                    std::future::pending::<()>().await;
                }
            } => {
                return ResolveResult::Cancelled;
            }
        }
    }
}

/// Apply a finished `SubAgentStatus` to the corresponding `DagNode`
/// in the spec — updates status, populates `result`.
fn apply_status(spec: &mut DagSpec, node_id: &str, status: &SubAgentStatus) {
    let n = match spec.nodes.iter_mut().find(|n| n.id == node_id) {
        Some(n) => n,
        None => {
            error!(node_id = %node_id, "apply_status: node not found in spec");
            return;
        }
    };
    n.status = match status.state {
        AgentState::Completed => NodeStatus::Completed,
        AgentState::Failed => NodeStatus::Failed,
        // `NodeStatus` has no `Cancelled` variant; cancellation
        // surfaces as `Skipped` in the planner's view (and is
        // additionally recorded in `cancelled` count on the report).
        AgentState::Cancelled => NodeStatus::Skipped,
        // Pending/Running shouldn't appear at this point, but be
        // defensive.
        AgentState::Pending => NodeStatus::Pending,
        AgentState::Running => NodeStatus::Running,
    };
    n.result = Some(NodeResult {
        content: status.output.clone(),
        model_used: status.model_used.clone(),
        tokens_used: status.tokens_used,
        execution_ms: status.elapsed_ms,
    });
}

/// Build a `NodeOutcome` snapshot from a `SubAgentStatus`.
fn status_to_outcome(node_id: &str, status: &SubAgentStatus) -> NodeOutcome {
    let ns = match status.state {
        AgentState::Completed => NodeStatus::Completed,
        AgentState::Failed => NodeStatus::Failed,
        // `NodeStatus` doesn't have a `Cancelled` variant; cancellation
        // surfaces as `Skipped` in the planner's view (and is
        // additionally recorded in `cancelled` count on the report).
        AgentState::Cancelled => NodeStatus::Skipped,
        AgentState::Pending => NodeStatus::Pending,
        AgentState::Running => NodeStatus::Running,
    };
    let now = chrono::Utc::now().timestamp_millis();
    NodeOutcome {
        node_id: node_id.to_string(),
        status: ns,
        execution_ms: status.elapsed_ms,
        model_used: status.model_used.clone(),
        tokens_used: status.tokens_used,
        output: status.output.clone(),
        error: status.error.clone(),
        started_at_ms: now - status.elapsed_ms as i64,
        finished_at_ms: now,
    }
}

/// Mark every Pending descendant of `failed_id` as Skipped, recursively.
/// Returns the list of node ids that were skipped.
///
/// Algorithm: a Pending node is skipped if it has at least one
/// parent that is **not** in the "can no longer contribute" set
/// (i.e. a parent that is `Failed`, `Running`, or `Pending` —
/// basically anything that won't be `Completed`). We iterate
/// until a fixed point so that a chain like A→B→C gets all
/// three marked when A is Failed.
fn skip_downstream(spec: &mut DagSpec, _failed_id: &str) -> Vec<String> {
    let mut skipped = Vec::new();
    loop {
        // Build a snapshot of parent statuses to avoid holding a
        // mutable borrow of `spec.nodes` while reading it again
        // inside the per-node check.
        let parent_statuses: Vec<(String, NodeStatus)> = spec
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.status.clone()))
            .collect();
        let mut changed = false;
        for n in spec.nodes.iter_mut() {
            if n.status != NodeStatus::Pending {
                continue;
            }
            // If this node has any parent that is not Completed and
            // not Skipped, then it can no longer be satisfied.
            let parent_blocked = spec.edges.iter().filter(|e| e.to == n.id).any(|e| {
                parent_statuses
                    .iter()
                    .find(|(pid, _)| *pid == e.from)
                    .map(|(_, ps)| !matches!(ps, NodeStatus::Completed | NodeStatus::Skipped))
                    .unwrap_or(true)
            });
            if parent_blocked {
                n.status = NodeStatus::Skipped;
                skipped.push(n.id.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    skipped
}

fn clone_empty_spec(spec: &DagSpec) -> DagSpec {
    DagSpec {
        swarm_id: spec.swarm_id.clone(),
        page_id: spec.page_id.clone(),
        original_task: spec.original_task.clone(),
        nodes: spec
            .nodes
            .iter()
            .map(|n| DagNode {
                status: NodeStatus::Pending,
                result: None,
                ..n.clone()
            })
            .collect(),
        edges: spec.edges.clone(),
        created_at: spec.created_at,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::DagEdge;

    fn node(id: &str, task: TaskType) -> DagNode {
        DagNode {
            id: id.to_string(),
            name: id.to_string(),
            description: format!("Task for {id}"),
            task_type: task,
            allowed_tools: vec!["echo".to_string()],
            model_hint: None,
            token_budget: 1_000,
            timeout_ms: 5_000,
            retry_count: 0,
            max_retries: 0,
            status: NodeStatus::Pending,
            result: None,
        }
    }

    #[test]
    fn task_type_maps_to_role() {
        assert_eq!(task_type_to_role(TaskType::CodeGeneration), SubAgentRole::Build);
        assert_eq!(task_type_to_role(TaskType::Research), SubAgentRole::Explore);
        assert_eq!(task_type_to_role(TaskType::Review), SubAgentRole::Review);
        assert_eq!(task_type_to_role(TaskType::General), SubAgentRole::General);
    }

    #[test]
    fn dag_node_to_spec_propagates_swarm_and_page() {
        let n = node("X", TaskType::Research);
        let s = dag_node_to_spec(&n, "swarm-1", "page-1");
        assert_eq!(s.id, "X");
        assert_eq!(s.swarm_id, "swarm-1");
        assert_eq!(s.parent_page_id, "page-1");
        assert_eq!(s.role, SubAgentRole::Explore);
    }

    #[test]
    fn skip_downstream_marks_descendants() {
        let mut spec = DagSpec {
            swarm_id: "s".into(),
            page_id: "p".into(),
            original_task: "t".into(),
            nodes: vec![
                node("A", TaskType::General),
                node("B", TaskType::General),
                node("C", TaskType::General),
            ],
            edges: vec![
                DagEdge {
                    from: "A".into(),
                    to: "B".into(),
                    label: None,
                },
                DagEdge {
                    from: "B".into(),
                    to: "C".into(),
                    label: None,
                },
            ],
            created_at: 0,
        };
        // Mark A as Failed, leave B/C as Pending, and verify that the
        // skip-downstream pass marks B and C as Skipped.
        spec.nodes[0].status = NodeStatus::Failed;
        let skipped = skip_downstream(&mut spec, "A");
        assert!(
            skipped.contains(&"B".to_string()),
            "B should be skipped when A failed: {skipped:?}"
        );
        assert!(
            skipped.contains(&"C".to_string()),
            "C should be skipped when A failed: {skipped:?}"
        );
    }
}
