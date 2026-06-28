# Hydragent: Feature Matrix, Current State & Roadmap

> A comprehensive catalog of the **Hydragent Unified AI Agent** capabilities, its current implementation state (as of v0.7.1, June 2026), and the development roadmap.

---

## Table of Contents

1. [Capability Catalog](#1-capability-catalog)
2. [Current Implementation State (v0.7.1)](#2-current-implementation-state-v071)
3. [Development Roadmap & Phased Plan](#3-development-roadmap--phased-plan)
4. [Telegram Native Features Roadmap](#4-telegram-native-features-roadmap)

---

## 1. Capability Catalog

Hydragent's features are distilled from leading agent architectures (memU, Vellum, OpenClaw, QwenPaw, and Claude Code).

### 1.1 Memory & Personalization
- **Episodic Memory**: Timestamped conversation logs and daily activity journals stored in SQLite.
- **Semantic Memory**: Fact-nodes about the world, indexed using local `all-MiniLM-L6-v2` embeddings and SQLite FTS5, fused via Reciprocal Rank Fusion (RRF).
- **Procedural Memory (Skills)**: Auto-inducted and curated skill templates (`skills/builtin/`).
- **Declarative Memory (Bounded Hot Memory)**: Core identity (`SOUL.md`, 12K character budget) and user preferences (`USER.md`, 6K character budget) with LLM-driven re-synthesis compaction.
- **Library Knowledge Graph**: Hierarchical graph containing Shelves, Books, Pages, and Desks. Accessible via Event Bus RPCs and visualized in a D3.js force-directed graph.

### 1.2 Security & Governance
- **Cryptographic Vault**: XChaCha20-Poly1305 + Argon2id encrypted credentials store. Features `mlock`-pinned memory buffers to prevent swapping to disk and an AES-256-GCM column cipher.
- **Model-Blind Keys**: Credentials are injected directly into network sockets at the kernel boundary; the LLM never sees its own API keys.
- **Dual-Slot Authentication**: Supports unlocking the vault via a user-memorable Passphrase PIN (PBKDF2/SHA256) or a local physical Admin Key File.
- **Merkle Audit Log**: Cryptographically verifies the integrity of all executed actions.
- **Taint Tracking & Injection Guard**: Dynamic taint-checking on user inputs to block prompt injection and credential leakage.

### 1.3 Execution Sandbox
- **WASM Sandbox (Wasmtime)**: CPU instruction-metered and memory-limited sandbox for executing custom scripts and tools without host filesystem/socket access.
- **Docker Container Sandbox**: Ephemeral containerized environments for full code execution and browser automation (Playwright).
- **MCP Server Connection**: Native integration with Model Context Protocol (MCP) servers (Notion, Linear, Postgres, GitHub, etc.).

---

## 2. Current Implementation State (v0.7.1)

### 2.1 Workspace Crates
| Crate | Purpose | Status |
|---|---|---|
| `hydragent-core` | Orchestrator, ReAct loop, CLI REPL, audit log | **Live** |
| `hydragent-types` | Shared event and data structures | **Live** |
| `hydragent-bus` | TCP event bus + protocol (`PROTOCOL.md`) | **Live** |
| `hydragent-model` | LLM providers (OpenRouter, local Ollama) | **Live** |
| `hydragent-tools` | Tool registry and built-in tools | **Live** |
| `hydragent-memory` | SQLite session store, vector index, context injector | **Live** |
| `hydragent-embed` | Local embedding provider (Candle + MiniLM) | **Live** |
| `hydragent-vault` | Encrypted secrets store with key rotation | **Live** |
| `hydragent-sandbox` | Sandboxed execution (Wasmtime + Docker) | **Live** |
| `hydragent-gateway` | Multi-channel adapter hosting process | **Live** |
| `hydragent-scheduler` | Cron scheduler + heartbeat engine | **Live** |
| `hydragent-planner` | DAG planning and task decomposition | **Live** |
| `hydragent-swarm` | Subagent spawner, coordinator, Model Council routing | **Live** |
| `hydragent-skills` | Skill library, Hermes extractor, 7-day curator, composer | **Live** |
| `hydragent-security` | Merkle chain, taint tracker, injection guard | **Live** |
| `hydragent-bench` | SKILL-BENCH and Golden Set benchmark runner | **Live** |

### 2.2 Active Channel Adapters
- **CLI REPL**: The default console interface (both Rust kernel-level and Python Rich-based REPL).
- **Telegram**: Real bot integration with inline keyboards and Mini App support.
- **Discord**: Slash commands and embeds.
- **Slack**: Bolt-style event handler.
- **Email**: IMAP/SMTP inbox watcher and sender.
- **Webhook**: Generic inbound HTTP triggers.

### 2.3 Registered Tools
1. `echo`: Sanity check tool.
2. `web_search`: Web search.
3. `file_read`: Host filesystem read.
4. `memory_store`: Persist semantic facts.
5. `memory_search`: Hybrid BM25 + vector search.
6. `memory_forget`: Delete semantic facts.
7. `standing_orders`: Read/write persistent rules.
8. `user_profile`: `USER.md` accessor.
9. `send_message`: Channel-agnostic outbound message.
10. `schedule_task`: Cron job registration.
11. `rss_subscribe`: RSS feed poller.

---

## 3. Development Roadmap & Phased Plan

### v0.7.1: Polishing & SDK Integration (Current)
- **Codenamed**: *Hydra Shine*
- **Scope**: Clean up the REPL output, implement log routing, finalize the `hydragent_py` SDK, wire the `skill_*` tools into the swarm, and consolidate project documentation.
- **Timeline**: Shipped (June 2026).

### v0.8.0: Edge Hardware & Local Inference (Next)
- **Codenamed**: *Hydra Edge*
- **Scope**:
  - Compile the Zig edge binary (`hydragent-edge` $\le 678$ KB).
  - Integrate PicoLM 4-bit GGUF for local, offline inference.
  - Deploy to ESP32-S3 and RISC-V development boards.
  - Implement an offline skill subset.
  - Wire up the MQTT adapter for IoT/smart-home integrations.
  - Implement Over-The-Air (OTA) skill updates.
- **Timeline**: 3–4 weeks.

### v0.9.0: Enterprise Features & Scaling
- **Codenamed**: *Hydra Enterprise*
- **Scope**:
  - Multi-tenant workspace isolation.
  - Role-Based Access Control (RBAC) for tool execution.
  - Standardized HaluMem benchmark suite.
  - SOC 2 compliance controls and encrypted log exports.
  - Public GitHub release and launch of Hydra Hub (the skill marketplace).
- **Timeline**: 4–6 weeks.

---

## 4. Telegram Native Features Roadmap

To push the Telegram adapter from a text-only chat agent to a rich Telegram-native experience, the following phased enhancements are planned:

- **Phase 0 (Foundation)**: *[COMPLETE]* JSON-friendly structured logging, token redaction filters, `/health` and `/ready` endpoints, `Application` lifecycle hooks, 300s permission request timeouts, and WebSocket dead-session cleanup.
- **Phase 1 (Multi-modal)**: Support receiving voice notes (processed via Whisper), photos/documents (processed via VLMs), and location pins (updating Spatial memory).
- **Phase 2 (Reactions)**: Read chat reactions to dynamically adjust the agent's tone or trigger message rollbacks.
- **Phase 3 (Inline Mode)**: Allow users to invoke Hydragent inside other chats by typing `@botname [query]`.
- **Phase 4 (Groups & Forums)**: Support Telegram Forum topics (`is_topic_message` routing) and group moderation commands.
- **Phase 5 (Mini App v2)**: Re-write the Mini App with Telegram SDK initialization, dark mode matching, local storage caching, and real-time WebSocket state synchronization.
- **Phase 6 (Webhooks)**: Transition from long polling to webhook mode for production scale.
