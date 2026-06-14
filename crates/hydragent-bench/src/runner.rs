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
}
