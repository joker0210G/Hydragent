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
