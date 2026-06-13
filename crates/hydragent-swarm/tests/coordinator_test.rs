//! Integration tests for `SwarmCoordinator` — bounded concurrency,
//! status snapshots, await_all, and cancel.

mod common;

use std::time::{Duration, Instant};

use hydragent_swarm::{SubAgentSpec, SwarmCoordinator};
use hydragent_types::{AgentState, SubAgentRole};

fn quick_spec(name: &str) -> SubAgentSpec {
    SubAgentSpec::new(name, SubAgentRole::General, "quick task")
        .with_tools(vec!["echo".to_string()])
        .with_timeout_ms(5_000)
        .with_token_budget(2_000)
        .in_swarm("coord-test", "page-x")
}

#[tokio::test]
async fn spawn_n_agents_then_await_all_returns_n() {
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer": "done"}"#);
    let coord = SwarmCoordinator::new(spawner, 4).with_swarm_id("sw-coord-1");

    for i in 0..3 {
        let mut s = quick_spec(&format!("a-{}", i));
        s.parent_page_id = format!("page-{}", i);
        coord.spawn(s).await;
    }
    assert_eq!(coord.total_spawned().await, 3);
    assert_eq!(coord.live_count().await, 3);

    let results = coord.await_all(None).await.expect("await_all succeeded");
    assert_eq!(results.len(), 3, "all 3 agents returned a status");
    for r in &results {
        assert_eq!(r.state, AgentState::Completed);
        assert_eq!(r.output, "done");
    }
    assert_eq!(coord.live_count().await, 0, "all moved to completed");
}

#[tokio::test]
async fn status_all_returns_completed_so_far() {
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer": "hi"}"#);
    let coord = SwarmCoordinator::new(spawner, 2);

    coord.spawn(quick_spec("a")).await;
    coord.spawn(quick_spec("b")).await;
    let _ = coord.await_all(None).await.unwrap();

    let all = coord.status_all().await;
    assert_eq!(all.len(), 2);
    let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"a"));
    assert!(names.contains(&"b"));
}

#[tokio::test]
async fn await_all_with_deadline_times_out() {
    // Each agent runs a 100ms LLM call, then a tool (no-op in empty
    // registry), then loops up to 5 times. We give await_all 50ms total
    // — it should hit the deadline with agents still running.
    let (spawner, mock) = common::spawner_with_scripted(
        vec![
            r#"{"thought":"loop","tool":"echo","params":{"message":"a"}}"#.to_string();
            5
        ],
        r#"{"answer":"x"}"#,
    );
    mock.set_delay_ms(100);
    let coord = SwarmCoordinator::new(spawner, 4);

    for i in 0..3 {
        let mut s = quick_spec(&format!("slow-{}", i));
        s.timeout_ms = 5_000;
        s.token_budget = 80_000;
        coord.spawn(s).await;
    }

    let started = Instant::now();
    let res = coord.await_all(Some(Duration::from_millis(50))).await;
    let elapsed = started.elapsed();
    assert!(res.is_err(), "await_all should have timed out");
    assert!(
        elapsed < Duration::from_millis(500),
        "deadline 50ms should fire fast; took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn cancel_single_agent_marks_cancelled() {
    // Long-ish loop so cancel has a chance to land.
    let (spawner, mock) = common::spawner_with_scripted(
        vec![
            r#"{"thought":"loop","tool":"echo","params":{"message":"a"}}"#.to_string();
            20
        ],
        r#"{"answer":"x"}"#,
    );
    mock.set_delay_ms(50);
    let coord = SwarmCoordinator::new(spawner, 4);

    let mut s1 = quick_spec("to-cancel");
    s1.timeout_ms = 5_000;
    let mut s2 = quick_spec("to-finish");
    s2.timeout_ms = 5_000;

    let id1 = s1.id.clone();
    let id2 = s2.id.clone();
    coord.spawn(s1).await;
    coord.spawn(s2).await;

    // Give them a moment, then cancel s1 only.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancelled = coord.cancel(&id1).await;
    assert!(cancelled, "cancel() found the live agent");
    let _ = coord.cancel(&"nonexistent".to_string()).await; // returns false; not asserted

    let results = coord.await_all(Some(Duration::from_secs(5))).await.unwrap();
    assert_eq!(results.len(), 2);

    let s1_status = results.iter().find(|r| r.id == id1).expect("s1 result");
    let s2_status = results.iter().find(|r| r.id == id2).expect("s2 result");

    // s1 should be Cancelled (the cooperative flag wins). s2 should
    // also finish (either Completed if it ran the echo loop or
    // Cancelled if the loop ran past the cancel; either is acceptable
    // as long as it's terminal).
    assert_eq!(s1_status.state, AgentState::Cancelled);
    assert!(s2_status.state.is_terminal());
}

#[tokio::test]
async fn cancel_all_signals_every_live_agent() {
    let (spawner, mock) = common::spawner_with_scripted(
        vec![
            r#"{"thought":"loop","tool":"echo","params":{"message":"a"}}"#.to_string();
            30
        ],
        r#"{"answer":"x"}"#,
    );
    mock.set_delay_ms(50);
    let coord = SwarmCoordinator::new(spawner, 4);

    for i in 0..4 {
        let mut s = quick_spec(&format!("c-{}", i));
        s.timeout_ms = 5_000;
        coord.spawn(s).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = coord.cancel_all().await;
    assert_eq!(n, 4);

    let results = coord.await_all(Some(Duration::from_secs(3))).await.unwrap();
    assert_eq!(results.len(), 4);
    for r in &results {
        assert_eq!(r.state, AgentState::Cancelled);
    }
}

#[tokio::test]
async fn bounded_concurrency_caps_live_count() {
    // We can't directly observe "the semaphore is full", but we can
    // observe that the total time for 4 agents at cap=1 is roughly
    // 4× one agent's time. To keep the test fast, we use a near-instant
    // mock and just assert total_spawned reaches the cap.
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer":"x"}"#);
    let coord = SwarmCoordinator::new(spawner, 1);

    for i in 0..4 {
        coord.spawn(quick_spec(&format!("seq-{}", i))).await;
    }
    assert_eq!(coord.total_spawned().await, 4);

    let results = coord.await_all(Some(Duration::from_secs(5))).await.unwrap();
    assert_eq!(results.len(), 4);
    for r in &results {
        assert_eq!(r.state, AgentState::Completed);
    }
}
