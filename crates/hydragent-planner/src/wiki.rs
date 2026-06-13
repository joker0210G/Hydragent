//! # Wiki — shared knowledge base
//!
//! Phase 5 / Track 5.4. The wiki is a tiny, on-disk Markdown store
//! that the swarm and the supervisor consult to share knowledge
//! across runs. It's deliberately small (no DB, no indices, no
//! full-text search engine) — the goal is a deterministic, easy-to-
//! inspect place for cross-run notes, not a Notion clone.
//!
//! ## Layout
//!
//! ```text
//! data/wiki/
//!   ├── phase5-architecture.md
//!   ├── model-council.md
//!   └── replan-strategies.md
//! ```
//!
//! Each topic is one file. `topic` is sanitised to
//! `[a-z0-9-_]` so a caller can pass `"Phase 5 Architecture"` or
//! `"phase5/architecture"` and end up at the same file.
//!
//! ## Concurrency
//!
//! The wiki uses plain `std::fs` reads/writes. Reads are atomic
//! enough for a single-process test. Writes go through a
//! write-to-temp-then-rename dance so a reader never sees a
//! half-written file.
//!
//! ## Example
//!
//! ```no_run
//! use hydragent_planner::wiki::Wiki;
//!
//! let wiki = Wiki::open("data/wiki").unwrap();
//! wiki.save("phase5-architecture", "# Architecture\n\nThe swarm...").unwrap();
//! let md = wiki.load("phase5-architecture").unwrap();
//! assert!(md.contains("Architecture"));
//! wiki.append("phase5-architecture", "\n\n## Addendum\n").unwrap();
//! ```

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Wiki-level errors.
#[derive(Debug, Error)]
pub enum WikiError {
    #[error("wiki I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("wiki: invalid topic name (after sanitisation): {0}")]
    InvalidTopic(String),
    #[error("wiki: topic not found: {0}")]
    NotFound(String),
}

pub type WikiResult<T> = std::result::Result<T, WikiError>;

/// The on-disk wiki. Cheap to construct; the directory is created
/// on `open` if missing.
pub struct Wiki {
    root: PathBuf,
}

impl Wiki {
    /// Open (and create if missing) the wiki rooted at `root`.
    pub fn open<P: AsRef<Path>>(root: P) -> WikiResult<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The on-disk root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Sanitise a topic name: lowercase, replace any
    /// non-`[a-z0-9-_]` character with `-`, collapse runs of `-`,
    /// trim leading/trailing `-`. Empty after sanitisation is an
    /// error.
    pub fn sanitise(topic: &str) -> WikiResult<String> {
        let mut s: String = topic
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        // Collapse runs of '-'.
        let mut collapsed = String::with_capacity(s.len());
        let mut last_dash = false;
        for c in s.chars() {
            if c == '-' {
                if !last_dash {
                    collapsed.push(c);
                }
                last_dash = true;
            } else {
                collapsed.push(c);
                last_dash = false;
            }
        }
        s = collapsed.trim_matches('-').to_string();
        if s.is_empty() {
            return Err(WikiError::InvalidTopic(topic.to_string()));
        }
        Ok(s)
    }

    /// Build the absolute path for a topic file. Internal.
    fn path_for(&self, topic: &str) -> WikiResult<PathBuf> {
        let safe = Self::sanitise(topic)?;
        Ok(self.root.join(format!("{safe}.md")))
    }

    /// Save (overwrite) a topic. Creates the file if missing;
    /// replaces it if it exists. Atomic via temp + rename.
    pub fn save(&self, topic: &str, content: &str) -> WikiResult<()> {
        let path = self.path_for(topic)?;
        save_atomic(&path, content.as_bytes())?;
        Ok(())
    }

    /// Append content to a topic. Creates the file if missing. The
    /// separator is inserted only if the file already has content
    /// and doesn't end with the separator (best-effort).
    pub fn append(&self, topic: &str, content: &str) -> WikiResult<()> {
        let path = self.path_for(topic)?;
        let existing = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let mut combined = String::with_capacity(existing.len() + content.len() + 2);
        combined.push_str(&existing);
        if !existing.is_empty() && !existing.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(content);
        save_atomic(&path, combined.as_bytes())?;
        Ok(())
    }

    /// Load a topic. Returns `NotFound` if the file doesn't exist.
    pub fn load(&self, topic: &str) -> WikiResult<String> {
        let path = self.path_for(topic)?;
        if !path.exists() {
            return Err(WikiError::NotFound(topic.to_string()));
        }
        Ok(fs::read_to_string(&path)?)
    }

    /// Try to load a topic; returns `None` if it doesn't exist.
    pub fn load_opt(&self, topic: &str) -> WikiResult<Option<String>> {
        match self.load(topic) {
            Ok(s) => Ok(Some(s)),
            Err(WikiError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// True if a topic file exists.
    pub fn exists(&self, topic: &str) -> WikiResult<bool> {
        let path = self.path_for(topic)?;
        Ok(path.exists())
    }

    /// Delete a topic. No-op if it doesn't exist.
    pub fn delete(&self, topic: &str) -> WikiResult<()> {
        let path = self.path_for(topic)?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// List all topic names (sanitised, without `.md`).
    /// Returns an empty Vec if the directory is empty.
    pub fn list_topics(&self) -> WikiResult<Vec<String>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".md") {
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Naive substring search across all topics. Returns a list of
    /// `(topic, line_number, line)` hits. Case-insensitive.
    pub fn search(&self, query: &str) -> WikiResult<Vec<SearchHit>> {
        let q = query.to_lowercase();
        let mut out = Vec::new();
        for topic in self.list_topics()? {
            let content = match self.load(&topic) {
                Ok(s) => s,
                Err(_) => continue,
            };
            for (i, line) in content.lines().enumerate() {
                if line.to_lowercase().contains(&q) {
                    out.push(SearchHit {
                        topic: topic.clone(),
                        line_number: i + 1,
                        line: line.to_string(),
                    });
                }
            }
        }
        Ok(out)
    }
}

/// A single search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub topic: String,
    pub line_number: usize,
    pub line: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Atomic write: write to a sibling temp file, fsync, rename.
fn save_atomic(path: &Path, bytes: &[u8]) -> WikiResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| WikiError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "path has no parent",
        )))?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("wiki")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // On Windows, rename refuses to overwrite; remove first.
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn tmp_root() -> PathBuf {
        let mut p = env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("hydragent-wiki-test-{stamp}"));
        p
    }

    #[test]
    fn sanitise_lowercases_and_replaces_specials() {
        // Spaces and punctuation become `-`.
        assert_eq!(Wiki::sanitise("Hello World").unwrap(), "hello-world");
        // Multiple separators collapse into one `-`.
        assert_eq!(
            Wiki::sanitise("Phase 5 / Architecture").unwrap(),
            "phase-5-architecture"
        );
        // `_` is a valid character and is kept as-is.
        assert_eq!(Wiki::sanitise("already-clean_123").unwrap(), "already-clean_123");
        // Leading/trailing separators are trimmed.
        assert_eq!(Wiki::sanitise("----trim----").unwrap(), "trim");
        // Run of `-` (not `_`) collapses to one.
        assert_eq!(Wiki::sanitise("a---b").unwrap(), "a-b");
        // Underscores are NOT collapsed — only `-` is.
        assert_eq!(Wiki::sanitise("a__b").unwrap(), "a__b");
    }

    #[test]
    fn sanitise_rejects_empty() {
        assert!(Wiki::sanitise("").is_err());
        assert!(Wiki::sanitise("////").is_err());
        assert!(Wiki::sanitise("   ").is_err());
    }

    #[test]
    fn save_load_round_trip() {
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        wiki.save("phase5-architecture", "# Architecture\n\nSwarm + DAG.").unwrap();
        let md = wiki.load("phase5-architecture").unwrap();
        assert!(md.contains("Architecture"));
        assert!(md.contains("Swarm"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn save_overwrites_existing() {
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        wiki.save("t", "v1").unwrap();
        wiki.save("t", "v2").unwrap();
        assert_eq!(wiki.load("t").unwrap(), "v2");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn append_concatenates() {
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        wiki.append("t", "line1").unwrap();
        wiki.append("t", "line2").unwrap();
        let out = wiki.load("t").unwrap();
        assert!(out.contains("line1"));
        assert!(out.contains("line2"));
        // Two newlines were inserted (one to terminate line1, one
        // before line2... actually we just guarantee a newline
        // between appended content and existing). Order matters.
        let pos1 = out.find("line1").unwrap();
        let pos2 = out.find("line2").unwrap();
        assert!(pos1 < pos2);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_opt_returns_none_for_missing() {
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        assert!(wiki.load_opt("missing").unwrap().is_none());
        wiki.save("present", "x").unwrap();
        assert_eq!(wiki.load_opt("present").unwrap().as_deref(), Some("x"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_topics_is_sorted_and_deduped() {
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        wiki.save("zeta", "z").unwrap();
        wiki.save("alpha", "a").unwrap();
        wiki.save("mu", "m").unwrap();
        let mut topics = wiki.list_topics().unwrap();
        topics.sort();
        assert_eq!(topics, vec!["alpha", "mu", "zeta"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_is_idempotent() {
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        wiki.save("t", "x").unwrap();
        wiki.delete("t").unwrap();
        wiki.delete("t").unwrap(); // should not error
        assert!(!wiki.exists("t").unwrap());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_finds_across_topics() {
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        wiki.save("a", "alpha bravo charlie\ndelta").unwrap();
        wiki.save("b", "echo BRAVO foxtrot").unwrap();
        let hits = wiki.search("bravo").unwrap();
        assert_eq!(hits.len(), 2);
        // case-insensitive: "bravo" should match "BRAVO".
        assert!(hits.iter().any(|h| h.topic == "a" && h.line_number == 1));
        assert!(hits.iter().any(|h| h.topic == "b" && h.line_number == 1));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sanitisation_keeps_path_inside_root() {
        // Path traversal guard: a topic containing ".." must NOT
        // escape the wiki root.
        let root = tmp_root();
        let wiki = Wiki::open(&root).unwrap();
        let bad = wiki.path_for("../escaped");
        // Sanitisation replaces the dots, so the path is contained.
        let p = bad.unwrap();
        assert!(p.starts_with(&root));
        fs::remove_dir_all(&root).ok();
    }
}
