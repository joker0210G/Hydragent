# Project Features & Capabilities

This document provides a detailed catalog of the features and capabilities built into the **Hydragent Unified AI Agent**, highlighting how they inherit and synthesize the best components of today's leading agents.

---

## 🧠 1. Memory & Deep Personalization

Hydragent implements a hybrid memory engine that merges structured database queries with unstructured semantic searches, optimized for data locality and privacy.

### Hierarchical Memory Structure (*from memU*)
Rather than using a flat vector database, memory is structured as a hierarchical file system layout:
1.  **Folders (Categories)**: Auto-organized topics and themes created on-the-fly without manual tagging.
2.  **Files (Memory Items)**: Specific facts, user preferences, and learned skills stored as discrete Markdown documents.
3.  **Mount Points (Resources)**: Conversation transcripts and local files indexed semantically.

### Dual-Mode Retrieval
To minimize API token costs and maintain low latency:
*   **Fast Context Phase**: Uses cheap, local, embedding-based similarity scoring to monitor system metrics, notifications, and inbox streams with millisecond latency, without invoking any LLM API.
*   **Deep Reasoning Mode**: Switched automatically only when a high-signal pattern is detected, routing the query to frontier reasoning models (like Claude Sonnet).

### Compaction & ReMe (*from OpenClaw / QwenPaw*)
*   **Dreaming Consolidation**: A nightly task compresses episodic logs into concise relational nodes, links them to the semantic memory graph, and updates the core `SOUL.md` and `USER.md` prompt files.
*   **ReMe Memory Kit**: Integrates a file-based journaling layout. Dynamic compaction splits context into a retention group (raw recent conversation turns) and a compaction group (summarized historical dialogue) before every inference cycle. Hybrid search utilizes both ChromaDB vector search and BM25 exact keyword matching, scoring 88.78% accuracy on HaluMem QA benchmarks.

---

## 🔒 2. Security & Data Governance

Hydragent operates on a zero-trust architecture. We follow *IronClaw's* core rule: **Secrets must never reach the LLM**.

*   **Boundary Key Injection**: API keys, credentials, and OAuth tokens are stored in an encrypted vault (`secrets.json.enc` using XChaCha20-Poly1305 + Argon2id). The vault parses outbound requests and injects headers/tokens at the network layer. The LLM never sees or processes raw credentials.
*   **16-Layer Cryptographic Security** (*from OpenFang*):
    *   *TEE Encrypted Memory*: Cloud executions run inside hardware-level Trusted Execution Environments (TEEs) on the NEAR AI Cloud.
    *   *Ed25519 Signing*: All agent skill manifests are cryptographically verified before load.
    *   *Merkle Audit Trails*: An immutable event log ledger tracks all actions executed by the agent.
    *   *Taint Tracking*: Scans untrusted input fields to prevent prompt-injection attacks.
    *   *Secret Zeroization*: Force-wipes API credentials from memory immediately after requests complete.
*   **Enterprise Policy Integration** (*from Moltis / SGNL*): Connects with SGNL engines to evaluate tool capabilities against the user's active directory permissions, corporate device posture, and enterprise data fabric access policies.

---

## 🔌 3. Unified Multi-Channel Gateway

Hydragent separates the presentation layer from execution. A single gateway process coordinates user I/O across platforms:

*   **Multi-Platform Decoupling**: Decouples chat channels from core model logic, routing text and attachments across 40+ adapters including Telegram, Discord, Slack, WhatsApp, and CLI.
*   **Offline Heartbeats & Cron**: A persistent cron daemon wakes the agent at specified intervals to check inbox status, RSS feeds, or monitor system logs, initiating proactive actions.

---

## 🖥️ 4. Execution Sandbox & Tool Orchestration

Hydragent provides a rich runtime environment that gives the agent sandboxed access to real operating systems:

*   **Headless Browser Bot**: Powered by *Playwright* in a separate Docker container. Hydragent can automate browser tasks, fill forms, and scrape web pages, using screen snapshots for vision-grounded navigation.
*   **Local Code Sandbox**: Run Python, JavaScript, or Bash commands in a safe, metered runtime environment (*Daytona/E2B* inspired).
*   **MCP Server Integration**: Direct compatibility with Anthropic's Model Context Protocol (MCP), allowing Hydragent to fetch resources, run prompt templates, and connect to remote databases or services seamlessly.
*   **Three-Tier Permission Matrix** (*from Microsoft Scout*):
    1.  *Auto-approve*: Harmless read-only commands (e.g., listing directory contents, git status) execute instantly.
    2.  *Prompt*: Commands altering state (e.g., package installations, file writes, outbound requests) pause and await user verification.
    3.  *Deny*: Destructive actions (e.g., system configuration deletion) are blocked at the code engine level.
*   **Takeover Handoff**: If a GUI task becomes too complex or encounters a block, the agent triggers "Takeover Mode," sending a screen visual and a control link to the user for manual correction.

---

## 🤝 5. Multi-Agent Swarms & Orchestration

To scale past single-task execution, Hydragent decomposes complex operations using multi-agent team patterns:

*   **DAG Task Planning** (*from Manus / OpenCode*): Complex objectives are broken into a Directed Acyclic Graph. Work is split across specialized sub-agents:
    *   *Plan Agent*: Read-only codebase explorer suggesting architectures.
    *   *Build Agent*: Empowered with write permissions to modify local workspaces.
    *   *Explore Agent*: Traverses project directories with language server (LSP) intelligence.
    *   *Scout Agent*: Scans external documentation repositories.
*   **Model Council Routing**: Tasks are dynamically routed to specialized models depending on the step.
*   **Self-Healing Re-planning** (*from Devin*): Automatically captures compile or execution errors, constructs an updated execution path, and retries tasks autonomously.
