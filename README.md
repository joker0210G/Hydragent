<p align="center">
  <img src="https://github.com/user-attachments/assets/6e8567e0-2409-4fbb-b287-a34b7aa06cb8" alt="Hydragent — the scholarly octopus mascot" width="280" />
</p>

<!-- Canonical source asset lives in-repo at: doc/assets/hydragent_tran.PNG (committed) -->
<!-- This CDN URL is the renderable mirror; both stay in sync via the commit. -->

# Hydragent 🐉 — The Unified AI Agent

> *The agent that grows, remembers, executes, and protects — synthesizing the architectural DNA of 40+ frontier AI systems into one coherent, privacy-first, model-agnostic runtime.*

[![Status: v0.7.0 Shipped](https://img.shields.io/badge/Status-v0.7.0%20Shipped-brightgreen)](#roadmap)
[![License: MIT](https://img.shields.io/badge/License-MIT-green)](LICENSE)
[![Core: Rust + Tokio](https://img.shields.io/badge/Core-Rust%20%2B%20Tokio-orange)](#architecture)
[![Edge: Zig](https://img.shields.io/badge/Edge-Zig%20%E2%89%A4678KB-yellow)](#architecture)
[![Adapters: Python](https://img.shields.io/badge/Adapters-Python-blue)](#architecture)
[![Security: 16-Layer](https://img.shields.io/badge/Security-16--Layer%20Cryptographic-red)](#security)

---

## 🐉 Why "Hydragent"?

The name is a deliberate portmanteau — **Hydra + Agent** — chosen to encode the
project's core philosophy in a single word. In Greek myth, the Lernaean Hydra is
defined by **regenerative growth**: when Heracles cut off one head during his
Second Labour, **two more grew back in its place**. Every wound made the Hydra
stronger.

Hydragent's architecture mirrors that mytheme in five concrete ways:

- **Many heads, one body** — one core runtime serving 40+ channel adapters
  (Telegram, Discord, Slack, voice, webhooks, CLI, …), all answering to a
  single orchestrator. *Axiom 5: One Agent, Every Channel.*
- **Cut one head, two grow back** — the self-improving skill engine (axiom 1)
  authors a new skill from every successful execution; the self-healing
  replanner (Devin 3.0 design) recovers from every failure by re-planning,
  not retreating. Pruning the skill library only seeds the next 7-day
  Curator cycle.
- **A beast that cannot be killed by direct assault** — the 16-layer
  cryptographic security pipeline (axiom 3) means defeat one layer (vault,
  taint tracker, injection guard, Merkle audit, …) and the next eleven still
  hold. *Axiom 4 cages every dangerous action.*
- **Specialized heads for specialized tasks** — the Agent Swarm + Model
  Council (axiom 6) can spawn up to 300 sub-agents (Kimi K2.6 design), each
  routed to the best-fit LLM via the **20+ model profiles** in
  `config/model_council.yaml`.
- **Memory as a water-body** — `hydragent-memory` organizes facts into a
  hierarchical file-system metaphor: `data/` is the "ocean" the knowledge
  lives in, indexed by the *memory.search* bus RPC. The `hydr-` root keeps
  the Latin *hydro* (water) alive in the codebase even when the surface
  just reads "agent".

> **In short**: *cut a Hydra head, two grow back. Cut a Hydragent skill, two
> new ones get authored from the failure mode.* The name isn't branding —
> it's a contract with the runtime.

## ⚡ Current State (v0.7.1 — 2026-06-15)

Hydragent is **shipped, not aspirational**. The numbers below are audited by
[`doc/STATE.md`](doc/STATE.md) against the working tree — they are not targets.

> **New here?** Read [`ONBOARDING.md`](ONBOARDING.md) first — the
> 10-minute zero-to-first-chat guide. Coming back to contribute? See
> [`CONTRIBUTING.md`](CONTRIBUTING.md).

| | |
|---|---|
| **Latest release** | **v0.7.1** — *Polish + Python SDK* (2026-06-15) |
| **Patch release** | **v0.6.1** (same day) — 4 user-perspective CLI bug fixes |
| **Workspace** | 16 Rust crates on `resolver = "2"` |
| **Test count** | **49 tests passing in `hydragent-core` kernel binary** (v0.7.1) |
| **Phase 7 net-new** | 86 tests — 52 skills + 30 bench + 4 skill-induction |
| **Channels live** | 6 messaging adapters + Telegram Mini App + browser **Control UI** + bus client |
| **Model Council** | **20+ model profiles** in `config/model_council.yaml`, 8 task types |
| **Skill library** | 3 builtins shipped; FTS5 + tag retrieval; 7-day Curator active |
| **Eval harness** | SKILL-BENCH v1 (**80 tasks**) + Golden Set v1 (**30 pairs**) |
| **Security** | 16-layer pipeline live; 79 vault tests pass; SQLCipher deferred post-MVP |
| **Phase 5 status** | Tracks 5.1–5.2 ✅; 5.3 (DagEngine) + 5.4 (self-healing) pending |
| **Edge binary** | 🐉 Stubbed — Zig workspace present, not yet compiling |
| **Full changelog** | [CHANGELOG.md](CHANGELOG.md) · [RELEASE_NOTES_v0.7.0.md](doc/releases/RELEASE_NOTES_v0.7.0.md) |
| **Ground truth** | [`doc/STATE.md`](doc/STATE.md) — verified against `git rev 3d99366` |

### 🐣 Install in one command

Hydragent ships with a single-line installer — the same UX as Ollama and
OpenClaw. Paste, run, done.

**Windows (PowerShell 5.1+ / 7+):**
```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | iex
```

**macOS / Linux:**
```bash
curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
```

What you get:

- The latest `hydragent` binary on your user `PATH` (~/.hydragent/bin).
- An auto-generated `Hydragent.cmd` / `hydragent` launcher.
- A data directory at `~/.hydragent/data/` with sane defaults.
- A guided first-run onboarding wizard that asks for a brain provider
  and writes `.env` for you.

Then:

```powershell
hydragent status          # one-shot status dashboard
hydragent serve           # gateway in the foreground
hydragent ps              # list running gateways
hydragent stop [pid]      # stop a running gateway
hydragent chat            # interactive REPL
Hydragent ui              # browser Control UI (http://127.0.0.1:8765/)
hydragent update          # ⬆️  self-update to the latest release
hydragent uninstall       # 🗑️  remove the install + PATH entry (prompts first)
```

> Full guide, flags, troubleshooting, offline / air-gapped installs, and
> uninstall: **[INSTALL.md](INSTALL.md)**.
>
> Working from a checkout? See the contributor flow below.

### 🛠 For contributors (build from source)

```bash
git clone https://github.com/joker0210G/Hydragent.git
cd hydragent
cargo build --release -p hydragent-core
./target/release/hydragent onboard       # 🐣 guided setup wizard
./target/release/hydragent chat         # 💬 interactive terminal REPL
```

Other useful commands: `hydragent doctor` (diagnose), `hydragent examples` (starter prompts), `hydragent test-brain` (smoke-test the live brain), `hydragent update` (self-update from GitHub Releases), `hydragent uninstall` (remove the install + PATH entry), `hydragent --help` (full reference). All subcommands have rich `long_about` text — try `hydragent onboard --help` to see it.

## 🌊 What is Hydragent?

Hydragent is a **next-generation, modular meta-agent** that synthesizes the best architectural decisions from the 2026 AI agent landscape into a single, coherent, privacy-first runtime. Rather than picking one agent to copy, Hydragent extracts the *design DNA* from each major system — then reimagines it as a deeply integrated whole.

**The core insight**: Every great agent of 2026 solved *one* problem brilliantly. Hydragent solves them *all simultaneously*:

| System Analyzed | Innovation Extracted |
|---|---|
| **Hermes Agent** (Nous Research) | Closed-loop self-improving skill engine; 7-day Curator cycle; **#1 on OpenRouter** (271B tokens); 7 terminal backends; auto-migration from OpenClaw |
| **OpenClaw** (350K+ ⭐) | **Standing Orders** (persistent behavioral rules across all sessions); **Dreaming** 3-stage nightly memory consolidation; 12+ channels; 6,000+ ClawHub skills; auth profile rotation with exponential backoff |
| **memU** | Hierarchical file-system memory; dual-mode retrieval (cheap embeddings 24/7 + LLM only on high-signal); **92.09% accuracy on Locomo benchmark** |
| **OpenFang** | 16-layer cryptographic security; Merkle audit trails; TEE execution; 40+ channels; 30+ built-in tools; pre-built autonomous "Hands" packages |
| **IronClaw** (NEAR AI) | WASM capability sandboxing; boundary key injection (secrets never touch LLM); **highest adversarial resilience** in NEAR AI evaluation suite |
| **Kimi K2.6** (Moonshot AI) | **Agent Swarm: up to 300 sub-agents, 4,000 coordinated steps**; 1T-param MoE, 32B active/token; 256K context; **SWE-bench Pro 58.6%** |
| **NanoClaw** | Container-isolated agents (~4k LoC auditable core); OS-level Docker isolation; no raw key storage (Agent Vault + OneCLI) |
| **ZeroClaw** | Single Rust binary (~8.8 MB); <5 MB RAM; <10 ms startup; trait-driven hot-swap architecture |
| **NullClaw** | Zig static binary (678 KB); ~1 MB RAM; <2 ms boot; 22+ providers; 18+ channels; hybrid search (0.7 vector / 0.3 BM25) |
| **PicoClaw** | Gene Evolution Protocol on $10 RISC-V hardware; self-bootstrapping (AI drove its own architecture migration) |
| **GoClaw** | OpenClaw rebuilt in Go with **multi-tenant isolation, 5-layer security**, native concurrency; multi-tenant PostgreSQL; single binary |
| **Manus** | VM-sandboxed parallel task execution; **GAIA benchmark 65%+** vs GPT-4o 32%; asynchronous long-running tasks (hours/days) |
| **Perplexity Computer** | **20+ model orchestration**; Model Council (compare 3 models for high-stakes decisions); **80% completion rate** on complex workflows; Personal Computer mode (local Mac Mini) |
| **Claude Code / Cowork** | Subagent delegation; **Plan mode** (read-only) + **Build mode** (full file ops); 1M-context; mailbox + file-locking Agent Teams |
| **Microsoft Scout** | 3-tier permission matrix; **Work IQ** always-on background intelligence layer; proactively flags schedule conflicts; governed Entra identity |
| **Taskade Genesis** | Workspace DNA; 500K+ deployed agents; 100K+ live apps; 11+ AI models per agent |
| **OpenCode** | Plan/Build mode separation (review before execution); 75+ LLM providers; LSP integration; **160K GitHub stars, 7.5M monthly devs** |
| **Devin 3.0** (Cognition Labs) | **Dynamic re-planning** on failures (self-healing); **self-maintained knowledge wiki**; live architectural diagrams; ARR $1M→$73M |
| **Khoj** | Second-brain semantic search; Obsidian/Notion/GitHub indexing; offline-capable; MIT licensed |
| **AnythingLLM** | 100% local RAG; Model Router; hybrid AI; desktop app with no Docker knowledge required |
| **Moltis** | Rust server; session branching; hot-reload; Pi-inspired self-extension; STT/TTS built-in; MCP-native |
| **Vellum** | BYOK (Bring Your Own Keys); credential process-boundary isolation (model never accesses creds); 8-type memory model |
| **QwenPaw / ReMe** | **88.78% HaluMem QA**, **94.06% memory accuracy**; BM25 + vector hybrid; dynamic compaction; Daemon Agent for health monitoring |
| **SuperAGI** | Concurrent multi-agent workflows; visual GUI agent management; role-based task splitting; token-budget controls |
| **Adept (ACT-1)** | Action Transformer: trained on human-computer interaction (not text); pioneered "action model" category |
| **Rabbit (DLAM)** | USB-attached hardware controller; no host software install on target machine; LAM interface-abstraction (resilient to API changes) |
| **MimiClaw** | AI agent on ESP32-S3 ($10, ~0.5W); full ReAct loop on bare metal; GPIO hardware control |
| **Inflection Pi** | Emotional/affective memory; 33-min avg conversation (10× competitors); 10M+ empathy fine-tuning samples |
| **Humane (CosmOS)** | Wearable-first, offline-capable, sensor-rich context ingestion |

---

## 🌟 Core Design Philosophy

Hydragent is built on **seven foundational axioms** distilled from the collective intelligence of 40+ agents. The seventh (📚 *Workspaces are Connected Knowledge Graphs*) is the only one that is **purely Hydragent-native** — the first six are borrowed from the systems listed in the table below.

### 1. 🧠 The Agent Must Grow
Borrowed from **Hermes Agent** — the only agent with a built-in learning loop. Hydragent creates skills from completed executions, improves them during reuse, and runs an autonomous *Curator* on a 7-day cycle that grades, consolidates, and prunes the skill library. Every interaction makes the agent measurably more capable.

### 2. 🗄️ Memory is a File System, Not a Flat Database
Borrowed from **memU** — memories are organized like a file system with Folders (auto-categorized topics), Files (discrete Markdown fact-items), and Mount Points (indexed external documents). Retrieval uses a dual-mode engine: a cheap embedding pass for ambient monitoring, escalating to frontier model reasoning only on high-signal queries.

### 3. 🔒 Secrets Must Never Touch the LLM
Borrowed from **IronClaw** — every API key, OAuth token, and credential lives inside an XChaCha20-Poly1305 + Argon2id encrypted vault. The orchestrator speaks only in header placeholders (`Authorization: Bearer {{GITHUB_TOKEN}}`); the dispatcher performs key injection at the network boundary, then immediately zeroizes memory. The model never processes raw credentials — *ever*.

### 4. 🏗️ Every Dangerous Action Runs in a Cage
Borrowed from **NanoClaw + Manus** — code execution, browser automation, shell commands, and third-party tool calls run inside Docker containers with filesystem isolation and WASM runtimes with zero socket access. Users see a three-tier permission gate (Auto-approve / Prompt / Deny) for every state-mutating action.

### 5. 🌐 One Agent, Every Channel
Borrowed from **OpenClaw + ZeroClaw** — a single runtime communicates across 40+ adapters: Telegram, Discord, WhatsApp, Slack, Signal, iMessage, Matrix, email, voice, webhooks, and CLI. The agent lives where you already are.

### 6. 🤝 Complex Work Spawns Specialist Swarms
Borrowed from **Claude Code + Taskade** — long-horizon tasks decompose into a Directed Acyclic Graph and spawn specialist subagents (Plan, Build, Explore, Scout) with scoped system prompts, individual tool permissions, and independent context windows. A Model Council routes each step to the best-fit LLM.

### 7. 📚 Workspaces are Connected Knowledge Graphs
Hydragent structures user sessions and workspaces as an interconnected **Library Knowledge Graph** (Shelves, Books, Pages, and Desks) rather than flat room directories. Users can create, link, and manipulate nodes with custom directed edges. The entire knowledge graph is compiled on-the-fly and rendered interactively inside the Telegram Mini App dashboard as a D3.js force-directed map.

---

## 🗂️ Repository Structure

```text
hydragent/
├── README.md                   # Project overview (this file)
├── ONBOARDING.md               # 🐣 Zero-to-first-chat guide (read this first)
├── CONTRIBUTING.md             # 🛠 Code conventions + where to add a tool/skill/channel
├── CHANGELOG.md                # Version history (Keep-a-Changelog format)
├── Cargo.toml                  # Rust workspace manifest (16 crates)
├── Cross.toml                  # Cross-compile targets (ARM64, RISC-V)
├── Hydragent.cmd               # 🪟 Windows single-entry point (install/onboard/chat/doctor)
│
├── doc/                        # All design, architecture, and process docs
│   ├── ARCHITECTURE.md         # Technical specification, layers, and API schemas
│   ├── ROADMAP.md              # Phased milestones and implementation timeline
│   ├── STATE.md                # ⚡ Ground truth: what is actually in the code
│   ├── FEATURES.md             # Comprehensive feature matrix & capability catalog
│   ├── RaD/                    # Research & Development source materials
│   │   ├── gemini.md           # Deep technical R&D (primary source)
│   │   ├── chatgpt.md          # Comparative agent analysis
│   │   └── ...
│   ├── phases/                 # Per-phase implementation retrospectives
│   ├── releases/               # Current & historical release notes (v0.7.x)
│   └── archive/                # Archived phase reports & older release notes
│
├── crates/                     # Rust Multi-Crate Workspace (16 crates)
│   ├── hydragent-core/         # Main orchestrator binary & react loop
│   ├── hydragent-types/        # Shared system types and events
│   ├── hydragent-bus/          # Event bus & API router
│   ├── hydragent-model/        # Model Router + 20+ profiles in Model Council
│   ├── hydragent-tools/        # ReAct tools registry & implementations
│   ├── hydragent-memory/       # Hierarchical database memory layer (SQLite, hybrid search)
│   ├── hydragent-embed/        # Vector embedding utilities
│   ├── hydragent-vault/        # Encrypted credential storage (XChaCha20-Poly1305)
│   ├── hydragent-sandbox/      # WASM sandbox execution boundary
│   ├── hydragent-gateway/      # Inbound channel deduplication and rate limiting
│   ├── hydragent-scheduler/    # Cron scheduler, Heartbeat engine & Work IQ
│   ├── hydragent-security/     # 16-layer security: Merkle audit, taint tracker, injection guard
│   ├── hydragent-planner/      # DAG-based task decomposition & complexity classifier
│   ├── hydragent-swarm/        # Subagent swarm runtime (up to 300 sub-agents)
│   ├── hydragent-skills/       # Self-improving skill engine + 7-day Curator
│   └── hydragent-bench/        # SKILL-BENCH eval harness + golden sets
│
├── adapters/                   # Python channel adapters (Telegram, Slack, Discord, Webhooks)
├── config/
│   ├── SOUL.md                 # Agent personality, values, and behavioral guidelines
│   ├── USER.md                 # User profile, preferences, and memory seed
│   └── model_council.yaml      # 20+ model profiles for the Model Council
│
├── data/                       # Local data: skill_library.sqlite, vault, ML models
├── tests/                      # Smoke tests + SKILL-BENCH (80 tasks) + golden set (30 pairs)
├── skills/builtin/             # 3 shipped skills: csv-to-json, summarize-issue, debug-rust
├── tools/finetune/             # Python LoRA fine-tuning pipeline (Gemma 2 2B)
│
├── .env.example                # Environment configuration template
└── LICENSE                     # MIT License
```

---

## 🏗️ 7-Layer Architecture Overview

Hydragent's runtime is composed of seven decoupled layers communicating over a gRPC/HTTP2 event bus. The **Rust core** (Tokio async) handles orchestration, security, and tool dispatch. An optional **Zig edge binary** (≤678 KB, <2 ms startup) targets RISC-V/ESP32-S3. **Python adapters** handle channels, RAG pipelines, and ML glue:

```
┌──────────────────────────────────────────────────────────┐
│ 1. Channel Gateway  [Telegram | Discord | Web | CLI | …]  │
└───────────────────────────────┬──────────────────────────┘
                                │  JSON-RPC Event Payload
┌──────────────────────────────────────────────────────────┐
│ 2. Event Bus & API Router  [JSON-RPC over TCP socket]    │
└───────────────────────────────┬──────────────────────────┘
                                │  Dispatched Task
┌───────────────────────────────▼──────────────────────────┐
│ 3. Core Orchestrator  [DAG Planner + ReAct Execution]      │
└───────────┬───────────────────┬──────────────────────────┘
            │                   │
┌───────────▼──────────┐ ┌──────▼──────────────────────────┐
│ 4. Memory Layer       │ │ 5. Model Router + Skill Engine   │
│  Episodic  (SQLite)   │ │  OpenRouter  /  Local Ollama     │
│  Semantic  (vectors)  │ │  Dynamic Model Council           │
│  Procedural(Skills)   │ │  20+ model specialist pool       │
│  Emotional (Profile)  │ │  + Skill Library (FTS5+Curator)  │
└───────────┬──────────┘
            │
┌───────────▼──────────────────────────────────────────────┐
│ 6. Tool Dispatcher & Security Vault  [Key injection]       │
└───────────┬──────────────────────────────────────────────┘
            │  Scoped Permissions + TEE Isolation
┌───────────▼──────────────────────────────────────────────┐
│ 7. Execution Sandbox  [WASM runtimes + Docker + MCP]       │
└──────────────────────────────────────────────────────────┘
```

For the full technical specification, interface contracts, and API schemas → **[ARCHITECTURE.md](ARCHITECTURE.md)**

> **Layer 5 also hosts the Skill Engine** — Hermes-style deterministic skill
> inducer, Skill Library with FTS5 + tag retrieval, 7-day Curator (promotes
> ≥ 0.7 success over ≥ 10 runs), and Skill Composer. Shipped in v0.7.0.
> See [Current State](#-current-state-v070--2026-06-14) above.

---

## 🚀 Getting Started

> **First time?** Read **[`ONBOARDING.md`](ONBOARDING.md)** — it walks you
> from `git clone` to first chat in ~10 minutes and explains the
> `Hydragent.cmd` entry point. The abbreviated version is below.

### TL;DR (Windows, 4 commands)

```powershell
git clone https://github.com/joker0210G/Hydragent.git
cd hydragent
.\Hydragent.cmd install      # one-time: Rust + MinGW + build
.\Hydragent.cmd onboard      # guided .env wizard
.\Hydragent.cmd chat         # start chatting
```

`Hydragent.cmd` is the **only entry point you need to remember**. It
auto-detects what state you're in (no Rust yet? missing MinGW? no
binary built? no `.env`?) and runs the right thing. The explicit
subcommands `install | onboard | chat | doctor | examples` are also
available.

### Prerequisites

| Prereq | Why | Required? |
|---|---|---|
| **Rust ≥ 1.78** | Builds the kernel | ✅ |
| **MinGW-w64 at `C:\mingw64`** | `dlltool.exe` for some build steps | ✅ |
| **Python ≥ 3.11** | Channel adapters + the Python SDK | ✅ for adapters, ❌ for chat |
| **An LLM endpoint** | The "brain" — any OpenAI-compatible `/v1/chat/completions` | ✅ |
| **Docker** | Not implemented (Phase 3 sandbox is Wasmtime-only — see [`doc/STATE.md`](doc/STATE.md)) | ❌ |
| **Zig 0.13+** | Only for the RISC-V / ESP32-S3 edge binary | ❌ |

`Hydragent.cmd install` downloads and wires up Rust + MinGW for you;
the only thing you need to bring is a LLM API key (or a local
Ollama / LM Studio server).

### 🐣 First-time setup (90 seconds)

The `Hydragent.cmd` flow above handles everything. If you prefer
explicit `cargo` commands:

```powershell
# 1. Build (first build = 2-5 minutes)
cargo build -p hydragent-core

# 2. Guided onboarding (writes a .env with BRAIN_BASE / BRAIN_KEY / BRAIN_MODEL)
.\target\debug\hydragent.exe onboard

# 3. Verify the live brain end-to-end
.\target\debug\hydragent.exe test-brain

# 4. Start chatting in the terminal
.\target\debug\hydragent.exe chat
```

`hydragent onboard` walks you through:

1. **Picking a provider** — arrow-key navigable picker (↑/↓/Enter, or type a digit to quick-select, `q` to cancel). On a non-TTY (piped/CI) it falls back to a plain number prompt.
2. **Pasting your API key** (input is masked with `rpassword`; leave empty for local Ollama / LM Studio).
3. **Picking a primary model** — same arrow-key picker, with a `custom` entry at the end.
4. **Saving a `.env`** that preserves any existing keys you already had.
5. **Running `test-brain`** to confirm the connection works before you commit.

Non-interactive mode for CI / scripts:

```powershell
.\target\debug\hydragent.exe onboard `
  --provider openrouter `
  --api-key "$env:OPENROUTER_API_KEY" `
  --model openai/gpt-4o-mini `
  --non-interactive --no-verify
```

### 🩺 Diagnose an existing setup

```powershell
.\Hydragent.cmd doctor
# or
.\target\debug\hydragent.exe doctor
```

Runs ~10 file-based checks (no network calls) and prints a colour-coded report:

| Symbol | Meaning |
|---|---|
| ✅ green | Check passed |
| ⚠️ yellow | Warning, not blocking |
| ❌ red | Failure — every red check prints a one-line fix hint |

Exits non-zero only if any check fails, so it's safe to wire into CI.

### 💡 Discover what's possible

```powershell
.\target\debug\hydragent.exe examples            # full catalogue
.\target\debug\hydragent.exe examples memory     # filter by tool name
```

Each example shows a copy-pasteable prompt, the tools it exercises, and a short note about what to expect.

### ⚙️ Configuration (manual)

If you prefer to hand-craft `.env`, only the **BRAIN_\*** keys are required
to chat. The vault passphrase and channel tokens are optional until you
use those features.

```ini
# .env — credentials stay here, never committed.
# Only the BRAIN_* block is required for first chat.

# ── The Brain (the agent's single live LLM) ───────────────────────
BRAIN_BASE=https://openrouter.ai/api/v1     # any OpenAI-compatible /v1 URL
BRAIN_KEY=sk-or-...                         # your API key (empty for local Ollama / LM Studio)
BRAIN_MODEL=openai/gpt-4o-mini              # primary model
BRAIN_FALLBACKS=openai/gpt-4o,anthropic/claude-3-haiku  # tried in order on failure

# ── Optional: encrypted vault (Phase 3) ───────────────────────────
# HYDRAGENT_VAULT_PASSPHRASE=<your-secure-local-passphrase>

# ── Optional: chat platform tokens (only if you run those adapters) ─
# TELEGRAM_BOT_TOKEN=...
# TELEGRAM_ALLOWED_CHAT_IDS=123456789,987654321
# DISCORD_BOT_TOKEN=...
# SLACK_BOT_TOKEN=...
# See .env.example for the full list.
```

> **Legacy aliases** (still supported): `OPENROUTER_API_KEYS` ↔
> `BRAIN_KEY`, `PRIMARY_MODEL` ↔ `BRAIN_MODEL`, `FALLBACK_MODELS` ↔
> `BRAIN_FALLBACKS`. The new `BRAIN_*` names are preferred and used
> by `hydragent onboard`. If both are set, `BRAIN_*` wins.
>
> **Common pitfall**: don't put a trailing slash on `BRAIN_BASE`. Use
> `https://x.y/v1`, not `https://x.y/v1/`. The kernel appends
> `/chat/completions` itself.

### Run

```powershell
# Option A — interactive terminal chat (REPL, no adapter needed)
.\Hydragent.cmd chat
# or
.\target\debug\hydragent.exe chat

# Option B — start the bus server so adapters (Telegram / Discord / Slack / CLI) can connect
.\target\debug\hydragent.exe                    # no subcommand = bus server
python adapters\cli_adapter.py                  # then in another shell

# Option C — from Python, via the official SDK
pip install -e adapters\
python -c "from hydragent_py import HydraClient; print(HydraClient.connect().__enter__().chat('Hello!'))"
```

Inside `hydragent chat`, type `/help` for a list of slash commands
(`/model`, `/memory list`, `/clear`, `/reasoning`, …). For the full
walkthrough of the REPL plus the reasoning-trace toggle, see
[`ONBOARDING.md` §3](ONBOARDING.md#3-the-4-step-first-run).

> **Coming back to add code?** See [`CONTRIBUTING.md`](CONTRIBUTING.md)
> for code conventions and the exact files to touch when you add a
> tool, skill, or channel adapter.

---

## 📊 Agent Benchmark Context

Hydragent draws design targets from real-world agent performance benchmarks:

| Benchmark | Target | Inspiration |
|---|---|---|
| HaluMem QA accuracy | ≥ 88.78% | QwenPaw ReMe compaction |
| Memory accuracy (HaluMem) | ≥ 94.06% | QwenPaw ReMe memory accuracy score |
| Locomo benchmark accuracy | ≥ 92.09% | memU proactive memory |
| Complex workflow completion | ≥ 80% | Perplexity Computer |
| GAIA benchmark | ≥ 65% | Manus (vs GPT-4o 32%) |
| SWE-bench Pro (code) | ≥ 58.6% | Kimi K2.6 Agent Swarm |
| Rust core startup latency | < 50 ms | ZeroClaw / NullClaw |
| Edge binary startup | < 2 ms | NullClaw Zig 678 KB |
| Edge binary footprint | < 1 MB RAM | NullClaw Zig binary (678 KB) |
| Context window | 1M tokens | Claude Code / Qwen flagship |
| Edge device operation | $10 board | PicoClaw / MimiClaw |
| Adversarial resilience | Best-in-class | IronClaw NEAR AI evaluation |

---

## 🧭 Capability Overview

| Category | Capability | Sources |
|---|---|---|
| **Memory** | 8-type hierarchical memory (episodic, semantic, procedural, emotional, spatial, social, temporal, declarative) | Vellum, memU, Hermes |
| **Self-improvement** | Autonomous skill authoring and 7-day Curator pruning cycle | Hermes, OpenClaw |
| **Security** | 16-layer cryptographic pipeline + TEE enclaves | OpenFang, IronClaw, NEAR AI |
| **Execution** | Docker + WASM sandboxed tool runtime | NanoClaw, Manus, IronClaw |
| **Multi-model** | Dynamic routing across 20+ model profiles (`config/model_council.yaml`) | Perplexity Computer, OpenRouter |
| **Channels** | 40+ platform adapters | OpenClaw, ZeroClaw, QwenPaw |
| **Personalization** | SOUL.md / USER.md persona seeding; affective memory | OpenClaw, MimiClaw, Inflection Pi |
| **Orchestration** | DAG subagent swarms with Model Council | Claude Code, Taskade, SuperAGI |
| **RAG** | Hybrid BM25 + vector semantic search over private docs | Khoj, AnythingLLM, QwenPaw |
| **Edge deployment** | RISC-V / ESP32 binary support; quantized 4-bit inference | MimiClaw, PicoClaw, NullClaw |
| **Human-in-loop** | Consent gates, Takeover Mode, audit trails | Microsoft Scout, Devin, IronClaw |
| **Evaluation** | Built-in multi-layer evaluation harness | AWS Bedrock AgentCore, SuperAGI |

For detailed capability breakdowns → **[FEATURES.md](FEATURES.md)**

---

## 📅 Development Roadmap (Summary)

| Phase | Weeks | Deliverable |
|---|---|---|
| Phase 1 | 1–6 | Rust core runtime (Tokio), JSON-RPC event bus, OpenRouter integration, CLI adapter |
| Phase 2 | 7–10 | Hierarchical memory (memU-style), BM25 + vector hybrid, nightly Dreaming pipeline, Standing Orders |
| Phase 3 | 11–14 | WASM sandbox, 3-tier permission matrix (Scout-style), encrypted vault (IronClaw-style) |
| Phase 4 | 15–18 | 40+ channel gateway; proactive heartbeat; cron daemon; Work IQ background awareness |
| Phase 5 | 19–22 | Kimi-style agent swarm (DAG + 300 sub-agent capacity), Model Council routing (20+ models), self-healing re-planner |
| Phase 6 | 23–26 | 16-layer security pipeline: Merkle audit, taint tracking, SGNL integration, Ed25519 signing |
| Phase 7 | 27–30 | Hermes-style self-improving skill engine, 7-day Curator, Gene Evolution Protocol — **✅ Shipped 2026-06-14 (v0.7.0, 567 tests)** |
| Phase 8 | 31+ | Edge hardware port (RISC-V/ESP32-S3 Zig binary), PicoLM local inference, offline-first, swarm tool registry, SKILL-BENCH ReAct agent — **🚧 Planned** |

Full milestone details → **[ROADMAP.md](ROADMAP.md)**

---

## 📄 License

Hydragent is open-source software licensed under the **MIT License**. See [LICENSE](LICENSE).

---

## 🌐 Acknowledgements

Hydragent stands on the shoulders of the open-source agent community. Core design inspiration drawn from: **Hermes Agent** (Nous Research), **OpenClaw** (PSPDFKit / Peter Steinberger), **ZeroClaw Labs**, **NanoClaw** (Gavriel Cohen / Docker), **IronClaw** (NEAR AI), **memU** (NevaMind AI), **Moltis**, **Khoj**, **AnythingLLM** (Mintplex Labs), **TrustClaw** (ComposioHQ), **QwenPaw** / **Kimi K2.6** (Alibaba / Moonshot AI), **PicoClaw** (Sipeed), **MimiClaw**, **GoClaw**, **OpenCode**, **Devin** (Cognition Labs), **Microsoft Scout**, **Manus AI**, **Perplexity Computer**, and all the others listed in the capability matrix above.
