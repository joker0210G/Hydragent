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

/// Cosine similarity between two embedding vectors. Returns `None` if
/// the vectors have different lengths or either is empty.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() { return None; }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { return None; }
    Some(dot / (norm_a * norm_b))
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

    // ----------------------------------------------------------------
    // cosine_similarity tests
    // ----------------------------------------------------------------

    #[test]
    fn cosine_identical_vectors_is_one() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_perpendicular_is_zero() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b).unwrap() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_is_minus_one() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b).unwrap() - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn cosine_different_lengths_returns_none() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), None);
    }

    #[test]
    fn cosine_empty_returns_none() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&a, &b), None);
    }

    #[test]
    fn cosine_zero_vector_returns_none() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), None);
    }

    #[test]
    fn cosine_partial_overlap_works() {
        let a = vec![1.0, 1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        // dot = 1, norm_a = sqrt(2), norm_b = 1, sim = 1/sqrt(2) ≈ 0.707
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!((sim - 0.70710678).abs() < 1e-5);
    }
}
