use std::path::Path;
use std::sync::Arc;

use hydragent_tools::registry::ToolRegistry;
use thiserror::Error;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use hydragent_model::council::{ModelCouncil, RoutingDecision};
use hydragent_model::profiles::CostTier;
use hydragent_model::router::ModelRouter;
use hydragent_types::{SubAgentRole, SubAgentSpec};

use crate::agent::SubAgent;

/// Errors from the spawner itself (not from the sub-agent run).
#[derive(Debug, Error)]
pub enum SpawnError {
    /// Spec is missing required fields.
    #[error("invalid spec: {0}")]
    InvalidSpec(String),
    /// Tokio task panicked or was cancelled.
    #[error("sub-agent task failed to join: {0}")]
    Join(#[from] tokio::task::JoinError),
    /// Council routing failed (e.g. the spec's `model_hint` is set but
    /// the council doesn't recognize it).
    #[error("council routing failed: {0}")]
    CouncilRouting(String),
}

/// Owns the shared resources needed to spawn sub-agents. Cheap to clone
/// (`Arc` inside).
///
/// Typical lifetime:
/// ```text
///     let spawner = SubAgentSpawner::new(registry, router);
///     let h1 = spawner.spawn(spec_a);
///     let h2 = spawner.spawn(spec_b);
///     let r1 = h1.await?;
///     let r2 = h2.await?;
/// ```
#[derive(Clone)]
pub struct SubAgentSpawner {
    registry: Arc<ToolRegistry>,
    router: Arc<ModelRouter>,
    /// Optional Model Council. When present, `spawn_with_council` will
    /// route the spec's task type through the council to pick a model
    /// (unless the spec has an explicit `model_hint`).
    council: Option<Arc<ModelCouncil>>,
}

impl SubAgentSpawner {
    /// Build a new spawner without a council (legacy behavior — every
    /// sub-agent uses the router's primary model).
    pub fn new(registry: Arc<ToolRegistry>, router: Arc<ModelRouter>) -> Self {
        Self {
            registry,
            router,
            council: None,
        }
    }

    /// Build a new spawner with a Model Council loaded from a YAML file.
    /// `path` is the path to `config/model_council.yaml`.
    pub fn with_council_yaml<P: AsRef<Path>>(
        registry: Arc<ToolRegistry>,
        router: Arc<ModelRouter>,
        path: P,
    ) -> Result<Self, SpawnError> {
        let council = ModelCouncil::load_from_yaml(path).map_err(|e| {
            SpawnError::CouncilRouting(format!("failed to load council yaml: {e}"))
        })?;
        Ok(Self {
            registry,
            router,
            council: Some(Arc::new(council)),
        })
    }

    /// Attach a pre-built Model Council to a spawner. Use this when
    /// the council is already loaded in memory (e.g. shared with the
    /// planner or scheduler).
    pub fn with_council(mut self, council: Arc<ModelCouncil>) -> Self {
        self.council = Some(council);
        self
    }

    /// Borrow the council handle, if one was attached.
    pub fn council(&self) -> Option<&Arc<ModelCouncil>> {
        self.council.as_ref()
    }

    /// Cheap clone of the registry handle (for callers that want to build
    /// their own `SubAgent` directly).
    pub fn registry_clone(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.registry)
    }

    /// Cheap clone of the router handle.
    pub fn router_clone(&self) -> Arc<ModelRouter> {
        Arc::clone(&self.router)
    }

    /// Validate a spec. Cheap, called from `spawn` automatically; exposed
    /// for callers that want to pre-check.
    pub fn validate(spec: &SubAgentSpec) -> Result<(), SpawnError> {
        if spec.id.is_empty() {
            return Err(SpawnError::InvalidSpec("id is empty".into()));
        }
        if spec.name.is_empty() {
            return Err(SpawnError::InvalidSpec("name is empty".into()));
        }
        if spec.task.is_empty() {
            return Err(SpawnError::InvalidSpec("task is empty".into()));
        }
        if spec.timeout_ms == 0 {
            return Err(SpawnError::InvalidSpec("timeout_ms must be > 0".into()));
        }
        if spec.token_budget == 0 {
            return Err(SpawnError::InvalidSpec("token_budget must be > 0".into()));
        }
        Ok(())
    }

    /// Spawn a sub-agent as a tokio task. The handle resolves to a
    /// `SubAgentStatus` once the run finishes (success, failure, cancel,
    /// or timeout — all of those are encoded in the status).
    ///
    /// This is the **legacy** entrypoint: it does **not** consult the
    /// council. Use [`Self::spawn_with_council`] to route the spec
    /// through the Model Council first.
    pub fn spawn(&self, spec: SubAgentSpec) -> JoinHandle<hydragent_types::SubAgentStatus> {
        Self::validate(&spec).expect("SubAgentSpec failed validation");
        let agent = SubAgent::new(spec.clone(), Arc::clone(&self.registry), Arc::clone(&self.router));
        let id = spec.id.clone();
        let name = spec.name.clone();
        debug!(sub_agent_id = %id, name = %name, "Spawning sub-agent task");
        tokio::spawn(async move {
            let status = agent.run().await;
            if status.state.is_terminal() {
                info!(
                    sub_agent_id = %status.id,
                    state = ?status.state,
                    elapsed_ms = status.elapsed_ms,
                    "Sub-agent finished"
                );
            } else {
                error!(sub_agent_id = %status.id, "Sub-agent returned non-terminal state");
            }
            status
        })
    }

    /// Spawn a sub-agent through the Model Council.
    ///
    /// Routing priority:
    /// 1. If `spec.model_hint` is `Some(model_id)`, that model is used
    ///    (caller override). If the council is attached AND the hint
    ///    is **not** in the council, we log a warning but proceed —
    ///    the caller is asking for a specific model and we honor it.
    /// 2. Otherwise, if a council is attached, route by the spec's
    ///    `SubAgentRole`-derived task tag with `CostTier::Any` budget.
    /// 3. Otherwise (no council), fall through to the router's primary.
    ///
    /// The picked model's `model_id` is written into the spec's
    /// `model_hint` before spawn, so the agent's LLM call uses it
    /// (and `SubAgentStatus.model_used` reflects it).
    pub fn spawn_with_council(
        &self,
        mut spec: SubAgentSpec,
    ) -> Result<JoinHandle<hydragent_types::SubAgentStatus>, SpawnError> {
        Self::validate(&spec)?;

        // 1. Honor an explicit caller hint, but warn if the council
        //    doesn't know about it.
        if let Some(ref hint) = spec.model_hint {
            if let Some(council) = &self.council {
                if council.get(hint).is_none() {
                    warn!(
                        sub_agent_id = %spec.id,
                        model_hint = %hint,
                        "spec.model_hint is not in the council — proceeding as caller override"
                    );
                } else {
                    debug!(
                        sub_agent_id = %spec.id,
                        model_hint = %hint,
                        "spec.model_hint honored (known by council)"
                    );
                }
            }
        } else if let Some(council) = &self.council {
            // 2. Council-driven routing.
            let task_tag = role_task_tag(spec.role);
            let decision: RoutingDecision = council.route(task_tag, CostTier::Any);
            let picked = decision.profile.model_id.clone();
            info!(
                sub_agent_id = %spec.id,
                name = %spec.name,
                role = ?spec.role,
                task_tag = %task_tag,
                routed_model = %picked,
                routing_path = ?decision.path,
                candidates = decision.candidates_considered,
                in_budget = decision.candidates_in_budget,
                "Model Council picked model for sub-agent"
            );
            spec.model_hint = Some(picked);
        }
        // 3. No council + no hint: spec.model_hint stays None and the
        //    router's primary model is used (legacy behavior).

        let agent = SubAgent::new(spec.clone(), Arc::clone(&self.registry), Arc::clone(&self.router));
        let id = spec.id.clone();
        let name = spec.name.clone();
        debug!(sub_agent_id = %id, name = %name, model_hint = ?spec.model_hint, "Spawning council-routed sub-agent");
        Ok(tokio::spawn(async move {
            let status = agent.run().await;
            if status.state.is_terminal() {
                info!(
                    sub_agent_id = %status.id,
                    state = ?status.state,
                    elapsed_ms = status.elapsed_ms,
                    model_used = %status.model_used,
                    "Sub-agent finished"
                );
            } else {
                error!(sub_agent_id = %status.id, "Sub-agent returned non-terminal state");
            }
            status
        }))
    }
}

/// Map a [`SubAgentRole`] to a `TaskType` snake_case tag the council
/// can route on.  This is intentionally a lossy projection — the
/// council's `general` tag catches anything not explicitly listed.
fn role_task_tag(role: SubAgentRole) -> &'static str {
    match role {
        SubAgentRole::Build => "code_generation",
        SubAgentRole::Explore => "research",
        SubAgentRole::Plan => "planning",
        SubAgentRole::Review => "review",
        SubAgentRole::Scout => "summarization",
        SubAgentRole::General => "general",
    }
}
