//! `rss_subscribe` tool — exposed to the LLM so it can add feeds to Work IQ.
//!
//! Accepts two forms of keyword input:
//! - `keywords`: list of plain strings → turned into `Include` rules (weight 1.0).
//! - `keyword_rules`: structured objects with `kind` ∈ {`include`, `phrase`, `exclude`},
//!   explicit `weight`, and either single tokens or multi-word phrases.

use async_trait::async_trait;
use hydragent_scheduler::work_iq::{BackfillPolicy, FeedSpec, KeywordRule};
use hydragent_types::{PermissionTier, ToolResult, ToolStatus};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum KeywordsInput {
    /// Old form: list of plain strings.
    Plain(Vec<String>),
    /// New form: list of structured rule objects.
    Structured(Vec<RuleInput>),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RuleInput {
    Include { phrase: String, weight: Option<f32> },
    Phrase { phrase: String, weight: Option<f32> },
    Exclude { phrase: String },
}

impl From<RuleInput> for KeywordRule {
    fn from(r: RuleInput) -> Self {
        match r {
            RuleInput::Include { phrase, weight } => KeywordRule::Include {
                phrase,
                weight: weight.unwrap_or(1.0),
            },
            RuleInput::Phrase { phrase, weight } => KeywordRule::Phrase {
                phrase,
                weight: weight.unwrap_or(1.0),
            },
            RuleInput::Exclude { phrase } => KeywordRule::Exclude { phrase },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RssSubscribeParams {
    url: String,
    name: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    keyword_rules: Vec<RuleInput>,
    #[serde(default)]
    backfill_policy: Option<String>,
    #[serde(default)]
    backfill_n: Option<i64>,
    #[serde(default = "default_digest_channel")]
    digest_channel: String,
    #[serde(default = "default_digest_cron")]
    digest_cron: String,
}

fn default_digest_channel() -> String {
    "current".to_string()
}

fn default_digest_cron() -> String {
    "0 8 * * *".to_string()
}

pub struct RssSubscribeTool {
    subscribe_fn: Arc<
        dyn Fn(FeedSpec) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync,
    >,
}

impl RssSubscribeTool {
    pub fn new<F>(subscribe_fn: F) -> Self
    where
        F: Fn(FeedSpec) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            subscribe_fn: Arc::new(subscribe_fn),
        }
    }
}

#[async_trait]
impl crate::tool_trait::Tool for RssSubscribeTool {
    fn name(&self) -> &str {
        "rss_subscribe"
    }

    fn description(&self) -> &str {
        "Add an RSS or Atom feed to the Work IQ monitor. Work IQ polls the feed periodically, evaluates keyword rules, and pushes an immediate alert on Telegram/Discord/etc. when an entry matches. A daily digest is also scheduled. Supported keyword rules: include (substring + weight), phrase (exact multi-word + weight), exclude (veto)."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "url":     { "type": "string", "description": "RSS or Atom feed URL" },
                "name":    { "type": "string", "description": "Friendly name for this feed (e.g., 'Rust Blog')" },
                "keywords": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Simple keywords. Each becomes an Include rule with weight 1.0. Use keyword_rules for more control."
                },
                "keyword_rules": {
                    "type": "array",
                    "description": "Structured keyword rules. Use when you need phrases or exclude lists.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "enum": ["include", "phrase", "exclude"] },
                            "phrase": { "type": "string" },
                            "weight": { "type": "number" }
                        },
                        "required": ["kind", "phrase"]
                    }
                },
                "backfill_policy": {
                    "type": "string",
                    "enum": ["none", "last_n", "last_24h"],
                    "description": "What to do with existing entries when this feed is first added. Defaults to last_n."
                },
                "backfill_n": {
                    "type": "integer",
                    "description": "When backfill_policy is last_n, the number of recent entries to keep. Defaults to 10."
                },
                "digest_channel": {
                    "type": "string",
                    "description": "Channel/Page ID to push daily digests to (e.g. 'telegram:123456789')"
                },
                "digest_cron": {
                    "type": "string",
                    "description": "Cron schedule for digest delivery. Defaults to '0 8 * * *' (daily 8 AM)"
                }
            },
            "required": ["url", "name"]
        }"#
    }

    fn permission_tier(&self) -> PermissionTier {
        PermissionTier::Prompt
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let params: RssSubscribeParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        // Build the FeedSpec.
        let rules: Vec<KeywordRule> = if !params.keyword_rules.is_empty() {
            params.keyword_rules.into_iter().map(Into::into).collect()
        } else if !params.keywords.is_empty() {
            params
                .keywords
                .into_iter()
                .map(|p| KeywordRule::Include {
                    phrase: p,
                    weight: 1.0,
                })
                .collect()
        } else {
            Vec::new()
        };

        let backfill = match params.backfill_policy.as_deref() {
            Some("last_24h") => BackfillPolicy::Last24h,
            Some("none") => BackfillPolicy::None,
            _ => BackfillPolicy::LastN,
        };

        let mut spec = FeedSpec::new(
            params.url.clone(),
            params.name.clone(),
            "", // legacy_csv not used when rules is non-empty
            params.digest_channel,
            params.digest_cron,
        );
        if !rules.is_empty() {
            spec = spec.with_rules(rules);
        }
        spec = spec.with_backfill(backfill, params.backfill_n.unwrap_or(10));

        match (self.subscribe_fn)(spec).await {
            Ok(_) => ToolResult {
                call_id: "".to_string(),
                output_json: format!(
                    r#"{{"subscribed":true,"feed_name":"{}","backfill_policy":"{}"}}"#,
                    params.name,
                    backfill.as_db()
                ),
                status: ToolStatus::Success,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: None,
            },
            Err(e) => ToolResult {
                call_id: "".to_string(),
                output_json: "".to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to subscribe to feed: {}", e)),
            },
        }
    }
}