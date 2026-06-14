//! Phase 7 / Track 7.1 — `skill_list` LLM-callable tool.
//!
//! Lists skills from the persistent library with optional filtering by
//! tier, name substring, and success-rate threshold. This is the
//! primary discovery surface for the chat LLM — when the user asks
//! "what skills do you have?", this tool is the answer.
//!
//! Each invocation opens a fresh `SkillLibrary` handle (mirroring
//! `AuditQueryTool`). The cost is a few ms (SQLite re-open of an
//! already-migrated DB) and avoids any lifetime / interior-mutability
//! dance with the `Tool` trait's `&self` method.

use std::path::PathBuf;

use async_trait::async_trait;
use hydragent_types::{SkillTier, ToolResult, ToolStatus};
use serde::Deserialize;
use serde_json::json;

use crate::tool_trait::Tool;

/// `skill_list` tool.
pub struct SkillListTool {
    db_path: PathBuf,
}

impl SkillListTool {
    /// Create a tool that reads from `data_dir/skill_library.sqlite`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            db_path: data_dir.into().join("skill_library.sqlite"),
        }
    }
}

#[derive(Deserialize, Default)]
struct SkillListParams {
    /// Optional tier filter. If omitted, all tiers are returned.
    /// Accepts "candidate", "active", "inactive", "archived".
    #[serde(default)]
    tier: Option<String>,
    /// Optional substring filter on skill name (case-insensitive).
    #[serde(default)]
    name_contains: Option<String>,
    /// Optional minimum success rate, 0.0-1.0.
    #[serde(default)]
    min_success_rate: Option<f32>,
    /// Max rows to return. Defaults to 20, capped at 100.
    #[serde(default)]
    limit: Option<u32>,
    /// Skip the first N rows (paging). Defaults to 0.
    #[serde(default)]
    offset: Option<u32>,
}

#[async_trait]
impl Tool for SkillListTool {
    fn name(&self) -> &str {
        "skill_list"
    }

    fn description(&self) -> &str {
        "Lists skills from the persistent skill library. \
         Pass tier='active' (or candidate/inactive/archived) to filter. \
         Pass name_contains for a substring match. Returns a compact \
         summary per skill (name, description, tier, success_rate, tags, \
         params). Use this to discover what skills are available before \
         invoking one with skill_run."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "tier": {
                    "type": "string",
                    "enum": ["candidate", "active", "inactive", "archived"],
                    "description": "Filter by skill lifecycle tier. Default: no filter."
                },
                "name_contains": {
                    "type": "string",
                    "description": "Substring match on skill name (case-insensitive)."
                },
                "min_success_rate": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "description": "Only return skills with success_rate >= this value."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Max rows to return (default 20)."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Skip the first N rows (paging, default 0)."
                }
            }
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();

        let params: SkillListParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id: String::new(),
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
                    call_id: String::new(),
                    output_json: json!({ "error": format!("open skill library: {e:#}") })
                        .to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("open skill library: {e}")),
                };
            }
        };

        let tier = match params.tier.as_deref() {
            None => None,
            Some("candidate") => Some(SkillTier::Candidate),
            Some("active") => Some(SkillTier::Active),
            Some("inactive") => Some(SkillTier::Inactive),
            Some("archived") => Some(SkillTier::Archived),
            Some(other) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: json!({
                        "error": format!("unknown tier: {other:?}"),
                        "hint": "use one of: candidate, active, inactive, archived (or omit for all)"
                    })
                    .to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("unknown tier: {other:?}")),
                };
            }
        };

        let filter = hydragent_skills::library::SkillFilter {
            tier,
            name_contains: params.name_contains,
            limit: Some(params.limit.unwrap_or(20).clamp(1, 100)),
            offset: Some(params.offset.unwrap_or(0)),
            min_success_rate: params.min_success_rate,
        };

        let skills = match lib.list_skills(filter).await {
            Ok(s) => s,
            Err(e) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: json!({ "error": format!("list_skills: {e}") }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("list_skills: {e}")),
                };
            }
        };

        let total = lib.count().await.unwrap_or(0);

        let rows: Vec<serde_json::Value> = skills
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "name": s.name,
                    "version": s.version,
                    "description": s.description,
                    "tier": s.tier.as_str(),
                    "tags": s.capability_tags,
                    "params": s.params.iter().map(|p| json!({
                        "name": p.name,
                        "type": p.type_,
                        "required": p.required,
                        "description": p.description,
                    })).collect::<Vec<_>>(),
                    "required_tools": s.required_tools,
                    "success_rate": s.success_rate,
                    "execution_count": s.execution_count,
                    "last_updated_ms": s.last_updated,
                })
            })
            .collect();

        ToolResult {
            call_id: String::new(),
            output_json: json!({
                "total_skills_in_library": total,
                "returned": rows.len(),
                "skills": rows,
            })
            .to_string(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}
