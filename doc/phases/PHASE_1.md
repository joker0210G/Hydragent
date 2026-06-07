# Phase 1: Core Runtime Bootstrap

> **Timeline**: Weeks 1–6  
> **Theme**: Build the minimum viable agent loop — a persistent **Rust binary** (Tokio async) that can hold a conversation, reason step-by-step, call an LLM, execute a tool, and survive a process restart. An optional ultra-small **Zig edge binary** is scaffolded in parallel for MCU/RISC-V targets.

---

## 📋 Table of Contents

1. [Phase Goals & Exit Criteria](#1-phase-goals--exit-criteria)
2. [Directory & Workspace Layout](#2-directory--workspace-layout)
3. [Technology Decisions](#3-technology-decisions)
4. [Week-by-Week Breakdown](#4-week-by-week-breakdown)
5. [Component Specifications](#5-component-specifications)
   - 5.1 [Rust Workspace & Build System (Cargo)](#51-rust-workspace--build-system-cargo)
   - 5.2 [Core Data Types & Schemas](#52-core-data-types--schemas)
   - 5.3 [Event Bus: JSON-RPC over Unix Socket](#53-event-bus-json-rpc-over-unix-socket)
   - 5.4 [Core Orchestrator & ReAct Loop](#54-core-orchestrator--react-loop)
   - 5.5 [OpenRouter SDK Integration](#55-openrouter-sdk-integration)
   - 5.6 [CLI Channel Adapter (Python)](#56-cli-channel-adapter-python)
   - 5.7 [Basic Tool Registry](#57-basic-tool-registry)
   - 5.8 [Session State (SQLite via sqlx)](#58-session-state-sqlite-via-sqlx)
   - 5.9 [Trait-Based Plugin Interfaces](#59-trait-based-plugin-interfaces)
   - 5.10 [Zig Edge Binary Scaffold (Optional)](#510-zig-edge-binary-scaffold-optional)
6. [Built-in Tools (Phase 1 Subset)](#6-built-in-tools-phase-1-subset)
7. [Configuration & Environment](#7-configuration--environment)
8. [Testing Strategy](#8-testing-strategy)
9. [Performance Targets](#9-performance-targets)
10. [Risks & Mitigations](#10-risks--mitigations)
11. [Definition of Done](#11-definition-of-done)

---

## 1. Phase Goals & Exit Criteria

Phase 1 produces a **single working binary** that serves as the foundation for all subsequent phases. Nothing is abstracted prematurely — the goal is a runnable, testable, auditable core.

### Hard Goals (must achieve before Phase 2)

| # | Goal | Validation |
|---|---|---|
| G1 | Rust core binary (`hydragent-core`) compiles for `x86_64-linux` and `aarch64-linux` | `cargo build --release --target aarch64-unknown-linux-gnu` |
| G2 | Optional Zig edge binary (`hydragent-edge`) compiles for `riscv64-linux`, ≤ 678 KB | `zig build -Dtarget=riscv64-linux-musl -Doptimize=ReleaseSmall` |
| G3 | Rust core cold startup latency < 50 ms; Zig edge < 2 ms | `time ./hydragent-core --ping` / `time ./hydragent-edge --ping` |
| G4 | ReAct loop executes: `web_search` → LLM reasoning → CLI response in < 3 s | `echo "What time is it in Tokyo?" \| ./hydragent-core` |
| G5 | Session state persists across process restarts (SQLite WAL via sqlx) | Kill process, re-launch, ask "What did I just ask?" |
| G6 | OpenRouter API calls stream tokens in real-time to CLI | Token-by-token streaming visible in terminal |
| G7 | All inter-layer calls route through the event bus (no direct coupling) | Code audit: no direct crate imports between orchestrator and gateway |
| G8 | Rust trait objects allow swapping channel/model/tool at runtime | Unit test: swap `MockTool` for `WebSearchTool` via `Box<dyn Tool>` |
| G9 | **Plan Mode** (read-only analysis, no file writes) implemented from Phase 1 | `hydragent-core plan "Refactor this module"` prints plan, no files changed |

### Soft Goals (target but not blocking)

- Terminal output is clean, color-coded, readable (not raw JSON)
- `--help` flag prints usage
- `hydragent --version` prints version string
- Basic error messages are human-readable (not stack dumps)
- `config/SOUL.md` with stub **Standing Orders** section pre-populated at first run
- OpenRouter model pool pre-configured with 20+ model fallback chains

---

## 2. Directory & Workspace Layout

The workspace is a **Cargo workspace** (Rust) with a companion `edge/` directory for the optional Zig binary and a `adapters/` directory for Python channel adapters.

```
hydragent/
│
├── Cargo.toml                    # Workspace root — declares all Rust crates
├── Cargo.lock
│
├── crates/                       # Rust crates (Cargo workspace members)
│   │
│   ├── hydragent-core/           # The main binary crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs           # Entry point: clap arg parsing, runtime init, Tokio spawn
│   │       ├── orchestrator.rs   # ReAct loop, DAG planner (stub), session manager
│   │       ├── react_loop.rs     # Think → Act → Observe → Evaluate async state machine
│   │       └── session.rs        # Session struct, sqlx-backed SQLite persistence
│   │
│   ├── hydragent-types/          # Shared types crate (no logic, no deps)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs            # IntentEvent, AgentResponse, ToolCall, Message, etc.
│   │
│   ├── hydragent-bus/            # Event bus: JSON-RPC over Unix socket
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── message.rs        # JSON-RPC 2.0 framing (serde structs)
│   │       └── router.rs         # Async routing table: method → handler
│   │
│   ├── hydragent-model/          # LLM provider clients
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── router.rs         # Model Router: provider selection, fallback chain
│   │       ├── openrouter.rs     # OpenRouter API client (reqwest + SSE streaming)
│   │       └── ollama.rs         # Ollama local API client (stub in Phase 1)
│   │
│   ├── hydragent-tools/          # Tool registry and built-in tools
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── registry.rs       # Tool registration, lookup, dispatch
│   │       ├── tool_trait.rs     # Tool trait definition (async fn execute)
│   │       ├── web_search.rs     # Tool: DuckDuckGo / SerpAPI
│   │       ├── file_read.rs      # Tool: scoped file read with path-traversal guard
│   │       └── echo.rs           # Tool: echo (debug)
│   │
│   └── hydragent-memory/         # Phase 1: session log only (Phase 2 expands)
│       ├── Cargo.toml
│       └── src/
│           └── session_store.rs  # sqlx SQLite session CRUD
│
├── edge/                         # Optional Zig edge binary (RISC-V / ESP32-S3)
│   ├── build.zig
│   ├── build.zig.zon
│   └── src/
│       └── main.zig              # Minimal Zig binary; reuses same JSON-RPC bus protocol
│
├── adapters/                     # Python channel adapters & tooling
│   ├── pyproject.toml            # uv / Poetry project config
│   ├── cli_adapter.py            # Phase 1 CLI: stdin → JSON-RPC → bus → stdout
│   ├── formatter.py              # Markdown → ANSI terminal renderer
│   └── requirements.txt
│
├── config/
│   ├── SOUL.md                   # Agent identity (persona, values, constraints)
│   ├── standing_orders.md        # Persistent behavioral rules (Standing Orders) — Phase 2 seed
│   ├── USER.md                   # User profile seed (empty at first run)
│   └── tools/
│       ├── web_search.yaml       # Tool config: API key ref, allowed domains
│       └── file_read.yaml        # Tool config: allowed base path
│
├── data/
│   └── sessions/                 # SQLite .db files (one per session_id)
│
├── tests/
│   ├── unit/                     # `cargo test` runs these automatically
│   └── integration/
│       ├── e2e_test.rs           # Rust integration: bus → orchestrator → tool → response
│       └── e2e_cli.py            # Python integration: CLI stdin → full round-trip
│
├── scripts/
│   ├── dev.sh                    # cargo run + adapter hot-reload
│   ├── test.sh                   # cargo test + pytest
│   └── cross_build.sh            # cargo cross + zig build for edge targets
│
├── .env.example
├── README.md
└── PHASE_1.md                    # This file
```

---

## 3. Technology Decisions

> **Team consensus** (adopted from engineering review): **Rust** for the core, **Zig** for the optional edge-only binary, **Python** for adapters and RAG/ML glue.

---

### 3.1 Language Roles at a Glance

| Component | Language | Rationale |
|---|---|---|
| Core orchestrator, DAG planner, event bus, tool dispatcher, security vault | **Rust** | Memory safety, mature async (Tokio), strong WASM/sandboxing, best-in-class hiring pool |
| Edge binary (RISC-V / ESP32-S3, optional) | **Zig** | Absolute minimal footprint (≤ 678 KB), first-class cross-compile, zero runtime |
| Channel adapters, RAG pipelines, ML glue, operator tooling, tests | **Python** | Rich ML/LLM ecosystem, fast prototyping, ideal for non-latency-critical paths |

---

### 3.2 Why Rust for the Core?

Inspired by **ZeroClaw** (Rust single-binary, < 5 MB) and **Moltis** (secure-by-design Rust server with MCP):

| Factor | Rust Advantage |
|---|---|
| **Memory safety** | Borrow checker eliminates entire classes of memory vulnerabilities at compile time |
| **Async runtime** | Tokio provides production-grade, zero-cost async I/O — ideal for the event bus and streaming LLM responses |
| **WASM support** | First-class `wasm32-wasi` target — Phase 3 tool sandboxing compiles Rust tools to WASM trivially |
| **Crate ecosystem** | `reqwest` (HTTP), `sqlx` (SQLite), `serde` (JSON), `tokio` (async), `clap` (CLI) — all mature, well-maintained |
| **Security** | No undefined behaviour by default; `unsafe` is explicit and auditable |
| **Hiring** | Significantly more Rust engineers available vs. Zig |

**Tradeoff**: Steeper learning curve than Python; longer compile times. Mitigated by `cargo-watch` for dev and `sccache` for CI caching.

> **R&D Insight**: **OpenCode** (160K GitHub stars, 7.5M monthly devs) validates Rust-based terminal agents: its Plan/Build mode separation proves that splitting read-only analysis from write-capable execution dramatically reduces errors and improves user trust. We adopt this pattern from Phase 1. **Hermes Agent** (#1 on OpenRouter, 271B tokens in 30 days) demonstrates that Rust-quality agents with learning loops outperform all-Python alternatives on sustained usage.

---

### 3.3 Why Zig for the Edge Binary Only?

Zig's strengths are exactly matched to the edge use case:

| Requirement | Zig Delivers |
|---|---|
| Binary ≤ 678 KB (NullClaw target) | `ReleaseSmall` + musl libc produces <700 KB static binary |
| < 2 ms cold startup | Zero runtime initialisation overhead |
| RISC-V cross-compile | `zig build -Dtarget=riscv64-linux-musl` works without a cross toolchain |
| No Docker / no OS services | Bare-metal compatible; self-contained |

**Scope**: Zig is used **only** in `edge/` — never in the core orchestrator or security vault. The edge binary speaks the same JSON-RPC bus protocol as the Rust core, so they are interchangeable in the gateway layer.

> **R&D Insight**: **NullClaw** (Zig, 678 KB, ~1 MB RAM, <2 ms boot) and **MimiClaw** (C on ESP32-S3, 150 KB, ~0.5W) validate the ultra-lightweight path. **ZeroClaw** (~8.8 MB Rust binary, <5 MB RAM, <10 ms startup) shows a pure-Rust agent can also be competitive for server deployments. The Zig path is reserved for genuine edge constraints.

**Tradeoff**: Smaller ecosystem; Zig is not used anywhere that benefits from crate-level libraries.

---

### 3.4 Why Python for Adapters & Tooling?

| Use Case | Python Advantage |
|---|---|
| Channel adapters (Telegram, Discord bots) | `python-telegram-bot`, `discord.py` — mature, well-documented |
| RAG pipelines (Phase 2+) | `langchain`, `chromadb`, `sentence-transformers`, `faiss` |
| ML model integration | Direct access to Hugging Face, `transformers`, `torch` |
| Operator tooling (CLI scripts, eval harness) | Fast scripting; no compile step for ops tasks |
| Integration tests | `pytest` + `httpx` for easy mocking of the JSON-RPC bus |

**Hard constraint**: Python is **never** used for the security vault, credential injection, or any latency-sensitive path in the orchestrator. Those remain in Rust.

---

### 3.5 Event Bus: JSON-RPC 2.0 over Unix Socket

Full gRPC with protobuf is deferred to Phase 2. In Phase 1, the event bus uses **JSON-RPC 2.0 over a Unix domain socket** — simple, debuggable, and sufficient for a single-process deployment. Key properties:
- Both the Rust core and Python adapters speak the same wire protocol
- The Zig edge binary also implements the same protocol — zero special-casing
- Interface contracts are stable; only the transport layer changes when upgrading to gRPC

---

### 3.6 LLM Provider: OpenRouter

- Single API key accesses Claude, GPT-4o, Gemini, Deepseek, Mistral, and 150+ models
- OpenAI-compatible API — no vendor lock-in; swappable with local Ollama
- SSE streaming support via `reqwest` + `tokio` in Rust
- Built-in fallback when a model is unavailable

---

### 3.7 Database: SQLite via `sqlx` (WAL mode)

- `sqlx` provides compile-time verified SQL queries in Rust (no runtime query string bugs)
- WAL mode allows concurrent async reads during writes (Tokio-safe)
- File-per-session layout for easy inspection, backup, and deletion
- Phase 2 expands the schema into the full hierarchical memory model

---

## 4. Week-by-Week Breakdown

### Week 1 — Workspace, Types, Build System

**Goal**: `cargo build` succeeds; core data types compile; Python adapter env is ready.

| Day | Task |
|---|---|
| Mon | Initialize Cargo workspace with crates: `hydragent-core`, `hydragent-types`, `hydragent-bus`, `hydragent-model`, `hydragent-tools`, `hydragent-memory`. Add `[workspace]` to root `Cargo.toml`. |
| Tue | Define all shared types in `hydragent-types/src/lib.rs`: `IntentEvent`, `AgentResponse`, `ToolCall`, `Message`, `ToolResult`, `ReActContext` — all `serde`-annotated. |
| Wed | Implement structured logging using `tracing` + `tracing-subscriber` (JSON output for prod, pretty ANSI for dev). |
| Thu | Implement `config.rs` using `dotenvy` + `config` crate: `.env` reader with environment-variable override. |
| Fri | Set up `adapters/` Python project: `pyproject.toml`, `uv` virtualenv, install `rich`, `httpx`, `python-dotenv`. |
| Sat | Write unit tests for types (`#[cfg(test)]` blocks). `cargo test` passes. |
| Sun | Scaffold Zig edge workspace in `edge/`; verify `zig build` compiles hello-world for RISC-V. |

**Deliverable**: `cargo build --workspace` green. `cd adapters && uv run python -c "import rich"` works. Zig edge scaffold compiles.

---

### Week 2 — Event Bus & JSON-RPC

**Goal**: Messages flow between Rust orchestrator and Python CLI adapter through the bus with no direct coupling.

| Day | Task |
|---|---|
| Mon | Implement `hydragent-bus/src/message.rs`: JSON-RPC 2.0 serde structs (`JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`) with full `serde_json` round-trip test. |
| Tue | Implement `hydragent-bus/src/router.rs`: `tokio::sync::mpsc`-backed async routing table; method → `async fn handler` dispatch. |
| Wed | Implement `hydragent-bus/src/lib.rs`: Tokio async Unix domain socket server (`tokio::net::UnixListener`) accepting connections from adapters. |
| Thu | Wire gateway → bus → orchestrator path; stub orchestrator echoes content back as `AgentResponse`. |
| Fri | Implement Python bus client in `adapters/bus_client.py`: `asyncio` Unix socket client sending `IntentEvent` JSON-RPC, receiving streamed `AgentResponse`. |
| Sat | Write integration test: Python sends `IntentEvent` → Rust bus → stub orchestrator → Python receives `AgentResponse`. Measure latency target < 1 ms. |
| Sun | Document bus wire protocol in `crates/hydragent-bus/PROTOCOL.md`. |

**Deliverable**: Python adapter and Rust orchestrator communicate through the bus. Verified by cross-language integration test.

---

### Week 3 — CLI Channel Adapter (Python) & Session State (Rust)

**Goal**: A human can type into the terminal and receive a streamed response. Session history persists across restarts.

| Day | Task |
|---|---|
| Mon | Implement `adapters/cli_adapter.py`: async readline loop → wraps input as `IntentEvent` JSON-RPC → sends to bus via `bus_client.py`. |
| Tue | Implement `adapters/formatter.py`: render `AgentResponse.content` (Markdown) to ANSI terminal using `rich.markdown.Markdown`. Stream tokens as they arrive. |
| Wed | Implement `hydragent-memory/src/session_store.rs`: `sqlx` async CRUD — `create_session`, `append_message`, `load_recent(n)`, `list_sessions`. |
| Thu | Initialize SQLite DB on first run (`sqlx::migrate!`). Wire session store into orchestrator: load last 20 messages at turn start; append user+assistant messages after each turn. |
| Fri | Test: start agent, type 3 messages, kill with Ctrl-C, restart, verify history recalled. |
| Sat | Add `--session <id>` CLI arg (parsed by `clap` in `main.rs`) to resume a specific session by ID. |
| Sun | Add `--list-sessions` subcommand; display session list with timestamps and turn counts. |

**Deliverable**: `python adapters/cli_adapter.py` starts a REPL. Messages persist in `data/sessions/<id>.db` across restarts. Demo screencast recorded.

---

### Week 4 — OpenRouter SDK & Streaming (Rust)

**Goal**: Real LLM responses stream into the terminal token-by-token via `reqwest` + Tokio async.

| Day | Task |
|---|---|
| Mon | Implement `hydragent-model/src/openrouter.rs`: `reqwest::Client` POST to OpenRouter `/v1/chat/completions` with `stream: true`. |
| Tue | Implement SSE streaming parser: use `reqwest` byte-stream + `tokio_stream::StreamExt` to parse `data: {...}\n\n` chunks; emit tokens via `tokio::sync::mpsc::Sender<String>`. |
| Wed | Implement retry logic with `tokio::time::sleep` exponential backoff (100 ms → 200 ms → 400 ms, max 3 retries) on HTTP 429/503. |
| Thu | Implement fallback chain in `hydragent-model/src/router.rs`: `claude-sonnet-4` → `gpt-4o` → `mistral-7b-instruct`. Each fallback emits a `tracing::warn!` event. |
| Fri | Wire token stream from model router through the event bus to the Python CLI adapter (bus sends `response.token` notifications per token). |
| Sat | Write mock HTTP tests using `wiremock`: inject SSE fixture, assert token callback fires in order. |
| Sun | Live test: ask 10 questions, verify token-by-token streaming in terminal. Test invalid model triggers fallback. |

**Deliverable**: LLM tokens stream character-by-character in the Python terminal. Model fallback activates and logs correctly.

---

### Week 5 — ReAct Loop (Rust) & Tool Registry (Rust)

**Goal**: The agent can reason, pick a tool, execute it safely, observe the result, and continue — all driven by Tokio async tasks.

| Day | Task |
|---|---|
| Mon | Implement `hydragent-core/src/react_loop.rs`: `async fn run_react_loop(ctx: ReActContext) -> AgentResponse` state machine with `max_steps` guard. |
| Tue | Implement `hydragent-tools/src/registry.rs`: `ToolRegistry` holding `HashMap<String, Box<dyn Tool + Send + Sync>>`; `register()` and `async fn invoke()` methods. |
| Wed | Implement `hydragent-tools/src/tool_trait.rs`: `#[async_trait] pub trait Tool` with `fn name()`, `fn description()`, `fn params_schema()`, `async fn execute()`. |
| Thu | Implement `hydragent-tools/src/web_search.rs`: `WebSearchTool` — `reqwest` GET to DuckDuckGo Instant Answer API; parse top 5 results. |
| Fri | Implement `hydragent-tools/src/file_read.rs`: `FileReadTool` — `std::path::Path::canonicalize()` to detect traversal; only allow paths under `WORKSPACE_DIR`. |
| Sat | Implement `hydragent-tools/src/echo.rs`: `EchoTool` — returns input unchanged. Use in all unit tests as a zero-dependency stand-in. |
| Sun | End-to-end test: `"What is the population of Tokyo?"` → ReAct loop calls `web_search`, receives result, generates answer. |

**Deliverable**: Full async ReAct loop with real tool execution. `cargo test` passes all tool unit tests.

---

### Week 6 — Hardening, Cross-Compilation & Polish

**Goal**: Production-quality Rust binary for desktop/server; Zig edge binary for RISC-V. All tests green. Performance targets met.

| Day | Task |
|---|---|
| Mon | Rust cross-compile: `cargo cross build --release --target aarch64-unknown-linux-gnu` succeeds. Set up `cross.toml`. |
| Tue | Zig edge binary audit: `zig build -Doptimize=ReleaseSmall -Dtarget=riscv64-linux-musl` produces ≤ 678 KB binary. |
| Wed | Startup latency: instrument `main.rs` with `std::time::Instant`; Rust core < 50 ms; Zig edge < 2 ms. |
| Thu | Error handling pass (Rust): all `?` propagations surface `anyhow::Error` with context; no raw `unwrap()` in production paths. |
| Fri | `clap`-based `--help`, `--version`, `--session`, `--list-sessions` flags; Python adapter `--help` via `argparse`. |
| Sat | Full test suite: `cargo test --workspace` + `pytest adapters/`. Fix all failures. |
| Sun | Write Phase 1 completion report; tag `v0.1.0` in git; draft GitHub Release with 3 binary artefacts. |

**Deliverable**: `v0.1.0` git tag. All exit criteria from Section 1 verified and documented.

---

## 5. Component Specifications

### 5.1 Rust Workspace & Build System (Cargo)

**`Cargo.toml`** — workspace root:

```toml
[workspace]
resolver = "2"
members = [
    "crates/hydragent-core",
    "crates/hydragent-types",
    "crates/hydragent-bus",
    "crates/hydragent-model",
    "crates/hydragent-tools",
    "crates/hydragent-memory",
]

[workspace.dependencies]
# Async runtime
tokio          = { version = "1", features = ["full"] }
tokio-stream   = "0.1"
async-trait    = "0.1"

# HTTP
reqwest        = { version = "0.12", features = ["json", "stream"] }
wiremock       = { version = "0.6", optional = true }   # dev/test only

# Serialization
serde          = { version = "1", features = ["derive"] }
serde_json     = "1"

# Database
sqlx           = { version = "0.8", features = ["sqlite", "runtime-tokio", "migrate", "uuid"] }

# CLI
clap           = { version = "4", features = ["derive"] }

# Logging / Tracing
tracing        = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# Config
dotenvy        = "0.15"
config         = "0.14"

# Utilities
anyhow         = "1"
uuid           = { version = "1", features = ["v4"] }
chrono         = { version = "0.4", features = ["serde"] }
sha2           = "0.10"   # For params_hash in ToolCallRecord
```

**`crates/hydragent-core/Cargo.toml`** (the main binary crate):

```toml
[package]
name    = "hydragent-core"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "hydragent"
path = "src/main.rs"

[dependencies]
hydragent-types  = { path = "../hydragent-types" }
hydragent-bus    = { path = "../hydragent-bus" }
hydragent-model  = { path = "../hydragent-model" }
hydragent-tools  = { path = "../hydragent-tools" }
hydragent-memory = { path = "../hydragent-memory" }
tokio            = { workspace = true }
clap             = { workspace = true }
tracing          = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow           = { workspace = true }
dotenvy          = { workspace = true }
config           = { workspace = true }
```

**Cross-compilation** — `Cross.toml` at workspace root:

```toml
[build.env]
passthrough = ["OPENROUTER_API_KEY", "RUST_LOG"]

[target.aarch64-unknown-linux-gnu]
image = "ghcr.io/cross-rs/aarch64-unknown-linux-gnu:main"

[target.riscv64gc-unknown-linux-gnu]
image = "ghcr.io/cross-rs/riscv64gc-unknown-linux-gnu:main"
```

```bash
# Build commands
cargo build --release                                         # host platform
cargo cross build --release --target aarch64-unknown-linux-gnu  # ARM64
cargo cross build --release --target riscv64gc-unknown-linux-gnu # RISC-V (Rust)

# Run tests
cargo test --workspace
```

---

### 5.2 Core Data Types & Schemas

All shared types live in `crates/hydragent-types/src/lib.rs` — the **single source of truth** for the entire system. All types derive `serde::Serialize/Deserialize` for JSON-RPC transport.

```rust
// crates/hydragent-types/src/lib.rs
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Inbound user message, normalised from any channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentEvent {
    /// UUID v4 — uniquely identifies the session
    pub session_id: String,
    /// e.g. "cli:default", "telegram:123456789"
    pub channel_id: String,
    pub user_id: String,
    /// Raw message text
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// Unix epoch milliseconds
    pub timestamp: i64,
    pub priority: Priority,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority { Urgent, #[default] Normal, Background }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub mime_type: String,
    /// Local file path or base64 data URI
    pub data: String,
    pub filename: Option<String>,
}

/// Agent response returned to the channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub session_id: String,
    pub content: String,
    pub format: ResponseFormat,
    #[serde(default)]
    pub consent_requests: Vec<ConsentRequest>,  // Phase 3+
    #[serde(default)]
    pub tool_calls_executed: Vec<ToolCallRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat { #[default] Markdown, Plain, Html }

/// A request to invoke a registered tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,           // UUID v4
    pub tool_id: String,           // e.g. "web_search"
    pub params_json: String,       // JSON-encoded params (NO raw credentials)
    pub tier: PermissionTier,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionTier {
    #[default] AutoApprove,
    Prompt,
    Deny,
}

/// Result returned by a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output_json: String,
    pub status: ToolStatus,
    pub execution_ms: u32,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus { Success, Failure, Timeout }

/// Stored in SQLite for audit display (credentials never stored here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub call_id: String,
    pub tool_id: String,
    pub params_hash: String,    // SHA-256 of params
    pub status: ToolStatus,
    pub execution_ms: u32,
    pub timestamp: i64,
}

/// Consent request sent to user before Prompt-tier tool executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRequest {
    pub call_id: String,
    pub tool_id: String,
    pub description: String,
    pub tier: PermissionTier,
}

/// A single conversation turn stored in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Message {
    pub id: i64,
    pub session_id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: i64,
    pub token_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum MessageRole { User, Assistant, System, Tool }

/// Active reasoning state for one conversation turn.
#[derive(Debug, Clone)]
pub struct ReActContext {
    pub intent: IntentEvent,
    pub history: Vec<Message>,
    pub current_step: u8,
    pub max_steps: u8,
    pub tool_results: Vec<ToolResult>,
    pub final_answer: Option<String>,
}
```

---

### 5.3 Event Bus: JSON-RPC over Unix Socket

In Phase 1, the event bus is implemented in Rust (`hydragent-bus`) using **JSON-RPC 2.0 over a Tokio Unix domain socket**. Both the Rust orchestrator and the Python adapters speak the same wire protocol.

```rust
// crates/hydragent-bus/src/message.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,     // always "2.0"
    pub method: String,
    pub params: Value,
    pub id: String,          // UUID v4
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

// Standard error codes
pub const ERR_PARSE:            i32 = -32700;
pub const ERR_INVALID_REQUEST:  i32 = -32600;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERR_INTERNAL:         i32 = -32603;
// Hydragent-specific codes
pub const ERR_TOOL_FAILED:      i32 = -32001;
pub const ERR_LLM_UNAVAILABLE:  i32 = -32002;
pub const ERR_CONSENT_DENIED:   i32 = -32003;
```

**Python bus client** (`adapters/bus_client.py`):

```python
import asyncio, json, uuid
from pathlib import Path

SOCKET_PATH = Path("/tmp/hydragent.sock")

class BusClient:
    def __init__(self):
        self.reader = None
        self.writer = None

    async def connect(self):
        self.reader, self.writer = await asyncio.open_unix_connection(str(SOCKET_PATH))

    async def send_intent(self, event: dict) -> str:
        """Send an IntentEvent and get back the full AgentResponse content."""
        request = {
            "jsonrpc": "2.0",
            "method": "intent.submit",
            "params": event,
            "id": str(uuid.uuid4()),
        }
        self.writer.write((json.dumps(request) + "\n").encode())
        await self.writer.drain()

        # Collect streamed response tokens until response.complete
        tokens = []
        async for line in self.reader:
            msg = json.loads(line)
            if msg.get("method") == "response.token":
                tokens.append(msg["params"]["token"])
            elif msg.get("method") == "response.complete":
                break
        return "".join(tokens)
```

**Bus methods (Phase 1)**:

| Method | Direction | Description |
|---|---|---|
| `intent.submit` | Gateway → Orchestrator | Submit a new user message for processing |
| `response.stream` | Orchestrator → Gateway | Stream response token by token (notification) |
| `response.complete` | Orchestrator → Gateway | Signal that the full response is ready |
| `tool.invoke` | Orchestrator → Tool Registry | Request tool execution |
| `tool.result` | Tool Registry → Orchestrator | Return tool execution result |
| `session.get` | Orchestrator → Session Store | Load session history |
| `session.append` | Orchestrator → Session Store | Append a new message to session |

---

### 5.4 Core Orchestrator & ReAct Loop

The orchestrator is the **reasoning kernel** of Phase 1. It implements the **ReAct (Reason + Act)** pattern, which forms the basis of most modern agents (Claude Code, Devin, SuperAGI).

```
ReAct Loop State Machine
─────────────────────────

              ┌─────────────────┐
   START ─────► THINK           │
              │  • Build context │
              │  • Query LLM     │
              └────────┬────────┘
                       │
              ┌────────▼────────┐       ┌──────────────┐
              │ PARSE OUTPUT    │──────►│  FINAL ANSWER│──► DONE
              │  • Tool call?   │       │  detected    │
              │  • Final answer?│       └──────────────┘
              └────────┬────────┘
                       │ Tool call detected
              ┌────────▼────────┐
              │ ACT             │
              │  • Invoke tool  │
              │  • Await result │
              └────────┬────────┘
                       │
              ┌────────▼────────┐
              │ OBSERVE         │
              │  • Record result│
              │  • Check errors │
              └────────┬────────┘
                       │
              ┌────────▼────────┐
              │ EVALUATE        │
              │  • Goal reached?│──── Yes ──► FINAL ANSWER
              │  • Step limit?  │──── Yes ──► GIVE UP (explain)
              │  • Error?       │──── Yes ──► RE-PLAN (Phase 5)
              └────────┬────────┘
                       │ No (continue)
                       └─────────────────► THINK (next step)
```

**Orchestrator system prompt template** (injected as the first system message):

```markdown
You are Hydra, a helpful and precise AI agent.

You have access to the following tools:
{{TOOL_DESCRIPTIONS}}

To use a tool, respond with EXACTLY this JSON format (no other text):
{"tool": "<tool_name>", "params": {<tool_params>}}

When you have enough information to answer the user, respond with EXACTLY:
{"answer": "<your final answer to the user>"}

Rules:
- Use tools only when necessary. If you can answer from memory, do so directly.
- Never invent tool results. Always wait for the actual result.
- If a tool fails, explain what happened and offer an alternative.
- Be concise. Prefer bullet points for lists of facts.
- Your persona is defined in SOUL.md. Always reflect those values.
```

**Max steps**: 10 (Phase 1). Configurable via `config.max_react_steps`.

---

### 5.5 OpenRouter SDK Integration

```rust
// crates/hydragent-model/src/openrouter.rs
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

pub struct OpenRouterClient {
    api_key: String,
    client: Client,
    base_url: String,
}

#[derive(Debug, Serialize)]
pub struct LLMRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,   // "system" | "user" | "assistant" | "tool"
    pub content: String,
}

impl OpenRouterClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            client: Client::new(),
            base_url: "https://openrouter.ai/api/v1".into(),
        }
    }

    /// Stream a chat completion. Sends tokens to `tx` as they arrive.
    /// Returns the full concatenated response when the stream ends.
    pub async fn chat_stream(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,   // channel for streaming tokens to bus
    ) -> Result<String> {
        let resp = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://github.com/your-org/hydragent")
            .header("X-Title", "Hydragent")
            .json(request)
            .send()
            .await
            .context("OpenRouter request failed")?;

        let mut full_content = String::new();
        let mut stream = resp.bytes_stream();

        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("SSE chunk error")?;
            let text = std::str::from_utf8(&bytes)?;

            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" { break; }
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        if let Some(token) = v["choices"][0]["delta"]["content"].as_str() {
                            full_content.push_str(token);
                            let _ = tx.send(token.to_string()).await;
                        }
                    }
                }
            }
        }
        Ok(full_content)
    }

    /// Retry wrapper with exponential backoff (100 ms → 200 ms → 400 ms).
    pub async fn chat_stream_with_retry(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
        max_retries: u8,
    ) -> Result<String> {
        for attempt in 0..max_retries {
            match self.chat_stream(request, tx.clone()).await {
                Ok(content) => return Ok(content),
                Err(e) => {
                    let delay = Duration::from_millis(100u64 << attempt);
                    warn!(attempt, delay_ms = delay.as_millis(), error = %e, "Retrying...");
                    sleep(delay).await;
                }
            }
        }
        anyhow::bail!("Max retries ({max_retries}) exceeded for OpenRouter request")
    }
}
```

**Supported model IDs** (Phase 1 routing table):
Managed directly inside the `.env` configuration file (e.g. `PRIMARY_MODEL` and `FALLBACK_MODELS` list).


---

### 5.6 CLI Channel Adapter (Python)

The CLI adapter is implemented in **Python** using `rich` for terminal rendering and `asyncio` for non-blocking token streaming. It is the first channel in Phase 1.

```python
# adapters/cli_adapter.py
import asyncio, sys, uuid
from datetime import datetime
from rich.console import Console
from rich.markdown import Markdown
from rich.prompt import Prompt
from bus_client import BusClient

console = Console()
SESSION_ID = str(uuid.uuid4())
USER_ID    = "local-user"
CHANNEL_ID = "cli:default"

async def main():
    bus = BusClient()
    await bus.connect()

    console.print("[bold cyan]🐉 Hydragent[/bold cyan]  v0.1.0 — Local AI Agent")
    console.print(f"Model: [dim]claude-sonnet-4 via OpenRouter[/dim]")
    console.print(f"Session: [dim]{SESSION_ID}[/dim]  (type [bold]exit[/bold] to quit)\n")

    while True:
        try:
            user_input = await asyncio.get_event_loop().run_in_executor(
                None, lambda: Prompt.ask("[cyan]You ›[/cyan]")
            )
        except (EOFError, KeyboardInterrupt):
            console.print("\n[dim]Goodbye.[/dim]")
            break

        if user_input.strip().lower() in ("exit", "quit"):
            console.print("[dim]Goodbye.[/dim]")
            break

        event = {
            "session_id": SESSION_ID,
            "channel_id": CHANNEL_ID,
            "user_id":    USER_ID,
            "content":    user_input,
            "attachments": [],
            "metadata":   {},
            "timestamp":  int(datetime.utcnow().timestamp() * 1000),
            "priority":   "normal",
        }

        console.print("[green]Hydra ›[/green] ", end="")
        response = await bus.send_intent(event)   # streams tokens to stdout via bus
        # Final render of full response as markdown
        console.print(Markdown(response))
        console.print()

if __name__ == "__main__":
    asyncio.run(main())
```

```
╭─────────────────────────────────────────────────────╮
│  🐉 Hydragent  v0.1.0 — Local AI Agent              │
│  Model: claude-sonnet-4 via OpenRouter               │
│  Session: sess-f7a2b91c  (type 'exit' to quit)       │
╰─────────────────────────────────────────────────────╯

You › What is the capital of France?

Hydra › The capital of France is **Paris**.

You › Search the web for the latest Zig release.

Hydra › [Using tool: web_search]
        Query: "Zig programming language latest release 2026"
        ...
        The latest stable release of Zig is **0.13.0**, released on May 2026.
        Key changes include improved stage2 compiler performance and
        enhanced cross-compilation support.

You › _
```

**Terminal formatting rules**:

| Content Type | Rendering |
|---|---|
| `**bold**` | ANSI Bold (`\e[1m`) |
| `*italic*` | ANSI Italic (`\e[3m`) |
| `` `code` `` | ANSI Bright White on Dark (`\e[97;40m`) |
| Code blocks | Boxed with `─` borders, syntax-highlighted (Phase 1: single color) |
| `[Using tool: X]` | Dim gray with wrench icon (`\e[2m🔧`) |
| User input prompt | Cyan `You ›` |
| Agent output prefix | Green `Hydra ›` |
| Error messages | Red `✗` prefix |

---

### 5.7 Basic Tool Registry

```rust
// crates/hydragent-tools/src/tool_trait.rs
use async_trait::async_trait;
use hydragent_types::ToolResult;

/// Every tool implements this trait. Boxed as `Box<dyn Tool + Send + Sync>`.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn params_schema(&self) -> &str;  // JSON Schema string (shown to LLM)
    async fn execute(&self, params_json: &str) -> ToolResult;
}

// crates/hydragent-tools/src/registry.rs
use std::collections::HashMap;
use std::sync::Arc;
use hydragent_types::{ToolCall, ToolResult, ToolStatus};
use tracing::{info, warn};
use crate::tool_trait::Tool;

/// Thread-safe tool registry. Shared via Arc<ToolRegistry>.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        info!(tool = %name, "Tool registered");
        self.tools.insert(name, Arc::new(tool));
    }

    pub async fn invoke(&self, call: &ToolCall) -> ToolResult {
        let start = std::time::Instant::now();
        match self.tools.get(&call.tool_id) {
            None => {
                warn!(tool_id = %call.tool_id, "Unknown tool invoked");
                ToolResult {
                    call_id: call.call_id.clone(),
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Tool '{}' not found", call.tool_id)),
                }
            }
            Some(tool) => tool.execute(&call.params_json).await,
        }
    }

    /// Build the tool-descriptions block injected into the system prompt.
    pub fn build_system_prompt_block(&self) -> String {
        self.tools.values()
            .map(|t| format!("- **{}**: {}\n  Params (JSON Schema): {}\n",
                             t.name(), t.description(), t.params_schema()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
```

---

### 5.8 Session State (SQLite via `sqlx`)

`sqlx` gives Rust **compile-time verified SQL queries** — any SQL typo is a compile error, not a runtime panic. Schema for Phase 1:

```sql
-- Initialized on first run at data/sessions/{session_id}.db

PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;

-- Stores all conversation turns
CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT    NOT NULL,
    role         TEXT    NOT NULL CHECK(role IN ('user','assistant','system','tool')),
    content      TEXT    NOT NULL,
    token_count  INTEGER,
    timestamp    INTEGER NOT NULL DEFAULT (unixepoch('now','subsec') * 1000)
);

-- Stores tool execution records for this session
CREATE TABLE IF NOT EXISTS tool_calls (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT    NOT NULL,
    call_id       TEXT    NOT NULL UNIQUE,
    tool_id       TEXT    NOT NULL,
    params_hash   TEXT    NOT NULL,  -- SHA-256 of params (credentials never stored)
    status        TEXT    NOT NULL CHECK(status IN ('success','failure','timeout')),
    execution_ms  INTEGER NOT NULL,
    timestamp     INTEGER NOT NULL DEFAULT (unixepoch('now','subsec') * 1000)
);

-- Stores session-level metadata
CREATE TABLE IF NOT EXISTS session_meta (
    session_id    TEXT    PRIMARY KEY,
    created_at    INTEGER NOT NULL,
    last_active   INTEGER NOT NULL,
    turn_count    INTEGER NOT NULL DEFAULT 0,
    model_used    TEXT
);

-- Indexes for fast session history loading
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(session_id);
```

**Session loading** — on each new conversation turn, the orchestrator loads the last N messages (default: 20) from SQLite to reconstruct the context window for the LLM call:

```zig
pub fn loadRecentHistory(
    session_id: []const u8,
    max_messages: u32,
) ![]Message {
    return db.query(
        \\SELECT id, session_id, role, content, token_count, timestamp
        \\FROM messages
        \\WHERE session_id = ?
        \\ORDER BY timestamp DESC
        \\LIMIT ?
    , .{ session_id, max_messages });
}
```

---

### 5.9 Trait-Based Plugin Interfaces

Rust traits replace Zig's vtable pattern and give the same runtime-swappable polymorphism with stronger compile-time guarantees:

```rust
// ── Channel trait ─────────────────────────────────────────────────────────
// crates/hydragent-bus/src/channel.rs

#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    fn channel_id(&self) -> &str;
    /// Receive the next inbound message (awaits until one arrives)
    async fn receive(&self) -> anyhow::Result<IntentEvent>;
    /// Send a complete response back to the user
    async fn send(&self, response: &AgentResponse) -> anyhow::Result<()>;
    /// Send a streaming token (optional; default is no-op until response.complete)
    async fn send_token(&self, _token: &str) -> anyhow::Result<()> { Ok(()) }
    /// Graceful shutdown
    async fn close(&self) {};
}

// ── Model trait ───────────────────────────────────────────────────────────
// crates/hydragent-model/src/model_trait.rs

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn provider_name(&self) -> &str;
    fn is_available(&self) -> bool;
    async fn chat_stream(
        &self,
        request: &LLMRequest,
        token_tx: mpsc::Sender<String>,
    ) -> anyhow::Result<String>;
}

// Usage: swap implementations at runtime via Box<dyn ModelProvider>
// e.g.  let provider: Box<dyn ModelProvider> =
//           if config.use_local { Box::new(OllamaClient::new()) }
//           else                { Box::new(OpenRouterClient::new(api_key)) };
```

---

### 5.10 Zig Edge Binary Scaffold (Optional)

The `edge/` directory contains a minimal Zig binary that:
1. Speaks the **same JSON-RPC bus protocol** as the Rust core
2. Embeds a tiny local LLM (TinyLlama 1.1B 4-bit GGUF via `llama.cpp` C API)
3. Has **no dependency** on Tokio, reqwest, or any Rust crate
4. Compiles to ≤ 678 KB for `riscv64-linux-musl`

```zig
// edge/src/main.zig  — stripped to essentials
const std = @import("std");

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const alloc = gpa.allocator();
    _ = alloc;

    // Connect to Unix socket (same path as Rust bus)
    const sock_path = "/tmp/hydragent.sock";
    const stream = try std.net.connectUnixSocket(sock_path);
    defer stream.close();

    // Minimal JSON-RPC 2.0 handler loop
    var buf: [4096]u8 = undefined;
    while (true) {
        const n = try stream.read(&buf);
        if (n == 0) break;
        // Parse JSON-RPC → run local TinyLlama → send response.complete
        try handleRequest(stream, buf[0..n]);
    }
}
```

> **Scope note**: The Zig edge binary is a Phase 8 deliverable (see ROADMAP.md). In Phase 1, only the scaffold (`edge/src/main.zig` + `edge/build.zig`) is committed — it compiles but returns a hardcoded `"Edge stub"` response. Full TinyLlama integration happens in Phase 8.

---

## 6. Built-in Tools (Phase 1 Subset)

Only 3 tools ship in Phase 1. More are added in Phase 3 and beyond.

### `web_search`

```yaml
name: web_search
description: "Search the web for current information. Returns top 5 results with titles, URLs, and snippets."
tier: auto_approve
params_schema:
  type: object
  required: [query]
  properties:
    query:
      type: string
      description: "The search query string"
    num_results:
      type: integer
      default: 5
      minimum: 1
      maximum: 10

implementation:
  provider: "DuckDuckGo Instant Answer API (no key required)"
  fallback: "SerpAPI (requires SERPAPI_KEY in .env)"
  endpoint: "https://api.duckduckgo.com/?q={query}&format=json&no_html=1"
  timeout_ms: 5000
```

**Output format**:
```json
{
  "results": [
    {
      "title": "Zig Programming Language",
      "url": "https://ziglang.org",
      "snippet": "A general-purpose programming language..."
    }
  ],
  "query": "Zig programming language",
  "result_count": 5
}
```

---

### `file_read`

```yaml
name: file_read
description: "Read the contents of a text file within the agent's workspace directory."
tier: auto_approve
params_schema:
  type: object
  required: [path]
  properties:
    path:
      type: string
      description: "Relative path within the workspace directory (e.g. 'notes/todo.md')"
    max_lines:
      type: integer
      default: 200
      description: "Maximum number of lines to return"

security:
  - Path traversal blocked: paths containing ".." are rejected
  - Absolute paths rejected
  - Allowed base: DATA_DIR/workspace/ (configurable in .env)
  - Max file size: 512 KB
```

---

### `echo`

```yaml
name: echo
description: "Returns its input unchanged. Used for testing the tool invocation pipeline."
tier: auto_approve
params_schema:
  type: object
  required: [message]
  properties:
    message:
      type: string
      description: "Any string to echo back"
```

---

## 7. Configuration & Environment

### `.env` file

```ini
# ── LLM Providers ─────────────────────────────────────────────────────────
OPENROUTER_API_KEY=sk-or-v1-...
# Optional: local Ollama instance (Phase 1 stub)
OLLAMA_URL=http://localhost:11434

# ── Tool Configuration ─────────────────────────────────────────────────────
# Optional: SerpAPI for web_search fallback
SERPAPI_KEY=

# ── Data Paths ─────────────────────────────────────────────────────────────
DATA_DIR=./data
WORKSPACE_DIR=./data/workspace
CONFIG_DIR=./config

# ── Runtime Tuning ─────────────────────────────────────────────────────────
# Primary model to use (OpenRouter model ID)
PRIMARY_MODEL=anthropic/claude-sonnet-4
# Fallback models (comma-separated, tried in order)
FALLBACK_MODELS=openai/gpt-4o,mistralai/mistral-7b-instruct
# Maximum ReAct loop steps before giving up
MAX_REACT_STEPS=10
# Number of recent messages to load into context each turn
CONTEXT_WINDOW_MESSAGES=20

# ── Logging ────────────────────────────────────────────────────────────────
# Options: debug, info, warn, error
LOG_LEVEL=info
# Options: json (for log aggregation), terminal (human-readable ANSI)
LOG_FORMAT=terminal
```

### `config/SOUL.md` — Agent identity

```markdown
# Hydra — Agent Identity

## Name
Hydra

## Personality
Curious, precise, and warm. Prefers concision over verbosity.
Uses bullet points for factual answers; conversational prose for personal topics.
Never pretends to know something it doesn't. Always cites sources.

## Values
- Privacy-first: Never ask for or store sensitive data unless explicitly directed
- Honesty: Never hallucinate facts. Say "I don't know" when uncertain.
- User autonomy: Suggest rather than decide. Always offer to explain reasoning.
- Security: Never expose credentials, keys, or internal state to the user.

## Hard Limits (never violate)
- Do NOT reveal raw API keys or credentials
- Do NOT execute destructive system commands without explicit user approval
- Do NOT claim to be a human
- Do NOT store personal data without the user's knowledge
```

---

## 8. Testing Strategy

### Unit Tests

Every module has a corresponding test file in `tests/unit/`. Tests use Zig's built-in `std.testing` framework.

| Test File | What It Covers |
|---|---|
| `react_loop_test.zig` | State machine transitions; step limit enforcement; tool-call parsing from LLM output |
| `tool_registry_test.zig` | Tool registration; invocation dispatch; error handling for unknown tools |
| `openrouter_test.zig` | SSE stream parsing with mock HTTP response; retry logic with injected errors |
| `session_test.zig` | SQLite read/write; history loading; persistence across simulated restarts |
| `types_test.zig` | JSON serialization/deserialization of all core types |
| `event_bus_test.zig` | Message routing; JSON-RPC framing; Unix socket communication |
| `web_search_test.zig` | Mock HTTP response parsing; result extraction |
| `file_read_test.zig` | Path traversal blocking; file size limits; happy-path read |

### Integration Tests

```bash
# tests/integration/e2e_test.zig

# Test 1: Simple answer without tool
echo "What is 2 + 2?" | ./hydragent --session test-01
# Expected: Agent responds "4" or similar without tool use. Exit 0.

# Test 2: Tool-use flow
echo "Search the web for the population of Tokyo" | ./hydragent --session test-02
# Expected: Output contains [Using tool: web_search], then a factual answer. Exit 0.

# Test 3: Session persistence
echo "My name is Alex." | ./hydragent --session test-03
pkill hydragent
echo "What is my name?" | ./hydragent --session test-03
# Expected: Agent answers "Alex" (recalled from SQLite). Exit 0.

# Test 4: Model fallback
PRIMARY_MODEL=invalid/model ./hydragent --session test-04 <<< "Hello"
# Expected: Agent falls back to gpt-4o, responds normally. Log shows fallback event. Exit 0.

# Test 5: Path traversal protection
echo "Read file ../../etc/passwd" | ./hydragent --session test-05
# Expected: Agent attempts file_read with blocked path; tool returns error; agent explains. Exit 0.
```

### Manual QA Checklist (Phase 1 sign-off)

```
[ ] Start agent fresh; verify welcome banner shows version + model name
[ ] Ask 5 factual questions; verify all use web_search correctly
[ ] Ask 3 conversational questions; verify agent does NOT unnecessarily use tools
[ ] Kill and restart; ask "What did we talk about?"; verify history recalled
[ ] Set PRIMARY_MODEL to garbage value; verify graceful fallback
[ ] Type "exit" or Ctrl+C; verify clean shutdown (no dangling SQLite locks)
[ ] Run `zig build -Dtarget=riscv64-linux-musl`; verify binary produced
[ ] Measure startup time: `time ./hydragent-edge --version`; must be < 2ms
[ ] Measure edge binary size: `ls -lh zig-out/bin/hydragent-edge`; must be ≤ 678 KB
```

---

## 9. Performance Targets

| Metric | Target | Measurement Method |
|---|---|---|
| Cold startup latency (edge binary) | < 2 ms | `time ./hydragent-edge --ping` (custom ping mode) |
| Edge binary size | ≤ 678 KB | `ls -lh zig-out/bin/hydragent-edge` |
| Full binary RAM usage (idle) | < 30 MB | `/proc/{pid}/status VmRSS` after startup |
| Event bus round-trip latency | < 0.5 ms | Measured in `event_bus_test.zig` |
| Session history load (20 messages) | < 5 ms | SQLite query benchmark in `session_test.zig` |
| Token streaming first-byte latency | < 800 ms | Time from sending request to first token printed |
| Full ReAct turn (web_search + answer) | < 3 s | End-to-end integration test timing |

---

## 10. Risks & Mitigations

| Risk | Category | Likelihood | Impact | Mitigation Strategy |
|---|---|---|---|---|
| Rust compile times slow CI | DevEx | Medium | Medium | Use `sccache` for caching; enable `cargo build --timings` to identify hot crates; split Dependabot PRs |
| `tokio` async complexity in orchestrator | Technical | Medium | Medium | Keep all `async fn` at the bus/HTTP boundary; use `tokio::spawn` sparingly; add `tracing` spans for every await |
| `sqlx` offline mode (no DB at compile time) | Technical | Medium | Low | Commit `sqlx-data.json` (generated by `cargo sqlx prepare`) for offline/CI builds |
| SSE streaming parser edge cases (Rust) | Technical | Medium | Medium | Use `wiremock` to inject malformed SSE fixtures in tests; handle partial chunks via `BufReader` |
| Python adapter / Rust bus version drift | Technical | Low | Medium | Pin bus protocol version in `PROTOCOL.md`; add schema validation in Python `BusClient.send_intent()` |
| Path traversal bypass in `FileReadTool` | Security | Low | High | Use `std::path::Path::canonicalize()` then check prefix; fuzz with `cargo-fuzz` |
| Zig edge scaffold compilation on Windows CI | Technical | Low | Low | Build Zig edge binary only on Linux runners; document Windows Zig setup separately |
| OpenRouter API pricing during dev | Cost | Medium | Low | Use `mistral-7b-instruct` (cheapest) for all dev/test calls; add `--dry-run` flag to return mock LLM response |
| LLM output JSON parsing failures | Technical | Medium | Medium | Wrap LLM output parser in fallback: try JSON extract → try regex → return raw string with warning log |

---

## 11. Definition of Done

Phase 1 is **complete** when all of the following are true:

### Code

- [ ] `cargo build --workspace` and `cargo test --workspace` both exit 0 with zero warnings (`RUSTFLAGS="-D warnings"`)
- [ ] All Python adapter tests pass: `pytest adapters/ -v` exits 0
- [ ] Zig edge scaffold compiles: `cd edge && zig build` exits 0
- [ ] No `TODO` or `FIXME` in Phase 1 source files (deferred items in GitHub issues with `phase-2+` label)
- [ ] Every public Rust item has a `///` doc comment; every public Python function has a docstring
- [ ] `cargo clippy --workspace -- -D warnings` exits 0

### Binary Targets

- [ ] `cargo build --release` produces `hydragent` binary for host platform
- [ ] `cargo cross build --release --target aarch64-unknown-linux-gnu` produces ARM64 binary
- [ ] Zig edge binary `hydragent-edge` produced for `riscv64-linux-musl`, size ≤ 678 KB
- [ ] Rust core cold startup < 50 ms; Zig edge cold startup < 2 ms

### Functional

- [ ] Agent answers factual questions using `web_search` correctly (5/5 manual tests pass)
- [ ] Session history persists across 3 simulated restarts
- [ ] Model fallback activates correctly when primary model is unavailable
- [ ] Streaming tokens display in real-time (not buffered until complete)
- [ ] Path traversal blocked for `file_read` tool

### Documentation

- [ ] `README.md` updated with Phase 1 quick-start instructions
- [ ] `PHASE_1.md` (this file) reviewed and reflects actual implementation
- [ ] `ARCHITECTURE.md` updated with any changes discovered during implementation
- [ ] Git history is clean: atomic commits, descriptive messages

### Release

- [ ] `v0.1.0` git tag created
- [ ] `CHANGELOG.md` entry for v0.1.0 written
- [ ] GitHub Release created with pre-built binaries for all 3 targets (Linux x86_64, ARM64, RISC-V)

---

## Appendix A: Development Environment Setup

```bash
# ── Rust toolchain ─────────────────────────────────────────────────────────
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add aarch64-unknown-linux-gnu
cargo install cargo-cross sccache cargo-watch

# ── Zig (for edge binary only) ─────────────────────────────────────────────
curl -L https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz | tar xJ
export PATH="$PWD/zig-linux-x86_64-0.13.0:$PATH"

# ── Python adapter environment ─────────────────────────────────────────────
curl -LsSf https://astral.sh/uv/install.sh | sh
cd adapters && uv sync

# ── Clone and configure ────────────────────────────────────────────────────
git clone https://github.com/your-org/hydragent.git
cd hydragent
cp .env.example .env
# Edit .env: add OPENROUTER_API_KEY

# ── Build and run (Rust core + Python adapter) ─────────────────────────────
RUSTFLAGS="-C link-arg=-fuse-ld=lld" cargo build --release
cargo sqlx prepare --workspace   # generate sqlx-data.json for offline builds
./target/release/hydragent &     # start Rust core (listens on Unix socket)
python adapters/cli_adapter.py   # start Python CLI adapter

# ── Run all tests ──────────────────────────────────────────────────────────
cargo test --workspace
pytest adapters/ -v

# ── Build Zig edge binary ──────────────────────────────────────────────────
cd edge
zig build -Dtarget=riscv64-linux-musl -Doptimize=ReleaseSmall
ls -lh zig-out/bin/hydragent-edge
```

## Appendix B: Key Design References

| Decision | Inspired By | Rationale |
|---|---|---|
| **Rust** for core runtime | ZeroClaw (Rust), Moltis (Rust server) | Memory safety + Tokio async + mature crate ecosystem |
| **Zig** for edge binary only | NullClaw (678 KB Zig), PicoClaw (Go/Zig MCU) | Absolute minimal footprint; first-class RISC-V cross-compile |
| **Python** for adapters | AnythingLLM, Khoj RAG stack | Rich ML/LLM libraries; fast prototyping for non-latency paths |
| ReAct loop | Claude Code, SuperAGI, Devin | Industry-proven agent reasoning pattern |
| Trait-based plugin interfaces | ZeroClaw multi-crate design | `Box<dyn Tool>` swappable at runtime without recompilation |
| JSON-RPC over Unix socket | OpenClaw gateway pattern | Simple, debuggable, language-agnostic transport |
| `sqlx` compile-time SQL | NullClaw / ZeroClaw (SQLite) | Zero runtime query bugs; WAL mode for concurrent access |
| OpenRouter gateway | Perplexity Computer, AnythingLLM | Single key, 150+ models, OpenAI-compatible, no vendor lock-in |
| SOUL.md + USER.md | OpenClaw, MimiClaw | Human-readable persona; LLM-native context injection |
| DuckDuckGo search (no key) | ZeroClaw, PicoClaw | Zero-config tool for rapid prototyping; SerpAPI as fallback |

---

*Next phase: [PHASE_2.md](PHASE_2.md) — Hierarchical Memory & BM25 Engine (Weeks 7–10)*
