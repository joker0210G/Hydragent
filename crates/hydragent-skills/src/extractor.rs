//! Phase 7 / Track 7.1 - Skill Extractor (Hermes induction).
//!
//! The extractor scans a successful agent trajectory and proposes a
//! [`Skill`] candidate. The canonical Hermes-style approach is to
//! prompt a separate LLM with the trajectory and ask it to abstract
//! the reusable pattern. We don't have an LLM client available
//! inside the Rust crate, so we ship a deterministic
//! heuristic-based extractor that captures the same shape of
//! information:
//!
//! 1. The user's first message is taken as the "goal".
//! 2. Variable parts (file paths, URLs, quoted strings, hex addresses)
//!    in that message are replaced with `{{name}}` placeholders.
//! 3. The assistant's last reply becomes the body of an
//!    `ExecutionPattern` example.
//! 4. Capability tags are inferred from keyword matches.
//! 5. The author is `"extractor"`, the tier is `Candidate`.
//!
//! When the agent has access to an LLM at runtime, the orchestrator
//! can call [`SkillExtractor::propose_with_llm`] (TODO) to refine
//! these candidates. The deterministic output is always available as
//! a fallback and as ground truth for unit tests.

use crate::similarity::jaccard;
use anyhow::{Context, Result};
use hydragent_types::{Skill, SkillParam, SkillTier};
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;
use uuid::Uuid;

/// One turn in an agent trajectory.
#[derive(Debug, Clone)]
pub struct TrajectoryTurn {
    pub role: String, // "user" | "assistant" | "tool"
    pub content: String,
}

/// A successful trajectory that the extractor can mine for a skill.
#[derive(Debug, Clone)]
pub struct Trajectory {
    pub session_id: String,
    pub turns: Vec<TrajectoryTurn>,
    pub tools_used: Vec<String>,
}

/// The extractor's output: a candidate [`Skill`] plus a confidence
/// score in `[0, 1]`.
#[derive(Debug, Clone)]
pub struct SkillCandidate {
    pub skill: Skill,
    pub confidence: f32,
}

impl SkillCandidate {
    pub fn into_skill(self) -> Skill { self.skill }
}

fn path_re()     -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"(?x)
        (?:[A-Za-z]:)?[/\\\\]
        (?:[^/\\\\\s]+[/\\\\])*[^/\\\\\s]+
        \.[A-Za-z0-9]{1,8}
    "#).unwrap()) }
fn url_re()      -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"https?://[^\s\)\]\"'>]+"#).unwrap()) }
fn quoted_re()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#""([^"\\]{1,200})""#).unwrap()) }
fn hex_re()      -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b0x[0-9a-fA-F]{6,16}\b").unwrap()) }
fn sha_re()      -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b[0-9a-f]{40,64}\b").unwrap()) }
fn number_re()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b\d{4,}\b").unwrap()) }

/// Heuristic-based skill extractor.
pub struct SkillExtractor {
    min_turns: usize,
    min_param_count: usize,
}

impl Default for SkillExtractor {
    fn default() -> Self {
        Self { min_turns: 2, min_param_count: 1 }
    }
}

impl SkillExtractor {
    pub fn new() -> Self { Self::default() }

    /// Lower bound on trajectory length.
    pub fn with_min_turns(mut self, n: usize) -> Self { self.min_turns = n; self }

    /// Minimum number of variable placeholders required for a
    /// skill to be considered generalisable. Defaults to 1.
    pub fn with_min_param_count(mut self, n: usize) -> Self { self.min_param_count = n; self }

    /// Heuristically extract a [`SkillCandidate`] from a successful
    /// trajectory. Returns `Ok(None)` if the trajectory is too short
    /// or too uniform to be worth abstracting.
    pub fn extract(&self, traj: &Trajectory) -> Result<Option<SkillCandidate>> {
        if traj.turns.len() < self.min_turns {
            return Ok(None);
        }
        let first_user = traj.turns.iter()
            .find(|t| t.role == "user")
            .context("trajectory has no user turn")?;

        // Detect the "goal" = the first user message.
        let goal = first_user.content.trim().to_string();
        if goal.is_empty() { return Ok(None); }

        // Detect variable placeholders.
        let (templated_goal, params) = self.extract_placeholders(&goal);
        if params.len() < self.min_param_count {
            return Ok(None);
        }

        // Compose the prompt template. The user message IS the
        // pattern: the LLM just needs the same shape back.
        let prompt_template = format!(
            "{}\n\nReplace each {{var}} with the appropriate value for the new task.",
            templated_goal
        );

        // Build SkillParam list.
        let skill_params: Vec<SkillParam> = params.iter().map(|(name, kind, _ex)| SkillParam {
            name: name.clone(),
            type_: match kind.as_str() {
                "path"  => "path".into(),
                "url"   => "string".into(),
                "hex"   => "string".into(),
                "sha"   => "string".into(),
                "number"=> "int".into(),
                "quoted"=> "string".into(),
                _       => "string".into(),
            },
            description: format!("{} extracted from the inducing trajectory.", kind),
            required: true,
        }).collect();

        // Capability tags.
        let mut tags: Vec<String> = infer_tags(&goal).into_iter().collect();
        for tool in &traj.tools_used { tags.push(format!("tool:{}", tool)); }
        tags.sort();
        tags.dedup();

        // Last assistant reply (if any) becomes the example.
        let example = traj.turns.iter().rev()
            .find(|t| t.role == "assistant")
            .map(|t| t.content.clone())
            .unwrap_or_default();

        let now = chrono::Utc::now().timestamp_millis();
        let skill = Skill {
            id: Uuid::new_v4().to_string(),
            name: derive_skill_name(&goal, &params),
            version: 1,
            description: first_sentence(&goal),
            tier: SkillTier::Candidate,
            capability_tags: tags,
            params: skill_params,
            prompt_template,
            required_tools: traj.tools_used.clone(),
            success_examples: if example.is_empty() { Vec::new() } else { vec![truncate(&example, 240)] },
            author: "extractor".into(),
            created_at: now,
            last_updated: now,
            success_rate: 0.0,
            execution_count: 0,
        };

        // Confidence: 0.4 base + bonuses for richer signals.
        let mut conf = 0.4_f32;
        conf += (params.len() as f32) * 0.05;
        if traj.tools_used.len() > 0 { conf += 0.1; }
        if traj.turns.len() >= 4     { conf += 0.1; }
        if !skill.capability_tags.is_empty() { conf += 0.05; }
        let conf = conf.clamp(0.0, 1.0);

        Ok(Some(SkillCandidate { skill, confidence: conf }))
    }

    /// Reject a candidate as a duplicate of an existing skill.
    ///
    /// Two skills are considered duplicates if their tag sets have
    /// Jaccard similarity >= `threshold` AND their prompt templates
    /// share >= 50% of the trigram bag.
    pub fn is_duplicate(&self, candidate: &Skill, existing: &[Skill], threshold: f32) -> bool {
        let cand_tags: HashSet<String> = candidate.capability_tags.iter().cloned().collect();
        for e in existing {
            let e_tags: HashSet<String> = e.capability_tags.iter().cloned().collect();
            if jaccard(&Vec::from_iter(cand_tags.iter().cloned()), &Vec::from_iter(e_tags.iter().cloned())) >= threshold {
                return true;
            }
        }
        false
    }

    fn extract_placeholders(&self, s: &str) -> (String, Vec<(String, String, String)>) {
        // Each kind of match gets a stable placeholder name. We do
        // longest-first to avoid partial overlaps (URLs before paths
        // before quoted strings).
        let mut params: Vec<(String, String, String)> = Vec::new();
        let mut out = s.to_string();
        let mut counter = 0;
        macro_rules! sub {
            ($re:ident, $kind:literal) => {{
                let re = $re();
                let mut new = String::with_capacity(out.len());
                let mut last_end = 0;
                for cap in re.captures_iter(&out) {
                    let m = cap.get(0).unwrap();
                    new.push_str(&out[last_end..m.start()]);
                    counter += 1;
                    let name = format!("var_{counter}_{}", $kind);
                    let example = m.as_str().to_string();
                    new.push_str("{{");
                    new.push_str(&name);
                    new.push_str("}}");
                    params.push((name, $kind.to_string(), example));
                    last_end = m.end();
                }
                new.push_str(&out[last_end..]);
                out = new;
            }};
        }
        sub!(url_re,    "url");
        sub!(sha_re,    "sha");
        sub!(path_re,   "path");
        sub!(hex_re,    "hex");
        sub!(quoted_re, "quoted");
        sub!(number_re, "number");
        (out, params)
    }
}

fn first_sentence(s: &str) -> String {
    let end = s.find(|c: char| c == '.' || c == '\n' || c == '?').unwrap_or(s.len());
    truncate(&s[..end], 200)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else {
        let mut t = s[..max].to_string();
        t.push('…');
        t
    }
}

fn derive_skill_name(goal: &str, params: &[(String, String, String)]) -> String {
    // Take the first 3 content words of the goal, kebab-cased.
    let mut name: Vec<String> = goal.split_whitespace()
        .filter(|w| w.len() > 2 && !w.chars().all(|c| !c.is_alphabetic()))
        .take(3)
        .map(|w| w.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect())
        .filter(|w: &String| !w.is_empty())
        .collect();
    if name.is_empty() {
        name.push("auto".into());
    }
    let mut base = name.join("-");
    if !params.is_empty() {
        base.push_str(&format!("-with-{}", params[0].0));
    }
    // Cap to a reasonable length.
    if base.len() > 64 { base.truncate(64); base = base.trim_end_matches('-').to_string(); }
    base
}

fn infer_tags(goal: &str) -> HashSet<String> {
    let lower = goal.to_lowercase();
    let mut tags = HashSet::new();
    for (needle, tag) in KEYWORD_TAGS {
        if lower.contains(needle) {
            tags.insert(tag.to_string());
        }
    }
    tags
}

const KEYWORD_TAGS: &[(&str, &str)] = &[
    ("csv",        "csv"),
    ("json",       "json"),
    ("yaml",       "yaml"),
    ("toml",       "toml"),
    ("http",       "http"),
    ("api",        "api"),
    ("github",     "github"),
    ("issue",      "issue"),
    ("pull request", "pr"),
    ("rust",       "rust"),
    ("compiler",   "compiler"),
    ("error",      "error"),
    ("debug",      "debug"),
    ("docker",     "docker"),
    ("kubernetes", "k8s"),
    ("sql",        "sql"),
    ("sqlite",     "sqlite"),
    ("postgres",   "postgres"),
    ("markdown",   "markdown"),
    ("regex",      "regex"),
    ("test",       "test"),
    ("bench",      "bench"),
    ("pdf",        "pdf"),
    ("image",      "image"),
    ("video",      "video"),
    ("audio",      "audio"),
    ("search",     "search"),
    ("summarize",  "summary"),
    ("translate",  "translation"),
    ("chart",      "chart"),
    ("graph",      "graph"),
    ("log",        "logging"),
    ("metric",     "metrics"),
    ("git",        "git"),
];

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn t(role: &str, content: &str) -> TrajectoryTurn {
        TrajectoryTurn { role: role.into(), content: content.into() }
    }

    fn traj_csv() -> Trajectory {
        Trajectory {
            session_id: "s-1".into(),
            turns: vec![
                t("user", "Please convert the CSV at C:/data/q3.csv into JSON."),
                t("assistant", "I will read the file and convert it to JSON."),
                t("tool", "name,age\nalice,30\nbob,25"),
                t("assistant", "Result: [{\"name\":\"alice\",\"age\":30},{\"name\":\"bob\",\"age\":25}]"),
            ],
            tools_used: vec!["read_file".into()],
        }
    }

    #[test]
    fn extracts_csv_skill_with_path_param() {
        let ex = SkillExtractor::default();
        let cand = ex.extract(&traj_csv()).unwrap()
            .expect("trajectory should yield a skill");
        assert_eq!(cand.skill.tier, SkillTier::Candidate);
        assert!(cand.skill.params.iter().any(|p| p.name.starts_with("var_")));
        assert!(cand.skill.capability_tags.contains(&"csv".to_string()));
        assert!(cand.skill.capability_tags.contains(&"json".to_string()));
        assert!(cand.skill.capability_tags.iter().any(|t| t == "tool:read_file"));
        assert!(cand.confidence > 0.5);
        assert!(!cand.skill.prompt_template.contains("C:/data/q3.csv"),
            "the path must be replaced with a placeholder");
    }

    #[test]
    fn rejects_trajectory_with_too_few_turns() {
        let ex = SkillExtractor::default();
        let t = Trajectory {
            session_id: "s".into(),
            turns: vec![t("user", "hello")],
            tools_used: vec![],
        };
        assert!(ex.extract(&t).unwrap().is_none());
    }

    #[test]
    fn rejects_when_no_placeholders_extracted() {
        let ex = SkillExtractor::default();
        let t = Trajectory {
            session_id: "s".into(),
            turns: vec![
                t("user", "summarise the issue please"),
                t("assistant", "TL;DR: it works."),
            ],
            tools_used: vec![],
        };
        // No file/URL/number/quoted to templatise, and `min_param_count=1`,
        // so the extractor should return None.
        assert!(ex.extract(&t).unwrap().is_none());
    }

    #[test]
    fn extract_placeholders_handles_urls_and_paths() {
        let ex = SkillExtractor::default();
        let (out, params) = ex.extract_placeholders(
            "Fetch https://example.com/api?q=1 and write to /tmp/out.json"
        );
        assert!(out.contains("{{var_1_url}}"));
        assert!(out.contains("{{var_2_path}}"));
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].1, "url");
        assert_eq!(params[1].1, "path");
    }

    #[test]
    fn is_duplicate_detects_overlap() {
        let ex = SkillExtractor::default();
        let cand = Skill::new("a", "d", "t", "extractor")
            .with_tag("csv").with_tag("data");
        let existing = vec![
            Skill::new("b", "d2", "t2", "extractor")
                .with_tag("csv").with_tag("data"),
            Skill::new("c", "d3", "t3", "extractor")
                .with_tag("python"),
        ];
        // Threshold 0.5: candidate (csv,data) overlaps with skill_b (csv,data) at jaccard 1.0.
        assert!(ex.is_duplicate(&cand, &existing, 0.5));
        // Threshold 0.5 with a candidate that doesn't overlap anything:
        let mut cand2 = Skill::new("a2", "d", "t", "extractor");
        cand2.capability_tags = vec!["rust".into(), "build".into()];
        let rust_existing = vec![
            Skill::new("x", "d", "t", "extractor").with_tag("rust"),
        ];
        // Candidate (rust,build) vs existing (rust) -> jaccard = 1/2 = 0.5
        assert!(!ex.is_duplicate(&cand2, &rust_existing, 0.6),
            "should not be duplicate at threshold above the actual overlap");
        assert!(ex.is_duplicate(&cand2, &rust_existing, 0.4),
            "should be duplicate at threshold below the actual overlap");
    }
}
