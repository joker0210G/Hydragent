//! # Bounded Markdown Memory
//!
//! Hydragent's implementation of the Hermes "bounded hot memory" pattern.
//!
//! Traditional passive storage systems accumulate everything without judgment,
//! producing bloat and collapsing signal-to-noise over time. The fix is two
//! plain Markdown files with **strict character-count ceilings** that force
//! the agent to curate rather than archive.
//!
//! ## Files and Limits
//!
//! | File | Limit | Purpose |
//! |------|-------|---------|
//! | `config/USER.md`  | [`USER_MD_CHAR_LIMIT`]  | Episodic — user preferences, style habits, communication patterns |
//! | `config/SOUL.md`  | [`SOUL_MD_CHAR_LIMIT`]  | World — agent personality, behavior rules, project context |
//!
//! ## Compaction Strategy (Hermes true approach)
//!
//! When a file exceeds its limit after an append, the **dream cycle** calls
//! an LLM re-synthesis pass (`compact_md_with_llm` in `dream.rs`). The LLM
//! receives the full over-limit content and rewrites it to fit within the cap,
//! merging near-duplicates and ranking by importance. This module intentionally
//! has **no LLM dependency** — it only tracks limits and performs file I/O.
//! Compaction orchestration lives in `hydragent-core/src/dream.rs`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::warn;

// ─────────────────────────────────────────────────────────────────────────────
// Character-limit constants
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum character count for `config/USER.md`.
///
/// Stores episodic user memory: preferences, communication style, recurring
/// patterns. The limit is intentionally modest to force curation of the
/// highest-signal user traits. At ~4 chars/token this is ≈1,500 tokens.
///
/// Hermes uses 1,375 chars; Hydragent uses 6,000 because the user profile
/// spans multi-domain technical context that needs more room to be actionable.
pub const USER_MD_CHAR_LIMIT: usize = 6_000;

/// Maximum character count for `config/SOUL.md`.
///
/// Stores world/agent memory: behavior rules, personality, project context.
/// SOUL.md covers what Hermes splits across both `user.md` and `memory.md`,
/// so its budget is correspondingly larger (~3,000 tokens at 4 chars/token).
///
/// Hermes uses 2,200 chars for `memory.md`; Hydragent uses 12,000 to
/// accommodate the combined agent-soul + project-world scope.
pub const SOUL_MD_CHAR_LIMIT: usize = 12_000;

/// Fraction of the limit at which a "headroom low" warning is emitted.
/// e.g. 0.10 → warn when < 10% of the budget remains.
const HEADROOM_WARN_THRESHOLD: f64 = 0.10;

// ─────────────────────────────────────────────────────────────────────────────
// BoundedMd
// ─────────────────────────────────────────────────────────────────────────────

/// A Markdown file with an enforced character-count ceiling.
///
/// `BoundedMd` wraps a path and a limit. It provides append-with-dedup and
/// limit-check helpers. **It does not perform LLM compaction itself** — that
/// responsibility lives in `hydragent-core/src/dream.rs` which has access to
/// the `ModelRouter`. Call [`BoundedMd::needs_compaction`] to check, then
/// drive the LLM call externally.
///
/// # Example
/// ```rust,no_run
/// use hydragent_memory::bounded_md::{BoundedMd, USER_MD_CHAR_LIMIT};
///
/// let bmd = BoundedMd::new("./config/USER.md", USER_MD_CHAR_LIMIT);
/// let appended = bmd.append_curated(
///     &["The user prefers Tokio async patterns.".to_string()],
///     "# Style & Communication Habits",
///     "# User Profile\n- Name: User\n\n# Style & Communication Habits\n",
/// ).unwrap();
/// if bmd.needs_compaction().unwrap() {
///     // call compact_md_with_llm(...) from dream.rs
/// }
/// ```
pub struct BoundedMd {
    path: PathBuf,
    limit: usize,
}

impl BoundedMd {
    /// Construct a `BoundedMd` for `path` with the given character `limit`.
    ///
    /// The file does not need to exist yet — it will be created on the first
    /// write if missing, using `default_template` supplied to
    /// [`append_curated`][BoundedMd::append_curated].
    pub fn new(path: impl Into<PathBuf>, limit: usize) -> Self {
        Self { path: path.into(), limit }
    }

    /// Read the file content. Returns an empty string if the file doesn't
    /// exist (not an error — the file may not have been created yet).
    pub fn read(&self) -> Result<String> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e).with_context(|| format!("BoundedMd: read {:?}", self.path)),
        }
    }

    /// Overwrite the file with `content`. Creates parent directories if needed.
    pub fn write(&self, content: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("BoundedMd: create dirs {:?}", parent))?;
        }
        std::fs::write(&self.path, content)
            .with_context(|| format!("BoundedMd: write {:?}", self.path))
    }

    /// Current character count of the file. Returns 0 if the file doesn't
    /// exist.
    pub fn len(&self) -> Result<usize> {
        Ok(self.read()?.chars().count())
    }

    /// Returns `true` if the file currently exceeds `self.limit` characters.
    pub fn is_over_limit(&self) -> Result<bool> {
        Ok(self.len()? > self.limit)
    }

    /// Returns `true` if the file currently exceeds `self.limit` characters.
    /// Alias for [`is_over_limit`][BoundedMd::is_over_limit] — used as the
    /// trigger signal in the dream cycle.
    pub fn needs_compaction(&self) -> Result<bool> {
        self.is_over_limit()
    }

    /// Remaining headroom as a percentage `[0.0, 100.0]`.
    ///
    /// Returns 0.0 when the file is at or over the limit. Values below
    /// `HEADROOM_WARN_THRESHOLD * 100` are worth a log warning.
    pub fn headroom_pct(&self) -> Result<f64> {
        let current = self.len()? as f64;
        let limit = self.limit as f64;
        if current >= limit {
            return Ok(0.0);
        }
        Ok((limit - current) / limit * 100.0)
    }

    /// Append `items` under `section_header`, skipping items already present
    /// (case-insensitive substring check). Creates the file from
    /// `default_template` if it doesn't exist yet.
    ///
    /// Returns `true` if at least one item was actually written.
    ///
    /// After calling this, check [`needs_compaction`][BoundedMd::needs_compaction]
    /// and trigger an LLM compaction pass in `dream.rs` if needed.
    pub fn append_curated(
        &self,
        items: &[String],
        section_header: &str,
        default_template: &str,
    ) -> Result<bool> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("BoundedMd: create dirs {:?}", parent))?;
        }

        let mut content = match std::fs::read_to_string(&self.path) {
            Ok(s) if s.is_empty() => default_template.to_string(),
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                default_template.to_string()
            }
            Err(e) => return Err(e).with_context(|| format!("BoundedMd: read {:?}", self.path)),
        };

        // Ensure the section header exists in the file
        if !content.contains(section_header) {
            if !content.ends_with('\n') && !content.is_empty() {
                content.push('\n');
            }
            content.push('\n');
            content.push_str(section_header);
            content.push('\n');
        }

        let mut wrote_any = false;
        let lowered_content_snapshot = content.to_lowercase();

        for item in items {
            let normalized = item.trim();
            if normalized.is_empty() {
                continue;
            }
            // Case-insensitive dedup — avoid re-storing near-identical entries
            if lowered_content_snapshot.contains(&normalized.to_lowercase()) {
                continue;
            }
            if !content.ends_with('\n') && !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&format!("- {}\n", normalized));
            wrote_any = true;
        }

        if wrote_any {
            std::fs::write(&self.path, &content)
                .with_context(|| format!("BoundedMd: write {:?}", self.path))?;

            // Emit a low-headroom warning so the dream cycle log surfaces it
            let current_chars = content.chars().count();
            let remaining_pct = if current_chars >= self.limit {
                0.0
            } else {
                (self.limit - current_chars) as f64 / self.limit as f64
            };
            if remaining_pct < HEADROOM_WARN_THRESHOLD {
                warn!(
                    path = ?self.path,
                    current_chars,
                    limit = self.limit,
                    headroom_pct = remaining_pct * 100.0,
                    "BoundedMd: file is within {}% of its character limit — compaction will be triggered",
                    (HEADROOM_WARN_THRESHOLD * 100.0) as u32,
                );
            }
        }

        Ok(wrote_any)
    }

    /// The character limit this instance enforces.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The file path managed by this instance.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn temp_bmd(limit: usize) -> (BoundedMd, NamedTempFile) {
        let file = NamedTempFile::new().unwrap();
        let bmd = BoundedMd::new(file.path(), limit);
        (bmd, file)
    }

    #[test]
    fn test_constants_exported() {
        assert!(USER_MD_CHAR_LIMIT > 0);
        assert!(SOUL_MD_CHAR_LIMIT > 0);
        assert!(SOUL_MD_CHAR_LIMIT > USER_MD_CHAR_LIMIT,
            "SOUL.md must have a larger budget than USER.md");
    }

    #[test]
    fn test_read_nonexistent_returns_empty() {
        let bmd = BoundedMd::new("/tmp/__hydragent_nonexistent_test_file_xyz.md", 100);
        assert_eq!(bmd.read().unwrap(), "");
        assert_eq!(bmd.len().unwrap(), 0);
        assert!(!bmd.is_over_limit().unwrap());
        assert!(!bmd.needs_compaction().unwrap());
    }

    #[test]
    fn test_append_under_limit() {
        let (bmd, _f) = temp_bmd(1000);
        let items = vec!["The user prefers Tokio.".to_string()];
        let wrote = bmd.append_curated(&items, "# Habits", "# Profile\n\n# Habits\n").unwrap();
        assert!(wrote);
        let content = bmd.read().unwrap();
        assert!(content.contains("The user prefers Tokio."));
        assert!(!bmd.needs_compaction().unwrap());
    }

    #[test]
    fn test_needs_compaction_when_over_limit() {
        let (bmd, mut f) = temp_bmd(10);
        // Write content well over the limit
        write!(f, "{}", "a".repeat(50)).unwrap();
        f.flush().unwrap();
        assert!(bmd.needs_compaction().unwrap());
        assert!(bmd.is_over_limit().unwrap());
        assert_eq!(bmd.headroom_pct().unwrap(), 0.0);
    }

    #[test]
    fn test_dedup_no_duplicate_appended() {
        let (bmd, _f) = temp_bmd(2000);
        let template = "# Profile\n\n# Habits\n";
        let items = vec!["The user prefers Tokio.".to_string()];
        bmd.append_curated(&items, "# Habits", template).unwrap();
        // Second append of the same item — should be skipped
        let wrote = bmd.append_curated(&items, "# Habits", template).unwrap();
        assert!(!wrote, "Duplicate item must not be re-appended");
        // Verify the item appears exactly once
        let content = bmd.read().unwrap();
        let count = content.matches("The user prefers Tokio.").count();
        assert_eq!(count, 1, "Item should appear exactly once in the file");
    }

    #[test]
    fn test_header_preserved_after_write() {
        let (bmd, _f) = temp_bmd(5000);
        let template = "# User Profile\n- Name: User\n\n# Style & Communication Habits\n";
        let items = vec!["Uses parentheses for meta-thoughts.".to_string()];
        bmd.append_curated(&items, "# Style & Communication Habits", template).unwrap();
        let content = bmd.read().unwrap();
        assert!(content.starts_with("# User Profile"));
        assert!(content.contains("- Name: User"));
    }

    #[test]
    fn test_write_and_read_roundtrip() {
        let (bmd, _f) = temp_bmd(1000);
        let data = "# Test\n- fact one\n";
        bmd.write(data).unwrap();
        assert_eq!(bmd.read().unwrap(), data);
        assert_eq!(bmd.len().unwrap(), data.chars().count());
    }

    #[test]
    fn test_headroom_pct_within_limit() {
        let (bmd, _f) = temp_bmd(100);
        bmd.write("12345").unwrap(); // 5 chars in a 100-char limit
        let pct = bmd.headroom_pct().unwrap();
        assert!((pct - 95.0).abs() < 0.01);
    }

    #[test]
    fn test_limit_and_path_accessors() {
        let bmd = BoundedMd::new("/tmp/test.md", USER_MD_CHAR_LIMIT);
        assert_eq!(bmd.limit(), USER_MD_CHAR_LIMIT);
        assert_eq!(bmd.path(), std::path::Path::new("/tmp/test.md"));
    }
}
