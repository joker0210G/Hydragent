# Phase 4: 40+ Channel Gateway, Proactive Heartbeat & Work IQ (Weeks 15–18)

> **Timeline**: Weeks 15–18
> **Theme**: Hydragent escapes the terminal. A **unified channel gateway** connects Telegram, Discord, WhatsApp, Slack, Signal, email, webhooks, and the CLI through a single routing bus. A **proactive heartbeat daemon** lets the agent push notifications, reminders, and ambient intelligence to the user unprompted. A **cron scheduler** enables recurring autonomous tasks. **Work IQ** is a persistent background awareness layer that monitors feeds, watches for anomalies, and surfaces actionable insights without being asked.

> ## ✅ Implementation Status — Core Complete (Weeks 15–18, as of June 2026)
> 
> Cross-checked against [`doc/STATE.md`](../STATE.md) at `git rev 3d99366` (June 2026).
> 
> **What is live:**
> - **Six messaging adapters** are live in `adapters/`: `cli_adapter.py`, `telegram_adapter.py`, `discord_adapter.py`, `slack_adapter.py`, `email_adapter.py`, `webhook_adapter.py`. The `bus_client.py`, `formatter.py`, `test_connection.py`, and a D3-based `miniapp/` (graph + glassmorphism UI) ship alongside.
> - **`hydragent-gateway`** (`channel_trait`, `router`, `dedup`, `rate_limiter`) is the hosting process for all adapters. `GatewayRouter::inbound_check` runs dedup + rate limiting, `outbound` and `push` dispatch to the right adapter.
> - **`hydragent-scheduler`** ships three engines:
>   - `heartbeat.rs` — proactive push to any channel via `GatewayRouter::push`.
>   - `cron_scheduler.rs` — `tokio-cron-scheduler` wrapper with SQLite persistence; **reloads active jobs on startup** via `reload_from_db()`.
>   - `work_iq.rs` — `WorkIqEngine` with `add_feed()`, `run_poll_cycle()`, RSS parsing via `feed-rs`, and keyword alert dispatch through the heartbeat engine.
> - **Three Phase-4 tools** are live: `send_message`, `schedule_task`, `rss_subscribe`.
> 
> **What is missing / not yet implemented:**
> - **Cron `task_type` is restricted** to `react_loop` and `message`. `heartbeat` and `work_iq_poll` are reserved strings but not currently emitted as cron task types; those run on their own dedicated loops, not through the cron executor.
> - **Out of the 40+ channels listed in the phase title**, only 6 are implemented. **Not implemented:** WhatsApp, Signal, Matrix, iMessage, Microsoft Teams, Lark, DingTalk, WeChat, QQ.
> - **Voice (Whisper STT / Coqui TTS), WebSocket-based web chat, and OAuth-broked IMAP/SMTP** are also not implemented.
> - **Work IQ is implemented at the engine level** (polling, digest persistence, alert push). The channel adapters for digest delivery rely on the standard `heartbeat.push` path; the digest *synthesis* step (LLM summarization of collected entries) is not yet wired.
> 
> **Definition of done coverage:** hard goals G1, G3, G4, G6, G7, G9 are met. G2 (Discord slash + DM), G5 (email IMAP/SMTP roundtrip), G8 (Work IQ ≥ 1 RSS feed with digest to Telegram) are partially met — the engines exist but end-to-end e2e tests against live platform APIs are not in the tree.

---

## 📋 Table of Contents

1. [Phase Goals & Exit Criteria](#1-phase-goals--exit-criteria)
2. [Directory & Workspace Layout Changes](#2-directory--workspace-layout-changes)
3. [Technology Decisions](#3-technology-decisions)
4. [Week-by-Week Breakdown](#4-week-by-week-breakdown)
5. [Component Specifications](#5-component-specifications)
   - 5.1 [Unified Channel Gateway Architecture](#51-unified-channel-gateway-architecture)
   - 5.2 [ChannelAdapter Trait & Router](#52-channeladapter-trait--router)
   - 5.3 [Telegram Adapter](#53-telegram-adapter)
   - 5.4 [Discord Adapter](#54-discord-adapter)
   - 5.5 [Slack Adapter](#55-slack-adapter)
   - 5.6 [Email Adapter (IMAP/SMTP)](#56-email-adapter-imapsmtp)
   - 5.7 [Webhook Adapter (Inbound HTTP)](#57-webhook-adapter-inbound-http)
   - 5.8 [Proactive Heartbeat & Push Notification Engine](#58-proactive-heartbeat--push-notification-engine)
   - 5.9 [Cron Scheduler (Autonomous Task Engine)](#59-cron-scheduler-autonomous-task-engine)
   - 5.10 [Work IQ — Background Awareness Layer](#510-work-iq--background-awareness-layer)
6. [Built-in Tools (Phase 4 Additions)](#6-built-in-tools-phase-4-additions)
7. [Configuration & Environment](#7-configuration--environment)
8. [Testing Strategy](#8-testing-strategy)
9. [Performance Targets](#9-performance-targets)
10. [Risks & Mitigations](#10-risks--mitigations)
11. [Definition of Done](#11-definition-of-done)

---

## 1. Phase Goals & Exit Criteria

Phase 4 is about **reach and proactivity**. Hydragent must live where the user already is — their Telegram, their Discord, their email — and it must be able to initiate contact, not just respond. Inspired by Microsoft Scout's Work IQ background intelligence, OpenClaw's 12+ channel adapters, and ZeroClaw's proactive heartbeat daemon.

### Hard Goals (must achieve before Phase 5)

| # | Goal | Validation |
|---|---|---|
| G1 | Telegram adapter receives user messages and delivers streamed agent responses | Integration test: message → Telegram bot API mock → response delivered with edit-in-place streaming |
| G2 | Discord adapter handles slash commands and DMs | Integration test: `/ask` slash command → agent response in same thread |
| G3 | Slack adapter handles `@Hydra` app mentions in channels | Integration test: `@Hydra what is the status?` → agent reply in thread |
| G4 | Webhook adapter accepts inbound HTTP POST and routes to orchestrator | `curl -X POST http://localhost:8080/webhook -d '{"content":"ping"}'` → agent response in webhook reply body |
| G5 | Email adapter polls IMAP, routes to orchestrator, sends SMTP reply | Integration test: mock IMAP server delivers email → agent replies via SMTP mock |
| G6 | Proactive heartbeat can push a message to *any* registered channel at agent initiative | Unit test: `heartbeat.push("telegram", session_id, "Your build just finished!")` delivers message |
| G7 | Cron scheduler executes a registered task on a `0 9 * * *` cron expression | Integration test: cron fires at T+5s (accelerated clock), executes `web_search` tool, stores result |
| G8 | Work IQ monitors ≥ 1 RSS feed; surfaces a digest to user via Telegram on schedule | End-to-end: feed URL configured → digest generated and pushed at configured interval |
| G9 | All Phase 1–3 tests remain green (no regressions) | `cargo test --workspace` and `pytest adapters/` both exit 0 |

### Soft Goals (target but not blocking)

- WhatsApp adapter (via Twilio API or WhatsApp Cloud API) — delivered as community contribution template
- Signal adapter (via `signal-cli`) — documented but not CI-tested (requires phone number)
- `./hydragent channels list` CLI shows all active adapters and their connection state
- Per-channel rate limiting to respect platform API quotas
- Message deduplication — the same user message routed through two channels doesn't fire the orchestrator twice
- Rich formatting: Telegram supports MarkdownV2; Discord supports embeds; adapters translate `AgentResponse` into platform-native format

---

## 2. Directory & Workspace Layout Changes

Phase 4 introduces `crates/hydragent-gateway` (the channel router) and `crates/hydragent-scheduler` (cron engine + heartbeat), plus Python adapter implementations for each platform.

```
hydragent/
│
├── crates/
│   │
│   ├── hydragent-core/                        # UPDATED
│   │   └── src/
│   │       ├── main.rs                        # UPDATED: gateway init, scheduler spawn, Work IQ spawn
│   │       ├── orchestrator.rs               # UPDATED: handles multi-channel AgentResponse routing
│   │       └── work_iq.rs                    # NEW: background awareness engine
│   │
│   ├── hydragent-gateway/                    # NEW CRATE: unified channel router
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── channel_trait.rs              # ChannelAdapter trait definition
│   │       ├── router.rs                     # GatewayRouter: routes IntentEvents to orchestrator
│   │       ├── channel_registry.rs           # Registry of all active adapters
│   │       ├── dedup.rs                      # Message deduplication (SHA-256 content hash + LRU cache)
│   │       ├── rate_limiter.rs              # Per-channel rate limiting (token bucket)
│   │       └── formatter.rs                 # AgentResponse → per-platform formatted message
│   │
│   ├── hydragent-scheduler/                  # NEW CRATE: cron + heartbeat
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── cron_scheduler.rs            # Tokio-cron-scheduler wrapper; job registry
│   │       ├── heartbeat.rs                 # HeartbeatEngine: push to any channel at agent initiative
│   │       ├── job_store.rs                 # SQLite-backed job persistence
│   │       └── work_iq_monitor.rs           # RSS/Atom feed poller + anomaly trigger
│   │
│   ├── hydragent-types/                      # UPDATED
│   │   └── src/
│   │       └── lib.rs                        # UPDATED: ChannelCapabilities, PushMessage, CronJob types
│   │
│   └── hydragent-tools/                      # UPDATED
│       └── src/
│           ├── schedule_task.rs             # NEW tool: schedule_task (create cron jobs via ReAct)
│           ├── send_message.rs              # NEW tool: send_message (push to a channel)
│           └── rss_subscribe.rs             # NEW tool: rss_subscribe (add Work IQ feed)
│
├── adapters/                                 # UPDATED: new platform adapters
│   ├── telegram_adapter.py                  # NEW: telethon/python-telegram-bot
│   ├── discord_adapter.py                   # NEW: discord.py
│   ├── slack_adapter.py                     # NEW: slack-sdk
│   ├── email_adapter.py                     # NEW: imaplib + smtplib
│   ├── webhook_adapter.py                   # NEW: FastAPI inbound webhook server
│   ├── bus_client.py                        # EXISTING: updated for push message support
│   └── formatter.py                         # UPDATED: per-platform markdown formatting
│
├── config/
│   ├── channels/
│   │   ├── telegram.yaml                    # NEW: Telegram bot token, allowed chat IDs
│   │   ├── discord.yaml                     # NEW: Discord bot token, guild IDs, command prefix
│   │   ├── slack.yaml                       # NEW: Slack app token, signing secret
│   │   ├── email.yaml                       # NEW: IMAP/SMTP credentials, polling interval
│   │   └── webhook.yaml                     # NEW: webhook port, secret header
│   └── work_iq/
│       ├── feeds.yaml                       # NEW: RSS/Atom feed URLs and digest schedules
│       └── monitors.yaml                    # NEW: keyword watches, anomaly thresholds
│
├── data/
│   ├── sessions/                            # Existing
│   ├── models/                              # Phase 2 embedding models
│   └── scheduler.db                         # NEW: SQLite for cron job definitions and history
│
└── tests/
    ├── unit/
    │   ├── dedup_test.rs                   # NEW: message deduplication
    │   ├── rate_limiter_test.rs             # NEW: token bucket behavior
    │   ├── cron_scheduler_test.rs           # NEW: job parsing, execution timing
    │   └── formatter_test.rs               # NEW: AgentResponse → Telegram MarkdownV2
    └── integration/
        ├── telegram_e2e_test.py            # NEW: mock Telegram Bot API
        ├── discord_e2e_test.py             # NEW: mock Discord gateway
        ├── webhook_e2e_test.py             # NEW: HTTP round-trip
        └── work_iq_test.rs                 # NEW: RSS poll → digest → push
```

---

## 3. Technology Decisions

> **Team consensus**: channel adapters stay in Python (rich ecosystem of bot libraries); the gateway router, scheduler, and Work IQ engine are in Rust (performance, concurrency). All adapters communicate with the Rust core over the existing JSON-RPC event bus.

---

### 3.1 Language Roles in Phase 4

| Component | Language | Rationale |
|---|---|---|
| Channel router, dedup, rate limiter | **Rust** | Handles all inter-adapter message routing; must be sub-millisecond |
| Cron scheduler | **Rust** | Uses `tokio-cron-scheduler`; Tokio-native, no blocking |
| Heartbeat push engine | **Rust** | Must reliably deliver pushes across all adapters from a single event |
| Work IQ feed monitor | **Rust** | RSS/Atom parsing + anomaly detection; deterministic, no ML needed |
| Telegram adapter | **Python** | `python-telegram-bot` v21 — most complete Telegram Bot API wrapper |
| Discord adapter | **Python** | `discord.py` — mature, slash command support, embed formatting |
| Slack adapter | **Python** | `slack-sdk` — official Slack SDK with Socket Mode support |
| Email adapter | **Python** | `imaplib` + `smtplib` — standard library, no extra dependencies |
| Webhook adapter | **Python** | `FastAPI` + `uvicorn` — production-grade async HTTP server in 30 lines |

---

### 3.2 Why Python for Channel Adapters (and not Rust)?

| Factor | Python Adapters | Rust Adapters |
|---|---|---|
| **Bot library maturity** | `python-telegram-bot` v21, `discord.py` 2.x — battle-tested, maintained by dedicated teams | `teloxide` for Telegram exists but is less feature-complete; no mature Discord library |
| **Development speed** | New adapter in ~100 lines; hot-reload during dev | New Rust adapter requires crate + trait impl + full recompile cycle |
| **Platform API changes** | Python libraries absorb API changes behind SDK; we just update the package | Rust requires manual API implementation for every endpoint |
| **Latency sensitivity** | Adapters are I/O bound (network calls to platform APIs) — Python's GIL is irrelevant | Rust's advantage (CPU-bound performance) doesn't apply here |
| **Hard constraint** | Python is only used in adapters — never in the security vault or orchestrator | — |

---

### 3.3 Telegram Adapter: Library Choice

| Library | Stars | Approach | Choice |
|---|---|---|---|
| `python-telegram-bot` v21 | 26k ⭐ | Async PTB; full Bot API coverage; built-in rate limiting | ✅ **Chosen** |
| `aiogram` v3 | 10k ⭐ | Async, FSM-first; slightly less documentation | ❌ Overkill for our use case |
| `telebot` | 8k ⭐ | Sync wrapper; doesn't fit our async architecture | ❌ Rejected |

---

### 3.4 Discord Adapter: Library Choice

| Library | Stars | Approach | Choice |
|---|---|---|---|
| `discord.py` v2 | 14k ⭐ | Async; slash commands via `app_commands`; maintained | ✅ **Chosen** |
| `nextcord` | 1.5k ⭐ | Fork of discord.py; less community support | ❌ Rejected |
| `hikari` | 1.7k ⭐ | Lower-level; requires more boilerplate for slash commands | ❌ Rejected |

---

### 3.5 Cron Engine: `tokio-cron-scheduler`

| Factor | Rationale |
|---|---|
| **Tokio-native** | Jobs run as Tokio tasks — no blocking thread pools; integrates naturally with our runtime |
| **Cron expression parsing** | Full POSIX cron + seconds-level precision for sub-minute tasks |
| **Job metadata** | Each job has a UUID; jobs can be removed at runtime by ID |
| **Persistence** | We layer our own SQLite persistence on top for crash recovery |

---

### 3.6 Work IQ Design Philosophy

Work IQ is inspired by **Microsoft Scout's "Work IQ always-on background intelligence layer"**. The core insight: most agent systems only respond to explicit queries. Scout proactively monitors the user's context and surfaces insights without being asked.

Work IQ monitors:
1. **RSS/Atom feeds** — tech news, project dependencies, security advisories
2. **Scheduled digests** — summarize the last 24h of monitored feeds
3. **Keyword watches** — alert when a specific term appears in monitored feeds
4. **Anomaly triggers** — alert when a monitor's normal pattern breaks (e.g., no commits in 3 days)

---

## 4. Week-by-Week Breakdown

### Week 15 — Channel Gateway Foundation & Telegram

**Goal**: The core gateway is wired up. Telegram works end-to-end. Messages from Telegram arrive at the orchestrator and responses are delivered back.

| Day | Task |
|---|---|
| Mon | Create `crates/hydragent-gateway` crate. Define `ChannelAdapter` trait in `channel_trait.rs`. Define `PushMessage` and `ChannelCapabilities` types in `hydragent-types`. Wire `GatewayRouter` into `main.rs` alongside existing `EventBus`. |
| Tue | Implement `ChannelRegistry` — a `HashMap<String, Arc<dyn ChannelAdapterBridge>>` where each entry is a connected adapter identified by `channel_id`. `GatewayRouter::broadcast(push_msg)` fans out to the matching adapter's `send()` method. |
| Wed | Implement `dedup.rs`: 256-byte SHA-256 hash of `(channel_id, user_id, content)` stored in an LRU cache (capacity 1,000). If hash seen within last 30 s, drop duplicate. |
| Thu | Implement `adapters/telegram_adapter.py`. Use `python-telegram-bot` PTB v21. On `message` event: wrap as `IntentEvent` JSON-RPC → send to Rust bus. On response stream: use `bot.edit_message_text()` to update the reply in-place as tokens arrive. |
| Fri | Implement `config/channels/telegram.yaml` loader in Rust. Validate bot token on startup via `getMe` API call. Log `channel_id=telegram:{chat_id}` in `tracing`. |
| Sat | Integration test: start Rust core + Telegram adapter against mock Telegram Bot API (WireMock). Send message → assert response delivered. Test Markdown → MarkdownV2 escaping. |
| Sun | `rate_limiter.rs`: token bucket per channel. Telegram = 30 messages/s global, 1 msg/s per chat. Discord = 5 req/s global. Configurable in `channels/*.yaml`. |

**Deliverable**: End-to-end Telegram conversation working locally. `cargo test` gateway unit tests green.

---

### Week 16 — Discord, Slack, Webhook & Email Adapters

**Goal**: All major adapters are operational. A unified gateway handles all four channels simultaneously.

| Day | Task |
|---|---|
| Mon | Implement `adapters/discord_adapter.py`. Register `/ask <query>` slash command. On invocation: defer response (Discord requires acknowledgment within 3 s), send `IntentEvent` to bus, stream response via `interaction.edit_original_response()`. |
| Tue | Implement `adapters/slack_adapter.py` with Socket Mode (no public URL needed for dev). Handle `app_mention` events. Use `client.chat_postMessage()` for response; update with `client.chat_update()` for streaming. |
| Wed | Implement `adapters/email_adapter.py`. Poll IMAP every `EMAIL_POLL_INTERVAL_SEC` seconds. For each unread email: parse sender, subject, body → `IntentEvent`. Send SMTP reply with agent response. Mark as read after processing. |
| Thu | Implement `adapters/webhook_adapter.py`. FastAPI HTTP POST endpoint at `POST /webhook`. Validate `X-Hydragent-Secret` header. Route body to bus as `IntentEvent`. Return agent response as JSON in HTTP reply. |
| Fri | Test all four adapters in parallel using Python `multiprocessing`. Verify all four can receive a message simultaneously and the orchestrator handles concurrent sessions correctly. |
| Sat | Implement `formatter.py` platform-specific rendering: Telegram (MarkdownV2 escape), Discord (embed fields for tool use), Slack (Block Kit `mrkdwn`), Email (plain text + HTML multipart). |
| Sun | `./hydragent channels list` CLI: queries `ChannelRegistry` state via JSON-RPC → prints table of adapter name, status (connected/disconnected), last message timestamp. |

**Deliverable**: All four adapters connect and handle a round-trip conversation. Multi-channel simultaneous test passes.

---

### Week 17 — Proactive Heartbeat & Cron Scheduler

**Goal**: The agent can push messages at its own initiative. Cron jobs execute autonomously.

| Day | Task |
|---|---|
| Mon | Implement `crates/hydragent-scheduler/src/heartbeat.rs`: `HeartbeatEngine::push(channel_id, session_id, content)`. Routes to `GatewayRouter::broadcast()`. Also broadcasts to all registered adapters if `channel_id = "*"`. |
| Tue | Implement `cron_scheduler.rs`. Wrap `tokio_cron_scheduler::JobScheduler`. Each `CronJob` has: `id (UUID)`, `cron_expr (String)`, `task (Box<dyn Fn() -> BoxFuture<'static, ()> + Send>)`. `add_job()` parses cron expression; `remove_job()` by UUID. |
| Wed | Implement `job_store.rs` — SQLite table `cron_jobs` for persistence. On startup: load all `status='active'` jobs and re-register them with `tokio-cron-scheduler`. On graceful shutdown: ensure no jobs are dropped mid-execution. |
| Thu | Implement the `schedule_task` tool (for the ReAct loop). The agent can call `schedule_task` with a cron expression + natural language task description. The tool: (1) parses the cron via `cron` crate validation, (2) inserts into `cron_jobs` table, (3) registers with live scheduler. |
| Fri | Test: trigger a cron job with `* * * * * *` (every second). Assert it fires exactly 5 times in 5 seconds. Assert job survives a process restart (reloaded from SQLite). Assert removing the job stops firing. |
| Sat | Implement `send_message` tool: the agent can push messages to any channel during a ReAct loop (e.g., "Send a Telegram notification to the user that the deployment is complete"). Tier: `auto_approve`. |
| Sun | Full integration test: agent scheduled to "check Hacker News top 10 every day at 9 AM" → fires → `web_search` executed → result stored in memory → heartbeat pushes digest to Telegram. |

**Deliverable**: `schedule_task` tool live. Cron jobs survive restarts. Heartbeat delivers messages to Telegram.

---

### Week 18 — Work IQ, Polish & Phase 4 Release

**Goal**: Work IQ is monitoring RSS feeds. Phase 4 is hardened, benchmarked, and tagged.

| Day | Task |
|---|---|
| Mon | Implement `work_iq.rs`: `WorkIqEngine::add_feed(url, schedule, keywords)`. Uses `tokio::time::interval` to poll feeds at configured frequency. Parse RSS/Atom via `feed-rs` crate. Extract new entries since last poll. |
| Tue | Implement Work IQ digest generation: collect all new entries across monitored feeds → build a structured prompt → call `llm.generate()` → format as bullet digest. Store digest as semantic memory (Phase 2 integration). |
| Wed | Implement keyword watches in `monitors.yaml`. If any new feed entry matches a keyword, immediately push a heartbeat alert to the configured channel — no waiting for scheduled digest. |
| Thu | Implement `rss_subscribe` tool: the agent can add a feed to its Work IQ monitor via the ReAct loop. E.g., "Monitor Rust blog for new releases." Persisted to `config/work_iq/feeds.yaml`. |
| Fri | Phase 4 full regression suite: `cargo test --workspace` + `pytest adapters/ -v` + all integration tests. Fix all failures. |
| Sat | Performance profiling: gateway round-trip latency (message in → response out) for each adapter. Update `ARCHITECTURE.md` with new 9-layer diagram. |
| Sun | Tag `v0.4.0`. Write CHANGELOG. Create GitHub Release. Prepare demo screencast (multi-channel conversation + cron job + Work IQ digest). |

**Deliverable**: `v0.4.0` tag. All Phase 4 exit criteria verified.

---

## 5. Component Specifications

### 5.1 Unified Channel Gateway Architecture

The channel gateway is the central routing hub that decouples platform-specific adapters from the core orchestrator. Every message, regardless of origin, is normalized to an `IntentEvent` before entering the orchestrator.

```
┌─────────────────────────────────────────────────────────────────────┐
│                     CHANNEL GATEWAY                                 │
│                                                                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌───────────┐ │
│  │  Telegram   │  │   Discord   │  │    Slack    │  │  Webhook  │ │
│  │  Adapter    │  │   Adapter   │  │   Adapter   │  │  Adapter  │ │
│  │  (Python)   │  │  (Python)   │  │  (Python)   │  │ (Python)  │ │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └─────┬─────┘ │
│         │                │                │                │        │
│         └────────────────┴────────────────┴────────────────┘        │
│                                  │ JSON-RPC over Unix Socket        │
│         ┌────────────────────────▼───────────────────────────┐      │
│         │           GatewayRouter (Rust)                      │      │
│         │  • Deduplication (SHA-256 LRU)                      │      │
│         │  • Rate Limiting (token bucket per channel)         │      │
│         │  • Session ID assignment                            │      │
│         │  • IntentEvent normalization                        │      │
│         └────────────────────────┬───────────────────────────┘      │
│                                  │                                   │
│         ┌────────────────────────▼───────────────────────────┐      │
│         │           Event Bus (Phase 1 JSON-RPC)              │      │
│         └────────────────────────┬───────────────────────────┘      │
│                                  │                                   │
│         ┌────────────────────────▼───────────────────────────┐      │
│         │           Core Orchestrator (Phase 1 ReAct)         │      │
│         └────────────────────────┬───────────────────────────┘      │
│                                  │ AgentResponse                    │
│         ┌────────────────────────▼───────────────────────────┐      │
│         │     ChannelRegistry::route_response()               │      │
│         │  Routes response back to originating adapter        │      │
│         └─────┬──────────┬──────────┬────────────┬───────────┘      │
│               │          │          │            │                   │
│  ┌────────────▼┐  ┌──────▼──────┐ ┌▼──────────┐ ┌▼───────────┐     │
│  │  Telegram   │  │   Discord   │ │  Slack    │ │  Webhook   │     │
│  │  Response   │  │   Embed     │ │  Block    │ │  JSON Body │     │
│  └─────────────┘  └─────────────┘ └───────────┘ └────────────┘     │
└─────────────────────────────────────────────────────────────────────┘
```

---

### 5.2 ChannelAdapter Trait & Router

#### 5.2.1 New Types in `hydragent-types`

```rust
// crates/hydragent-types/src/lib.rs (additions for Phase 4)

/// What a channel can do — not all adapters support all capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelCapabilities {
    /// Can deliver token-by-token streaming (e.g., edit-in-place)
    pub streaming: bool,
    /// Can receive file/image attachments
    pub file_attachments: bool,
    /// Can render Markdown formatting
    pub markdown: bool,
    /// Supports buttons/interactive elements
    pub interactive: bool,
    /// Maximum message length in characters (platform-enforced)
    pub max_message_len: usize,
}

/// A proactive message pushed from agent to user (agent-initiated, not response to query).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushMessage {
    pub channel_id: String,       // e.g., "telegram:123456789"
    pub session_id: String,
    pub content: String,
    pub markdown: bool,
    pub metadata: HashMap<String, String>,
}

/// A scheduled cron job stored in SQLite and registered with the scheduler.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CronJob {
    pub id: String,               // UUID
    pub cron_expr: String,        // e.g., "0 9 * * *"
    pub description: String,      // Human-readable
    pub task_type: String,        // "react_loop" | "heartbeat" | "work_iq_poll"
    pub task_params: String,      // JSON params for the task
    pub target_channel_id: String,// Where to deliver results
    pub status: String,           // "active" | "paused" | "deleted"
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub run_count: i64,
}
```

#### 5.2.2 GatewayRouter

```rust
// crates/hydragent-gateway/src/router.rs

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use hydragent_types::{IntentEvent, AgentResponse, PushMessage};
use crate::{ChannelAdapterBridge, Deduplicator, RateLimiter};

/// The central gateway router.
/// Receives messages from all channel adapters and routes them to the orchestrator.
/// Receives agent responses and routes them back to the originating adapter.
pub struct GatewayRouter {
    /// channel_id → adapter bridge (for routing responses and pushes back)
    adapters: RwLock<HashMap<String, Arc<dyn ChannelAdapterBridge>>>,
    /// Sends normalized IntentEvents to the EventBus
    bus_tx: mpsc::Sender<IntentEvent>,
    /// Deduplicates messages within a 30s window
    dedup: Deduplicator,
    /// Per-channel rate limiters
    rate_limiters: RwLock<HashMap<String, RateLimiter>>,
}

impl GatewayRouter {
    pub fn new(bus_tx: mpsc::Sender<IntentEvent>) -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            bus_tx,
            dedup: Deduplicator::new(1000),
            rate_limiters: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new channel adapter bridge.
    pub fn register_adapter(&self, channel_id: String, adapter: Arc<dyn ChannelAdapterBridge>) {
        tracing::info!(channel_id = %channel_id, "Channel adapter registered");
        self.adapters.write().insert(channel_id.clone(), adapter.clone());
        self.rate_limiters.write().insert(
            channel_id.clone(),
            RateLimiter::default_for_channel(&channel_id),
        );
    }

    /// Called when a channel adapter receives a new user message.
    /// Performs deduplication + rate limiting before forwarding to bus.
    pub async fn inbound(&self, event: IntentEvent) -> anyhow::Result<()> {
        // 1. Deduplication
        if self.dedup.is_duplicate(&event.channel_id, &event.user_id, &event.content) {
            tracing::debug!(
                channel_id = %event.channel_id,
                "Dropped duplicate message"
            );
            return Ok(());
        }

        // 2. Rate limiting
        let allowed = {
            let limiters = self.rate_limiters.read();
            limiters.get(&event.channel_id)
                .map(|rl| rl.try_acquire())
                .unwrap_or(true)
        };

        if !allowed {
            tracing::warn!(
                channel_id = %event.channel_id,
                "Rate limit exceeded — message dropped"
            );
            // Optionally: queue message for delayed processing
            return Ok(());
        }

        // 3. Forward to orchestrator via bus
        tracing::info!(
            channel_id = %event.channel_id,
            session_id = %event.session_id,
            user_id = %event.user_id,
            content_len = event.content.len(),
            "Routing inbound message to orchestrator"
        );

        self.bus_tx.send(event).await
            .map_err(|e| anyhow::anyhow!("Bus channel closed: {}", e))?;

        Ok(())
    }

    /// Route an agent response or push message to the originating adapter.
    pub async fn outbound(&self, channel_id: &str, response: AgentResponse) -> anyhow::Result<()> {
        let adapter = {
            self.adapters.read()
                .get(channel_id)
                .cloned()
        };

        match adapter {
            Some(a) => a.send_response(response).await,
            None => {
                tracing::error!(channel_id, "No adapter registered for channel");
                Ok(())
            }
        }
    }

    /// Push a proactive message to any registered channel.
    pub async fn push(&self, msg: PushMessage) -> anyhow::Result<()> {
        if msg.channel_id == "*" {
            // Broadcast to all channels
            let adapters: Vec<Arc<dyn ChannelAdapterBridge>> = self.adapters.read().values().cloned().collect();
            for adapter in adapters {
                let _ = adapter.send_push(msg.clone()).await;
            }
            return Ok(());
        }

        self.outbound(&msg.channel_id, AgentResponse {
            session_id: msg.session_id,
            channel_id: msg.channel_id.clone(),
            content: msg.content,
            is_complete: true,
            ..Default::default()
        }).await
    }
}
```

---

### 5.3 Telegram Adapter

The Telegram adapter is implemented in Python using `python-telegram-bot` v21 (async).

```python
# adapters/telegram_adapter.py

import asyncio
import uuid
import yaml
from pathlib import Path
from datetime import datetime

from telegram import Update
from telegram.ext import Application, MessageHandler, filters, ContextTypes
from telegram.error import TelegramError

from bus_client import BusClient
from formatter import TelegramFormatter

formatter = TelegramFormatter()

class TelegramAdapter:
    def __init__(self, config_path: str = "config/channels/telegram.yaml"):
        cfg = yaml.safe_load(Path(config_path).read_text())
        self.bot_token = cfg["bot_token"]
        self.allowed_chat_ids = set(cfg.get("allowed_chat_ids", []))
        self.bus = BusClient()
        self.app = Application.builder().token(self.bot_token).build()

    async def on_message(self, update: Update, ctx: ContextTypes.DEFAULT_TYPE):
        """Handle incoming Telegram messages and route to Hydragent core."""
        chat_id = update.effective_chat.id
        user_id = str(update.effective_user.id)
        text = update.message.text or ""

        # Access control
        if self.allowed_chat_ids and chat_id not in self.allowed_chat_ids:
            await update.message.reply_text("⛔ Unauthorized.")
            return

        # Send a placeholder message we will edit in-place during streaming
        placeholder = await update.message.reply_text("⏳ Thinking…")

        # Build the IntentEvent for the bus
        event = {
            "session_id": f"telegram:{chat_id}",
            "channel_id": f"telegram:{chat_id}",
            "user_id":    f"telegram_user:{user_id}",
            "content":    text,
            "attachments": [],
            "metadata":   {"platform": "telegram"},
            "timestamp":  int(datetime.utcnow().timestamp() * 1000),
            "priority":   "normal",
        }

        accumulated_text = ""

        async def on_token(token: str):
            """Called for each streamed token from the orchestrator."""
            nonlocal accumulated_text
            accumulated_text += token
            # Edit-in-place to simulate streaming
            try:
                escaped = formatter.escape_markdown_v2(accumulated_text)
                await ctx.bot.edit_message_text(
                    chat_id=chat_id,
                    message_id=placeholder.message_id,
                    text=escaped,
                    parse_mode="MarkdownV2"
                )
            except TelegramError:
                pass  # Ignore minor edit errors (e.g., message not modified)

        # Stream the response
        final_response = await self.bus.send_intent(event, on_token=on_token)

        # Final render with full Markdown
        try:
            escaped_final = formatter.escape_markdown_v2(final_response)
            await ctx.bot.edit_message_text(
                chat_id=chat_id,
                message_id=placeholder.message_id,
                text=escaped_final,
                parse_mode="MarkdownV2"
            )
        except TelegramError as e:
            # Fallback to plain text if Markdown parsing fails
            await ctx.bot.edit_message_text(
                chat_id=chat_id,
                message_id=placeholder.message_id,
                text=final_response
            )

    async def start(self):
        """Connect to the bus and start polling Telegram."""
        await self.bus.connect()
        self.app.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, self.on_message))

        # Register push handler: bus can push messages to Telegram proactively
        self.bus.register_push_handler("telegram", self._handle_push)

        print(f"✅ Telegram adapter started. Bot: @{(await self.app.bot.get_me()).username}")
        await self.app.run_polling(drop_pending_updates=True)

    async def _handle_push(self, push: dict):
        """Handle proactive push messages from the Rust heartbeat engine."""
        chat_id_str = push.get("channel_id", "").replace("telegram:", "")
        if not chat_id_str.isdigit():
            return
        await self.app.bot.send_message(
            chat_id=int(chat_id_str),
            text=formatter.escape_markdown_v2(push["content"]),
            parse_mode="MarkdownV2"
        )

if __name__ == "__main__":
    adapter = TelegramAdapter()
    asyncio.run(adapter.start())
```

**Telegram Formatting Rules**:

| Content Type | Telegram MarkdownV2 Rendering |
|---|---|
| `**bold**` | `*bold*` |
| `_italic_` | `_italic_` |
| `` `code` `` | `` `code` `` |
| ` ```code block``` ` | ` ```code block``` ` |
| `[link](url)` | `[link](url)` |
| `[Using tool: X]` | `_🔧 Using tool: X_` |
| Agent response prefix | No prefix — edit-in-place replaces placeholder |
| Escape chars | `.`, `!`, `-`, `(`, `)`, `[`, `]`, `{`, `}`, `>`, `#`, `+`, `-`, `=`, `\|` all escaped with `\` |

---

### 5.4 Discord Adapter

```python
# adapters/discord_adapter.py

import asyncio
import discord
from discord import app_commands
from discord.ext import commands
import yaml
from pathlib import Path
from bus_client import BusClient

class HydraDiscordClient(discord.Client):
    def __init__(self, config_path: str = "config/channels/discord.yaml"):
        cfg = yaml.safe_load(Path(config_path).read_text())
        intents = discord.Intents.default()
        intents.message_content = True
        super().__init__(intents=intents)

        self.tree = app_commands.CommandTree(self)
        self.token = cfg["bot_token"]
        self.guild_ids = [discord.Object(id=g) for g in cfg.get("guild_ids", [])]
        self.bus = BusClient()

    async def setup_hook(self):
        """Register slash commands."""
        @self.tree.command(name="ask", description="Ask Hydragent a question")
        async def ask_command(interaction: discord.Interaction, query: str):
            await interaction.response.defer(thinking=True)

            session_id = f"discord:{interaction.channel_id}"
            event = {
                "session_id": session_id,
                "channel_id": f"discord:{interaction.channel_id}",
                "user_id":    f"discord_user:{interaction.user.id}",
                "content":    query,
                "attachments": [],
                "metadata":   {"platform": "discord", "guild_id": str(interaction.guild_id)},
                "timestamp":  int(discord.utils.utcnow().timestamp() * 1000),
                "priority":   "normal",
            }

            accumulated = ""
            last_edit_len = 0

            async def on_token(token: str):
                nonlocal accumulated, last_edit_len
                accumulated += token
                # Edit every 50 new characters to avoid rate limits
                if len(accumulated) - last_edit_len >= 50:
                    await interaction.edit_original_response(content=f"💬 {accumulated}…")
                    last_edit_len = len(accumulated)

            final = await self.bus.send_intent(event, on_token=on_token)

            # Deliver final as embed for rich formatting
            embed = discord.Embed(description=final[:4096], color=0x5865F2)
            embed.set_footer(text=f"Session: {session_id} | Hydragent v0.4.0")
            await interaction.edit_original_response(content=None, embed=embed)

        for guild in self.guild_ids:
            self.tree.copy_global_to(guild=guild)
            await self.tree.sync(guild=guild)

    async def on_message(self, message: discord.Message):
        """Handle DMs and @mentions outside slash commands."""
        if message.author.bot:
            return
        if self.user not in message.mentions and not isinstance(message.channel, discord.DMChannel):
            return

        content = message.content.replace(f"<@{self.user.id}>", "").strip()
        if not content:
            return

        async with message.channel.typing():
            session_id = f"discord:{message.channel.id}"
            event = {
                "session_id": session_id,
                "channel_id": f"discord:{message.channel.id}",
                "user_id":    f"discord_user:{message.author.id}",
                "content":    content,
                "attachments": [],
                "metadata":   {"platform": "discord"},
                "timestamp":  int(message.created_at.timestamp() * 1000),
                "priority":   "normal",
            }

            response = await self.bus.send_intent(event)
            # Split long responses into multiple messages (Discord 2000 char limit)
            for i in range(0, len(response), 1990):
                await message.reply(response[i:i+1990])

    async def start_adapter(self):
        await self.bus.connect()
        self.bus.register_push_handler("discord", self._handle_push)
        await self.start(self.token)

    async def _handle_push(self, push: dict):
        channel_id_str = push["channel_id"].replace("discord:", "")
        channel = self.get_channel(int(channel_id_str))
        if channel:
            await channel.send(push["content"])

if __name__ == "__main__":
    client = HydraDiscordClient()
    asyncio.run(client.start_adapter())
```

---

### 5.5 Slack Adapter

```python
# adapters/slack_adapter.py

import asyncio
import yaml
from pathlib import Path
from slack_sdk.web.async_client import AsyncWebClient
from slack_sdk.socket_mode.aiohttp import SocketModeClient
from slack_sdk.socket_mode.request import SocketModeRequest
from slack_sdk.socket_mode.response import SocketModeResponse
from bus_client import BusClient

class SlackAdapter:
    def __init__(self, config_path: str = "config/channels/slack.yaml"):
        cfg = yaml.safe_load(Path(config_path).read_text())
        self.web_client = AsyncWebClient(token=cfg["bot_token"])
        self.socket_client = SocketModeClient(
            app_token=cfg["app_token"],     # xapp-... token for Socket Mode
            web_client=self.web_client,
        )
        self.bus = BusClient()

    async def process_event(self, client: SocketModeClient, req: SocketModeRequest):
        """Handle all Slack Socket Mode events."""
        if req.type != "events_api":
            return

        event = req.payload.get("event", {})

        # Only handle app_mention or DMs
        if event.get("type") not in ("app_mention", "message"):
            await client.send_socket_mode_response(SocketModeResponse(envelope_id=req.envelope_id))
            return

        # Ignore bot messages
        if event.get("bot_id"):
            await client.send_socket_mode_response(SocketModeResponse(envelope_id=req.envelope_id))
            return

        await client.send_socket_mode_response(SocketModeResponse(envelope_id=req.envelope_id))

        channel = event["channel"]
        user = event.get("user", "unknown")
        text = event.get("text", "").strip()
        thread_ts = event.get("thread_ts") or event.get("ts")

        # Post initial "thinking" message in thread
        response = await self.web_client.chat_postMessage(
            channel=channel,
            thread_ts=thread_ts,
            text="_⏳ Thinking…_",
            mrkdwn=True
        )
        reply_ts = response["ts"]

        bus_event = {
            "session_id": f"slack:{channel}:{user}",
            "channel_id": f"slack:{channel}",
            "user_id":    f"slack_user:{user}",
            "content":    text,
            "attachments": [],
            "metadata":   {"platform": "slack", "thread_ts": thread_ts},
            "timestamp":  int(float(event.get("ts", 0)) * 1000),
            "priority":   "normal",
        }

        accumulated = ""

        async def on_token(token: str):
            nonlocal accumulated
            accumulated += token

        final = await self.bus.send_intent(bus_event, on_token=on_token)

        # Update the placeholder message with the final response
        await self.web_client.chat_update(
            channel=channel,
            ts=reply_ts,
            text=final,
            mrkdwn=True
        )

    async def start(self):
        await self.bus.connect()
        self.bus.register_push_handler("slack", self._handle_push)
        self.socket_client.socket_mode_request_listeners.append(self.process_event)
        await self.socket_client.connect()
        print("✅ Slack adapter connected via Socket Mode")
        await asyncio.Event().wait()  # Run forever

    async def _handle_push(self, push: dict):
        channel_id = push["channel_id"].replace("slack:", "")
        await self.web_client.chat_postMessage(channel=channel_id, text=push["content"])

if __name__ == "__main__":
    adapter = SlackAdapter()
    asyncio.run(adapter.start())
```

---

### 5.6 Email Adapter (IMAP/SMTP)

```python
# adapters/email_adapter.py

import asyncio
import imaplib
import smtplib
import email as email_lib
from email.mime.text import MIMEText
from email.mime.multipart import MIMEMultipart
import yaml
from pathlib import Path
from bus_client import BusClient

class EmailAdapter:
    """
    Polls an IMAP inbox for unread emails from allowed senders.
    Routes email body as IntentEvent to Hydragent core.
    Replies via SMTP with the agent's response.
    """
    def __init__(self, config_path: str = "config/channels/email.yaml"):
        cfg = yaml.safe_load(Path(config_path).read_text())
        self.imap_host = cfg["imap_host"]
        self.imap_port = cfg.get("imap_port", 993)
        self.smtp_host = cfg["smtp_host"]
        self.smtp_port = cfg.get("smtp_port", 587)
        self.username = cfg["username"]
        self.password = cfg["password"]
        self.allowed_senders = set(cfg.get("allowed_senders", []))
        self.poll_interval = cfg.get("poll_interval_sec", 30)
        self.bus = BusClient()

    async def poll_forever(self):
        """Poll IMAP for new messages at configured interval."""
        await self.bus.connect()
        print(f"✅ Email adapter polling {self.imap_host} every {self.poll_interval}s")

        while True:
            try:
                await self._poll_once()
            except Exception as e:
                print(f"⚠️  Email poll error: {e}")
            await asyncio.sleep(self.poll_interval)

    async def _poll_once(self):
        """Connect to IMAP, fetch unread emails, process each."""
        loop = asyncio.get_event_loop()
        await loop.run_in_executor(None, self._sync_poll)

    def _sync_poll(self):
        """Synchronous IMAP polling (runs in thread pool executor)."""
        with imaplib.IMAP4_SSL(self.imap_host, self.imap_port) as imap:
            imap.login(self.username, self.password)
            imap.select("INBOX")

            _, message_ids = imap.search(None, "UNSEEN")
            for msg_id in message_ids[0].split():
                _, msg_data = imap.fetch(msg_id, "(RFC822)")
                raw_email = msg_data[0][1]
                msg = email_lib.message_from_bytes(raw_email)

                sender = email_lib.utils.parseaddr(msg["From"])[1]
                subject = msg.get("Subject", "(no subject)")

                # Access control
                if self.allowed_senders and sender not in self.allowed_senders:
                    print(f"⛔ Rejected email from unauthorized sender: {sender}")
                    continue

                # Extract plain text body
                body = ""
                if msg.is_multipart():
                    for part in msg.walk():
                        if part.get_content_type() == "text/plain":
                            body = part.get_payload(decode=True).decode("utf-8", errors="replace")
                            break
                else:
                    body = msg.get_payload(decode=True).decode("utf-8", errors="replace")

                # Run async bus call synchronously
                import asyncio as _asyncio
                response = _asyncio.run(self._process_email(sender, subject, body.strip()))

                # Send SMTP reply
                self._send_reply(sender, subject, response)

                # Mark as read
                imap.store(msg_id, "+FLAGS", "\\Seen")

    async def _process_email(self, sender: str, subject: str, body: str) -> str:
        session_id = f"email:{sender.replace('@', '_at_')}"
        event = {
            "session_id": session_id,
            "channel_id": f"email:{sender}",
            "user_id":    f"email:{sender}",
            "content":    f"Subject: {subject}\n\n{body}",
            "attachments": [],
            "metadata":   {"platform": "email", "original_subject": subject},
            "timestamp":  int(__import__("time").time() * 1000),
            "priority":   "normal",
        }
        return await self.bus.send_intent(event)

    def _send_reply(self, to_addr: str, original_subject: str, body: str):
        subject = f"Re: {original_subject}" if not original_subject.startswith("Re:") else original_subject

        msg = MIMEMultipart("alternative")
        msg["Subject"] = subject
        msg["From"] = self.username
        msg["To"] = to_addr

        msg.attach(MIMEText(body, "plain"))

        with smtplib.SMTP(self.smtp_host, self.smtp_port) as smtp:
            smtp.starttls()
            smtp.login(self.username, self.password)
            smtp.sendmail(self.username, to_addr, msg.as_string())

if __name__ == "__main__":
    adapter = EmailAdapter()
    asyncio.run(adapter.poll_forever())
```

---

### 5.7 Webhook Adapter (Inbound HTTP)

```python
# adapters/webhook_adapter.py

import asyncio
import hmac
import hashlib
import yaml
from pathlib import Path
from fastapi import FastAPI, Request, HTTPException, Header
from fastapi.responses import JSONResponse
import uvicorn
from bus_client import BusClient

class WebhookAdapter:
    def __init__(self, config_path: str = "config/channels/webhook.yaml"):
        cfg = yaml.safe_load(Path(config_path).read_text())
        self.port = cfg.get("port", 8080)
        self.secret = cfg.get("secret", "")   # HMAC-SHA256 secret for request verification
        self.bus = BusClient()
        self.app = FastAPI(title="Hydragent Webhook Adapter")
        self._register_routes()

    def _register_routes(self):
        @self.app.post("/webhook")
        async def handle_webhook(
            request: Request,
            x_hydragent_signature: str = Header(default="")
        ):
            body = await request.body()

            # Verify HMAC signature if secret is configured
            if self.secret:
                expected = "sha256=" + hmac.new(
                    self.secret.encode(), body, hashlib.sha256
                ).hexdigest()
                if not hmac.compare_digest(x_hydragent_signature, expected):
                    raise HTTPException(status_code=401, detail="Invalid signature")

            try:
                payload = await request.json()
            except Exception:
                raise HTTPException(status_code=400, detail="Invalid JSON body")

            content = payload.get("content") or payload.get("text") or payload.get("message", "")
            if not content:
                raise HTTPException(status_code=400, detail="Missing 'content' field")

            session_id = payload.get("session_id", f"webhook:{request.client.host}")
            event = {
                "session_id": session_id,
                "channel_id": f"webhook:{request.client.host}",
                "user_id":    payload.get("user_id", f"webhook_user:{request.client.host}"),
                "content":    content,
                "attachments": [],
                "metadata":   dict(request.headers),
                "timestamp":  __import__("time").time_ns() // 1_000_000,
                "priority":   payload.get("priority", "normal"),
            }

            response = await self.bus.send_intent(event)
            return JSONResponse({"response": response, "session_id": session_id})

        @self.app.get("/health")
        async def health():
            return {"status": "ok", "adapter": "webhook"}

    async def start(self):
        await self.bus.connect()
        config = uvicorn.Config(self.app, host="0.0.0.0", port=self.port, log_level="info")
        server = uvicorn.Server(config)
        print(f"✅ Webhook adapter listening on port {self.port}")
        await server.serve()

if __name__ == "__main__":
    adapter = WebhookAdapter()
    asyncio.run(adapter.start())
```

---

### 5.8 Proactive Heartbeat & Push Notification Engine

The `HeartbeatEngine` is the Rust-side mechanism that allows the agent to reach out to users without waiting for a user message.

```rust
// crates/hydragent-scheduler/src/heartbeat.rs

use std::sync::Arc;
use hydragent_types::PushMessage;
use hydragent_gateway::GatewayRouter;
use tracing::{info, warn};

/// The Heartbeat Engine enables agent-initiated messages.
/// It wraps the GatewayRouter's `push()` method with scheduling context.
pub struct HeartbeatEngine {
    router: Arc<GatewayRouter>,
}

impl HeartbeatEngine {
    pub fn new(router: Arc<GatewayRouter>) -> Self {
        Self { router }
    }

    /// Push a message to a specific channel.
    /// `channel_id` can be a specific channel ("telegram:123456789") or "*" for broadcast.
    pub async fn push(
        &self,
        channel_id: impl Into<String>,
        session_id: impl Into<String>,
        content: impl Into<String>,
    ) -> anyhow::Result<()> {
        let channel_id = channel_id.into();
        let content = content.into();

        info!(
            channel_id = %channel_id,
            content_len = content.len(),
            "Heartbeat pushing proactive message"
        );

        self.router.push(PushMessage {
            channel_id,
            session_id: session_id.into(),
            content,
            markdown: true,
            metadata: Default::default(),
        }).await
    }

    /// Push a scheduled digest summary to a channel.
    pub async fn push_digest(
        &self,
        channel_id: impl Into<String>,
        session_id: impl Into<String>,
        title: &str,
        items: &[String],
    ) -> anyhow::Result<()> {
        let content = format!(
            "📋 **{}**\n\n{}",
            title,
            items.iter()
                .enumerate()
                .map(|(i, item)| format!("{}. {}", i + 1, item))
                .collect::<Vec<_>>()
                .join("\n")
        );

        self.push(channel_id, session_id, content).await
    }
}
```

---

### 5.9 Cron Scheduler (Autonomous Task Engine)

```rust
// crates/hydragent-scheduler/src/cron_scheduler.rs

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use tokio_cron_scheduler::{JobScheduler, Job};
use hydragent_types::CronJob;
use sqlx::SqlitePool;
use uuid::Uuid;
use anyhow::{Context, Result};

pub struct CronScheduler {
    inner: JobScheduler,
    /// Maps CronJob UUID → tokio-cron-scheduler job UUID
    job_handles: Mutex<HashMap<String, uuid::Uuid>>,
    db: SqlitePool,
}

impl CronScheduler {
    pub async fn new(db: SqlitePool) -> Result<Arc<Self>> {
        let scheduler = JobScheduler::new().await
            .context("Failed to create JobScheduler")?;
        scheduler.start().await.context("Failed to start JobScheduler")?;

        let this = Arc::new(Self {
            inner: scheduler,
            job_handles: Mutex::new(HashMap::new()),
            db,
        });

        // Reload active jobs from SQLite on startup
        this.reload_from_db().await?;

        Ok(this)
    }

    /// Register a new cron job. Persists to SQLite.
    pub async fn add_job(
        &self,
        cron_expr: &str,
        description: &str,
        task_fn: impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync + 'static,
    ) -> Result<String> {
        let job_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();

        // Validate the cron expression
        cron_expr.parse::<cron::Schedule>()
            .with_context(|| format!("Invalid cron expression: '{}'", cron_expr))?;

        // Create the Tokio job
        let job = Job::new_async(cron_expr, move |_, _| {
            Box::pin(task_fn())
        }).with_context(|| format!("Failed to create cron job with expression: '{}'", cron_expr))?;

        let scheduler_uuid = self.inner.add(job).await?;

        // Track handle mapping
        self.job_handles.lock().insert(job_id.clone(), scheduler_uuid);

        // Persist to SQLite
        sqlx::query!(
            r#"
            INSERT INTO cron_jobs (id, cron_expr, description, status, created_at)
            VALUES (?, ?, ?, 'active', ?)
            "#,
            job_id, cron_expr, description, now
        )
        .execute(&self.db)
        .await?;

        tracing::info!(job_id, cron_expr, description, "Cron job registered");
        Ok(job_id)
    }

    /// Remove a cron job by its UUID.
    pub async fn remove_job(&self, job_id: &str) -> Result<bool> {
        let scheduler_uuid = {
            let mut handles = self.job_handles.lock();
            handles.remove(job_id)
        };

        if let Some(uuid) = scheduler_uuid {
            self.inner.remove(&uuid).await?;

            sqlx::query!(
                "UPDATE cron_jobs SET status = 'deleted' WHERE id = ?",
                job_id
            ).execute(&self.db).await?;

            tracing::info!(job_id, "Cron job removed");
            return Ok(true);
        }

        Ok(false)
    }

    /// Record a job execution in SQLite (called inside each job's closure).
    pub async fn record_execution(&self, job_id: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query!(
            "UPDATE cron_jobs SET last_run_at = ?, run_count = run_count + 1 WHERE id = ?",
            now, job_id
        ).execute(&self.db).await?;
        Ok(())
    }

    /// Load all `active` jobs from SQLite and re-register them.
    async fn reload_from_db(&self) -> Result<()> {
        let jobs = sqlx::query_as!(
            CronJob,
            "SELECT * FROM cron_jobs WHERE status = 'active'"
        ).fetch_all(&self.db).await?;

        tracing::info!(count = jobs.len(), "Reloading cron jobs from database");

        for job in jobs {
            tracing::info!(
                job_id = %job.id,
                cron_expr = %job.cron_expr,
                description = %job.description,
                "Re-registering persistent cron job"
            );
            // Note: full task reconstruction requires task_type dispatch logic
            // This is a simplified stub; production code would dispatch based on task_type
        }

        Ok(())
    }
}
```

**SQLite schema for scheduler** (`migrations/003_scheduler.sql`):

```sql
CREATE TABLE IF NOT EXISTS cron_jobs (
    id              TEXT    PRIMARY KEY,   -- UUID
    cron_expr       TEXT    NOT NULL,      -- e.g. "0 9 * * *"
    description     TEXT    NOT NULL,      -- Human-readable task description
    task_type       TEXT    NOT NULL DEFAULT 'react_loop',
    task_params     TEXT    NOT NULL DEFAULT '{}',
    target_channel_id TEXT  NOT NULL DEFAULT '*',
    status          TEXT    NOT NULL CHECK(status IN ('active', 'paused', 'deleted')),
    created_at      INTEGER NOT NULL,
    last_run_at     INTEGER,
    run_count       INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_cron_jobs_status ON cron_jobs(status);

CREATE TABLE IF NOT EXISTS cron_job_runs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id          TEXT    NOT NULL,
    started_at      INTEGER NOT NULL,
    completed_at    INTEGER,
    status          TEXT    NOT NULL CHECK(status IN ('running', 'completed', 'failed')),
    output_summary  TEXT,
    FOREIGN KEY(job_id) REFERENCES cron_jobs(id)
);
```

---

### 5.10 Work IQ — Background Awareness Layer

Work IQ is the always-on background intelligence. It monitors external information sources and proactively surfaces what matters, before the user has to ask.

```rust
// crates/hydragent-scheduler/src/work_iq_monitor.rs

use feed_rs::parser;
use reqwest::Client;
use std::collections::HashMap;
use chrono::Utc;
use hydragent_scheduler::HeartbeatEngine;

#[derive(Debug, Clone)]
pub struct FeedMonitor {
    pub url: String,
    pub name: String,
    pub keywords: Vec<String>,        // Alert immediately on match
    pub digest_channel: String,       // Channel to push digests to
    pub digest_cron: String,          // When to push digests
    pub last_seen_entry_id: Option<String>,
}

pub struct WorkIqEngine {
    monitors: Vec<FeedMonitor>,
    heartbeat: HeartbeatEngine,
    http: Client,
    llm_provider: Arc<dyn ModelProvider>,
    session_id: String,
}

impl WorkIqEngine {
    pub async fn run_poll_cycle(&mut self) -> anyhow::Result<WorkIqStats> {
        let mut stats = WorkIqStats::default();

        for monitor in &mut self.monitors {
            match self.poll_feed(monitor).await {
                Ok(new_entries) => {
                    stats.feeds_polled += 1;
                    stats.new_entries += new_entries.len();

                    // 1. Check for keyword alerts (immediate push)
                    for entry in &new_entries {
                        for keyword in &monitor.keywords {
                            let content = format!("{} {}", entry.title, entry.summary);
                            if content.to_lowercase().contains(&keyword.to_lowercase()) {
                                let alert = format!(
                                    "🔔 **Work IQ Alert** — keyword `{}` matched in **{}**\n\n**{}**\n{}\n{}",
                                    keyword, monitor.name, entry.title, entry.summary, entry.url
                                );
                                let _ = self.heartbeat.push(
                                    &monitor.digest_channel,
                                    &self.session_id,
                                    &alert,
                                ).await;
                                stats.alerts_sent += 1;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        url = %monitor.url,
                        error = %e,
                        "Work IQ: failed to poll feed"
                    );
                }
            }
        }

        Ok(stats)
    }

    async fn poll_feed(&self, monitor: &mut FeedMonitor) -> anyhow::Result<Vec<FeedEntry>> {
        let response = self.http
            .get(&monitor.url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        let bytes = response.bytes().await?;
        let feed = parser::parse(bytes.as_ref())
            .map_err(|e| anyhow::anyhow!("Feed parse error: {}", e))?;

        let mut new_entries = Vec::new();
        for entry in feed.entries {
            let entry_id = entry.id.clone();

            // Skip already-seen entries
            if monitor.last_seen_entry_id.as_deref() == Some(&entry_id) {
                break;
            }

            let title = entry.title.map(|t| t.content).unwrap_or_default();
            let summary = entry.summary
                .map(|s| s.content)
                .or_else(|| entry.content.first().map(|c| c.body.clone().unwrap_or_default()))
                .unwrap_or_default();

            // Truncate long summaries
            let summary = if summary.len() > 500 {
                format!("{}…", &summary[..497])
            } else {
                summary
            };

            let url = entry.links.first().map(|l| l.href.clone()).unwrap_or_default();

            new_entries.push(FeedEntry { id: entry_id, title, summary, url });
        }

        // Update last seen
        if let Some(first) = new_entries.first() {
            monitor.last_seen_entry_id = Some(first.id.clone());
        }

        Ok(new_entries)
    }

    /// Generate a digest of new entries using the LLM.
    pub async fn generate_digest(
        &self,
        monitor_name: &str,
        entries: &[FeedEntry],
    ) -> anyhow::Result<String> {
        if entries.is_empty() {
            return Ok(format!("📰 **{}** — No new entries since last digest.", monitor_name));
        }

        let entry_list = entries.iter()
            .map(|e| format!("- **{}**: {} ({})", e.title, e.summary, e.url))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Summarize the following {} feed entries into a concise daily digest in 3-5 bullet points. \
             Focus on the most important and actionable items.\n\nEntries:\n{}",
            monitor_name, entry_list
        );

        self.llm_provider.generate_non_streaming(&prompt).await
    }
}

#[derive(Debug, Clone)]
pub struct FeedEntry {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub url: String,
}

#[derive(Debug, Default)]
pub struct WorkIqStats {
    pub feeds_polled: usize,
    pub new_entries: usize,
    pub alerts_sent: usize,
    pub digests_generated: usize,
}
```

---

## 6. Built-in Tools (Phase 4 Additions)

Three new tools give the agent the ability to interact with the scheduling and channel system from within a ReAct loop.

### `schedule_task`

```yaml
name: schedule_task
description: "Schedule an autonomous task to run on a cron schedule. Use when the user asks the agent to do something automatically on a recurring basis."
tier: prompt  # Modifies system state — requires user approval
params_schema:
  type: object
  required: [cron_expr, description, task]
  properties:
    cron_expr:
      type: string
      description: "Standard cron expression (5 or 6 fields). Examples: '0 9 * * *' (daily 9 AM), '*/30 * * * *' (every 30 minutes)"
    description:
      type: string
      description: "Human-readable description of what this task does."
    task:
      type: string
      description: "What the agent should do when the job fires (e.g., 'Search for Rust news and push a digest to Telegram')."
    target_channel:
      type: string
      description: "Channel ID to deliver results to (e.g., 'telegram:123456789'). Defaults to the current session's channel."
      default: "current"

output:
  type: object
  properties:
    job_id:      { type: string, description: "UUID of the created cron job" }
    cron_expr:   { type: string }
    description: { type: string }
    next_run:    { type: string, description: "ISO 8601 timestamp of next scheduled execution" }
```

---

### `send_message`

```yaml
name: send_message
description: "Proactively send a message to any registered channel. Use when you need to notify the user on a different channel from the current one, or push an update after completing a background task."
tier: auto_approve
params_schema:
  type: object
  required: [channel_id, content]
  properties:
    channel_id:
      type: string
      description: "Target channel (e.g., 'telegram:123456789', 'discord:987654321', '*' for all channels)"
    content:
      type: string
      description: "Message content (Markdown supported)"

output:
  type: object
  properties:
    delivered:  { type: boolean }
    channel_id: { type: string }
```

---

### `rss_subscribe`

```yaml
name: rss_subscribe
description: "Add an RSS or Atom feed to the Work IQ monitor. The agent will check this feed periodically and alert you if matching keywords are found."
tier: prompt
params_schema:
  type: object
  required: [url, name]
  properties:
    url:
      type: string
      description: "RSS or Atom feed URL"
    name:
      type: string
      description: "Friendly name for this feed (e.g., 'Rust Blog')"
    keywords:
      type: array
      items: { type: string }
      description: "Keywords that trigger an immediate alert if found in any entry title or summary"
      default: []
    digest_channel:
      type: string
      description: "Channel to push daily digests to"
      default: "current"
    digest_cron:
      type: string
      description: "Cron schedule for digest delivery"
      default: "0 8 * * *"

output:
  type: object
  properties:
    subscribed:   { type: boolean }
    feed_name:    { type: string }
    latest_entry: { type: string, description: "Title of the most recent entry in the feed" }
```

---

## 7. Configuration & Environment

### Updated `.env.example`

```ini
# ── Phase 1-3 (unchanged) ─────────────────────────────────────────────────
OPENROUTER_API_KEYS=sk-or-v1-...
PRIMARY_MODEL=nvidia/nemotron-3-ultra-550b-a55b:free
FALLBACK_MODELS=openai/gpt-4o-mini,meta-llama/llama-3-8b-instruct:free
ENABLE_SEMANTIC_MEMORY=true
ENABLE_DREAMING=true

# ── Phase 4: Gateway ───────────────────────────────────────────────────────

# Enable/disable individual channel adapters
ENABLE_TELEGRAM=true
ENABLE_DISCORD=false
ENABLE_SLACK=false
ENABLE_EMAIL=false
ENABLE_WEBHOOK=true

# ── Phase 4: Webhook ───────────────────────────────────────────────────────
WEBHOOK_PORT=8080
WEBHOOK_SECRET=change_me_to_a_random_secret_string

# ── Phase 4: Cron Scheduler ────────────────────────────────────────────────
ENABLE_SCHEDULER=true
SCHEDULER_DB_PATH=./data/scheduler.db

# ── Phase 4: Work IQ ───────────────────────────────────────────────────────
ENABLE_WORK_IQ=true
WORK_IQ_POLL_INTERVAL_SEC=300     # 5 minutes between feed polls
WORK_IQ_FEEDS_CONFIG=./config/work_iq/feeds.yaml
```

### `config/channels/telegram.yaml`

```yaml
# Telegram Bot configuration
bot_token: "YOUR_TELEGRAM_BOT_TOKEN"   # From @BotFather

# Restrict to specific chat IDs (empty list = allow all)
allowed_chat_ids:
  - 123456789    # Your personal Telegram chat ID

# Rate limiting (messages per second)
rate_limit_per_chat: 1
rate_limit_global: 30

# Streaming: edit-in-place every N characters
streaming_edit_interval_chars: 50
```

### `config/channels/discord.yaml`

```yaml
bot_token: "YOUR_DISCORD_BOT_TOKEN"
application_id: "YOUR_DISCORD_APPLICATION_ID"

# Sync slash commands to these specific guilds (empty = global, 1h delay)
guild_ids:
  - 111111111111111111

# Allow @mentions in these channel types: ["text", "dm", "thread"]
allowed_channel_types: ["text", "dm"]
```

### `config/work_iq/feeds.yaml`

```yaml
feeds:
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    keywords: ["stable", "release", "1.", "CVE"]
    digest_channel: "telegram:123456789"
    digest_cron: "0 8 * * *"    # 8 AM daily

  - name: "Hacker News Top"
    url: "https://hnrss.org/frontpage"
    keywords: ["Rust", "AI agent", "LLM", "security breach"]
    digest_channel: "telegram:123456789"
    digest_cron: "0 9 * * 1"   # Monday 9 AM (weekly digest)

  - name: "Cargo Security Advisories"
    url: "https://rustsec.org/feed.xml"
    keywords: []                # Alert on ALL new advisories
    digest_channel: "*"         # Broadcast to all channels
    digest_cron: "0 9 * * *"
```

---

## 8. Testing Strategy

### 8.1 Unit Tests

| Test File | What It Covers |
|---|---|
| `dedup_test.rs` | Same `(channel_id, user_id, content)` within 30s is dropped; different content same user passes; same content after 31s passes (LRU expiry) |
| `rate_limiter_test.rs` | Token bucket: first N requests pass; N+1 is rejected; tokens refill correctly after 1s window |
| `cron_scheduler_test.rs` | Valid cron `"* * * * * *"` fires 5 times in 5s (accelerated); invalid cron `"99 * * * *"` returns `Err`; remove_job stops execution |
| `formatter_test.rs` | `TelegramFormatter::escape_markdown_v2("Hello (world)!")` produces `"Hello \\(world\\)\\!"`; code blocks survive escaping |
| `work_iq_test.rs` | Mock HTTP feed → 3 new entries parsed; keyword match triggers heartbeat; empty feed returns 0 entries |
| `heartbeat_test.rs` | `push("*", ...)` calls all registered adapters; `push("telegram:123", ...)` calls only Telegram adapter |

### 8.2 Integration Tests

```python
# tests/integration/telegram_e2e_test.py

import pytest
import asyncio
from unittest.mock import AsyncMock, patch, MagicMock

@pytest.mark.asyncio
async def test_telegram_message_roundtrip():
    """
    Test a full Telegram message → bus → orchestrator (mocked) → Telegram response flow.
    """
    mock_bus = AsyncMock()
    mock_bus.send_intent = AsyncMock(return_value="The capital of France is Paris.")
    mock_bot = AsyncMock()

    with patch("adapters.telegram_adapter.BusClient", return_value=mock_bus):
        from adapters.telegram_adapter import TelegramAdapter
        adapter = TelegramAdapter.__new__(TelegramAdapter)
        adapter.bus = mock_bus
        adapter.allowed_chat_ids = set()  # Allow all

        # Simulate incoming message
        mock_update = MagicMock()
        mock_update.effective_chat.id = 123456789
        mock_update.effective_user.id = 987654321
        mock_update.message.text = "What is the capital of France?"
        mock_update.message.reply_text = AsyncMock(return_value=MagicMock(message_id=42))

        mock_ctx = MagicMock()
        mock_ctx.bot.edit_message_text = AsyncMock()

        await adapter.on_message(mock_update, mock_ctx)

        # Assert bus received the intent event
        mock_bus.send_intent.assert_called_once()
        event = mock_bus.send_intent.call_args[0][0]
        assert event["content"] == "What is the capital of France?"
        assert event["channel_id"] == "telegram:123456789"

        # Assert final response was sent
        mock_ctx.bot.edit_message_text.assert_called()
        last_call = mock_ctx.bot.edit_message_text.call_args
        assert "Paris" in last_call.kwargs.get("text", "")

@pytest.mark.asyncio
async def test_webhook_roundtrip():
    """Test webhook POST → bus → response in HTTP reply."""
    from fastapi.testclient import TestClient
    from adapters.webhook_adapter import WebhookAdapter

    mock_bus = AsyncMock()
    mock_bus.send_intent = AsyncMock(return_value="Pong!")
    mock_bus.connect = AsyncMock()

    adapter = WebhookAdapter.__new__(WebhookAdapter)
    adapter.secret = ""
    adapter.bus = mock_bus
    adapter._register_routes()

    client = TestClient(adapter.app)
    response = client.post("/webhook", json={"content": "ping"})

    assert response.status_code == 200
    assert response.json()["response"] == "Pong!"
```

```rust
// tests/integration/work_iq_test.rs

#[tokio::test]
async fn test_work_iq_keyword_alert() {
    // 1. Set up mock HTTP server with RSS feed
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(include_str!("fixtures/sample_rss.xml")))
        .mount(&mock_server)
        .await;

    // 2. Create monitor with keyword "stable"
    let mut engine = WorkIqEngine::new_test(
        vec![FeedMonitor {
            url: format!("{}/feed.xml", mock_server.uri()),
            name: "Test Feed".to_string(),
            keywords: vec!["stable".to_string()],
            digest_channel: "test:channel".to_string(),
            digest_cron: "0 9 * * *".to_string(),
            last_seen_entry_id: None,
        }],
        MockHeartbeat::new(),
    );

    // 3. Run one poll cycle
    let stats = engine.run_poll_cycle().await.unwrap();

    // 4. Assertions
    assert_eq!(stats.feeds_polled, 1);
    assert!(stats.new_entries > 0);
    // RSS fixture contains "Rust 1.80 stable release"
    assert_eq!(stats.alerts_sent, 1);
}
```

### 8.3 Manual QA Checklist (Phase 4 Sign-off)

```
[ ] Start Telegram adapter; send "Hello" → verify response in Telegram chat
[ ] Send "What is 2 + 2?" → verify streamed response (edit-in-place visible)
[ ] Start Discord adapter; run /ask "Summarize AI news" → response appears as embed
[ ] @mention bot in a channel → response appears in thread
[ ] Start Webhook adapter; run curl -X POST http://localhost:8080/webhook \
    -H "Content-Type: application/json" -d '{"content":"ping"}' → response in JSON body
[ ] Ask agent: "Every morning at 9 AM, search for Rust news and send me a summary"
    → schedule_task tool fires → job appears in cron_jobs table
[ ] Wait for cron to fire (set DREAMING_INTERVAL_SEC=5 for testing)
    → Telegram notification received
[ ] Add RSS feed via chat: "Monitor https://blog.rust-lang.org/feed.xml for stable releases"
    → rss_subscribe tool → feed in feeds.yaml → next poll shows entries
[ ] Kill agent; restart → cron job reloads from SQLite → continues firing
[ ] `./hydragent channels list` → shows all 4 adapters with status
[ ] `cargo test --workspace` → exits 0
[ ] `pytest adapters/ -v` → exits 0
```

---

## 9. Performance Targets

| Metric | Target | Measurement Method |
|---|---|---|
| Gateway inbound routing latency | < 1 ms | `tracing::instrument` span from `inbound()` entry to `bus_tx.send()` |
| Message deduplication check | < 0.1 ms | LRU cache lookup, in-memory HashMap |
| Rate limiter check | < 0.1 ms | Token bucket atomic counter |
| Telegram response first-byte latency | < 800 ms | Time from message received to first `edit_message_text` call |
| Discord slash command acknowledgment | < 3 s | Discord hard-limits; we must defer within 3s |
| Webhook HTTP response time (no LLM) | < 50 ms | FastAPI endpoint measured by `httpx` in test |
| Email poll cycle | < 5 s per inbox | IMAP connect + fetch + mark-read |
| Cron job fire accuracy | ± 1 s | Compare `actual_fire_time` vs `expected_fire_time` in test |
| Work IQ feed poll (per feed) | < 10 s | HTTP GET + feed parse timeout |
| Work IQ digest generation | < 30 s | LLM call latency for digest prompt |
| Heartbeat push delivery | < 2 s | From `push()` call to adapter `send()` return |
| Concurrent channel handling | 10 sessions | 10 simultaneous messages across 4 adapters, all responding correctly |

---

## 10. Risks & Mitigations

| Risk | Category | Likelihood | Impact | Mitigation Strategy |
|---|---|---|---|---|
| **Telegram Bot API rate limits** | API | High | Medium | `python-telegram-bot` has built-in rate limiting. Configure `rate_limit_per_chat=1` in `telegram.yaml`. Implement per-chat message queue. |
| **Discord slash command 3s acknowledgment deadline** | API | Medium | High | `await interaction.response.defer(thinking=True)` immediately on invocation. Never run synchronous code before the defer. |
| **Slack Socket Mode disconnections** | Network | Medium | Medium | Use `socket_client.connect()` with auto-reconnect. Log disconnect/reconnect events via `tracing`. |
| **Email IMAP polling causing duplicate processing** | Logic | Medium | High | Mark emails as `\Seen` immediately after fetching (before processing). Implement `dedup.rs` at gateway level keyed on IMAP Message-ID header. |
| **Webhook impersonation (no HMAC verification)** | Security | Low | High | Enforce `WEBHOOK_SECRET` in production. `HMAC-SHA256` verification before processing. Return 401 on invalid signature. |
| **Cron job accumulation (zombie jobs)** | Storage | Low | Low | `./hydragent jobs list` shows all jobs with last-run timestamp. Auto-delete jobs that have failed > 10 times consecutively. |
| **Work IQ LLM digest cost** | Cost | Medium | Low | Daily digest at 9 AM uses `DREAMING_MODEL` (cheap model). Limit digest to 10 entries max. Cache digest per feed per day. |
| **RSS feed parse errors** | Reliability | Medium | Low | `feed-rs` handles most RSS/Atom variants. Wrap in `Result` and log warning; never crash the poll cycle. |
| **Multiple adapters routing same user message** | Logic | Low | Medium | `Deduplicator` keys on `(channel_id, user_id, content)` — cross-channel duplicates are prevented by unique `channel_id` prefix. |

---

## 11. Definition of Done

Phase 4 is **complete** when all of the following are true:

### Code

- [ ] `cargo build --workspace` and `cargo test --workspace` both exit 0 with `RUSTFLAGS="-D warnings"`
- [ ] `cargo clippy --workspace -- -D warnings` exits 0
- [ ] All Python adapter tests pass: `pytest adapters/ -v` exits 0
- [ ] No `TODO` or `FIXME` in Phase 4 source files
- [ ] All Phase 1, 2, 3 tests continue to pass (zero regressions)

### Adapters

- [ ] Telegram adapter: end-to-end message roundtrip with streaming verified
- [ ] Discord adapter: slash command `/ask` delivers embed response
- [ ] Slack adapter: `@mention` in channel delivers response in thread
- [ ] Webhook adapter: `POST /webhook` returns JSON with agent response
- [ ] Email adapter: IMAP poll detects mock email; SMTP reply sent
- [ ] All adapters: `push()` via heartbeat delivers proactive message

### Scheduler

- [ ] Cron jobs survive process restart (reloaded from SQLite)
- [ ] `schedule_task` tool creates persisted job visible in `cron_jobs` table
- [ ] Job fires within ± 1 s of scheduled time in integration test
- [ ] `remove_job()` stops further executions (verified in unit test)

### Work IQ

- [ ] ≥ 1 RSS feed monitored and polled correctly
- [ ] Keyword match triggers immediate heartbeat alert
- [ ] Digest generated by LLM and pushed to configured channel on schedule

### Performance

- [ ] Gateway inbound routing < 1 ms (measured with tracing)
- [ ] No memory leaks detected after 1-hour continuous multi-channel load test

### Documentation

- [ ] `README.md` updated: Getting Started section includes multi-channel setup
- [ ] `ARCHITECTURE.md` updated with 9-layer diagram (adds Channel Gateway layer)
- [ ] `config/channels/*.yaml.example` files committed for all 4 adapters
- [ ] `PHASE_4.md` (this file) reviewed and reflects actual implementation

### Release

- [ ] `v0.4.0` git tag created
- [ ] `CHANGELOG.md` entry for v0.4.0 written
- [ ] Demo screencast: multi-channel conversation + scheduled cron + Work IQ digest

---

*Previous phase: [PHASE_3.md](PHASE_3.md) — WASM Sandbox, Encrypted Vault & 3-Tier Permission Matrix (Weeks 11–14)*
*Next phase: [PHASE_5.md](PHASE_5.md) — Kimi-style Agent Swarm, DAG Planner & Model Council (Weeks 19–22)*
