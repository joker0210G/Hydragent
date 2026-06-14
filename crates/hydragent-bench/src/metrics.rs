//! Retrieval-quality metrics.
//!
//! We implement four classical IR metrics that are easy to reason
//! about and cheap to compute:
//!
//! * **Recall@K** — fraction of relevant docs that appear in the top-K
//!   retrieved. Used by SKILL-BENCH (single-relevance).
//! * **MRR** (Mean Reciprocal Rank) — 1 / (rank of first relevant doc).
//!   Used by SKILL-BENCH.
//! * **Precision** — |relevant ∩ retrieved| / |retrieved|.
//! * **Recall** — |relevant ∩ retrieved| / |relevant|.
//! * **F1** — harmonic mean of precision and recall. Used by golden set.

/// Recall@K: was the *single* relevant doc in the top K retrieved?
///
/// Returns 1.0 if `relevant` is in the first K of `retrieved`, else 0.0.
/// `K` is capped at `retrieved.len()`.
pub fn recall_at_k(relevant: &str, retrieved: &[String], k: usize) -> f64 {
    if retrieved.is_empty() {
        return 0.0;
    }
    let mut top_k = retrieved.iter().take(k);
    if top_k.any(|r| r.as_str() == relevant) { 1.0 } else { 0.0 }
}

/// Reciprocal rank: 1 / position of the first relevant doc in
/// `retrieved` (1-indexed). Returns 0.0 if not present.
pub fn reciprocal_rank(relevant: &str, retrieved: &[String]) -> f64 {
    for (i, r) in retrieved.iter().enumerate() {
        if r == relevant {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Precision, recall, F1 for a multi-relevance retrieval.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prf {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

impl Prf {
    pub fn compute(relevant: &[String], retrieved: &[String]) -> Self {
        if retrieved.is_empty() {
            return Prf { precision: 0.0, recall: 0.0, f1: 0.0 };
        }
        let rel_set: std::collections::HashSet<&str> = relevant.iter().map(String::as_str).collect();
        let hits = retrieved.iter().filter(|r| rel_set.contains(r.as_str())).count();
        let precision = hits as f64 / retrieved.len() as f64;
        let recall = if rel_set.is_empty() {
            0.0
        } else {
            hits as f64 / rel_set.len() as f64
        };
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        Prf { precision, recall, f1 }
    }
}

impl Default for Prf {
    fn default() -> Self { Prf { precision: 0.0, recall: 0.0, f1: 0.0 } }
}

/// Macro-style aggregator: mean of a metric over N items.
pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() { 0.0 } else { xs.iter().sum::<f64>() / xs.len() as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_at_k_hits_in_top() {
        let r = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(recall_at_k("a", &r, 1), 1.0);
        assert_eq!(recall_at_k("b", &r, 2), 1.0);
        assert_eq!(recall_at_k("c", &r, 3), 1.0);
    }

    #[test]
    fn recall_at_k_misses() {
        let r = vec!["a".into(), "b".into()];
        assert_eq!(recall_at_k("c", &r, 2), 0.0);
        assert_eq!(recall_at_k("a", &r, 0), 0.0); // K=0 ⇒ no top-K
    }

    #[test]
    fn recall_at_k_handles_empty() {
        let r: Vec<String> = vec![];
        assert_eq!(recall_at_k("a", &r, 3), 0.0);
    }

    #[test]
    fn reciprocal_rank_first_position() {
        let r = vec!["a".into(), "b".into()];
        assert!((reciprocal_rank("a", &r) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn reciprocal_rank_second_position() {
        let r = vec!["a".into(), "b".into()];
        assert!((reciprocal_rank("b", &r) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn reciprocal_rank_missing() {
        let r = vec!["a".into()];
        assert_eq!(reciprocal_rank("b", &r), 0.0);
    }

    #[test]
    fn prf_perfect() {
        let rel = vec!["a".into(), "b".into()];
        let ret = vec!["a".into(), "b".into()];
        let p = Prf::compute(&rel, &ret);
        assert_eq!(p.precision, 1.0);
        assert_eq!(p.recall, 1.0);
        assert_eq!(p.f1, 1.0);
    }

    #[test]
    fn prf_partial() {
        let rel = vec!["a".into(), "b".into()];
        let ret = vec!["a".into(), "c".into()];
        let p = Prf::compute(&rel, &ret);
        // precision = 1/2, recall = 1/2, f1 = 0.5
        assert!((p.precision - 0.5).abs() < 1e-9);
        assert!((p.recall - 0.5).abs() < 1e-9);
        assert!((p.f1 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn prf_zero_relevant_zero_recall() {
        let rel: Vec<String> = vec![];
        let ret = vec!["a".into()];
        let p = Prf::compute(&rel, &ret);
        assert_eq!(p.recall, 0.0);
        assert_eq!(p.f1, 0.0);
    }

    #[test]
    fn prf_zero_retrieved_is_zero() {
        let rel = vec!["a".into()];
        let ret: Vec<String> = vec![];
        let p = Prf::compute(&rel, &ret);
        assert_eq!(p.precision, 0.0);
        assert_eq!(p.recall, 0.0);
        assert_eq!(p.f1, 0.0);
    }

    #[test]
    fn mean_empty() { assert_eq!(mean(&[]), 0.0); }
    #[test]
    fn mean_one() { assert_eq!(mean(&[3.0]), 3.0); }
    #[test]
    fn mean_three() { assert!((mean(&[1.0, 2.0, 3.0]) - 2.0).abs() < 1e-9); }
}
