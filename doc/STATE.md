# Hydragent: Current Implementation State

> **Purpose**: This document records the **actual** state of the Hydragent codebase
> as of `git rev 3d99366` (June 2026). It is the ground truth for the rest of the
> documentation in `doc/`. Phase docs (`PHASE_1.md` … `PHASE_9.md`) describe the
> *intent*; this file describes what is **in the repository today**.
>
> When you change the code, update this file. When you finish a phase, update the
> matching entry in [§1 Implementation Status](#1-implementation-status).>
> **Patch release 0.6.1 (2026-06-14)**: 4 user-perspective bug fixes from
> `hydragent chat` CLI testing are now in. AskUser over-triggering is fixed
> via the rewritten strategy-router prompt; pending-clarification state bleed
> is fixed via the new `looks_like_clarification_answer` heuristic; the LLM
> can now call `sanitizer_list_patterns` to inspect the pattern library; the
> `security vault-init` subcommand exists and is idempotent. See
> `CHANGELOG.md` [0.6.1] for the full list.
>
> **Phase 7 COMPLETE 2026-06-14**: see `PHASE_7_COMPLETION_SUMMARY.md` for the 4-week plan
> (Weeks 27-30 → v0.7.0). `hydragent-skills` and `hydragent-bench` crates shipped;
> 86 net-new tests pass (52 skills + 30 bench + 4 skill-induction in core). See
> `RELEASE_NOTES_v0.7.0.md` for the full feature list and `CHANGELOG.md` [0.7.0].
---

## 0. How to read this document

* Every claim has been verified against the working tree at `3d99366`.
* Phase docs that disagree with this file are out of date — update them, don't
  override this file.
* Anything listed under [§3 Not Yet Implemented](#3-not-yet-implemented) is a
  future task, not a documentation bug.

---

## 1. Implementation Status

| Phase | Theme | Status | Notes |
|---|---|---|---|
| 1 | Core Runtime & Zig Bootstrap | **Implemented (Rust path)** | ReAct loop, CLI adapter, 3 primitive tools live. Zig edge binary stubbed but not the primary runtime. |
| 2 | Hierarchical Memory & BM25 Engine | **Implemented — Phase 2 tests GREEN (21/21)** | SQLite-backed memory, vector index (`vectors.bin`, linear scan — **not HNSW**), hybrid retrieval, Dreaming scaffolding. **Bus RPC `memory.search` is now wired** (added 2026-06-12, see `doc/archive/phases/PHASE_2_FINAL_REPORT.md` Bug 1). FTS5 triggers (`fts_insert` / `fts_update` / `fts_delete`) ARE present and working. ChromaDB mentioned in docs but **not used** in code. Stress-test report: `doc/archive/phases/PHASE_2_FINAL_REPORT.md`. |
| 3 | Sandboxed Execution & 3-Tier Permissions | **Partially Implemented** | `PermissionTier` enum and `ToolStatus` are live; `hydragent-vault` (XChaCha20-Poly1305 + Argon2id + KeyInjector) is live; `hydragent-sandbox` is wired to Wasmtime with fuel metering. **Docker execution sandbox is NOT implemented** and has been moved to a later phase. Audit log and taint-tracking subsystems are stubs. |
| 4 | Multi-Channel Gateway & Proactive Agent | **Mostly Implemented (Weeks 15–18 complete)** | 6 messaging adapters + miniapp + bus client. Cron engine + scheduler live with SQLite persistence. Work IQ engine (`WorkIqEngine`) live with RSS polling + keyword alerts. WhatsApp, IMAP/SMTP, voice, WebSockets not yet wired. |
| 5 | Subagent Swarm & Model Council | **Weeks 19–20 done; Weeks 21–22 not started** | `hydragent-planner` (Week 19) is real and working: `dag.rs` (cycle detection, topo sort), `decomposer.rs` (LLM-driven complexity classification + decomposition), `scheduler.rs` (`ReadyQueue`), `serializer.rs` (save/load), and `bin/planner_demo.rs` interactive demo. 5 unit tests in `tests/planner_tests.rs`. **Track 5.1 (Week 20 Mon–Wed)**: `hydragent-swarm` crate shipped with `SubAgent`, `SubAgentSpawner`, `SwarmCoordinator` — 35 tests (9 unit + 10 agent + 10 coordinator + 6 load). G6 confirmed: 20 concurrent sub-agents < 2s. **Track 5.2 (Week 20 Wed–Sat)**: Model Council shipped — 23 profiles in `config/model_council.yaml`, `ModelProfile` + `ModelCouncil` in `hydragent-model`, `SubAgentSpawner::spawn_with_council` routes by `SubAgentRole`, `SubAgentStatus.model_used` reports the routed model id. 30 new unit tests in `hydragent-model` + 9 new integration tests in `hydragent-swarm/tests/council_spawn_test.rs`. G4 satisfied. **Track 5.3 (Week 21)**: not started — needs `DagExecutionEngine` + `AgentMailbox`. **Track 5.4 (Week 22)**: not started — needs supervisor + self-healing replanner. |
| 6 | 16-Layer Security & Audit Hardening | **Tracks 6.1–6.4 shipped; v0.6.1 user-perspective fixes landed; Track 6.5 deferred post-MVP** | `hydragent-vault` Phase 3 + Track 6.4 (mlock, SecureBuffer, ColumnCipher, Rotator) is live and 79 tests pass. `hydragent-security` crate (Tracks 6.1 Merkle chain, 6.2 taint tracker, 6.3 injection guard) is **on disk and cross-crate integrated** as of 0.6.1 — 5 new LLM tools (`audit_query`, `taint_check`, `sanitizer_scan`, `sanitizer_list_patterns`, `vault_rotate`) are registered in `hydragent-core` and reachable from the strategy router. The new `security vault-init` subcommand wraps the existing `Vault::init` with an idempotent path. **Track 6.5 (SQLCipher at-rest for SQLite) deferred to post-MVP** per 2026-06-14 decision. |
| 7 | Self-Improving Skill Engine & Curator | **✅ COMPLETE 2026-06-14 (v0.7.0)** | `hydragent-skills` shipped (SkillLibrary + Hermes extractor + Executor + 7-Day Curator + Composer; 48 unit + 4 integration tests). `hydragent-bench` shipped (SKILL-BENCH 80 tasks + Golden Set 30 pairs; 25 unit + 5 integration tests). Python LoRA fine-tuning pipeline shipped in `tools/finetune/`. Dreaming integration in `hydragent-core/src/skill_induction.rs` (4 tests). 3 builtin skills in `skills/builtin/`. See `RELEASE_NOTES_v0.7.0.md`. |
| 8 | Edge Hardware & Local Inference | **Stubbed** | `edge/` Zig workspace present (`build.zig`, `build.zig.zon`, `src/`) but not yet compiling or running a model. |
| 9 | Enterprise Features & Public Release | **Not Started** | — |

### 1.1 Crates in the workspace

From `Cargo.toml` (`resolver = "2"`, members list):

| Crate | Purpose | Phase |
|---|---|---|
| `hydragent-core` | Main binary, orchestrator, audit | 1, 3 |
| `hydragent-types` | Shared structs: `IntentEvent`, `AgentResponse`, `ToolCall`, `Message`, `CronJob`, … | 1, 4 |
| `hydragent-bus` | TCP event bus + protocol (`PROTOCOL.md`) | 1, 4 |
| `hydragent-model` | LLM provider adapters (OpenRouter, etc.) | 1, 5 |
| `hydragent-tools` | Tool registry + 12 built-in tools | 1, 2, 4 |
| `hydragent-memory` | Session store, semantic store, vector index, retrieval, context injector | 2 |
| `hydragent-embed` | Embedding provider | 2 |
| `hydragent-vault` | Encrypted secret storage (XChaCha20-Poly1305 + Argon2id, mlock-pinned SecureBuffer, AES-256-GCM column cipher, key rotation) | 3, 6.4 |
| `hydragent-sandbox` | Sandboxed execution surface (Wasmtime / container) | 3 |
| `hydragent-gateway` | Multi-channel adapter hosting process | 4 |
| `hydragent-scheduler` | Cron scheduler + heartbeat engine | 4 |
| `hydragent-planner` | DAG / planning (Week 19 of Phase 5 — working) | 5 |
| `hydragent-swarm` | Subagent spawner + coordinator + Model Council routing (Week 20 of Phase 5 — working) | 5 |
| `hydragent-skills` | Skill library, Hermes-style extractor, executor, 7-day curator, composer | 7 |
| `hydragent-security` | On disk (untracked) — Tracks 6.1, 6.2, 6.3 in code, builds clean; cross-crate tests not yet run; **Track 6.5 SQLCipher deferred post-MVP** | 6 |
| `hydragent-bench` | SKILL-BENCH (80 retrieval tasks) + Golden Set (30 multi-relevance pairs) benchmark harness + CLI | 7 |

### 1.2 Channel adapters actually shipped

`adapters/` (Python):

| File | Status | Notes |
|---|---|---|
| `cli_adapter.py` | ✅ Live | First channel, default user. |
| `telegram_adapter.py` | ✅ Live | Real bot integration. |
| `discord_adapter.py` | ✅ Live | Slash commands + embeds. |
| `slack_adapter.py` | ✅ Live | Bolt-style. |
| `email_adapter.py` | ✅ Live | IMAP/SMTP. |
| `webhook_adapter.py` | ✅ Live | Generic inbound webhook. |
| `bus_client.py` | ✅ Live | Talks directly to the Rust bus. |
| `formatter.py` | ✅ Live | Channel-agnostic message rendering. |
| `test_connection.py` | ✅ Live | Adapter smoke test. |
| `generate_library_graph.py` | ✅ Live | Builds the D3.js graph for the miniapp. |
| `miniapp/` | ✅ Live | D3 graph + glassmorphism UI. |
| WhatsApp, Signal, Matrix, iMessage, Teams, Lark, etc. | ❌ Not present | Phase 4 spec lists them; not built. |

### 1.3 Tools actually registered

`crates/hydragent-tools/src/lib.rs` declares 12 modules:

| Tool | Phase | Notes |
|---|---|---|
| `echo` | 1 | Sanity tool. |
| `web_search` | 1 | Web search. |
| `file_read` | 1 | Host filesystem read. |
| `memory_store` | 2 | Persist semantic fact. |
| `memory_search` | 2 | Hybrid retrieval. |
| `memory_forget` | 2 | Delete semantic fact. |
| `standing_orders` | 2 | Read/write persistent rules (lives in `tools/`, not `memory/` as Phase 2 doc suggested). |
| `user_profile` | 2 | USER.md accessor. |
| `send_message` | 4 | Channel-agnostic outbound message. |
| `schedule_task` | 4 | Cron job registration. |
| `rss_subscribe` | 4 | RSS feed poller. |
| (14) `tool_trait`, `registry` | — | Plumbing, not user-facing. |

### 1.4 Bus RPC methods (JSON-RPC 2.0 over TCP, port 5000)

`crates/hydragent-core/src/main.rs` registers the following methods on
the router. **Note**: tools (above) and bus RPC methods (below) are
*not* the same surface — tools are invoked by the LLM, bus RPCs are
invoked by Python adapters / external clients.

| RPC method | Handler | Notes |
|---|---|---|
| `intent.submit` | orchestrator | Submit a user intent; runs full ReAct loop |
| `memory.list` | `MemoryListHandler` | List semantic memories (filterable by `tag`, `limit`) |
| `memory.search` | `MemorySearchHandler` | **Added 2026-06-12** — hybrid BM25 + vector + RRF search |
| `memory.delete` | `MemoryDeleteHandler` | Delete by id; cascades to FTS5 + tags + vector (fixed 2026-06-11) |
| `memory.clear` | `MemoryClearHandler` | Wipe all semantic memories (idempotent) |

The complete bus protocol is documented in
[`crates/hydragent-bus/PROTOCOL.md`](../crates/hydragent-bus/PROTOCOL.md).

---

## 2. Type-level reality

The canonical types live in `crates/hydragent-types/src/lib.rs`. The phase docs
sometimes drift from these names; the truth is below.

### 2.1 `IntentEvent`

```rust
pub struct IntentEvent {
    pub page_id: String,        // UUID v4 — uniquely identifies the page
    pub channel_id: String,     // e.g. "cli:default", "telegram:123456789"
    pub user_id: String,
    pub content: String,
    pub attachments: Vec<Attachment>,
    pub metadata: HashMap<String, String>,
    pub timestamp: i64,         // Unix epoch milliseconds
    pub priority: Priority,     // Urgent | Normal | Background
}
```

> **Important**: Earlier phase docs (Phase 4 onward) call this `session_id` in
> the diagrams. The Rust code has **always** used `page_id` and the field is
> applied consistently across the bus, gateway, and adapters. Treat `page_id`
> as canonical. (`session_id` is reserved for a future grouping concept; do
> not introduce it as a synonym.)

### 2.2 `AgentResponse`

```rust
pub struct AgentResponse {
    pub page_id: String,
    pub content: String,
    pub format: ResponseFormat,                  // Markdown | Plain | Html
    pub consent_requests: Vec<ConsentRequest>,   // Phase 3+
    pub tool_calls_executed: Vec<ToolCallRecord>,
}
```

### 2.3 `ToolCall` / `ToolResult` / `ToolCallRecord`

```rust
pub struct ToolCall {
    pub call_id: String,
    pub tool_id: String,
    pub params_json: String,    // JSON-encoded params, NO raw credentials
    pub tier: PermissionTier,   // AutoApprove | Prompt | Deny
}

pub struct ToolResult {
    pub call_id: String,
    pub output_json: String,
    pub status: ToolStatus,     // Success | Failure | Timeout
    pub execution_ms: u32,
    pub error_message: Option<String>,
}

pub struct ToolCallRecord {     // Stored in audit log
    pub call_id: String,
    pub tool_id: String,
    pub params_hash: String,    // SHA-256 of params
    pub status: ToolStatus,
    pub execution_ms: u32,
    pub timestamp: i64,
}
```

### 2.4 `Message` / `MessageRole`

```rust
pub struct Message {
    pub id: i64,
    pub page_id: String,        // not session_id
    pub role: MessageRole,      // User | Assistant | System | Tool
    pub content: String,
    pub timestamp: i64,
    pub token_count: Option<i32>,
}
```

### 2.5 `MemoryDocument`

```rust
pub struct MemoryDocument {
    pub id: String,
    pub content: String,
    pub timestamp: i64,
    pub importance: i64,        // note: integer, not float
    pub rrf_score: f64,
}
```

### 2.6 `PermissionRequest` / `PermissionResponse`

```rust
pub struct PermissionRequest {
    pub request_id: String,
    pub page_id: String,        // not session_id
    pub tool_id: String,
    pub params_summary: String,
    pub tier: PermissionTier,
    pub expires_at_ms: i64,
}
```

### 2.7 `PushMessage`

```rust
pub struct PushMessage {
    pub channel_id: String,
    pub page_id: String,        // not session_id
    pub content: String,
    pub markdown: bool,
    pub metadata: HashMap<String, String>,
}
```

### 2.8 `CronJob`

```rust
pub struct CronJob {
    pub id: String,
    pub cron_expr: String,
    pub description: String,
    pub task_type: String,      // "react_loop" | "heartbeat" | "work_iq_digest"
    pub task_params: String,
    pub target_channel_id: String,
    pub status: String,         // "active" | "paused" | "deleted"
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub run_count: i64,
}
```

The executor in `crates/hydragent-core/src/main.rs` dispatches on
`task_type` and supports:

| `task_type`        | Handler                                                       |
|--------------------|---------------------------------------------------------------|
| `react_loop`       | Runs full LLM ReAct loop on `task_params.prompt` (or the raw string) and pushes the streamed response via `HeartbeatEngine` to `target_channel_id` |
| `heartbeat`        | Pushes `task_params` verbatim to `target_channel_id` via `HeartbeatEngine` (proactive relay, no LLM) |
| `work_iq_digest`   | Calls `WorkIqEngine::generate_and_send_digest(task_params, target_channel_id)` — fetches un-digested entries, summarizes them with the LLM, marks them digested, and pushes the digest |

The earlier Phase 4 doc listed `task_type ∈ { "react_loop", "message", "heartbeat", "work_iq_poll" }`;
in the current code the consolidated task types are **`react_loop`**, **`heartbeat`** (renamed
from `message` on 2026-06-13 for naming consistency with `HeartbeatEngine`), and
**`work_iq_digest`** (auto-emitted by the `rss_subscribe` tool).

---

## 3. Not Yet Implemented

Items below appear in the phase docs as if they exist. They don't. Treat them
as forward-looking.

### 3.1 Phase 6 (Security)
* `hydragent-security` crate — **on disk** (untracked, builds clean); 6 source modules + 4 integration test files. Not yet wired into `hydragent-core` end-to-end.
* Merkle-chained audit (`MerkleAuditChain`) — **on disk** (`crates/hydragent-security/src/merkle.rs`); no cross-crate integration test yet.
* Taint tracker + 6 taint categories — **on disk** (`crates/hydragent-types/src/lib.rs` Phase-6 section + `crates/hydragent-security/src/taint.rs`); not yet exercised by the core ReAct loop.
* Ed25519 action signing — **on disk** (`crates/hydragent-security/src/signer.rs`); signer is not invoked by any tool handler yet.
* SGNL-style continuous authorization — **on disk** (`crates/hydragent-security/src/sgnl.rs`); the `Authorize` step is not in the request path yet.
* Prompt-injection scanner — **on disk** (`crates/hydragent-security/src/sanitizer.rs` + `crates/hydragent-security/src/anomaly.rs`); the scanner is not invoked by any channel adapter yet.
* ~~SQLCipher-encrypted SQLite~~ — **deferred to post-MVP** per 2026-06-14 decision (column-AES in the vault already covers secrets; SQLite databases at `data/memory/`, `data/audit/`, `data/sessions/` remain plaintext on disk).
* `mlock`-pinned `SecureBuffer` — ✅ **done** (Track 6.4: `crates/hydragent-vault/src/{mlock,secure_buffer}.rs`, 12 unit tests + 6 integration tests).
* Credential rotation commands — ✅ **done** (Track 6.4: `crates/hydragent-vault/src/{column_cipher,rotator}.rs`, 21 unit tests + 16 integration tests).

### 3.2 Phase 7 (Skills)
* `hydragent-skills` crate
* `hydragent-bench` crate
* `SkillSpec` YAML format
* `SkillExtractor` (Hermes-style induction)
* `SkillExecutor` (ReAct subroutine replay)
* `SevenDayCurator`
* LoRA fine-tuning pipeline (`tools/finetune/`)
* `SKILL-BENCH` task suite
* Golden-set evaluator

### 3.3 Phase 5 (Swarm)
* DAG decomposition engine
* Subagent spawner + IPC mailbox
* 300-agent / 4,000-step swarm ceiling
* Hermes Kanban + heartbeat
* Model Council + 20-model routing table

### 3.4 Phase 4 (Channels)
* WhatsApp, Signal, Matrix, iMessage, Teams, Lark, DingTalk, WeChat, QQ
* IMAP/SMTP OAuth brokering (raw `email_adapter.py` works, but the
  OAuth-by-vault path described in `ARCHITECTURE.md` §6.2 is not implemented)
* Voice: Whisper STT, Coqui TTS
* Web chat widget with WebSocket streaming
* Auth profile rotation with exponential backoff

### 3.5 Phase 2 (Memory)
* ChromaDB integration (the code uses an in-house vector index writing to
  `crates/hydragent-memory/vectors.bin`; ChromaDB is mentioned in the docs
  only as inspiration)

### 3.6 Other
* `tools/finetune/` directory (referenced by Phase 7 but not in the tree)
* `bench/`, `skills/`, `migrations/005_*.sql` (all referenced but not in tree)
* `data/audit/chain.db` (Phase 6)
* `config/security/policy.yaml` (Phase 6)
* `config/security/injection_patterns.yaml` (Phase 6)

---

## 4. Open Questions

These need an owner to resolve before the next phase can land cleanly.

1. **Wasmtime vs. process sandbox in `hydragent-sandbox`**: the crate exists but
   it is unclear which runtime path is the production one. Phase 3 says
   Wasmtime + Docker; the code only exposes the surface.
2. **Vector store backend**: ChromaDB is documented, in-house HNSW is
   implemented. Pick one and align the docs.
3. **Type field `page_id` vs. `session_id`**: keep `page_id` (recommended) and
   rewrite the Phase 4+ diagrams, **or** rename in code. Don't leave both in
   the documentation.
4. **`standing_orders` location**: lives in `hydragent-tools` but is really
   a memory concept. Decide whether to move it.
5. **`hydragent-embed` vs. `hydragent-model`**: both wrap external APIs;
   the boundary between "embedding provider" and "chat provider" is
   enforced only by convention.

---

## 5. Verification recipe

The following commands can be run from the repo root to verify the claims in
this document:

```bash
# Workspace members (should list 12, not 15)
grep -A20 '^\[workspace\]' Cargo.toml

# Tools actually registered
cat crates/hydragent-tools/src/lib.rs

# Adapters actually shipped
ls adapters/*.py

# Canonical types
cat crates/hydragent-types/src/lib.rs

# Phase-6 crate (should be absent)
test -d crates/hydragent-security && echo PRESENT || echo ABSENT

# Phase-7 crates (should be absent)
test -d crates/hydragent-skills && echo PRESENT || echo ABSENT
test -d crates/hydragent-bench  && echo PRESENT || echo ABSENT

# Current HEAD
git rev-parse HEAD
```

If any of these commands disagree with this document, the code is the source
of truth and this file needs updating.
