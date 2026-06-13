//! # Swarm Supervisor — post-run synthesis & final-response aggregation
//!
//! Phase 5 / Track 5.4. Once a `DagExecutionEngine` has finished
//! running every node in a DAG, the swarm still owes the user a
//! single coherent final answer. The supervisor is the component that
//! turns the per-node outputs into that answer.
//!
//! ## Strategy
//!
//! The supervisor is intentionally a *thin* layer over the
//! `ModelProvider` (the same one that powers the sub-agents). It
//! collects the terminal node outputs in topological-ish order, then
//! asks the model to **synthesise** them into one Markdown response.
//!
//! Why a synthesis LLM call rather than `join("\n")` or a
//! hand-written heuristic? Two reasons:
//!
//! 1. **Quality** — the LLM can dedupe, resolve conflicts, and write
//!    the answer in the user's voice rather than a stilted
//!    "Node A said: ... Node B said: ..." dump.
//! 2. **Reuse** — the same router that picked models for the
//!    sub-agents picks a model for the synthesis step. We log
//!    `status.model_used` for parity with `SubAgentStatus`.
//!
//! ## Failure handling
//!
//! If the synthesis call fails, the supervisor falls back to a
//! best-effort plain-text concatenation. The `aggregate` method
//! never returns `Err` — it always produces a `SupervisedResponse`
//! that contains at least the raw concatenation. A `warn!` is
//! emitted on the way.
//!
//! ## Example
//!
//! ```no_run
//! use hydragent_swarm::supervisor::Supervisor;
//! use std::sync::Arc;
//! use hydragent_model::router::ModelRouter;
//! use hydragent_model::ModelProvider;
//!
//! # async fn demo(provider: Arc<dyn ModelProvider>) -> anyhow::Result<()> {
//! let router = Arc::new(ModelRouter::new(provider, "router-primary".into(), vec![]));
//! let sup = Supervisor::new(router);
//! let response = sup.aggregate("Build a CLI todo app", &[
//!     ("research".into(), "Rust + clap is the right pick".into()),
//!     ("design".into(), "Three modules: cli, store, todo".into()),
//! ]).await?;
//! assert!(!response.content.is_empty());
//! # Ok(()) }
//! ```

use std::sync::Arc;
use std::time::Instant;

use hydragent_model::openrouter::{ChatMessage, LLMRequest};
use hydragent_model::router::ModelRouter;
use hydragent_model::ModelProvider;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// The supervisor's final output: a synthesised answer plus the
/// per-node sources that fed it, so callers can show the trail if
/// the user asks "where did that come from?".
#[derive(Debug, Clone)]
pub struct SupervisedResponse {
    /// The final answer returned to the user / channel.
    pub content: String,
    /// The model id that synthesised the answer. `"fallback"` if the
    /// synthesis call failed and we fell back to concatenation.
    pub model_used: String,
    /// Wall-clock duration of the synthesis call (ms). 0 for
    /// fallback.
    pub elapsed_ms: u64,
    /// `(node_name, node_output)` for every terminal node that fed
    /// the synthesis, in the order the supervisor saw them.
    pub sources: Vec<(String, String)>,
    /// `true` if the response is a plain concatenation fallback
    /// (synthesis call failed).
    pub fell_back: bool,
}

/// Stateless: the supervisor holds only a model router, and is
/// cheap to construct. Wrap in `Arc` and share across the swarm.
pub struct Supervisor {
    router: Arc<ModelRouter>,
    /// The model id the supervisor should use for the synthesis call.
    /// `None` = use the router's primary model.
    synthesis_model: Option<String>,
    /// Token budget for the synthesis call. Defaults to 2000.
    max_tokens: u32,
}

impl Supervisor {
    /// Build a supervisor that uses the router's primary model for
    /// synthesis and a 2000-token budget.
    pub fn new(router: Arc<ModelRouter>) -> Self {
        Self {
            router,
            synthesis_model: None,
            max_tokens: 2_000,
        }
    }

    /// Override the model used for synthesis (otherwise the router
    /// picks from the configured council / fallback chain).
    pub fn with_synthesis_model(mut self, model_id: impl Into<String>) -> Self {
        self.synthesis_model = Some(model_id.into());
        self
    }

    /// Override the synthesis token budget. Defaults to 2000.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Aggregate per-node outputs into a single coherent answer.
    ///
    /// `original_task` is the user-facing task the swarm was set up
    /// to solve. It is included verbatim in the synthesis prompt so
    /// the LLM can frame the answer against the original ask.
    /// `node_outputs` is `(node_name, node_output)` pairs; the
    /// supervisor preserves the caller's order (typically
    /// topological).
    pub async fn aggregate(
        &self,
        original_task: &str,
        node_outputs: &[(String, String)],
    ) -> anyhow::Result<SupervisedResponse> {
        if node_outputs.is_empty() {
            return Ok(SupervisedResponse {
                content: String::new(),
                model_used: String::new(),
                elapsed_ms: 0,
                sources: Vec::new(),
                fell_back: false,
            });
        }

        // Trivial case: a single node. Don't bother calling the LLM
        // to "synthesise" one source — return it verbatim. Saves
        // tokens, latency, and avoids a useless second model hop.
        if node_outputs.len() == 1 {
            return Ok(SupervisedResponse {
                content: node_outputs[0].1.clone(),
                model_used: "passthrough".into(),
                elapsed_ms: 0,
                sources: node_outputs.to_vec(),
                fell_back: false,
            });
        }

        let started = Instant::now();
        let prompt = build_synthesis_prompt(original_task, node_outputs);
        let model = self.synthesis_model.clone().unwrap_or_default();
        let request = LLMRequest {
            model,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: prompt,
            }],
            max_tokens: Some(self.max_tokens),
            stream: false,
        };

        // We don't need a streaming channel for synthesis — it's a
        // single-shot call. Use a bounded channel that we ignore
        // (the non-streaming codepath doesn't write to `tx`).
        let (tx, _rx) = mpsc::channel::<String>(1);
        let provider: Arc<dyn ModelProvider> = self.router.provider();
        let result = provider.chat_stream(&request, tx).await;

        match result {
            Ok(raw) => {
                let content = parse_synthesis(&raw);
                info!(
                    nodes = node_outputs.len(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    out_bytes = content.len(),
                    "supervisor.aggregate: synthesis complete"
                );
                Ok(SupervisedResponse {
                    content,
                    model_used: self.router.provider_label().to_string(),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    sources: node_outputs.to_vec(),
                    fell_back: false,
                })
            }
            Err(e) => {
                warn!(error = %e, "supervisor.aggregate: synthesis LLM call failed, falling back to concatenation");
                Ok(SupervisedResponse {
                    content: concatenate_fallback(node_outputs),
                    model_used: "fallback".into(),
                    elapsed_ms: 0,
                    sources: node_outputs.to_vec(),
                    fell_back: true,
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build the synthesis prompt. The LLM is asked to behave like a
/// senior editor producing a final report — not a stenographer.
fn build_synthesis_prompt(original_task: &str, nodes: &[(String, String)]) -> String {
    let mut s = String::new();
    s.push_str("You are the final-response synthesizer for a swarm of specialist sub-agents.\n");
    s.push_str("Your job: read the per-node findings below and produce ONE coherent final\n");
    s.push_str("answer to the user's original task. The user has NOT seen the per-node\n");
    s.push_str("outputs — only what you write will reach them.\n\n");
    s.push_str("## Original task\n");
    s.push_str(original_task);
    s.push_str("\n\n## Sub-agent findings (in order of completion)\n\n");
    for (i, (name, content)) in nodes.iter().enumerate() {
        s.push_str(&format!("### {}. {}\n", i + 1, name));
        s.push_str(content.trim());
        s.push_str("\n\n");
    }
    s.push_str("## Your output\n");
    s.push_str("Write a single, coherent Markdown response that:\n");
    s.push_str("  * directly answers the original task,\n");
    s.push_str("  * dedupes, reconciles, and orders the findings sensibly,\n");
    s.push_str("  * uses the user's voice (no \"Sub-agent A said...\" preamble),\n");
    s.push_str("  * is concise but complete — the user should not need to read the\n");
    s.push_str("    per-node findings to understand the answer.\n");
    s
}

/// Extract a usable answer from the LLM's raw output. We accept:
///   * a plain string,
///   * a `{"answer": "..."}` JSON object (matches the spawner's
///     tool-loop contract),
///   * a `{"thought": "...", "answer": "..."}` object,
///   * a `{"thought": "...", "final_answer": "..."}` object (used by
///     some sub-agent prompts).
fn parse_synthesis(raw: &str) -> String {
    let trimmed = raw.trim();
    // Try JSON parse first.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(s) = v.get("answer").and_then(|x| x.as_str()) {
            return s.to_string();
        }
        if let Some(s) = v.get("final_answer").and_then(|x| x.as_str()) {
            return s.to_string();
        }
        if let Some(s) = v.get("content").and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    trimmed.to_string()
}

/// Plain-text fallback when the LLM call fails. Each node is
/// rendered as a Markdown section.
fn concatenate_fallback(nodes: &[(String, String)]) -> String {
    let mut s = String::new();
    s.push_str("(Synthesis unavailable — showing per-node findings verbatim.)\n\n");
    for (name, content) in nodes {
        s.push_str(&format!("## {}\n", name));
        s.push_str(content.trim());
        s.push_str("\n\n");
    }
    s
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_synthesis_accepts_plain_string() {
        assert_eq!(parse_synthesis("hello world"), "hello world");
        assert_eq!(parse_synthesis("  spaced  \n"), "spaced");
    }

    #[test]
    fn parse_synthesis_extracts_answer_field() {
        let json = r#"{"thought":"thinking...","answer":"the actual answer"}"#;
        assert_eq!(parse_synthesis(json), "the actual answer");
    }

    #[test]
    fn parse_synthesis_extracts_final_answer_field() {
        let json = r#"{"thought":"thinking...","final_answer":"the real deal"}"#;
        assert_eq!(parse_synthesis(json), "the real deal");
    }

    #[test]
    fn parse_synthesis_extracts_content_field() {
        let json = r#"{"content":"a report body"}"#;
        assert_eq!(parse_synthesis(json), "a report body");
    }

    #[test]
    fn parse_synthesis_passthrough_on_unrecognised_json() {
        let json = r#"{"weird":"shape"}"#;
        // No recognised field — pass through trimmed.
        assert_eq!(parse_synthesis(json), json);
    }

    #[test]
    fn concatenate_fallback_includes_all_nodes() {
        let nodes = vec![
            ("A".to_string(), "alpha".to_string()),
            ("B".to_string(), "beta".to_string()),
        ];
        let out = concatenate_fallback(&nodes);
        assert!(out.contains("## A"));
        assert!(out.contains("alpha"));
        assert!(out.contains("## B"));
        assert!(out.contains("beta"));
        assert!(out.contains("Synthesis unavailable"));
    }

    #[test]
    fn synthesis_prompt_contains_original_task_and_nodes() {
        let nodes = vec![
            ("research".to_string(), "findings A".to_string()),
            ("plan".to_string(), "plan B".to_string()),
        ];
        let p = build_synthesis_prompt("Build a CLI todo app", &nodes);
        assert!(p.contains("Build a CLI todo app"));
        assert!(p.contains("### 1. research"));
        assert!(p.contains("findings A"));
        assert!(p.contains("### 2. plan"));
        assert!(p.contains("plan B"));
    }
}
