# Hydragent: Feature Matrix & Capability Catalog

> A comprehensive breakdown of every capability in the **Hydragent Unified AI Agent**, with the source agent each feature was distilled from and the technical implementation approach.

> **⚠️ This file is the *capability catalog* — what the system is *designed* to do.** It is **not** a status report.
> Several features listed here (ChromaDB semantic store, Dreaming pipeline, Wasmtime/Docker sandboxes, WhatsApp/Signal/Matrix/voice channels, Model Council, 16-layer security, Merkle audit, self-improving skill engine, edge model runner, ClawHub marketplace, RISC-V / ESP32-S3 binary, full Daemon Agent observability, etc.) are **planned but not yet present in the code**.
> For what is *really* shipped in the working tree, see **[`STATE.md`](STATE.md)**.

---

## Table of Contents

1. [Memory & Deep Personalization](#1-memory--deep-personalization)
2. [Security & Data Governance](#2-security--data-governance)
3. [Unified Multi-Channel Gateway](#3-unified-multi-channel-gateway)
4. [Execution Sandbox & Tool Orchestration](#4-execution-sandbox--tool-orchestration)
5. [Multi-Agent Swarm Orchestration](#5-multi-agent-swarm-orchestration)
6. [Self-Improving Skill Engine](#6-self-improving-skill-engine)
7. [Personalization & User Modeling](#7-personalization--user-modeling)
8. [Multimodal Input & Sensing](#8-multimodal-input--sensing)
9. [Multi-Model Council & Routing](#9-multi-model-council--routing)
10. [RAG & Knowledge Base Integration](#10-rag--knowledge-base-integration)
11. [Human-in-the-Loop & Ethics Controls](#11-human-in-the-loop--ethics-controls)
12. [Edge & Embedded Deployment](#12-edge--embedded-deployment)
13. [Evaluation & Observability](#13-evaluation--observability)
14. [Plugin & Skill Marketplace](#14-plugin--skill-marketplace)
15. [Lifecycle Management](#15-lifecycle-management)

---

## 1. Memory & Deep Personalization

Hydragent's memory system is its most differentiating layer — a multi-tier, self-compacting knowledge store that fuses design patterns from **memU**, **Vellum**, **Hermes Agent**, **QwenPaw**, **OpenClaw**, **AnythingLLM**, and **Khoj**.

### 1.1 Eight-Type Hierarchical Memory Model *(from Vellum + memU)*

Rather than a single flat vector database, Hydragent implements a full taxonomy of memory types, each with its own storage backend, decay function, and retrieval pathway:

| Memory Type | Description | Storage Backend | Decay Policy |
|---|---|---|---|
| **Episodic** | Timestamped conversation logs, daily activity journals | SQLite (append-only WAL) | Compressed after 7 days via Dreaming pipeline |
| **Semantic** | Fact-nodes about the world, user's domain knowledge | SQLite `semantic_memories` + in-house `VectorStore` (linear scan, `vectors.bin`) | Relevance-score weighted; **LRU eviction NOT wired** (table grows unbounded) |
| **Procedural** | Learned skill programs, tool execution patterns | Markdown skill files (`skills/`) | Graded on 7-day Curator cycle |
| **Emotional / Affective** | User sentiment history, tone preferences, comfort zones | SQLite profile table | Rolling 30-day window |
| **Social** | Relationship graph — contacts, organizations, interaction history | SQLite graph schema | Persistent; manually curated |
| **Spatial** | Locations, environments, geofenced contexts | GeoJSON flat files | Session-scoped unless explicitly saved |
| **Temporal** | Recurring patterns, scheduled intents, circadian preferences | Cron-style YAML patterns | Active until user revokes |
| **Declarative** | Explicit user instructions: "always respond in bullet points" | `USER.md` / `SOUL.md` Markdown files | Permanent until overwritten |

### 1.2 Hierarchical File-System Layout *(from memU)*

Memory is addressed like a file system, not a relational database. This allows both human-readable inspection and efficient LLM-native access:

```
memory/
├── user/
│   ├── preferences.md       # Declarative: tone, format, language preferences
│   ├── relationships.md     # Social: named contacts and relationship context
│   └── profile.json         # Structured profile seed (name, role, interests)
├── episodic/
│   ├── 2026-06-06.log       # Raw daily conversation log
│   └── 2026-06-05.summary   # Compressed summary from Dreaming pipeline
├── semantic/
│   └── vectors.bin           # In-house VectorStore (serialized HashMap, not ChromaDB)
├── skills/
│   ├── send_email.md        # Auto-generated skill: email composition
│   ├── github_pr.md         # Auto-generated skill: pull request workflow
│   └── curator.log          # Skill grading history
└── SOUL.md                  # Agent core identity, values, behavioral rules
```

### 1.3 Hybrid Retrieval Engine *(from memU + QwenPaw ReMe)*

The retrieval system is a single-mode **hybrid search** — a fast/deep path
split is documented in the design but **not implemented**. What shipped:

**Hybrid Search** — `SQLite FTS5 (BM25) exact keyword match` + **in-house
linear-scan vector index** (cosine similarity over a `HashMap<String,
Vec<f32>>`, **not ChromaDB / HNSW**) fused via Reciprocal Rank Fusion
(RRF), exposed as the `memory_search` tool AND the bus RPC `memory.search`.

> **Reality notes (2026-06-12)**:
> - The "fast phase" with `nomic-embed-text` and "< 5 ms latency" target is
>   not implemented; the shipped embedder is `all-MiniLM-L6-v2` (local,
>   ~90 MB, via Candle) and the vector path is a linear scan.
> - The "deep reasoning mode" escalation to a frontier model is not
>   implemented; the orchestrator always calls the configured `BRAIN_MODEL`.
> - The "88.78% HaluMem QA" number is the QwenPaw ReMe baseline; it has
>   **not been measured on this codebase**. See [`PHASE_2_FINAL_REPORT.md`](../archive/phases/PHASE_2_FINAL_REPORT.md).
> - Verified: G1 hybrid search returns ranked results across BM25 + vector
>   + RRF; E1 cross-session recall PASS.

### 1.4 Nightly "Dreaming" Compaction Pipeline *(from OpenClaw + memU)*

Inspired by biological memory consolidation during sleep, Hydragent runs a nightly compaction job at 02:00 AM local time:

```
1. Read today's raw episodic log
2. LLM Compaction Node:
   a. Create condensed session summary (< 500 tokens)
   b. Extract new user facts & preference signals
   c. Identify new reusable skill patterns
   d. Link related memories (associative strengthening)
3. Update episodic DB with compressed summary
4. For each extracted fact:
   - If fact exists in Semantic DB → merge & update embedding (strength++)
   - If new → insert new semantic vector node
   - If contradicts known fact → surface for user review
5. Re-generate USER.md and SOUL.md with updated facts
6. Grade and prune skills library (Curator)
7. Strengthen frequently-accessed memory pathways (Hebbian-style)
8. Sleep until next heartbeat
```

### 1.5 ReMe Compaction Kit *(from QwenPaw)*

The ReMe subsystem handles the mid-session context window management problem:
- **Retention Group**: Last N raw conversation turns (configurable, default: 15)
- **Compaction Group**: Summarized historical dialogue beyond the retention window
- Dynamic split point shifts based on available context budget, preventing context-window overflow during long sessions

### 1.6 Standing Orders *(from OpenClaw)*

**Standing Orders** are persistent behavioral instructions that apply across all sessions without needing to re-prompt the agent. They live in `SOUL.md` as a persistent rule set and are loaded at agent startup before any conversation begins:

```markdown
# Standing Orders
- Always respond in bullet points for technical summaries
- Never share raw credentials or API keys
- Default to Celsius for temperatures (user preference)
- Flag any action that would send data outside the EU
- Summarize emails > 200 words before displaying
```

Distinct from `USER.md` (user profile) and from cron tasks (scheduled), Standing Orders modify the agent's *real-time behavior* on every turn. The Dreaming pipeline automatically suggests new Standing Orders based on behavioral patterns observed over 7+ days.

### 1.7 Benchmark Targets

| Metric | Target | Source |
|---|---|---|
| HaluMem QA accuracy | ≥ 88.78% | QwenPaw ReMe baseline |
| HaluMem memory accuracy | ≥ 94.06% | QwenPaw ReMe full evaluation |
| Locomo benchmark accuracy | ≥ 92.09% | memU proactive memory eval |
| Fast retrieval latency | < 5 ms | memU spec |
| Deep retrieval latency | < 300 ms | Internal target |
| Compaction token reduction | > 60% | Internal target |

---

## 2. Security & Data Governance

Hydragent operates on a **zero-trust architecture**. The cardinal rule, borrowed from **IronClaw**: *Secrets must never reach the LLM*. Every security layer is additive — if one layer fails, the next contains the damage.

### 2.1 Boundary Key Injection & Encrypted Vault *(from IronClaw + NanoClaw)*

```
Orchestrator → Dispatcher: "Call GitHub API with {{GITHUB_TOKEN}}"
                                           ↓
                              [Vault decrypts key into memory]
                                           ↓
                              Dispatcher → Network: "Authorization: Bearer ghp_abc..."
                                           ↓
                              [Key zeroized from RAM immediately after response]
```

**Vault Implementation**:
- Encryption: `XChaCha20-Poly1305` (authenticated encryption)
- Key Derivation: `Argon2id` with tunable memory cost (default: 64 MB, 3 iterations)
- Storage: Memory-mapped encrypted file (`secrets.json.enc`) — never written in plaintext
- Access: Vault process runs in a separate OS process; orchestrator communicates via Unix socket with capability tokens

### 2.2 16-Layer Cryptographic Security Pipeline *(from OpenFang)*

| Layer | Mechanism | Purpose |
|---|---|---|
| L1 | XChaCha20-Poly1305 vault encryption | Credential protection at rest |
| L2 | Argon2id key derivation | Brute-force resistance for vault passphrase |
| L3 | Ed25519 skill/plugin manifest signing | Prevent loading of tampered skill files |
| L4 | Merkle tree audit log | Immutable, tamper-evident action history |
| L5 | Taint tracking on input fields | Prompt injection detection and blocking |
| L6 | WASM capability restrictions | Zero filesystem/socket access for tool scripts |
| L7 | Docker container namespacing | OS-level agent process isolation |
| L8 | Secret zeroization after use | Prevent memory-scraping attacks |
| L9 | Network egress allowlist | Blocks unauthorized outbound connections |
| L10 | TEE hardware enclave (cloud) | Full process + memory encryption at boot |
| L11 | OAuth-only credential brokering | No raw secret storage for 3rd-party APIs |
| L12 | Workspace path scoping | LLM can only read/write within designated folder |
| L13 | Action consent gates (3-tier) | User approval before state-mutating operations |
| L14 | Session replay protection | Nonce-signed API request tokens |
| L15 | SGNL enterprise policy enforcement | Active-directory + device posture verification |
| L16 | Differential privacy in audit logs | Anonymized telemetry; no PII in log exports |

### 2.3 TEE Execution Enclaves *(from IronClaw / NEAR AI Cloud)*

For cloud deployments, the Hydragent runtime runs inside **Trusted Execution Environments (TEEs)**:
- Supported platforms: NEAR AI Cloud (SGX), AWS Nitro Enclaves, Azure Confidential Computing
- All runtime memory, keys, and process state are encrypted from boot to shutdown
- Remote attestation allows users to cryptographically verify that the correct, unmodified code is running

### 2.4 Microsoft Scout 3-Tier Permission Matrix *(from Microsoft Scout)*

Every tool invocation is classified before execution:

| Tier | Trigger Condition | Examples | User Action Required |
|---|---|---|---|
| **Auto-approve** | Read-only, no external side effects | `git status`, `ls`, `grep`, `echo` | None |
| **Prompt** | State-mutating or external communication | `npm install`, `git commit`, HTTP POST, email send | Explicit approval button |
| **Deny** | Destructive or system-critical | `rm -rf`, `sudo`, credential exposure, disk format | Blocked; user override required to unlock |

### 2.5 Data Governance & Privacy Controls

- **Local-first default**: All data stays on the user's machine unless explicitly configured otherwise
- **Opt-in telemetry**: Anonymous usage statistics only; off by default
- **Memory erasure**: `hydragent memory purge --type=episodic --before=30d` to delete data by category and age
- **GDPR compliance tooling**: Export all personal data (`hydragent export --gdpr`), full deletion capability
- **Data partitioning**: Personal vs. enterprise data stored in separate schemas; never co-mingled

### 2.6 BYOK (Bring Your Own Keys) *(from Vellum)*

For power users and enterprise deployments:
- Users supply their own API keys for supported providers (Anthropic, OpenAI, Google, Mistral, Cohere, etc.)
- Keys are stored exclusively in the local encrypted vault — never transmitted to Hydragent infrastructure
- Credential process-boundary isolation: the model process has **zero direct access** to decrypted credentials during inference; key injection happens at the network boundary in a separate OS process
- Per-provider key rotation with automated expiry detection and renewal prompts
- Multi-key failover: if a primary key is rate-limited or revoked, the next key in the rotation chain is used automatically

---

## 3. Unified Multi-Channel Gateway

A single Hydragent runtime instance communicates across **40+ channel adapters**, separating presentation from execution. Inspired by **OpenClaw** (350K+ GitHub stars), **ZeroClaw**, **NullClaw** (18+ channels), and **QwenPaw**.

### 3.1 Supported Channels

| Category | Platforms |
|---|---|
| **Messaging** | Telegram, WhatsApp, Discord, Slack, Signal, iMessage, Matrix, Mattermost |
| **Social** | Twitter/X, Reddit, LinkedIn, Bluesky, Nostr |
| **Email** | SMTP/IMAP (Gmail, Outlook, ProtonMail) |
| **Voice** | Whisper STT + Coqui TTS (local), ElevenLabs TTS (cloud) |
| **Work** | Microsoft Teams, DingTalk, Lark, QQ, WeChat Work |
| **Developer** | CLI (terminal-native), GitHub webhooks, GitLab CI, Jira |
| **Web** | Embedded web chat widget, REST webhook, WebSocket stream |
| **IoT / Hardware** | MQTT broker, GPIO serial (for edge deployments) |

### 3.2 Gateway Architecture

```
[Incoming Message: Telegram]
         │
         ▼
[Channel Adapter] ─── Normalizes to internal IntentEvent schema
         │
         ▼
[Event Bus (gRPC)] ─── Dispatches to Core Orchestrator
         │
         ▼ (Response)
[Channel Adapter] ─── Formats response for Telegram markdown
         │
         ▼
[Outgoing Message: Telegram]
```

### 3.3 Proactive Agent Mode *(from OpenClaw + memU)*

Hydragent doesn't wait to be asked. A persistent heartbeat daemon enables:
- **Cron-triggered tasks**: YAML-defined schedules (`"Every Monday 09:00 → Summarize weekly GitHub PRs"`)
- **Inbox monitoring**: Proactively scans email/Slack for high-priority items matching user-defined patterns
- **RSS/news feeds**: Polls configured feeds and surfaces relevant items based on semantic profile match
- **System monitoring**: Tracks disk, CPU, network metrics and fires alerts on anomalies
- **Dynamic cron**: The agent can create its own recurring tasks based on conversation context (`"Remind me about this next Friday"`)
- **Auth profile rotation**: Exponential backoff cooldown (1 min → 5 min → 25 min, capped at 1 hour) to prevent gateway blocking
- **Work IQ** *(from Microsoft Scout)*: An always-on background awareness layer that continuously observes user work patterns. It proactively flags schedule conflicts before meetings, surfaces relevant documents before calls, and anticipates needs based on calendar + email context — without being asked.

---

## 4. Execution Sandbox & Tool Orchestration

Hydragent provides a rich, multi-tiered tool runtime environment that gives the agent real-world agency — with every dangerous operation safely caged.

### 4.1 Headless Browser Automation *(from Claude Computer Use + Manus)*

- **Engine**: Playwright in a dedicated Docker container with no host filesystem mount
- **Vision grounding**: Takes screenshots after each action; GPT-4o Vision / Claude Vision validates the UI state before proceeding
- **Capabilities**: Form filling, web scraping, navigation, JavaScript execution in-browser
- **Takeover Mode**: If a GUI task fails or becomes ambiguous, Hydragent sends a screen capture + control link to the user for manual intervention, then resumes

### 4.2 Plan Mode vs Build Mode *(from OpenCode + Claude Code)*

Inspired by OpenCode's and Claude Code's architectural insight that *review before execution* dramatically reduces errors:

| Mode | Access | Description |
|---|---|---|
| **Plan Mode** | Read-only | Agent analyzes codebase, outlines a strategy, shows a step-by-step plan. No file writes. No tool executions. Ideal for audits and reviews. |
| **Build Mode** | Full file ops | Agent executes the approved plan: writes files, runs commands, calls APIs. Requires explicit user transition from Plan Mode. |

Users can inspect the plan before committing to execution — analogous to a `--dry-run` flag for the entire agent.

### 4.3 Local Code Sandbox *(from Devin + Manus + NanoClaw)*

```
Supported Runtimes: Python 3.12+, Node.js 22+, Bash/Zsh, Go 1.22+, Rust
Execution Engine:   Daytona / E2B-inspired container orchestration
Resource Limits:    CPU: 2 cores max, RAM: 512 MB max, Timeout: 120s
Filesystem Access:  Scoped to /workspace/{page-id}/ only
Network Access:     Allowlist-gated; blocked by default
```

### 4.4 MCP (Model Context Protocol) Integration *(from Claude Code + Moltis)*

Native compatibility with Anthropic's MCP standard:
- **MCP Resources**: Hydragent can fetch and embed external context resources (databases, docs, APIs)
- **MCP Prompt Templates**: Pre-defined prompt templates for common tasks (code review, summarization, email drafting)
- **MCP Tools**: Expose Hydragent's own tools to other MCP-compatible agents and IDEs (Cursor, Windsurf, Claude Desktop)
- **Remote MCP Servers**: Connect to community MCP servers (Linear, Notion, Stripe, Postgres, etc.)

### 4.5 Built-in Tool Library

| Tool | Description | Sandbox Level |
|---|---|---|
| `web_search` | Multi-engine search (Google, Bing, DuckDuckGo, Perplexity) | WASM (network-only, allowlisted) |
| `browser_bot` | Playwright headless browser automation | Docker (isolated) |
| `code_exec` | Python / Bash / Node.js code execution | Docker (isolated, resource-capped) |
| `file_io` | Read/write within scoped workspace folder | Host (path-restricted) |
| `email` | Send/read via SMTP/IMAP with OAuth brokering | Vault-gated |
| `calendar` | Create/read events via Google Calendar / Caldav | OAuth-gated |
| `git` | Commit, diff, push, PR operations | Auto-approve / Prompt gated |
| `http_request` | Generic HTTP calls to allowlisted domains | Network egress allowlist |
| `memory_query` | Direct semantic/episodic memory lookup | Local only |
| `skill_create` | Generate new tool skill from task description | LLM-generated, Ed25519-signed before load |
| `mcp_call` | Invoke any registered MCP server tool | Configurable per-server |
| `vision` | Analyze images / screenshots with vision model | Local WASM or API |
| `tts` / `stt` | Text-to-speech / speech-to-text transcription | Local Whisper / Coqui |

---

## 5. Multi-Agent Swarm Orchestration

For complex, long-horizon objectives, Hydragent decomposes work into a **Directed Acyclic Graph (DAG)** and distributes execution across specialist subagents. Pattern drawn from **Claude Code**, **Manus**, **Taskade Genesis**, **SuperAGI**, **Hermes Agent v0.13 Kanban release**, and **Kimi K2.6 Agent Swarm** (up to 300 parallel sub-agents).

### 5.1 Standard Subagent Roles

| Agent Role | System Prompt Scope | Tool Access | Context Window |
|---|---|---|---|
| **Plan Agent** | Read-only architect; proposes task tree | `file_read`, `memory_query`, `web_search` | Full parent context |
| **Build Agent** | Write-enabled executor; implements steps | `file_io`, `code_exec`, `git` | Isolated fresh window |
| **Explore Agent** | LSP-aware codebase navigator | `file_read`, `git`, LSP diagnostics | Isolated fresh window |
| **Scout Agent** | External documentation researcher | `web_search`, `browser_bot`, `http_request` | Isolated fresh window |
| **Review Agent** | Security + quality gatekeeper (pre-commit) | `file_read`, `code_exec` (test runner) | Isolated fresh window |

### 5.2 Kimi-Inspired Swarm Capacity *(from Kimi K2.6)*

Inspired by Kimi K2.6's **300 sub-agent, 4,000-step** execution architecture:
- **Swarm ceiling**: Up to 300 concurrent sub-agents per task tree
- **Step budget**: 4,000 coordinated steps per long-running project
- **Swarm supervisor**: DAG coordinator monitors all branches; detects stuck branches and re-routes
- **Inter-agent mailbox**: Structured message passing via Unix socket IPC (file-locking coordination for shared artifacts)
- **Swarm result aggregation**: Parallel branch results merged via a dedicated Aggregator agent before final response

### 5.3 Hermes-Style Kanban Multi-Agent Board *(from Hermes Agent v0.13)*

Long-running projects use a durable Kanban board:
- **Heartbeat**: Each worker agent sends a heartbeat every 30 seconds
- **Zombie detection**: Workers missing 3 consecutive heartbeats are automatically reclaimed
- **Task retries**: Failed tasks auto-retry with an updated plan (configurable max: 5 attempts)
- **Hallucination recovery**: Output validation step before task is marked complete
- **Hand-off protocol**: Workers can explicitly hand tasks to other workers via structured message passing

### 5.3 Model Council Routing *(from Perplexity Computer)*

The Model Council dynamically assigns the best-fit model to each subtask in the execution graph. Based on **Perplexity Computer's** approach of orchestrating **20+ models simultaneously**:

```
Task: "Write a blog post about quantum computing"
  ├── Research subtask → Gemini Flash (fast web grounding)
  ├── Outline subtask  → GPT-4o (structured reasoning)
  ├── Draft subtask    → Claude Sonnet (long-form writing quality)
  └── Edit subtask     → Mistral (efficient proofreading)
```

**High-stakes decisions** optionally invoke a **Model Council vote** — 3 candidate models independently draft an approach; the Aggregator agent selects the best or synthesizes the consensus. Routing is based on: task type classifier, cost budget, latency requirement, and model benchmark scores.

### 5.4 Self-Healing Re-planning *(from Devin / Cognition Labs)*

When a tool execution fails (compile error, network timeout, unexpected output):

1. The error trace is captured and formatted as structured context
2. A re-planning call is issued to the Model Router with the error trace + original goal
3. An updated execution branch is generated and substituted into the DAG
4. Retry proceeds from the failure point (not from scratch)
5. If re-planning fails 3+ times, the human-in-the-loop Consent Gate escalates to the user

---

## 6. Self-Improving Skill Engine

The most distinctive feature of Hydragent — borrowed directly from **Hermes Agent** (Nous Research), the *only* agent with a genuine built-in learning loop. Hermes was **#1 on OpenRouter's global token rankings** (271B tokens in 30 days), validating that a self-improving agent with quality outputs drives organic adoption.

### 6.1 Skill Authoring Pipeline

When Hydragent successfully completes a novel task for the first time:

1. **Execution trace logging**: Every tool call, decision branch, and output is captured
2. **Skill synthesis**: An LLM pass distills the trace into a reusable, parameterized skill program (saved as a signed Markdown file in `skills/`)
3. **Ed25519 signing**: The skill manifest is cryptographically signed before it can be loaded by the skill engine
4. **Index update**: The skill is added to a semantic skill-search index — future similar tasks route through it automatically

### 6.2 7-Day Autonomous Curator Cycle *(from Hermes Agent)*

A background Curator process runs every 7 days:

- **Grades** all skills based on: success rate, usage frequency, user feedback, recency
- **Consolidates** redundant skills (finds semantically similar skills and merges them)
- **Prunes** skills below a quality threshold (score < 0.4 are archived, not deleted)
- **Promotes** high-performing skills to a "Core Skills" tier with faster access paths
- Generates a weekly `curator.log` summary visible to the user

### 6.3 Self-Maintained Knowledge Wiki *(from Devin 3.0)*

Inspired by Devin 3.0's approach to codebase knowledge:
- As the agent works on projects, it maintains a **living knowledge wiki** about each project/domain
- The wiki is updated automatically after each significant task: architecture decisions, discovered patterns, resolved bugs, API endpoint mappings
- The wiki is queryable via semantic search and automatically injected into subagent context when relevant
- Users can view, edit, and export the knowledge wiki at any time
- Generates **live architectural diagrams** (Mermaid/PlantUML) that stay in sync with codebase changes

### 6.4 Gene Evolution Protocol *(from PicoClaw)*

For monitoring and automation tasks, Hydragent uses a bio-inspired evolution loop:
- Monitoring strategies are encoded as configurable "genes" (parameter sets)
- Strategies that achieve better outcomes over time are given higher weight and reproduced
- Low-performing strategies are mutated and retested
- This enables the agent to continuously optimize its own alert thresholds, polling frequencies, and action triggers without manual tuning

---

## 7. Personalization & User Modeling

Hydragent achieves deep personalization through a combination of explicit configuration and implicit behavioral learning.

### 7.1 SOUL.md & USER.md Persona System *(from OpenClaw + MimiClaw)*

The agent's identity and its understanding of the user are encoded in two living Markdown files:

**`SOUL.md`** — Agent identity:
```markdown
# Agent Identity
Name: Hydra
Personality: Curious, precise, warm. Direct over verbose.
Values: Privacy-first, honesty, user autonomy
Communication style: Bullet-points for technical content; conversational for personal topics
Prohibited: Never share raw credentials, never claim to be human
```

**`USER.md`** — User model:
```markdown
# User Profile
Name: [User's name]
Role: [Profession / domain]
Interests: [Top interests]
Working hours: [Timezone + schedule]
Preferences:
  - Response format: Bullet points for summaries, prose for creative tasks
  - Verbosity: Concise (< 3 paragraphs unless asked)
  - Language: English (formal)
Contact graph: [Known contacts and relationships]
```

Both files are automatically updated by the nightly Dreaming pipeline based on behavioral signals.

### 7.2 Affective / Emotional Memory *(from Inflection Pi + Vellum)*

- Tracks user sentiment across sessions (positive/neutral/negative tone detection)
- Detects discomfort signals and adjusts communication style proactively
- Stores "emotional anchors" — topics or events with high personal significance
- Applies tone matching: matches formality and warmth to the user's current communication pattern

### 7.3 Fine-Tuning & RLHF Loop

- **Thumbs-up/down**: Every response can receive explicit feedback, stored as training signal
- **Implicit feedback**: Response length adjustments, rephrasing requests, and conversation abandonment are captured as negative signals
- **LoRA adapter tuning**: For power users, optional local fine-tuning of a small LoRA adapter layer on a base model using accumulated feedback data
- **Persona templates**: Pre-built personas (Scholar, Engineer, Creative, Coach) that users can select or blend

---

## 8. Multimodal Input & Sensing

### 8.1 Supported Input Modalities

| Modality | Implementation | Source Inspiration |
|---|---|---|
| **Text** | Baseline LLM natural language | All agents |
| **Voice (input)** | OpenAI Whisper (local), Deepgram (cloud) | NanoClaw, QwenPaw, Moltis |
| **Voice (output)** | Coqui TTS (local), ElevenLabs (cloud) | Moltis, QwenPaw |
| **Vision / Images** | Claude Vision, GPT-4o Vision, LLaVA (local) | Claude Cowork, Manus, ACT-1 |
| **Documents** | PDF, DOCX, XLSX, CSV parsing pipeline | AnythingLLM, Khoj |
| **Screen / UI** | Screenshot analysis + Playwright actions | Claude Computer Use, Manus |
| **Sensor / IoT** | MQTT broker, GPIO serial, temperature feeds | MimiClaw, Humane CosmOS |
| **Calendar / Email** | OAuth-connected structured data | Perplexity Computer, Microsoft Scout |

### 8.2 Vision-Grounded Execution *(from Claude Computer Use + Manus)*

When performing GUI-level tasks, Hydragent:
1. Takes a screenshot of the current state
2. Runs it through a vision model to identify interactive elements (buttons, forms, dropdowns)
3. Generates a structured action plan (click(x,y), type("..."), scroll(direction))
4. Validates each action's outcome via subsequent screenshot analysis before proceeding

---

## 9. Multi-Model Council & Routing

### 9.1 Model Pool (19+ Models) *(from Perplexity Computer)*

Hydragent maintains a routing table of specialist models:

| Task Category | Primary Model | Fallback | Local Option |
|---|---|---|---|
| Core reasoning / orchestration | Claude Sonnet 4 / Opus 4 | GPT-4o | Qwen 2.5-72B (Ollama) |
| Fast web research | Gemini Flash | Perplexity Sonar | — |
| Code generation | Claude Sonnet / Deepseek Coder | GPT-4o | Deepseek Coder (Ollama) |
| Long-form writing | Claude Sonnet | GPT-4o | Mistral Instruct (Ollama) |
| Image generation | DALL-E 3 / Flux | Stable Diffusion | SD local |
| Embeddings | nomic-embed-text (local) | OpenAI text-embedding-3-small | nomic-embed-text |
| Voice STT | Whisper large-v3 (local) | Deepgram Nova | Whisper (local) |
| Lightweight summaries | GPT-4o mini / Gemini Flash | Mistral 7B | TinyLlama (edge) |
| High-stakes decisions | Model Council vote (3 candidates) | Best-of-3 selection | — |

### 9.2 Model-Agnostic Interface

All model calls route through an OpenAI-compatible API interface, enabling:
- **Seamless provider swap**: Change from OpenAI to Anthropic to local Ollama without changing orchestrator code
- **Cost-aware routing**: Each task carries a `budget_usd` constraint; the router selects the cheapest model meeting quality requirements
- **Latency-aware routing**: Time-sensitive tasks (< 2s budget) automatically select faster, smaller models
- **Fallback chains**: If a model is unavailable or rate-limited, the next model in the chain is tried automatically

---

## 10. RAG & Knowledge Base Integration

### 10.1 Personal Knowledge Base *(from Khoj + AnythingLLM)*

Hydragent indexes private documents into a searchable knowledge base:
- **Supported formats**: PDF, DOCX, Markdown, HTML, CSV, EPUB, PPTX, plain text
- **Indexing pipeline**: Text extraction → chunking (512 token chunks, 50 token overlap) → embedding → ChromaDB storage
- **Semantic search**: BM25 + vector hybrid retrieval (RRF fusion)
- **Live updates**: File watcher monitors configured folders; re-indexes on change

### 10.2 Web & Research RAG *(from Perplexity Computer + Khoj)*

- Real-time web grounding: Hydragent can search, scrape, and synthesize web content into responses
- Source attribution: Every fact retrieved from the web or document base is cited with its source
- Conflict resolution: When sources disagree, the agent surfaces the conflict and asks the user which to trust
- **Deep Research Mode**: Multi-step research pipeline — generates sub-questions, retrieves sources for each, synthesizes a comprehensive report

---

## 11. Human-in-the-Loop & Ethics Controls

### 11.1 Consent Gates *(from Microsoft Scout + IronClaw)*

- All state-mutating operations (file writes, emails, API calls, purchases) require explicit user approval before execution
- Approval UI supports: one-click approve, modify-then-approve, deny-and-explain, delegate-to-later
- **Audit trail**: Every approved and denied action is logged to the immutable Merkle audit log

### 11.2 Transparency Mode

- **Thought trace**: Users can enable "explain mode" to see the agent's chain-of-thought before each action
- **Tool log**: Full record of every tool called, with inputs and outputs (redacted credentials)
- **Memory diff**: After each Dreaming cycle, the user receives a diff of what changed in USER.md / SOUL.md

### 11.3 Ethical Guardrails

- **Bias detection**: Responses involving political, social, or sensitive topics include a "multiple perspectives" flag
- **Graceful failure**: Tool errors are caught, formatted, and explained — never raw stack traces to the user
- **Tone adaptation**: Agent adjusts formality and warmth to match user communication style
- **Hard limits**: Hardcoded refusals for credential exposure, self-replication without consent, disabling security layers, and impersonation of real humans

---

## 12. Edge & Embedded Deployment

### 12.1 Ultra-Lightweight Runtime *(from NullClaw + ZeroClaw + PicoClaw)*

| Build Target | Binary Size | RAM Footprint | Startup Time | LLM Mode |
|---|---|---|---|---|
| **Desktop (full)** | ~15 MB | < 100 MB | < 50 ms | Cloud API + Local Ollama |
| **Server (optimized)** | ~5 MB | < 30 MB | < 10 ms | Cloud API |
| **Edge Zig binary** | ~678 KB | < 1 MB | < 2 ms | Local quantized only |
| **Microcontroller** | ~150 KB | ~10 MB PSRAM | < 500 ms | TinyLLM on-device |

### 12.2 RISC-V & Microcontroller Support *(from MimiClaw + PicoClaw)*

- **RISC-V target**: Zig static binary cross-compiled to SOPHGO SG2002 RISC-V boards
- **ESP32-S3 target**: C-based PicoLM engine running quantized TinyLlama 1.1B (4-bit GGUF)
- **Power profile**: ~0.5W operation on ESP32-S3 (matching MimiClaw specifications)
- **Offline-first**: Full agent operation without internet connectivity using on-device models

---

## 13. Evaluation & Observability

### 13.1 Built-in Multi-Layer Evaluation *(from AWS Bedrock AgentCore + SuperAGI)*

Hydragent includes an evaluation harness measuring performance across three layers:

| Layer | Metrics | Method |
|---|---|---|
| **Model-level** | MMLU, GSM8K, HellaSWAG, HaluMem | Automated benchmark runner |
| **Component-level** | Intent accuracy, tool success rate, memory hit rate, error recovery rate | Instrumented unit tests |
| **End-to-end** | Task completion %, user satisfaction (1-5), response latency, hallucination rate | Simulation suite + user feedback |

### 13.2 Observability Stack

- **Metrics**: Prometheus-compatible metrics endpoint (`/metrics`) for Grafana dashboards
- **Tracing**: OpenTelemetry trace export for distributed request tracing
- **Logging**: Structured JSON logs with configurable verbosity; Merkle-hashed for tamper detection
- **Alerting**: Configurable thresholds for error rate, latency spikes, memory growth, and skill degradation

### 13.3 Daemon Agent *(from QwenPaw)*

A dedicated **Daemon Agent** runs as a background health monitor:
- Monitors agent health: memory DB size growth, skill quality degradation, tool error rates
- Sends a periodic "heartbeat" summary to the user's preferred channel (configurable: daily / weekly)
- Alerts on anomalies: unusual memory growth (potential poisoning), repeated tool failures, Curator score drops
- Can be queried directly: `"Hydra, how are you doing?"` → returns Daemon health report
- Self-corrects minor issues autonomously (e.g., re-indexes a corrupted memory segment, retries failed Curator run)

---

## 14. Plugin & Skill Marketplace

### 14.1 ClawHub-Compatible Skill Ecosystem *(from OpenClaw + TrustClaw)*

Hydragent is compatible with the broader ClawHub ecosystem while adding security guarantees:
- **6,000+ community skills** available via ClawHub marketplace
- **Security scanning**: Every imported skill is scanned for dangerous patterns before load
- **Ed25519 verification**: Skills from trusted publishers are signed and verified
- **Sandboxed execution**: All marketplace skills run inside WASM cages regardless of source

### 14.2 Private Plugin Marketplace *(from Claude Cowork + Taskade)*

For enterprise deployments:
- Administrators can publish internal skills/tools to a private, organization-scoped plugin registry
- Skills are distributed as portable Markdown+config bundles (no binary dependencies)
- Access control: Role-based skill visibility (e.g., Finance team sees billing tools, Engineering sees CI/CD tools)

## 15. Lifecycle Management

### 15.1 Self-Update *(new in Unreleased)*

The kernel binary can update itself in place from GitHub Releases:

- Queries the GitHub Releases API for the latest tag.
- Compares against `CARGO_PKG_VERSION` (the build-time version stamp);
  refuses to downgrade dev builds.
- Downloads the matching release asset for the host platform triple
  (`x86_64-pc-windows-msvc` → `.zip`, everything else → `.tar.gz`).
- Replaces the running binary without admin elevation or `PATH` mutation.

**Archive extraction** has a **3-tier fallback chain** for portability:

1. **System `tar -xf`** — always tried first; works on Win10+, macOS, Linux.
2. **Native Rust extraction** via the `tar` + `flate2` crates for `.tar.gz`
   and the `zip` crate for `.zip` — cross-platform, zero external deps.
3. **PowerShell `Expand-Archive`** for `.zip` on older Windows where
   `tar.exe` is missing (Windows 7 / 8).

### 15.2 Uninstall *(new in Unreleased)*

The kernel ships a first-class uninstaller (`hydragent uninstall`):

- Interactive confirmation by default; `-y` / `--yes` flag for scripted
  teardown.
- Removes the install directory (`~/.hydragent/`).
- Strips the matching `export PATH=…` line from the user's shell rc
  file (bash / zsh / fish / PowerShell profile).
- Detects and refuses to delete a source checkout's `.git/` tree
  without explicit confirmation (prevents "I ran uninstall inside my
  dev repo" data loss).

---

*For implementation details and technical specifications → **[ARCHITECTURE.md](ARCHITECTURE.md)***
*For the development roadmap and milestones → **[ROADMAP.md](ROADMAP.md)***
