//! # hydragent-swarm
//!
//! Phase 5 / Track 5.1 — Sub-agent spawning, isolation, and coordination.
//!
//! The swarm crate adds three things on top of the existing tool + model
//! plumbing:
//!
//! * [`SubAgent`] — a single isolated run: scoped tool allowlist, scoped
//!   permission gate, and a final [`SubAgentStatus`].
//! * [`SubAgentSpawner`] — a wrapper that owns a shared `ToolRegistry` +
//!   `ModelRouter` and turns a [`SubAgentSpec`] into a running tokio task
//!   with hard timeout + token budget enforcement.
//! * [`SwarmCoordinator`] — a thin handle to a set of running sub-agents
//!   with `status_all`, `await_all`, `cancel`, and bounded concurrency.
//!
//! The goal of Track 5.1 is to prove the runtime path. The LLM call itself
//! is a one-shot `chat_stream`/non-streaming call followed by a single
//! tool-loop; a full ReAct loop per sub-agent is reserved for Track 5.3
//! (DAG Execution Engine). See `TODO_PHASE5.md` for context.

// Module-level doc lints disabled at the crate level; per-module docs
// live at the top of each file and are linked from the `pub use` list.

/// The isolated sub-agent runtime.
pub mod agent;
/// The shared spawner (owns registry + router, produces JoinHandles).
pub mod spawner;
/// The bounded-concurrency coordinator over a swarm of sub-agents.
pub mod coordinator;
/// File-based inter-agent mailbox.
pub mod mailbox;
/// Post-run synthesis & final-response aggregation.
pub mod supervisor;

pub use agent::{SubAgent, SubAgentError};
pub use spawner::{SubAgentSpawner, SpawnError};
pub use coordinator::{SwarmCoordinator, CoordinatorError};
pub use mailbox::{AgentMailbox, InboxEntry, MailMessage, MailboxError};
pub use supervisor::{Supervisor, SupervisedResponse};

pub use hydragent_types::{
    SubAgentSpec, SubAgentStatus, SubAgentRole, AgentState,
};
