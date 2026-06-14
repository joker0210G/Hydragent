//! JSON report serialisation for a benchmark run.
//!
//! The report includes both suite-level aggregate scores and per-task
//! details (query, expected, top-K hits). Reports are written to
//! `reports/bench-v{X}.json` and consumed by the release notes.

use crate::runner::{GoldenScores, SkillBenchScores};
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub version: String,
    pub generated_at: String,
    pub skill_bench: Option<SkillBenchScores>,
    pub golden_set: Option<GoldenScores>,
}

impl BenchReport {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            generated_at: Utc::now().to_rfc3339(),
            skill_bench: None,
            golden_set: None,
        }
    }

    pub fn with_skill_bench(mut self, s: SkillBenchScores) -> Self {
        self.skill_bench = Some(s);
        self
    }

    pub fn with_golden_set(mut self, s: GoldenScores) -> Self {
        self.golden_set = Some(s);
        self
    }

    /// Pretty-print to stdout. One section per suite.
    pub fn print_summary(&self) {
        println!("== Bench Report {} ==", self.version);
        println!("Generated: {}", self.generated_at);
        if let Some(s) = &self.skill_bench {
            println!();
            println!("SKILL-BENCH (n={})", s.n);
            println!("  Recall@1  = {:.3}", s.recall_at_1);
            println!("  Recall@3  = {:.3}", s.recall_at_3);
            println!("  Recall@5  = {:.3}", s.recall_at_5);
            println!("  MRR       = {:.3}", s.mrr);
        }
        if let Some(s) = &self.golden_set {
            println!();
            println!("Golden set (n={})", s.n);
            println!("  Precision = {:.3}", s.mean_precision);
            println!("  Recall    = {:.3}", s.mean_recall);
            println!("  F1        = {:.3}", s.mean_f1);
        }
    }
}

// Allow using SkillBenchScores / GoldenScores directly via the runner
// module without re-importing. This is purely a documentation re-export
// for downstream users.
pub use crate::runner::Retriever;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serialization_roundtrip() {
        let r = BenchReport::new("v0.7.0")
            .with_skill_bench(SkillBenchScores {
                recall_at_1: 0.5, recall_at_3: 0.8, recall_at_5: 0.9,
                mrr: 0.65, n: 80,
            })
            .with_golden_set(GoldenScores {
                mean_precision: 0.4, mean_recall: 0.5, mean_f1: 0.44, n: 30,
            });
        let s = serde_json::to_string(&r).unwrap();
        let back: BenchReport = serde_json::from_str(&s).unwrap();
        assert_eq!(back.version, "v0.7.0");
        assert!(back.skill_bench.is_some());
        assert!(back.golden_set.is_some());
    }

    #[test]
    fn empty_report_serializes() {
        let r = BenchReport::new("v0.7.0");
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("v0.7.0"));
    }
}
