//! Benchmark dataset types and loaders.
//!
//! Two suites live in `tests/bench/`:
//!
//! * `skill_bench_v1.jsonl` — single-label skill retrieval, 80 tasks
//! * `golden_set_v1.jsonl` — multi-label retrieval, 30 hand-verified pairs

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// One row in `skill_bench_v1.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillBenchTask {
    pub id: String,
    pub query: String,
    pub expected_skill: String,
    pub expected_tags: Vec<String>,
    pub difficulty: String, // "easy" | "medium" | "hard"
    pub category: String,
}

/// One row in `golden_set_v1.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoldenSetItem {
    pub id: String,
    pub query: String,
    pub relevant: Vec<String>, // 1..3 skill ids
}

#[derive(Debug, Error)]
pub enum DatasetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse line {line_no}: {source}")]
    Parse { line_no: usize, source: serde_json::Error },
}

/// Load a JSONL file as `Vec<T>`. Skips blank lines; reports line
/// number on parse failures for easy diffing.
pub fn load_jsonl<T>(path: &Path) -> Result<Vec<T>, DatasetError>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in bytes.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let item: T = serde_json::from_str(line)
            .map_err(|e| DatasetError::Parse { line_no: i + 1, source: e })?;
        out.push(item);
    }
    Ok(out)
}

/// Convenience constructor: load SKILL-BENCH.
pub fn load_skill_bench(path: &Path) -> Result<Vec<SkillBenchTask>, DatasetError> {
    load_jsonl(path)
}

/// Convenience constructor: load golden set.
pub fn load_golden_set(path: &Path) -> Result<Vec<GoldenSetItem>, DatasetError> {
    load_jsonl(path)
}

// ─────────────────────────────────────────────────────────────────────
// Task-completion benchmark (end-to-end)
// ─────────────────────────────────────────────────────────────────────
//
// Unlike SKILL-BENCH / golden set (which only measure *retrieval*
// quality), this suite measures whether the agent actually *completes*
// a task end-to-end through the real ReAct loop. Each row carries an
// explicit, mostly-automatic pass/fail rule so we don't need an LLM
// judge yet.
//
// Grading is intentionally simple and transparent:
//
//   * `must_contain`   — every string in this list must appear (case-
//                        insensitive substring) in the agent's final
//                        answer OR in any tool output it produced.
//   * `must_not_contain` — if any string here appears, the task fails
//                        (used to catch "I cannot" / "as an AI" refusals
//                        or leaked secrets).
//   * `required_tools` — if set, the agent must have invoked *all* of
//                        these tool names at least once during the run.
//                        (Order-independent; just a coverage check.)
//
// A task passes only if ALL of the above hold. This is deliberately
// conservative: a missing keyword fails the task rather than guessing.

/// One row in `task_bench_v1.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskBenchItem {
    pub id: String,
    /// Human-readable difficulty bucket, mirrors the retrieval suites.
    pub difficulty: String, // "easy" | "medium" | "hard"
    /// The prompt we feed to the agent (stands in for a user message).
    pub prompt: String,
    /// Every string here must appear (case-insensitive) in the final
    /// answer or in any tool output. Empty = no keyword requirement.
    #[serde(default)]
    pub must_contain: Vec<String>,
    /// If any string here appears anywhere in the answer/tool output,
    /// the task fails. Empty = no prohibition.
    #[serde(default)]
    pub must_not_contain: Vec<String>,
    /// If set, the agent must call every one of these tools ≥1 time.
    #[serde(default)]
    pub required_tools: Vec<String>,
    /// Free-text note for humans reading the report (not used for grading).
    #[serde(default)]
    pub note: String,
}

/// Convenience constructor: load the task-completion suite.
pub fn load_task_bench(path: &Path) -> Result<Vec<TaskBenchItem>, DatasetError> {
    load_jsonl(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn loads_skill_bench_tasks() {
        let f = write_tmp(
            r#"{"id":"SB0001","query":"x","expected_skill":"a","expected_tags":["t"],"difficulty":"easy","category":"code"}
{"id":"SB0002","query":"y","expected_skill":"b","expected_tags":[],"difficulty":"hard","category":"data"}
"#,
        );
        let tasks = load_skill_bench(f.path()).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "SB0001");
        assert_eq!(tasks[1].expected_skill, "b");
        assert_eq!(tasks[1].difficulty, "hard");
    }

    #[test]
    fn loads_golden_set_items() {
        let f = write_tmp(
            r#"{"id":"GS0001","query":"x","relevant":["a"]}
{"id":"GS0002","query":"y","relevant":["a","b"]}
"#,
        );
        let items = load_golden_set(f.path()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].relevant, vec!["a"]);
        assert_eq!(items[1].relevant.len(), 2);
    }

    #[test]
    fn loads_task_bench_items_with_defaults() {
        // `must_contain` / `must_not_contain` / `required_tools` / `note`
        // are optional and should default to empty when omitted.
        let f = write_tmp(
            r#"{"id":"TB0001","difficulty":"easy","prompt":"say hello"}
{"id":"TB0002","difficulty":"hard","prompt":"chain tools","must_contain":["done"],"required_tools":["web_search","memory_store"],"note":"needs 2 tools"}
"#,
        );
        let tasks = load_task_bench(f.path()).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].must_contain.is_empty());
        assert!(tasks[0].required_tools.is_empty());
        assert_eq!(tasks[1].must_contain, vec!["done"]);
        assert_eq!(tasks[1].required_tools, vec!["web_search", "memory_store"]);
        assert_eq!(tasks[1].note, "needs 2 tools");
    }

    #[test]
    fn skips_blank_lines() {
        let f = write_tmp(
            r#"{"id":"GS0001","query":"x","relevant":["a"]}

{"id":"GS0002","query":"y","relevant":["b"]}
"#,
        );
        let items = load_golden_set(f.path()).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn reports_line_number_on_parse_error() {
        let f = write_tmp(
            r#"{"id":"GS0001","query":"x","relevant":["a"]}
this-is-not-json
"#,
        );
        let err = load_golden_set(f.path()).unwrap_err();
        match err {
            DatasetError::Parse { line_no, .. } => assert_eq!(line_no, 2),
            _ => panic!("expected Parse error, got {err:?}"),
        }
    }
}
