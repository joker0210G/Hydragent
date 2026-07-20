//! The benchmark runner.
//!
//! The runner is decoupled from any concrete retrieval backend. It
//! takes a closure / function `retrieve: Fn(&str) -> Vec<String>` and
//! runs it over each item in a benchmark suite, accumulating metrics.
//!
//! This is the *pure* runner: no async, no IO. The CLI binary in
//! `bin/bench.rs` wires the runner to a real [`SkillLibrary`]
//! retrieval implementation.

use crate::dataset::{GoldenSetItem, SkillBenchTask};
use crate::metrics::{mean, recall_at_k, reciprocal_rank, Prf};
use serde::{Deserialize, Serialize};

/// A retrieval function: given a query, return ranked skill ids.
/// The first element is the top-1 prediction; ordering matters.
pub type Retriever = Box<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// Aggregate scores for SKILL-BENCH (single-relevance).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillBenchScores {
    pub recall_at_1: f64,
    pub recall_at_3: f64,
    pub recall_at_5: f64,
    pub mrr: f64,
    pub n: usize,
}

/// Aggregate scores for the golden set (multi-relevance).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoldenScores {
    pub mean_precision: f64,
    pub mean_recall: f64,
    pub mean_f1: f64,
    pub n: usize,
}

impl SkillBenchScores {
    pub fn compute(tasks: &[SkillBenchTask], retrieve: &Retriever) -> Self {
        if tasks.is_empty() {
            return Self::default();
        }
        let mut r1 = Vec::new();
        let mut r3 = Vec::new();
        let mut r5 = Vec::new();
        let mut mrr = Vec::new();
        for t in tasks {
            let hits = retrieve(&t.query);
            r1.push(recall_at_k(&t.expected_skill, &hits, 1));
            r3.push(recall_at_k(&t.expected_skill, &hits, 3));
            r5.push(recall_at_k(&t.expected_skill, &hits, 5));
            mrr.push(reciprocal_rank(&t.expected_skill, &hits));
        }
        Self {
            recall_at_1: mean(&r1),
            recall_at_3: mean(&r3),
            recall_at_5: mean(&r5),
            mrr: mean(&mrr),
            n: tasks.len(),
        }
    }
}

impl GoldenScores {
    pub fn compute(items: &[GoldenSetItem], retrieve: &Retriever) -> Self {
        if items.is_empty() {
            return Self::default();
        }
        let mut precs = Vec::new();
        let mut recs = Vec::new();
        let mut f1s = Vec::new();
        for it in items {
            let hits = retrieve(&it.query);
            let p = Prf::compute(&it.relevant, &hits);
            precs.push(p.precision);
            recs.push(p.recall);
            f1s.push(p.f1);
        }
        Self {
            mean_precision: mean(&precs),
            mean_recall: mean(&recs),
            mean_f1: mean(&f1s),
            n: items.len(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Task-completion benchmark (end-to-end)
// ─────────────────────────────────────────────────────────────────────
//
// Unlike the two retrieval benchmarks above, this one drives the
// **full ReAct loop** and grades whether the agent actually *completed*
// the user's task. The runner is still decoupled from the agent — it
// takes a `TaskExecutor` closure so unit tests can use a deterministic
// fake; the CLI binary in `bin/task_bench.rs` wires it to
// `hydragent_core::react_loop::run_react_loop`.

use crate::dataset::TaskBenchItem;

/// What the agent produced for one task. Cheap, owned, JSON-safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutcome {
    /// Final markdown/text answer from the agent.
    pub answer: String,
    /// Concatenated text of every tool's `output_json` field (and any
    /// `error_message`). Used by the grader so a tool's output can
    /// satisfy a `must_contain` keyword even if the agent didn't repeat
    /// it in the prose answer.
    pub tool_outputs: String,
    /// Names of tools invoked, in invocation order.
    pub tools_invoked: Vec<String>,
    /// Wall-clock duration of the run in milliseconds.
    pub duration_ms: u64,
}

/// Per-task graded result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub id: String,
    pub difficulty: String,
    pub passed: bool,
    /// Human-readable reasons the task failed (empty on pass).
    pub failures: Vec<String>,
    pub outcome: TaskOutcome,
}

/// Aggregate scores for the task-completion suite.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskBenchScores {
    pub passed: usize,
    pub total: usize,
    pub pass_rate: f64,
    /// Pass rate bucketed by difficulty (so we can see if "hard"
    /// regresses without dragging the overall number down).
    pub by_difficulty: std::collections::BTreeMap<String, DifficultyBucket>,
    pub n: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DifficultyBucket {
    pub passed: usize,
    pub total: usize,
    pub pass_rate: f64,
}

/// An executor plugs the runner into the live agent loop. It receives
/// the task prompt and returns whatever the agent produced.
///
/// Note: the future is only required to be `Send` (so it can be awaited
/// across an `.await` point), not `Sync`. The live ReAct-loop future is
/// not `Sync` (it captures a `dyn Future` from the tool trait), so we
/// deliberately omit the `Sync` bound here.
pub type TaskExecutor =
    Box<dyn Fn(&TaskBenchItem) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskOutcome> + Send>> + Send + Sync>;

/// Grade one outcome against one task's pass/fail rule.
pub fn grade(task: &TaskBenchItem, outcome: &TaskOutcome) -> Vec<String> {
    let mut failures = Vec::new();
    // Combine answer + tool outputs into one big haystack for keyword
    // matching. Lower-cased once, in advance.
    let answer_lc = outcome.answer.to_lowercase();
    let tools_lc = outcome.tool_outputs.to_lowercase();
    let haystack = format!("{}\n{}", answer_lc, tools_lc);

    for needle in &task.must_contain {
        if !haystack.contains(&needle.to_lowercase()) {
            failures.push(format!("missing required keyword {:?}", needle));
        }
    }
    for needle in &task.must_not_contain {
        if haystack.contains(&needle.to_lowercase()) {
            failures.push(format!("forbidden keyword present: {:?}", needle));
        }
    }
    for tool in &task.required_tools {
        if !outcome.tools_invoked.iter().any(|t| t == tool) {
            failures.push(format!("required tool not called: {}", tool));
        }
    }
    failures
}

/// Run the full suite. Returns per-task results and aggregate scores.
pub async fn run_task_bench(
    tasks: &[TaskBenchItem],
    executor: &TaskExecutor,
) -> (Vec<TaskResult>, TaskBenchScores) {
    let mut results = Vec::with_capacity(tasks.len());
    let mut by_difficulty: std::collections::BTreeMap<String, DifficultyBucket> =
        std::collections::BTreeMap::new();
    let mut passed = 0usize;

    for task in tasks {
        eprintln!(
            "[task_bench] running {} (difficulty={}) — {}",
            task.id, task.difficulty, task.prompt
        );
        let outcome = executor(task).await;
        let failures = grade(task, &outcome);
        let ok = failures.is_empty();
        if ok {
            passed += 1;
        }
        let bucket = by_difficulty
            .entry(task.difficulty.clone())
            .or_default();
        bucket.total += 1;
        if ok {
            bucket.passed += 1;
        }

        results.push(TaskResult {
            id: task.id.clone(),
            difficulty: task.difficulty.clone(),
            passed: ok,
            failures,
            outcome,
        });
    }

    let total = tasks.len();
    for bucket in by_difficulty.values_mut() {
        bucket.pass_rate = if bucket.total > 0 {
            bucket.passed as f64 / bucket.total as f64
        } else {
            0.0
        };
    }
    let scores = TaskBenchScores {
        passed,
        total,
        pass_rate: if total > 0 { passed as f64 / total as f64 } else { 0.0 },
        by_difficulty,
        n: total,
    };

    (results, scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_bench(id: &str, expected: &str) -> SkillBenchTask {
        SkillBenchTask {
            id: id.into(),
            query: format!("query for {expected}"),
            expected_skill: expected.into(),
            expected_tags: vec![],
            difficulty: "easy".into(),
            category: "code".into(),
        }
    }

    fn golden(id: &str, relevant: Vec<&str>) -> GoldenSetItem {
        GoldenSetItem {
            id: id.into(),
            query: format!("query for {relevant:?}"),
            relevant: relevant.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn perfect_retriever_scores_one() {
        let tasks = vec![skill_bench("SB1", "a"), skill_bench("SB2", "b")];
        let r: Retriever = Box::new(|q| {
            if q.contains("a") { vec!["a".into()] } else { vec!["b".into()] }
        });
        let s = SkillBenchScores::compute(&tasks, &r);
        assert_eq!(s.recall_at_1, 1.0);
        assert_eq!(s.mrr, 1.0);
        assert_eq!(s.n, 2);
    }

    #[test]
    fn wrong_retriever_scores_zero() {
        let tasks = vec![skill_bench("SB1", "a")];
        let r: Retriever = Box::new(|_| vec!["x".into()]);
        let s = SkillBenchScores::compute(&tasks, &r);
        assert_eq!(s.recall_at_1, 0.0);
        assert_eq!(s.mrr, 0.0);
    }

    #[test]
    fn second_position_mrr_is_half() {
        let tasks = vec![skill_bench("SB1", "a")];
        let r: Retriever = Box::new(|_| vec!["x".into(), "a".into()]);
        let s = SkillBenchScores::compute(&tasks, &r);
        assert_eq!(s.recall_at_1, 0.0);
        assert_eq!(s.recall_at_3, 1.0);
        assert!((s.mrr - 0.5).abs() < 1e-9);
    }

    #[test]
    fn empty_tasks_default_scores() {
        let r: Retriever = Box::new(|_| Vec::<String>::new());
        let s = SkillBenchScores::compute(&[], &r);
        assert_eq!(s.n, 0);
        assert_eq!(s.mrr, 0.0);
    }

    #[test]
    fn golden_perfect() {
        let items = vec![golden("GS1", vec!["a"])];
        let r: Retriever = Box::new(|_| vec!["a".into()]);
        let s = GoldenScores::compute(&items, &r);
        assert!((s.mean_precision - 1.0).abs() < 1e-9);
        assert!((s.mean_recall - 1.0).abs() < 1e-9);
        assert!((s.mean_f1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn golden_no_hits() {
        let items = vec![golden("GS1", vec!["a", "b"])];
        let r: Retriever = Box::new(|_| vec!["c".into(), "d".into()]);
        let s = GoldenScores::compute(&items, &r);
        assert_eq!(s.mean_precision, 0.0);
        assert_eq!(s.mean_recall, 0.0);
        assert_eq!(s.mean_f1, 0.0);
    }

    // ─── Task-completion benchmark tests ──────────────────────────────

    fn task(id: &str, must_contain: Vec<&str>, must_not: Vec<&str>, tools: Vec<&str>) -> TaskBenchItem {
        TaskBenchItem {
            id: id.into(),
            difficulty: "easy".into(),
            prompt: format!("prompt for {id}"),
            must_contain: must_contain.into_iter().map(String::from).collect(),
            must_not_contain: must_not.into_iter().map(String::from).collect(),
            required_tools: tools.into_iter().map(String::from).collect(),
            note: String::new(),
        }
    }

    fn outcome(answer: &str, tool_outputs: &str, tools: &[&str]) -> TaskOutcome {
        TaskOutcome {
            answer: answer.into(),
            tool_outputs: tool_outputs.into(),
            tools_invoked: tools.iter().map(|s| s.to_string()).collect(),
            duration_ms: 100,
        }
    }

    #[test]
    fn grade_passes_when_all_rules_met() {
        let t = task("TB1", vec!["391"], vec!["refuse"], vec!["code_execute"]);
        let o = outcome("391", "", &["code_execute"]);
        assert!(grade(&t, &o).is_empty());
    }

    #[test]
    fn grade_flags_missing_keyword() {
        let t = task("TB1", vec!["391"], vec![], vec![]);
        let o = outcome("not the number", "", &[]);
        let f = grade(&t, &o);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("391"));
    }

    #[test]
    fn grade_flags_forbidden_keyword_anywhere() {
        let t = task("TB1", vec![], vec!["I cannot"], vec![]);
        // The forbidden keyword should be matched even if it only
        // appears in tool output, not the prose answer.
        let o = outcome("all good", "tool replied: I cannot help with that", &[]);
        let f = grade(&t, &o);
        assert!(!f.is_empty());
    }

    #[test]
    fn grade_flags_missing_required_tool() {
        let t = task("TB1", vec![], vec![], vec!["web_search"]);
        let o = outcome("hi", "", &["echo"]);
        let f = grade(&t, &o);
        assert!(f.iter().any(|s| s.contains("web_search")));
    }

    #[test]
    fn grade_keyword_match_is_case_insensitive() {
        let t = task("TB1", vec!["HYDRA-7"], vec![], vec![]);
        let o = outcome("hydra-7 is the codename", "", &[]);
        assert!(grade(&t, &o).is_empty());
    }

    #[tokio::test]
    async fn run_task_bench_aggregates_pass_rate_and_buckets() {
        let tasks = vec![
            task("TB1", vec!["391"], vec![], vec![]),
            task("TB2", vec!["999"], vec![], vec![]), // will fail
            {
                let mut t = task("TB3", vec!["ok"], vec![], vec![]);
                t.difficulty = "hard".into();
                t
            },
        ];
        // Fake executor: succeeds iff id ends in "1" or "3".
        let executor: TaskExecutor = Box::new(|t| {
            let id = t.id.clone();
            Box::pin(async move {
                if id.ends_with("1") || id.ends_with("3") {
                    outcome("391 (or ok)", "", &[])
                } else {
                    outcome("nope", "", &[])
                }
            })
        });
        let (results, scores) = run_task_bench(&tasks, &executor).await;
        assert_eq!(results.len(), 3);
        assert_eq!(scores.passed, 2);
        assert_eq!(scores.total, 3);
        assert!((scores.pass_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!(results[0].passed);
        assert!(!results[1].passed);
        assert!(results[2].passed);
        // Buckets: 1 easy pass, 1 easy fail, 1 hard pass.
        let easy = scores.by_difficulty.get("easy").unwrap();
        assert_eq!(easy.total, 2);
        assert_eq!(easy.passed, 1);
        assert!((easy.pass_rate - 0.5).abs() < 1e-9);
        let hard = scores.by_difficulty.get("hard").unwrap();
        assert_eq!(hard.total, 1);
        assert_eq!(hard.passed, 1);
    }
}
