# Hydragent 🐉 — The Unified AI Agent

> *The agent that grows, remembers, executes, and protects — synthesizing the architectural DNA of 40+ frontier AI systems into one coherent, privacy-first, model-agnostic runtime.*

[![Status: Active Design](https://img.shields.io/badge/Status-Active%20Design-blue)](#roadmap)
[![License: MIT](https://img.shields.io/badge/License-MIT-green)](LICENSE)
[![Runtime: Zig / Rust](https://img.shields.io/badge/Runtime-Zig%20%2F%20Rust-orange)](#architecture)
[![Security: 16-Layer](https://img.shields.io/badge/Security-16--Layer%20Cryptographic-red)](#security)

---

## 🌊 What is Hydragent?

Hydragent is a **next-generation, modular meta-agent** that synthesizes the best architectural decisions from the 2026 AI agent landscape into a single, coherent, privacy-first runtime. Rather than picking one agent to copy, Hydragent extracts the *design DNA* from each major system — then reimagines it as a deeply integrated whole.

**The core insight**: Every great agent of 2026 solved *one* problem brilliantly. Hydragent solves them *all simultaneously*:

| System Analyzed | Innovation Extracted |
|---|---|
| **Hermes Agent** (Nous Research) | Closed-loop self-improving skill engine; 7-day autonomous curator cycle |
| **memU** | Hierarchical file-system memory layout; 10x token cost reduction via dual-mode retrieval |
| **OpenFang** | 16-layer cryptographic security with Merkle audit trails and TEE execution |
| **IronClaw** | WASM capability sandboxing; boundary key injection (secrets never touch LLM) |
| **NanoClaw** | Container-isolated agents (~4k LoC auditable core); Agent Swarm coordination |
| **ZeroClaw** | Single Rust binary runtime; multi-crate workspace decomposition; <5 MB footprint |
| **NullClaw** | Ultra-lightweight Zig static binary (678 KB); SQLite-native hybrid (vector + BM25) |
| **PicoClaw** | Gene Evolution Protocol for self-adapting monitoring strategies on $10 hardware |
| **Manus** | VM-sandboxed parallel task execution; autonomous web dev from single prompt |
| **Perplexity Computer** | 19-model orchestration; Opus 4.6 core reasoning + specialist routing |
| **Claude Code / Cowork** | Subagent delegation with scoped system prompts; 1M-context session management |
| **Microsoft Scout** | 3-tier permission matrix (Auto-approve / Prompt / Deny); governed digital identity |
| **OpenClaw** | Multi-channel gateway (40+ platforms); ClawHub skill ecosystem; cron-proactive tasks |
| **Taskade Genesis** | Workspace DNA for multi-agent teams; no-code agent builder; real-time collaboration |
| **Khoj** | Second-brain semantic search across personal documents; multi-platform access |
| **AnythingLLM** | 100% local RAG over private docs; Model Router for hybrid AI |
| **Moltis** | Secure Rust server with built-in STT/TTS; MCP-native; no-external-key-exposure |
| **TrustClaw** | OAuth-only credential brokering (Composio); 1000+ skill marketplace; zero-config keys |
| **Vellum** | 8-type layered memory model: episodic, semantic, procedural, emotional, spatial, and more |
| **QwenPaw / ReMe** | Proactive "growing" memory; BM25 + vector hybrid scoring 88.78% on HaluMem QA |
| **SuperAGI** | Concurrent multi-agent workflows; built-in telemetry and token-budget controls |
| **Devin (Cognition Labs)** | Self-healing re-planning on compile/execution errors; full dev environment control |
| **Adept (ACT-1)** | Vision + code-execution chain-of-thought foundation |
| **Rabbit (DLAM)** | Hardware-agnostic controller via USB; plug-and-play without host software install |
| **MimiClaw** | AI agent on ESP32-S3 ($10, ~0.5W); on-device inference at microcontroller scale |
| **Inflection Pi** | Emotional/affective memory modeling; tone-adaptive personalization |
| **Humane (CosmOS)** | Wearable-first, offline-capable, sensor-rich context ingestion |

---

## 🌟 Core Design Philosophy

Hydragent is built on **six foundational axioms** distilled from the collective intelligence of 40+ agents:

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

---

## 🗂️ Repository Structure

```text
hydragent/
├── RaD/                        # Research & Development source materials
│   ├── gemini.md               # Deep technical R&D (primary source)
│   ├── chatgpt.md              # Comparative agent analysis
│   ├── deepseek.md             # Agent landscape & framework comparison
│   ├── perplexity.md           # Agent feature catalog
│   └── kimi.md                 # Additional agent R&D notes
│
├── README.md                   # Project overview (this file)
├── FEATURES.md                 # Comprehensive feature matrix & capability catalog
├── ARCHITECTURE.md             # Technical specification, layers, and API schemas
├── ROADMAP.md                  # Phased milestones and implementation timeline
│
├── src/
│   ├── core/                   # Orchestrator, DAG planner, ReAct loop
│   ├── memory/                 # Hierarchical memory, SQLite, ChromaDB, BM25
│   ├── gateway/                # Channel adapters (Telegram, Discord, Slack, CLI...)
│   ├── security/               # Vault, WASM sandbox, TEE integration, taint tracker
│   ├── tools/                  # Browser bot, code sandbox, MCP, file I/O, search
│   ├── swarm/                  # Subagent spawning, Model Council, DAG coordinator
│   └── skills/                 # Self-generated skill library, Curator process
│
├── config/
│   ├── SOUL.md                 # Agent personality, values, and behavioral guidelines
│   ├── USER.md                 # User profile, preferences, and memory seed
│   └── agents/                 # Subagent role definitions and system prompts
│
├── .env.example                # Environment configuration template
└── LICENSE                     # MIT License
```

---

## 🏗️ 7-Layer Architecture Overview

Hydragent's runtime is composed of seven decoupled layers communicating over a gRPC/HTTP2 event bus. The core compiles as a **hyper-optimized Zig static binary** (<1 MB RAM, 678 KB on disk, <2 ms startup):

```
┌──────────────────────────────────────────────────────────┐
│ 1. Channel Gateway  [Telegram | Discord | Web | CLI | …]  │
└───────────────────────────────┬──────────────────────────┘
                                │  JSON-RPC Event Payload
┌───────────────────────────────▼──────────────────────────┐
│ 2. Event Bus & API Router  [gRPC / HTTP2 Message Bus]      │
└───────────────────────────────┬──────────────────────────┘
                                │  Dispatched Task
┌───────────────────────────────▼──────────────────────────┐
│ 3. Core Orchestrator  [DAG Planner + ReAct Execution]      │
└───────────┬───────────────────┬──────────────────────────┘
            │                   │
┌───────────▼──────────┐ ┌──────▼──────────────────────────┐
│ 4. Memory Layer       │ │ 5. Model Router                  │
│  Episodic  (SQLite)   │ │  OpenRouter  /  Local Ollama     │
│  Semantic  (ChromaDB) │ │  Dynamic Model Council           │
│  Procedural(Skills)   │ │  19+ model specialist pool       │
│  Emotional (Profile)  │ └─────────────────────────────────┘
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

---

## 🚀 Getting Started (Planned MVP)

### Prerequisites

- **Zig 0.13+** or **Rust 1.78+** (for core runtime build)
- **Docker** (for execution sandbox isolation)
- **Node.js v22+** (for gateway bridge / channel adapters)
- **An LLM backend**: OpenRouter API key *or* local Ollama instance (Llama 3, Qwen 2.5, Mistral)

### Quick Install (MVP Shell)

```bash
# Clone repository
git clone https://github.com/your-org/hydragent.git
cd hydragent

# Install Node.js channel adapter dependencies
npm install

# Build the Zig core runtime
zig build -Doptimize=ReleaseFast

# Bootstrap the encrypted credential vault
./hydragent vault init

# Configure your agent persona
cp config/SOUL.md.example config/SOUL.md
cp config/USER.md.example config/USER.md
```

### Configuration

```ini
# .env — credentials stay here, never committed
OPENROUTER_API_KEY=sk-or-...
LOCAL_OLLAMA_URL=http://localhost:11434
DATA_DIR=./data
VAULT_PASSPHRASE=<your-secure-local-passphrase>
TELEGRAM_BOT_TOKEN=...
```

### Run

```bash
# Start the full runtime (gateway + orchestrator + memory)
./hydragent start

# Or in lightweight edge mode (no Docker, local model only)
./hydragent start --edge --model=tinyllama
```

---

## 📊 Agent Benchmark Context

Hydragent draws design targets from real-world agent performance benchmarks:

| Benchmark | Target | Inspiration |
|---|---|---|
| HaluMem QA memory accuracy | ≥ 88.78% | QwenPaw ReMe compaction |
| Complex workflow completion | ≥ 80% | Perplexity Computer |
| Startup latency | < 2 ms | ZeroClaw / NullClaw |
| Binary footprint | < 1 MB RAM | NullClaw Zig binary (678 KB) |
| Context window | 1M tokens | Claude Code session management |
| Edge device operation | $10 board | PicoClaw / MimiClaw |

---

## 🧭 Capability Overview

| Category | Capability | Sources |
|---|---|---|
| **Memory** | 8-type hierarchical memory (episodic, semantic, procedural, emotional, spatial, social, temporal, declarative) | Vellum, memU, Hermes |
| **Self-improvement** | Autonomous skill authoring and 7-day Curator pruning cycle | Hermes, OpenClaw |
| **Security** | 16-layer cryptographic pipeline + TEE enclaves | OpenFang, IronClaw, NEAR AI |
| **Execution** | Docker + WASM sandboxed tool runtime | NanoClaw, Manus, IronClaw |
| **Multi-model** | Dynamic routing across 19+ models | Perplexity Computer, OpenRouter |
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
| Phase 1 | 1–6 | Core Zig runtime, gRPC event bus, OpenRouter integration |
| Phase 2 | 7–10 | Hierarchical memory, BM25 engine, nightly Dreaming pipeline |
| Phase 3 | 11–14 | WASM sandbox, 3-tier permissions, encrypted vault |
| Phase 4 | 15–18 | Subagent swarm orchestration, Model Council, self-healing |
| Phase 5 | 19–22 | 16-layer security, Merkle audit, taint tracking, SGNL integration |
| Phase 6 | 23+ | Edge hardware port (RISC-V), local inference, Gene Evolution Protocol |

Full milestone details → **[ROADMAP.md](ROADMAP.md)**

---

## 📄 License

Hydragent is open-source software licensed under the **MIT License**. See [LICENSE](LICENSE).

---

## 🌐 Acknowledgements

Hydragent stands on the shoulders of the open-source agent community. Core design inspiration drawn from: **Hermes Agent** (Nous Research), **OpenClaw** (PSPDFKit / Peter Steinberger), **ZeroClaw Labs**, **NanoClaw** (Gavriel Cohen / Docker), **IronClaw** (NEAR AI), **memU** (NevaMind AI), **Moltis**, **Khoj**, **AnythingLLM** (Mintplex Labs), **TrustClaw** (ComposioHQ), **QwenPaw** (Alibaba / AgentScope), **PicoClaw** (Sipeed), **MimiClaw**, and all the others listed in the capability matrix above.
