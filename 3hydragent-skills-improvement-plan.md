# Hydragent Skill Engine — Improvement Plan

> **Scope:** `crates/hydragent-skills/` and `skills/builtin/`
> **Total items:** 12 improvements across 4 areas
> **Priority key:** 🔴 High · 🟡 Medium · 🟢 Low-effort / high-value

---

## Table of Contents

1. [Extractor Fixes (3 flagged gaps)](#1-extractor-fixes)
   - 1.1 [Smart Parameter Naming](#11-smart-parameter-naming)
   - 1.2 [Tool Dependency Mapping](#12-tool-dependency-mapping)
   - 1.3 [Syntax-Aware Parsing](#13-syntax-aware-parsing)
2. [Quality Gate Improvements](#2-quality-gate-improvements)
   - 2.1 [Richer `is_trajectory_successful` logic](#21-richer-is_trajectory_successful-logic)
   - 2.2 [Minimum confidence threshold](#22-minimum-confidence-threshold)
3. [Curator Upgrades](#3-curator-upgrades)
   - 3.1 [Semantic deduplication pass](#31-semantic-deduplication-pass)
   - 3.2 [Version rollback on demotion](#32-version-rollback-on-demotion)
   - 3.3 [Config-driven thresholds](#33-config-driven-thresholds)
4. [New Built-in Skills](#4-new-built-in-skills)
   - 4.1 [git-commit-message](#41-git-commit-message)
   - 4.2 [explain-shell-command](#42-explain-shell-command)
   - 4.3 [query-json-with-jq](#43-query-json-with-jq)
   - 4.4 [debug-python-traceback](#44-debug-python-traceback)
   - 4.5 [generate-markdown-doc](#45-generate-markdown-doc)
   - 4.6 [find-files-by-pattern](#46-find-files-by-pattern)

---

## 1. Extractor Fixes

These three items are the **highest priority** in the whole plan because they affect every skill auto-inducted from this point forward. Fixing them now means the skill library grows with clean, reusable templates rather than accumulating junk that must be cleaned up later.

**File:** `crates/hydragent-skills/src/extractor.rs`

---

### 1.1 Smart Parameter Naming

**Priority:** 🔴 High

#### Problem

The heuristic fallback in `extract_from_trajectory` names every regex-matched placeholder with a generic positional label:

```rust
// Current behaviour
fn name_slot(idx: usize) -> String {
    format!("var_{}_quoted", idx)   // → var_1_quoted, var_2_quoted, …
}
```

A prompt template full of `{{var_1_quoted}}` and `{{var_2_quoted}}` is impossible to reuse — the agent has no idea what to supply without re-reading the original trajectory. It also makes the FTS5 search less useful because the template text carries no semantic signal.

#### Solution

Scan a small **context window** of 1–3 tokens to the left of each matched span. Map recognised nouns to descriptive slot names. Fall back to a positional label only if nothing matches.

```rust
/// Maps a keyword found near a matched span to a descriptive slot name.
/// Keys are lowercase substrings; values are the slot names to use.
static KEYWORD_MAP: &[(&str, &str)] = &[
    // Filesystem
    ("file",   "file_path"),
    ("path",   "file_path"),
    ("dir",    "directory"),
    ("output", "output_path"),
    // Networking
    ("url",    "repo_url"),
    ("repo",   "repo_url"),
    ("host",   "host_url"),
    ("endpoint", "api_endpoint"),
    // Auth
    ("token",  "api_token"),
    ("key",    "api_key"),
    ("secret", "api_secret"),
    // Code / errors
    ("error",  "error_msg"),
    ("trace",  "traceback"),
    ("query",  "search_query"),
    ("branch", "branch_name"),
    ("commit", "commit_hash"),
    ("diff",   "staged_diff"),
    // Generic data
    ("input",  "input_data"),
    ("data",   "payload"),
    ("config", "config_value"),
    ("name",   "resource_name"),
];

/// Returns a human-readable slot name by inspecting the tokens
/// immediately preceding the matched span.
///
/// # Arguments
/// * `preceding_tokens` – up to 3 whitespace-split tokens before the span
/// * `idx`              – fallback position index
fn name_slot(preceding_tokens: &[&str], idx: usize) -> String {
    for token in preceding_tokens.iter().rev() {
        let lower = token.to_lowercase();
        // Strip trailing punctuation so "file:" still matches "file"
        let lower = lower.trim_end_matches(|c: char| !c.is_alphanumeric());
        for (kw, slot_name) in KEYWORD_MAP {
            if lower.contains(kw) {
                return slot_name.to_string();
            }
        }
    }
    // Last resort — at least prefix makes it findable
    format!("param_{}", idx)
}
```

#### Integration point

In the existing regex substitution loop, collect the 3 tokens that precede the match start and pass them to `name_slot`:

```rust
let tokens_before: Vec<&str> = text[..cap.start()]
    .split_whitespace()
    .rev()
    .take(3)
    .collect();

let slot = name_slot(&tokens_before, slot_idx);
slot_idx += 1;
```

#### Expected outcome

| Before | After |
|--------|-------|
| `{{var_1_quoted}}` | `{{file_path}}` |
| `{{var_2_quoted}}` | `{{repo_url}}` |
| `{{var_3_quoted}}` | `{{api_token}}` |

---

### 1.2 Tool Dependency Mapping

**Priority:** 🔴 High

#### Problem

The heuristic fallback currently writes a **static, hardcoded** list into `required_tools`:

```rust
// Current — wrong for most skills
let required_tools = vec!["file_creator".to_string()];
```

This means skills often declare the wrong tool requirements, causing the Executor to either block valid runs (missing tool check fails) or allow runs without the tools the agent actually needs.

#### Solution

Walk the trajectory's tool call history and collect the distinct tool names used. This is the ground truth — the agent literally used exactly these tools to complete the task.

```rust
/// Extracts the set of tool names actually used across all turns in a trajectory.
/// Uses BTreeSet for deterministic ordering in the output YAML.
fn extract_required_tools(trajectory: &Trajectory) -> Vec<String> {
    trajectory
        .turns
        .iter()
        .flat_map(|turn| &turn.tool_calls)
        .map(|tool_call| tool_call.tool_name.clone())
        .collect::<std::collections::BTreeSet<_>>()  // dedup + sort
        .into_iter()
        .collect()
}
```

#### Integration point

Replace the static assignment in `heuristic_extract()`:

```rust
// REMOVE:
let required_tools = vec!["file_creator".to_string()];

// ADD:
let required_tools = extract_required_tools(&trajectory);
```

#### Notes

- If the trajectory used zero tools, `required_tools` will be an empty `Vec`, which is valid — the Executor will skip the tool-availability check.
- Internal/system tools (e.g. `__think__`, `__memory__`) should be filtered out if you have them:

```rust
const INTERNAL_TOOLS: &[&str] = &["__think__", "__memory__", "__plan__"];

fn extract_required_tools(trajectory: &Trajectory) -> Vec<String> {
    trajectory
        .turns
        .iter()
        .flat_map(|turn| &turn.tool_calls)
        .map(|tc| tc.tool_name.clone())
        .filter(|name| !INTERNAL_TOOLS.contains(&name.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}
```

---

### 1.3 Syntax-Aware Parsing

**Priority:** 🔴 High

#### Problem

When a user's input contains a JSON object or YAML block, the current regex substitution treats it as plain text and produces many separate `{{var_N}}` slots — one for every path, string, number, or URL inside the blob. This defeats reusability because the template becomes coupled to the exact structure of that particular JSON.

**Example — current behaviour:**

User input: `Process this config: {"host": "api.example.com", "port": 8080, "token": "abc123"}`

Current template output:
```
Process this config: {"host": {{var_1_quoted}}, "port": {{var_2_quoted}}, "token": {{var_3_quoted}}}
```

That is not a reusable template — it breaks the moment someone passes a config with different keys.

**Desired template output:**
```
Process this config: {{json_payload}}
```

#### Solution

Run a **structured block detection pass** before the regex substitution loop. Detect JSON objects/arrays and YAML front-matter, replace the entire block with a single typed parameter, and record it in the `params` list with `type: json` or `type: yaml`.

```rust
#[derive(Debug)]
enum ParamType {
    Text,
    Json,
    Yaml,
}

#[derive(Debug)]
struct StructuredBlock {
    /// The exact text span to replace
    span: String,
    /// The slot name to insert (e.g. "json_payload")
    slot: String,
    /// The declared type for the parameter schema
    kind: ParamType,
}

/// Detects JSON objects/arrays and YAML front-matter blocks in `text`.
/// Returns a list of blocks found, ordered by their start position.
fn detect_structured_blocks(text: &str) -> Vec<StructuredBlock> {
    let mut blocks = Vec::new();

    // --- JSON objects and arrays ---
    // Greedy match; validate with serde_json to avoid false positives.
    let json_re = Regex::new(r"(\{[\s\S]*?\}|\[[\s\S]*?\])").unwrap();
    for cap in json_re.captures_iter(text) {
        let span = cap[1].to_string();
        if serde_json::from_str::<serde_json::Value>(&span).is_ok() {
            blocks.push(StructuredBlock {
                span,
                slot: "json_payload".into(),
                kind: ParamType::Json,
            });
        }
    }

    // --- YAML front-matter (--- … ---) ---
    let yaml_re = Regex::new(r"(?m)^---\n([\s\S]*?)\n---").unwrap();
    for cap in yaml_re.captures_iter(text) {
        // Validate that it parses as YAML
        if serde_yaml::from_str::<serde_yaml::Value>(&cap[1]).is_ok() {
            blocks.push(StructuredBlock {
                span: cap[0].to_string(),
                slot: "yaml_config".into(),
                kind: ParamType::Yaml,
            });
        }
    }

    blocks
}
```

#### Integration point

Call this **before** the existing regex substitution loop, and register the detected blocks as typed parameters:

```rust
// In heuristic_extract():

let mut first_turn_text = trajectory.first_user_turn().to_string();
let mut params: Vec<Param> = Vec::new();

// 1. Detect and lift structured blocks first
let structured_blocks = detect_structured_blocks(&first_turn_text);
for block in &structured_blocks {
    // Replace the entire blob with a single slot
    first_turn_text = first_turn_text.replacen(
        &block.span,
        &format!("{{{{{}}}}}", block.slot),
        1,
    );
    params.push(Param {
        name: block.slot.clone(),
        param_type: match block.kind {
            ParamType::Json => "json".into(),
            ParamType::Yaml => "yaml".into(),
            ParamType::Text => "text".into(),
        },
        required: true,
        description: format!("Structured {} input", block.slot),
        ..Default::default()
    });
}

// 2. Now run the existing regex substitution on the cleaned text
// (JSON/YAML blobs are already replaced, so regex won't fragment them)
// … existing substitution code …
```

#### Dependency

Add `serde_yaml` to `Cargo.toml` if not already present:

```toml
[dependencies]
serde_yaml = "0.9"
```

---

## 2. Quality Gate Improvements

**File:** `crates/hydragent-skills/src/extractor.rs`

Bad skills entering the Candidate pool are more expensive to clean up than they are to reject at the gate. These two changes add meaningful signal without requiring LLM calls for every trajectory.

---

### 2.1 Richer `is_trajectory_successful` Logic

**Priority:** 🔴 High

#### Problem

The current gate accepts any trajectory where:
1. Tool logs contain no `"error"` substring, AND
2. The final message contains `"done"` or `"here is"`

This is too permissive. A trajectory whose final message says **"Here is the error I encountered, sorry I couldn't complete this"** passes both checks.

#### Solution

Add four independent boolean checks. Each check is a necessary condition — the trajectory must pass **all four**:

```rust
/// Failure language that should never appear in a successful final message.
static FAILURE_SIGNALS: &[&str] = &[
    "sorry",
    "i couldn't",
    "i was unable",
    "i cannot",
    "failed to",
    "partial result",
    "unfortunately",
    "let me try again",
    "i don't have",
    "i do not have",
];

/// Positive completion language. At least one must be present.
static SUCCESS_SIGNALS: &[&str] = &[
    "done",
    "here is",
    "here are",
    "completed",
    "successfully",
    "finished",
    "created",
    "updated",
    "the result",
    "output:",
    "result:",
];

fn is_trajectory_successful(trajectory: &Trajectory) -> bool {
    // Check 1: Minimum meaningful length (user turn + at least one agent turn)
    if trajectory.turns.len() < 2 {
        return false;
    }

    // Check 2: No tool errors in any turn's tool logs
    let tool_logs_clean = trajectory
        .turns
        .iter()
        .flat_map(|turn| &turn.tool_logs)
        .all(|log| {
            let lower = log.to_lowercase();
            !lower.contains("error")
                && !lower.contains("exception")
                && !lower.contains("traceback")
                && !lower.contains("panicked")
        });
    if !tool_logs_clean {
        return false;
    }

    let final_msg = trajectory.final_message.to_lowercase();

    // Check 3: No failure language in the final message
    let no_failure_language = FAILURE_SIGNALS
        .iter()
        .all(|&signal| !final_msg.contains(signal));
    if !no_failure_language {
        return false;
    }

    // Check 4: At least one positive completion signal present
    SUCCESS_SIGNALS
        .iter()
        .any(|&signal| final_msg.contains(signal))
}
```

#### Test cases to add

```rust
#[cfg(test)]
mod quality_gate_tests {
    use super::*;

    #[test]
    fn rejects_apology_with_positive_word() {
        // "here is the error" used to pass; should now fail
        let t = make_trajectory("Here is the error I encountered, sorry.");
        assert!(!is_trajectory_successful(&t));
    }

    #[test]
    fn rejects_single_turn_trajectory() {
        let t = make_trajectory_turns(1, "Done!");
        assert!(!is_trajectory_successful(&t));
    }

    #[test]
    fn rejects_tool_exception() {
        let t = make_trajectory_with_tool_log("thread panicked at main.rs:42");
        assert!(!is_trajectory_successful(&t));
    }

    #[test]
    fn accepts_clean_completion() {
        let t = make_trajectory("Here is the result: [output]");
        assert!(is_trajectory_successful(&t));
    }
}
```

---

### 2.2 Minimum Confidence Threshold

**Priority:** 🟡 Medium

#### Problem

The LLM extractor already produces a `confidence` score between 0.0 and 1.0, but it is only used for logging. Low-confidence proposals enter the Candidate pool, where they consume Curator cycles for 7 days before being demoted.

#### Solution

Reject proposals below a configurable threshold before they reach `SkillLibrary::insert_skill`. The recommended starting value is **0.72** — empirically, proposals below this tend to produce templates with too many generic slots.

```rust
// In SkillExtractor::extract_from_trajectory(), after the LLM call:

/// Minimum LLM confidence score required to induct a new skill.
/// Tune upward if too many low-quality skills are entering Candidate.
/// Tune downward if valid skills are being rejected.
const MIN_INDUCTION_CONFIDENCE: f32 = 0.72;

let proposed = llm_client.propose_skill(&trajectory_context).await?;

if proposed.confidence < MIN_INDUCTION_CONFIDENCE {
    tracing::debug!(
        skill_name = %proposed.name,
        confidence = proposed.confidence,
        threshold  = MIN_INDUCTION_CONFIDENCE,
        "Skill proposal rejected: confidence below threshold"
    );
    return Ok(None);
}

tracing::info!(
    skill_name = %proposed.name,
    confidence = proposed.confidence,
    "Skill proposal accepted — inserting as Candidate"
);
// … proceed to insert_skill …
```

#### Tuning guidance

| Scenario | Adjustment |
|----------|-----------|
| Too many low-quality Candidates being demoted | Raise threshold to 0.80 |
| Valid skills being rejected too often | Lower threshold to 0.65 |
| Library growing too slowly | Lower threshold to 0.60 (accept more, let Curator prune) |
| Library full of noise | Raise threshold to 0.85 |

---

## 3. Curator Upgrades

**File:** `crates/hydragent-skills/src/curator.rs`

The Curator is currently a good foundation but has two gaps that become painful at scale: it cannot detect skill duplication, and it can only demote — never recover — a degrading skill.

---

### 3.1 Semantic Deduplication Pass

**Priority:** 🟡 Medium

#### Problem

When auto-induction runs at volume, multiple Candidate skills for the same task accumulate. For example, a "summarise a GitHub PR" skill and a "summarise a GitHub issue" skill are semantically very close. Without deduplication, both enter the Active pool and compete for the same search queries, splitting execution counts and producing inconsistent results.

#### Solution

Add a deduplication step at the **start** of each daily Curator cycle, before the promotion/demotion checks. Compare each Candidate's description embedding against every Active skill's embedding. If the cosine similarity exceeds the threshold, merge by incrementing the Active skill's `execution_count` and archiving the Candidate.

```rust
// In SevenDayCurator::run_cycle():

/// Cosine similarity threshold above which two skills are considered duplicates.
/// 0.88 is intentionally high — only merge skills that are semantically almost identical.
const DEDUP_SIMILARITY_THRESHOLD: f32 = 0.88;

async fn deduplicate_candidates(
    &self,
    lib: &SkillLibrary,
) -> anyhow::Result<()> {
    let candidates = lib.list_by_tier(Tier::Candidate).await?;
    let actives    = lib.list_by_tier(Tier::Active).await?;

    for candidate in &candidates {
        // Embed the candidate's description
        let candidate_embedding = self
            .embed_client
            .embed(&candidate.description)
            .await?;

        for active in &actives {
            // Embed the active skill's description
            let active_embedding = self
                .embed_client
                .embed(&active.description)
                .await?;

            let similarity = cosine_similarity(&candidate_embedding, &active_embedding);

            if similarity > DEDUP_SIMILARITY_THRESHOLD {
                tracing::info!(
                    candidate = %candidate.name,
                    active    = %active.name,
                    similarity,
                    "Deduplication: merging candidate into active skill"
                );

                // Transfer the candidate's execution count to the active skill
                lib.increment_execution_count(
                    &active.id,
                    candidate.execution_count,
                ).await?;

                // Archive the duplicate candidate
                lib.set_tier(&candidate.id, Tier::Archived).await?;

                // Log to skill_versions for audit trail
                lib.record_version_event(
                    &active.id,
                    VersionEvent::DedupMerge {
                        merged_from: candidate.id.clone(),
                        merged_at: chrono::Utc::now(),
                    },
                ).await?;

                break; // Stop comparing this candidate against other actives
            }
        }
    }

    Ok(())
}
```

#### Call order in `run_cycle()`

```rust
pub async fn run_cycle(&self, lib: &SkillLibrary) -> anyhow::Result<()> {
    // Step 1: Dedup first — avoid promoting a skill that's a duplicate
    self.deduplicate_candidates(lib).await?;

    // Step 2: Promote high-performing Candidates → Active
    self.promote_candidates(lib).await?;

    // Step 3: Demote / rollback failing Active skills
    self.demote_or_rollback_actives(lib).await?;

    // Step 4: Archive long-idle skills
    self.archive_idle_skills(lib).await?;

    Ok(())
}
```

#### Cosine similarity helper

If not already in the codebase:

```rust
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Embedding dimension mismatch");
    let dot: f32  = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { return 0.0; }
    dot / (norm_a * norm_b)
}
```

---

### 3.2 Version Rollback on Demotion

**Priority:** 🟡 Medium

#### Problem

When an Active skill's success rate drops below 50%, it is currently moved straight to Inactive with no recovery path. But the most common cause of a sudden success-rate drop is a bad update to the skill's `prompt_template` — not the skill concept itself. The previous version may have been perfectly good.

#### Solution

Before demoting to Inactive, attempt to restore the previous version from `skill_versions`. If a previous version exists, restore it and give it one Curator cycle at Active status as a trial. Only demote if there is no previous version to roll back to, or if the rolled-back version also fails.

```rust
// In the demotion branch of run_cycle():

async fn demote_or_rollback_actives(
    &self,
    lib: &SkillLibrary,
) -> anyhow::Result<()> {
    let actives = lib.list_by_tier(Tier::Active).await?;

    for skill in actives {
        let success_rate = skill.success_rate;
        let max_failure_rate = self.config.demotion.max_failure_rate;

        if success_rate < (1.0 - max_failure_rate) {
            tracing::warn!(
                skill    = %skill.name,
                version  = skill.version,
                rate     = success_rate,
                "Active skill below success threshold — attempting rollback"
            );

            match lib.get_previous_version(&skill.id).await? {
                Some(prev_version) => {
                    tracing::info!(
                        skill   = %skill.name,
                        from_v  = skill.version,
                        to_v    = prev_version.version,
                        "Rolling back to previous version"
                    );
                    // Restore previous template and param schema
                    lib.restore_version(&skill.id, &prev_version).await?;
                    // Reset stats so the rolled-back version gets a fair trial
                    lib.reset_skill_stats(&skill.id).await?;
                    // Keep the skill Active — Curator will re-evaluate next cycle
                    // (no tier change needed)
                }
                None => {
                    tracing::info!(
                        skill = %skill.name,
                        "No previous version available — demoting to Inactive"
                    );
                    lib.set_tier(&skill.id, Tier::Inactive).await?;
                }
            }
        }
    }

    Ok(())
}
```

#### Required `library.rs` additions

```rust
impl SkillLibrary {
    /// Returns the most recent version entry before the skill's current version.
    pub async fn get_previous_version(
        &self,
        skill_id: &str,
    ) -> anyhow::Result<Option<SkillVersion>> {
        // Query skill_versions ordered by version DESC, skip the current (OFFSET 1)
        sqlx::query_as!(
            SkillVersion,
            "SELECT * FROM skill_versions
             WHERE skill_id = ?
             ORDER BY version DESC
             LIMIT 1 OFFSET 1",
            skill_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    /// Overwrites the live skill row with data from a historical version.
    pub async fn restore_version(
        &self,
        skill_id: &str,
        version: &SkillVersion,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "UPDATE skills
             SET prompt_template = ?, params = ?, version = ?, updated_at = ?
             WHERE id = ?",
            version.prompt_template,
            version.params_json,
            version.version,
            chrono::Utc::now().to_rfc3339(),
            skill_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Resets execution_count and success_rate after a rollback.
    pub async fn reset_skill_stats(&self, skill_id: &str) -> anyhow::Result<()> {
        sqlx::query!(
            "UPDATE skills SET execution_count = 0, success_rate = 0.0 WHERE id = ?",
            skill_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
```

---

### 3.3 Config-Driven Thresholds

**Priority:** 🟢 Low-effort / high-value

#### Problem

All Curator decision thresholds are hardcoded constants scattered across `curator.rs`. Tuning them requires a recompile and redeploy, which is impractical once the skill library is running in production.

#### Solution

Introduce `config/curator.toml` and load it at startup into a `CuratorConfig` struct. All thresholds read from the config at runtime.

**`config/curator.toml`**

```toml
# Curator configuration
# All values can be changed without recompiling — just restart the service.

[promotion]
# A Candidate must have this many executions before promotion is considered
min_executions   = 5
# And this success rate or higher
min_success_rate = 0.85

[demotion]
# An Active skill is demoted (or rolled back) if success rate drops below:
max_failure_rate = 0.50

[archival]
# A skill with no executions in this many days is moved to Archived
idle_days = 30

[deduplication]
# Cosine similarity threshold for merging a Candidate into an existing Active skill
similarity_threshold = 0.88

[induction]
# Minimum LLM confidence score to accept a proposed skill
min_confidence = 0.72
```

**`curator.rs` — config struct**

```rust
#[derive(Debug, serde::Deserialize, Clone)]
pub struct CuratorConfig {
    pub promotion:     PromotionConfig,
    pub demotion:      DemotionConfig,
    pub archival:      ArchivalConfig,
    pub deduplication: DeduplicationConfig,
    pub induction:     InductionConfig,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct PromotionConfig {
    pub min_executions:   u32,
    pub min_success_rate: f32,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct DemotionConfig {
    pub max_failure_rate: f32,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct ArchivalConfig {
    pub idle_days: u32,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct DeduplicationConfig {
    pub similarity_threshold: f32,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct InductionConfig {
    pub min_confidence: f32,
}

impl CuratorConfig {
    pub fn load() -> anyhow::Result<Self> {
        let config_str = std::fs::read_to_string("config/curator.toml")
            .context("Could not read config/curator.toml")?;
        toml::from_str(&config_str).context("Failed to parse curator config")
    }
}
```

---

## 4. New Built-in Skills

**Directory:** `skills/builtin/`

The current catalog of three skills (CSV→JSON, GitHub issue summary, Rust debug) covers a narrow slice of developer tasks. All six skills below can be added as YAML files immediately — they are picked up on the next `SkillLibrary::open` startup sync with no code changes required.

---

### 4.1 git-commit-message

**File:** `skills/builtin/git-commit-message.yaml`

**Purpose:** Generates a well-formed Conventional Commits message from a `git diff --staged` output.

```yaml
id: skill-builtin-git-commit-message
name: git-commit-message
version: 1
description: >
  Takes the output of `git diff --staged` and generates a Conventional Commits
  message with type, optional scope, a ≤72-char summary, and an optional body
  explaining why (not what). Supports feat, fix, chore, refactor, docs, test, ci.
tier: Active
capability_tags: [git, commit, conventional-commits, version-control]
author: hydragent-team

params:
  - name: staged_diff
    type: text
    required: true
    description: Output of `git diff --staged`. Paste the full diff.
  - name: ticket_id
    type: text
    required: false
    description: >
      Optional issue or ticket reference to append to the footer
      (e.g. "GH-42", "JIRA-1234"). Leave blank if none.

prompt_template: |
  You are a senior engineer writing a git commit message for the following staged diff.

  Staged diff:
  ```
  {{staged_diff}}
  ```
  {{#if ticket_id}}
  Related ticket: {{ticket_id}}
  {{/if}}

  Rules:
  1. Follow the Conventional Commits specification (https://www.conventionalcommits.org).
  2. Choose one type: feat | fix | refactor | docs | test | chore | ci | perf | style.
  3. Add a scope in parentheses only if the change is clearly scoped to one module or file.
  4. Keep the subject line under 72 characters.
  5. Body (optional): explain WHY, not WHAT. Skip if the subject line is self-explanatory.
  6. Footer: add `Refs: {{ticket_id}}` if a ticket was supplied.

  Output ONLY the commit message — no preamble, no explanation.

required_tools: []
success_examples:
  - input:
      staged_diff: "diff --git a/src/auth.rs …\n+    validate_jwt_expiry(token);"
      ticket_id: "GH-88"
    output: |
      fix(auth): validate JWT expiry before accepting tokens

      Tokens with an expired `exp` claim were being accepted because
      the expiry check was never called.

      Refs: GH-88
```

---

### 4.2 explain-shell-command

**File:** `skills/builtin/explain-shell-command.yaml`

**Purpose:** Explains what a shell command does, flag by flag, with safety warnings and modern alternatives.

```yaml
id: skill-builtin-explain-shell-command
name: explain-shell-command
version: 1
description: >
  Takes any shell command and explains it for a developer unfamiliar with it.
  Produces a one-sentence summary, a flag-by-flag table, a list of gotchas
  or destructive behaviours, and a safer or more modern alternative if one exists.
  Useful for onboarding, code review, and security audits.
tier: Active
capability_tags: [shell, bash, cli, linux, explanation, onboarding, security]
author: hydragent-team

params:
  - name: command
    type: text
    required: true
    description: The exact shell command to explain (including all flags and arguments).

prompt_template: |
  Explain this shell command to a developer who has never seen it before:

  ```sh
  {{command}}
  ```

  Your response must contain exactly these four sections:

  **1. Summary**
  One sentence. What does this command do?

  **2. Flag-by-flag breakdown**
  A table with columns: Flag | What it does | Default if omitted

  **3. Gotchas and risks**
  List any destructive, irreversible, or surprising behaviour.
  If the command is entirely safe, write "None."

  **4. Modern alternative**
  If a safer, faster, or more idiomatic equivalent exists in 2024
  (e.g. `fd` instead of `find`, `rg` instead of `grep -r`), show it.
  If none, write "None."

required_tools: []
success_examples:
  - input:
      command: "find . -name '*.log' -mtime +30 -delete"
    output: |
      **1. Summary**
      Deletes all `.log` files in the current directory tree that were last
      modified more than 30 days ago.

      **2. Flag-by-flag breakdown**
      | Flag | What it does | Default if omitted |
      |------|--------------|--------------------|
      | `.` | Starting directory for the search | Current directory |
      | `-name '*.log'` | Match files ending in `.log` | Match all files |
      | `-mtime +30` | Only files modified more than 30 days ago | All modification times |
      | `-delete` | Delete matched files in place | No deletion (print only) |

      **3. Gotchas and risks**
      - `-delete` is irreversible — there is no recycle bin.
      - Order matters: `-delete` must come AFTER all filter flags or it deletes too broadly.
      - Does not follow symlinks by default; add `-L` before `.` if needed.

      **4. Modern alternative**
      `fd --extension log --changed-before 30d --exec rm` — safer flag ordering,
      coloured output, and respects `.gitignore` by default.
```

---

### 4.3 query-json-with-jq

**File:** `skills/builtin/query-json-with-jq.yaml`

**Purpose:** Translates a plain-English question about a JSON payload into a `jq` expression, executes it, and explains the result.

> This skill directly exercises the new `ParamType::Json` introduced in [Fix 1.3](#13-syntax-aware-parsing).

```yaml
id: skill-builtin-query-json-with-jq
name: query-json-with-jq
version: 1
description: >
  Takes a raw JSON input and a plain-English question about its contents.
  Returns the correct jq expression, the extracted value, and a plain-language
  explanation of how the expression works. Handles nested paths, arrays,
  filtering, and aggregation.
tier: Active
capability_tags: [json, jq, data, query, extraction, cli]
author: hydragent-team

params:
  - name: json_payload
    type: json
    required: true
    description: The raw JSON object or array to query.
  - name: question
    type: text
    required: true
    description: >
      Plain-English question about the JSON content
      (e.g. "What are the names of all users with role admin?").

prompt_template: |
  JSON input:
  ```json
  {{json_payload}}
  ```

  Question: {{question}}

  Respond with exactly three sections:

  **jq expression**
  The complete jq expression that answers the question. Assume input is piped
  from stdin: `echo '...' | jq '<expression>'`

  **Result**
  The exact value that the expression would output, formatted as valid JSON.

  **Explanation**
  One or two sentences explaining how the expression works, written for
  someone learning jq for the first time.

required_tools: []
success_examples:
  - input:
      json_payload: |
        [
          {"name": "alice", "role": "admin", "age": 31},
          {"name": "bob",   "role": "viewer", "age": 25},
          {"name": "carol", "role": "admin", "age": 28}
        ]
      question: "What are the names of all users with role admin?"
    output: |
      **jq expression**
      `[.[] | select(.role == "admin") | .name]`

      **Result**
      `["alice", "carol"]`

      **Explanation**
      `.[]` unpacks the array into a stream of objects. `select(.role == "admin")`
      keeps only objects where the role field equals "admin". `.name` extracts
      just the name field, and the outer `[…]` collects the stream back into an array.
```

---

### 4.4 debug-python-traceback

**File:** `skills/builtin/debug-python-traceback.yaml`

**Purpose:** Python counterpart to the existing `debug-rust-error` skill. Diagnoses a traceback, provides a corrected snippet, and explains the fix.

```yaml
id: skill-builtin-debug-python-traceback
name: debug-python-traceback
version: 1
description: >
  Diagnoses a Python traceback and, optionally, the surrounding source code.
  Identifies the root cause, proposes a corrected code snippet, and explains
  why the fix resolves the issue. Mirrors debug-rust-error for Python projects.
tier: Active
capability_tags: [python, debug, traceback, error, exception]
author: hydragent-team

params:
  - name: traceback
    type: text
    required: true
    description: The full Python traceback (copy from terminal output).
  - name: source_snippet
    type: text
    required: false
    description: >
      The relevant source code around the error location.
      Optional but strongly improves diagnosis quality.
  - name: python_version
    type: text
    required: false
    default: "3"
    description: Python major version (e.g. "3.11"). Affects API suggestions.

prompt_template: |
  Python {{python_version}} traceback:
  ```
  {{traceback}}
  ```
  {{#if source_snippet}}
  Source code:
  ```python
  {{source_snippet}}
  ```
  {{/if}}

  Diagnose and fix this error. Respond with exactly three sections:

  **Root cause**
  One sentence. What went wrong and why?

  **Corrected code**
  A minimal, runnable code snippet that fixes the issue.
  Show only the changed lines with enough context to be clear.

  **Why this fixes it**
  Two to four sentences explaining the fix in plain language, suitable for
  a developer who is still learning Python.

required_tools: []
success_examples:
  - input:
      traceback: |
        Traceback (most recent call last):
          File "app.py", line 12, in <module>
            result = total / count
        ZeroDivisionError: division by zero
      source_snippet: |
        total = sum(scores)
        count = len([s for s in scores if s > 0])
        result = total / count
    output: |
      **Root cause**
      `count` is zero because no scores are greater than 0, and dividing
      `total` by zero raises `ZeroDivisionError`.

      **Corrected code**
      ```python
      count = len([s for s in scores if s > 0])
      result = total / count if count > 0 else 0.0
      ```

      **Why this fixes it**
      The guard `if count > 0` checks before dividing. If there are no
      qualifying scores, `result` is set to `0.0` as a safe default instead
      of crashing. This pattern — "check before divide" — is the standard
      way to handle potential zero denominators in Python.
```

---

### 4.5 generate-markdown-doc

**File:** `skills/builtin/generate-markdown-doc.yaml`

**Purpose:** Takes a function or struct signature plus an optional docstring and generates a complete Markdown documentation section ready to paste into a README or docs site.

```yaml
id: skill-builtin-generate-markdown-doc
name: generate-markdown-doc
version: 1
description: >
  Generates a Markdown documentation section for a function, method, or struct.
  Inputs are the code signature and an optional existing docstring.
  Output is a complete, consistently formatted Markdown block including
  a description, parameters table, return value, and usage example.
tier: Active
capability_tags: [docs, markdown, documentation, api, rust, python, typescript]
author: hydragent-team

params:
  - name: signature
    type: text
    required: true
    description: >
      The function, method, or struct signature exactly as it appears in source
      (e.g. `pub fn insert_skill(&self, skill: &Skill) -> anyhow::Result<()>`).
  - name: docstring
    type: text
    required: false
    description: Existing doc comment or docstring to expand. May be blank.
  - name: language
    type: text
    required: false
    default: "rust"
    description: >
      Programming language of the signature. Affects example syntax and
      parameter type formatting. Options: rust | python | typescript | go.

prompt_template: |
  Language: {{language}}

  Signature:
  ```{{language}}
  {{signature}}
  ```
  {{#if docstring}}
  Existing doc comment:
  {{docstring}}
  {{/if}}

  Generate a complete Markdown documentation section for this item.
  Use the following structure exactly:

  ## `<name>`

  <One or two sentence description of what this does and when to use it.>

  ### Parameters

  | Parameter | Type | Required | Description |
  |-----------|------|----------|-------------|
  | … | … | ✓ / — | … |

  ### Returns

  <What the return value represents. For Result/Option types, describe both branches.>

  ### Example

  ```{{language}}
  <A minimal, runnable example demonstrating the most common use case.>
  ```

required_tools: []
success_examples:
  - input:
      signature: "pub async fn get_previous_version(&self, skill_id: &str) -> anyhow::Result<Option<SkillVersion>>"
      language: "rust"
    output: |
      ## `get_previous_version`

      Returns the most recently stored historical version of a skill, falling back
      to `None` if the skill has never been updated and has only one version on record.
      Primarily used by the Curator to roll back a demoted skill.

      ### Parameters

      | Parameter | Type | Required | Description |
      |-----------|------|----------|-------------|
      | `skill_id` | `&str` | ✓ | The unique identifier of the skill to query. |

      ### Returns

      `Ok(Some(SkillVersion))` — the previous version record if one exists.
      `Ok(None)` — the skill has no prior version (it was never updated).
      `Err(…)` — a database error occurred.

      ### Example

      ```rust
      let prev = library.get_previous_version("skill-builtin-debug-rust-error").await?;
      if let Some(v) = prev {
          println!("Previous version: {}", v.version);
      }
      ```
```

---

### 4.6 find-files-by-pattern

**File:** `skills/builtin/find-files-by-pattern.yaml`

**Purpose:** Translates a natural-language file search description into a `fd` (or `find`) command, executes it via `bash_tool`, and returns the results grouped by directory.

```yaml
id: skill-builtin-find-files-by-pattern
name: find-files-by-pattern
version: 1
description: >
  Translates a plain-English description of what to search for into the
  appropriate `fd` or `find` shell command, executes it, and returns matched
  paths grouped by directory. Use for tasks like "find all Rust test files
  modified in the last week" or "find every YAML file under config/".
tier: Active
capability_tags: [files, search, find, fd, shell, filesystem]
author: hydragent-team

required_tools: [bash_tool]

params:
  - name: description
    type: text
    required: true
    description: Plain-English description of what to find (file type, location, age, size, etc.).
  - name: root_dir
    type: text
    required: false
    default: "."
    description: Directory to search from. Defaults to the current working directory.
  - name: prefer_fd
    type: text
    required: false
    default: "true"
    description: >
      Set to "true" to prefer `fd` over `find` when available. `fd` respects
      .gitignore and is generally faster. Set to "false" to force POSIX `find`.

prompt_template: |
  Root directory: {{root_dir}}
  Search description: {{description}}
  Prefer fd: {{prefer_fd}}

  Step 1 — Build the command.
  If prefer_fd is "true", write an `fd` command. Fall back to `find` only if
  the description requires a flag that fd does not support.

  Step 2 — Execute the command using bash_tool.

  Step 3 — Format and return the results.
  Group the matching paths by parent directory. If no files match, say so clearly.
  If more than 50 files match, show the first 50 and report the total count.

  Always show the exact command you ran before the results.

success_examples:
  - input:
      description: "All Rust test files modified in the last 7 days"
      root_dir: "."
    output: |
      Command run:
      ```sh
      fd --extension rs --changed-within 7d --exec grep -l '#\[test\]' {}
      ```

      Results (3 files):
      **crates/hydragent-skills/src/**
      - extractor_tests.rs
      - curator_tests.rs

      **crates/hydragent-core/src/**
      - integration_tests.rs
```

---

## Summary Table

| # | Improvement | File | Priority | Effort |
|---|-------------|------|----------|--------|
| 1.1 | Smart parameter naming | `extractor.rs` | 🔴 High | Low (~30 lines) |
| 1.2 | Auto-extract `required_tools` | `extractor.rs` | 🔴 High | Low (~10 lines) |
| 1.3 | JSON/YAML syntax-aware parsing | `extractor.rs` | 🔴 High | Medium (~60 lines) |
| 2.1 | Richer quality gate | `extractor.rs` | 🔴 High | Low (~40 lines) |
| 2.2 | Min confidence threshold | `extractor.rs` | 🟡 Medium | Trivial (~5 lines) |
| 3.1 | Semantic deduplication | `curator.rs` | 🟡 Medium | Medium (~50 lines) |
| 3.2 | Version rollback on demotion | `curator.rs` + `library.rs` | 🟡 Medium | Medium (~40 lines) |
| 3.3 | Config-driven thresholds | `config/curator.toml` | 🟢 Low-effort | Low (~30 lines) |
| 4.1 | Built-in: git-commit-message | `skills/builtin/` | 🟢 Low-effort | YAML only |
| 4.2 | Built-in: explain-shell-command | `skills/builtin/` | 🟢 Low-effort | YAML only |
| 4.3 | Built-in: query-json-with-jq | `skills/builtin/` | 🟢 Low-effort | YAML only |
| 4.4 | Built-in: debug-python-traceback | `skills/builtin/` | 🟢 Low-effort | YAML only |
| 4.5 | Built-in: generate-markdown-doc | `skills/builtin/` | 🟢 Low-effort | YAML only |
| 4.6 | Built-in: find-files-by-pattern | `skills/builtin/` | 🟢 Low-effort | YAML only |

## Recommended Implementation Order

1. **Start here (all in `extractor.rs`):** Fix 1.1 → 1.2 → 2.1 → 2.2 → 1.3
   - These are independent of each other and can be PR'd separately.
   - Fixes 1.1 and 1.2 are the smallest code changes with the biggest quality impact.

2. **Then (config + curator):** Fix 3.3 → 3.2 → 3.1
   - Do 3.3 (config) first so the thresholds in 3.1 and 3.2 read from config, not constants.
   - 3.1 requires the embedding client to be available — check if `hydragent-embed` is wired into the Curator before starting.

3. **Last (new skills):** Add the six YAML files in any order.
   - No code changes required. Drop them in `skills/builtin/` and restart — they'll be auto-synced.
   - Fixes 4.3 (`query-json-with-jq`) will benefit even more once Fix 1.3 is in place.
