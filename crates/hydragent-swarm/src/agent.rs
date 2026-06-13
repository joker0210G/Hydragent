use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use hydragent_types::{
    AgentState, MessageRole, PermissionTier, SubAgentRole, SubAgentSpec, SubAgentStatus,
    ToolCall, ToolResult, ToolStatus,
};
use hydragent_tools::registry::ToolRegistry;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use hydragent_model::router::ModelRouter;

use crate::spawner::SubAgentSpawner;

/// Errors a sub-agent can surface.
#[derive(Debug, Error)]
pub enum SubAgentError {
    /// The spec references a tool the sub-agent is not allowed to use.
    #[error("tool '{0}' is not in the sub-agent's allowlist")]
    ToolNotAllowed(String),
    /// The shared tool call failed at the registry level.
    #[error("tool call failed: {0}")]
    ToolFailure(String),
    /// The LLM call failed.
    #[error("llm call failed: {0}")]
    Llm(String),
    /// Sub-agent exceeded its token budget.
    #[error("token budget exhausted: used {used} / {budget}")]
    TokenBudgetExhausted {
        /// Approximate number of tokens consumed.
        used: u32,
        /// Configured ceiling.
        budget: u32,
    },
    /// Sub-agent hit its wall-clock timeout.
    #[error("sub-agent timed out after {0} ms")]
    Timeout(u64),
    /// The tool result was not parseable JSON.
    #[error("invalid tool result: {0}")]
    InvalidToolResult(String),
}

/// A running sub-agent. One instance per spec; cloned handles are cheap
/// (inner state is `Arc<Mutex<...>>`).
///
/// The sub-agent is **stateless across calls** — every call to `run` starts
/// from a clean tool-loop and produces a single `SubAgentStatus`. Concurrency
/// control happens one level up in [`SwarmCoordinator`].
#[derive(Clone)]
pub struct SubAgent {
    /// Original spec (immutable for the lifetime of this sub-agent).
    spec: SubAgentSpec,
    /// Shared tool registry (read-only reference, no mutation).
    registry: Arc<ToolRegistry>,
    /// Shared model router (clones cheaply, internally `Arc`).
    router: Arc<ModelRouter>,
    /// Optional per-run cancel signal (set by coordinator on `cancel`).
    cancelled: Arc<Mutex<bool>>,
}

impl std::fmt::Debug for SubAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubAgent")
            .field("id", &self.spec.id)
            .field("name", &self.spec.name)
            .field("role", &self.spec.role)
            .finish()
    }
}

impl SubAgent {
    /// Build a new sub-agent from a spec, registry, and router.
    pub fn new(
        spec: SubAgentSpec,
        registry: Arc<ToolRegistry>,
        router: Arc<ModelRouter>,
    ) -> Self {
        Self {
            spec,
            registry,
            router,
            cancelled: Arc::new(Mutex::new(false)),
        }
    }

    /// Borrow the spec.
    pub fn spec(&self) -> &SubAgentSpec {
        &self.spec
    }

    /// Build the system prompt for this sub-agent. If `spec.system_prompt`
    /// is empty, a role-derived default is used; the available tool list is
    /// always the **allowlist-filtered** view, never the full registry.
    fn build_system_prompt(&self) -> String {
        let allow: HashSet<&str> = self
            .spec
            .allowed_tools
            .iter()
            .map(String::as_str)
            .collect();

        // Filter the registry's tool block down to the allowlist.
        let filtered_block = self
            .registry
            .build_system_prompt_block()
            .lines()
            .filter(|line| {
                // Tool lines start with "- **<name>**:". Match by name token.
                if let Some(rest) = line.strip_prefix("- **") {
                    if let Some(name) = rest.split("**").next() {
                        return allow.contains(name);
                    }
                }
                // Keep blank lines / continuations only if previous matched; for
                // simplicity, also keep lines that start with whitespace (JSON
                // schema continuations) — they belong to a tool block above.
                // We err on the side of *more* tools in the prompt: anything we
                // can't classify as a tool header line is dropped.
                false
            })
            .collect::<Vec<_>>()
            .join("\n");

        let role_prompt = if self.spec.system_prompt.is_empty() {
            default_role_prompt(self.spec.role, &self.spec.name)
        } else {
            self.spec.system_prompt.clone()
        };

        format!(
            "{}\n\n\
            # Allowed Tools (allowlist-enforced)\n\
            {}\n\n\
            # Operating Constraints\n\
            - Token budget: {} tokens\n\
            - Wall-clock budget: {} ms\n\
            - Role: {:?}\n\
            - Sub-agent ID: {}\n\n\
            # Output Format\n\
            Respond with a single JSON object on a single line, with no markdown \
            wrapping. Choose one of two shapes:\n\n\
            1. To call a tool:\n\
            {{\"thought\": \"...\", \"tool\": \"<one of the allowed tools>\", \"params\": {{...}}}}\n\n\
            2. To provide the final answer:\n\
            {{\"thought\": \"...\", \"answer\": \"<your markdown response>\"}}\n",
            role_prompt,
            filtered_block,
            self.spec.token_budget,
            self.spec.timeout_ms,
            self.spec.role,
            self.spec.id,
        )
    }

    /// Look up the effective permission tier of a tool **as invoked by this
    /// sub-agent**. Tools not in the allowlist are reported as `Deny`.
    fn effective_tier(&self, tool_id: &str) -> PermissionTier {
        if !self.spec.allowed_tools.iter().any(|t| t == tool_id) {
            return PermissionTier::Deny;
        }
        self.registry.get_tier(tool_id)
    }

    /// Run the sub-agent to completion. Returns a [`SubAgentStatus`] either
    /// way (success, failure, or cancellation). Never panics.
    pub async fn run(self) -> SubAgentStatus {
        let start = Instant::now();
        let id = self.spec.id.clone();
        let name = self.spec.name.clone();
        let role = self.spec.role;
        let swarm_id = self.spec.swarm_id.clone();
        let parent_page_id = self.spec.parent_page_id.clone();

        info!(sub_agent_id = %id, role = ?role, name = %name, "Sub-agent starting");

        let mut status = SubAgentStatus {
            id: id.clone(),
            name: name.clone(),
            role,
            swarm_id: swarm_id.clone(),
            parent_page_id: parent_page_id.clone(),
            state: AgentState::Running,
            model_used: String::new(),
            tokens_used: 0,
            elapsed_ms: 0,
            output: String::new(),
            tool_calls: Vec::new(),
            error: None,
        };

        let system_prompt = self.build_system_prompt();
        let user_task = self.spec.task.clone();

        // Step 0: ask the LLM for either a tool call or a final answer.
        // We bound the tool loop to 5 iterations — plenty for Track 5.1.
        const MAX_LOOP_STEPS: u8 = 5;
        let mut transcript: Vec<(MessageRole, String)> = Vec::new();
        transcript.push((MessageRole::System, system_prompt.clone()));
        transcript.push((MessageRole::User, user_task.clone()));

        for step in 0..MAX_LOOP_STEPS {
            // Cooperative cancel check.
            if *self.cancelled.lock().await {
                status.state = AgentState::Cancelled;
                status.error = Some("cancelled by coordinator".to_string());
                status.elapsed_ms = start.elapsed().as_millis() as u64;
                warn!(sub_agent_id = %id, "Sub-agent cancelled mid-loop");
                return status;
            }

            // Wall-clock timeout check.
            if start.elapsed().as_millis() as u64 > self.spec.timeout_ms {
                status.state = AgentState::Cancelled;
                status.error = Some(format!(
                    "timeout after {} ms (budget {} ms)",
                    start.elapsed().as_millis() as u64,
                    self.spec.timeout_ms
                ));
                status.elapsed_ms = start.elapsed().as_millis() as u64;
                warn!(sub_agent_id = %id, "Sub-agent timed out");
                return status;
            }

            // Token-budget check (rough estimate: sum of transcript chars / 4).
            let approx_tokens: u32 = transcript
                .iter()
                .map(|(_, s)| (s.len() as u32).div_ceil(4))
                .sum();
            if approx_tokens > self.spec.token_budget {
                status.state = AgentState::Failed;
                status.error = Some(format!(
                    "token budget exhausted: ~{} / {}",
                    approx_tokens, self.spec.token_budget
                ));
                status.elapsed_ms = start.elapsed().as_millis() as u64;
                warn!(sub_agent_id = %id, "Sub-agent over token budget");
                return status;
            }
            status.tokens_used = approx_tokens;

            // Render transcript to a single prompt for the non-streaming API.
            let prompt = render_transcript(&transcript);

            debug!(sub_agent_id = %id, step, "Calling LLM");
            let raw = match self.router.generate_non_streaming(&prompt, self.spec.model_hint.as_deref()).await {
                Ok(s) => s,
                Err(e) => {
                    status.state = AgentState::Failed;
                    status.error = Some(format!("LLM error: {}", e));
                    status.elapsed_ms = start.elapsed().as_millis() as u64;
                    return status;
                }
            };
            // Record which model was actually used for this step.
            // When the spec's `model_hint` is set (e.g. by the council),
            // the router will try *only* that model — so the reported
            // model_used is the hint itself. Otherwise we fall back to
            // the router's primary model_id.
            status.model_used = self
                .spec
                .model_hint
                .clone()
                .unwrap_or_else(|| self.router.provider_label().to_string());

            // Parse the JSON response.
            let parsed: Value = match extract_json_obj(&raw) {
                Some(v) => v,
                None => {
                    // LLM sometimes ignores the JSON rule and writes prose.
                    // Treat that as the final answer.
                    status.state = AgentState::Completed;
                    status.output = raw.trim().to_string();
                    status.elapsed_ms = start.elapsed().as_millis() as u64;
                    info!(sub_agent_id = %id, "Sub-agent produced prose answer");
                    return status;
                }
            };

            let thought = parsed
                .get("thought")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            // Branch 1: tool call.
            if let Some(tool_name) = parsed.get("tool").and_then(Value::as_str) {
                let params = parsed
                    .get("params")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let params_json = serde_json::to_string(&params).unwrap_or("{}".to_string());

                let call_id = format!("{}-{}", id, status.tool_calls.len());
                let call = ToolCall {
                    call_id: call_id.clone(),
                    tool_id: tool_name.to_string(),
                    params_json: params_json.clone(),
                    tier: self.effective_tier(tool_name),
                };

                // Allowlist gate (in addition to tier, this is the swarm-specific
                // guarantee: a sub-agent cannot call a tool it wasn't told it
                // could call, even if the tool itself is AutoApprove tier).
                if call.tier == PermissionTier::Deny {
                    let result = ToolResult {
                        call_id: call_id.clone(),
                        output_json: "{}".to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: 0,
                        error_message: Some(format!(
                            "tool '{}' denied (not in allowlist)",
                            tool_name
                        )),
                    };
                    status.tool_calls.push(call);
                    transcript.push((
                        MessageRole::Tool,
                        format!("TOOL_RESULT {}: {}", tool_name, "denied"),
                    ));
                    debug!(sub_agent_id = %id, tool = tool_name, "Tool denied by allowlist");
                    let _ = result;
                } else {
                    let result = self.registry.invoke(&call).await;
                    let ok = matches!(result.status, ToolStatus::Success);
                    let summary = if ok {
                        truncate(&result.output_json, 500)
                    } else {
                        format!(
                            "ERROR: {}",
                            result
                                .error_message
                                .clone()
                                .unwrap_or_else(|| "unknown".into())
                        )
                    };
                    status.tool_calls.push(call);
                    transcript.push((
                        MessageRole::Tool,
                        format!("TOOL_RESULT {}: {}", tool_name, summary),
                    ));
                    debug!(sub_agent_id = %id, tool = tool_name, ok, "Tool executed");
                }
                let _ = thought; // (logged via the transcript)
                continue;
            }

            // Branch 2: final answer.
            if let Some(answer) = parsed.get("answer").and_then(Value::as_str) {
                status.state = AgentState::Completed;
                status.output = answer.to_string();
                status.elapsed_ms = start.elapsed().as_millis() as u64;
                info!(
                    sub_agent_id = %id,
                    elapsed_ms = status.elapsed_ms,
                    "Sub-agent completed"
                );
                return status;
            }

            // Neither branch matched — treat as completion with whatever we have.
            status.state = AgentState::Completed;
            status.output = raw;
            status.elapsed_ms = start.elapsed().as_millis() as u64;
            return status;
        }

        // Loop exhausted without an answer.
        status.state = AgentState::Failed;
        status.error = Some(format!("exceeded {} loop steps without final answer", MAX_LOOP_STEPS));
        status.elapsed_ms = start.elapsed().as_millis() as u64;
        status
    }

    /// Signal this sub-agent to cancel. Cooperative: the next loop iteration
    /// (or the next `await` point) will see the flag and bail.
    pub async fn cancel(&self) {
        *self.cancelled.lock().await = true;
    }

    /// Build via a [`SubAgentSpawner`]. Convenience for callers that already
    /// have a spawner in hand.
    pub fn from_spawner(
        spawner: &SubAgentSpawner,
        spec: SubAgentSpec,
    ) -> Self {
        Self::new(spec, spawner.registry_clone(), spawner.router_clone())
    }
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

fn default_role_prompt(role: SubAgentRole, name: &str) -> String {
    let role_str = format!("{:?}", role);
    format!(
        "You are a specialist sub-agent in a hydragent swarm. Your task name is \
        \"{}\" and your role is {}. Stay focused, be concise, and only use the tools \
        in your allowlist. Produce a final answer in JSON form when you're done — do \
        not narrate intermediate steps to the user.",
        name, role_str
    )
}

/// Render the transcript as a flat prompt with role tags. The non-streaming
/// API takes a single string, so we inline the role markers.
fn render_transcript(transcript: &[(MessageRole, String)]) -> String {
    let mut out = String::new();
    for (role, content) in transcript {
        let tag = match role {
            MessageRole::System => "[SYSTEM]",
            MessageRole::User => "[USER]",
            MessageRole::Assistant => "[ASSISTANT]",
            MessageRole::Tool => "[TOOL]",
        };
        out.push_str(tag);
        out.push('\n');
        out.push_str(content);
        out.push_str("\n\n");
    }
    out.push_str("[ASSISTANT]\n");
    out
}

/// Pull the first JSON object out of a string (handles ```json fences and
/// leading prose). Returns `None` if no `{...}` block is found.
fn extract_json_obj(s: &str) -> Option<Value> {
    // Strip ```json fences if present.
    let cleaned = s
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Find first '{' and walk braces to find the matching '}'.
    let start = cleaned.find('{')?;
    let bytes = cleaned.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if escape {
            escape = false;
            continue;
        }
        if b == b'\\' && in_string {
            escape = true;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                let slice = &cleaned[start..=i];
                return serde_json::from_str(slice).ok();
            }
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ----------------------------------------------------------------------------
// Unit tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_obj_handles_fenced() {
        let s = "```json\n{\"a\": 1}\n```";
        let v = extract_json_obj(s).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn extract_json_obj_handles_prose_wrapping() {
        let s = "Here is my plan:\n{\"tool\": \"echo\", \"params\": {\"message\": \"hi\"}}\nDone.";
        let v = extract_json_obj(s).unwrap();
        assert_eq!(v["tool"], "echo");
    }

    #[test]
    fn extract_json_obj_handles_nested_braces() {
        let s = r#"{"thought":"x","params":{"a":{"b":1}}}"#;
        let v = extract_json_obj(s).unwrap();
        assert_eq!(v["params"]["a"]["b"], 1);
    }

    #[test]
    fn extract_json_obj_returns_none_for_no_json() {
        let s = "just a plain answer with no braces";
        assert!(extract_json_obj(s).is_none());
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_long_string_truncated() {
        // 20 ASCII characters, max=10: result is the first 10 chars +
        // an ellipsis glyph. We assert on character count (not byte
        // length) because the ellipsis is multibyte in UTF-8.
        let s = "a".repeat(20);
        let t = truncate(&s, 10);
        let char_count = t.chars().count();
        assert!(
            t.starts_with("aaaaaaaaaa"),
            "expected truncated prefix, got {:?}",
            t
        );
        assert!(t.ends_with('…'), "expected trailing ellipsis, got {:?}", t);
        assert_eq!(char_count, 11, "10 chars + 1 ellipsis = 11 (got {:?})", t);
    }

    #[test]
    fn render_transcript_includes_all_roles() {
        let t = vec![
            (MessageRole::System, "sys".into()),
            (MessageRole::User, "usr".into()),
            (MessageRole::Tool, "res".into()),
        ];
        let s = render_transcript(&t);
        assert!(s.contains("[SYSTEM]"));
        assert!(s.contains("[USER]"));
        assert!(s.contains("[TOOL]"));
        assert!(s.ends_with("[ASSISTANT]\n"));
    }
}
