//! Test helpers for hydragent-planner integration tests.
//!
//! The planner's integration tests need a working `SubAgentSpawner`
//! that returns deterministic, fast `SubAgentStatus` values without
//! actually hitting a real LLM. We re-create the minimal mock
//! infrastructure here (the swarm's own mock lives in
//! `crates/hydragent-swarm/tests/common/` and is not exported).
//!
//! The mock is deliberately simpler than the swarm one: it doesn't
//! need a per-agent cycle, queue draining, or per-call delay. It
//! always returns the configured `final_answer` from a single
//! `chat_stream` call so the spawner can build a
//! `SubAgentStatus::Completed { final_answer, .. }`.

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use hydragent_model::router::ModelRouter;
use hydragent_model::ModelProvider;
use hydragent_swarm::SubAgentSpawner;
use hydragent_tools::registry::ToolRegistry;
use tokio::sync::mpsc;

/// Trivial `ModelProvider` that always returns the same canned JSON
/// answer (which the spawner will treat as the agent's "final"
/// response). The JSON shape mirrors what a real LLM would return
/// from the spawner's tool-loop perspective: `{"thought": ...,
/// "answer": "..."}`.
pub struct StaticMockProvider {
    label: String,
    answer: String,
}

impl StaticMockProvider {
    pub fn new(answer: impl Into<String>) -> Self {
        Self {
            label: "planner-static-mock".to_string(),
            answer: answer.into(),
        }
    }
}

#[async_trait]
impl ModelProvider for StaticMockProvider {
    fn provider_name(&self) -> &str {
        &self.label
    }
    fn is_available(&self) -> bool {
        true
    }
    async fn chat_stream(
        &self,
        _request: &hydragent_model::openrouter::LLMRequest,
        _tx: mpsc::Sender<String>,
    ) -> anyhow::Result<String> {
        Ok(format!(
            r#"{{"thought":"mock","answer":"{}"}}"#,
            self.answer.replace('"', "\\\"")
        ))
    }
}

/// Build a `SubAgentSpawner` whose sub-agents always return the
/// configured final answer. The spawner is wired with an empty
/// `ToolRegistry` (no tools → the spawner's tool-loop exits after a
/// single chat call), which keeps the test fast and deterministic.
pub fn spawner_with_answer(answer: impl Into<String>) -> SubAgentSpawner {
    let provider: Arc<dyn ModelProvider> = Arc::new(StaticMockProvider::new(answer));
    let router = Arc::new(ModelRouter::new(
        provider,
        "planner-mock-model".to_string(),
        vec![],
    ));
    SubAgentSpawner::new(Arc::new(ToolRegistry::new()), router)
}
