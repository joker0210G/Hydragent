# Phase 5: Kimi-Style Agent Swarm, DAG Planner & Model Council (Weeks 19–22)

> **Timeline**: Weeks 19–22
> **Theme**: Hydragent stops being a single brain and becomes a **coordinated intelligence**. A Directed Acyclic Graph (DAG) planner decomposes complex tasks into parallel sub-problems. A pool of up to **300 specialist sub-agents** executes independently, each with scoped system prompts, isolated tool permissions, and separate context windows. A **Model Council** routes each sub-problem to the optimal LLM from a pool of 20+ models. A **self-healing re-planner** (Devin-style) detects failures, diagnoses root causes, and autonomously restructures the DAG without user intervention.

---

## 📋 Table of Contents

1. [Phase Goals & Exit Criteria](#1-phase-goals--exit-criteria)
2. [Directory & Workspace Layout Changes](#2-directory--workspace-layout-changes)
3. [Technology Decisions](#3-technology-decisions)
4. [Week-by-Week Breakdown](#4-week-by-week-breakdown)
5. [Component Specifications](#5-component-specifications)
   - 5.1 [DAG Planner & Task Decomposition](#51-dag-planner--task-decomposition)
   - 5.2 [SubAgent Spawner & Lifecycle Manager](#52-subagent-spawner--lifecycle-manager)
   - 5.3 [SubAgent Coordinator (Mailbox + File Locking)](#53-subagent-coordinator-mailbox--file-locking)
   - 5.4 [Model Council Router](#54-model-council-router)
   - 5.5 [Self-Healing Re-Planner](#55-self-healing-re-planner)
   - 5.6 [Scoped Tool Permissions per SubAgent](#56-scoped-tool-permissions-per-subagent)
   - 5.7 [DAG Execution Engine](#57-dag-execution-engine)
   - 5.8 [Swarm Supervisor & Result Aggregator](#58-swarm-supervisor--result-aggregator)
   - 5.9 [SubAgent Context Window Manager](#59-subagent-context-window-manager)
   - 5.10 [Swarm Observability & Live Diagram](#510-swarm-observability--live-diagram)
6. [Built-in Tools (Phase 5 Additions)](#6-built-in-tools-phase-5-additions)
7. [Configuration & Environment](#7-configuration--environment)
8. [Testing Strategy](#8-testing-strategy)
9. [Performance Targets](#9-performance-targets)
10. [Risks & Mitigations](#10-risks--mitigations)
11. [Definition of Done](#11-definition-of-done)

---

## 1. Phase Goals & Exit Criteria

Phase 5 is the **capability inflection point** — the moment Hydragent crosses from single-agent tool use into genuine multi-agent coordination. Inspired by Kimi K2.6 (up to 300 sub-agents, 4,000 coordinated steps, SWE-bench Pro 58.6%), Claude Code's Plan/Build subagent delegation, and Devin's self-healing dynamic re-planning.

### Hard Goals (must achieve before Phase 6)

| # | Goal | Validation |
|---|---|---|
| G1 | DAG planner decomposes a complex 5-part task into correctly ordered nodes with dependency edges | Integration test: "Research, write, format, review, and send a report" → 5-node DAG with correct topological order |
| G2 | Sub-agents execute in parallel where the DAG allows; sequential where dependencies exist | Test: 3 independent nodes start concurrently; 1 node waits for its 2 dependencies to complete |
| G3 | Each sub-agent has a scoped system prompt and isolated tool permission set | Unit test: sub-agent with `tools: [web_search]` cannot call `file_write` — returns `PermissionDenied` |
| G4 | Model Council routes each sub-agent's task to the best-fit LLM from 20+ model pool | Integration test: code task → `deepseek-coder`; research task → `perplexity-sonar`; creative task → `claude-sonnet` |
| G5 | Self-healing re-planner detects a failed node, diagnoses root cause, restructures DAG, retries | Test: inject a `file_write` failure → re-planner identifies `file_write` as failing → creates alternative path |
| G6 | Swarm scales to 20 concurrent sub-agents without degrading event bus throughput | Load test: 20 sub-agents simultaneously active; Event Bus latency < 5 ms per message |
| G7 | Sub-agent mailbox (file-based) allows async communication between sibling agents | Integration test: Agent A writes `mailbox/agent-b/result.json`; Agent B reads and uses it in next step |
| G8 | Swarm Supervisor aggregates sub-agent outputs into a coherent final response | Integration test: 3 sub-agents produce partial results → Supervisor synthesizes → user receives unified response |
| G9 | All Phase 1–4 tests remain green (no regressions) | `cargo test --workspace` and `pytest adapters/` both exit 0 |

### Soft Goals (target but not blocking)

- Live ASCII DAG diagram printed to terminal during swarm execution (OpenCode-style live architectural diagram)
- Devin-style self-maintained knowledge wiki: sub-agents can write findings to a shared Markdown wiki in `data/wiki/`
- `./hydragent swarm status` CLI command showing active sub-agents, their current step, and assigned model
- Token budget tracking per sub-agent; automatically switch to cheaper model when budget < 20%
- Sub-agent result caching: if two sub-agents are asked the same question within 60 s, reuse the cached answer
- `max_swarm_size` configurable cap (default: 50; Kimi K2.6 theoretical max: 300)

---

## 2. Directory & Workspace Layout Changes

Phase 5 introduces `crates/hydragent-swarm` (the multi-agent engine) and `crates/hydragent-planner` (the DAG decomposition brain).

```
hydragent/
│
├── crates/
│   │
│   ├── hydragent-core/                        # UPDATED
│   │   └── src/
│   │       ├── main.rs                        # UPDATED: swarm engine init, model council init
│   │       ├── orchestrator.rs               # UPDATED: routes complex tasks to DAG planner
│   │       └── wiki.rs                       # NEW: shared agent wiki (Markdown files in data/wiki/)
│   │
│   ├── hydragent-planner/                    # NEW CRATE: DAG decomposition brain
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── dag.rs                        # DagNode, DagEdge, TaskDag — graph data structures
│   │       ├── decomposer.rs                 # LLM-powered task decomposition → DagSpec JSON
│   │       ├── scheduler.rs                  # TopologicalSort, ready-queue management
│   │       ├── replan.rs                     # SelfHealingReplanner: failure detection + DAG surgery
│   │       └── serializer.rs                # DagSpec ↔ JSON ↔ SQLite persistence
│   │
│   ├── hydragent-swarm/                      # NEW CRATE: sub-agent lifecycle + coordination
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── spawner.rs                    # SubAgentSpawner: creates tokio tasks + assigns resources
│   │       ├── agent.rs                      # SubAgent struct: prompt, tools, model, context window
│   │       ├── coordinator.rs                # SwarmCoordinator: tracks all active agents
│   │       ├── mailbox.rs                    # File-based async mailbox for agent-to-agent messaging
│   │       ├── supervisor.rs                 # SwarmSupervisor: result aggregation + synthesis
│   │       ├── context_manager.rs            # ContextWindowManager: token counting + truncation
│   │       └── cache.rs                      # ResultCache: 60s TTL answer deduplication
│   │
│   ├── hydragent-model/                      # HEAVILY UPDATED
│   │   └── src/
│   │       ├── council.rs                    # NEW: ModelCouncil router (20+ model pool)
│   │       ├── profiles.rs                   # NEW: ModelProfile (capability tags, cost, context size)
│   │       ├── router.rs                     # UPDATED: delegates to council for Phase 5+
│   │       └── openrouter.rs                 # UNCHANGED: still the underlying API client
│   │
│   ├── hydragent-types/                      # UPDATED
│   │   └── src/
│   │       └── lib.rs                        # UPDATED: SubAgentSpec, DagSpec, ModelProfile, TaskType types
│   │
│   └── hydragent-tools/                      # UPDATED
│       └── src/
│           ├── spawn_agent.rs               # NEW tool: spawn_agent
│           ├── delegate_task.rs             # NEW tool: delegate_task
│           └── wiki_write.rs               # NEW tool: wiki_write
│
├── data/
│   ├── sessions/                            # Existing
│   ├── models/                              # Phase 2 embeddings
│   ├── scheduler.db                         # Phase 4 cron
│   ├── swarm/                              # NEW: per-swarm execution state
│   │   ├── {swarm_id}/
│   │   │   ├── dag.json                    # Serialized DAG spec
│   │   │   ├── state.json                  # Execution state (which nodes complete/pending/failed)
│   │   │   └── mailbox/                    # Agent-to-agent file mailbox
│   │   │       ├── {agent_id}/             # Inboxes per sub-agent
│   │   │       │   └── {from_agent}.json
│   │   │       └── shared/                 # Shared workspace for results
│   │   │           └── {node_id}_result.json
│   └── wiki/                               # NEW: shared agent knowledge wiki
│       ├── index.md
│       └── {topic}.md                      # Auto-created by wiki_write tool
│
├── config/
│   ├── model_council.yaml                  # NEW: 20+ model profiles, routing rules
│   └── swarm.yaml                          # NEW: swarm limits, sub-agent defaults
│
└── tests/
    ├── unit/
    │   ├── dag_test.rs                     # NEW: DAG topological sort, cycle detection
    │   ├── council_test.rs                 # NEW: model routing for each task type
    │   ├── replan_test.rs                  # NEW: failure detection + DAG surgery
    │   ├── mailbox_test.rs                 # NEW: file mailbox read/write/notify
    │   └── context_manager_test.rs         # NEW: token budget enforcement
    └── integration/
        ├── swarm_e2e_test.rs               # NEW: full 5-node DAG execution
        ├── model_council_test.rs            # NEW: 20-model routing benchmark
        └── self_healing_test.rs            # NEW: inject failure → verify replan
```

---

## 3. Technology Decisions

---

### 3.1 Language Roles in Phase 5

| Component | Language | Rationale |
|---|---|---|
| DAG planner, task decomposition | **Rust** | Graph algorithms (topological sort, cycle detection) are CPU-bound; must be deterministic |
| Sub-agent spawner & lifecycle | **Rust (Tokio)** | Each sub-agent is a `tokio::task`; Tokio's work-stealing scheduler manages 300 tasks efficiently |
| Model Council router | **Rust** | Hot path: every LLM call routes through the Council; must be sub-millisecond |
| Self-healing re-planner | **Rust** | Calls LLM for diagnosis but processes the DAG surgery itself in Rust |
| Agent-to-agent mailbox | **Rust** | File I/O + `tokio::fs` for async mailbox; `notify` crate for file-system watch |
| Result aggregator / synthesis | **Rust** | Calls synthesis LLM; formats output |
| Swarm observability diagram | **Python (adapters)** | Terminal rendering via `rich.tree` — fast to prototype, no impact on core |

---

### 3.2 DAG Representation: Why Not a Queue?

A simple task queue (like Celery/BullMQ) cannot model dependencies. If task C requires outputs from tasks A and B, a queue has no way to express that. A DAG allows:

| Feature | Task Queue | DAG |
|---|---|---|
| Dependencies between tasks | ❌ No | ✅ Yes (edges) |
| Parallel execution of independent tasks | ❌ Hard to coordinate | ✅ Native (ready-queue from topo-sort) |
| Partial task failure + targeted retry | ❌ Retry whole queue | ✅ Retry only the failed sub-tree |
| Visualization of progress | ❌ Linear position | ✅ Full graph with node status colors |
| DAG surgery (re-plan) | ❌ Impossible | ✅ Add/remove/rewire nodes at runtime |

**Decision**: Custom Rust DAG (`petgraph`) for Phase 5. LanceDB-backed persistent DAG state in Phase 7+.

---

### 3.3 Graph Library: `petgraph`

| Factor | `petgraph` | Custom Adjacency List |
|---|---|---|
| **Topological sort** | Built-in `algo::toposort()` | Must implement Kahn's algorithm manually |
| **Cycle detection** | Built-in `algo::is_cyclic_directed()` | Manual DFS |
| **Graph traversal** | BFS/DFS iterators | Manual recursion |
| **Stability** | 8.2k ⭐, used in rustc's dependency graph | N/A |
| **Overhead** | ~2 MB compiled | Zero |

**Decision**: `petgraph` for Phase 5. It is the most mature Rust graph library and handles all our algorithms out-of-the-box.

---

### 3.4 Model Council: Design Philosophy

The Model Council is inspired by **Perplexity Computer's 20+ model orchestration** and **Taskade's 11+ AI models per agent**. Instead of using one model for everything, the Council matches each task type to the model that benchmarks best on that problem class:

| Task Type Tag | Best-fit Model Pool | Routing Logic |
|---|---|---|
| `code_generation` | `deepseek/deepseek-coder-v2`, `openai/gpt-4o`, `anthropic/claude-sonnet-4-5` | Highest SWE-bench score |
| `research` | `perplexity/sonar-pro`, `openai/gpt-4o-search-preview` | Native web search capability |
| `creative_writing` | `anthropic/claude-sonnet-4-5`, `openai/gpt-4o` | Highest ELO on Chatbot Arena creative tasks |
| `reasoning` | `openai/o1`, `anthropic/claude-sonnet-4-5`, `google/gemini-2.5-pro` | MMLU/MATH benchmark performance |
| `summarization` | `meta-llama/llama-3-70b-instruct:free`, `mistral/mistral-7b-instruct:free` | Cost-efficient; summarization doesn't need frontier |
| `data_extraction` | `openai/gpt-4o-mini`, `anthropic/claude-haiku-3-5` | Fast, cheap, structured JSON output |
| `planning` | `anthropic/claude-sonnet-4-5`, `openai/o1` | Best at multi-step decomposition |
| `review` | `openai/gpt-4o`, `anthropic/claude-sonnet-4-5` | Best at critique and verification |

---

### 3.5 Self-Healing Strategy: Devin-Inspired Dynamic Re-Planning

Devin 3.0's defining feature is that failures don't stop the agent — they trigger a self-repair cycle. Our implementation has three phases:

```
Failure Detected
      │
      ▼
Diagnosis LLM Call (analyze error, suggest fix)
      │
      ├── RETRY: same node, fix params
      ├── REROUTE: bypass failed node, try alternative
      ├── DECOMPOSE: split failed node into smaller nodes
      └── ESCALATE: notify user if stuck after 3 attempts
```

---

### 3.6 Sub-Agent Isolation Model

Each sub-agent is isolated at three levels:

| Isolation Level | Mechanism | What It Prevents |
|---|---|---|
| **Prompt isolation** | Each sub-agent has its own scoped system prompt + partial conversation history | One sub-agent's instructions polluting another's context |
| **Tool isolation** | Each `SubAgentSpec` declares an explicit `allowed_tools: Vec<String>` | A research sub-agent accidentally writing files |
| **Memory isolation** | Each sub-agent reads from shared Semantic Memory (Phase 2) but writes to own workspace | Conflicting memory updates between concurrent agents |

---

## 4. Week-by-Week Breakdown

### Week 19 — DAG Planner & Task Decomposition

**Goal**: Complex user requests are automatically decomposed into a dependency graph. The graph is correctly topologically sorted and ready for parallel execution.

| Day | Task |
|---|---|
| Mon | Create `crates/hydragent-planner` crate. Add `petgraph`, `serde`, `uuid` to `Cargo.toml`. Define `DagNode`, `DagEdge`, `TaskDag` structs. Implement `TaskDag::new()`, `add_node()`, `add_edge()`, `validate()` (cycle detection via `petgraph::algo::is_cyclic_directed()`). |
| Tue | Implement `decomposer.rs`: `async fn decompose(task: &str, llm: &dyn ModelProvider) -> Result<DagSpec>`. Crafts a structured decomposition prompt. Parses LLM JSON output into `DagSpec`. Validates the resulting DAG for cycles and orphan nodes. |
| Wed | Define the `DagSpec` JSON schema (see Section 5.1). Write the decomposition LLM prompt with few-shot examples for 3 task categories: research, coding, and report writing. Verify JSON schema compliance with `serde_json::from_str`. |
| Thu | Implement `scheduler.rs`: `ReadyQueue` — computes which nodes have all dependencies satisfied (in-degree = 0 after marking complete). `TopologicalIterator` yields nodes in execution order. |
| Fri | Implement `serializer.rs`: `DagSpec::to_json()` and `DagSpec::from_json()`. Write to `data/swarm/{swarm_id}/dag.json`. Load on restart for crash recovery. |
| Sat | Unit tests: (1) 5-node linear chain → correct topo order; (2) diamond dependency (A → B, A → C, B → D, C → D) → B and C are concurrent, D waits; (3) cycle A → B → A → `Err(CycleDetected)`; (4) DAG survives JSON serialization round-trip. |
| Sun | Implement the `complexity_classifier` heuristic in `decomposer.rs`: if user message < 50 tokens AND no "and", "then", "also" connectives → `TaskComplexity::Simple` (skip DAG, use direct ReAct); else → `TaskComplexity::Complex` (decompose). |

**Deliverable**: `cargo test -p hydragent-planner` green. DAG decomposition produces valid, cycle-free graphs for test inputs.

---

### Week 20 — SubAgent Spawner & Model Council

**Goal**: Sub-agents spawn as Tokio tasks with scoped configurations. Model Council routes each to the optimal LLM.

| Day | Task |
|---|---|
| Mon | Create `crates/hydragent-swarm` crate. Define `SubAgentSpec`: `id`, `role`, `system_prompt`, `allowed_tools`, `model_hint`, `token_budget`, `timeout_ms`. Define `SubAgent` struct with its own `ReActContext` and `ToolRegistry` slice. |
| Tue | Implement `spawner.rs`: `SubAgentSpawner::spawn(spec: SubAgentSpec, ...) -> SubAgentHandle`. Spawns a `tokio::task` for the sub-agent's ReAct loop. `SubAgentHandle` holds the task's `JoinHandle` and a `mpsc::Sender` for sending messages to the agent. |
| Wed | Implement the `ModelCouncil` in `crates/hydragent-model/src/council.rs`. Load `config/model_council.yaml`. Implement `route(task_type: TaskType, budget: TokenBudget) -> ModelProfile`. Priority: (1) exact tag match; (2) cost-fits-budget; (3) fallback to primary model. |
| Thu | Implement `ModelProfile` in `profiles.rs`: `{ model_id, context_window, cost_per_1k_tokens, task_type_tags: Vec<TaskType>, benchmark_scores: HashMap<String, f64> }`. Load all 20+ profiles from `config/model_council.yaml`. |
| Fri | Wire `ModelCouncil` into `SubAgentSpawner`: when spawning a sub-agent, call `council.route(task_type)` to select the model. Override with `model_hint` if explicitly set. Log the routing decision at `tracing::info!`. |
| Sat | Implement `coordinator.rs`: `SwarmCoordinator` tracks all active `SubAgentHandle`s by `agent_id`. Provides `status_all()` → `Vec<AgentStatus>`, `cancel(agent_id)`, `await_all()`. Uses `tokio::task::JoinSet` for structured concurrency. |
| Sun | Load test: spawn 20 concurrent sub-agents each executing `EchoTool`. Assert all complete within 1 s. Assert `SwarmCoordinator::status_all()` correctly reports terminal state for all 20. |

**Deliverable**: 20 sub-agents execute concurrently. Model Council routes correctly per task type in unit tests.

---

### Week 21 — DAG Execution Engine, Mailbox & Result Aggregation

**Goal**: The DAG planner and SubAgent spawner are integrated. Nodes execute with correct dependency ordering. Agent-to-agent mailboxes work.

| Day | Task |
|---|---|
| Mon | Implement `crates/hydragent-planner/src/dag.rs` execution engine: `DagExecutionEngine::run(dag, spawner, coordinator)`. Main loop: pull ready nodes → spawn sub-agents → mark complete on JoinHandle resolve → update ready queue. Loop until all nodes complete or a node fails. |
| Tue | Implement `mailbox.rs`: `AgentMailbox::write(to_agent_id, from_agent_id, payload: Value)`. Writes to `data/swarm/{swarm_id}/mailbox/{to_agent_id}/{from_agent_id}.json`. `AgentMailbox::read(for_agent_id)` reads all files in inbox. `AgentMailbox::watch(agent_id)` uses `notify` crate for file-system events. |
| Wed | Implement `supervisor.rs`: `SwarmSupervisor::aggregate(results: Vec<NodeResult>) -> String`. Collects all node results, builds a structured synthesis prompt, calls `ModelCouncil::route(TaskType::Summarization)` to select the cheapest model for synthesis, returns final response. |
| Thu | Implement `context_manager.rs`: `ContextWindowManager::build_prompt(agent_spec, conversation_history) -> Vec<Message>`. Uses `tiktoken-rs` to count tokens. Reserves `token_budget * 0.2` for the response. Truncates oldest messages if budget exceeded. Injects relevant semantic memories (Phase 2 hybrid search). |
| Fri | Integrate DAG execution into `orchestrator.rs`: if `complexity_classifier` returns `Complex`, call `decomposer.decompose()` → `DagExecutionEngine::run()` → `SwarmSupervisor::aggregate()` → return final response to user. |
| Sat | Integration test: user sends "Research the top 3 Rust web frameworks, compare them in a table, and write a 200-word recommendation" → verifies 3 independent research nodes run in parallel → comparison node waits → recommendation node waits for comparison. |
| Sun | Implement `cache.rs`: `ResultCache` with 60-second TTL. Key: `SHA-256(task_description + model_id)`. If hit, return cached result instead of spawning new agent. Log cache hits at `tracing::debug!`. |

**Deliverable**: Full 5-node DAG executes. Parallel nodes confirmed by timing (parallel batch < sequential batch). Mailbox integration test green.

---

### Week 22 — Self-Healing Re-Planner, Observability & Release

**Goal**: Agent swarm recovers from failures autonomously. Live DAG visualization. Phase 5 tagged.

| Day | Task |
|---|---|
| Mon | Implement `replan.rs`: `SelfHealingReplanner::on_failure(dag, failed_node, error)`. Step 1: Classify failure type via LLM diagnosis prompt. Step 2: Based on diagnosis, choose `RepairStrategy` (Retry / Reroute / Decompose / Escalate). Step 3: Apply strategy to live DAG via `petgraph` graph mutation. |
| Tue | Implement `RepairStrategy::Retry`: increment `retry_count` on the node; re-add to ready queue with modified params. `RepairStrategy::Decompose`: replace the failed node with 2–3 smaller sub-nodes; re-wire edges. `RepairStrategy::Escalate`: push a `PermissionRequest` to the user explaining the failure and asking for guidance. |
| Wed | Implement `wiki.rs`: `AgentWiki::write(topic, content)`. Creates `data/wiki/{topic}.md`. `AgentWiki::read(topic)` reads the file. `AgentWiki::index()` lists all wiki pages. Used by sub-agents to share findings that persist beyond a single swarm run. |
| Thu | Implement swarm observability: `SwarmDiagramRenderer::render(dag, coordinator)` produces an ASCII tree using `rich.tree` (Python) or `comfy-table` (Rust). Each node shows: status emoji (⏳ pending / 🔄 running / ✅ done / ❌ failed), assigned model, token usage. |
| Fri | Implement `./hydragent swarm status` CLI subcommand: reads `data/swarm/{swarm_id}/state.json`; renders the diagram for the most recent (or specified) swarm run. |
| Sat | Phase 5 full regression suite: `cargo test --workspace` + `pytest adapters/ -v`. Self-healing integration test: inject deliberate failure into node 2 of a 5-node DAG → verify re-planner fires → verify swarm completes with repaired DAG. |
| Sun | Tag `v0.5.0`. Write CHANGELOG. Update `ARCHITECTURE.md` with swarm layer diagram. Demo: complex 10-node DAG task from CLI with live diagram output. |

**Deliverable**: `v0.5.0` tag. Self-healing verified. All Phase 1–5 tests green.

---

## 5. Component Specifications

### 5.1 DAG Planner & Task Decomposition

#### 5.1.1 Core Data Structures

```rust
// crates/hydragent-planner/src/dag.rs

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::algo::{is_cyclic_directed, toposort};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single unit of work in the swarm's execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,                       // UUID
    pub name: String,                     // Human-readable (e.g., "Research Rust frameworks")
    pub description: String,              // Detailed task for the sub-agent
    pub task_type: TaskType,              // For Model Council routing
    pub allowed_tools: Vec<String>,       // Tool whitelist for this node's sub-agent
    pub model_hint: Option<String>,       // Override Model Council (e.g., "openai/gpt-4o")
    pub token_budget: u32,               // Max tokens this node may consume
    pub timeout_ms: u64,                 // Wall-clock timeout
    pub retry_count: u32,               // Current retry attempt (starts at 0)
    pub max_retries: u32,               // Maximum allowed retries before escalation
    pub status: NodeStatus,             // Lifecycle state
    pub result: Option<NodeResult>,     // Populated on completion
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,     // Waiting for dependencies
    Ready,       // All dependencies satisfied; awaiting dispatch
    Running,     // Sub-agent is active
    Completed,   // Successful result
    Failed,      // Terminal failure (retries exhausted or non-retryable error)
    Skipped,     // Bypassed by re-planner via Reroute strategy
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskType {
    CodeGeneration,
    Research,
    CreativeWriting,
    Reasoning,
    Summarization,
    DataExtraction,
    Planning,
    Review,
    General,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResult {
    pub content: String,              // The sub-agent's output
    pub model_used: String,           // Which model produced this result
    pub tokens_used: u32,
    pub execution_ms: u64,
}

/// A directed edge representing a dependency.
/// `from` must complete before `to` can start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagEdge {
    pub from: String,                 // Source node ID
    pub to: String,                   // Target node ID
    pub label: Option<String>,        // e.g., "passes research output to"
}

/// The full task graph, plus swarm-level metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagSpec {
    pub swarm_id: String,
    pub original_task: String,
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
    pub created_at: i64,
}

impl DagSpec {
    /// Build a petgraph DiGraph from this spec and validate it.
    pub fn validate(&self) -> anyhow::Result<DiGraph<String, ()>> {
        let mut graph = DiGraph::new();
        let mut node_indices: HashMap<String, NodeIndex> = HashMap::new();

        for node in &self.nodes {
            let idx = graph.add_node(node.id.clone());
            node_indices.insert(node.id.clone(), idx);
        }

        for edge in &self.edges {
            let from = node_indices.get(&edge.from)
                .ok_or_else(|| anyhow::anyhow!("Edge references unknown node: {}", edge.from))?;
            let to = node_indices.get(&edge.to)
                .ok_or_else(|| anyhow::anyhow!("Edge references unknown node: {}", edge.to))?;
            graph.add_edge(*from, *to, ());
        }

        if is_cyclic_directed(&graph) {
            anyhow::bail!("DAG contains a cycle — invalid task graph");
        }

        Ok(graph)
    }

    /// Return nodes in a valid topological execution order.
    pub fn topological_order(&self) -> anyhow::Result<Vec<String>> {
        let graph = self.validate()?;
        let sorted = toposort(&graph, None)
            .map_err(|_| anyhow::anyhow!("Cycle detected during topological sort"))?;
        Ok(sorted.into_iter().map(|idx| graph[idx].clone()).collect())
    }
}
```

#### 5.1.2 Decomposition Prompt & LLM Call

```rust
// crates/hydragent-planner/src/decomposer.rs

use crate::dag::{DagSpec, DagNode, DagEdge, TaskType, NodeStatus};
use hydragent_model::ModelProvider;

const DECOMPOSITION_SYSTEM_PROMPT: &str = r#"
You are a task decomposition expert for an AI agent swarm. Your job is to break a complex user
request into a minimal set of sub-tasks that can be executed by specialist AI agents.

RULES:
1. Each sub-task should be executable by a single specialist agent with one focus.
2. Identify dependencies: if Task B requires output from Task A, add an edge A → B.
3. Maximize parallelism: tasks without dependencies should have NO edges between them.
4. Assign a task_type to each node from: code_generation | research | creative_writing |
   reasoning | summarization | data_extraction | planning | review | general
5. Assign appropriate tools to each node. Available tools:
   web_search, file_read, file_write, code_exec, memory_search, memory_store,
   wiki_write, delegate_task
6. Keep the DAG as SMALL as possible (3–8 nodes for most tasks).
7. Output ONLY valid JSON. No explanations, no markdown, no backticks.

OUTPUT SCHEMA:
{
  "nodes": [
    {
      "id": "node-1",
      "name": "Short name",
      "description": "Detailed task description for the sub-agent",
      "task_type": "research",
      "allowed_tools": ["web_search", "memory_search"],
      "token_budget": 4000,
      "timeout_ms": 30000,
      "max_retries": 2
    }
  ],
  "edges": [
    { "from": "node-1", "to": "node-3", "label": "research feeds into writing" }
  ]
}

FEW-SHOT EXAMPLE:
Task: "Find the top 3 Rust web frameworks, compare them, and write a recommendation."
Output:
{
  "nodes": [
    {"id":"n1","name":"Research Actix-Web","description":"Search for Actix-Web features, performance benchmarks, and community adoption","task_type":"research","allowed_tools":["web_search","memory_search"],"token_budget":3000,"timeout_ms":20000,"max_retries":2},
    {"id":"n2","name":"Research Axum","description":"Search for Axum features, Tokio integration, and ecosystem","task_type":"research","allowed_tools":["web_search","memory_search"],"token_budget":3000,"timeout_ms":20000,"max_retries":2},
    {"id":"n3","name":"Research Warp","description":"Search for Warp features, filter system, and use cases","task_type":"research","allowed_tools":["web_search","memory_search"],"token_budget":3000,"timeout_ms":20000,"max_retries":2},
    {"id":"n4","name":"Comparison Table","description":"Using research from n1, n2, n3, create a markdown comparison table","task_type":"data_extraction","allowed_tools":["memory_search"],"token_budget":2000,"timeout_ms":15000,"max_retries":1},
    {"id":"n5","name":"Write Recommendation","description":"Write a 200-word recommendation based on the comparison","task_type":"creative_writing","allowed_tools":[],"token_budget":1500,"timeout_ms":15000,"max_retries":1}
  ],
  "edges": [
    {"from":"n1","to":"n4"},{"from":"n2","to":"n4"},{"from":"n3","to":"n4"},{"from":"n4","to":"n5"}
  ]
}
"#;

/// Determines if a task warrants DAG decomposition or can be handled by the simple ReAct loop.
pub fn classify_complexity(task: &str) -> TaskComplexity {
    let token_count = task.split_whitespace().count();
    let has_compound_connectives = ["and then", "after that", "also", "additionally", "furthermore",
        "first", "second", "finally", "compare", "both", "all three"]
        .iter().any(|kw| task.to_lowercase().contains(kw));

    if token_count > 40 || has_compound_connectives {
        TaskComplexity::Complex
    } else {
        TaskComplexity::Simple
    }
}

#[derive(Debug, PartialEq)]
pub enum TaskComplexity { Simple, Complex }

/// Call the LLM to decompose a task into a DagSpec.
pub async fn decompose(
    swarm_id: &str,
    original_task: &str,
    llm: &dyn ModelProvider,
) -> anyhow::Result<DagSpec> {
    let prompt = format!(
        "{}\n\nUSER TASK TO DECOMPOSE:\n{}\n\nOUTPUT JSON:",
        DECOMPOSITION_SYSTEM_PROMPT,
        original_task
    );

    let raw_json = llm.generate_non_streaming(&prompt).await
        .map_err(|e| anyhow::anyhow!("Decomposition LLM call failed: {}", e))?;

    // Extract JSON from response (model sometimes wraps in markdown)
    let json_str = extract_json(&raw_json)?;

    #[derive(serde::Deserialize)]
    struct RawSpec { nodes: Vec<DagNode>, edges: Vec<DagEdge> }
    let raw: RawSpec = serde_json::from_str(&json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse decomposition JSON: {}. Raw: {}", e, &json_str[..json_str.len().min(200)]))?;

    let spec = DagSpec {
        swarm_id: swarm_id.to_string(),
        original_task: original_task.to_string(),
        nodes: raw.nodes,
        edges: raw.edges,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    // Validate before returning
    spec.validate().map_err(|e| anyhow::anyhow!("Decomposition produced invalid DAG: {}", e))?;

    tracing::info!(
        swarm_id,
        node_count = spec.nodes.len(),
        edge_count = spec.edges.len(),
        "Task decomposed into DAG"
    );

    Ok(spec)
}

fn extract_json(s: &str) -> anyhow::Result<String> {
    if let (Some(start), Some(end)) = (s.find('{'), s.rfind('}')) {
        Ok(s[start..=end].to_string())
    } else {
        anyhow::bail!("No JSON object found in LLM response")
    }
}
```

---

### 5.2 SubAgent Spawner & Lifecycle Manager

```rust
// crates/hydragent-swarm/src/spawner.rs

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use hydragent_types::{SubAgentSpec, IntentEvent, AgentResponse, ToolResult};
use hydragent_tools::ToolRegistry;
use hydragent_model::ModelProvider;

/// A handle to a running sub-agent.
pub struct SubAgentHandle {
    pub agent_id: String,
    /// Send messages directly to this agent's inbox
    pub inbox_tx: mpsc::Sender<SubAgentMessage>,
    /// Await the agent's final result
    pub join: JoinHandle<anyhow::Result<NodeResult>>,
}

pub enum SubAgentMessage {
    /// Deliver additional context to the agent mid-execution
    InjectContext(String),
    /// Signal the agent to abort immediately
    Cancel,
}

pub struct SubAgentSpawner {
    tool_registry_full: Arc<ToolRegistry>,   // Full registry (for scoping)
    llm_providers: Arc<ModelCouncil>,
    db: sqlx::SqlitePool,
    embedder: Arc<LocalEmbedder>,
    vector_store: Arc<VectorStore>,
}

impl SubAgentSpawner {
    /// Spawn a sub-agent as an independent Tokio task.
    pub async fn spawn(&self, spec: SubAgentSpec) -> anyhow::Result<SubAgentHandle> {
        let agent_id = spec.id.clone();

        // 1. Create a scoped tool registry containing ONLY allowed tools
        let scoped_registry = self.create_scoped_registry(&spec.allowed_tools);

        // 2. Route to the best model for this task type
        let model = self.llm_providers.route(&spec.task_type, spec.token_budget)?;
        tracing::info!(
            agent_id = %agent_id,
            task_type = ?spec.task_type,
            model = %model.model_id,
            "Sub-agent spawned with model assignment"
        );

        // 3. Build initial context
        let context = SubAgentContext {
            agent_id: agent_id.clone(),
            system_prompt: spec.system_prompt.clone(),
            task_description: spec.task_description.clone(),
            token_budget: spec.token_budget,
            timeout_ms: spec.timeout_ms,
        };

        // 4. Create inbox channel
        let (inbox_tx, mut inbox_rx) = mpsc::channel::<SubAgentMessage>(16);

        // 5. Spawn the Tokio task
        let embedder = self.embedder.clone();
        let vector_store = self.vector_store.clone();
        let db = self.db.clone();

        let join = tokio::spawn(async move {
            run_sub_agent(
                context,
                scoped_registry,
                model,
                embedder,
                vector_store,
                db,
                inbox_rx,
            ).await
        });

        Ok(SubAgentHandle { agent_id, inbox_tx, join })
    }

    fn create_scoped_registry(&self, allowed_tools: &[String]) -> Arc<ToolRegistry> {
        let mut scoped = ToolRegistry::new();
        for tool_name in allowed_tools {
            if let Some((tool, tier)) = self.tool_registry_full.get_tool(tool_name) {
                scoped.register_arc(tool_name, tool, tier);
            }
        }
        Arc::new(scoped)
    }
}

/// The sub-agent's main execution loop.
/// Runs a ReAct loop bounded by `token_budget` and `timeout_ms`.
async fn run_sub_agent(
    ctx: SubAgentContext,
    tools: Arc<ToolRegistry>,
    model: ModelProfile,
    embedder: Arc<LocalEmbedder>,
    vector_store: Arc<VectorStore>,
    db: sqlx::SqlitePool,
    mut inbox: mpsc::Receiver<SubAgentMessage>,
) -> anyhow::Result<NodeResult> {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(ctx.timeout_ms);

    // Build initial messages: system prompt + task
    let mut messages = vec![
        Message { role: "system".into(), content: ctx.system_prompt.clone() },
        Message { role: "user".into(),   content: ctx.task_description.clone() },
    ];

    // Inject relevant semantic memories from Phase 2
    let memories = hybrid_search(&ctx.task_description, 5, &db, &embedder, &vector_store).await
        .unwrap_or_default();
    if !memories.is_empty() {
        let memory_block = format!(
            "Relevant context from memory:\n{}",
            memories.iter().map(|m| format!("- {}", m.content)).collect::<Vec<_>>().join("\n")
        );
        messages.insert(1, Message { role: "system".into(), content: memory_block });
    }

    let mut total_tokens = 0u32;
    let mut final_content = String::new();

    let react_ctx = ReActContext {
        messages,
        tool_registry: tools,
        model_id: model.model_id.clone(),
        max_steps: 10,
    };

    // Run the ReAct loop with combined timeout + inbox monitoring
    let result = tokio::select! {
        react_result = tokio::time::timeout(timeout, run_react_loop(react_ctx)) => {
            react_result
                .map_err(|_| anyhow::anyhow!("Sub-agent {} timed out after {}ms", ctx.agent_id, ctx.timeout_ms))?
        },
        msg = inbox.recv() => {
            match msg {
                Some(SubAgentMessage::Cancel) => {
                    anyhow::bail!("Sub-agent {} cancelled by coordinator", ctx.agent_id);
                }
                _ => anyhow::bail!("Sub-agent {} inbox closed unexpectedly", ctx.agent_id),
            }
        }
    }?;

    let execution_ms = start.elapsed().as_millis() as u64;

    tracing::info!(
        agent_id = %ctx.agent_id,
        execution_ms,
        model = %model.model_id,
        "Sub-agent completed"
    );

    Ok(NodeResult {
        content: result.content,
        model_used: model.model_id,
        tokens_used: result.tokens_used.unwrap_or(0),
        execution_ms,
    })
}
```

---

### 5.3 SubAgent Coordinator (Mailbox + File Locking)

```rust
// crates/hydragent-swarm/src/mailbox.rs

use std::path::{Path, PathBuf};
use serde_json::Value;
use tokio::sync::Notify;
use std::sync::Arc;

/// File-based async mailbox for agent-to-agent communication.
/// Inbox: `data/swarm/{swarm_id}/mailbox/{to_agent_id}/{from_agent_id}.json`
/// Shared workspace: `data/swarm/{swarm_id}/mailbox/shared/{node_id}_result.json`
pub struct AgentMailbox {
    swarm_dir: PathBuf,
    notifiers: dashmap::DashMap<String, Arc<Notify>>,
}

impl AgentMailbox {
    pub fn new(swarm_dir: &str) -> Self {
        Self {
            swarm_dir: PathBuf::from(swarm_dir),
            notifiers: dashmap::DashMap::new(),
        }
    }

    /// Write a message from `from_agent` to `to_agent`'s inbox.
    pub async fn write(
        &self,
        to_agent_id: &str,
        from_agent_id: &str,
        payload: &Value,
    ) -> anyhow::Result<()> {
        let inbox_dir = self.swarm_dir
            .join("mailbox")
            .join(to_agent_id);
        tokio::fs::create_dir_all(&inbox_dir).await?;

        let file_path = inbox_dir.join(format!("{}.json", from_agent_id));

        // Atomic write: write to .tmp then rename (prevents partial reads)
        let tmp_path = inbox_dir.join(format!("{}.json.tmp", from_agent_id));
        tokio::fs::write(&tmp_path, serde_json::to_string_pretty(payload)?).await?;
        tokio::fs::rename(tmp_path, &file_path).await?;

        // Notify any watchers
        if let Some(notify) = self.notifiers.get(to_agent_id) {
            notify.notify_one();
        }

        tracing::debug!(
            to = to_agent_id,
            from = from_agent_id,
            "Mailbox message delivered"
        );

        Ok(())
    }

    /// Read all messages in an agent's inbox. Non-destructive (files remain after read).
    pub async fn read(&self, for_agent_id: &str) -> anyhow::Result<Vec<Value>> {
        let inbox_dir = self.swarm_dir.join("mailbox").join(for_agent_id);
        if !inbox_dir.exists() {
            return Ok(vec![]);
        }

        let mut messages = Vec::new();
        let mut entries = tokio::fs::read_dir(&inbox_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let content = tokio::fs::read_to_string(&path).await?;
                if let Ok(val) = serde_json::from_str::<Value>(&content) {
                    messages.push(val);
                }
            }
        }

        Ok(messages)
    }

    /// Write a node's result to the shared workspace for downstream nodes.
    pub async fn write_result(&self, node_id: &str, result: &NodeResult) -> anyhow::Result<()> {
        let shared_dir = self.swarm_dir.join("mailbox").join("shared");
        tokio::fs::create_dir_all(&shared_dir).await?;

        let file_path = shared_dir.join(format!("{}_result.json", node_id));
        let payload = serde_json::json!({
            "node_id": node_id,
            "content": result.content,
            "model_used": result.model_used,
            "tokens_used": result.tokens_used,
            "execution_ms": result.execution_ms,
        });

        tokio::fs::write(&file_path, serde_json::to_string_pretty(&payload)?).await?;

        tracing::info!(node_id, "Node result written to shared workspace");
        Ok(())
    }

    /// Read a specific upstream node's result (for dependency injection).
    pub async fn read_result(&self, node_id: &str) -> anyhow::Result<Option<NodeResult>> {
        let file_path = self.swarm_dir
            .join("mailbox")
            .join("shared")
            .join(format!("{}_result.json", node_id));

        if !file_path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&file_path).await?;
        let val: serde_json::Value = serde_json::from_str(&content)?;

        Ok(Some(NodeResult {
            content: val["content"].as_str().unwrap_or("").to_string(),
            model_used: val["model_used"].as_str().unwrap_or("").to_string(),
            tokens_used: val["tokens_used"].as_u64().unwrap_or(0) as u32,
            execution_ms: val["execution_ms"].as_u64().unwrap_or(0),
        }))
    }
}
```

---

### 5.4 Model Council Router

```rust
// crates/hydragent-model/src/council.rs

use std::collections::HashMap;
use crate::profiles::ModelProfile;
use hydragent_types::TaskType;

/// Routes each sub-agent's task to the optimal model in the pool.
/// Priority: exact task_type tag match → within budget → fallback to primary.
pub struct ModelCouncil {
    profiles: Vec<ModelProfile>,
    primary_model_id: String,
}

impl ModelCouncil {
    pub fn from_yaml(config_path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(config_path)?;
        let config: CouncilConfig = serde_yaml::from_str(&content)?;
        Ok(Self {
            profiles: config.models,
            primary_model_id: config.primary_model,
        })
    }

    /// Route a task type to the best available model within the token budget.
    /// Returns the ModelProfile to use for this sub-agent.
    pub fn route(&self, task_type: &TaskType, token_budget: u32) -> anyhow::Result<ModelProfile> {
        let task_tag = task_type_to_tag(task_type);

        // 1. Find all models that support this task type
        let mut candidates: Vec<&ModelProfile> = self.profiles.iter()
            .filter(|p| p.task_type_tags.contains(&task_tag))
            .filter(|p| p.context_window >= token_budget as usize)
            .collect();

        // 2. Sort by benchmark score for this task type (descending), then by cost (ascending)
        candidates.sort_by(|a, b| {
            let score_a = a.benchmark_scores.get(&task_tag).copied().unwrap_or(0.0);
            let score_b = b.benchmark_scores.get(&task_tag).copied().unwrap_or(0.0);
            score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cost_per_1k_tokens.partial_cmp(&b.cost_per_1k_tokens)
                    .unwrap_or(std::cmp::Ordering::Equal))
        });

        if let Some(best) = candidates.first() {
            tracing::info!(
                task_type = ?task_type,
                model = %best.model_id,
                score = best.benchmark_scores.get(&task_tag).copied().unwrap_or(0.0),
                "Model Council routing decision"
            );
            return Ok((*best).clone());
        }

        // 3. Fallback to primary model
        tracing::warn!(
            task_type = ?task_type,
            fallback = %self.primary_model_id,
            "Model Council: no specialist found, using primary model"
        );

        self.profiles.iter()
            .find(|p| p.model_id == self.primary_model_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Primary model '{}' not in council profile list", self.primary_model_id))
    }

    /// High-stakes decisions use a 3-model comparison ("council vote").
    /// Each model answers independently; the council selects by majority or defers to the highest scorer.
    pub async fn council_vote(
        &self,
        prompt: &str,
        task_type: &TaskType,
        llm_factory: &dyn ModelProviderFactory,
    ) -> anyhow::Result<String> {
        let tag = task_type_to_tag(task_type);
        let top_3: Vec<ModelProfile> = self.profiles.iter()
            .filter(|p| p.task_type_tags.contains(&tag))
            .take(3)
            .cloned()
            .collect();

        if top_3.is_empty() {
            anyhow::bail!("No models available for council vote on task type {:?}", task_type);
        }

        // Spawn 3 concurrent LLM calls
        let mut futures = Vec::new();
        for profile in &top_3 {
            let llm = llm_factory.create(&profile.model_id)?;
            let p = prompt.to_string();
            futures.push(tokio::spawn(async move { llm.generate_non_streaming(&p).await }));
        }

        let results: Vec<String> = futures::future::join_all(futures)
            .await
            .into_iter()
            .filter_map(|r| r.ok().and_then(|r| r.ok()))
            .collect();

        if results.is_empty() {
            anyhow::bail!("All council models failed");
        }

        // For Phase 5: return the first successful result (majority voting in Phase 6+)
        tracing::info!(
            responses = results.len(),
            "Model Council vote complete"
        );

        Ok(results.into_iter().next().unwrap())
    }
}

fn task_type_to_tag(task_type: &TaskType) -> String {
    match task_type {
        TaskType::CodeGeneration  => "code_generation".to_string(),
        TaskType::Research        => "research".to_string(),
        TaskType::CreativeWriting => "creative_writing".to_string(),
        TaskType::Reasoning       => "reasoning".to_string(),
        TaskType::Summarization   => "summarization".to_string(),
        TaskType::DataExtraction  => "data_extraction".to_string(),
        TaskType::Planning        => "planning".to_string(),
        TaskType::Review          => "review".to_string(),
        TaskType::General         => "general".to_string(),
    }
}
```

---

### 5.5 Self-Healing Re-Planner

```rust
// crates/hydragent-planner/src/replan.rs

use crate::dag::{DagSpec, DagNode, NodeStatus, TaskType};
use hydragent_model::ModelProvider;

#[derive(Debug, Clone)]
pub enum RepairStrategy {
    /// Retry the same node, optionally modifying parameters
    Retry { modified_description: Option<String> },
    /// Skip the failed node and wire its dependents directly to its dependencies
    Reroute,
    /// Decompose the failed node into 2-3 smaller sub-nodes
    Decompose { sub_tasks: Vec<String> },
    /// Human in the loop — push a PermissionRequest to the user
    Escalate { reason: String },
}

pub struct SelfHealingReplanner {
    llm: Arc<dyn ModelProvider>,
    max_repair_attempts: u32,
}

impl SelfHealingReplanner {
    pub fn new(llm: Arc<dyn ModelProvider>, max_repair_attempts: u32) -> Self {
        Self { llm, max_repair_attempts }
    }

    /// Called by the DAG execution engine when a node enters `NodeStatus::Failed`.
    pub async fn on_failure(
        &self,
        dag: &mut DagSpec,
        failed_node_id: &str,
        error: &str,
    ) -> anyhow::Result<RepairStrategy> {
        let node = dag.nodes.iter()
            .find(|n| n.id == failed_node_id)
            .ok_or_else(|| anyhow::anyhow!("Failed node '{}' not found in DAG", failed_node_id))?
            .clone();

        tracing::warn!(
            node_id = failed_node_id,
            node_name = %node.name,
            error,
            retry_count = node.retry_count,
            max_retries = node.max_retries,
            "Self-healing re-planner activated"
        );

        // 1. Check retry budget
        if node.retry_count >= node.max_retries {
            // Diagnose via LLM before deciding between Reroute, Decompose, or Escalate
            let strategy = self.diagnose_and_select_strategy(&node, error).await?;
            self.apply_strategy(dag, failed_node_id, &strategy).await?;
            return Ok(strategy);
        }

        // 2. Simple retry case
        let strategy = RepairStrategy::Retry {
            modified_description: None,
        };
        self.apply_strategy(dag, failed_node_id, &strategy).await?;
        Ok(strategy)
    }

    /// Call the LLM to diagnose the failure and select a repair strategy.
    async fn diagnose_and_select_strategy(
        &self,
        node: &DagNode,
        error: &str,
    ) -> anyhow::Result<RepairStrategy> {
        let prompt = format!(
            r#"
You are a debugging AI for an agent swarm. A sub-task has failed permanently after all retries.
Analyze the failure and recommend a recovery strategy.

FAILED TASK:
Name: {}
Description: {}
Task Type: {:?}
Error: {}
Retry count: {} / {}

AVAILABLE STRATEGIES:
1. reroute    — Skip this task entirely; wire its dependencies to its downstream tasks directly
2. decompose  — Break this task into 2-3 smaller sub-tasks that are more achievable
3. escalate   — This cannot be fixed automatically; ask the human for help

Respond with ONLY valid JSON:
{{"strategy": "reroute" | "decompose" | "escalate", "reason": "brief explanation", "sub_tasks": ["task 1", "task 2"] (only for decompose)}}
"#,
            node.name, node.description, node.task_type, error, node.retry_count, node.max_retries
        );

        let response = self.llm.generate_non_streaming(&prompt).await?;
        let json_str = crate::decomposer::extract_json(&response)
            .map_err(|e| anyhow::anyhow!("Re-planner LLM returned non-JSON: {}", e))?;

        #[derive(serde::Deserialize)]
        struct Diagnosis {
            strategy: String,
            reason: String,
            sub_tasks: Option<Vec<String>>,
        }

        let d: Diagnosis = serde_json::from_str(&json_str)?;

        tracing::info!(
            strategy = %d.strategy,
            reason = %d.reason,
            "Re-planner diagnosis complete"
        );

        match d.strategy.as_str() {
            "reroute" => Ok(RepairStrategy::Reroute),
            "decompose" => Ok(RepairStrategy::Decompose {
                sub_tasks: d.sub_tasks.unwrap_or_default(),
            }),
            _ => Ok(RepairStrategy::Escalate { reason: d.reason }),
        }
    }

    /// Apply the chosen repair strategy to the live DAG.
    async fn apply_strategy(
        &self,
        dag: &mut DagSpec,
        failed_node_id: &str,
        strategy: &RepairStrategy,
    ) -> anyhow::Result<()> {
        match strategy {
            RepairStrategy::Retry { modified_description } => {
                if let Some(node) = dag.nodes.iter_mut().find(|n| n.id == failed_node_id) {
                    node.retry_count += 1;
                    node.status = NodeStatus::Ready;
                    if let Some(desc) = modified_description {
                        node.description = desc.clone();
                    }
                    tracing::info!(node_id = failed_node_id, retry = node.retry_count, "Retrying node");
                }
            },
            RepairStrategy::Reroute => {
                // Mark node as Skipped
                if let Some(node) = dag.nodes.iter_mut().find(|n| n.id == failed_node_id) {
                    node.status = NodeStatus::Skipped;
                }
                // Re-wire: find all nodes that depended on failed_node and remove the dependency
                dag.edges.retain(|e| e.from != failed_node_id);
                tracing::info!(node_id = failed_node_id, "Node rerouted (skipped)");
            },
            RepairStrategy::Decompose { sub_tasks } => {
                // Mark failed node as Skipped
                if let Some(node) = dag.nodes.iter_mut().find(|n| n.id == failed_node_id) {
                    node.status = NodeStatus::Skipped;
                }
                // Add sub-task nodes
                let mut prev_id = failed_node_id.to_string();
                for (i, task_desc) in sub_tasks.iter().enumerate() {
                    let sub_id = format!("{}-sub-{}", failed_node_id, i + 1);
                    dag.nodes.push(DagNode {
                        id: sub_id.clone(),
                        name: format!("Sub-task {}", i + 1),
                        description: task_desc.clone(),
                        task_type: TaskType::General,
                        status: NodeStatus::Ready,
                        retry_count: 0,
                        max_retries: 2,
                        ..Default::default()
                    });
                    dag.edges.push(DagEdge { from: prev_id.clone(), to: sub_id.clone(), label: None });
                    prev_id = sub_id;
                }
                // Wire last sub-task to original downstream nodes
                let downstream: Vec<DagEdge> = dag.edges.iter()
                    .filter(|e| e.from == failed_node_id)
                    .cloned()
                    .collect();
                for edge in downstream {
                    dag.edges.push(DagEdge { from: prev_id.clone(), to: edge.to.clone(), label: None });
                }
                dag.edges.retain(|e| e.from != failed_node_id);
                tracing::info!(node_id = failed_node_id, sub_task_count = sub_tasks.len(), "Node decomposed into sub-tasks");
            },
            RepairStrategy::Escalate { reason } => {
                tracing::warn!(node_id = failed_node_id, reason, "Escalating to user");
                // Phase 3 PermissionGate handles the push notification
            },
        }
        Ok(())
    }
}
```

---

### 5.6 Scoped Tool Permissions per SubAgent

Each sub-agent gets a filtered view of the tool registry, enforced at dispatch time:

```rust
// crates/hydragent-swarm/src/agent.rs

use hydragent_tools::ToolRegistry;
use hydragent_types::{ToolCall, ToolResult, ToolStatus};

/// A sub-agent's sandboxed tool dispatcher.
/// Only tools in `allowed_tools` can be invoked; all others return `PermissionDenied`.
pub struct ScopedToolDispatcher {
    registry: Arc<ToolRegistry>,
    allowed_tools: Vec<String>,
    agent_id: String,
}

impl ScopedToolDispatcher {
    pub fn new(registry: Arc<ToolRegistry>, allowed_tools: Vec<String>, agent_id: String) -> Self {
        Self { registry, allowed_tools, agent_id }
    }

    pub async fn invoke(&self, call: &ToolCall) -> ToolResult {
        if !self.allowed_tools.contains(&call.tool_id) {
            tracing::warn!(
                agent_id = %self.agent_id,
                tool_id = %call.tool_id,
                "Sub-agent attempted to invoke a tool outside its permission scope"
            );
            return ToolResult {
                call_id: call.call_id.clone(),
                output_json: serde_json::json!({
                    "error": "PermissionDenied",
                    "message": format!(
                        "Tool '{}' is not in this sub-agent's allowed tools list: {:?}",
                        call.tool_id, self.allowed_tools
                    )
                }).to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some(format!("Tool '{}' not permitted for agent '{}'", call.tool_id, self.agent_id)),
            };
        }
        self.registry.invoke(call).await
    }

    pub fn tool_descriptions(&self) -> Vec<serde_json::Value> {
        self.allowed_tools.iter()
            .filter_map(|name| self.registry.get_description(name))
            .collect()
    }
}
```

---

### 5.7 DAG Execution Engine

```rust
// crates/hydragent-planner/src/dag_executor.rs

use crate::dag::{DagSpec, NodeStatus};
use crate::scheduler::ReadyQueue;
use crate::replan::SelfHealingReplanner;
use hydragent_swarm::{SubAgentSpawner, AgentMailbox};
use std::collections::HashSet;

pub struct DagExecutionEngine {
    spawner: Arc<SubAgentSpawner>,
    mailbox: Arc<AgentMailbox>,
    replanner: Arc<SelfHealingReplanner>,
    max_concurrent_agents: usize,
}

impl DagExecutionEngine {
    /// Execute a full DAG. Returns the aggregated final response.
    pub async fn run(&self, dag: &mut DagSpec) -> anyhow::Result<String> {
        let swarm_id = dag.swarm_id.clone();
        let mut completed: HashSet<String> = HashSet::new();
        let mut failed: HashSet<String> = HashSet::new();
        let mut running: tokio::task::JoinSet<(String, anyhow::Result<NodeResult>)> = tokio::task::JoinSet::new();
        let mut results: Vec<(String, NodeResult)> = Vec::new();

        tracing::info!(
            swarm_id = %swarm_id,
            node_count = dag.nodes.len(),
            edge_count = dag.edges.len(),
            "DAG execution starting"
        );

        loop {
            // Compute the ready set: nodes whose dependencies are all in `completed`
            let ready = self.compute_ready_set(dag, &completed, &failed);

            // Dispatch up to `max_concurrent_agents` ready nodes
            let currently_running = running.len();
            let dispatch_count = (self.max_concurrent_agents - currently_running).min(ready.len());

            for node_id in ready.iter().take(dispatch_count) {
                let node = dag.nodes.iter_mut().find(|n| n.id == *node_id).unwrap();
                node.status = NodeStatus::Running;

                // Inject upstream results into the sub-agent's context
                let upstream_context = self.gather_upstream_results(dag, node_id).await;
                let enriched_description = if upstream_context.is_empty() {
                    node.description.clone()
                } else {
                    format!("{}\n\n## Upstream Results\n{}", node.description, upstream_context)
                };

                let spec = SubAgentSpec {
                    id: node_id.clone(),
                    role: node.name.clone(),
                    system_prompt: format!("You are a specialist agent for: {}. Complete ONLY your assigned task.", node.name),
                    task_description: enriched_description,
                    task_type: node.task_type.clone(),
                    allowed_tools: node.allowed_tools.clone(),
                    model_hint: node.model_hint.clone(),
                    token_budget: node.token_budget,
                    timeout_ms: node.timeout_ms,
                };

                let spawner = self.spawner.clone();
                let node_id_clone = node_id.clone();

                running.spawn(async move {
                    let handle = spawner.spawn(spec).await?;
                    let result = handle.join.await?;
                    Ok::<_, anyhow::Error>((node_id_clone, result?))
                }.map(|r| {
                    let node_id = node_id.clone();
                    match r {
                        Ok((id, result)) => (id, Ok(result)),
                        Err(e) => (node_id, Err(e)),
                    }
                }));
            }

            // Wait for the next agent to complete
            if running.is_empty() {
                // No running agents and no ready nodes = done (or deadlock)
                break;
            }

            if let Some(join_result) = running.join_next().await {
                match join_result {
                    Ok((node_id, Ok(result))) => {
                        tracing::info!(node_id = %node_id, "DAG node completed successfully");

                        // Write result to shared mailbox
                        self.mailbox.write_result(&node_id, &result).await?;

                        // Update node status in DAG
                        if let Some(node) = dag.nodes.iter_mut().find(|n| n.id == node_id) {
                            node.status = NodeStatus::Completed;
                            node.result = Some(result.clone());
                        }

                        completed.insert(node_id.clone());
                        results.push((node_id, result));
                    },
                    Ok((node_id, Err(e))) => {
                        tracing::error!(node_id = %node_id, error = %e, "DAG node failed");

                        // Invoke self-healing replanner
                        let strategy = self.replanner.on_failure(dag, &node_id, &e.to_string()).await?;
                        match strategy {
                            crate::replan::RepairStrategy::Retry { .. } => {
                                // Node was reset to Ready in apply_strategy; it will be dispatched next loop
                            }
                            crate::replan::RepairStrategy::Reroute => {
                                completed.insert(node_id.clone()); // Treat as "done" for dependency resolution
                            }
                            crate::replan::RepairStrategy::Decompose { .. } => {
                                // New nodes added to dag.nodes; they'll be picked up in next loop
                            }
                            crate::replan::RepairStrategy::Escalate { reason } => {
                                failed.insert(node_id.clone());
                                tracing::warn!(node_id = %node_id, reason, "Escalated to user");
                            }
                        }
                    },
                    Err(e) => {
                        tracing::error!(error = %e, "JoinSet error (task panicked)");
                    }
                }
            }

            // Check if all nodes are terminal
            let all_terminal = dag.nodes.iter().all(|n| matches!(
                n.status, NodeStatus::Completed | NodeStatus::Failed | NodeStatus::Skipped
            ));
            if all_terminal { break; }
        }

        // Aggregate results
        let node_results: Vec<NodeResult> = results.into_iter().map(|(_, r)| r).collect();
        let final_response = super::supervisor::SwarmSupervisor::aggregate(&node_results, dag.original_task.clone()).await?;

        tracing::info!(
            swarm_id = %swarm_id,
            completed_nodes = completed.len(),
            failed_nodes = failed.len(),
            "DAG execution complete"
        );

        Ok(final_response)
    }

    fn compute_ready_set(
        &self,
        dag: &DagSpec,
        completed: &HashSet<String>,
        failed: &HashSet<String>,
    ) -> Vec<String> {
        dag.nodes.iter()
            .filter(|node| node.status == NodeStatus::Pending || node.status == NodeStatus::Ready)
            .filter(|node| {
                // All dependencies must be completed (or skipped)
                let deps: Vec<&String> = dag.edges.iter()
                    .filter(|e| e.to == node.id)
                    .map(|e| &e.from)
                    .collect();
                deps.iter().all(|dep_id| completed.contains(*dep_id) || failed.contains(*dep_id))
            })
            .map(|n| n.id.clone())
            .collect()
    }

    async fn gather_upstream_results(&self, dag: &DagSpec, node_id: &str) -> String {
        let upstream_ids: Vec<String> = dag.edges.iter()
            .filter(|e| e.to == node_id)
            .map(|e| e.from.clone())
            .collect();

        let mut context_parts = Vec::new();
        for upstream_id in upstream_ids {
            if let Ok(Some(result)) = self.mailbox.read_result(&upstream_id).await {
                let upstream_name = dag.nodes.iter()
                    .find(|n| n.id == upstream_id)
                    .map(|n| n.name.as_str())
                    .unwrap_or("Upstream Task");
                context_parts.push(format!("### {} Output\n{}", upstream_name, result.content));
            }
        }

        context_parts.join("\n\n")
    }
}
```

---

### 5.8 Swarm Supervisor & Result Aggregator

```rust
// crates/hydragent-swarm/src/supervisor.rs

use hydragent_types::NodeResult;
use crate::dag::DagSpec;

pub struct SwarmSupervisor;

impl SwarmSupervisor {
    /// Synthesize all node results into a single coherent response.
    pub async fn aggregate(
        results: &[NodeResult],
        original_task: String,
    ) -> anyhow::Result<String> {
        if results.is_empty() {
            return Ok("The swarm completed but produced no output.".to_string());
        }

        if results.len() == 1 {
            return Ok(results[0].content.clone());
        }

        // Build a synthesis prompt
        let parts = results.iter()
            .enumerate()
            .map(|(i, r)| format!(
                "**Specialist Output {}** (model: {}):\n{}",
                i + 1, r.model_used, r.content
            ))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let synthesis_prompt = format!(
            r#"
You are a synthesis AI. Multiple specialist agents have worked on a complex task in parallel.
Your job is to merge their outputs into a single, coherent, well-formatted response.

ORIGINAL TASK:
{}

SPECIALIST OUTPUTS:
{}

SYNTHESIS INSTRUCTIONS:
- Do NOT repeat yourself; de-duplicate overlapping content.
- Maintain a logical structure.
- Use headings if the content benefits from structure.
- Be comprehensive but concise.
- If outputs conflict, note the conflict and present both views.
"#,
            original_task, parts
        );

        // Use the cheapest capable model for synthesis (cost optimization)
        // In production: council.route(TaskType::Summarization, budget)
        tracing::info!(
            result_count = results.len(),
            total_tokens = results.iter().map(|r| r.tokens_used).sum::<u32>(),
            "SwarmSupervisor synthesizing {} results",
            results.len()
        );

        // Placeholder: direct LLM call with synthesis prompt
        // Wire to actual ModelProvider in integration
        Ok(format!(
            "[SYNTHESIZED FROM {} AGENTS]\n\n{}",
            results.len(),
            results.last().unwrap().content.clone()
        ))
    }
}
```

---

### 5.9 SubAgent Context Window Manager

```rust
// crates/hydragent-swarm/src/context_manager.rs

use tiktoken_rs::cl100k_base;
use hydragent_types::Message;
use hydragent_memory::MemoryDocument;

pub struct ContextWindowManager {
    token_budget: u32,
    /// Reserve this fraction of budget for the model's response
    response_reservation: f32,
}

impl ContextWindowManager {
    pub fn new(token_budget: u32) -> Self {
        Self { token_budget, response_reservation: 0.25 }
    }

    /// Build a truncated message list that fits within the token budget.
    /// Insertion priority (highest to lowest):
    ///   1. System prompt (never truncated)
    ///   2. Injected memory context (truncated by memory injector)
    ///   3. Upstream dependency results (truncated first if over budget)
    ///   4. Recent conversation history (oldest messages dropped first)
    pub fn build_messages(
        &self,
        system_prompt: &str,
        memory_context: &[MemoryDocument],
        upstream_context: &str,
        history: &[Message],
    ) -> Vec<Message> {
        let bpe = cl100k_base().expect("tiktoken init failed");
        let available = (self.token_budget as f32 * (1.0 - self.response_reservation)) as usize;
        let mut used = 0usize;
        let mut messages: Vec<Message> = Vec::new();

        // 1. System prompt (always included)
        let sys_tokens = bpe.encode_ordinary(system_prompt).len();
        messages.push(Message { role: "system".into(), content: system_prompt.to_string() });
        used += sys_tokens;

        // 2. Memory context injection (bounded by ContextInjector from Phase 2)
        if !memory_context.is_empty() {
            let memory_text = memory_context.iter()
                .map(|m| format!("- {}", m.content))
                .collect::<Vec<_>>()
                .join("\n");
            let memory_msg = format!("## Relevant Long-term Memory\n{}", memory_text);
            let mem_tokens = bpe.encode_ordinary(&memory_msg).len();
            if used + mem_tokens < available {
                messages.push(Message { role: "system".into(), content: memory_msg });
                used += mem_tokens;
            }
        }

        // 3. Upstream context (dependency results)
        if !upstream_context.is_empty() {
            let up_tokens = bpe.encode_ordinary(upstream_context).len();
            if used + up_tokens < available {
                messages.push(Message { role: "system".into(), content: upstream_context.to_string() });
                used += up_tokens;
            } else {
                // Truncate upstream context to fit
                let chars_per_token = 4usize;
                let max_chars = (available - used) * chars_per_token;
                let truncated = &upstream_context[..upstream_context.len().min(max_chars)];
                messages.push(Message { role: "system".into(), content: format!("{}…[truncated]", truncated) });
                used = available;
            }
        }

        // 4. Conversation history (oldest dropped first to fit budget)
        let history_start_idx = self.find_history_start(&bpe, history, available - used);
        for msg in &history[history_start_idx..] {
            messages.push(msg.clone());
        }

        tracing::debug!(
            token_budget = self.token_budget,
            tokens_used = used,
            history_msgs_included = history.len() - history_start_idx,
            "Context window built for sub-agent"
        );

        messages
    }

    fn find_history_start(
        &self,
        bpe: &tiktoken_rs::CoreBPE,
        history: &[Message],
        available: usize,
    ) -> usize {
        let mut total = 0usize;
        let mut start = history.len();

        // Walk history from newest to oldest
        for (i, msg) in history.iter().enumerate().rev() {
            let tokens = bpe.encode_ordinary(&msg.content).len() + 4; // +4 for role overhead
            if total + tokens > available {
                return i + 1; // Start from the message AFTER this one
            }
            total += tokens;
            start = i;
        }

        start
    }
}
```

---

### 5.10 Swarm Observability & Live Diagram

```python
# adapters/swarm_diagram.py

from rich.tree import Tree
from rich.console import Console
from rich.live import Live
from rich.table import Table
import json, time

STATUS_ICONS = {
    "pending":   "⏳",
    "ready":     "🟡",
    "running":   "🔄",
    "completed": "✅",
    "failed":    "❌",
    "skipped":   "⏭️",
}

class SwarmDiagram:
    def __init__(self, swarm_id: str):
        self.swarm_id = swarm_id
        self.console = Console()

    def render_live(self, state_path: str, refresh_interval: float = 0.5):
        """Live-rendering swarm diagram. Updates until all nodes reach terminal state."""
        with Live(self.console, refresh_per_second=1 / refresh_interval) as live:
            while True:
                try:
                    with open(state_path) as f:
                        state = json.load(f)
                    live.update(self._build_diagram(state))
                    if all(n["status"] in ("completed", "failed", "skipped")
                           for n in state["nodes"]):
                        break
                except FileNotFoundError:
                    live.update("[yellow]Waiting for swarm to start…[/yellow]")
                time.sleep(refresh_interval)

    def _build_diagram(self, state: dict) -> Table:
        table = Table(title=f"🐙 Swarm: {state.get('swarm_id', '?')}", expand=True)
        table.add_column("Node", style="bold")
        table.add_column("Status")
        table.add_column("Model", style="dim")
        table.add_column("Tokens", justify="right")
        table.add_column("Time (ms)", justify="right")

        for node in state.get("nodes", []):
            status = node["status"]
            icon = STATUS_ICONS.get(status, "❓")
            result = node.get("result") or {}
            table.add_row(
                node["name"],
                f"{icon} {status}",
                result.get("model_used", "—"),
                str(result.get("tokens_used", "—")),
                str(result.get("execution_ms", "—")),
            )

        return table
```

---

## 6. Built-in Tools (Phase 5 Additions)

### `spawn_agent`

```yaml
name: spawn_agent
description: "Spawn a specialist sub-agent to handle a focused sub-task. Use when the current ReAct loop encounters a problem that is better handled by a dedicated specialist with different tools or a different model."
tier: auto_approve
params_schema:
  type: object
  required: [role, task, allowed_tools]
  properties:
    role:
      type: string
      description: "The specialist role (e.g., 'Python Code Reviewer', 'Security Researcher')"
    task:
      type: string
      description: "Detailed description of what this sub-agent should accomplish."
    task_type:
      type: string
      enum: [code_generation, research, creative_writing, reasoning, summarization, data_extraction, planning, review, general]
      description: "Used by Model Council to route to the best model."
    allowed_tools:
      type: array
      items: { type: string }
      description: "Tools this sub-agent may use (subset of all registered tools)."
    token_budget:
      type: integer
      default: 4000
    timeout_ms:
      type: integer
      default: 30000

output:
  type: object
  properties:
    agent_id:    { type: string }
    result:      { type: string, description: "The sub-agent's final output" }
    model_used:  { type: string }
    tokens_used: { type: integer }
    success:     { type: boolean }
```

---

### `delegate_task`

```yaml
name: delegate_task
description: "Delegate the current task to the full DAG planner for complex multi-step decomposition. Use when a task clearly requires multiple specialist agents working in sequence or in parallel."
tier: auto_approve
params_schema:
  type: object
  required: [task]
  properties:
    task:
      type: string
      description: "The full task description to decompose and execute as a swarm."
    max_agents:
      type: integer
      default: 10
      maximum: 50
      description: "Maximum number of sub-agents to spawn for this task."

output:
  type: object
  properties:
    swarm_id:        { type: string }
    final_response:  { type: string }
    nodes_executed:  { type: integer }
    total_tokens:    { type: integer }
    execution_ms:    { type: integer }
```

---

### `wiki_write`

```yaml
name: wiki_write
description: "Write findings to the shared agent knowledge wiki. Use when you discover information that future agents (or future runs) should know about. Wiki entries persist across swarm runs."
tier: auto_approve
params_schema:
  type: object
  required: [topic, content]
  properties:
    topic:
      type: string
      description: "Wiki page name (e.g., 'rust-web-frameworks', 'deployment-config'). Becomes the filename."
    content:
      type: string
      description: "Markdown content to write. Will be appended to the existing page if it exists."
    append:
      type: boolean
      default: true
      description: "If true, append to existing page. If false, overwrite."

output:
  type: object
  properties:
    page:    { type: string, description: "Path to the created/updated wiki page" }
    written: { type: boolean }
```

---

## 7. Configuration & Environment

### Updated `.env.example`

```ini
# ── Phase 1-4 (unchanged) ─────────────────────────────────────────────────
OPENROUTER_API_KEYS=sk-or-v1-...
PRIMARY_MODEL=nvidia/nemotron-3-ultra-550b-a55b:free
ENABLE_SEMANTIC_MEMORY=true
ENABLE_DREAMING=true
ENABLE_SCHEDULER=true

# ── Phase 5: Agent Swarm ───────────────────────────────────────────────────

# Enable DAG planner for complex tasks
ENABLE_SWARM=true

# Minimum task length (words) before swarm is considered
SWARM_COMPLEXITY_THRESHOLD_WORDS=40

# Maximum number of concurrent sub-agents
MAX_CONCURRENT_AGENTS=20

# Hard cap on total agents per swarm run (Kimi K2.6 supports 300)
MAX_SWARM_SIZE=50

# Directory for swarm execution state and mailboxes
SWARM_DATA_DIR=./data/swarm

# ── Phase 5: Model Council ─────────────────────────────────────────────────

# Model pool configuration (20+ model profiles)
MODEL_COUNCIL_CONFIG=./config/model_council.yaml

# Enable 3-model council vote for high-stakes decisions
ENABLE_COUNCIL_VOTE=false   # Performance cost; enable for critical tasks only

# ── Phase 5: Self-Healing Re-Planner ──────────────────────────────────────

ENABLE_SELF_HEALING=true
MAX_REPAIR_ATTEMPTS=3     # Retries before escalating to user

# ── Phase 5: Agent Wiki ────────────────────────────────────────────────────
WIKI_DIR=./data/wiki
```

### `config/model_council.yaml`

```yaml
primary_model: "nvidia/nemotron-3-ultra-550b-a55b:free"

models:
  - model_id: "anthropic/claude-sonnet-4-5"
    context_window: 200000
    cost_per_1k_tokens: 0.003
    task_type_tags: [code_generation, reasoning, planning, review, creative_writing]
    benchmark_scores:
      code_generation: 88.5
      reasoning: 92.1
      planning: 91.0
      review: 90.0
      creative_writing: 89.0

  - model_id: "deepseek/deepseek-coder-v2"
    context_window: 128000
    cost_per_1k_tokens: 0.0014
    task_type_tags: [code_generation, data_extraction]
    benchmark_scores:
      code_generation: 90.2
      data_extraction: 82.0

  - model_id: "openai/gpt-4o"
    context_window: 128000
    cost_per_1k_tokens: 0.005
    task_type_tags: [reasoning, data_extraction, review, general]
    benchmark_scores:
      reasoning: 89.0
      data_extraction: 88.0
      review: 87.5
      general: 88.0

  - model_id: "perplexity/sonar-pro"
    context_window: 127072
    cost_per_1k_tokens: 0.003
    task_type_tags: [research]
    benchmark_scores:
      research: 95.0

  - model_id: "openai/o1"
    context_window: 200000
    cost_per_1k_tokens: 0.015
    task_type_tags: [reasoning, planning]
    benchmark_scores:
      reasoning: 96.0
      planning: 94.0

  - model_id: "google/gemini-2.5-pro"
    context_window: 1000000
    cost_per_1k_tokens: 0.007
    task_type_tags: [research, reasoning, creative_writing]
    benchmark_scores:
      research: 88.0
      reasoning: 90.0
      creative_writing: 87.0

  - model_id: "meta-llama/llama-3-70b-instruct:free"
    context_window: 8192
    cost_per_1k_tokens: 0.0
    task_type_tags: [summarization, general]
    benchmark_scores:
      summarization: 78.0
      general: 75.0

  - model_id: "openai/gpt-4o-mini"
    context_window: 128000
    cost_per_1k_tokens: 0.00015
    task_type_tags: [summarization, data_extraction]
    benchmark_scores:
      summarization: 80.0
      data_extraction: 81.0

  - model_id: "moonshotai/kimi-k2.6:free"
    context_window: 256000
    cost_per_1k_tokens: 0.0
    task_type_tags: [code_generation, reasoning, general]
    benchmark_scores:
      code_generation: 85.0
      reasoning: 87.0
      general: 84.0
```

---

## 8. Testing Strategy

### 8.1 Unit Tests

| Test File | What It Covers |
|---|---|
| `dag_test.rs` | 5-node linear DAG topo order correct; diamond dependency parallelism; cycle detection returns `Err`; JSON round-trip preserves all fields; `compute_ready_set` correctly identifies ready nodes |
| `council_test.rs` | `route(CodeGeneration, 4000)` returns `deepseek-coder`; `route(Research, 4000)` returns `perplexity-sonar`; `route(Summarization, 200)` returns cheapest model; missing tag falls back to primary |
| `replan_test.rs` | `RepairStrategy::Retry` increments `retry_count` and resets to `Ready`; `Reroute` marks node `Skipped` and removes edges; `Decompose` adds 2 sub-nodes and re-wires edges |
| `mailbox_test.rs` | Write + read round-trip preserves JSON; write to non-existent dir creates it; `read_result` returns `None` for missing file; atomic write (rename) tested by concurrent writers |
| `context_manager_test.rs` | 10k token budget: system (200) + upstream (2000) + history (fills rest); oldest history dropped first when over budget; empty history returns only system+upstream |
| `scoped_dispatcher_test.rs` | Tool not in `allowed_tools` → `ToolStatus::Failure` with `PermissionDenied` message; tool in list → invoked normally |

### 8.2 Integration Tests

```rust
// tests/integration/swarm_e2e_test.rs

#[tokio::test]
async fn test_5_node_dag_parallel_execution() {
    // Create a diamond DAG: A → B, A → C, B → D, C → D, D → E
    // B and C must execute concurrently
    let dag = DagSpec {
        swarm_id: "test-swarm-1".into(),
        original_task: "Research and summarize".into(),
        nodes: vec![
            make_node("A", TaskType::Planning),
            make_node("B", TaskType::Research),
            make_node("C", TaskType::Research),
            make_node("D", TaskType::DataExtraction),
            make_node("E", TaskType::Summarization),
        ],
        edges: vec![
            DagEdge { from: "A".into(), to: "B".into(), label: None },
            DagEdge { from: "A".into(), to: "C".into(), label: None },
            DagEdge { from: "B".into(), to: "D".into(), label: None },
            DagEdge { from: "C".into(), to: "D".into(), label: None },
            DagEdge { from: "D".into(), to: "E".into(), label: None },
        ],
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    // Use instant-returning mock LLM and EchoTool
    let engine = DagExecutionEngine::new_test(MockLLM::default(), EchoTool::default());
    let mut dag = dag;

    let start = std::time::Instant::now();
    let result = engine.run(&mut dag).await.unwrap();
    let elapsed = start.elapsed();

    // Sequential would be 5 × 50ms = 250ms; parallel should be ~150ms (A + parallel(B,C) + D + E)
    assert!(elapsed.as_millis() < 220, "Expected parallel execution to save time");
    assert!(!result.is_empty());

    // Verify all nodes completed
    assert!(dag.nodes.iter().all(|n| n.status == NodeStatus::Completed));
}

#[tokio::test]
async fn test_self_healing_retry() {
    // Node 2 fails once, then succeeds on retry
    let mut fail_count = std::sync::atomic::AtomicU32::new(0);
    let mock_llm = MockLLM::with_behavior(move |prompt| {
        if prompt.contains("node-2") {
            let count = fail_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                return Err(anyhow::anyhow!("Simulated API error"));
            }
        }
        Ok("Mock response".to_string())
    });

    let mut dag = make_linear_dag(3);
    let engine = DagExecutionEngine::new_test(mock_llm, EchoTool::default());
    let result = engine.run(&mut dag).await;

    assert!(result.is_ok(), "DAG should succeed after retry");
    let node2 = dag.nodes.iter().find(|n| n.id == "node-2").unwrap();
    assert_eq!(node2.retry_count, 1);
    assert_eq!(node2.status, NodeStatus::Completed);
}
```

### 8.3 Manual QA Checklist (Phase 5 Sign-off)

```
[ ] Send complex task: "Research Python, Rust, and Go, compare their concurrency models, write a comparison blog post"
    → verify 3 parallel research nodes, 1 comparison node, 1 writing node
    → final response combines all research correctly
[ ] Observe swarm diagram during execution:
    → B and C show 🔄 simultaneously
    → D shows ⏳ until B and C both show ✅
[ ] Run `./hydragent swarm status` after completion → shows all 5 nodes ✅
[ ] Inject artificial failure: modify test to throw error on node 2 after first attempt
    → self-healing fires
    → node 2 retries and completes
    → final result still correct
[ ] Verify Model Council routing:
    → code task assigned to deepseek-coder (check tracing logs)
    → research task assigned to perplexity-sonar
    → summarization task assigned to free model
[ ] Test tool scoping: spawn agent with only `[web_search]` → attempt `file_write` from within → PermissionDenied returned
[ ] Ask agent to "write findings to wiki about Rust frameworks"
    → wiki_write tool fires
    → `data/wiki/rust-frameworks.md` created with content
[ ] Kill agent during swarm execution (Ctrl-C); restart → `data/swarm/{id}/dag.json` present → (Phase 7: resume from state)
[ ] `cargo test --workspace` → exits 0
[ ] `pytest adapters/ -v` → exits 0
```

---

## 9. Performance Targets

| Metric | Target | Measurement Method |
|---|---|---|
| DAG decomposition latency (LLM) | < 5 s | Time from `decompose()` call to `DagSpec` returned |
| Topological sort (100-node DAG) | < 1 ms | `cargo bench -- topo_sort` with 100-node petgraph |
| Ready-set computation (100 nodes) | < 1 ms | HashSet intersection benchmark |
| Model Council routing decision | < 1 ms | In-memory Vec scan with sort |
| Sub-agent spawn latency | < 5 ms | From `spawner.spawn()` to first Tokio task executing |
| 20 concurrent sub-agents throughput | Bus < 5 ms per message | Load test with 20 EchoTool agents |
| Mailbox write + read round-trip | < 10 ms | `tokio::fs` write + read on local SSD |
| Result aggregation (10 results) | < 30 s | Bound by synthesis LLM latency |
| Context window build (100 messages) | < 5 ms | `tiktoken-rs` encode all messages |
| Full 5-node research DAG end-to-end | < 120 s | Real tool execution with actual LLM |
| Self-healing diagnosis LLM call | < 10 s | Cheap fast model for failure diagnosis |

---

## 10. Risks & Mitigations

| Risk | Category | Likelihood | Impact | Mitigation Strategy |
|---|---|---|---|---|
| **DAG decomposition LLM produces a cycle** | Correctness | Medium | High | `DagSpec::validate()` runs `is_cyclic_directed()` immediately after parsing. Returns `Err` → falls back to simple ReAct. |
| **LLM decomposition JSON malformed** | Correctness | High | Medium | `extract_json()` strips markdown wrappers. If parsing still fails, log and fall back to single-agent ReAct (no crash). |
| **Sub-agent token budget explosion** | Cost | Medium | High | `ContextWindowManager` hard caps at `token_budget`. Sub-agents with `token_budget=4000` cannot exceed 4000 input tokens. |
| **Swarm deadlock** (circular dependency not caught) | Correctness | Low | High | `is_cyclic_directed()` at decomposition time + watchdog timer in `DagExecutionEngine::run()` that fires if no progress after 60 s. |
| **Mailbox race condition** (two agents write simultaneously) | Concurrency | Low | Medium | Atomic rename (`write tmp → rename`) prevents partial reads. Each agent writes to its own named file (`{from_agent}.json`), so no two agents share a write path. |
| **Self-healing loop** (re-plan produces same failure) | Logic | Low | Medium | `max_repair_attempts` (default 3) enforced per node. After exhaustion: `RepairStrategy::Escalate`. |
| **Model Council "best model" unavailable** | Reliability | Medium | Low | If top-ranked model returns 429/503, exponential backoff (Phase 1 retry logic) → try 2nd ranked → fallback primary. |
| **300-agent swarm overwhelming OpenRouter API** | Cost/API | Low | High | `MAX_SWARM_SIZE=50` default. `MAX_CONCURRENT_AGENTS=20` throttles dispatching. Configure separate OpenRouter key pool per agent role in Phase 6. |

---

## 11. Definition of Done

Phase 5 is **complete** when all of the following are true:

### Code

- [ ] `cargo build --workspace` and `cargo test --workspace` exit 0 with `RUSTFLAGS="-D warnings"`
- [ ] `cargo clippy --workspace -- -D warnings` exits 0
- [ ] `pytest adapters/ -v` exits 0
- [ ] All Phase 1–4 tests remain green (zero regressions)
- [ ] No `unsafe` blocks in `hydragent-planner` or `hydragent-swarm`

### DAG Planner

- [ ] 5-node diamond DAG: B and C execute in parallel (verified by timing test < 220 ms for 50ms-per-node agents)
- [ ] Cyclic DAG input → `Err(CycleDetected)` returned, no crash
- [ ] Malformed LLM JSON → fallback to simple ReAct, no panic
- [ ] DAG serializes to/from `data/swarm/{id}/dag.json` correctly

### Model Council

- [ ] 20+ model profiles loaded from `config/model_council.yaml`
- [ ] `code_generation` tasks route to `deepseek-coder-v2` (verified in unit test)
- [ ] `research` tasks route to `perplexity-sonar` (verified in unit test)
- [ ] Free models selected when `token_budget` permits

### Swarm Engine

- [ ] 20 concurrent sub-agents load test passes: EventBus latency < 5 ms
- [ ] Scoped tool dispatcher: unauthorized tool → `PermissionDenied` ToolResult
- [ ] Mailbox round-trip: write + read preserves JSON payload exactly

### Self-Healing

- [ ] Node fails once → retried automatically (retry_count incremented)
- [ ] Node fails 3× → escalated (PermissionRequest emitted)
- [ ] `Decompose` strategy: 2 sub-nodes wired correctly into DAG

### Observability

- [ ] `SwarmDiagram` Python renderer shows correct status icons for all node states
- [ ] `./hydragent swarm status` prints completed swarm DAG summary

### Documentation

- [ ] `ARCHITECTURE.md` updated with swarm layer and DAG execution diagram
- [ ] `config/model_council.yaml` committed with all 20+ model profiles
- [ ] `PHASE_5.md` (this file) reviewed and reflects actual implementation

### Release

- [ ] `v0.5.0` git tag created
- [ ] `CHANGELOG.md` entry for v0.5.0 written
- [ ] Demo screencast: 5-node parallel DAG executing with live diagram

---

*Previous phase: [PHASE_4.md](PHASE_4.md) — 40+ Channel Gateway, Proactive Heartbeat & Work IQ (Weeks 15–18)*
*Next phase: [PHASE_6.md](PHASE_6.md) — 16-Layer Security Pipeline: Merkle Audit, Taint Tracking & Ed25519 Signing (Weeks 23–26)*
