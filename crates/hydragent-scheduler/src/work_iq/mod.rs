//! Work IQ — Hydragent's background awareness engine.
//!
//! Polls RSS/Atom feeds, evaluates keyword rules, and pushes alerts &
//! digests to a target channel via the [`HeartbeatEngine`].
//!
//! Public surface kept stable:
//! - [`WorkIqEngine::new`]
//! - [`WorkIqEngine::add_feed`]
//! - [`WorkIqEngine::run_poll_cycle`]
//! - [`WorkIqEngine::generate_and_send_digest`]
//!
//! New additions:
//! - [`WorkIqEngine::list_feeds`], [`WorkIqEngine::pause_feed`], [`WorkIqEngine::resume_feed`]
//! - [`WorkIqEngine::cleanup_old_entries`] (TTL sweep)
//! - [`WorkIqEngine::generate_multi_feed_digest`]
//! - [`WorkIqEngine::generate_and_send_digest`] is **soft-deprecated** for single-feed
//!   use; the multi-feed path is preferred for daily briefs.
//!
//! Internals are split across:
//! - [`match_engine`] — keyword rule evaluation.
//! - [`poller`]      — HTTP fetch, ETag/Last-Modified caching, backoff.

pub mod match_engine;
pub mod poller;

use crate::HeartbeatEngine;
use anyhow::{Context, Result};
use chrono::Utc;
use hydragent_model::router::ModelRouter;
use poller::{backoff_delay, FeedPoller, ParsedEntry, PollOutcome, PollResult, PollerConfig, MAX_CONSECUTIVE_FAILURES};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

pub use match_engine::{KeywordRule, MatchResult};

// Re-export for callers that imported `WorkIqStats` etc. directly.
pub use poller::{DEFAULT_TIMEOUT as POLLER_DEFAULT_TIMEOUT, SUMMARY_CHAR_LIMIT};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// How many entries to backfill when first adding a feed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BackfillPolicy {
    /// Do not fetch any existing entries — start watching from now on.
    None,
    /// Fetch the most recent N entries (set via `backfill_n`).
    LastN,
    /// Fetch everything published in the last 24 hours.
    Last24h,
}

impl BackfillPolicy {
    pub fn from_db(s: &str) -> Self {
        match s {
            "last_n" => Self::LastN,
            "last_24h" => Self::Last24h,
            _ => Self::None,
        }
    }
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LastN => "last_n",
            Self::Last24h => "last_24h",
        }
    }
}

impl Default for BackfillPolicy {
    fn default() -> Self {
        Self::LastN
    }
}

/// One row from the `work_iq_feeds` table.
#[derive(Debug, Clone, FromRow)]
pub struct FeedMonitor {
    pub url: String,
    pub name: String,
    pub keywords: String,
    pub digest_channel: String,
    pub digest_cron: String,
    pub last_seen_id: Option<String>,
    pub keywords_json: Option<String>,
    pub backfill_policy: String,
    pub backfill_n: i64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub enabled: i64,
    pub consecutive_failures: i64,
    pub last_polled_at: Option<i64>,
    pub last_seen_published_at: Option<i64>,
}

impl FeedMonitor {
    /// Decoded keyword rules. Falls back to the legacy CSV `keywords` column
    /// when `keywords_json` is empty.
    pub fn decoded_rules(&self) -> Vec<KeywordRule> {
        if let Some(json) = &self.keywords_json {
            if !json.is_empty() {
                if let Ok(rules) = KeywordRule::list_from_json(json) {
                    return rules;
                }
                // Corrupt JSON — fall through to legacy.
                warn!(
                    feed = %self.url,
                    "Work IQ: keywords_json invalid, falling back to legacy CSV column"
                );
            }
        }
        KeywordRule::from_legacy_csv(&self.keywords)
    }

    pub fn backfill_policy_enum(&self) -> BackfillPolicy {
        BackfillPolicy::from_db(&self.backfill_policy)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled != 0
    }
}

/// One row from the `work_iq_entries` table.
#[derive(Debug, Clone, FromRow)]
pub struct FeedEntry {
    pub id: String,
    pub feed_url: String,
    pub title: String,
    pub summary: String,
    pub url: String,
    pub fetched_at: i64,
    pub published_at: Option<i64>,
    pub digested: bool,
    pub score: f64,
}

/// Summary of one poll cycle.
#[derive(Debug, Default, Clone, Serialize)]
pub struct WorkIqStats {
    pub feeds_polled: usize,
    pub feeds_skipped_disabled: usize,
    pub feeds_skipped_backoff: usize,
    pub feeds_failed: usize,
    pub new_entries: usize,
    pub alerts_sent: usize,
    pub digests_triggered: usize,
}

/// Inputs for [`WorkIqEngine::add_feed`]. Use this for full control; the
/// legacy 5-argument form is preserved for backwards compatibility.
#[derive(Debug, Clone)]
pub struct FeedSpec {
    pub url: String,
    pub name: String,
    /// Either `rules` or `legacy_csv` must be set.
    pub rules: Option<Vec<KeywordRule>>,
    pub legacy_csv: Option<String>,
    pub digest_channel: String,
    pub digest_cron: String,
    pub backfill_policy: BackfillPolicy,
    pub backfill_n: i64,
}

impl FeedSpec {
    pub fn new(
        url: impl Into<String>,
        name: impl Into<String>,
        keywords_csv: impl Into<String>,
        digest_channel: impl Into<String>,
        digest_cron: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            name: name.into(),
            rules: None,
            legacy_csv: Some(keywords_csv.into()),
            digest_channel: digest_channel.into(),
            digest_cron: digest_cron.into(),
            backfill_policy: BackfillPolicy::default(),
            backfill_n: 10,
        }
    }

    pub fn with_rules(mut self, rules: Vec<KeywordRule>) -> Self {
        self.rules = Some(rules);
        self.legacy_csv = None;
        self
    }

    pub fn with_backfill(mut self, policy: BackfillPolicy, n: i64) -> Self {
        self.backfill_policy = policy;
        self.backfill_n = n;
        self
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Bounded poll concurrency. With 4 in flight, 20 feeds each taking ~3s
/// finish in roughly 15s instead of 60s.
const POLL_CONCURRENCY: usize = 4;
/// Default Telegram message length limit. Used for chunking long digests.
const TELEGRAM_MSG_LIMIT: usize = 4096;
/// TTL for ingested entries (default: 30 days).
const DEFAULT_ENTRY_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub struct WorkIqEngine {
    pool: SqlitePool,
    heartbeat: Arc<HeartbeatEngine>,
    http: FeedPoller,
    model_router: Arc<ModelRouter>,
}

impl WorkIqEngine {
    pub fn new(
        pool: SqlitePool,
        heartbeat: Arc<HeartbeatEngine>,
        model_router: Arc<ModelRouter>,
    ) -> Arc<Self> {
        Arc::new(Self {
            pool,
            heartbeat,
            http: FeedPoller::default(),
            model_router,
        })
    }

    // ----- Add / update feeds ------------------------------------------------

    /// Legacy 5-argument form. Internally builds a [`FeedSpec`] with default
    /// backfill (`LastN`, 10) and `Include`-rules from the CSV.
    pub async fn add_feed(
        &self,
        url: &str,
        name: &str,
        keywords: &str,
        digest_channel: &str,
        digest_cron: &str,
    ) -> Result<()> {
        self.add_feed_with_spec(&FeedSpec::new(
            url,
            name,
            keywords,
            digest_channel,
            digest_cron,
        ))
        .await
    }

    /// Full-fidelity feed registration. Use this from the tool layer.
    pub async fn add_feed_with_spec(&self, spec: &FeedSpec) -> Result<()> {
        let keywords_json = match &spec.rules {
            Some(rules) => Some(KeywordRule::list_to_json(rules)?),
            None => None,
        };
        let legacy_csv = spec.legacy_csv.clone().unwrap_or_default();

        sqlx::query(
            "INSERT INTO work_iq_feeds (
                url, name, keywords, keywords_json,
                digest_channel, digest_cron,
                backfill_policy, backfill_n,
                etag, last_modified, enabled,
                consecutive_failures, last_polled_at, last_seen_published_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, 1, 0, NULL, NULL)
             ON CONFLICT(url) DO UPDATE SET
                name                   = excluded.name,
                keywords               = excluded.keywords,
                keywords_json          = excluded.keywords_json,
                digest_channel         = excluded.digest_channel,
                digest_cron            = excluded.digest_cron,
                backfill_policy        = excluded.backfill_policy,
                backfill_n             = excluded.backfill_n,
                -- On re-subscribe, reset failure counters and cached headers
                consecutive_failures   = 0,
                etag                   = NULL,
                last_modified          = NULL,
                enabled                = 1",
        )
        .bind(&spec.url)
        .bind(&spec.name)
        .bind(&legacy_csv)
        .bind(keywords_json)
        .bind(&spec.digest_channel)
        .bind(&spec.digest_cron)
        .bind(spec.backfill_policy.as_db())
        .bind(spec.backfill_n)
        .execute(&self.pool)
        .await?;

        info!(url = %spec.url, name = %spec.name, "Work IQ: added/updated feed");
        Ok(())
    }

    /// Pause a feed (set `enabled = 0`).
    pub async fn pause_feed(&self, url: &str) -> Result<()> {
        sqlx::query("UPDATE work_iq_feeds SET enabled = 0 WHERE url = ?")
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Resume a previously paused feed.
    pub async fn resume_feed(&self, url: &str) -> Result<()> {
        sqlx::query("UPDATE work_iq_feeds SET enabled = 1 WHERE url = ?")
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// List all feeds (for diagnostics / the Control UI).
    pub async fn list_feeds(&self) -> Result<Vec<FeedMonitor>> {
        let rows = sqlx::query_as::<_, FeedMonitor>(
            "SELECT url, name, keywords, digest_channel, digest_cron, last_seen_id,
                    keywords_json, backfill_policy, backfill_n, etag, last_modified,
                    enabled, consecutive_failures, last_polled_at, last_seen_published_at
             FROM work_iq_feeds
             ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // ----- Polling -----------------------------------------------------------

    /// Run one poll cycle across every enabled feed. Bounded concurrency.
    pub async fn run_poll_cycle(&self) -> Result<WorkIqStats> {
        let feeds = self.list_feeds().await?;
        let mut stats = WorkIqStats::default();
        let now = Utc::now().timestamp_millis();

        // Filter + partition by readiness.
        let mut ready = Vec::new();
        for feed in feeds {
            if !feed.is_enabled() {
                stats.feeds_skipped_disabled += 1;
                continue;
            }
            if let Some(last) = feed.last_polled_at {
                let needed = backoff_delay(feed.consecutive_failures.max(0) as u32);
                if now - last < needed.as_millis() as i64 {
                    stats.feeds_skipped_backoff += 1;
                    continue;
                }
            }
            ready.push(feed);
        }

        // Bounded-concurrency poll: poll at most POLL_CONCURRENCY feeds at once.
        let results: Vec<(FeedMonitor, Result<PollResult>)> =
            futures_util::stream::iter(ready.into_iter().map(|feed| {
                let http = self.http.clone();
                let pool = self.pool.clone();
                let heartbeat = self.heartbeat.clone();
                async move {
                    let cfg = PollerConfig {
                        etag: feed.etag.clone(),
                        last_modified: feed.last_modified.clone(),
                        timeout: DEFAULT_TIMEOUT,
                    };
                    let is_first = feed.last_polled_at.is_none();
                    let res = http.fetch(&feed.url, &cfg, is_first).await;
                    let r = match res {
                        Ok(mut pr) => {
                            pr.new_failure = false;
                            // Process the entries (persist + alert).
                            if let Err(e) = handle_poll_result(
                                &pool, &heartbeat, &feed, &pr, is_first,
                            )
                            .await
                            {
                                Err(e)
                            } else {
                                Ok(pr)
                            }
                        }
                        Err(e) => Err(e),
                    };
                    (feed, r)
                }
            }))
            .buffer_unordered(POLL_CONCURRENCY)
            .collect()
            .await;

        for (feed, result) in results {
            match result {
                Ok(pr) => {
                    stats.feeds_polled += 1;
                    let added = update_feed_state_after_poll(&self.pool, &feed, &pr, now).await?;
                    stats.new_entries += added.0;
                    stats.alerts_sent += added.1;
                }
                Err(e) => {
                    stats.feeds_failed += 1;
                    let _ = record_failure(&self.pool, &feed).await;
                    warn!("Work IQ: failed to poll feed {}: {}", feed.url, e);
                }
            }
        }

        Ok(stats)
    }

    // ----- Digest generation -------------------------------------------------

    /// Generate and send a digest for a single feed URL. (Kept for the cron
    /// `work_iq_digest` task handler.)
    pub async fn generate_and_send_digest(&self, feed_url: &str, target_channel: &str) -> Result<()> {
        let feed = sqlx::query_as::<_, FeedMonitor>(
            "SELECT url, name, keywords, digest_channel, digest_cron, last_seen_id,
                    keywords_json, backfill_policy, backfill_n, etag, last_modified,
                    enabled, consecutive_failures, last_polled_at, last_seen_published_at
             FROM work_iq_feeds WHERE url = ?",
        )
        .bind(feed_url)
        .fetch_optional(&self.pool)
        .await?
        .context("Feed not found")?;

        let entries = load_pending_entries(&self.pool, feed_url).await?;
        let body = compose_single_feed_digest(&self.model_router, &feed, &entries).await;
        push_chunked(&self.heartbeat, target_channel, body).await?;
        mark_digested(&self.pool, &entries).await;
        Ok(())
    }

    /// Generate a combined digest across **all** enabled feeds with pending
    /// entries. Used for the daily cross-feed brief.
    pub async fn generate_multi_feed_digest(&self, target_channel: &str) -> Result<WorkIqStats> {
        let feeds = self.list_feeds().await?;
        let mut stats = WorkIqStats::default();

        for feed in feeds {
            if !feed.is_enabled() {
                continue;
            }
            let entries = load_pending_entries(&self.pool, &feed.url).await?;
            if entries.is_empty() {
                continue;
            }
            let body =
                compose_single_feed_digest(&self.model_router, &feed, &entries).await;
            if let Err(e) = push_chunked(&self.heartbeat, target_channel, body).await {
                error!("Work IQ: digest push failed for {}: {}", feed.url, e);
                continue;
            }
            mark_digested(&self.pool, &entries).await;
            stats.digests_triggered += 1;
        }

        if stats.digests_triggered == 0 {
            let msg = "📰 **Work IQ Daily Brief**\n\nNo new entries across your feeds.".to_string();
            let _ = self
                .heartbeat
                .push(
                    target_channel.to_string(),
                    format!("work_iq-digest-empty-{}", Utc::now().timestamp()),
                    msg,
                )
                .await;
        }

        Ok(stats)
    }

    // ----- Maintenance -------------------------------------------------------

    /// Delete entries older than the TTL. Returns the number deleted.
    pub async fn cleanup_old_entries(&self, ttl: Option<Duration>) -> Result<u64> {
        let ttl_ms = ttl
            .map(|d| d.as_millis() as i64)
            .unwrap_or(DEFAULT_ENTRY_TTL_MS);
        let cutoff = Utc::now().timestamp_millis() - ttl_ms;
        let res = sqlx::query("DELETE FROM work_iq_entries WHERE fetched_at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}

// ---------------------------------------------------------------------------
// Helpers (free functions so tests can target them without an engine)
// ---------------------------------------------------------------------------

async fn handle_poll_result(
    pool: &SqlitePool,
    heartbeat: &Arc<HeartbeatEngine>,
    feed: &FeedMonitor,
    pr: &PollResult,
    is_first_poll: bool,
) -> Result<(usize, usize)> {
    let mut new_count = 0usize;
    let mut alert_count = 0usize;

    match &pr.outcome {
        PollOutcome::NotModified => return Ok((0, 0)),
        PollOutcome::FirstFetch(entries) => {
            // Apply backfill policy: trim the entry list before persistence.
            let kept = apply_backfill(feed, entries.clone());
            for entry in &kept {
                if persist_entry(pool, feed, entry, /*score*/ 0.0).await? {
                    new_count += 1;
                }
            }
        }
        PollOutcome::Updated(entries) => {
            let rules = feed.decoded_rules();
            for entry in entries {
                let result = match_engine::evaluate(&rules, &format!("{} {}", entry.title, entry.summary));
                if result.excluded {
                    continue; // Don't store vetoed entries.
                }
                let inserted = persist_entry(pool, feed, entry, result.score as f64).await?;
                if !inserted {
                    continue; // Already in DB.
                }
                new_count += 1;
                if result.matched_positive {
                    let msg = format!(
                        "🔔 **Work IQ Alert** — score {:.2} on **{}**\n\n**{}**\n{}\n{}",
                        result.score, feed.name, entry.title, entry.summary, entry.url
                    );
                    let key = format!("work_iq-alert-{}-{}", feed.url, entry.id);
                    if let Err(e) = heartbeat.push(feed.digest_channel.clone(), key, msg).await {
                        error!("Work IQ: failed to push alert: {}", e);
                    } else {
                        alert_count += 1;
                    }
                }
            }
        }
    }

    let _ = is_first_poll; // Reserved for future use (e.g., welcome message).
    Ok((new_count, alert_count))
}

fn apply_backfill(feed: &FeedMonitor, mut entries: Vec<ParsedEntry>) -> Vec<ParsedEntry> {
    match feed.backfill_policy_enum() {
        BackfillPolicy::None => Vec::new(),
        BackfillPolicy::LastN => {
            entries.truncate(feed.backfill_n.max(0) as usize);
            entries
        }
        BackfillPolicy::Last24h => {
            let cutoff = Utc::now().timestamp_millis() - 24 * 60 * 60 * 1000;
            entries.retain(|e| {
                e.published_at
                    .map(|ts| ts >= cutoff)
                    .unwrap_or(false)
            });
            entries
        }
    }
}

async fn persist_entry(
    pool: &SqlitePool,
    feed: &FeedMonitor,
    entry: &ParsedEntry,
    score: f64,
) -> Result<bool> {
    let res = sqlx::query(
        "INSERT OR IGNORE INTO work_iq_entries
            (id, feed_url, title, summary, url, fetched_at, published_at, digested, score)
         VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(&entry.id)
    .bind(&feed.url)
    .bind(&entry.title)
    .bind(&entry.summary)
    .bind(&entry.url)
    .bind(Utc::now().timestamp_millis())
    .bind(entry.published_at)
    .bind(score)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

async fn update_feed_state_after_poll(
    pool: &SqlitePool,
    feed: &FeedMonitor,
    pr: &PollResult,
    now_ms: i64,
) -> Result<(usize, usize)> {
    // Recompute new/alerts by replaying through handle_poll_result is wasteful;
    // instead we just update state here and let handle_poll_result own counts.
    // (Counts are returned by handle_poll_result; this function only writes
    // the persistent feed state.)
    let new_last_seen_published_at = match &pr.outcome {
        PollOutcome::Updated(entries) | PollOutcome::FirstFetch(entries) => entries
            .iter()
            .filter_map(|e| e.published_at)
            .max()
            .or(feed.last_seen_published_at),
        PollOutcome::NotModified => feed.last_seen_published_at,
    };

    sqlx::query(
        "UPDATE work_iq_feeds SET
            etag                       = ?,
            last_modified              = ?,
            last_polled_at             = ?,
            last_seen_published_at     = ?,
            consecutive_failures       = 0
         WHERE url = ?",
    )
    .bind(&pr.etag)
    .bind(&pr.last_modified)
    .bind(now_ms)
    .bind(new_last_seen_published_at)
    .bind(&feed.url)
    .execute(pool)
    .await?;

    Ok((0, 0))
}

async fn record_failure(pool: &SqlitePool, feed: &FeedMonitor) -> Result<()> {
    sqlx::query(
        "UPDATE work_iq_feeds SET
            consecutive_failures = consecutive_failures + 1,
            last_polled_at       = ?,
            enabled              = CASE WHEN consecutive_failures + 1 >= ? THEN 0 ELSE enabled END
         WHERE url = ?",
    )
    .bind(Utc::now().timestamp_millis())
    .bind(MAX_CONSECUTIVE_FAILURES as i64)
    .bind(&feed.url)
    .execute(pool)
    .await?;
    Ok(())
}

async fn load_pending_entries(pool: &SqlitePool, feed_url: &str) -> Result<Vec<FeedEntry>> {
    let rows = sqlx::query_as::<_, FeedEntry>(
        "SELECT id, feed_url, title, summary, url, fetched_at, published_at, digested, score
         FROM work_iq_entries
         WHERE feed_url = ? AND digested = 0
         ORDER BY COALESCE(published_at, fetched_at) DESC",
    )
    .bind(feed_url)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn mark_digested(pool: &SqlitePool, entries: &[FeedEntry]) {
    for entry in entries {
        let _ = sqlx::query("UPDATE work_iq_entries SET digested = 1 WHERE id = ?")
            .bind(&entry.id)
            .execute(pool)
            .await;
    }
}

async fn compose_single_feed_digest(
    model_router: &Arc<ModelRouter>,
    feed: &FeedMonitor,
    entries: &[FeedEntry],
) -> String {
    if entries.is_empty() {
        return format!("📰 **{} Work IQ Digest**\n\nNo new entries since last digest.", feed.name);
    }

    let mut entry_list = String::new();
    for entry in entries.iter().take(50) {
        entry_list.push_str(&format!(
            "- **{}**: {} ({})\n",
            entry.title, entry.summary, entry.url
        ));
    }

    let prompt = format!(
        "Summarize the following {} feed entries into a concise digest in 3-5 bullet points. \
         Make it structured and highly readable. Focus on actionable details.\n\nEntries:\n{}",
        feed.name, entry_list
    );

    let digest = model_router
        .generate_non_streaming(&prompt, None)
        .await
        .unwrap_or_else(|e| {
            format!("(Error generating summary: {})", e)
        });

    format!("📰 **{} Work IQ Digest**\n\n{}", feed.name, digest)
}

/// Push a (potentially long) message to a channel, chunking to respect
/// per-channel limits. Telegram = 4096 chars; we use that as a conservative
/// default since it's the most restrictive common case.
async fn push_chunked(
    heartbeat: &Arc<HeartbeatEngine>,
    channel: &str,
    body: String,
) -> Result<()> {
    if body.len() <= TELEGRAM_MSG_LIMIT {
        let key = format!("work_iq-digest-{}", Utc::now().timestamp());
        heartbeat.push(channel.to_string(), key, body).await?;
        return Ok(());
    }
    let chunks = chunk_message(&body, TELEGRAM_MSG_LIMIT);
    for (i, chunk) in chunks.iter().enumerate() {
        let key = format!("work_iq-digest-{}-{}", Utc::now().timestamp(), i);
        if let Err(e) = heartbeat.push(channel.to_string(), key, chunk.clone()).await {
            error!("Work IQ: digest chunk {} failed: {}", i, e);
        }
    }
    Ok(())
}

/// Split `body` into chunks of at most `limit` characters each, preferring
/// to break on paragraph (`\n\n`) and line (`\n`) boundaries.
pub fn chunk_message(body: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return vec![body.to_string()];
    }
    if body.chars().count() <= limit {
        return vec![body.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for paragraph in body.split("\n\n") {
        let paragraph_with_sep = format!("{}\n\n", paragraph);
        if paragraph_with_sep.chars().count() > limit {
            // Single paragraph too long — flush what we have and split it on lines.
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            for line in paragraph.split('\n') {
                let line_with_sep = format!("{}\n", line);
                if line_with_sep.chars().count() > limit {
                    // Single line too long — hard truncate.
                    if !current.is_empty() {
                        chunks.push(std::mem::take(&mut current));
                    }
                    chunks.push(utf8_truncate_to_chars(&line_with_sep, limit));
                } else if current.chars().count() + line_with_sep.chars().count() > limit {
                    chunks.push(std::mem::take(&mut current));
                    current.push_str(&line_with_sep);
                } else {
                    current.push_str(&line_with_sep);
                }
            }
            continue;
        }

        if current.chars().count() + paragraph_with_sep.chars().count() > limit {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(&paragraph_with_sep);
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn utf8_truncate_to_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_message_short_input_single_chunk() {
        let chunks = chunk_message("hello world", 100);
        assert_eq!(chunks, vec!["hello world".to_string()]);
    }

    #[test]
    fn chunk_message_zero_limit_returns_whole_input() {
        let chunks = chunk_message("hello", 0);
        assert_eq!(chunks, vec!["hello".to_string()]);
    }

    #[test]
    fn chunk_message_splits_on_paragraph_boundary() {
        let body = "AAAA\n\nBBBB\n\nCCCC";
        let chunks = chunk_message(body, 5);
        // "AAAA\n\n" is 6 chars > 5, so we flush and start fresh.
        assert!(chunks.len() >= 2);
        // No chunk exceeds the limit.
        for c in &chunks {
            assert!(c.chars().count() <= 5, "chunk too long: {:?}", c);
        }
        // All content preserved.
        let joined: String = chunks.join("");
        assert_eq!(joined, body);
    }

    #[test]
    fn chunk_message_splits_long_paragraph_on_lines() {
        let body = "line1\nline2\nline3\nline4";
        let chunks = chunk_message(body, 8);
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.chars().count() <= 8, "chunk too long: {:?}", c);
        }
        let joined: String = chunks.iter().flat_map(|s| s.chars()).collect();
        // Strip newlines to compare content.
        let stripped_joined: String = joined.chars().filter(|c| *c != '\n').collect();
        let stripped_body: String = body.chars().filter(|c| *c != '\n').collect();
        assert_eq!(stripped_joined, stripped_body);
    }

    #[test]
    fn chunk_message_handles_oversized_single_line() {
        let body = "x".repeat(5000);
        let chunks = chunk_message(&body, 100);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chars().count() <= 100);
        }
    }

    #[test]
    fn backfill_policy_round_trip() {
        for p in [BackfillPolicy::None, BackfillPolicy::LastN, BackfillPolicy::Last24h] {
            assert_eq!(BackfillPolicy::from_db(p.as_db()), p);
        }
    }

    #[test]
    fn backfill_policy_unknown_defaults_to_none() {
        assert_eq!(BackfillPolicy::from_db("bogus"), BackfillPolicy::None);
    }

    #[test]
    fn apply_backfill_none_returns_empty() {
        let feed = FeedMonitor {
            url: "u".into(), name: "n".into(), keywords: String::new(),
            digest_channel: "c".into(), digest_cron: "0 0 * * *".into(),
            last_seen_id: None, keywords_json: None,
            backfill_policy: "none".into(), backfill_n: 10,
            etag: None, last_modified: None, enabled: 1,
            consecutive_failures: 0, last_polled_at: None,
            last_seen_published_at: None,
        };
        let entries: Vec<ParsedEntry> = (0..5).map(|i| ParsedEntry {
            id: format!("e{}", i), title: "t".into(), summary: "s".into(),
            url: "u".into(), published_at: None,
        }).collect();
        assert!(apply_backfill(&feed, entries).is_empty());
    }

    #[test]
    fn apply_backfill_last_n_truncates() {
        let feed = FeedMonitor {
            url: "u".into(), name: "n".into(), keywords: String::new(),
            digest_channel: "c".into(), digest_cron: "0 0 * * *".into(),
            last_seen_id: None, keywords_json: None,
            backfill_policy: "last_n".into(), backfill_n: 3,
            etag: None, last_modified: None, enabled: 1,
            consecutive_failures: 0, last_polled_at: None,
            last_seen_published_at: None,
        };
        let entries: Vec<ParsedEntry> = (0..10).map(|i| ParsedEntry {
            id: format!("e{}", i), title: "t".into(), summary: "s".into(),
            url: "u".into(), published_at: None,
        }).collect();
        let kept = apply_backfill(&feed, entries);
        assert_eq!(kept.len(), 3);
    }

    #[test]
    fn apply_backfill_last_24h_filters_by_published_at() {
        let now = Utc::now().timestamp_millis();
        let feed = FeedMonitor {
            url: "u".into(), name: "n".into(), keywords: String::new(),
            digest_channel: "c".into(), digest_cron: "0 0 * * *".into(),
            last_seen_id: None, keywords_json: None,
            backfill_policy: "last_24h".into(), backfill_n: 10,
            etag: None, last_modified: None, enabled: 1,
            consecutive_failures: 0, last_polled_at: None,
            last_seen_published_at: None,
        };
        let mut entries = Vec::new();
        entries.push(ParsedEntry {
            id: "old".into(), title: "t".into(), summary: "s".into(), url: "u".into(),
            published_at: Some(now - 48 * 60 * 60 * 1000), // 2 days ago
        });
        entries.push(ParsedEntry {
            id: "new".into(), title: "t".into(), summary: "s".into(), url: "u".into(),
            published_at: Some(now - 60 * 60 * 1000), // 1 hour ago
        });
        entries.push(ParsedEntry {
            id: "unknown".into(), title: "t".into(), summary: "s".into(), url: "u".into(),
            published_at: None,
        });
        let kept = apply_backfill(&feed, entries);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "new");
    }

    #[test]
    fn feed_monitor_decoded_rules_prefers_json_over_csv() {
        let feed = FeedMonitor {
            url: "u".into(), name: "n".into(),
            keywords: "legacy, csv".into(),
            digest_channel: "c".into(), digest_cron: "0 0 * * *".into(),
            last_seen_id: None,
            keywords_json: Some(
                r#"[{"kind":"include","phrase":"json","weight":2.0}]"#.into(),
            ),
            backfill_policy: "none".into(), backfill_n: 10,
            etag: None, last_modified: None, enabled: 1,
            consecutive_failures: 0, last_polled_at: None,
            last_seen_published_at: None,
        };
        let rules = feed.decoded_rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].phrase(), "json");
    }

    #[test]
    fn feed_monitor_decoded_rules_falls_back_to_csv() {
        let feed = FeedMonitor {
            url: "u".into(), name: "n".into(),
            keywords: "rust, tokio".into(),
            digest_channel: "c".into(), digest_cron: "0 0 * * *".into(),
            last_seen_id: None, keywords_json: None,
            backfill_policy: "none".into(), backfill_n: 10,
            etag: None, last_modified: None, enabled: 1,
            consecutive_failures: 0, last_polled_at: None,
            last_seen_published_at: None,
        };
        let rules = feed.decoded_rules();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].phrase(), "rust");
        assert_eq!(rules[1].phrase(), "tokio");
    }

    #[test]
    fn feed_monitor_decoded_rules_handles_corrupt_json() {
        let feed = FeedMonitor {
            url: "u".into(), name: "n".into(),
            keywords: "fallback".into(),
            digest_channel: "c".into(), digest_cron: "0 0 * * *".into(),
            last_seen_id: None,
            keywords_json: Some("{not valid json}".into()),
            backfill_policy: "none".into(), backfill_n: 10,
            etag: None, last_modified: None, enabled: 1,
            consecutive_failures: 0, last_polled_at: None,
            last_seen_published_at: None,
        };
        let rules = feed.decoded_rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].phrase(), "fallback");
    }

    #[test]
    fn feed_spec_with_rules_sets_legacy_csv_to_none() {
        let spec = FeedSpec::new("u", "n", "ignored", "c", "0 0 * * *")
            .with_rules(vec![KeywordRule::Include {
                phrase: "x".into(), weight: 1.0,
            }]);
        assert!(spec.rules.is_some());
        assert!(spec.legacy_csv.is_none());
    }
}