//! Test helpers shared across the integration test files in `tests/`.
//!
//! This module is included via `mod common;` from each test file. The
//! leading underscore on the module name is the convention Cargo's
//! integration test harness requires to keep the helpers from being
//! auto-discovered as a separate test target.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hydragent_model::openrouter::{ChatMessage, LLMRequest};
use hydragent_model::router::ModelRouter;
use hydragent_model::ModelProvider;
use hydragent_tools::registry::ToolRegistry;
use tokio::sync::mpsc;

use hydragent_swarm::SubAgentSpawner;

/// A `ModelProvider` that returns canned responses from a queue. Each
/// call to `chat_stream` pops one entry and uses it as the assistant
/// reply. When the queue is empty it returns a deterministic
/// "final answer" so tests don't hang.
pub struct MockModelProvider {
    label: String,
    queue: Arc<Mutex<VecDeque<String>>>,
    /// If `Some`, this is returned verbatim whenever the queue is empty
    /// (and also on the very first call when the queue is empty after
    /// the constructor). Lets `fixed` return the same answer every time
    /// rather than draining the queue after one shot.
    fallback: Arc<Mutex<Option<String>>>,
    /// Optional per-call delay (in milliseconds) injected before
    /// returning. Lets tests simulate a slow LLM so they can hit
    /// cancel/timeout code paths.
    delay_ms: Arc<Mutex<u64>>,
    /// Optional `alternating` cycle: when set, the provider ignores the
    /// queue and returns `cycle[call_count % cycle.len()]` on every
    /// call. `call_count` is per-provider-instance, so each test that
    /// builds a fresh `scripted_cycle` for each agent gets a
    /// deterministic per-agent script.
    cycle: Arc<Mutex<Option<Vec<String>>>>,
    /// Call counter for `cycle` mode. Monotonic.
    call_count: Arc<Mutex<u64>>,
    /// Optional per-agent cycle map: when set, the provider looks up
    /// the agent's id (parsed from the request's system prompt) in
    /// this map and returns the corresponding cycle. Lets a single
    /// provider serve N agents with independent scripts. Used by the
    /// 20-agent load test.
    per_agent_cycle: Arc<Mutex<Option<std::collections::HashMap<String, Vec<String>>>>>,
    /// Per-agent call counters for `per_agent_cycle` mode.
    per_agent_counts: Arc<Mutex<std::collections::HashMap<String, u64>>>,
}

impl MockModelProvider {
    /// Build a mock that always returns the same answer (regardless of
    /// prompt). Useful for tests that don't care about LLM output.
    pub fn fixed(answer: impl Into<String>) -> Self {
        let a = answer.into();
        Self {
            label: "mock-fixed".to_string(),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            fallback: Arc::new(Mutex::new(Some(a))),
            delay_ms: Arc::new(Mutex::new(0)),
            cycle: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
            per_agent_cycle: Arc::new(Mutex::new(None)),
            per_agent_counts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Build a mock that returns each canned answer in order, then falls
    /// back to `default_answer` once the queue is drained.
    pub fn scripted(answers: Vec<String>, default_answer: impl Into<String>) -> Self {
        Self {
            label: "mock-scripted".to_string(),
            queue: Arc::new(Mutex::new(VecDeque::from(answers))),
            fallback: Arc::new(Mutex::new(Some(default_answer.into()))),
            delay_ms: Arc::new(Mutex::new(0)),
            cycle: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
            per_agent_cycle: Arc::new(Mutex::new(None)),
            per_agent_counts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Build a mock that returns `cycle[call_count % cycle.len()]` on
    /// every call. The cycle is per-provider-instance, so if you build
    /// one provider per agent (e.g. in load tests), each agent sees
    /// the same deterministic script: e.g. `["tool_call", "final"]`
    /// means "first call returns a tool call, second call returns a
    /// final answer, then it repeats (but agents usually finish in
    /// 2 calls so the repeat doesn't matter)".
    pub fn scripted_cycle(cycle: Vec<String>) -> Self {
        assert!(!cycle.is_empty(), "cycle must have at least one entry");
        Self {
            label: "mock-cycle".to_string(),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            fallback: Arc::new(Mutex::new(None)),
            delay_ms: Arc::new(Mutex::new(0)),
            cycle: Arc::new(Mutex::new(Some(cycle))),
            call_count: Arc::new(Mutex::new(0)),
            per_agent_cycle: Arc::new(Mutex::new(None)),
            per_agent_counts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Build a mock that serves each agent its own cycle, looked up by
    /// agent id (parsed from the request's system prompt). Each agent
    /// has its own monotonic call counter so cycles don't bleed
    /// across agents. Use this for load tests where many agents share
    /// a single provider.
    pub fn per_agent_cycle(map: std::collections::HashMap<String, Vec<String>>) -> Self {
        assert!(!map.is_empty(), "per-agent cycle map must be non-empty");
        Self {
            label: "mock-per-agent".to_string(),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            fallback: Arc::new(Mutex::new(None)),
            delay_ms: Arc::new(Mutex::new(0)),
            cycle: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
            per_agent_cycle: Arc::new(Mutex::new(Some(map))),
            per_agent_counts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Configure a per-call delay (ms). Useful for cancellation tests
    /// that need the mock to "think" for a while.
    pub fn set_delay_ms(&self, ms: u64) {
        *self.delay_ms.lock().unwrap() = ms;
    }

    /// Number of unused canned answers still in the queue.
    pub fn remaining(&self) -> usize {
        self.queue.lock().unwrap().len()
    }
}

#[async_trait]
impl ModelProvider for MockModelProvider {
    fn provider_name(&self) -> &str {
        &self.label
    }

    fn is_available(&self) -> bool {
        true
    }

    async fn chat_stream(
        &self,
        request: &LLMRequest,
        _tx: mpsc::Sender<String>,
    ) -> anyhow::Result<String> {
        // Simulate latency for cancel/timeout tests.
        let delay = *self.delay_ms.lock().unwrap();
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }

        // 0. Per-agent cycle: lookup the agent's id (parsed from the
        //    first system message) and return the next entry in its
        //    own private cycle. Lets a single provider serve N agents
        //    with N independent scripts. Used by the 20-agent load
        //    test where agents must each see a tool call followed by
        //    a final answer.
        if let Some(map) = self.per_agent_cycle.lock().unwrap().clone() {
            let agent_id = parse_agent_id(&request.messages);
            if let Some(cycle) = agent_id.as_ref().and_then(|id| map.get(id)) {
                let n = {
                    let mut counts = self.per_agent_counts.lock().unwrap();
                    let entry = counts.entry(agent_id.unwrap()).or_insert(0);
                    *entry += 1;
                    *entry
                };
                let idx = ((n - 1) as usize) % cycle.len();
                return Ok(cycle[idx].clone());
            }
            // No cycle for this agent — fall through to queue/fallback.
        }

        // 1. Cycle mode: deterministic per-instance repeating script.
        //    Each provider instance has its own call_count, so tests
        //    that build one provider per agent get a per-agent script
        //    (e.g. first call returns a tool call, second returns
        //    a final answer) without races on a shared queue.
        if let Some(cycle) = self.cycle.lock().unwrap().clone() {
            let n = {
                let mut c = self.call_count.lock().unwrap();
                *c += 1;
                *c
            };
            let idx = ((n - 1) as usize) % cycle.len();
            return Ok(cycle[idx].clone());
        }

        // 2. Pop from the queue first (scripted-only behaviour).
        let from_queue = {
            let mut q = self.queue.lock().unwrap();
            q.pop_front()
        };
        if let Some(s) = from_queue {
            return Ok(s);
        }
        // 3. Empty queue — return the configured fallback if any.
        let fb = self.fallback.lock().unwrap().clone();
        if let Some(s) = fb {
            return Ok(s);
        }
        // 4. Last resort (shouldn't happen with `fixed`/`scripted`).
        Ok(r#"{"thought":"no-mock-configured","answer":"ok"}"#.to_string())
    }
}

/// Build a `SubAgentSpawner` with an empty tool registry and a mock router
/// that returns the given canned answer. The mock provider is also
/// returned in an `Arc` for assertions (e.g. checking how many times the
/// LLM was called).
pub fn spawner_with_answer(answer: impl Into<String>) -> (SubAgentSpawner, Arc<MockModelProvider>) {
    let mock = Arc::new(MockModelProvider::fixed(answer.into()));
    let provider: Arc<dyn ModelProvider> = mock.clone();
    let router = Arc::new(ModelRouter::new(
        provider,
        "mock-model".to_string(),
        vec![],
    ));
    (SubAgentSpawner::new(Arc::new(ToolRegistry::new()), router), mock)
}

/// Build a `SubAgentSpawner` with a scripted mock that returns each
/// answer in order, then a default.
pub fn spawner_with_scripted(
    answers: Vec<String>,
    default: impl Into<String>,
) -> (SubAgentSpawner, Arc<MockModelProvider>) {
    let mock = Arc::new(MockModelProvider::scripted(answers, default));
    let provider: Arc<dyn ModelProvider> = mock.clone();
    let router = Arc::new(ModelRouter::new(
        provider,
        "mock-model".to_string(),
        vec![],
    ));
    (SubAgentSpawner::new(Arc::new(ToolRegistry::new()), router), mock)
}

/// Build a `SubAgentSpawner` with a single mock provider that serves
/// each agent its own private script, looked up by agent id parsed
/// from the request. Lets many agents share one provider while
/// each following a deterministic per-agent cycle
/// (e.g. `[tool_call, final_answer]`). Used by load tests.
pub fn spawner_with_per_agent(
    per_agent: std::collections::HashMap<String, Vec<String>>,
) -> (SubAgentSpawner, Arc<MockModelProvider>) {
    let mock = Arc::new(MockModelProvider::per_agent_cycle(per_agent));
    let provider: Arc<dyn ModelProvider> = mock.clone();
    let router = Arc::new(ModelRouter::new(
        provider,
        "mock-model".to_string(),
        vec![],
    ));
    (SubAgentSpawner::new(Arc::new(ToolRegistry::new()), router), mock)
}

// `ChatMessage` is unused in helpers but referenced from the trait
// signature; keep an import so trait_impl errors are obvious if removed.
#[allow(dead_code)]
fn _unused_chat_message_marker(_: ChatMessage) {}

/// Extract a sub-agent identifier from the request. The mock supports
/// two ways of identifying the agent, both embedded in the system
/// block of the transcript:
///   1. A `Your task name is "<name>"` line (the human-readable
///      spec.name, e.g. "agent-07"). This is the key tests use in
///      their per-agent cycle maps because it's stable and readable.
///   2. A `- Sub-agent ID: <uuid>` line (the spec id, used for trace
///      correlation). This is the canonical id.
///
/// We try (1) first because the name is the natural key for
/// per-agent cycle maps in tests. Returns `None` if neither marker is
/// present.
fn parse_agent_id(messages: &[ChatMessage]) -> Option<String> {
    // Prefer the task-name line because tests map by readable names.
    if let Some(name) = scan_for(messages, "Your task name is \"") {
        if !name.is_empty() {
            return Some(name);
        }
    }
    // Fall back to the Sub-agent ID (canonical spec id).
    if let Some(id) = scan_for(messages, "Sub-agent ID:") {
        return Some(id);
    }
    None
}

/// Walk every message (system first, then user, then everything else)
/// looking for the first non-whitespace, non-quote token that follows
/// `needle`. Returns `None` if `needle` is not present in any
/// message.
fn scan_for(messages: &[ChatMessage], needle: &str) -> Option<String> {
    let order: Vec<&ChatMessage> = {
        let mut v: Vec<&ChatMessage> = messages
            .iter()
            .filter(|m| m.role == "system")
            .collect();
        v.extend(messages.iter().filter(|m| m.role == "user"));
        v.extend(messages.iter().filter(|m| m.role != "system" && m.role != "user"));
        v
    };
    for m in order {
        if let Some(id) = extract_id_after(&m.content, needle) {
            return Some(id);
        }
    }
    None
}

/// Pull a token from `content` immediately after the first occurrence
/// of `needle`. The token runs until whitespace, newline, or a quote
/// is seen. Returns `None` if the needle is absent or there is no
/// non-whitespace token after it.
fn extract_id_after(content: &str, needle: &str) -> Option<String> {
    let idx = content.find(needle)?;
    let after = &content[idx + needle.len()..];
    let id: String = after
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| !c.is_whitespace() && *c != '\n' && *c != '\r' && *c != '"')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

#[cfg(test)]
mod id_parse_tests {
    use super::*;

    #[test]
    fn parses_id_from_system_prompt() {
        let m = ChatMessage {
            role: "system".to_string(),
            content: "some header\n- Role: General\n- Sub-agent ID: agent-07\nmore".to_string(),
        };
        assert_eq!(parse_agent_id(&[m]), Some("agent-07".to_string()));
    }

    #[test]
    fn returns_none_when_marker_missing() {
        let m = ChatMessage {
            role: "system".to_string(),
            content: "no id here".to_string(),
        };
        assert_eq!(parse_agent_id(&[m]), None);
    }

    /// The non-streaming router path packs the whole transcript (incl.
    /// the system block) into a single user-role message. Make sure
    /// `parse_agent_id` can still pick the id out of that.
    #[test]
    fn parses_id_from_user_message_transcript() {
        let transcript = "[SYSTEM]\nYou are a sub-agent.\n\
                          # Allowed Tools (allowlist-enforced)\n\
                          - **echo**\n\n\
                          # Operating Constraints\n\
                          - Token budget: 2000 tokens\n\
                          - Wall-clock budget: 5000 ms\n\
                          - Role: General\n\
                          - Sub-agent ID: <uuid>\n\n\
                          [USER]\nno-op task\n\n\
                          [ASSISTANT]\n";
        let m = ChatMessage {
            role: "user".to_string(),
            content: transcript.to_string(),
        };
        // The Sub-agent ID is the canonical id; this transcript uses a
        // placeholder rather than a real uuid, so we just check the
        // marker is found.
        assert!(parse_agent_id(&[m]).is_some());
    }

    /// The task-name line `Your task name is "<name>"` is the readable
    /// key tests use in their per-agent cycle maps. Make sure we can
    /// extract the name even when the Sub-agent ID line is not
    /// accessible (e.g. tokenization removed the angle brackets).
    #[test]
    fn parses_name_from_task_name_line() {
        let transcript = "You are a specialist sub-agent in a hydragent swarm. \
                          Your task name is \"agent-13\" and your role is General. \
                          Stay focused.";
        let m = ChatMessage {
            role: "user".to_string(),
            content: transcript.to_string(),
        };
        assert_eq!(parse_agent_id(&[m]), Some("agent-13".to_string()));
    }
}
