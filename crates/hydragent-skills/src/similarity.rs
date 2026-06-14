//! Phase 7 / Track 7.1 - Similarity search helpers.
//!
//! **Status: skeleton**. The real implementation uses either a
//! vector index (sqlite-vec) or a Jaccard tag-similarity ranker.
//! For now we expose a tiny tag-Jaccard helper that the FTS5 query
//! fallback can use.

use std::collections::HashSet;

/// Jaccard similarity between two tag sets. Returns 0.0 when both are
/// empty (no overlap is meaningful when there's nothing to overlap).
pub fn jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let sa: HashSet<&str> = a.iter().map(String::as_str).collect();
    let sb: HashSet<&str> = b.iter().map(String::as_str).collect();
    let inter = sa.intersection(&sb).count() as f32;
    let union = sa.union(&sb).count() as f32;
    if union == 0.0 { 0.0 } else { inter / union }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_identical_is_one() {
        let a = vec!["x".to_string(), "y".to_string()];
        let b = vec!["y".to_string(), "x".to_string()];
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_disjoint_is_zero() {
        let a = vec!["x".to_string()];
        let b = vec!["y".to_string()];
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn jaccard_overlap_partial() {
        let a = vec!["x".to_string(), "y".to_string()];
        let b = vec!["y".to_string(), "z".to_string()];
        // |intersect| = 1, |union| = 3 → 1/3
        assert!((jaccard(&a, &b) - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_both_empty_is_zero() {
        let a: Vec<String> = vec![];
        let b: Vec<String> = vec![];
        assert_eq!(jaccard(&a, &b), 0.0);
    }
}
