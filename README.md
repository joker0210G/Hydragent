# Hydragent: The Unified AI Agent

> A next-generation, local-first, highly personalized AI agent combining the architectural strengths of today's best agentic platforms.

Hydragent is a modular meta-agent designed to consolidate the key innovations from the 2026 AI agent landscape. By separating orchestration, memory, security, and execution into distinct, pluggable interfaces, Hydragent achieves frontier-grade capability while remaining private-by-default, model-agnostic, and self-improving.

---

## 🌟 The Core Vision: Synthesis of Pros

Instead of building another single-purpose assistant, Hydragent extracts and integrates the best design patterns of current-gen systems:

*   **Self-Improving Skill Engine** (*from Nous Hermes Agent*): The agent code-generates its own tools (`SKILL.md`) based on past execution history and refines them recursively.
*   **Dreaming Memory Consolidation** (*from OpenClaw*): A three-stage nightly sleep/consolidation loop that compresses, links, and strengthens local memory stores.
*   **Defense-in-Depth Isolation** (*from IronClaw*): API keys and credentials are kept in an encrypted hardware vault and injected only at the network boundary—completely hidden from the LLM.
*   **Sandboxed Multi-Environment Execution** (*from Manus / Devin*): Code execution, browser automation, and shell access run inside isolated Docker/WASM environments to protect host resources.
*   **Unified Multi-Channel Gateway** (*from OpenClaw*): Interact with your agent seamlessly across Slack, Discord, Telegram, CLI, and Web UI.
*   **Workspace DNA Collaboration** (*from Taskade*): Run multi-agent swarms with specialist roles, shared workspaces, and persistent, collaborative memory.

---

## 🗂️ Project Repository Layout

The project is organized into the following documentation structures to guide implementation:

```text
├── RaD/                   # Original research, development reports, and references
├── README.md              # Project overview, highlights, and quickstart (this file)
├── FEATURES.md            # Comprehensive feature matrix and capability catalog
├── ARCHITECTURE.md        # Technical specification of modular layers and API schemas
└── ROADMAP.md             # Phased milestones and implementation timeline
```

---

## 🏗️ 7-Layer Architecture Overview

Hydragent decouples cognitive operations from channel interfaces and model runtimes:

```mermaid
graph TD
    Client[Channels: Telegram / Discord / Web] -->|gRPC / WebSockets| Gateway[Channel Gateway]
    Gateway -->|Intent Events| Orchestrator[Core Orchestrator]
    
    Orchestrator -->|Memory Queries| Memory[Hierarchical Memory: Episodic / Semantic / Procedural]
    Orchestrator -->|Dynamic Model Routing| ModelRouter[Model Router: OpenRouter / Ollama]
    Orchestrator -->|Tool Execution Plans| Dispatcher[Tool Dispatcher]
    
    Dispatcher -->|Capability Permissions| Sandbox[WASM & Docker Sandboxes]
    Sandbox -->|Executes| Tools[Browser / Python / Shell / MCP]
    
    classDef secure fill:#f96,stroke:#333,stroke-width:2px;
    class Sandbox,Memory secure;
```

For a detailed breakdown of the execution flow, interface contracts, and schema layouts, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## 🚀 Getting Started (Planned MVP)

### Prerequisites
*   Node.js v20+ / Go / Rust build chains (based on component preferences)
*   Docker (for sandbox isolation)
*   An OpenRouter API key (or local Ollama instance running Llama 3)

### Installation
```bash
# Clone the repository
git clone https://github.com/your-repo/hydragent.git
cd hydragent

# Install core dependencies
npm install

# Initialize local environment
cp .env.example .env
```

### Configuration
Configure your credentials in the secure env file:
```ini
OPENROUTER_API_KEY=your_key_here
LOCAL_OLLAMA_URL=http://localhost:11434
DATA_DIR=./data
```

For full setup procedures and available plugins, refer to the [FEATURES.md](file:///f:/Workspace(temp)/repo/ai%20agent/FEATURES.md) guidelines.

---

## 📄 License

Hydragent is open-source software licensed under the [MIT License](LICENSE).
