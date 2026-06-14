//! Phase 7 / Track 7.1 - Skill types, builders, and re-exports.
//!
//! The canonical [`Skill`], [`SkillTier`], [`SkillParam`], [`SkillVersion`],
//! and [`SkillExecutionRecord`] types live in `hydragent-types` so other
//! crates can use them without a transitive dep on this crate. This
//! module re-exports them and adds crate-local helpers (the
//! `SkillSpec` YAML shape, the `BUILTIN_SKILLS` constant, and a few
//! conversion utilities).

use hydragent_types::{Skill, SkillParam, SkillTier};

/// The on-disk YAML shape for a [`Skill`]. Field names mirror the
/// snake_case serialization of the Rust struct so the YAML round-trips
/// via `serde_yaml`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SkillSpec {
    pub id: String,
    pub name: String,
    #[serde(default = "default_version")]
    pub version: u32,
    pub description: String,
    #[serde(default)]
    pub tier: SkillTier,
    #[serde(default)]
    pub capability_tags: Vec<String>,
    #[serde(default)]
    pub params: Vec<SkillParam>,
    pub prompt_template: String,
    #[serde(default)]
    pub required_tools: Vec<String>,
    #[serde(default)]
    pub success_examples: Vec<String>,
    pub author: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub last_updated: i64,
    #[serde(default)]
    pub success_rate: f32,
    #[serde(default)]
    pub execution_count: u32,
}

fn default_version() -> u32 { 1 }

impl From<Skill> for SkillSpec {
    fn from(s: Skill) -> Self {
        Self {
            id: s.id,
            name: s.name,
            version: s.version,
            description: s.description,
            tier: s.tier,
            capability_tags: s.capability_tags,
            params: s.params,
            prompt_template: s.prompt_template,
            required_tools: s.required_tools,
            success_examples: s.success_examples,
            author: s.author,
            created_at: s.created_at,
            last_updated: s.last_updated,
            success_rate: s.success_rate,
            execution_count: s.execution_count,
        }
    }
}

impl From<SkillSpec> for Skill {
    fn from(s: SkillSpec) -> Self {
        Self {
            id: s.id,
            name: s.name,
            version: s.version,
            description: s.description,
            tier: s.tier,
            capability_tags: s.capability_tags,
            params: s.params,
            prompt_template: s.prompt_template,
            required_tools: s.required_tools,
            success_examples: s.success_examples,
            author: s.author,
            created_at: s.created_at,
            last_updated: s.last_updated,
            success_rate: s.success_rate,
            execution_count: s.execution_count,
        }
    }
}

/// Serialize a [`Skill`] as canonical YAML.
pub fn skill_to_yaml(skill: &Skill) -> Result<String, serde_yaml::Error> {
    serde_yaml::to_string(&SkillSpec::from(skill.clone()))
}

/// Parse a [`Skill`] from canonical YAML.
pub fn skill_from_yaml(yaml: &str) -> Result<Skill, serde_yaml::Error> {
    let spec: SkillSpec = serde_yaml::from_str(yaml)?;
    Ok(spec.into())
}

/// Render a `{{param}}` template with the supplied values.
///
/// The renderer is intentionally minimal: it supports `{{name}}` and
/// `{{ name }}` (whitespace trimmed) and unknown placeholders are
/// preserved verbatim so the LLM can see and complain about them.
pub fn render_template(template: &str, params: &std::collections::HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Find the closing `}}`
            if let Some(end_rel) = find_close(&bytes[i + 2..]) {
                let inner = &template[i + 2..i + 2 + end_rel];
                let name = inner.trim();
                match params.get(name) {
                    Some(v) => out.push_str(v),
                    None => {
                        // Preserve the placeholder so the LLM sees it
                        out.push_str("{{");
                        out.push_str(inner);
                        out.push_str("}}");
                    }
                }
                i += 2 + end_rel + 2;
                continue;
            }
        }
        out.push(template.as_bytes()[i] as char);
        i += 1;
    }
    out
}

fn find_close(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Built-in skills shipped in `config/skills/builtin/`. The
/// [`SkillLibrary::load_builtins`](crate::library::SkillLibrary::load_builtins)
/// function reads these from disk; this constant is a list of canonical
/// names so callers can refer to them without resolving paths.
pub const BUILTIN_SKILL_NAMES: &[&str] = &[
    "convert-csv-to-json",
    "summarize-github-issue",
    "debug-rust-error",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn render_replaces_known_placeholders() {
        let tpl = "Hello {{name}}, your score is {{score}}.";
        let mut p = HashMap::new();
        p.insert("name".into(), "alice".into());
        p.insert("score".into(), "42".into());
        let out = render_template(tpl, &p);
        assert_eq!(out, "Hello alice, your score is 42.");
    }

    #[test]
    fn render_trims_whitespace_in_placeholder() {
        let tpl = "Hi {{  user  }}!";
        let mut p = HashMap::new();
        p.insert("user".into(), "bob".into());
        assert_eq!(render_template(tpl, &p), "Hi bob!");
    }

    #[test]
    fn render_preserves_unknown_placeholders() {
        let tpl = "x={{a}} y={{b}} z={{c}}";
        let mut p = HashMap::new();
        p.insert("a".into(), "1".into());
        p.insert("c".into(), "3".into());
        let out = render_template(tpl, &p);
        // `b` is unknown, should be preserved verbatim
        assert_eq!(out, "x=1 y={{b}} z=3");
    }

    #[test]
    fn render_handles_no_placeholders() {
        let tpl = "no placeholders here";
        let p = HashMap::new();
        assert_eq!(render_template(tpl, &p), "no placeholders here");
    }

    #[test]
    fn render_handles_unclosed_braces() {
        // Single `{` is not a placeholder opener; the renderer falls
        // through and emits the char.
        let tpl = "x = {single}";
        let p = HashMap::new();
        assert_eq!(render_template(tpl, &p), "x = {single}");
    }

    #[test]
    fn render_handles_empty_placeholder() {
        // `{{}}` is technically a placeholder with empty name; treat as
        // unknown (preserved verbatim).
        let tpl = "x={{}}";
        let p = HashMap::new();
        assert_eq!(render_template(tpl, &p), "x={{}}");
    }

    #[test]
    fn skill_yaml_roundtrip() {
        let s = Skill::new(
            "demo",
            "demo skill",
            "Hello {{name}}",
            "user:test",
        ).with_tag("demo");
        let yaml = skill_to_yaml(&s).unwrap();
        let back = skill_from_yaml(&yaml).unwrap();
        assert_eq!(back.id, s.id);
        assert_eq!(back.name, "demo");
        assert_eq!(back.prompt_template, "Hello {{name}}");
        assert_eq!(back.capability_tags, vec!["demo"]);
    }

    #[test]
    fn skill_yaml_version_defaults_to_one() {
        let yaml = r#"
id: abc
name: foo
description: bar
prompt_template: "{{x}}"
author: test
"#;
        let s = skill_from_yaml(yaml).unwrap();
        assert_eq!(s.version, 1);
        assert_eq!(s.execution_count, 0);
        assert_eq!(s.tier, SkillTier::Candidate);
    }

    #[test]
    fn skill_spec_to_and_from_skill() {
        let s = Skill::new("a", "b", "{{c}}", "u");
        let spec: SkillSpec = s.clone().into();
        let back: Skill = spec.into();
        assert_eq!(s.id, back.id);
        assert_eq!(s.name, back.name);
        assert_eq!(s.prompt_template, back.prompt_template);
    }

    #[test]
    fn builtins_are_three_named() {
        assert_eq!(BUILTIN_SKILL_NAMES.len(), 3);
        assert!(BUILTIN_SKILL_NAMES.contains(&"convert-csv-to-json"));
        assert!(BUILTIN_SKILL_NAMES.contains(&"summarize-github-issue"));
        assert!(BUILTIN_SKILL_NAMES.contains(&"debug-rust-error"));
    }

    #[test]
    fn skill_version_re_exports_correctly() {
        // Ensure the re-exports are wired (compile-time check).
        let v = crate::SkillVersion {
            skill_id: "x".into(),
            version: 1,
            yaml: "".into(),
            created_at: 0,
            changelog: "".into(),
        };
        assert_eq!(v.version, 1);
    }
}
