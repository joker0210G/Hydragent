   # Hydragent — Current State Analysis

   > **Generated**: 2026-06-18
   > **Repository**: https://github.com/joker0210G/Hydragent.git
   > **Branch**: main
   > **Git HEAD**: 3d99366 (June 2026)
   > **Latest Release**: v0.7.2 (unreleased) / v0.7.1 (2026-06-15)

   ---

   ## 1. What Hydragent Is

   Hydragent is a **unified AI agent runtime** written primarily in **Rust (16 crates)** with **Python channel adapters** and a
 stubbed **Zig edge binary**. It is designed as a privacy-first, model-agnostic AI agent that synthesizes architectural patterns
 from 40+ frontier AI systems into a single runtime.

   The core is a **ReAct loop orchestrator** that:
   - Accepts user intents from multiple channels (Telegram, Discord, Slack, CLI, Email, Webhooks, WebSockets)
   - Queries hierarchical memory (SQLite + BM25 + linear vector scan)
   - Routes LLM calls through a Model Council (20+ model profiles)
   - Executes tools through a permission-tiered dispatcher
   - Runs a self-improving skill engine with a 7-day Curator cycle

   ---

   ## 2. Technology Stack

   | Layer | Language | Key Dependencies |
   |-------|----------|------------------|
   | Core Orchestrator | Rust (Tokio async) | tokio, reqwest, serde, sqlx (SQLite), clap, tracing |
   | Event Bus | Rust | JSON-RPC 2.0 over TCP (port 5000) |
   | Channel Adapters | Python 3.11+ | python-telegram-bot, discord.py, slack-bolt, websockets |
   | Python SDK | Python | hydragent_py package with sync/async client |
   | Memory Engine | Rust + SQLite | sqlx (WAL mode), in-house vector index (linear scan) |
   | Embedding | Rust | onnxruntime + local MiniLM model |
   | Security Vault | Rust | XChaCha20-Poly1305 + Argon2id, AES-256-GCM, Ed25519 |
   | Sandbox | Rust + Wasmtime | WASM execution with fuel metering |
   | Edge Binary | Zig (stubbed) | build.zig present, not compiling |
   | Benchmarks | Rust + Python | SKILL-BENCH (80 tasks), Golden Set (30 pairs), LoRA fine-tuning |

   ---

   ## 3. Workspace Structure (16 Rust Crates)

   | Crate | Purpose | Implementation Status |
   |-------|---------|----------------------|
   | `hydragent-core` | Main binary (`hydragent`), ReAct loop, orchestrator, CLI REPL | ✅ Fully functional — 72+ tests |
   | `hydragent-types` | Shared types: `IntentEvent`, `AgentResponse`, `ToolCall`, `Message`, etc. | ✅ Done |
   | `hydragent-bus` | TCP event bus + JSON-RPC router | ✅ Done |
   | `hydragent-model` | Model Router — OpenRouter, Custom OpenAI, Ollama; Model Council with 20+ profiles | ✅ Done |
   | `hydragent-tools` | Tool registry — 12 built-in tools + 5 Phase 6 security tools + 4 Phase 7 skill tools | ✅ Done |
   | `hydragent-memory` | SQLite session store, semantic store, vector index, retrieval, context injector | ✅ Done — 21/21 tests
 green |
   | `hydragent-embed` | Local embedding via ONNX Runtime + MiniLM | ✅ Done |
   | `hydragent-vault` | Encrypted credential vault (XChaCha20-Poly1305 + Argon2id) + column cipher + key rotation | ✅ Done — 79
 tests |
   | `hydragent-sandbox` | Wasmtime sandbox for tool execution | ✅ Partial — Wasmtime wired, Docker NOT implemented |
   | `hydragent-gateway` | Inbound deduplication + rate limiting for channel messages | ✅ Done |
   | `hydragent-scheduler` | Cron scheduler + heartbeat engine + Work IQ (RSS/keyword alerts) | ✅ Done |
   | `hydragent-planner` | DAG planning: decomposer, scheduler, serializer, cycle detection, topo sort | ✅ Done — Week 19 complete
 |
   | `hydragent-swarm` | Subagent spawner + coordinator + Model Council routing | ✅ Done — Week 20 complete |
   | `hydragent-security` | 16-layer security: Merkle audit, taint tracker, injection guard, sanitizer, SGNL, signer | ⚠️ On disk,
 builds clean, NOT fully wired into ReAct loop |
   | `hydragent-skills` | Skill library (SQLite+FTS5), Hermes extractor, executor, 7-day Curator, Composer | ✅ Done — 52 tests |
   | `hydragent-bench` | SKILL-BENCH eval harness + Golden Set + CLI report generator | ✅ Done — 30 tests |

   **Workspace resolver**: `resolver = "2"`

   ---

   ## 4. Binary Entry Points

   The main binary is **`hydragent`** (`crates/hydragent-core/src/main.rs`). Subcommands:

   | Subcommand | Purpose |
   |------------|---------|
   | *(none)* | Start the JSON-RPC bus server on TCP 127.0.0.1:5000 |
   | `onboard` | Guided `.env` setup wizard with provider/model picker |
   | `chat` | Interactive terminal REPL with slash commands |
   | `doctor` | Diagnostic checks (~10 file-based checks, color-coded) |
   | `examples` | Catalogue of starter prompts |
   | `memory {list,clear}` | Inspect/wipe semantic memories |
   | `embed {list,compute}` | Manage vector embeddings |
   | `vault {init,list,get,set,delete,rotate}` | Encrypted credential management |
   | `test-brain` | Live end-to-end brain connectivity test |
   | `swarm-demo` | Interactive DAG planner demo |

   **Windows convenience**: `Hydragent.cmd` — one-click install/onboard/chat/doctor entry point.

   ---

   ## 5. Data Flow Architecture

 ```

 ┌─────────────────────────────────────────────────────────┐
 │  Channel Adapters (Python)                               │
 │  [Telegram] [Discord] [Slack] [Email] [Webhook] [CLI]   │
 └────────────────────────┬────────────────────────────────┘
                          │ JSON-RPC over TCP:5000
 ┌────────────────────────▼────────────────────────────────┐
 │  hydragent-bus — Event Bus & API Router                  │
 │  • Deduplication • Rate limiting • Session correlation   │
 └────────────────────────┬────────────────────────────────┘
                          │ Dispatched IntentEvent
 ┌────────────────────────▼────────────────────────────────┐
 │  hydragent-core — Orchestrator                           │
 │  • Strategy router (simple vs complex vs clarify)        │
 │  • ReAct loop (Think → Act → Observe → Evaluate)         │
 │  • Tool dispatch via ToolRegistry                        │
 │  • Memory query + model routing                          │
 │  • Audit logging to Merkle chain                         │
 └────────────────────────┬────────────────────────────────┘
            ┌─────────────┼─────────────┐
            ▼             ▼             ▼
     ┌──────────┐  ┌──────────┐  ┌───────────────┐
     │ Memory   │  │ Model    │  │ Tools         │
     │ (SQLite) │  │ Council  │  │ (12 + 5 + 4)  │
     └──────────┘  └──────────┘  └───────────────┘

 ```

   ---

   ## 6. Types (Canonical)

   All types live in `crates/hydragent-types/src/lib.rs`.

   - `IntentEvent` — inbound message with `page_id` (UUID v4), `channel_id`, `user_id`, `content`
   - `AgentResponse` — outbound reply with `content`, `format`, `consent_requests`, `tool_calls_executed`
   - `ToolCall` — `{ call_id, tool_id, params_json, tier }`
   - `ToolResult` — `{ call_id, output_json, status, execution_ms, error_message }`
   - `ToolCallRecord` — audit log entry with SHA-256 `params_hash` (no raw credentials)
   - `Message` — SQLite row for conversation turns (`page_id`, `role`, `content`, `timestamp`)
   - `PermissionTier` — `AutoApprove | Prompt | Deny`
   - `ReActContext` — active reasoning state for one turn

   **Note**: The documentation sometimes uses `session_id`; the **canonical code field is `page_id`**. Do not introduce `session_id`
 as a synonym.

   ---

   ## 7. Memory Layer

   **Storage**: SQLite (WAL mode) at `data/sessions.db` + `data/semantic.db`

   **Tables**:
   - `pages` — conversation pages keyed by `page_id`
   - `messages` — conversation turns (user, assistant, system, tool)
   - `semantic_memories` — facts with FTS5 full-text index
   - `fts` — FTS5 virtual table with insert/update/delete triggers
   - `user_profile` — key/value preferences
   - `standing_orders` — persistent behavioral rules

   **Vector Index**: `crates/hydragent-memory/src/vector_index.rs` — linear scan over `HashMap<String, Vec<f32>>` (NOT HNSW;
 ChromaDB is documented but not used)

   **Retrieval**: Hybrid BM25 (FTS5) + cosine similarity + Reciprocal Rank Fusion (k=60)

   **Dreaming Pipeline**: Consolidation worker in `hydragent-core/src/dream.rs` — gated by `enable_dreaming` flag (default: true)

   ---

   ## 8. Tool Registry (21 Tools)

   `crates/hydragent-tools/src/lib.rs` registers all tools.

   | Tool | Tier | Description |
   |------|------|-------------|
   | `echo` | AutoApprove | Sanity check |
   | `web_search` | AutoApprove | Web search via SearXNG |
   | `file_read` | Prompt | Host filesystem read |
   | `memory_store` | AutoApprove | Persist a semantic fact |
   | `memory_search` | AutoApprove | Hybrid retrieval |
   | `memory_forget` | AutoApprove | Delete semantic fact by ID |
   | `standing_orders` | AutoApprove | Read/write persistent rules |
   | `user_profile` | AutoApprove | Read/write user profile |
   | `send_message` | AutoApprove | Push message to channel |
   | `schedule_task` | Prompt | Register cron job |
   | `rss_subscribe` | AutoApprove | RSS feed polling |
   | `agent_reach` | AutoApprove | Web scraping |
   | `audit_query` | AutoApprove | Query Merkle audit chain (Phase 6) |
   | `sanitizer_scan` | AutoApprove | Scan for injection patterns (Phase 6) |
   | `sanitizer_list_patterns` | AutoApprove | List sanitizer patterns (Phase 6) |
   | `taint_check` | AutoApprove | Check taint status (Phase 6) |
   | `vault_rotate` | Prompt | Rotate vault keys (Phase 6) |
   | `skill_list` | AutoApprove | List skills in library (Phase 7) |
   | `skill_search` | AutoApprove | Search skills by tag/keyword (Phase 7) |
   | `skill_run` | AutoApprove | Execute a skill (Phase 7) |
   | `skill_curator_run` | Prompt | Trigger Curator manually (Phase 7) |

   **Permission tiers**: `AutoApprove` (runs silently), `Prompt` (asks user first), `Deny` (blocked)

   ---

   ## 9. Model Council

   Configuration: `config/model_council.yaml` — 20+ profiles covering 8 task types.

   **Routing logic**: Task type → filter by budget → filter by latency → score by quality×(1/cost)×(1/latency) → select top model.

   **Providers supported**: OpenRouter, Custom OpenAI-compatible, Ollama (local)

   **Profiles include**: DeepSeek Coder, Claude 3.5 Sonnet, Claude 3 Haiku, Perplexity Sonar, Llama 3.1 405B, GPT-4o Mini, GPT-4o,
 Gemini 1.5 Flash, Qwen 2.5, Mistral Large, and more.

   ---

   ## 10. Channel Adapters (Python)

   All in `adapters/` directory. Each reads credentials from environment variables.

   | Adapter | File | Status |
   |---------|------|--------|
   | Telegram Bot | `telegram_adapter.py` | ✅ Live |
   | Discord Bot | `discord_adapter.py` | ✅ Live |
   | Slack Bot | `slack_adapter.py` | ✅ Live |
   | Email (IMAP/SMTP) | `email_adapter.py` | ✅ Live |
   | Generic Webhook | `webhook_adapter.py` | ✅ Live |
   | WebSocket | `websocket_adapter.py` | ✅ Live |
   | CLI/REPL | `cli_adapter.py` (shim) | ✅ Live |
   | Bus Client | `bus_client.py` | ✅ Live |
   | Formatter | `formatter.py` | ✅ Live |

   **NOT implemented**: WhatsApp, Signal, Matrix, iMessage, Teams, Lark, DingTalk, WeChat, QQ, Voice (STT/TTS)

   **Mini App**: `adapters/miniapp/` — D3.js force-directed graph + glassmorphism UI for Telegram Mini App

   **Python SDK**: `adapters/hydragent_py/` — `HydraClient`, `HydraConfig`, `BusClient`, REPL, plugin system

   ---

   ## 11. Security Pipeline

   **Crate**: `hydragent-security` (6 source modules, 4 integration test files)

   | Layer | Component | Status |
   |-------|-----------|--------|
   | Vault | XChaCha20-Poly1305 + Argon2id | ✅ Done |
   | Vault | `mlock`-pinned `SecureBuffer` | ✅ Done |
   | Vault | AES-256-GCM Column Cipher | ✅ Done |
   | Vault | Key Rotator | ✅ Done |
   | Audit | Merkle audit chain | ⚠️ On disk, not invoked by tools |
   | Taint | 6-category taint tracker | ⚠️ On disk, not exercised by ReAct |
   | Injection | Prompt injection scanner + anomaly detection | ⚠️ On disk, not invoked by adapters |
   | Signing | Ed25519 action signing | ⚠️ On disk, not invoked |
   | Auth | SGNL-style continuous authorization | ⚠️ On disk, not in request path |
   | SQLCipher | SQLite at-rest encryption | ❌ Deferred post-MVP |

   **Note**: The 5 Phase 6 tools ARE registered in `hydragent-core` and reachable from the strategy router, but the underlying
 security subsystems (Merkle, taint, sanitizer) are not yet exercised end-to-end in the ReAct loop.

   ---

   ## 12. Skill Engine (Phase 7 — COMPLETE)

   **Crate**: `hydragent-skills`

   **Components**:
   - `library.rs` — SQLite + FTS5 skill storage (CRUD, tag search)
   - `skill.rs` — `Skill`, `SkillSpec`, `SkillParam`, Mustache template rendering
   - `extractor.rs` — Hermes-style deterministic skill induction (no LLM)
   - `executor.rs` — Skill execution with param validation + tool allowlist
   - `curator.rs` — `SevenDayCurator` (0 3 * * 0 cron, tier transitions)
   - `composer.rs` — Merge ≥2 compatible skills, resolve param conflicts
   - `tools.rs` — 5 LLM-callable tool wrappers

   **Built-in skills** (shipped in `skills/builtin/`):
   - `convert-csv-to-json.yaml`
   - `summarize-github-issue.yaml`
   - `debug-rust-error.yaml`

   **Database schema**: `migrations/005_skill_library.sql` — `skills`, `skill_versions`, `skill_tags`, `skill_executions`,
 `skills_fts`

   ---

   ## 13. Benchmark Harness

   **Crate**: `hydragent-bench`

   - `dataset.rs` — JSONL loaders for SKILL-BENCH + Golden Set
   - `metrics.rs` — Recall@K, MRR, Precision/Recall/F1
   - `runner.rs` — Pluggable retriever evaluation
   - `report.rs` — JSON report + ASCII summary generator

   **CLI**: `cargo run -p hydragent-bench --bin bench -- --skill-bench --golden-set --output reports/`

   **Data**:
   - `tests/bench/skill_bench_v1.jsonl` — 80 tasks (10 skills × 8 paraphrases)
   - `tests/bench/golden_set_v1.jsonl` — 30 pairs (single/dual/triple relevance)

   ---

   ## 14. Edge Binary (Stubbed)

   **Directory**: `edge/`

   - `build.zig` — Zig build script
   - `build.zig.zon` — Zig package manifest
   - `src/main.zig` — Stub source

   **Status**: Workspace present but **not compiling or running**. Phase 8 (RISC-V / ESP32-S3) not started.

   ---

   ## 15. Configuration Files

   | File | Purpose |
   |------|---------|
   | `.env` / `.env.example` | LLM credentials, channel tokens, tuning params |
   | `config/SOUL.md` | Agent personality, values, behavioral guidelines |
   | `config/USER.md` | User profile, preferences, memory seed |
   | `config/model_council.yaml` | 20+ model profiles for Model Council routing |
   | `config/security/*.yaml.example` | Taint sinks, policy, injection patterns (Phase 6) |

   ---

   ## 16. Test Coverage

   | Surface | Tests | Status |
   |---------|-------|--------|
   | `hydragent-core` (incl. config + markdown_render) | 72+ | ✅ |
   | `hydragent-core/config.rs` | 20 | ✅ |
   | `hydragent-core/markdown_render.rs` | 17 | ✅ |
   | `hydragent-memory` (Phase 2) | 21 | ✅ |
   | `hydragent-vault` (Phase 3 + 6.4) | 79 | ✅ |
   | `hydragent-swarm` (Phase 5, Week 20) | 35 | ✅ |
   | `hydragent-model` (Phase 5, Week 20) | 30 | ✅ |
   | `hydragent-planner` (Phase 5, Week 19) | 19 | ✅ |
   | `hydragent-security` (Phase 6) | ~38 | ✅ (builds clean) |
   | `hydragent-skills` (Phase 7) | 52 | ✅ |
   | `hydragent-bench` (Phase 7) | 30 | ✅ |
   | `hydragent-core/skill_induction.rs` (Dreaming) | 4 | ✅ |
   | **Total estimated** | **~400+** | |

   ---

   ## 17. Known Gaps & Open Questions

   1. **Docker sandbox**: Not implemented. Code sandbox is Wasmtime-only.
   2. **Vector store**: Linear scan (not HNSW). ChromaDB is documented but unused.
   3. **`session_id` vs `page_id`**: Docs drift; code uses `page_id` exclusively.
   4. **Security wiring**: `hydragent-security` modules exist but are not invoked in the core ReAct loop (tools are registered but
 underlying subsystems are stubs in practice).
   5. **SQLCipher**: Deferred post-MVP.
   6. **Edge binary**: Not compiling.
   7. **Standing orders location**: Lives in `hydragent-tools` but conceptually belongs in memory.
   8. **Vector store backend choice**: HNSW vs ChromaDB — needs decision.
   9. **Phase 5 Tracks 5.3–5.4**: DagExecutionEngine + self-healing replanner not started.
   10. **WhatsApp/Signal/Matrix adapters**: Spec'd but not built.

   ---

   ## 18. File Inventory

   | Directory | Files | Description |
   |-----------|-------|-------------|
   | `crates/` | 16 Rust crates | Core workspace |
   | `adapters/` | ~15 Python files | Channel adapters + SDK |
   | `tests/` | ~12 test files | Smoke tests, e2e, bench data |
   | `skills/builtin/` | 3 YAML files | Shipped skills |
   | `tools/finetune/` | 3 Python files | LoRA fine-tuning pipeline |
   | `config/` | 4+ YAML/Markdown | Council, SOUL, USER, security |
   | `doc/` | 15+ Markdown | Architecture, roadmap, phases, R&D |
   | `migrations/` | 1 SQL file | Skill library schema |
   | `data/` | Runtime data | SQLite DBs, vault, models, logs |
   | `edge/` | 3 Zig files | Stubbed edge binary |
   | `sandbox/` | 2 WASM examples | `echo.wasm`, `file_read.wasm` |

   ---

   ## 19. Quick Start Commands

   ```bash
   # Build
   cargo build --release

   # Onboard (guided .env wizard)
   ./target/release/hydragent onboard

   # Chat
   ./target/release/hydragent chat

   # Doctor
   ./target/release/hydragent doctor

   # Test brain connectivity
   ./target/release/hydragent test-brain

   # Run all tests
   cargo test --workspace
 ```

 ────────────────────────────────────────────────────────────────────────────────

 20. Documentation Index

 ┌────────────────────────────────────┬────────────────────────────────────────────────────┐
 │ Document                           │ Purpose                                            │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ README.md                          │ Project overview, philosophy, capability matrix    │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ ONBOARDING.md                      │ 10-minute zero-to-first-chat guide                 │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ CONTRIBUTING.md                    │ Code conventions, how to add tools/skills/channels │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ CHANGELOG.md                       │ Version history (Keep-a-Changelog)                 │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ doc/ARCHITECTURE.md                │ Deep technical specification (design vs reality)   │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ doc/STATE.md                       │ ⚡ Ground truth — what is actually in the code     │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ doc/ROADMAP.md                     │ Phased milestones and timeline                     │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ doc/FEATURES.md                    │ Feature matrix & capability catalog                │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ doc/RaD/*.md                       │ Research & Development source materials            │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ doc/phases/PHASE_1.md … PHASE_7.md │ Per-phase implementation retrospectives            │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ ../releases/RELEASE_NOTES_v0.7.0.md   │ v0.7.0 walkthrough                                 │
 ├────────────────────────────────────┼────────────────────────────────────────────────────┤
 │ PHASE_7_COMPLETION_SUMMARY.md      │ Phase 7 completion report                          │
 └────────────────────────────────────┴────────────────────────────────────────────────────┘

 ────────────────────────────────────────────────────────────────────────────────

 21. What Works Right Now

 - ✅ cargo build compiles the full workspace cleanly
 - ✅ hydragent chat runs an interactive terminal REPL
 - ✅ hydragent onboard writes a working .env with provider picker
 - ✅ hydragent test-brain streams responses from live LLM
 - ✅ hydragent doctor runs diagnostic checks
 - ✅ JSON-RPC bus server accepts connections on TCP 5000
 - ✅ Python adapters (Telegram, Discord, Slack, Email) talk to the bus
 - ✅ SQLite-backed memory with hybrid BM25 + vector retrieval
 - ✅ 12+ tools registered in ReAct loop
 - ✅ Model Council routes across 20+ profiles
 - ✅ Encrypted vault with key rotation
 - ✅ Skill library with FTS5 + 7-day Curator
 - ✅ SKILL-BENCH (80 tasks) + Golden Set (30 pairs)
 - ✅ WASM sandbox with fuel metering