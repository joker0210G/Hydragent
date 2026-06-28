# Hydragent: Technical Architecture Specification

> Deep technical specification of the **Hydragent Unified AI Agent** — layers, interfaces, data schemas, execution flows, security boundaries, and training pipelines.

---

## Table of Contents

1. [Architectural Philosophy & Conceptual Mapping](#1-architectural-philosophy--conceptual-mapping)
2. [Runtime Stack & Footprint](#2-runtime-stack--footprint)
3. [Layer-by-Layer Specification](#3-layer-by-layer-specification)
4. [The Event Bus Wire Protocol & JSON-RPC API](#4-the-event-bus-wire-protocol--json-rpc-api)
5. [Python SDK & Plugin System](#5-python-sdk--plugin-system)
6. [Memory Architecture & Dreaming Pipeline](#6-memory-architecture--dreaming-pipeline)
7. [Security Architecture & Cryptographic Vault](#7-security-architecture--cryptographic-vault)
8. [Skill Engine & Auto-Induction Improvements](#8-skill-engine--auto-induction-improvements)
9. [LoRA Fine-Tuning Pipeline](#9-lora-fine-tuning-pipeline)
10. [Subagent Swarm Topology](#10-subagent-swarm-topology)
11. [Deployment Topologies](#11-deployment-topologies)

---

## 1. Architectural Philosophy & Conceptual Mapping

Hydragent synthesizes six architectural principles, each derived from a different tier of the modern AI agent landscape:

| Principle | Source | Technical Expression |
|---|---|---|
| **Separation of Concerns** | LangGraph / CrewAI | Each layer exposes a typed JSON-RPC/gRPC interface; no direct coupling. |
| **Zero-Trust Security** | IronClaw / OpenFang | Every inter-layer call carries a capability token; secrets are never passed as arguments. |
| **Memory-as-Filesystem** | memU | Memory is addressable, navigable, and human-readable (Markdown-first). |
| **Ultra-Low Footprint** | NullClaw / ZeroClaw | Core runtime compiles to a single static binary. |
| **Pluggable Components** | OpenClaw / ZeroClaw | Models, channels, tools, and memory backends swap without recompilation. |
| **Human Primacy** | Microsoft Scout / Devin | No state-mutating action executes without passing through a permission gate. |

### 1.1 The Library Analogy (Hermes Self-Improving Agent Model)

Hydragent operates based on the **Library Analogy** conceptual architecture:

| Library Concept | Self-Improving Representation | Implementation Layer |
|---|---|---|
| **The Desk** | Active execution workspace (commands, skills, search, tool calls) | `react_loop.rs` |
| **Draft Paper** | Ephemeral context of the ongoing conversation | In-memory message list (not written to persistent SQLite tables until session ends) |
| **Page** | Condensed knowledgeable insights + User's personality/habit traits extracted from the session | `nodes` table (type = `"page"`) + `USER.md`/`SOUL.md` |
| **Book** | Topic clusters compiling related pages (e.g. "Aerospace", "AI", "Rust") | `nodes` table (type = `"book"`) |
| **Shelf** | Domain categorization clusters (e.g. "User's Area of Interest", "Way of Thinking") | `nodes` table (type = `"shelf"`) |
| **Web Connections** | Dynamic relationships mapping books to shelves, pages to books, and cross-references | `edges` table (generated/updated via `graphify`) |
| **Librarian** | The Hydragent core, performing actions, dreaming, and managing the library | `dream.rs` / `main.rs` |

### 1.2 Cost-Effective Ingestion Loop: 75% Graphify + 25% LLM

To protect the user's API budgets, we avoid relying entirely on LLMs to build and cluster the knowledge graph. We divide the labor:

```
[Draft Paper] ──► [Librarian (LLM - 25% Cost)] ──► Extracts Summary & Personality
                                                        │
                                            (Passes details to Graphify)
                                                        ▼
[Customized Graphify (Local - 75% Weight)] ◄────────────┘
        │
        ├─► Local AST parsing (finds code dependencies, file nodes)
        ├─► Graphify Clustering (computes Louvain communities for Books & Shelves)
        └─► Writes Page/Book/Shelf nodes and Edges directly to SQLite
```

1. **LLM Role (25% Weight)**:
   - **Summarization**: Compresses the ephemeral **Draft Paper** into a **Page** (insights summary).
   - **Personality/Habits Extraction**: Extracts user personality markers, style habits, and behavior rules (to update `USER.md` and `SOUL.md` under strict character budget caps).
2. **Customizing Graphify for Hydragent (75% Weight - Local & Code-First)**:
   - **Document-Free Mode**: Graphify's file detector (`collect_files` in `detect.py`) is configured to bypass raw documents and markdown files to eliminate redundant LLM extraction costs.
   - **Dynamic Node Ingestion API**: Graphify's `build.py` module is extended to accept our live memory nodes (Pages, Books, Shelves) and user personality records rather than relying purely on filesystem files.
   - **Community Clustering Overrides**: Graphify uses the Louvain community-detection algorithm. We customize this clustering step to automatically organize our generated **Page** nodes into **Books** (topics) and map those Books onto **Shelves** (domains) depending on shared tags and cross-references.

### 1.3 Relational vs Graph Storage: The Hybrid Query Bridge

To prevent the LLM from making duplicate queries, and to minimize execution time, we construct a **Unified Hybrid Query Bridge** in `crates/hydragent-memory/src/retrieval.rs`.

```
                  ┌──────────────────────────────┐
                  │      User Prompt / Query     │
                  └──────────────┬───────────────┘
                                 │
                                 ▼
               [Unified Memory Bridge (Local Retrieval)]
                                 │
         ┌───────────────────────┴───────────────────────┐
         ▼ (Step 1: Local SQLite FTS5)                   ▼ (Step 2: Graph Expansion)
  Finds matching Page Nodes                     Traverses neighbors of matched Pages
  using fast keyword index                      (Books & Shelves) for context
         │                                               │
         └───────────────────────┬───────────────────────┘
                                 │
                                 ▼
                     [Ranked Context Bubble]
                                 │ (Single Injection)
                                 ▼
                     [System Prompt Context]
```

- **Parallel Local Search**: The SQLite keyword index match (FTS5) and the local Graphify AST/network traversal run in parallel using async tokio joins, completing in `< 10ms`.
- **No-LLM Retrieval**: The bridge works entirely without LLM search steps.
- **Single Injection**: By compiling Books, Shelves, and Pages into one ordered string, we prevent redundant context bloat, keeping prompt tokens small and fast to process.

---

## 2. Runtime Stack & Footprint

### 2.1 Core Runtime: Rust + Zig + Python Hybrid

Hydragent uses a **language-per-concern** strategy:

| Layer | Language | Rationale |
|---|---|---|
| Core orchestrator, event bus, tool dispatcher, security vault | **Rust** | Memory safety, Tokio async, auditable `unsafe`, WASM targets, strong crate ecosystem |
| Edge binary (RISC-V / ESP32-S3, optional) | **Zig** | ≤ 678 KB static binary, < 2 ms cold start, first-class cross-compile |
| Channel adapters, RAG pipelines, ML glue, eval harness, SDK | **Python** | Rich ML/LLM libraries, fast prototyping; never used in security-critical or latency-critical paths |

```
Rust core runtime footprint:
  RAM (server):     < 30 MB
  RAM (full):       < 100 MB
  Startup latency:  < 50 ms (Rust binary, cold start)

Zig edge footprint:
  RAM (edge):       < 1 MB
  Startup latency:  < 2 ms (Zig binary, cold start)
```

---

## 3. Layer-by-Layer Specification

### Layer 1: Channel Gateway (Adapters)
- Normalizes all inbound user messages into internal `IntentEvent` schema; formats all outbound responses into channel-appropriate format.
- Deduplication (`Deduplicator`): Indexes inbound messages by `request_id`, `channel_id`, and `user_id` inside an in-memory window of `1000` entries. Identical queries are silently dropped.
- Rate Limiting (`RateLimiter`): Protects the system and backend LLMs from API flooding. Exceeding thresholds drops the messages at the routing gate.

### Layer 2: Event Bus & API Router
- Multiplexes JSON-RPC messages over a TCP loopback socket on port `5000`.
- Manages connection handshakes, heartbeats, and asynchronous process isolation.

### Layer 3: Core Orchestrator
- The reasoning kernel. Takes an `IntentEvent`, queries memory, constructs a DAG execution plan via the Model Router, and coordinates tool execution through the Tool Dispatcher.
- **Plan Mode**: DAG is presented to the user for review before any Build-Mode execution begins; no file writes or tool calls in Plan Mode.

### Layer 4: Memory Layer
- Fuses episodic logs (SQLite), semantic vector memories (Candle all-MiniLM-L6-v2), procedural skills, and declarative files (`USER.md` / `SOUL.md`).

### Layer 5: Model Router & Council
- Routes each LLM call to the optimal model based on task type (code / research / writing / reasoning / embedding / vision), cost budget, latency requirement, and model availability.

### Layer 6: Tool Dispatcher & Security Vault
- Executes tool calls with security enforcement — permission gating, credential injection, egress filtering, and audit logging.

### Layer 7: Execution Sandbox
- WASM (Wasmtime) for lightweight isolated execution.
- Docker containers for full browser automation and code execution.

---

## 4. The Event Bus Wire Protocol & JSON-RPC API

This section describes the inter-process communication protocol used by Hydragent components to exchange messages between the Rust core and various channel adapters.

- **Transport**: TCP over IPv4 Loopback.
- **Host / Port**: `127.0.0.1:5000` (customizable via `BUS_PORT` in `.env`).
- **Framing**: Line-delimited JSON (`\n`).
- **Format**: JSON-RPC 2.0.

### 4.1 JSON-RPC Handlers

#### 1. Registration (`gateway.register`)
Registers the adapter as an active channel.
- **Request**:
  ```json
  {"jsonrpc": "2.0", "method": "gateway.register", "params": {"channel_id": "telegram"}, "id": "reg-1"}
  ```
- **Response**:
  ```json
  {"jsonrpc": "2.0", "result": {"status": "ok"}, "error": null, "id": "reg-1"}
  ```

#### 2. Submitting Input (`intent.submit`)
Submits a user prompt turn.
- **Request**:
  ```json
  {
    "jsonrpc": "2.0",
    "method": "intent.submit",
    "params": {
      "page_id": "session-uuid",
      "channel_id": "telegram",
      "user_id": "user-uuid",
      "content": "Tell me a joke",
      "attachments": [],
      "metadata": {},
      "timestamp": 1700000000000,
      "priority": "normal"
    },
    "id": "intent-1"
  }
  ```
- **Stream Notifications (Server ──► Client)**:
  - **Token Chunk**: `{"jsonrpc": "2.0", "method": "response.token", "params": {"token": "Why "}}`
  - **Status Message**: `{"jsonrpc": "2.0", "method": "response.status", "params": {"status": "[Searching...]"}}`
  - **Permission Prompt**: If a tool requires confirmation:
    ```json
    {
      "jsonrpc": "2.0",
      "method": "response.permission_request",
      "params": {
        "request_id": "req-uuid",
        "page_id": "session-uuid",
        "tool_id": "file_write",
        "params_summary": "Write output to log.txt",
        "tier": "Prompt",
        "expires_at_ms": 1700000030000
      }
    }
    ```
  - **Stream End**: `{"jsonrpc": "2.0", "method": "response.complete", "params": {}}`
- **Final Response**:
  ```json
  {
    "jsonrpc": "2.0",
    "result": {
      "page_id": "session-uuid",
      "content": "Why did the chicken cross the road? To get to the other side!",
      "format": "markdown",
      "consent_requests": [],
      "tool_calls_executed": []
    },
    "error": null,
    "id": "intent-1"
  }
  ```

#### 3. Granting Permission (`permission.respond`)
Approves or denies a pending tool execution request.
- **Request**:
  ```json
  {"jsonrpc": "2.0", "method": "permission.respond", "params": {"request_id": "req-uuid", "approved": true}, "id": "perm-1"}
  ```
- **Response**:
  ```json
  {"jsonrpc": "2.0", "result": {"status": "ok"}, "error": null, "id": "perm-1"}
  ```

---

## 5. Python SDK & Plugin System

The `hydragent_py` package (found in `adapters/`) is the canonical Python surface for the Hydragent kernel. It handles plugin discovery, SDK client connections, and provides the interactive `hydra-cli`.

### 5.1 Quick Start
```python
from hydragent_py import HydraClient

# Send a chat message
with HydraClient.connect() as hydra:
    response = hydra.chat("Summarize today's news.")
    print(response)
```

### 5.2 Plugin Authoring
Create a Python file in `~/.hydragent/plugins/` (e.g., `greet.py`):
```python
from hydragent_py.plugins import PluginContext, ToolSpec

def register(ctx: PluginContext) -> None:
    ctx.add_tool(ToolSpec(
        name="greet",
        description="Greet a user.",
        parameters={"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
        permission="AutoApprove",
        handler=lambda args: f"Hello, {args['name']}!",
    ))
```

---

## 6. Memory Architecture & Dreaming Pipeline

### 6.1 Database Schema
- **Episodic Memory**: Raw logs and compressed daily summaries are stored in SQLite in WAL mode.
- **Semantic Memory**: SQLite `semantic_memories` coupled with an in-house linear-scan vector index (`vectors.bin`).
- **Declarative Memory**: Contained in `config/USER.md` (limited to 6,000 characters) and `config/SOUL.md` (limited to 12,000 characters).

### 6.2 Bounded Markdown Hot Memory (Hermes Pattern)
When declarative memories exceed their character budgets, the system triggers **LLM-driven re-synthesis**:
1. Read the over-limit file contents.
2. Send to the LLM to group, consolidate, and rank facts.
3. Validate that the output fits strictly within the character limit.
4. Overwrite the file with the consolidated version.

---

## 7. Security Architecture & Cryptographic Vault

### 7.1 The Cryptographic Vault
The Vault protects sensitive API keys and tokens from external exploits and LLM exposure.
- **Model-Blind Keys**: Keys are injected directly into network clients at the socket connection level. The LLM never sees them.
- **Dual-Slot Authentication**:
  - **Slot 0 (Passphrase PIN)**: Unlocks the master key using a user-memorable PIN. Key derivation uses **PBKDF2 with SHA256** and a unique salt.
  - **Slot 1 (Admin Key File)**: Unlocks the master key using a local physical cryptographic private key file (e.g., Ed25519). Allows PIN recovery.
- **Security Command Tree**:
  - `hydragent vault init`
  - `hydragent vault set <scope> <value>`
  - `hydragent vault list` (lists scopes only, values are hidden)
  - `hydragent vault delete <scope>`

### 7.2 Remote Vault Administration Challenge-Response
Vault operations bypass the conversational loop and event logs completely. Remote administration uses a secure challenge-response:
1. Client requests a vault write.
2. Server issues a cryptographic challenge (random nonce + timestamp).
3. Client signs the challenge using the derived Passphrase PIN.
4. Server validates the signature against the hash stored in `config/security/admin_auth.hash` and authorizes the write.

---

## 8. Skill Engine & Auto-Induction Improvements

The `hydragent-skills` engine manages reusable skill templates. During the nightly dreaming cycle, successful ReAct trajectories are analyzed and inducted as skills.

### 8.1 Auto-Induction Pipeline

```
[Successful Trajectory]
           │
           ▼
[Smart Parameter Naming]  ──► Uses KEYWORD_MAP to map context nouns (e.g. "path" -> "{{file_path}}")
           │
           ▼
[Syntax-Aware Parsing]   ──► Detects JSON/YAML blocks and replaces them with a single typed parameter
           │
           ▼
[Tool Dependency Map]    ──► Collects the actual tool names used in the trajectory
           │
           ▼
[Quality Gate Check]     ──► Requires passing is_trajectory_successful (no failure keywords, minimum turns)
           │
           ▼
 [Confidence Threshold]  ──► Rejects proposals with an LLM confidence score < 0.72
           │
           ▼
[Semantic Deduplicator]  ──► Merges with active skills if cosine similarity > 0.88
           │
           ▼
    [Active Skill]
```

### 8.2 Version Rollback on Demotion
If an Active skill's success rate falls below `50%`, the Curator attempts a rollback:
1. Check `skill_versions` for the previous working version.
2. If found, restore the previous `prompt_template` and parameters, and reset the execution metrics.
3. The skill remains active for a trial period. If it fails again (or if no previous version exists), it is demoted to `Inactive`.

### 8.3 Config-Driven Thresholds (`config/curator.toml`)
All curator thresholds are loaded from a configuration file at startup, avoiding the need to recompile the binary to tune parameters.

---

## 9. LoRA Fine-Tuning Pipeline

Hydragent includes a lightweight LoRA (Low-Rank Adaptation) pipeline in `tools/finetune/` to fine-tune base causal language models on successful agent trajectories.

```
[Raw Trajectories (jsonl)]
           │
           ▼
   [generate_dataset.py]  ──► Folds system/tool turns, extracts {{skill}} tags
           │
           ▼
    [Dataset (jsonl)]
           │
           ▼
     [train_lora.py]      ──► 4-bit PEFT + bitsandbytes training (Llama-3.2-1B default)
           │
           ▼
    [LoRA Adapter Out]
```

- **Hardware Requirements**: Requires a CUDA GPU with $\ge 16$ GB VRAM for training (e.g., RTX 4080 or A4000) using `bf16`/`fp16` and gradient checkpointing.
- **Taint-Checked**: The dataset generator ensures zero `Secret`-classified credentials or vault contents leak into the training data.

---

## 10. Subagent Swarm Topology

When a task requires multi-agent parallelism, the Core Orchestrator spawns a swarm:
- **Topology**: Parent-child star topology.
- **Heartbeat Protocol**: Workers send heartbeats every 30 seconds. If 3 heartbeats are missed, the parent marks the worker as a `ZOMBIE` and returns its task to the queue.
- **Validation**: Before marking a task `DONE`, the parent validates the output (e.g., running tests for code, or cross-referencing sources for research).

---

## 11. Deployment Topologies

- **Local Desktop (Full Stack)**: Runs the `hydragent` binary locally, talking to a local Ollama instance and running tasks in local Docker sandboxes.
- **Self-Hosted Server (Always-On)**: Runs `hydragent-server` as a headless systemd service, routing communication to messaging adapters (Telegram, Discord, Slack).
- **Edge / Embedded (Offline-First)**: A lightweight `hydragent-edge` Zig binary ($\le 678$ KB) running on RISC-V or ESP32-S3 boards, using a local GGUF model.
- **Enterprise Cloud (TEE-Secured)**: Deployed inside Confidential Computing environments (e.g., AWS Nitro Enclaves) with full memory encryption and remote attestation.

---

*For feature descriptions and status → **[FEATURES.md](FEATURES.md)***
*For onboarding and setup → **[ONBOARDING.md](ONBOARDING.md)***
