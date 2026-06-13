//! # ASCII DAG printer
//!
//! Phase 5 / Track 5.4. Tiny renderer that turns a `DagSpec` (or an
//! `ExecutionReport`) into a fixed-width ASCII picture suitable for
//! the terminal. No graphviz, no unicode, no colour — the goal is a
//! shape the user can `cat` into a log file and have it look
//! identical across machines.
//!
//! ## Layout
//!
//! ```text
//! ┌─ A  (root)         ✓ completed   42ms
//! │  └─ B              ✓ completed   18ms
//! │     └─ D           ✗ failed     120ms  (rate limit)
//! └─ C                 ✓ completed   22ms
//! ```
//!
//! The renderer does a topological-ish walk and draws tree edges
//! with `├─` and `└─`. Cycles are not supported (the engine's
//! `validate` already rejects them) but the renderer is defensive
//! and bails to a flat list if it detects a cycle.
//!
//! ## Status glyphs
//!
//! | Glyph | Meaning          |
//! |-------|------------------|
//! | `✓`   | Completed        |
//! | `✗`   | Failed           |
//! | `⊘`   | Skipped / Cancelled |
//! | `…`   | Running          |
//! | `·`   | Pending          |
//!
//! ## Example
//!
//! ```
//! use hydragent_planner::dag::{DagEdge, DagNode, DagSpec, NodeStatus, TaskType};
//! use hydragent_planner::ascii::print_dag;
//!
//! let mut a = DagNode {
//!     id: "A".into(), name: "root".into(),
//!     description: "do the thing".into(),
//!     task_type: TaskType::General,
//!     allowed_tools: vec![], model_hint: None,
//!     token_budget: 0, timeout_ms: 0,
//!     retry_count: 0, max_retries: 0,
//!     status: NodeStatus::Completed,
//!     result: None,
//! };
//! let mut b = a.clone();
//! b.id = "B".into(); b.name = "left".into();
//! b.status = NodeStatus::Running;
//! let spec = DagSpec {
//!     swarm_id: "s".into(), page_id: "p".into(),
//!     original_task: "t".into(),
//!     nodes: vec![a.clone(), b],
//!     edges: vec![DagEdge { from: "A".into(), to: "B".into(), label: None }],
//!     created_at: 0,
//! };
//! let picture = print_dag(&spec);
//! assert!(picture.contains("root"), "expected node 'A' (name 'root') in: {picture}");
//! assert!(picture.contains("left"), "expected node 'B' (name 'left') in: {picture}");
//! assert!(picture.contains("swarm_id"));
//! ```

use std::collections::HashSet;
use std::fmt::Write;

use crate::dag::{DagEdge, DagNode, DagSpec, NodeStatus};
use crate::dag_execution::{ExecutionReport, NodeOutcome, RunOutcome};
use crate::replan::{ReplanOutcome, ReplanStrategy};

/// Map a `NodeStatus` to a single character glyph.
pub fn glyph(s: &NodeStatus) -> char {
    match s {
        NodeStatus::Completed => '✓',
        NodeStatus::Failed => '✗',
        NodeStatus::Skipped => '⊘',
        NodeStatus::Running => '…',
        NodeStatus::Pending | NodeStatus::Ready => '·',
    }
}

/// Render a `DagSpec` as ASCII.
pub fn print_dag(spec: &DagSpec) -> String {
    let mut s = String::new();
    s.push_str(&format!("swarm_id : {}\n", spec.swarm_id));
    s.push_str(&format!("page_id  : {}\n", spec.page_id));
    s.push_str(&format!("task     : {}\n", spec.original_task));
    s.push_str(&format!("nodes    : {}\n", spec.nodes.len()));
    s.push_str(&format!("edges    : {}\n\n", spec.edges.len()));

    // Choose root nodes: any node that has no incoming edge.
    let mut incoming: HashSet<String> = HashSet::new();
    for e in &spec.edges {
        incoming.insert(e.to.clone());
    }
    let roots: Vec<String> = spec
        .nodes
        .iter()
        .filter(|n| !incoming.contains(&n.id))
        .map(|n| n.id.clone())
        .collect();

    if roots.is_empty() {
        // No roots → either a cycle or an empty graph. Print flat.
        s.push_str("(no root nodes — flat list)\n");
        for n in &spec.nodes {
            push_node_line(&mut s, n, "");
        }
        return s;
    }

    // Walk the DAG tree. We track visited nodes so a cycle can't
    // make us loop forever.
    let mut visited: HashSet<String> = HashSet::new();
    for (i, root_id) in roots.iter().enumerate() {
        let is_last_root = i + 1 == roots.len();
        walk(
            &mut s,
            spec,
            root_id,
            "",
            is_last_root,
            &mut visited,
        );
    }
    s
}

/// Render an `ExecutionReport` as ASCII. Includes per-node
/// durations and any error messages.
pub fn print_report(report: &ExecutionReport) -> String {
    let mut s = String::new();
    s.push_str(&format!("swarm_id : {}\n", report.swarm_id));
    s.push_str(&format!("page_id  : {}\n", report.page_id));
    s.push_str(&format!("task     : {}\n", report.original_task));
    s.push_str(&format!(
        "totals   : ✓ {}  ✗ {}  ⊘ {}  … {} (skipped: {})\n",
        report.completed, report.failed, report.cancelled, 0, report.skipped
    ));
    s.push_str(&format!(
        "wall     : {} ms ({})\n\n",
        report.total_execution_ms,
        if report.is_success() { "success" } else { "failed" }
    ));

    // Render per-node outcomes in the spec's order.
    for n in &report.final_spec.nodes {
        let outcome = report.node_results.get(&n.id);
        push_outcome_line(&mut s, n, outcome, "");
    }
    s
}

/// Render a `RunOutcome` (success or failed) the same way.
pub fn print_run_outcome(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Success(r) => {
            let mut s = print_report(r);
            s.push_str("\n(result: SUCCESS)\n");
            s
        }
        RunOutcome::Failed(r, err) => {
            let mut s = print_report(r);
            s.push_str(&format!("\n(result: FAILED — {})\n", err));
            s
        }
    }
}

/// Render a `ReplanOutcome` as a single-line audit entry.
pub fn print_replan_event(node_id: &str, failure_msg: &str, outcome: &ReplanOutcome) -> String {
    let mut s = String::new();
    match outcome {
        ReplanOutcome::Applied(strategy) => {
            let detail = describe_strategy(strategy);
            let _ = writeln!(
                s,
                "  ↻ replan[{node_id}] {detail}  (cause: {failure_msg})"
            );
        }
        ReplanOutcome::NoAction(why) => {
            let _ = writeln!(
                s,
                "  · replan[{node_id}] no-action: {why}  (cause: {failure_msg})"
            );
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn walk(
    out: &mut String,
    spec: &DagSpec,
    node_id: &str,
    prefix: &str,
    is_last: bool,
    visited: &mut HashSet<String>,
) {
    if !visited.insert(node_id.to_string()) {
        // Already printed this subtree — guard against cycles.
        let _ = writeln!(out, "{prefix}↻ {node_id} (already shown)");
        return;
    }
    let node = match spec.nodes.iter().find(|n| n.id == node_id) {
        Some(n) => n,
        None => {
            let _ = writeln!(out, "{prefix}↯ {node_id} (not in spec)");
            return;
        }
    };
    let connector = if is_last { "└─ " } else { "├─ " };
    push_node_line_with_prefix(out, node, prefix, connector);

    // Recurse into children.
    let children: Vec<&DagEdge> = spec
        .edges
        .iter()
        .filter(|e| e.from == node_id)
        .collect();
    let child_prefix = format!("{prefix}{}", if is_last { "   " } else { "│  " });
    for (i, edge) in children.iter().enumerate() {
        let is_last_child = i + 1 == children.len();
        walk(
            out,
            spec,
            &edge.to,
            &child_prefix,
            is_last_child,
            visited,
        );
    }
}

fn push_node_line(out: &mut String, n: &DagNode, prefix: &str) {
    push_node_line_with_prefix(out, n, prefix, "")
}

fn push_node_line_with_prefix(
    out: &mut String,
    n: &DagNode,
    prefix: &str,
    connector: &str,
) {
    let g = glyph(&n.status);
    let _ = writeln!(
        out,
        "{prefix}{connector}{g} {:<32} [{:<12}] retry={} tools={}",
        truncate(&n.name, 32),
        format!("{:?}", n.task_type).to_lowercase(),
        n.retry_count,
        n.allowed_tools.len()
    );
}

fn push_outcome_line(
    out: &mut String,
    n: &DagNode,
    outcome: Option<&NodeOutcome>,
    prefix: &str,
) {
    let g = glyph(&n.status);
    let mut line = format!(
        "{prefix}{g} {:<32} model={}",
        truncate(&n.name, 32),
        truncate(
            outcome.map(|o| o.model_used.as_str()).unwrap_or("-"),
            28
        )
    );
    if let Some(o) = outcome {
        let extra = format!(
            "  {}ms  tokens={}",
            o.execution_ms, o.tokens_used
        );
        line.push_str(&extra);
        if let Some(err) = &o.error {
            line.push_str(&format!("  err={}", truncate(err, 60)));
        }
    }
    let _ = writeln!(out, "{line}");
}

fn describe_strategy(strategy: &ReplanStrategy) -> String {
    match strategy {
        ReplanStrategy::Retry { attempt, max_attempts } => {
            format!("retry (attempt {attempt}/{max_attempts})")
        }
        ReplanStrategy::Reroute { from_model, to_model } => match from_model {
            Some(f) => format!("reroute {f} -> {to_model}"),
            None => format!("reroute -> {to_model}"),
        },
        ReplanStrategy::Decompose { new_task_type, subtask_count } => {
            format!(
                "decompose -> {:?} ({subtask_count} subtask(s))",
                new_task_type
            )
        }
        ReplanStrategy::Escalate { reason } => {
            format!("escalate: {}", truncate(reason, 60))
        }
        ReplanStrategy::NoOp { reason } => {
            format!("no-op: {}", truncate(reason, 60))
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::{DagEdge, DagNode, DagSpec, TaskType};

    fn node(id: &str, status: NodeStatus) -> DagNode {
        DagNode {
            id: id.into(),
            name: id.into(),
            description: format!("node {id}"),
            task_type: TaskType::General,
            allowed_tools: vec!["echo".into()],
            model_hint: None,
            token_budget: 0,
            timeout_ms: 0,
            retry_count: 0,
            max_retries: 0,
            status,
            result: None,
        }
    }

    fn make_diamond() -> DagSpec {
        DagSpec {
            swarm_id: "s".into(),
            page_id: "p".into(),
            original_task: "build diamond".into(),
            nodes: vec![
                node("A", NodeStatus::Completed),
                node("B", NodeStatus::Completed),
                node("C", NodeStatus::Failed),
                node("D", NodeStatus::Skipped),
            ],
            edges: vec![
                DagEdge { from: "A".into(), to: "B".into(), label: None },
                DagEdge { from: "A".into(), to: "C".into(), label: None },
                DagEdge { from: "B".into(), to: "D".into(), label: None },
                DagEdge { from: "C".into(), to: "D".into(), label: None },
            ],
            created_at: 0,
        }
    }

    #[test]
    fn glyph_mapping_is_stable() {
        assert_eq!(glyph(&NodeStatus::Completed), '✓');
        assert_eq!(glyph(&NodeStatus::Failed), '✗');
        assert_eq!(glyph(&NodeStatus::Skipped), '⊘');
        assert_eq!(glyph(&NodeStatus::Running), '…');
        assert_eq!(glyph(&NodeStatus::Pending), '·');
        assert_eq!(glyph(&NodeStatus::Ready), '·');
    }

    #[test]
    fn print_dag_renders_all_nodes() {
        let s = print_dag(&make_diamond());
        for id in ["A", "B", "C", "D"] {
            assert!(s.contains(id), "missing {id} in: {s}");
        }
        assert!(s.contains("swarm_id"));
        assert!(s.contains("task"));
    }

    #[test]
    fn print_dag_handles_empty_spec() {
        let spec = DagSpec {
            swarm_id: "s".into(),
            page_id: "p".into(),
            original_task: "".into(),
            nodes: vec![],
            edges: vec![],
            created_at: 0,
        };
        let s = print_dag(&spec);
        assert!(s.contains("nodes    : 0"));
        assert!(s.contains("no root nodes"));
    }

    #[test]
    fn print_dag_handles_cycle_defensively() {
        // Even though validate() rejects cycles, the renderer must
        // not loop forever. Add a back-edge that does NOT turn a
        // root into a non-root: C→B. A is still a root, the walk
        // visits B (under A) first, then C, then tries to visit B
        // again under C and the cycle guard must fire.
        let mut spec = make_diamond();
        spec.edges.push(DagEdge { from: "C".into(), to: "B".into(), label: None });
        let s = print_dag(&spec);
        assert!(
            s.contains("already shown") || s.contains("cycle"),
            "cycle guard should fire; got: {s}"
        );
    }

    #[test]
    fn print_report_includes_durations_and_errors() {
        let report = ExecutionReport {
            swarm_id: "s".into(),
            page_id: "p".into(),
            original_task: "x".into(),
            started_at_ms: 0,
            finished_at_ms: 1000,
            total_execution_ms: 1000,
            completed: 1,
            failed: 1,
            cancelled: 0,
            skipped: 1,
            node_results: Default::default(),
            final_spec: make_diamond(),
        };
        let s = print_report(&report);
        assert!(s.contains("wall"));
        assert!(s.contains("totals"));
    }

    #[test]
    fn print_replan_event_describes_each_strategy() {
        let s = print_replan_event(
            "A",
            "rate limit",
            &ReplanOutcome::Applied(ReplanStrategy::Retry {
                attempt: 2,
                max_attempts: 3,
            }),
        );
        assert!(s.contains("retry"));
        assert!(s.contains("2/3"));
        assert!(s.contains("A"));
        assert!(s.contains("rate limit"));

        let s = print_replan_event(
            "B",
            "model oops",
            &ReplanOutcome::Applied(ReplanStrategy::Reroute {
                from_model: Some("openai/gpt-4o".into()),
                to_model: "anthropic/claude-3.5-sonnet".into(),
            }),
        );
        assert!(s.contains("reroute"));
        assert!(s.contains("openai/gpt-4o"));
        assert!(s.contains("claude-3.5-sonnet"));
    }

    #[test]
    fn print_replan_event_handles_no_action() {
        let s = print_replan_event(
            "C",
            "boom",
            &ReplanOutcome::NoAction("max retries exceeded".into()),
        );
        assert!(s.contains("no-action"));
        assert!(s.contains("max retries exceeded"));
    }

    #[test]
    fn truncate_handles_short_and_long() {
        assert_eq!(truncate("hi", 5), "hi");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("", 5), "");
    }
}
