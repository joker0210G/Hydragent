//! Phase 7 / Track 7.2 - Skill Executor.
//!
//! The executor turns a [`Skill`] into a rendered prompt and the
//! surrounding scaffolding the agent needs to actually invoke the
//! skill. It is intentionally a thin layer over the renderer in
//! [`crate::skill`] plus a few orchestration helpers:
//!
//! - [`SkillExecutor::validate`] - check that all required params
//!   are present and that required tools are available.
//! - [`SkillExecutor::render`] - expand the template.
//! - [`SkillExecutor::execute`] - validate + render + dispatch to a
//!   [`SkillBackend`] (a model call, a tool call, or a no-op stub).
//!
//! The `Backend` trait is async; the real backend is owned by the
//! orchestrator. We ship a [`StubBackend`] that just returns the
//! rendered prompt, useful for unit tests and dry-runs.

use crate::skill::render_template;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use hydragent_types::{Skill, SkillExecutionRecord};
use std::collections::HashMap;

/// Result of rendering a skill template.
#[derive(Debug, Clone)]
pub struct SkillRenderResult {
    /// The expanded prompt the model should see.
    pub rendered: String,
    /// Subset of `params` that were unused (warn-only).
    pub unused_params: Vec<String>,
    /// Subset of the skill's declared params that were missing
    /// (defaulted or warned).
    pub missing_params: Vec<String>,
}

/// A real skill backend (model call, tool invocation, etc.). Owned by
/// the orchestrator.
#[async_trait]
pub trait SkillBackend: Send + Sync {
    /// Run the skill with the already-rendered prompt. Returns the
    /// raw model output (or a tool result) and a record describing
    /// the call for the telemetry pipeline.
    async fn invoke(
        &self,
        skill: &Skill,
        rendered: &str,
        params: &HashMap<String, String>,
    ) -> Result<(String, SkillExecutionRecord)>;
}

/// Backend that does nothing - returns the rendered prompt as the
/// "output". Useful for tests and `--dry-run`.
pub struct StubBackend;

#[async_trait]
impl SkillBackend for StubBackend {
    async fn invoke(
        &self,
        skill: &Skill,
        rendered: &str,
        params: &HashMap<String, String>,
    ) -> Result<(String, SkillExecutionRecord)> {
        let start = std::time::Instant::now();
        let rec = SkillExecutionRecord {
            skill_id: skill.id.clone(),
            success: true,
            latency_ms: start.elapsed().as_millis() as u32,
            timestamp: chrono::Utc::now().timestamp_millis(),
            params_json: serde_json::to_string(params)?,
            error: None,
        };
        Ok((rendered.to_string(), rec))
    }
}

/// Top-level executor.
pub struct SkillExecutor {
    pub max_missing_params: usize,
}

impl Default for SkillExecutor {
    fn default() -> Self {
        Self { max_missing_params: 0 }
    }
}

impl SkillExecutor {
    pub fn new() -> Self { Self::default() }

    /// Allow up to `n` missing required params (they become a
    /// rendered warning block in the prompt).
    pub fn lenient(mut self, n: usize) -> Self { self.max_missing_params = n; self }

    /// Validate that all required params are present and that
    /// required tools are available. Returns the missing param
    /// names on failure.
    pub fn validate(
        &self,
        skill: &Skill,
        params: &HashMap<String, String>,
        available_tools: &[String],
    ) -> Result<()> {
        let missing: Vec<String> = skill.params.iter()
            .filter(|p| p.required && !params.contains_key(&p.name))
            .map(|p| p.name.clone())
            .collect();
        if missing.len() > self.max_missing_params {
            bail!("skill '{}' missing required params: {:?}", skill.name, missing);
        }
        for tool in &skill.required_tools {
            if !available_tools.iter().any(|t| t == tool) {
                bail!("skill '{}' requires tool '{}' which is not available", skill.name, tool);
            }
        }
        Ok(())
    }

    /// Render the template with the supplied params. Returns the
    /// expanded prompt plus diagnostics.
    pub fn render(
        &self,
        skill: &Skill,
        params: &HashMap<String, String>,
    ) -> SkillRenderResult {
        let rendered = render_template(&skill.prompt_template, params);
        let unused: Vec<String> = params.keys()
            .filter(|k| !skill.params.iter().any(|p| &p.name == *k))
            .cloned()
            .collect();
        let missing: Vec<String> = skill.params.iter()
            .filter(|p| p.required && !params.contains_key(&p.name))
            .map(|p| p.name.clone())
            .collect();
        SkillRenderResult { rendered, unused_params: unused, missing_params: missing }
    }

    /// Validate, render, and dispatch to a backend.
    pub async fn execute(
        &self,
        skill: &Skill,
        params: HashMap<String, String>,
        available_tools: &[String],
        backend: &dyn SkillBackend,
    ) -> Result<String> {
        self.validate(skill, &params, available_tools)?;
        let r = self.render(skill, &params);
        if !r.missing_params.is_empty() && r.missing_params.len() > self.max_missing_params {
            bail!("refusing to execute: missing params {:?}", r.missing_params);
        }
        let (out, _rec) = backend.invoke(skill, &r.rendered, &params).await?;
        Ok(out)
    }
}

/// Convenience: look up a skill by name from a library and execute it.
/// Returns an error if the skill is not found.
pub async fn execute_skill_by_name(
    lib: &crate::library::SkillLibrary,
    executor: &SkillExecutor,
    name: &str,
    params: HashMap<String, String>,
    available_tools: &[String],
    backend: &dyn SkillBackend,
) -> Result<String> {
    let skill = lib.get_skill_by_name(name).await?
        .ok_or_else(|| anyhow!("skill '{name}' not found in library"))?;
    executor.execute(&skill, params, available_tools, backend).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydragent_types::{Skill, SkillParam};

    fn csv_skill() -> Skill {
        Skill::new(
            "convert-csv-to-json",
            "Convert CSV to JSON",
            "Convert this CSV to JSON:\n```\n{{csv}}\n```\nDelimiter: {{delimiter}}",
            "extractor",
        )
        .with_param(SkillParam {
            name: "csv".into(),
            type_: "string".into(),
            description: "csv blob".into(),
            required: true,
        })
        .with_param(SkillParam {
            name: "delimiter".into(),
            type_: "string".into(),
            description: "field separator".into(),
            required: true,
        })
        .with_required_tool("echo")
    }

    fn p(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    #[test]
    fn render_expands_known_placeholders() {
        let ex = SkillExecutor::new();
        let s = csv_skill();
        let r = ex.render(&s, &p(&[("csv", "a,b\n1,2"), ("delimiter", ",")]));
        assert!(r.rendered.contains("a,b\n1,2"));
        assert!(r.rendered.contains("Delimiter: ,"));
        assert!(r.unused_params.is_empty());
        assert!(r.missing_params.is_empty());
    }

    #[test]
    fn render_reports_missing_required_params() {
        let ex = SkillExecutor::new();
        let s = csv_skill();
        let r = ex.render(&s, &p(&[("csv", "x")]));
        assert!(r.rendered.contains("{{delimiter}}"),
            "missing placeholder should be preserved verbatim");
        assert_eq!(r.missing_params, vec!["delimiter"]);
    }

    #[test]
    fn render_reports_unused_params() {
        let ex = SkillExecutor::new();
        let s = csv_skill();
        let r = ex.render(&s, &p(&[("csv", "x"), ("delimiter", ","), ("extra", "y")]));
        assert_eq!(r.unused_params, vec!["extra"]);
    }

    #[test]
    fn validate_fails_when_required_param_missing() {
        let ex = SkillExecutor::new();
        let s = csv_skill();
        let err = ex.validate(&s, &p(&[("csv", "x")]), &[]).unwrap_err();
        assert!(err.to_string().contains("delimiter"));
    }

    #[test]
    fn validate_fails_when_required_tool_missing() {
        let ex = SkillExecutor::new();
        let s = csv_skill();
        let err = ex.validate(&s, &p(&[("csv", "x"), ("delimiter", ",")]), &[]).unwrap_err();
        assert!(err.to_string().contains("tool 'echo'"));
    }

    #[test]
    fn validate_passes_with_all_params_and_tools() {
        let ex = SkillExecutor::new();
        let s = csv_skill();
        ex.validate(&s, &p(&[("csv", "x"), ("delimiter", ",")]), &["echo".into()]).unwrap();
    }

    #[tokio::test]
    async fn execute_dispatches_to_backend() {
        let ex = SkillExecutor::new();
        let s = csv_skill();
        let out = ex.execute(
            &s,
            p(&[("csv", "a,b\n1,2"), ("delimiter", ",")]),
            &["echo".into()],
            &StubBackend,
        ).await.unwrap();
        assert!(out.contains("a,b"));
    }
}
