# Project Implementation Roadmap

This document outlines the phased development plan, resource allocations, milestones, and risk mitigation strategies for building the **Hydragent Unified AI Agent**.

---

## 📅 Overview of Phases

```text
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 1: Core Runtime & Zig Boilerplate (Weeks 1-6)                     │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 2: Hierarchical Memory & BM25 Engine (Weeks 7-10)                 │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 3: Sandboxed Execution & 3-Tier Permissions (Weeks 11-14)         │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 4: Swarm Orchestration & Specialist Roles (Weeks 15-18)           │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 5: 16-Layer Security & Egress Auditing (Weeks 19-22)             │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 6: Edge Hardware & Local Inference (Weeks 23+)                    │
└────────────────────────────────────────────────────────────────────────┘
```

---

## 🛠️ Phase Details

### Phase 1: Core Runtime & Zig Boilerplate (Weeks 1-6)
*   **Milestone**: Deploy a persistent, systems-level compiled backend shell executing a basic ReAct loop, using OpenRouter endpoint APIs, starting up in <2ms.
*   **Key Tasks**:
    *   Initialize the Zig compiler workspace and configure static cross-compiling.
    *   Build the core vtable interface structures to support dynamic tool/channel swapping.
    *   Establish the gRPC event bus.
    *   Integrate OpenRouter SDK with fallback rotation chains.
*   **Risk**: Tooling and libraries in systems-level languages (Zig/Rust) are less mature than Python/Node.js.
    *   *Mitigation*: Implement C-bindings for legacy components or route heavy integration operations through decoupled Node.js bridge scripts.

---

### Phase 2: Hierarchical Memory & BM25 Engine (Weeks 7-10)
*   **Milestone**: Build a memU-style memory file-system layout and QwenPaw ReMe compaction scoring 88%+ on HaluMem.
*   **Key Tasks**:
    *   Develop the hierarchical SQLite schema representing Folders, Files, and Mount Points.
    *   Implement the dual-mode retrieval flow, triggering low-power embedding checks before routing to expensive reasoning models.
    *   Deploy BM25 exact matching alongside ChromaDB semantic vector indexes.
    *   Write the nightly "Dreaming" compaction parser that compresses daily logs into SOUL.md.
*   **Risk**: High vector database read overhead slowing execution.
    *   *Mitigation*: Page only active memory documents and load semantic vector indexes dynamically.

---

### Phase 3: Sandboxed Execution & 3-Tier Permissions (Weeks 11-14)
*   **Milestone**: Deploy isolated WebAssembly (Wasm) runtime tool containers and a Microsoft Scout-style permission gates monitor.
*   **Key Tasks**:
    *   Integrate a Playwright remote browser in a dedicated Docker sandbox.
    *   Configure *Wasmtime* execution wrappers for custom compiled capabilities.
    *   Deploy the encrypted vault (`secrets.json.enc`) utilizing Argon2id key derivation.
    *   Implement the 3-tier shell permission gate (Auto-approve, Prompt, Deny).
*   **Risk**: Browser sandboxes can be slow to initialize.
    *   *Mitigation*: Keep a warm pool of pre-booted Docker/browser nodes.

---

### Phase 4: Swarm Orchestration & Specialist Roles (Weeks 15-18)
*   **Milestone**: Launch parallel subagents configured in Plan, Build, Explore, and Scout topologies, coordinating over the gRPC bus.
*   **Key Tasks**:
    *   Write standard roles for Plan (read-only), Build (read-write), Explore (LSP compiler metrics), and Scout (docs gatherer) subagents.
    *   Implement dynamic re-planning on command errors.
    *   Configure "Model Council" consensus filters.
*   **Risk**: Subagents entering recursive execution loop pings.
    *   *Mitigation*: Force strict turn caps (max 5) on all inter-session agent dialogues.

---

### Phase 5: 16-Layer Security & Egress Auditing (Weeks 19-22)
*   **Milestone**: Complete security auditing including Merkle audit trails, taint tracking, and SGNL identity integrations.
*   **Key Tasks**:
    *   Implement Merkle tree structures to log all tool executions immutably.
    *   Configure taint analysis to inspect variable fields for prompt injections.
    *   Build egress traffic scanning to capture and zeroize credentials.
    *   Integrate with corporate directory access managers.
*   **Risk**: Audit log database sizes expanding quickly.
    *   *Mitigation*: Periodically snapshot Merkle logs and archive historical records to cloud storage enclaves.

---

### Phase 6: Edge Hardware & Local Inference (Weeks 23+)
*   **Milestone**: Run Hydragent on a $10 RISC-V edge developer board running quantized 4-bit GGUF models.
*   **Key Tasks**:
    *   Port the compiled runtime binary to SOPHGO SG2002 RISC-V targets.
    *   Configure local model paging via a C-based PicoLM engine.
    *   Test offline execution using quantized TinyLlama 1.1B models.
