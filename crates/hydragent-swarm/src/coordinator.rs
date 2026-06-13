use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::info;

use hydragent_types::{SubAgentSpec, SubAgentStatus};

use crate::agent::SubAgent;
use crate::spawner::SubAgentSpawner;

/// Errors from the coordinator (not from the sub-agents themselves).
#[derive(Debug, Error)]
pub enum CoordinatorError {
    /// A `join_all` call hit the configured wall-clock timeout.
    #[error("await_all timed out after {0:?} with {1} agents still running")]
    AwaitTimeout(Duration, usize),
}

/// A handle to one running sub-agent inside the coordinator. The handle
/// keeps a reference to the live `SubAgent` (so we can call `cancel` on it)
/// and a `JoinHandle` to its eventual `SubAgentStatus`.
struct LiveEntry {
    /// The sub-agent itself (cheap to clone; needed for cancel).
    agent: SubAgent,
    /// Resolves to the final status when the task ends.
    handle: tokio::task::JoinHandle<SubAgentStatus>,
}

/// Coordinates a set of sub-agents. Supports:
///   * bounded concurrency (a semaphore caps how many run at once),
///   * `spawn` (queue or run immediately),
///   * `status_all` (snapshot of completed statuses so far),
///   * `await_all` (block until every queued agent finishes, with timeout),
///   * `cancel` (signal one or every running agent to stop).
///
/// Track 5.1 keeps it intentionally simple — no DAG scheduling, no result
/// fan-in. Those come in Track 5.3.
pub struct SwarmCoordinator {
    spawner: SubAgentSpawner,
    /// Max number of sub-agents running simultaneously.
    semaphore: Arc<Semaphore>,
    /// Live agents, in spawn order.
    live: Arc<Mutex<Vec<LiveEntry>>>,
    /// Completed statuses, in completion order.
    completed: Arc<Mutex<Vec<SubAgentStatus>>>,
    /// Optional swarm_id for trace correlation.
    swarm_id: String,
}

impl SwarmCoordinator {
    /// Build a coordinator with the given concurrency cap and shared
    /// spawner. `max_concurrency = 0` is treated as "unbounded" (semaphore
    /// with the maximum number of permits tokio will accept — effectively
    /// uncapped, ~2^61).
    pub fn new(spawner: SubAgentSpawner, max_concurrency: usize) -> Self {
        // tokio's `Semaphore::new` rejects `usize::MAX` (or close to it)
        // with a debug-assert in release. We use the documented cap,
        // `Semaphore::MAX_PERMITS`, for the "unbounded" case.
        const MAX_PERMITS: usize = Semaphore::MAX_PERMITS;
        let permits = if max_concurrency == 0 {
            MAX_PERMITS
        } else {
            max_concurrency.min(MAX_PERMITS)
        };
        Self {
            spawner,
            semaphore: Arc::new(Semaphore::new(permits)),
            live: Arc::new(Mutex::new(Vec::new())),
            completed: Arc::new(Mutex::new(Vec::new())),
            swarm_id: String::new(),
        }
    }

    /// Tag every agent spawned through this coordinator with a `swarm_id`.
    /// Useful for log/trace correlation.
    pub fn with_swarm_id(mut self, id: impl Into<String>) -> Self {
        self.swarm_id = id.into();
        self
    }

    /// The shared spawner (for callers that need to spawn outside the
    /// coordinator's bounded queue).
    pub fn spawner(&self) -> &SubAgentSpawner {
        &self.spawner
    }

    /// Number of agents that have been spawned (live + completed).
    pub async fn total_spawned(&self) -> usize {
        let live = self.live.lock().await.len();
        let completed = self.completed.lock().await.len();
        live + completed
    }

    /// Number of agents currently running.
    pub async fn live_count(&self) -> usize {
        self.live.lock().await.len()
    }

    /// Snapshot of completed statuses so far.
    pub async fn status_all(&self) -> Vec<SubAgentStatus> {
        self.completed.lock().await.clone()
    }

    /// Spawn one sub-agent. Acquires a semaphore permit (blocks if at cap)
    /// and stores the live entry for later cancel/await.
    pub async fn spawn(&self, mut spec: SubAgentSpec) {
        if !self.swarm_id.is_empty() && spec.swarm_id.is_empty() {
            spec.swarm_id = self.swarm_id.clone();
        }
        let _permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .expect("semaphore never closed");

        let agent = SubAgent::new(
            spec.clone(),
            self.spawner.registry_clone(),
            self.spawner.router_clone(),
        );

        // Move the work into a task. The permit is held for the lifetime of
        // this task; when the task ends (success/fail/cancel), the permit
        // drops and a queued agent can start.
        let live = Arc::clone(&self.live);
        let completed = Arc::clone(&self.completed);
        let swarm_id = spec.swarm_id.clone();

        let agent_for_task = agent.clone();
        let handle = tokio::spawn(async move {
            let start = Instant::now();
            let result = agent_for_task.run().await;
            let _elapsed = start.elapsed();

            // Record completion.
            {
                let mut done = completed.lock().await;
                done.push(result.clone());
            }

            // Remove from live. We don't have the index here, so we filter.
            {
                let mut l = live.lock().await;
                l.retain(|e| e.agent.spec().id != result.id);
            }

            info!(
                sub_agent_id = %result.id,
                swarm_id = %swarm_id,
                state = ?result.state,
                "Coordinator: agent finished"
            );

            result
        });

        let entry = LiveEntry {
            agent,
            handle,
        };
        let mut l = self.live.lock().await;
        l.push(entry);
    }

    /// Cancel a single agent by id. Returns `true` if a live agent was
    /// found and signaled.
    pub async fn cancel(&self, sub_agent_id: &str) -> bool {
        let l = self.live.lock().await;
        for e in l.iter() {
            if e.agent.spec().id == sub_agent_id {
                e.agent.cancel().await;
                return true;
            }
        }
        false
    }

    /// Cancel every live agent. Returns the number signaled.
    pub async fn cancel_all(&self) -> usize {
        let l = self.live.lock().await;
        let count = l.len();
        for e in l.iter() {
            e.agent.cancel().await;
        }
        count
    }

    /// Wait for every live agent to finish. Returns all completed statuses
    /// (including any that finished before this call). If `deadline` is
    /// `Some`, the wait is bounded; the call returns an error if the
    /// deadline hits with agents still running.
    pub async fn await_all(
        &self,
        deadline: Option<Duration>,
    ) -> Result<Vec<SubAgentStatus>, CoordinatorError> {
        // Take the live entries out so concurrent spawns don't interfere.
        let entries: Vec<LiveEntry> = {
            let mut l = self.live.lock().await;
            std::mem::take(&mut *l)
        };

        // Collect the inner JoinHandles. We don't have a way to "subscribe"
        // to a JoinHandle from a JoinSet without moving the future, so we
        // instead spawn one tiny task per handle that just awaits and
        // returns the value (or skips JoinError). We use a JoinSet of those.
        let mut set: JoinSet<SubAgentStatus> = JoinSet::new();
        for e in entries {
            let id = e.agent.spec().id.clone();
            set.spawn(async move {
                match e.handle.await {
                    Ok(s) => s,
                    Err(je) => SubAgentStatus {
                        id,
                        name: String::new(),
                        role: hydragent_types::SubAgentRole::General,
                        swarm_id: String::new(),
                        parent_page_id: String::new(),
                        state: hydragent_types::AgentState::Failed,
                        model_used: String::new(),
                        tokens_used: 0,
                        elapsed_ms: 0,
                        output: String::new(),
                        tool_calls: Vec::new(),
                        error: Some(format!("sub-agent task panicked or aborted: {}", je)),
                    },
                }
            });
        }

        let started = Instant::now();
        // Pre-seed `collected` with the agent ids we expect to see, so
        // we can dedupe by id. The spawned task pushes its result into
        // `self.completed` AND returns it via the handle — if we naively
        // cloned `completed` and then awaited the handle, we would
        // double-count any agent whose task finished between the moment
        // we read `live` and the moment we awaited its handle.
        let mut collected: Vec<SubAgentStatus> = self.completed.lock().await.clone();
        let mut seen: std::collections::HashSet<String> =
            collected.iter().map(|s| s.id.clone()).collect();

        loop {
            if set.is_empty() {
                return Ok(collected);
            }
            match deadline {
                Some(d) => {
                    let remaining = d.checked_sub(started.elapsed()).unwrap_or(Duration::ZERO);
                    if remaining.is_zero() {
                        let still_running = set.len();
                        return Err(CoordinatorError::AwaitTimeout(d, still_running));
                    }
                    match timeout(remaining, set.join_next()).await {
                        Ok(Some(Ok(status))) => {
                            if seen.insert(status.id.clone()) {
                                collected.push(status);
                            }
                        }
                        Ok(Some(Err(_))) => { /* task panicked; skip */ }
                        Ok(None) => continue,
                        Err(_) => {
                            let still_running = set.len();
                            return Err(CoordinatorError::AwaitTimeout(d, still_running));
                        }
                    }
                }
                None => {
                    match set.join_next().await {
                        Some(Ok(status)) => {
                            if seen.insert(status.id.clone()) {
                                collected.push(status);
                            }
                        }
                        Some(Err(_)) => { /* task panicked; skip */ }
                        None => continue,
                    }
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Unit tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hydragent_model::openrouter::OpenRouterClient;
    use hydragent_model::router::ModelRouter;
    use hydragent_tools::registry::ToolRegistry;

    fn make_spawner() -> SubAgentSpawner {
        let provider: Arc<dyn hydragent_model::ModelProvider> =
            Arc::new(OpenRouterClient::new(vec!["fake-key".to_string()]));
        let router = Arc::new(ModelRouter::new(
            provider,
            "fake-model".to_string(),
            vec![],
        ));
        SubAgentSpawner::new(Arc::new(ToolRegistry::new()), router)
    }

    #[tokio::test]
    async fn coordinator_new_creates_empty_state() {
        let coord = SwarmCoordinator::new(make_spawner(), 4);
        assert_eq!(coord.live_count().await, 0);
        assert_eq!(coord.status_all().await.len(), 0);
        assert_eq!(coord.total_spawned().await, 0);
    }

    #[tokio::test]
    async fn max_concurrency_zero_means_unbounded() {
        // Sanity: building with 0 should not panic.
        let _coord = SwarmCoordinator::new(make_spawner(), 0);
    }
}
