//! # hydragent-bench
//!
//! Phase 7 / Track 7.4 - Benchmarking harness and golden set.
//!
//! This crate ships two benchmark suites and a runner:
//!
//! * **SKILL-BENCH** (`skill_bench_v1.jsonl`) — 80 skill-retrieval
//!   tasks. Each task has a query and an expected skill. The runner
//!   queries the skill library, measures Recall@1 / Recall@3 /
//!   MRR@10, and writes a JSON report.
//! * **Golden set** (`golden_set_v1.jsonl`) — 30 hand-verified
//!   `(query, relevant_skill_ids)` pairs. Evaluates retrieval
//!   precision / recall / F1.
//!
//! The runner is invoked from the binary `hydragent-bench` (see
//! `bin/bench.rs`).
//!
//! ## Quick start
//! ```bash
//! cargo run -p hydragent-bench --release -- \
//!     --skill-bench tests/bench/skill_bench_v1.jsonl \
//!     --golden-set tests/bench/golden_set_v1.jsonl \
//!     --skill-library data/skill_library.sqlite \
//!     --output reports/bench-v0.7.0.json
//! ```

pub mod dataset;
pub mod metrics;
pub mod runner;
pub mod report;
