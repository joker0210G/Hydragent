//! Phase 7 / Track 7.1 — `skill_search` LLM-callable tool.
//!
//! Hybrid search over the skill library. Runs three strategies in
//! parallel-shaped order:
//!   1. FTS5 against `(name, description)` via `search_by_keyword`
//!   2. LIKE-based fuzzy match (fallback) via `search_fuzzy`
//!   3. Per-token tag search via `search_by_tag` (deduped)
//!
//! All three result sets are returned so the LLM can pick the most
//! useful hit. A skill appearing in multiple sets is reported once per
//! set (the dedup is per-set, not cross-set).

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use hydragent_types::{Skill, ToolResult, ToolStatus};
use serde::Deserialize;
use serde_json::json;

use crate::tool_trait::Tool;

pub struct SkillSearchTool {
    db_path: PathBuf,
}

impl SkillSearchTool {
    /// Create a tool that reads from `data_dir/skill_library.sqlite`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            db_path: data_dir.into().join("skill_library.sqlite"),
        }
    }
}

#[derive(Deserialize)]
struct SkillSearchParams {
    /// Free-text query.
    query: String,
    /// Max results per strategy (fts / fuzzy / tag). Defaults to 5,
    /// capped at 50.
    #[serde(default)]
    limit: Option<u32>,
}

fn skill_to_json(s: &Skill) -> serde_json::Value {
    json!({
        "id": s.id,
        "name": s.name,
        "version": s.version,
        "description": s.description,
        "tier": s.tier.as_str(),
        "tags": s.capability_tags,
        "success_rate": s.success_rate,
    })
}

#[async_trait]
impl Tool for SkillSearchTool {
    fn name(&self) -> &str {
        "skill_search"
    }

    fn description(&self) -> &str {
        "Searches the skill library by free-text query. \
         Runs an FTS5 query against (name, description), then a \
         LIKE-based fuzzy match, then a per-token tag search. \
         Returns up to three ranked result sets. Use this when you \
         know roughly what kind of skill you need but aren't sure of \
         the exact name. For exact-name lookup use skill_run with \
         skill_name_or_id."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Free-text search query (e.g. 'csv json', 'github issue')."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Max results per strategy (default 5)."
                }
            },
            "required": ["query"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let call_id = String::new();

        let params: SkillSearchParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("invalid parameters: {e}") })
                        .to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("invalid parameters: {e}")),
                };
            }
        };

        let lib = match hydragent_skills::library::SkillLibrary::open(&self.db_path).await {
            Ok(l) => l,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("open skill library: {e:#}") })
                        .to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("open skill library: {e}")),
                };
            }
        };

        let limit = params.limit.unwrap_or(5).clamp(1, 50);

        // Strategy 1: FTS5
        let fts_hits: Vec<Skill> = lib
            .search_by_keyword(&params.query, limit)
            .await
            .unwrap_or_default();

        // Strategy 2: fuzzy fallback
        let fuzzy_hits: Vec<Skill> = if fts_hits.is_empty() {
            lib.search_fuzzy(&params.query, limit).await.unwrap_or_default()
        } else {
            // FTS already returned good hits — skip fuzzy to avoid noise.
            Vec::new()
        };

        // Strategy 3: per-token tag match (deduped by id)
        let mut tag_seen: HashSet<String> = HashSet::new();
        let mut tag_hits: Vec<Skill> = Vec::new();
        for token in params.query.split_whitespace() {
            if token.len() < 2 {
                continue;
            } // skip 1-char noise
            if let Ok(matches) = lib.search_by_tag(token).await {
                for s in matches {
                    if tag_seen.insert(s.id.clone()) {
                        tag_hits.push(s);
                        if tag_hits.len() as u32 >= limit {
                            break;
                        }
                    }
                }
            }
            if tag_hits.len() as u32 >= limit {
                break;
            }
        }

        ToolResult {
            call_id,
            output_json: json!({
                "query": params.query,
                "limit_per_strategy": limit,
                "fts_hit_count": fts_hits.len(),
                "fuzzy_hit_count": fuzzy_hits.len(),
                "tag_hit_count": tag_hits.len(),
                "fts_hits":   fts_hits.iter().map(skill_to_json).collect::<Vec<_>>(),
                "fuzzy_hits": fuzzy_hits.iter().map(skill_to_json).collect::<Vec<_>>(),
                "tag_hits":   tag_hits.iter().map(skill_to_json).collect::<Vec<_>>(),
            })
            .to_string(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}
