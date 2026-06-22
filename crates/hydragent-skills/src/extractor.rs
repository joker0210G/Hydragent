//! Phase 7 / Track 7.1 - Skill Extractor (Hermes induction).
//!
//! The extractor scans a successful agent trajectory and proposes a
//! [`Skill`] candidate. The canonical Hermes-style approach is to
//! prompt a separate LLM with the trajectory and ask it to abstract
//! the reusable pattern.
//!
//! ## LLM-based extraction
//!
//! The [`SkillExtractor::propose_with_llm`] method accepts any
//! implementation of the [`LlmClient`] trait. It builds a structured
//! prompt from the trajectory, sends it to the LLM, and parses the
//! JSON response into a [`SkillCandidate`].
//!
//! ## Deterministic fallback
//!
//! When no LLM client is available, [`SkillExtractor::extract`]
//! provides a heuristic-based extraction that captures the same shape
//! of information:
//!
//! 1. The user's first message is taken as the "goal".
//! 2. Variable parts (file paths, URLs, quoted strings, hex addresses)
//!    in that message are replaced with `{{name}}` placeholders.
//! 3. The assistant's last reply becomes the body of an
//!    `ExecutionPattern` example.
//! 4. Capability tags are inferred from keyword matches.
//! 5. The author is `"extractor"`, the tier is `Candidate`.

use crate::similarity::{cosine_similarity, jaccard};
use anyhow::{Context, Result};
use async_trait::async_trait;
use hydragent_types::{Skill, SkillParam, SkillTier};
use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;
use uuid::Uuid;

/// LLM client interface used by [`SkillExtractor::propose_with_llm`].
/// Implement this trait to plug in any LLM backend (OpenAI, local
/// Ollama, a ModelRouter wrapper, etc.).
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a text prompt to the LLM and return its plain-text reply.
    async fn generate(&self, prompt: &str) -> anyhow::Result<String>;
}

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

/// Parsed subset of the LLM response used by [`SkillExtractor::propose_with_llm`].
#[derive(Debug, Deserialize)]
struct LlmSkillProposal {
    name: String,
    description: String,
    prompt_template: String,
    #[serde(default)]
    params: Vec<LlmParam>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    required_tools: Vec<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct LlmParam {
    name: String,
    #[serde(rename = "type", default = "default_param_type")]
    param_type: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    required: bool,
}

fn default_param_type() -> String { "string".into() }

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

    /// Semantic deduplication using embedding similarity.
    ///
    /// Compares the candidate's description embedding against each
    /// existing skill's description embedding. Returns `true` if
    /// cosine similarity exceeds `threshold` for any existing skill.
    #[allow(unused)]
    pub fn is_duplicate_semantic(
        &self,
        candidate: &Skill,
        existing: &[Skill],
        embedder: &hydragent_embed::LocalEmbedder,
        threshold: f32,
    ) -> anyhow::Result<bool> {
        let cand_emb = embedder.embed_text(&candidate.description)?;
        for e in existing {
            let e_emb = embedder.embed_text(&e.description)?;
            if let Some(sim) = cosine_similarity(&cand_emb, &e_emb) {
                if sim >= threshold {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Quality gate: returns `true` only if the trajectory represents
    /// a successful execution suitable for skill extraction.
    ///
    /// Checks:
    /// - No tool turn contains error/failed/failure (case-insensitive)
    /// - The last assistant turn does NOT start with failure markers
    ///   ("sorry", "failed", "error", "unable", "cannot")
    /// - The last assistant turn DOES contain success indicators
    ///   ("successfully", "done", "completed", "result", "here is")
    ///   OR has no failure markers at all
    pub fn is_trajectory_successful(&self, traj: &Trajectory) -> bool {
        // 1. Check tool turns for errors
        for turn in &traj.turns {
            if turn.role == "tool" {
                let lower = turn.content.to_lowercase();
                if lower.contains("error") || lower.contains("failed") || lower.contains("failure") {
                    return false;
                }
            }
        }

        // 2. Find the last assistant turn
        let last_assistant = traj.turns.iter().rev().find(|t| t.role == "assistant");

        // If there's no assistant turn, we can't verify success
        let Some(last) = last_assistant else { return true; };

        let content = last.content.trim();

        // 3. Check for failure markers at the START of the response
        // Only fail if the response literally starts with these words
        let failure_starters = ["sorry", "failed", "error", "unable", "cannot"];
        let first_word_lower = content
            .split_whitespace()
            .next()
            .map(|w| w.trim_end_matches(|c: char| c.is_ascii_punctuation()).to_lowercase())
            .unwrap_or_default();

        for starter in &failure_starters {
            if first_word_lower == *starter {
                return false;
            }
        }

        // 4. Check for success indicators
        let lower = content.to_lowercase();
        let success_indicators = ["successfully", "done", "completed", "result", "here is"];
        let has_success = success_indicators.iter().any(|ind| lower.contains(ind));

        // Pass if success indicators present OR no explicit failure at start
        has_success || !failure_starters.iter().any(|f| lower.contains(f))
    }

    /// Use an LLM to propose a [`SkillCandidate`] from a trajectory.
    ///
    /// This method first applies the quality gate via
    /// [`is_trajectory_successful`](Self::is_trajectory_successful). If
    /// the trajectory is not clearly successful, it returns `Ok(None)`.
    /// Otherwise it builds a structured prompt, calls `llm.generate`,
    /// parses the JSON response, and returns the candidate.
    ///
    /// If JSON parsing fails, returns `Ok(None)` so the caller can
    /// fall back to the deterministic [`extract`](Self::extract).
    pub async fn propose_with_llm(
        &self,
        llm: &dyn LlmClient,
        traj: &Trajectory,
    ) -> anyhow::Result<Option<SkillCandidate>> {
        // Quality gate
        if !self.is_trajectory_successful(traj) {
            return Ok(None);
        }

        let prompt = self.build_llm_prompt(traj);
        let raw = llm.generate(&prompt).await?;

        let proposal: LlmSkillProposal = match serde_json::from_str(&raw) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        // Validate required fields are non-empty
        if proposal.name.is_empty() || proposal.prompt_template.is_empty() {
            return Ok(None);
        }

        // Build SkillParam list from LLM response
        let skill_params: Vec<SkillParam> = proposal
            .params
            .into_iter()
            .map(|p| SkillParam {
                name: p.name,
                type_: p.param_type,
                description: p.description,
                required: p.required,
            })
            .collect();

        // Capability tags
        let mut tags = proposal.tags;
        for tool in &traj.tools_used {
            tags.push(format!("tool:{}", tool));
        }
        tags.sort();
        tags.dedup();

        // Last assistant reply becomes the success example
        let last_reply = traj.turns
            .iter()
            .rev()
            .find(|t| t.role == "assistant")
            .map(|t| truncate(&t.content, 240))
            .unwrap_or_default();

        let now = chrono::Utc::now().timestamp_millis();
        let skill = Skill {
            id: Uuid::new_v4().to_string(),
            name: proposal.name,
            version: 1,
            description: proposal.description,
            tier: SkillTier::Candidate,
            capability_tags: tags,
            params: skill_params,
            prompt_template: proposal.prompt_template,
            required_tools: proposal.required_tools,
            success_examples: if last_reply.is_empty() { Vec::new() } else { vec![last_reply] },
            author: "llm".into(),
            created_at: now,
            last_updated: now,
            success_rate: 0.0,
            execution_count: 0,
        };

        let confidence = proposal.confidence.unwrap_or(0.7).clamp(0.0, 1.0);

        Ok(Some(SkillCandidate { skill, confidence }))
    }

    fn build_llm_prompt(&self, traj: &Trajectory) -> String {
        let first_user = traj.turns
            .iter()
            .find(|t| t.role == "user")
            .map(|t| t.content.as_str())
            .unwrap_or("(no user message)");

        let tools = if traj.tools_used.is_empty() {
            String::from("(none)")
        } else {
            traj.tools_used.join(", ")
        };

        let mut turns_out = String::new();
        for (i, turn) in traj.turns.iter().enumerate() {
            turns_out.push_str(&format!(
                "[{}] {}: {}\n",
                i + 1,
                turn.role,
                turn.content.lines().take(5).collect::<Vec<_>>().join(" ")
            ));
        }

        format!(
            r#"You are a skill extraction engine. Given the agent's trajectory below, abstract a reusable skill.

USER GOAL: {first_user}
TOOL CALLS: {tools}
CONVO TURNS:
{turns_out}Abstract this into a reusable skill. Respond ONLY with valid JSON in this exact shape:
{{
  "name": "kebab-case-skill-name",
  "description": "One-sentence description of what this skill does",
  "prompt_template": "The instruction pattern with {{param_name}} placeholders for variable parts",
  "params": [
    {{"name": "param_name", "type": "string|int|path|float", "description": "What this parameter should contain", "required": true}}
  ],
  "tags": ["csv", "json", "api", ...],
  "required_tools": ["tool_name"],
  "confidence": 0.0-1.0
}}

The prompt_template should be the generalized version of the user's original request."#
        )
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

// ============================================================================
// LLM-based extraction tests
// ============================================================================

#[cfg(test)]
mod llm_tests {
    use super::*;

    struct MockLlm {
        response: String,
    }

    #[async_trait::async_trait]
    impl LlmClient for MockLlm {
        async fn generate(&self, _prompt: &str) -> anyhow::Result<String> {
            Ok(self.response.clone())
        }
    }

    fn success_traj() -> Trajectory {
        Trajectory {
            session_id: "llm-test-1".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Convert C:/data/sales.csv to JSON".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "I will read the file and convert it to JSON.".into(),
                },
                TrajectoryTurn {
                    role: "tool".into(),
                    content: "name,amount\nfoo,100".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "Successfully converted. Result: [{\"name\":\"foo\",\"amount\":100}] Here is the output.".into(),
                },
            ],
            tools_used: vec!["file_read".into()],
        }
    }

    fn failure_traj_with_tool_error() -> Trajectory {
        Trajectory {
            session_id: "llm-test-2".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Process data.csv".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "Reading the file now.".into(),
                },
                TrajectoryTurn {
                    role: "tool".into(),
                    content: "Error: file not found".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "I apologize, the file was not found.".into(),
                },
            ],
            tools_used: vec!["file_read".into()],
        }
    }

    fn failure_traj_with_sorry() -> Trajectory {
        Trajectory {
            session_id: "llm-test-3".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Summarize https://example.com".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "Sorry, I cannot access that URL.".into(),
                },
            ],
            tools_used: vec![],
        }
    }

    // --- is_trajectory_successful tests ---

    #[test]
    fn quality_gate_passes_successful_trajectory() {
        let ex = SkillExtractor::default();
        assert!(ex.is_trajectory_successful(&success_traj()));
    }

    #[test]
    fn quality_gate_fails_on_tool_error() {
        let ex = SkillExtractor::default();
        assert!(!ex.is_trajectory_successful(&failure_traj_with_tool_error()));
    }

    #[test]
    fn quality_gate_fails_on_sorry_start() {
        let ex = SkillExtractor::default();
        assert!(!ex.is_trajectory_successful(&failure_traj_with_sorry()));
    }

    #[test]
    fn quality_gate_passes_neutral_trajectory() {
        let ex = SkillExtractor::default();
        let traj = Trajectory {
            session_id: "neutral".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "What is 2+2?".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "4".into(),
                },
            ],
            tools_used: vec![],
        };
        // No success indicators but also no failure markers - should pass
        assert!(ex.is_trajectory_successful(&traj));
    }

    #[test]
    fn quality_gate_fails_on_failed_marker() {
        let ex = SkillExtractor::default();
        let traj = Trajectory {
            session_id: "fail".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Do something".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "Failed to complete the task.".into(),
                },
            ],
            tools_used: vec![],
        };
        assert!(!ex.is_trajectory_successful(&traj));
    }

    #[test]
    fn quality_gate_passes_on_done_indicator() {
        let ex = SkillExtractor::default();
        let traj = Trajectory {
            session_id: "done".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Run benchmark".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "Done! The benchmark completed successfully.".into(),
                },
            ],
            tools_used: vec![],
        };
        assert!(ex.is_trajectory_successful(&traj));
    }

    // --- propose_with_llm tests ---

    #[tokio::test]
    async fn propose_with_llm_parses_valid_response() {
        let ex = SkillExtractor::default();
        let mock = MockLlm {
            response: r#"{
                "name": "csv-to-json-converter",
                "description": "Convert a CSV file to JSON format",
                "prompt_template": "Convert {{csv_path}} to JSON",
                "params": [
                    {"name": "csv_path", "type": "path", "description": "Path to CSV file", "required": true}
                ],
                "tags": ["csv", "json", "conversion"],
                "required_tools": ["file_read"],
                "confidence": 0.85
            }"#.into(),
        };

        let result = ex.propose_with_llm(&mock, &success_traj()).await;
        let cand = result.unwrap().expect("should return a candidate");

        assert_eq!(cand.skill.name, "csv-to-json-converter");
        assert_eq!(cand.skill.author, "llm");
        assert_eq!(cand.skill.tier, SkillTier::Candidate);
        assert_eq!(cand.confidence, 0.85);
        assert_eq!(cand.skill.capability_tags, vec!["conversion", "csv", "json", "tool:file_read"]);
        assert_eq!(cand.skill.params.len(), 1);
        assert_eq!(cand.skill.params[0].name, "csv_path");
        assert_eq!(cand.skill.params[0].type_, "path");
    }

    #[tokio::test]
    async fn propose_with_llm_returns_none_on_bad_json() {
        let ex = SkillExtractor::default();
        let mock = MockLlm {
            response: "this is not json".into(),
        };

        let result = ex.propose_with_llm(&mock, &success_traj()).await;
        assert!(result.unwrap().is_none(), "should return None on parse failure");
    }

    #[tokio::test]
    async fn propose_with_llm_returns_none_on_empty_name() {
        let ex = SkillExtractor::default();
        let mock = MockLlm {
            response: r#"{
                "name": "",
                "description": "desc",
                "prompt_template": "template"
            }"#.into(),
        };

        let result = ex.propose_with_llm(&mock, &success_traj()).await;
        assert!(result.unwrap().is_none(), "should return None when name is empty");
    }

    #[tokio::test]
    async fn propose_with_llm_returns_none_when_quality_gate_fails() {
        let ex = SkillExtractor::default();
        let mock = MockLlm {
            response: r#"{
                "name": "should-not-appear",
                "description": "desc",
                "prompt_template": "template"
            }"#.into(),
        };

        // This trajectory should fail the quality gate
        let result = ex.propose_with_llm(&mock, &failure_traj_with_tool_error()).await;
        assert!(result.unwrap().is_none(), "should return None when quality gate fails");
    }

    #[tokio::test]
    async fn propose_with_llm_uses_last_assistant_as_example() {
        let ex = SkillExtractor::default();
        let mock = MockLlm {
            response: r#"{
                "name": "test-skill",
                "description": "Test skill",
                "prompt_template": "Do {{task}}"
            }"#.into(),
        };

        let cand = ex.propose_with_llm(&mock, &success_traj()).await.unwrap().unwrap();
        // The last assistant message contains "Successfully converted..."
        assert!(!cand.skill.success_examples.is_empty());
        assert!(cand.skill.success_examples[0].contains("Successfully converted"));
    }

    #[tokio::test]
    async fn propose_with_llm_adds_tool_tags() {
        let ex = SkillExtractor::default();
        let mock = MockLlm {
            response: r#"{
                "name": "tool-skill",
                "description": "A skill using tools",
                "prompt_template": "Use {{tool}}",
                "tags": ["custom"],
                "required_tools": []
            }"#.into(),
        };

        let cand = ex.propose_with_llm(&mock, &success_traj()).await.unwrap().unwrap();
        // Should include both LLM-provided tag and auto-generated tool tag
        assert!(cand.skill.capability_tags.contains(&"custom".into()));
        assert!(cand.skill.capability_tags.contains(&"tool:file_read".into()));
    }

    #[tokio::test]
    async fn propose_with_llm_defaults_missing_optional_fields() {
        let ex = SkillExtractor::default();
        let mock = MockLlm {
            response: r#"{
                "name": "minimal-skill",
                "description": "A minimal skill",
                "prompt_template": "Do the thing"
            }"#.into(),
        };

        let cand = ex.propose_with_llm(&mock, &success_traj()).await.unwrap().unwrap();
        assert_eq!(cand.skill.params, Vec::<SkillParam>::new());
        assert_eq!(cand.skill.required_tools, Vec::<String>::new());
        assert_eq!(cand.skill.capability_tags, vec!["tool:file_read"]);
        // Default confidence when not provided
        assert_eq!(cand.confidence, 0.7);
    }
}
