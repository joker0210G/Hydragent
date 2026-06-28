<p align="center">
  <img src="https://github.com/user-attachments/assets/6e8567e0-2409-4fbb-b287-a34b7aa06cb8" alt="Hydragent — the scholarly octopus mascot" width="280" />
</p>

# Hydragent — The Unified AI Agent

> *The agent that grows, remembers, executes, and protects — synthesizing the architectural DNA of 40+ frontier AI systems into one coherent, privacy-first, model-agnostic runtime.*

[![Status: v0.7.1 Shipped](https://img.shields.io/badge/Status-v0.7.1%20Shipped-brightgreen)](#-current-state-v071)
[![License: MIT](https://img.shields.io/badge/License-MIT-green)](LICENSE)
[![Core: Rust](https://img.shields.io/badge/Core-Rust-orange)](doc/ARCHITECTURE.md)
[![Edge: Zig](https://img.shields.io/badge/Edge-Zig%20%E2%89%A4678KB-yellow)](doc/ARCHITECTURE.md)
[![Adapters: Python](https://img.shields.io/badge/Adapters-Python-blue)](doc/ARCHITECTURE.md)

---

## 🐉 What is Hydragent?

The name **Hydragent** (Hydra + Agent) reflects the project's core philosophy:
- **Many heads, one body**: A single Rust-based core runtime serving multiple channel adapters (Telegram, Discord, Slack, CLI, webhooks).
- **Cut one head, two grow back**: A self-improving skill engine that automatically inducts new skills from successful executions and a self-healing replanner that learns from failure.
- **Uncompromising security**: A 16-layer cryptographic pipeline featuring a dual-slot vault (Passphrase PIN and Admin Key File) to keep your credentials safe and out of reach from LLM exposure.

---

## ⚡ Current State (v0.7.1)

Hydragent is a fully-functional runtime. Here is a quick snapshot of the current implementation:

| Component | Status / Details |
|---|---|
| **Core Workspace** | 16 Rust crates (`hydragent-core`, `hydragent-vault`, `hydragent-skills`, etc.) |
| **Testing** | 49 unit tests in the core binary, 79 vault tests, 52 skills tests, and 30 bench tests. |
| **Channels** | CLI REPL, Telegram (with Mini App support), Discord, Slack, Webhooks, and Email (IMAP/SMTP). |
| **Model Council** | 20+ model profiles (`config/model_council.yaml`) with dynamic task routing. |
| **Skill Library** | Auto-induction of skills during the nightly Dream cycle, managed by a 7-day Curator. |
| **Evaluation** | SKILL-BENCH (80 retrieval tasks) and the Golden Set (30 multi-relevance pairs). |

---

## 🐣 Quick Install & Onboarding

### 1. One-Command Installer

**Windows (PowerShell 5.1+):**
```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | iex
```

**macOS / Linux:**
```bash
curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
```

### 2. Getting Started from Source
If you are contributing or running from a local checkout:
```powershell
git clone https://github.com/joker0210G/Hydragent.git
cd Hydragent
.\Hydragent.cmd install   # Build and install prerequisites
.\Hydragent.cmd onboard   # Configure your LLM provider
.\Hydragent.cmd chat      # Start chatting!
```

---

## 📚 Documentation Index

The documentation has been consolidated into a few high-value files:

- **[ONBOARDING.md](ONBOARDING.md)**: Your guide to getting started. Contains developer onboarding, prerequisites, end-user installation options, contribution guidelines, and the project's changelog.
- **[doc/CLI.md](doc/CLI.md)**: Guide to the two CLIs (Rust `hydragent` vs Python `hydra-cli`) and their specific use cases.
- **[doc/ARCHITECTURE.md](doc/ARCHITECTURE.md)**: Deep technical specifications covering the core runtime stack, Event Bus wire protocol, JSON-RPC API, Python SDK, cryptographic vault, skill engine, and LoRA fine-tuning pipeline.
- **[doc/FEATURES.md](doc/FEATURES.md)**: The complete capability catalog, active tool/crate listings, and the development roadmap.

> [!NOTE]
> Research and development files are kept in the [doc/RaD/](doc/RaD/) folder and remain completely untouched.

---

## 📅 Development Roadmap (Summary)

- **Phase 1 (Core)**: Rust core runtime, JSON-RPC event bus, OpenRouter integration, CLI REPL. *(Shipped)*
- **Phase 2 (Memory)**: Hierarchical memory, BM25 + vector hybrid retrieval, nightly Dreaming compaction, Bounded hot memory (`USER.md` / `SOUL.md`). *(Shipped)*
- **Phase 3-4 (Sandbox & Gateway)**: WASM sandboxing, multi-channel gateways, cron scheduler, and proactive alerts. *(Shipped)*
- **Phase 5 (Swarm)**: Multi-agent swarm (DAG planner, 300 sub-agents) and Model Council routing. *(Shipped)*
- **Phase 6-7 (Security & Skills)**: 16-layer security (Merkle audit, taint tracking), self-improving skill engine, 7-day Curator, and SKILL-BENCH. *(Shipped)*
- **Phase 8 (Edge - *Next*)**: Zig edge port ($\le 678$ KB binary), PicoLM local offline GGUF inference, MQTT adapter. *(Planned)*
- **Phase 9 (Enterprise)**: Multi-tenant workspace isolation, RBAC, and SOC 2 controls. *(Planned)*

For the complete roadmap, see **[doc/FEATURES.md](doc/FEATURES.md#3-development-roadmap--phased-plan)**.

---

## 📄 License

Hydragent is open-source software licensed under the **MIT License**. See [LICENSE](LICENSE).
