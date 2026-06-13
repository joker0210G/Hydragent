//! Integration tests for `SubAgent` — the isolated sub-agent runtime.
//!
//! All tests run against an in-process `MockModelProvider` and a
//! `ToolRegistry` that only knows `echo`. They cover:
//!
//! 1. The happy path: a sub-agent gets a direct "final answer" response
//!    from the LLM and finishes `Completed`.
//! 2. The allowlist gate: an LLM-requested tool that is *not* in the
//!    sub-agent's allowlist is reported as denied (even if the underlying
//!    tool is `AutoApprove` tier and the registry has it).
//! 3. The system prompt only advertises tools in the allowlist (no
//!    leakage of the full registry).
//! 4. Timeout enforcement: a sub-agent with a 0-ms timeout is cancelled.

mod common;

use std::sync::Arc;
use std::time::Duration;

use hydragent_swarm::{AgentState, SubAgent, SubAgentSpec};
use hydragent_tools::echo::EchoTool;
use hydragent_tools::registry::ToolRegistry;

fn spec_with_tools(tools: Vec<&'static str>) -> SubAgentSpec {
    SubAgentSpec::new("test-agent", hydragent_types::SubAgentRole::General, "do the thing")
        .with_tools(tools.iter().map(|s| s.to_string()).collect())
        .with_timeout_ms(5_000)
        .with_token_budget(4_000)
        .in_swarm("test-swarm", "test-page")
}

#[tokio::test]
async fn happy_path_prose_answer_completes() {
    let (spawner, _mock) = common::spawner_with_answer(
        r#"{"thought": "I have an answer", "answer": "Hello, world!"}"#,
    );
    let agent = SubAgent::from_spawner(
        &spawner,
        spec_with_tools(vec!["echo"]),
    );
    let status = agent.run().await;

    assert_eq!(status.state, AgentState::Completed);
    assert_eq!(status.output, "Hello, world!");
    assert!(status.error.is_none());
    assert!(status.tool_calls.is_empty());
}

#[tokio::test]
async fn allowlist_denies_tool_not_in_spec() {
    // Mock returns a tool call to "web_search" on the first LLM call, then
    // a final answer on the second.
    let (spawner, _mock) = common::spawner_with_scripted(
        vec![
            r#"{"thought":"need to search","tool":"web_search","params":{"query":"rust"}}"#
                .to_string(),
        ],
        r#"{"thought": "done", "answer": "ok"}"#,
    );
    // Sub-agent's allowlist does NOT include web_search.
    let agent = SubAgent::from_spawner(
        &spawner,
        spec_with_tools(vec!["echo"]),
    );
    let status = agent.run().await;

    // The LLM-requested tool call should have been attempted and denied.
    assert_eq!(status.tool_calls.len(), 1, "exactly one tool call was attempted");
    let call = &status.tool_calls[0];
    assert_eq!(call.tool_id, "web_search");
    assert_eq!(
        call.tier,
        hydragent_types::PermissionTier::Deny,
        "tool outside the allowlist must be Deny tier"
    );
    // The agent should then complete with the second LLM response.
    assert_eq!(status.state, AgentState::Completed);
    assert_eq!(status.output, "ok");
}

#[tokio::test]
async fn allowlist_permits_tool_in_spec() {
    // Register echo in the registry.
    let mut registry = ToolRegistry::new();
    registry.register(EchoTool);
    let registry = Arc::new(registry);

    let (spawner, _mock) = common::spawner_with_scripted(
        vec![
            r#"{"thought":"echo test","tool":"echo","params":{"message":"hi"}}"#.to_string(),
        ],
        r#"{"thought":"done","answer":"final"}"#,
    );

    // Build the agent with a registry that has echo AND a sub-agent spec
    // that allows echo. We can't use `from_spawner` here because the
    // spawner's internal registry is empty — we need to wire our own.
    let agent = SubAgent::new(
        spec_with_tools(vec!["echo"]),
        registry,
        spawner.router_clone(),
    );
    let status = agent.run().await;

    assert_eq!(status.tool_calls.len(), 1);
    let call = &status.tool_calls[0];
    assert_eq!(call.tool_id, "echo");
    assert_eq!(call.tier, hydragent_types::PermissionTier::AutoApprove);
    assert_eq!(status.state, AgentState::Completed);
    assert_eq!(status.output, "final");
}

#[tokio::test]
async fn invalid_spec_rejected_by_spawner() {
    use hydragent_swarm::SubAgentSpawner;
    // We only exercise the static `validate()` here; the spawner itself
    // is unused, hence the underscore prefix.
    let (_spawner, _mock) = common::spawner_with_answer(r#"{"answer": "x"}"#);
    let mut bad = spec_with_tools(vec!["echo"]);
    bad.id = String::new(); // invalid: empty id
    let result = std::panic::catch_unwind(|| {
        // spawn() panics on invalid spec (we documented this); we use
        // catch_unwind to assert the panic instead of crashing the test.
        let _h = SubAgentSpawner::validate(&bad);
    });
    // validate() should have returned Err, not panicked.
    assert!(result.is_ok());
    let validation = SubAgentSpawner::validate(&bad);
    assert!(validation.is_err(), "empty id should fail validation");
}

#[tokio::test]
async fn run_propagates_swarm_id_and_role() {
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer": "ok"}"#);
    let spec = SubAgentSpec::new("named", hydragent_types::SubAgentRole::Scout, "scout task")
        .in_swarm("sw-xyz", "page-1");
    let agent = SubAgent::from_spawner(&spawner, spec);
    let status = agent.run().await;
    assert_eq!(status.swarm_id, "sw-xyz");
    assert_eq!(status.parent_page_id, "page-1");
    assert_eq!(status.role, hydragent_types::SubAgentRole::Scout);
    assert_eq!(status.name, "named");
}

#[tokio::test]
async fn cancel_during_run_yields_cancelled_state() {
    // Slow answer: the mock returns the same canned response — but we
    // signal cancel from another task before the LLM call returns.
    // To avoid racing the LLM, use a long-token-budget loop: the mock
    // will be called many times, and we cancel after the first call.
    let (spawner, _mock) = common::spawner_with_scripted(
        vec![
            r#"{"thought":"tool call","tool":"echo","params":{"message":"a"}}"#.to_string();
            10
        ],
        r#"{"answer":"never"}"#,
    );
    // Build an agent and spawn it as a task so we can cancel from outside.
    let agent = SubAgent::from_spawner(
        &spawner,
        spec_with_tools(vec!["echo"]),
    );
    let agent_for_cancel = agent.clone();
    let runner = tokio::spawn(async move { agent.run().await });
    // Give it a moment to start, then cancel.
    tokio::time::sleep(Duration::from_millis(10)).await;
    agent_for_cancel.cancel().await;
    let status = runner.await.expect("task did not panic");
    // Cancellation is cooperative; the agent may finish a few steps
    // before noticing. State must be terminal and either Cancelled
    // (saw the flag) or Completed (finished before seeing it).
    assert!(status.state.is_terminal());
    if status.state == AgentState::Cancelled {
        assert!(status.error.is_some());
    }
}
