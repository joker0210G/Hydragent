use std::sync::Arc;
use sqlx::{SqlitePool, FromRow};
use feed_rs::parser;
use reqwest::Client;
use chrono::Utc;
use crate::HeartbeatEngine;
use hydragent_model::router::ModelRouter;
use anyhow::{Result, Context};
use tracing::{info, warn, error};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BackfillPolicy {
    None,
    LastN,
    Last24h,
}

impl BackfillPolicy {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeywordRule {
    Include { phrase: String, weight: f32 },
    Phrase { phrase: String, weight: f32 },
    Exclude { phrase: String },
}

#[derive(Debug, Clone)]
pub struct FeedSpec {
    pub url: String,
    pub name: String,
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

#[derive(Debug, Clone, FromRow)]
pub struct FeedMonitor {
    pub url: String,
    pub name: String,
    pub keywords: String,             // Comma-separated
    pub digest_channel: String,       // target page_id / channel
    pub digest_cron: String,
    pub last_seen_id: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct FeedEntry {
    pub id: String,
    pub feed_url: String,
    pub title: String,
    pub summary: String,
    pub url: String,
    pub fetched_at: i64,
    pub digested: bool,
}

#[derive(Debug, Default)]
pub struct WorkIqStats {
    pub feeds_polled: usize,
    pub new_entries: usize,
    pub alerts_sent: usize,
}

pub struct WorkIqEngine {
    pool: SqlitePool,
    heartbeat: Arc<HeartbeatEngine>,
    http: Client,
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
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            model_router,
        })
    }

    pub async fn add_feed_with_spec(&self, spec: &FeedSpec) -> Result<()> {
        let keywords_csv = if let Some(ref rules) = spec.rules {
            let kws: Vec<String> = rules.iter().map(|r| match r {
                KeywordRule::Include { phrase, .. } => phrase.clone(),
                KeywordRule::Phrase { phrase, .. } => phrase.clone(),
                KeywordRule::Exclude { phrase } => phrase.clone(),
            }).collect();
            kws.join(", ")
        } else {
            spec.legacy_csv.clone().unwrap_or_default()
        };
        self.add_feed(
            &spec.url,
            &spec.name,
            &keywords_csv,
            &spec.digest_channel,
            &spec.digest_cron,
        ).await
    }

    /// Add a new feed to the monitor database
    pub async fn add_feed(
        &self,
        url: &str,
        name: &str,
        keywords: &str,
        digest_channel: &str,
        digest_cron: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO work_iq_feeds (url, name, keywords, digest_channel, digest_cron)
             VALUES (?, ?, ?, ?, ?)"
        )
        .bind(url)
        .bind(name)
        .bind(keywords)
        .bind(digest_channel)
        .bind(digest_cron)
        .execute(&self.pool)
        .await?;

        info!(url, name, "Work IQ: added feed");
        Ok(())
    }

    /// Run poll cycle for all active feeds in the database
    pub async fn run_poll_cycle(&self) -> Result<WorkIqStats> {
        let mut stats = WorkIqStats::default();

        let feeds = sqlx::query_as::<_, FeedMonitor>(
            "SELECT url, name, keywords, digest_channel, digest_cron, last_seen_id FROM work_iq_feeds"
        )
        .fetch_all(&self.pool)
        .await?;

        for mut feed in feeds {
            match self.poll_feed(&mut feed).await {
                Ok(new_entries) => {
                    stats.feeds_polled += 1;
                    stats.new_entries += new_entries.len();

                    // Check keyword alerts (immediate push)
                    let keywords: Vec<String> = feed.keywords
                        .split(',')
                        .map(|s| s.trim().to_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect();

                    for entry in &new_entries {
                        for keyword in &keywords {
                            let match_content = format!("{} {}", entry.title, entry.summary).to_lowercase();
                            if match_content.contains(keyword) {
                                let alert = format!(
                                    "🔔 **Work IQ Alert** — keyword `{}` matched in **{}**\n\n**{}**\n{}\n{}",
                                    keyword, feed.name, entry.title, entry.summary, entry.url
                                );
                                if let Err(e) = self.heartbeat.push(feed.digest_channel.clone(), format!("work_iq-alert-{}", entry.id), alert).await {
                                    error!("Work IQ: failed to push alert: {}", e);
                                } else {
                                    stats.alerts_sent += 1;
                                }
                                break; // alert once per entry
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Work IQ: failed to poll feed {}: {}", feed.url, e);
                }
            }
        }

        Ok(stats)
    }

    async fn poll_feed(&self, feed: &mut FeedMonitor) -> Result<Vec<FeedEntry>> {
        let response = self.http.get(&feed.url).send().await?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
        }

        let bytes = response.bytes().await?;
        let parsed_feed = parser::parse(bytes.as_ref())
            .map_err(|e| anyhow::anyhow!("Feed parse error: {}", e))?;

        let mut new_entries = Vec::new();
        let now = Utc::now().timestamp_millis();

        for entry in parsed_feed.entries {
            let entry_id = entry.id.clone();

            // Check if already seen
            if feed.last_seen_id.as_deref() == Some(&entry_id) {
                break;
            }

            let title = entry.title.map(|t| t.content).unwrap_or_default();
            let summary_raw = entry.summary
                .map(|s| s.content)
                .or_else(|| entry.content.as_ref().and_then(|c| c.body.clone()))
                .unwrap_or_default();

            // Truncate summary if too long
            let summary = if summary_raw.len() > 500 {
                format!("{}...", &summary_raw[..497])
            } else {
                summary_raw
            };

            let url = entry.links.first().map(|l| l.href.clone()).unwrap_or_default();

            let feed_entry = FeedEntry {
                id: entry_id,
                feed_url: feed.url.clone(),
                title,
                summary,
                url,
                fetched_at: now,
                digested: false,
            };

            // Persist feed entry to SQLite
            let _ = sqlx::query(
                "INSERT OR IGNORE INTO work_iq_entries (id, feed_url, title, summary, url, fetched_at, digested)
                 VALUES (?, ?, ?, ?, ?, ?, 0)"
            )
            .bind(&feed_entry.id)
            .bind(&feed_entry.feed_url)
            .bind(&feed_entry.title)
            .bind(&feed_entry.summary)
            .bind(&feed_entry.url)
            .bind(feed_entry.fetched_at)
            .execute(&self.pool)
            .await;

            new_entries.push(feed_entry);
        }

        // Update last seen id in memory and DB
        if let Some(first) = new_entries.first() {
            feed.last_seen_id = Some(first.id.clone());
            sqlx::query("UPDATE work_iq_feeds SET last_seen_id = ? WHERE url = ?")
                .bind(&first.id)
                .bind(&feed.url)
                .execute(&self.pool)
                .await?;
        }

        Ok(new_entries)
    }

    /// Generate and send digest for a specific feed URL to the target channel.
    pub async fn generate_and_send_digest(&self, feed_url: &str, target_channel: &str) -> Result<()> {
        let feed = sqlx::query_as::<_, FeedMonitor>(
            "SELECT url, name, keywords, digest_channel, digest_cron, last_seen_id FROM work_iq_feeds WHERE url = ?"
        )
        .bind(feed_url)
        .fetch_optional(&self.pool)
        .await?
        .context("Feed not found")?;

        let pending_entries = sqlx::query_as::<_, FeedEntry>(
            "SELECT id, feed_url, title, summary, url, fetched_at, digested
             FROM work_iq_entries WHERE feed_url = ? AND digested = 0 ORDER BY fetched_at DESC"
        )
        .bind(feed_url)
        .fetch_all(&self.pool)
        .await?;

        if pending_entries.is_empty() {
            let clean_msg = format!("📰 **{} Work IQ Digest**\n\nNo new entries since last digest.", feed.name);
            let _ = self.heartbeat.push(target_channel.to_string(), format!("work_iq-digest-empty-{}", Utc::now().timestamp()), clean_msg).await;
            return Ok(());
        }

        let mut entry_list = String::new();
        for entry in &pending_entries {
            entry_list.push_str(&format!("- **{}**: {} ({})\n", entry.title, entry.summary, entry.url));
        }

        let prompt = format!(
            "Summarize the following {} feed entries into a concise digest in 3-5 bullet points. \
             Make it structured and highly readable. Focus on actionable details.\n\nEntries:\n{}",
            feed.name, entry_list
        );

        let digest = self.model_router.generate_non_streaming(&prompt, None).await
            .unwrap_or_else(|e| format!("📰 **{} Work IQ Digest**\n\n(Error generating summary: {})", feed.name, e));

        let formatted_digest = format!("📰 **{} Work IQ Digest**\n\n{}", feed.name, digest);

        // Push digest
        self.heartbeat.push(target_channel.to_string(), format!("work_iq-digest-{}", Utc::now().timestamp()), formatted_digest).await?;

        // Mark entries as digested
        for entry in pending_entries {
            let _ = sqlx::query("UPDATE work_iq_entries SET digested = 1 WHERE id = ?")
                .bind(&entry.id)
                .execute(&self.pool)
                .await;
        }

        Ok(())
    }
}
