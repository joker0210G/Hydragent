// filepath: crates/hydragent-swarm/tests/council_spawn_test.rs
//! Integration tests for `SubAgentSpawner::spawn_with_council`.
//!
//! These tests verify the spawner correctly:
//!  - loads a real `ModelCouncil` from `config/model_council.yaml`
//!  - routes a spec by role to a known-good model id
//!  - preserves a caller-supplied `model_hint` (caller override)
//!  - falls back to the router primary when no council is attached
//!
//! The LLM call itself is mocked via the shared `MockModelProvider`
//! in `common::spawner_with_answer`; we never make a real network
//! call. Assertions are made on:
//!  - the resulting `SubAgentStatus.model_used`, which the spawner
//!    propagates from `spec.model_hint` end-to-end.

use std::sync::Arc;

use hydragent_model::council::ModelCouncil;
use hydragent_model::profiles::CostTier;
use hydragent_types::{AgentState, SubAgentRole, SubAgentSpec};

mod common;

fn sample_spec(role: SubAgentRole, hint: Option<&str>) -> SubAgentSpec {
    SubAgentSpec {
        id: "sa-council-001".to_string(),
        name: "council-routed".to_string(),
        role,
        task: "Small deterministic task for the council routing test.".to_string(),
        system_prompt: String::new(),
        allowed_tools: vec!["echo".to_string()],
        token_budget: 8_000,
        timeout_ms: 30_000,
        parent_page_id: String::new(),
        swarm_id: String::new(),
        model_hint: hint.map(|s| s.to_string()),
    }
}

#[test]
fn council_yaml_loads_and_contains_expected_profiles() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let council = ModelCouncil::load_from_yaml(path)
        .expect("config/model_council.yaml should parse");
    assert!(council.len() >= 20, "expected >=20 profiles, got {}", council.len());
    assert!(council.get("deepseek/deepseek-coder").is_some(),
        "deepseek-coder should be in the council");
    assert!(council.get("meta-llama/llama-3.1-405b-instruct:free").is_some(),
        "llama-3-405b free should be in the council (primary)");
}

#[test]
fn council_routes_code_role_to_code_generation_pick() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let council = ModelCouncil::load_from_yaml(path).expect("yaml should load");
    let decision = council.route("code_generation", CostTier::Any);
    assert!(!decision.profile.model_id.is_empty(),
        "council should pick *some* model for code_generation");
    let candidates: Vec<&str> = council
        .profiles_for_tag("code_generation")
        .iter()
        .map(|p| p.model_id.as_str())
        .collect();
    assert!(
        candidates.iter().any(|m| m == &decision.profile.model_id.as_str()),
        "picked {} must be in the code_generation candidate set {:?}",
        decision.profile.model_id,
        candidates
    );
}

#[test]
fn spawn_with_council_builds_spawner() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer":"hi"}"#);
    let council = ModelCouncil::load_from_yaml(path).expect("yaml should load");
    let spawner = spawner.with_council(Arc::new(council));
    assert!(spawner.council().is_some(), "council should be attached");
}

#[tokio::test]
async fn spawn_with_council_uses_routed_model_when_no_hint() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let (spawner_no_council, _mock) = common::spawner_with_answer(r#"{"answer":"done"}"#);
    let council = ModelCouncil::load_from_yaml(path).expect("yaml should load");
    let decision = council.route("code_generation", CostTier::Any);
    let expected_model = decision.profile.model_id.clone();
    let spawner = spawner_no_council.with_council(Arc::new(council));

    // Build a spec for a Build role (maps to "code_generation" tag).
    let spec = sample_spec(SubAgentRole::Build, None);
    let handle = spawner
        .spawn_with_council(spec.clone())
        .expect("spawn should succeed");
    let status = handle.await.expect("task should join");
    assert_eq!(status.state, AgentState::Completed, "status: {:?}", status);
    // The spawner should have written the council's pick into the
    // spec.model_hint, which the router then used (and which
    // status.model_used now reflects).
    assert_eq!(
        status.model_used, expected_model,
        "status.model_used should match the council's routed model"
    );
}

#[tokio::test]
async fn spawn_with_council_preserves_caller_hint() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer":"done"}"#);
    let council = ModelCouncil::load_from_yaml(path).expect("yaml should load");
    let spawner = spawner.with_council(Arc::new(council));

    // Caller explicitly picks a model. The council's pick should
    // be ignored, and the spec's hint should be used.
    let explicit = "anthropic/claude-3.5-sonnet".to_string();
    let spec = sample_spec(SubAgentRole::Build, Some(&explicit));
    let handle = spawner
        .spawn_with_council(spec)
        .expect("spawn should succeed");
    let status = handle.await.expect("task should join");
    assert_eq!(status.state, AgentState::Completed);
    assert_eq!(
        status.model_used, explicit,
        "caller's model_hint should be used verbatim"
    );
}

#[tokio::test]
async fn spawn_with_council_preserves_caller_hint_even_if_unknown_to_council() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer":"done"}"#);
    let council = ModelCouncil::load_from_yaml(path).expect("yaml should load");
    let spawner = spawner.with_council(Arc::new(council));

    // Caller override for a model *not* in the council.
    let explicit = "some/external-model-v999".to_string();
    let spec = sample_spec(SubAgentRole::Explore, Some(&explicit));
    let handle = spawner
        .spawn_with_council(spec)
        .expect("spawn should succeed");
    let status = handle.await.expect("task should join");
    assert_eq!(status.state, AgentState::Completed);
    assert_eq!(
        status.model_used, explicit,
        "caller override should win, even if not in council"
    );
}

#[tokio::test]
async fn spawn_without_council_uses_router_primary() {
    let (spawner, _mock) = common::spawner_with_answer(r#"{"answer":"ok"}"#);
    // No council attached: legacy behavior.
    let spec = sample_spec(SubAgentRole::Build, None);
    let handle = spawner.spawn(spec);
    let status = handle.await.expect("task should join");
    assert_eq!(status.state, AgentState::Completed);
    // Without a council, the spec has no model_hint, so
    // status.model_used falls back to the router's provider label.
    // The shared MockModelProvider's label is "mock-fixed" (set in
    // its `fixed` constructor).
    assert_eq!(status.model_used, "mock-fixed");
}

#[test]
fn route_explicit_returns_decision_for_known_model() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let council = ModelCouncil::load_from_yaml(path).expect("yaml should load");
    let d = council
        .route_explicit("deepseek/deepseek-coder")
        .expect("deepseek-coder is in the council");
    assert_eq!(d.profile.model_id, "deepseek/deepseek-coder");
}

#[test]
fn route_explicit_returns_none_for_unknown_model() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/model_council.yaml");
    let council = ModelCouncil::load_from_yaml(path).expect("yaml should load");
    assert!(council.route_explicit("not-a-real-model-xyz").is_none());
}
