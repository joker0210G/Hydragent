//! Phase 7 / Track 7.1 — `skill_run` LLM-callable tool.
//!
//! Looks up a skill by name (kebab-case) or UUID, renders its prompt
//! template with the supplied parameters, and returns the rendered
//! prompt as the "output". We use [`StubBackend`] for execution so the
//! tool is self-contained: no model call, no tool-invocation, no
//! execution telemetry recorded (a future Track 7.x will add a real
//! backend that pipes the rendered prompt back through the brain
//! model).
//!
//! This makes the skill library effectively a **prompt-template
//! registry** for the chat LLM. The LLM can ask "what skills do I
//! have?", search by topic, then `skill_run` a specific skill to get
//! the well-formed prompt it should answer with.
//!
//! Behaviour notes:
//! - The render is **lenient** (max_missing_params = usize::MAX). We
//!   always return the rendered prompt; missing required params are
//!   surfaced in `diagnostics.missing_params` so the LLM can ask the
//!   user. This avoids the situation where a half-rendered prompt is
//!   silently dropped because one field was empty.
//! - If the caller provides an `input` field, we auto-fill the first
//!   matching `csv` / `text` / `input` / `body` param. This is a
//!   convenience for the common "here's the data, run the skill"
//!   pattern.
//! - Lookup is name-first (kebab-case canonical) then ID. UUIDs are
//!   only useful for direct cursor-to-skill flows.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use hydragent_types::ToolResult;
use serde::Deserialize;
use serde_json::json;

use crate::tool_trait::Tool;

/// Convenience param names auto-filled from `input`.
const AUTO_FILL_PARAMS: &[&str] = &["csv", "text", "input", "body", "content"];

pub struct SkillRunTool {
    db_path: PathBuf,
}

impl SkillRunTool {
    /// Create a tool that reads from `data_dir/skill_library.sqlite`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            db_path: data_dir.into().join("skill_library.sqlite"),
        }
    }
}

#[derive(Deserialize, Default)]
struct SkillRunParams {
    /// Skill name (kebab-case, e.g. "convert-csv-to-json") or UUID.
    skill_name_or_id: String,
    /// Map of param name → string value. Missing required params are
    /// reported in `diagnostics.missing_params` but do not block the
    /// render.
    #[serde(default)]
    params: HashMap<String, String>,
    /// Optional free-text input. If the skill has a `csv` or `text`
    /// (or `input` / `body` / `content`) param and you omit it from
    /// `params`, this value is auto-filled.
    #[serde(default)]
    input: Option<String>,
}

#[async_trait]
impl Tool for SkillRunTool {
    fn name(&self) -> &str {
        "skill_run"
    }

    fn description(&self) -> &str {
        "Looks up a skill by name or ID, renders its prompt template \
         with the supplied params, and returns the rendered prompt. \
         Use this when the user asks for a task that matches a known \
         skill (e.g. 'convert this CSV to JSON' → skill_run with \
         skill_name_or_id='convert-csv-to-json'). The rendered prompt \
         is what you should use as your response context."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "skill_name_or_id": {
                    "type": "string",
                    "description": "Skill kebab-case name (e.g. 'convert-csv-to-json') or UUID."
                },
                "params": {
                    "type": "object",
                    "description": "Map of param name → string value. Required params are listed in skill_list output.",
                    "additionalProperties": { "type": "string" }
                },
                "input": {
                    "type": "string",
                    "description": "Optional free-text input. If the skill has a 'csv' or 'text' param and you omit it, this is used."
                }
            },
            "required": ["skill_name_or_id"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let call_id = String::new();

        let params: SkillRunParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("invalid parameters: {e}") })
                        .to_string(),
                    status: hydragent_types::ToolStatus::Failure,
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
                    status: hydragent_types::ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("open skill library: {e}")),
                };
            }
        };

        // Lookup by name first, then by id.
        let lookup_result =
            lib.get_skill_by_name(&params.skill_name_or_id).await;
        let skill = match lookup_result {
            Ok(Some(s)) => Some(s),
            Ok(None) => lib.get_skill(&params.skill_name_or_id).await.unwrap_or(None),
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("lookup: {e}") }).to_string(),
                    status: hydragent_types::ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("lookup: {e}")),
                };
            }
        };

        let Some(skill) = skill else {
            return ToolResult {
                call_id,
                output_json: json!({
                    "error": format!("skill {:?} not found", params.skill_name_or_id),
                    "hint": "use skill_list or skill_search to discover available skills"
                })
                .to_string(),
                status: hydragent_types::ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!(
                    "skill {:?} not found",
                    params.skill_name_or_id
                )),
            };
        };

        // Build the param map: explicit params win, then auto-fill
        // a convenience param from `input` if the skill declares one.
        let mut render_params: HashMap<String, String> = params.params.clone();
        if let Some(inp) = &params.input {
            for p in &skill.params {
                if !p.required {
                    continue;
                }
                if render_params.contains_key(&p.name) {
                    continue;
                }
                if AUTO_FILL_PARAMS.contains(&p.name.as_str()) {
                    render_params.insert(p.name.clone(), inp.clone());
                    break;
                }
            }
        }

        // Lenient render — never fail on missing required params; surface
        // them in `missing_params` so the LLM can ask the user. We
        // intentionally bypass `SkillExecutor::validate` (which would
        // bail on missing required tools) and go straight to `render`.
        let executor = hydragent_skills::executor::SkillExecutor::new().lenient(usize::MAX);
        let rendered = executor.render(&skill, &render_params);

        // StubBackend returns the rendered prompt as the "output" with
        // success=true and zero backend latency.
        use hydragent_skills::executor::SkillBackend; // brings `invoke` into scope
        let backend = hydragent_skills::executor::StubBackend;
        let (output, _record) = match backend
            .invoke(&skill, &rendered.rendered, &render_params)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("backend: {e}") }).to_string(),
                    status: hydragent_types::ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("backend: {e}")),
                };
            }
        };

        ToolResult {
            call_id,
            output_json: json!({
                "skill_name": skill.name,
                "skill_id": skill.id,
                "version": skill.version,
                "tier": skill.tier.as_str(),
                "rendered_prompt": output,
                "diagnostics": {
                    "params_provided": render_params.keys().collect::<Vec<_>>(),
                    "unused_params":   rendered.unused_params,
                    "missing_params":  rendered.missing_params,
                }
            })
            .to_string(),
            status: hydragent_types::ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}
