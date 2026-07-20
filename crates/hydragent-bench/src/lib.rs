//! # hydragent-bench
//!
//! Phase 7 / Track 7.4 - Benchmarking harness and golden set.
//!
//! This crate ships three benchmark suites and a runner:
//!
//! * **SKILL-BENCH** (`skill_bench_v1.jsonl`) — 80 skill-retrieval
//!   tasks. Each task has a query and an expected skill. The runner
//!   queries the skill library, measures Recall@1 / Recall@3 /
//!   MRR@10, and writes a JSON report.
//! * **Golden set** (`golden_set_v1.jsonl`) — 30 hand-verified
//!   `(query, relevant_skill_ids)` pairs. Evaluates retrieval
//!   precision / recall / F1.
//! * **TASK-BENCH** (`task_bench_v1.jsonl`) — ~20 end-to-end
//!   task-completion tasks. The runner drives the **real ReAct loop**
//!   for each task and grades the result against an automatic
//!   pass/fail rule (must_contain / must_not_contain / required_tools).
//!
//! The retrieval runners are invoked from `bin/bench.rs`; the
//! task-completion runner lives in `bin/task_bench.rs`.
//!
//! ## Quick start
//! ```bash
//! # Retrieval benchmarks
//! cargo run -p hydragent-bench --release --bin bench -- \
//!     --skill-bench tests/bench/skill_bench_v1.jsonl \
//!     --golden-set tests/bench/golden_set_v1.jsonl \
//!     --output reports/bench-v0.7.0.json
//!
//! # End-to-end task benchmark
//! cargo run -p hydragent-bench --release --bin task_bench -- \
//!     --tasks tests/bench/task_bench_v1.jsonl \
//!     --output reports/task_bench-v0.7.0.json
//! ```

pub mod dataset;
pub mod metrics;
pub mod runner;
pub mod report;
