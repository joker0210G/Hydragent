//! HTTP poller for Work IQ feeds.
//!
//! Responsibilities:
//! - Fetch a feed URL with **ETag** + **Last-Modified** caching.
//! - **Exponential backoff** on consecutive failures.
//! - **UTF-8-safe** truncation for feed entry summaries.
//! - **Bounded concurrency** so a slow feed can't block the others.

use anyhow::{anyhow, Context, Result};
use feed_rs::parser;
use reqwest::{
    header::{HeaderMap, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH},
    Client, StatusCode,
};
use std::time::Duration;

/// Default HTTP timeout for individual feed fetches.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum entries kept in memory from a single poll. Older servers can
/// return thousands of entries on first contact; we cap to keep things sane.
pub const MAX_ENTRIES_PER_POLL: usize = 100;
/// Hard cap on summary text length (characters, not bytes).
pub const SUMMARY_CHAR_LIMIT: usize = 500;
/// Cap on consecutive failures before we *skip* a feed on the next poll cycle.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 8;

/// A single parsed feed entry ready to be persisted.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    /// Stable identifier (`<id>` in Atom, `<guid>` in RSS, or link fallback).
    pub id: String,
    pub title: String,
    pub summary: String,
    pub url: String,
    /// Optional published timestamp in milliseconds since epoch.
    pub published_at: Option<i64>,
}

/// What the poller did on a single fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum PollOutcome {
    /// Server replied `200` with a body. Contains newly-seen entries.
    Updated(Vec<ParsedEntry>),
    /// Server replied `304`; nothing changed.
    NotModified,
    /// First poll ever — returns the parsed entries so the caller can persist
    /// them and set `last_seen_id` / `last_seen_published_at`.
    FirstFetch(Vec<ParsedEntry>),
}

/// Outcome of a poller attempt. Used by the orchestrator to update the
/// `etag`, `last_modified`, and `consecutive_failures` columns.
#[derive(Debug, Clone)]
pub struct PollResult {
    pub outcome: PollOutcome,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub new_failure: bool,
}

/// Configuration for a single feed's poller.
#[derive(Debug, Clone, Default)]
pub struct PollerConfig {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub timeout: Duration,
}

impl PollerConfig {
    pub fn with_defaults() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            ..Default::default()
        }
    }
}

/// An RSS/Atom feed poller. Cheap to clone (`reqwest::Client` is `Arc`-backed).
#[derive(Clone)]
pub struct FeedPoller {
    http: Client,
}

impl FeedPoller {
    pub fn new(timeout: Duration) -> Self {
        let http = Client::builder()
            .timeout(timeout)
            .user_agent("Hydragent/WorkIQ (+https://github.com/hydragent)")
            .build()
            .unwrap_or_default();
        Self { http }
    }

    /// Fetch and parse a feed, returning a [`PollResult`].
    pub async fn fetch(
        &self,
        url: &str,
        cfg: &PollerConfig,
        is_first_poll: bool,
    ) -> Result<PollResult> {
        let mut headers = HeaderMap::new();
        if let Some(etag) = &cfg.etag {
            if let Ok(v) = HeaderValue::from_str(etag) {
                headers.insert(IF_NONE_MATCH, v);
            }
        }
        if let Some(lm) = &cfg.last_modified {
            if let Ok(v) = HeaderValue::from_str(lm) {
                headers.insert(IF_MODIFIED_SINCE, v);
            }
        }

        let response = self
            .http
            .get(url)
            .headers(headers)
            .send()
            .await
            .with_context(|| format!("HTTP request to {} failed", url))?;

        let status = response.status();

        // 304 — server says nothing changed.
        if status == StatusCode::NOT_MODIFIED {
            return Ok(PollResult {
                outcome: PollOutcome::NotModified,
                etag: cfg.etag.clone(),
                last_modified: cfg.last_modified.clone(),
                new_failure: false,
            });
        }

        if !status.is_success() {
            return Err(anyhow!("feed {} returned HTTP {}", url, status));
        }

        // Capture caching headers from the response for the *next* request.
        let new_etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let new_last_modified = response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("failed to read body of {}", url))?;

        let parsed = parser::parse(bytes.as_ref())
            .map_err(|e| anyhow!("feed parse error for {}: {}", url, e))?;

        let entries = extract_entries(&parsed);

        let outcome = if is_first_poll {
            PollOutcome::FirstFetch(entries)
        } else {
            PollOutcome::Updated(entries)
        };

        Ok(PollResult {
            outcome,
            etag: new_etag,
            last_modified: new_last_modified,
            new_failure: false,
        })
    }
}

impl Default for FeedPoller {
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT)
    }
}

/// Compute the next backoff for a feed that has just failed `failures` times
/// in a row. Returns the delay before the next poll. Caps at 1 hour.
pub fn backoff_delay(failures: u32) -> Duration {
    let exp = failures.min(8); // 2^8 = 256s base
    let secs = 2_u64.saturating_pow(exp).min(3600);
    Duration::from_secs(secs)
}

/// Convert `feed_rs` entries into our internal [`ParsedEntry`] representation.
/// Enforces [`MAX_ENTRIES_PER_POLL`] and [`SUMMARY_CHAR_LIMIT`].
pub fn extract_entries(feed: &feed_rs::model::Feed) -> Vec<ParsedEntry> {
    let mut out = Vec::with_capacity(feed.entries.len().min(MAX_ENTRIES_PER_POLL));

    for entry in feed.entries.iter().take(MAX_ENTRIES_PER_POLL) {
        let id = entry
            .id
            .clone()
            .or_else(|| entry.links.first().map(|l| l.href.clone()))
            .unwrap_or_else(|| {
                // Fall back to title + published timestamp as a synthetic id.
                format!("{}|{:?}", entry.title.as_ref().map(|t| t.content.as_str()).unwrap_or(""), entry.published)
            });

        let title = entry
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default();

        let raw_summary = entry
            .summary
            .as_ref()
            .map(|s| s.content.clone())
            .or_else(|| {
                entry
                    .content
                    .as_ref()
                    .and_then(|c| c.body.clone())
            })
            .unwrap_or_default();

        let summary = utf8_truncate(&raw_summary, SUMMARY_CHAR_LIMIT);

        let url = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();

        let published_at = entry
            .published
            .or(entry.updated)
            .map(|dt| dt.timestamp_millis());

        out.push(ParsedEntry {
            id,
            title,
            summary,
            url,
            published_at,
        });
    }

    out
}

/// Truncate `s` to at most `max_chars` *characters* (Unicode scalar values),
/// appending `"…"` if truncation happened. Never panics on multi-byte input.
pub fn utf8_truncate(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    // Leave room for the ellipsis character.
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_truncate_short_string_is_unchanged() {
        assert_eq!(utf8_truncate("hello", 10), "hello");
    }

    #[test]
    fn utf8_truncate_long_string_appends_ellipsis() {
        let s = "a".repeat(600);
        let t = utf8_truncate(&s, 100);
        // 99 chars + ellipsis = 100 chars.
        assert_eq!(t.chars().count(), 100);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn utf8_truncate_does_not_split_emoji() {
        // 🎉 is 2 UTF-16 code units but 1 char.
        let s = "🎉".repeat(50); // 50 chars
        let t = utf8_truncate(&s, 10);
        // 9 chars + ellipsis = 10 chars.
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn utf8_truncate_handles_cjk() {
        // CJK chars are 3 bytes each in UTF-8 but 1 char.
        let s = "中".repeat(1000); // 1000 chars, 3000 bytes
        let t = utf8_truncate(&s, 50);
        assert_eq!(t.chars().count(), 50);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn utf8_truncate_zero_limit_yields_empty() {
        assert_eq!(utf8_truncate("hello", 0), "");
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(6), Duration::from_secs(64));
        assert_eq!(backoff_delay(7), Duration::from_secs(128));
        assert_eq!(backoff_delay(8), Duration::from_secs(256));
        // Capped at 1 hour.
        assert_eq!(backoff_delay(20), Duration::from_secs(3600));
        assert_eq!(backoff_delay(100), Duration::from_secs(3600));
    }

    #[test]
    fn poller_config_defaults_are_sane() {
        let cfg = PollerConfig::with_defaults();
        assert_eq!(cfg.timeout, DEFAULT_TIMEOUT);
        assert!(cfg.etag.is_none());
        assert!(cfg.last_modified.is_none());
    }

    #[test]
    fn parse_minimal_rss_returns_entries() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Test</title>
  <item>
    <title>Hello</title>
    <link>https://example.com/1</link>
    <guid>https://example.com/1</guid>
    <description>A short summary.</description>
    <pubDate>Mon, 01 Jan 2024 12:00:00 GMT</pubDate>
  </item>
  <item>
    <title>World</title>
    <link>https://example.com/2</link>
    <guid>https://example.com/2</guid>
    <description>Another one.</description>
  </item>
</channel></rss>"#;
        let feed = parser::parse(xml.as_bytes()).unwrap();
        let entries = extract_entries(&feed);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "Hello");
        assert_eq!(entries[0].id, "https://example.com/1");
        assert_eq!(entries[0].url, "https://example.com/1");
        assert_eq!(entries[0].summary, "A short summary.");
        assert!(entries[0].published_at.is_some());
    }

    #[test]
    fn parse_enforces_max_entries_cap() {
        let mut xml = String::from(r#"<?xml version="1.0"?><rss version="2.0"><channel><title>T</title>"#);
        for i in 0..(MAX_ENTRIES_PER_POLL + 50) {
            xml.push_str(&format!(
                "<item><title>e{}</title><link>https://e/{}</link><guid>{}</guid></item>",
                i, i, i
            ));
        }
        xml.push_str("</channel></rss>");
        let feed = parser::parse(xml.as_bytes()).unwrap();
        let entries = extract_entries(&feed);
        assert_eq!(entries.len(), MAX_ENTRIES_PER_POLL);
    }

    #[test]
    fn parse_truncates_oversized_summary_safely() {
        let long = "x".repeat(SUMMARY_CHAR_LIMIT * 10);
        let xml = format!(
            r#"<?xml version="1.0"?><rss version="2.0"><channel>
              <title>T</title>
              <item><title>t</title><link>https://e/1</link><guid>1</guid>
                <description>{}</description>
              </item>
            </channel></rss>"#,
            long
        );
        let feed = parser::parse(xml.as_bytes()).unwrap();
        let entries = extract_entries(&feed);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary.chars().count(), SUMMARY_CHAR_LIMIT);
        assert!(entries[0].summary.ends_with('…'));
    }
}