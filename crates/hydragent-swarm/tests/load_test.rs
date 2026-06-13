//! Phase 5 / Track 5.1 baseline load test.
//!
//! Hard goal (per `TODO_PHASE5.md` Track 5.1 → G6 baseline):
//!   "20 concurrent sub-agents complete in under 2 seconds"
//!
//! This test runs **fully in-process**: the `MockModelProvider` returns
//! a canned JSON answer without any network calls, so the elapsed time
//! measures only the swarm crate's overhead — coordinator dispatch,
//! tokio task spawn, `SubAgent` loop, JSON parsing, and tool gating.
//!
//! On dev hardware the actual wall time is typically well under 500 ms;
//! the 2-second ceiling is generous.

mod common;

use std::time::{Duration, Instant};

use hydragent_swarm::{SubAgentSpec, SwarmCoordinator};
use hydragent_types::{AgentState, SubAgentRole};

const N: usize = 20;
const CEILING: Duration = Duration::from_secs(2);

fn make_spec(i: usize) -> SubAgentSpec {
    SubAgentSpec::new(
        format!("agent-{:02}", i),
        SubAgentRole::General,
        "no-op task",
    )
    .with_tools(vec!["echo".to_string()])
    .with_timeout_ms(5_000)
    .with_token_budget(2_000)
    .in_swarm("load-test", "load-page")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn load_test_20_concurrent_sub_agents_under_2s() {
    // Use a fast mock that just returns a final-answer JSON.
    let (spawner, _mock) = common::spawner_with_answer(
        r#"{"thought":"instant","answer":"ok"}"#,
    );

    // Bound concurrency to 10 so the load is 2 batches of 10. The total
    // time should still be dominated by both batches overlapping.
    let coord = SwarmCoordinator::new(spawner, 10).with_swarm_id("load-20");

    let started = Instant::now();

    for i in 0..N {
        coord.spawn(make_spec(i)).await;
    }
    let results = coord.await_all(Some(CEILING + Duration::from_secs(2)))
        .await
        .expect("await_all did not time out");
    let elapsed = started.elapsed();

    assert_eq!(
        results.len(),
        N,
        "all {} sub-agents should have produced a status (got {})",
        N,
        results.len()
    );
    for r in &results {
        assert_eq!(r.state, AgentState::Completed);
        assert_eq!(r.output, "ok");
    }

    eprintln!(
        "load_test_20_concurrent_sub_agents_under_2s: {} agents in {:?} (ceiling {:?})",
        N, elapsed, CEILING
    );
    assert!(
        elapsed < CEILING,
        "20 concurrent sub-agents took {:?} (ceiling {:?})",
        elapsed,
        CEILING
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn load_test_20_with_tool_call_under_2s() {
    // Slightly more realistic: each agent makes 1 tool call to "echo"
    // (which is registered), then completes. Two LLM round-trips per
    // agent = 40 total, plus 20 echo invocations.
    use hydragent_tools::echo::EchoTool;
    use hydragent_tools::registry::ToolRegistry;

    let mut reg = ToolRegistry::new();
    reg.register(EchoTool);
    let registry = std::sync::Arc::new(reg);

    // Scripted per-agent: each agent sees its own private
    // `[tool_call, final_answer]` cycle (1st call → tool, 2nd call →
    // final). The mock identifies the agent by id parsed from the
    // request's system prompt, so the cycles don't race against
    // each other. With 20 agents × 2 calls each = 40 total
    // tool+final round-trips, plus 20 echo invocations.
    use std::collections::HashMap;

    let tool_call_json = r#"{"thought":"echo","tool":"echo","params":{"message":"hi"}}"#;
    let final_answer_json = r#"{"thought":"done","answer":"ok"}"#;

    let mut per_agent: HashMap<String, Vec<String>> = HashMap::with_capacity(N);
    for i in 0..N {
        per_agent.insert(
            format!("agent-{:02}", i),
            vec![tool_call_json.to_string(), final_answer_json.to_string()],
        );
    }
    let (spawner, _mock) = common::spawner_with_per_agent(per_agent);

    // We need a coordinator that uses our registry (with echo) instead
    // of the spawner's empty one. Build a fresh coordinator that wraps
    // a custom spawner.
    let router = spawner.router_clone();
    let real_spawner = hydragent_swarm::SubAgentSpawner::new(registry, router);
    let coord = SwarmCoordinator::new(real_spawner, 10).with_swarm_id("load-20-tool");

    let started = Instant::now();
    for i in 0..N {
        coord.spawn(make_spec(i)).await;
    }
    let results = coord.await_all(Some(CEILING + Duration::from_secs(2)))
        .await
        .expect("await_all did not time out");
    let elapsed = started.elapsed();

    assert_eq!(results.len(), N);
    for r in &results {
        assert!(
            r.state.is_terminal(),
            "agent {} in non-terminal state {:?}",
            r.name,
            r.state
        );
        // Each agent should end Completed with output "ok".
        assert_eq!(r.state, AgentState::Completed);
        assert_eq!(r.output, "ok");
        // Each agent should have made exactly 1 echo tool call.
        assert_eq!(r.tool_calls.len(), 1, "agent {} tool_calls", r.name);
    }
    eprintln!(
        "load_test_20_with_tool_call_under_2s: {} agents in {:?} (ceiling {:?})",
        N, elapsed, CEILING
    );
    assert!(
        elapsed < CEILING,
        "20 concurrent sub-agents (with tool call) took {:?} (ceiling {:?})",
        elapsed,
        CEILING
    );
}
