# Phase 3: WASM Sandbox, Encrypted Vault & 3-Tier Permission Matrix (Weeks 11–14)

> **Timeline**: Weeks 11–14
> **Theme**: Make Hydragent **safe enough to trust with secrets and shell access** — a WASM capability sandbox that eliminates tool escape, an XChaCha20-Poly1305 encrypted credential vault where the LLM never sees raw keys, and a 3-tier user-consent permission gate for every state-mutating action.

> ## ⚠️ Implementation Status — Partially Implemented (as of June 2026)
> 
> Cross-checked against [`doc/STATE.md`](../STATE.md) at `git rev 3d99366` (June 2026).
> 
> **Schedule note:** The Docker-execution-sandbox week (originally the final week of Phase 3) has been **moved to a later phase** and is **not implemented** in this tree. Treat §5.7 of this document as deferred scope. The rest of Phase 3 ships as follows.
> 
> **What is live:**
> - **`PermissionTier` enum and `ToolStatus` are live** in `hydragent-types` and threaded through the tool registry.
> - **`hydragent-vault` crate exists** and provides XChaCha20-Poly1305 + Argon2id key derivation. The encryption surface is in place; the prompt-injection guard, taint tracking, and Merkle audit chain from this doc are **not**.
> - **`hydragent-sandbox` crate exists** with `engine.rs`, `wasm_tool.rs`, and `limits.rs`. It is wired to `wasmtime` with fuel-based CPU metering and can load the pre-compiled WASM tool artifacts under `sandbox/tools/`. Integration tests (`test_echo_tool_execution`, `test_file_read_tool_sandbox`) pass.
> - **Taint tracking surface** (`hydragent-vault::taint::TaintedString`) is present as a basic type wrapper that redacts on `Display`/`Debug`. The full 6-category propagation graph from this doc is **not** built.
> - **`KeyInjector` placeholder substitution** (`hydragent-vault::injector`) is implemented and unit-tested.
> 
> **What is stubbed or missing:**
> - **Docker execution sandbox is NOT implemented** and is deferred to a later phase. The Docker-related week in Phase 3 has been moved out of the schedule.
> - **Audit log is a stub.** The Merkle-chain audit log (§5.6) and full taint propagation (§5.8) are not built — the more complete versions live in Phase 6.
> - **Permission gate UI** (the 60-second-prompt flow) is the next milestone; the tier is declared on the tool but the user-facing prompt loop is not wired in `hydragent-core` yet.
> - **Open question (see STATE.md §4):** the sandbox crate's production runtime — Wasmtime vs. process isolation — has not been picked yet for non-WASM tool types.

---

## 📋 Table of Contents

1. [Phase Goals & Exit Criteria](#1-phase-goals--exit-criteria)
2. [Directory & Workspace Layout Changes](#2-directory--workspace-layout-changes)
3. [Technology Decisions](#3-technology-decisions)
4. [Week-by-Week Breakdown](#4-week-by-week-breakdown)
5. [Component Specifications](#5-component-specifications)
   - 5.1 [WASM Tool Sandbox](#51-wasm-tool-sandbox)
   - 5.2 [Encrypted Credential Vault](#52-encrypted-credential-vault)
   - 5.3 [3-Tier Permission Matrix](#53-3-tier-permission-matrix)
   - 5.4 [Key Injection at Network Boundary](#54-key-injection-at-network-boundary)
   - 5.5 [Sandbox-Aware Tool Registry](#55-sandbox-aware-tool-registry)
   - 5.6 [Audit Log & Merkle Chain (Stub)](#56-audit-log--merkle-chain-stub)
   - 5.7 [Docker Execution Sandbox](#57-docker-execution-sandbox)
   - 5.8 [Taint Tracking Subsystem](#58-taint-tracking-subsystem)
   - 5.9 [Security Vault CLI Interface](#59-security-vault-cli-interface)
   - 5.10 [Permission Gate UI (Python CLI & Telegram)](#510-permission-gate-ui-python-cli--telegram)
6. [Built-in Tools (Phase 3 Additions)](#6-built-in-tools-phase-3-additions)
7. [Configuration & Environment](#7-configuration--environment)
8. [Testing Strategy](#8-testing-strategy)
9. [Performance Targets](#9-performance-targets)
10. [Security Threat Model](#10-security-threat-model)
11. [Risks & Mitigations](#11-risks--mitigations)
12. [Definition of Done](#12-definition-of-done)

---

## 1. Phase Goals & Exit Criteria

Phase 3 elevates Hydragent from a capable conversational agent into a **trustworthy autonomous executor**. Inspired by IronClaw's boundary key injection model, NanoClaw's Docker isolation, and Microsoft Scout's 3-tier consent matrix, Phase 3 introduces the security infrastructure that makes it safe to give the agent real power: shell access, credential usage, and code execution.

### Hard Goals (must achieve before Phase 4)

| # | Goal | Validation |
|---|---|---|
| G1 | All tools execute inside a WASM runtime (`wasmtime`) with zero host network access | `cargo test wasm_sandbox::network_blocked` passes |
| G2 | Credential vault encrypts secrets with XChaCha20-Poly1305 + Argon2id KDF; LLM never receives raw key bytes | Code audit: no `String` or `str` containing raw API key ever enters `LLMRequest` struct |
| G3 | 3-tier permission gate (Auto / Prompt / Deny) for every state-mutating tool call | Integration test: `tier: prompt` tool waits for explicit user approval before executing |
| G4 | Key injection happens at network boundary (dispatcher, not orchestrator) | Code audit + unit test: mock request contains `{{GITHUB_TOKEN}}` placeholder; injector replaces; LLM context never had raw key |
| G5 | `code_exec` tool runs user Python/JS/Bash inside Docker container with no host filesystem mount | `docker run --rm --network none --read-only` confirmed in integration test |
| G6 | Audit log records every vault access, tool execution, and permission decision | SQLite `audit_log` table populated after each action; verified by test |
| G7 | Taint tagging prevents secrets from propagating into LLM context | Unit test: taint-tagged string rejected by prompt builder |
| G8 | `vault init` and `vault set/get/list/delete` CLI commands operational | `./hydragent vault --help` shows all subcommands |
| G9 | All Phase 1 and Phase 2 tests remain green (no regressions) | `cargo test --workspace` and `pytest adapters/` both exit 0 |

### Soft Goals (target but not blocking)

- Clear user-facing language explaining *why* a permission prompt appeared
- Timeout on permission prompts (default: 60 s) — auto-deny on timeout for safety
- Audit log entries rotatable/exportable via `./hydragent audit export --json`
- Vault entries are associated with named scopes (e.g., `github.token`, `serpapi.key`)

---

## 2. Directory & Workspace Layout Changes

Phase 3 adds the `hydragent-sandbox` and `hydragent-vault` crates, plus a `sandbox/` directory for pre-compiled WASM tool bytecode.

```
hydragent/
│
├── crates/
│   │
│   ├── hydragent-core/                # UPDATED: wires sandbox + vault into orchestrator
│   │   └── src/
│   │       ├── main.rs                # UPDATED: vault init guard, sandbox pool init
│   │       ├── orchestrator.rs        # UPDATED: permission gate before each tool dispatch
│   │       ├── react_loop.rs          # UPDATED: taint-aware observation collector
│   │       └── audit.rs              # NEW: structured audit event emitter
│   │
│   ├── hydragent-sandbox/             # NEW: WASM runtime wrapper
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── engine.rs             # wasmtime Engine + Store factory
│   │       ├── wasm_tool.rs          # WasmTool: loads .wasm module, calls execute()
│   │       ├── docker_runner.rs      # DockerTool: spawns containers, streams stdout
│   │       └── limits.rs            # ResourceLimits: CPU, memory, time caps
│   │
│   ├── hydragent-vault/               # NEW: encrypted credential store
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── vault.rs              # Vault: open/create, set, get, list, delete
│   │       ├── crypto.rs             # XChaCha20-Poly1305 + Argon2id KDF
│   │       ├── injector.rs           # KeyInjector: replaces {{PLACEHOLDER}} in requests
│   │       └── taint.rs             # TaintTag: marks secret-derived data
│   │
│   ├── hydragent-types/               # UPDATED: PermissionTier, TaintTag types
│   ├── hydragent-tools/               # UPDATED: Tool trait now declares PermissionTier
│   └── hydragent-memory/             # UPDATED: audit_log table migration
│
├── sandbox/
│   ├── tools/
│   │   ├── web_search.wasm           # Compiled WASM tool (DuckDuckGo)
│   │   ├── file_read.wasm            # Compiled WASM tool (workspace scoped)
│   │   └── echo.wasm                 # Compiled WASM tool (no-op)
│   └── build.sh                      # Compiles Rust tools → wasm32-wasi
│
├── config/
│   ├── SOUL.md                        # UPDATED: security-related hard limits
│   ├── permissions.yaml              # NEW: per-tool tier overrides
│   └── vault/
│       └── .hydravault               # Binary encrypted vault file (gitignored)
│
├── tests/
│   ├── unit/
│   │   ├── vault_test.rs             # Encrypt/decrypt, KDF, placeholder injection
│   │   ├── wasm_sandbox_test.rs      # Module load, execute, resource limits
│   │   ├── taint_test.rs             # Taint propagation and rejection
│   │   └── permission_gate_test.rs   # Tier routing, timeout, approval flow
│   └── integration/
│       ├── sandbox_e2e_test.rs       # Full tool execution through WASM sandbox
│       └── vault_cli_test.sh         # Bash: vault init, set, get, delete
```

---

## 3. Technology Decisions

### 3.1 WASM Runtime: `wasmtime`

We use **`wasmtime`** (Bytecode Alliance) as the WebAssembly runtime.

| Factor | Rationale |
|---|---|
| **Safety model** | Each `wasmtime::Instance` is strictly isolated — no host memory access unless explicitly linked via `wasmtime::Linker`. We link zero host functions by default. |
| **WASI support** | `wasmtime-wasi` provides the POSIX-like interface needed for Rust-compiled tools (`fs`, `env`, `clock`) scoped to a virtual root. |
| **Resource limits** | `wasmtime::Config::max_wasm_stack()`, `StoreLimitsBuilder::memory_size()` and fuel-based CPU metering are all supported natively. |
| **Rust integration** | `wasmtime` is a pure Rust crate; no FFI or `unsafe` in our code. |
| **Performance** | JIT compilation via Cranelift; within ~1.5–2× native speed for most tool workloads. |

**Alternative rejected**: `wasmer` — similar capability but less unified API across AOT/JIT and requires more `unsafe` glue.

### 3.2 Encryption: XChaCha20-Poly1305 + Argon2id

Inspired by IronClaw's cryptographic pipeline:

| Algorithm | Purpose | Crate |
|---|---|---|
| **Argon2id** | Key Derivation Function (passphrase → 256-bit key) | `argon2` |
| **XChaCha20-Poly1305** | Authenticated Encryption with Associated Data (AEAD) | `chacha20poly1305` |
| **`OsRng`** | Cryptographically secure random salt/nonce generation | `rand::rngs::OsRng` |

Why XChaCha20 over AES-256-GCM?
- **24-byte nonce** makes random nonce reuse practically impossible (vs 12-byte AES-GCM).
- No timing-side-channel vulnerability on CPUs without AES-NI hardware acceleration.
- Recommended by DJB & tweetNaCl — proven against multi-key attacks.

### 3.3 Docker Container Isolation

For the `code_exec` tool, WASM is insufficient when running arbitrary user-supplied Python/Node/Bash. We use Docker with a hardened configuration:

```
docker run
  --rm                    # Auto-remove on exit
  --network none          # Zero network access
  --read-only             # Root filesystem is read-only
  --tmpfs /tmp:size=32m   # Only /tmp is writable, 32 MB max
  --memory 256m           # Hard RAM limit
  --cpus 0.5              # Half a CPU core max
  --pids-limit 50         # Block fork bombs
  --cap-drop ALL          # Drop all Linux capabilities
  --security-opt no-new-privileges
  --user 1001:1001        # Non-root user
  hydragent-sandbox:latest
  /usr/bin/timeout 10 python3 /workspace/user_code.py
```

### 3.4 Permission Tier Design (Microsoft Scout / OpenClaw Inspired)

| Tier | Name | Behavior | Examples |
|---|---|---|---|
| `0` | **Auto-approve** | Executes without prompting | `echo`, `web_search`, `memory_search` |
| `1` | **Prompt** | Pauses loop, presents action to user, waits up to 60 s | `file_write`, `code_exec`, `memory_forget` |
| `2` | **Deny** | Always refuses; logs attempt | `vault_delete_all`, `system_shutdown` |

---

## 4. Week-by-Week Breakdown

### Week 11 — Encrypted Vault & CLI

**Goal**: Secrets live in an encrypted vault. `./hydragent vault` commands work end-to-end. Nothing is stored in plaintext in `.env` except the vault passphrase itself.

| Day | Task |
|---|---|
| Mon | Add `hydragent-vault` crate to workspace. Implement `crypto.rs`: Argon2id KDF (`argon2::hash_raw`) with tuned parameters (`m=65536`, `t=3`, `p=1`). Implement `XChaCha20Poly1305` encrypt/decrypt wrapper. Write deterministic unit tests with fixed salt/nonce. |
| Tue | Implement `vault.rs`: `Vault::open(path, passphrase)` — loads `.hydravault` binary file; decrypts payload with derived key. `Vault::create(path, passphrase)` — generates fresh salt, initializes empty encrypted store. |
| Wed | Implement `set(scope, value)`, `get(scope) -> Option<TaintedString>`, `list() -> Vec<String>`, `delete(scope)` on `Vault`. Use an internal `HashMap<String, Vec<u8>>` serialized via `serde_json` then AEAD-encrypted. |
| Thu | Implement `hydragent-vault/src/taint.rs`: `TaintedString` newtype wrapping `String`. `TaintedString` does NOT implement `Display`, `Debug` (only `[REDACTED]`), or `Serialize`. This prevents accidental logging. |
| Fri | Wire vault `init` and `set/get/list/delete` subcommands into `main.rs` via `clap` subcommand tree. Test: `./hydragent vault init`, `./hydragent vault set github.token ghp_xxxx`, `./hydragent vault get github.token` (shows masked value). |
| Sat | Integration test: create vault, set 5 secrets, restart process (vault re-opens from disk), verify all 5 retrievable. Verify wrong passphrase → `Err(VaultError::DecryptionFailed)`. |
| Sun | Write `vault_cli_test.sh` bash integration test. Document vault wire format in `crates/hydragent-vault/VAULT_FORMAT.md`. |

**Deliverable**: `./hydragent vault {init,set,get,list,delete}` fully functional. Vault file is opaque binary. `cargo test` vault tests green.

---

### Week 12 — WASM Sandbox Engine

**Goal**: Phase 1 tools (`web_search`, `file_read`, `echo`) run inside `wasmtime` WASM instances with verified resource limits. Network access from within WASM is blocked.

| Day | Task |
|---|---|
| Mon | Add `hydragent-sandbox` crate. Add `wasmtime`, `wasmtime-wasi` to workspace dependencies. Implement `engine.rs`: shared `wasmtime::Engine` with Cranelift JIT; configure fuel metering (1 billion fuel units = ~1 s CPU time). |
| Tue | Implement `wasm_tool.rs`: `WasmTool::load(module_path: &str) -> Result<Self>`. Compiles `.wasm` module, instantiates with `wasmtime_wasi::WasiCtxBuilder` scoped to `preopened_dir("/workspace")` only. |
| Wed | Implement `wasm_tool.rs::execute(params_json: &str) -> ToolResult`. Calls exported `tool_execute` function in the WASM module. Writes params to WASM linear memory; reads result back. Uses `wasmtime::TypedFunc<(i32, i32), (i32, i32)>` for zero-copy ABI. |
| Thu | Implement `limits.rs`: `ResourceLimits` struct — `max_fuel: u64`, `max_memory_bytes: u64`, `max_exec_ms: u64`. Hook `StoreLimitsBuilder` to enforce memory limits. Hook `tokio::time::timeout` to enforce execution time. |
| Fri | Compile Phase 1 Rust tools to `wasm32-wasi`: add `wasm32-wasi` target, write `sandbox/build.sh`. Verify `web_search.wasm`, `file_read.wasm`, `echo.wasm` compile < 1 MB each. |
| Sat | Unit test battery: (1) network access blocked inside WASM, (2) path traversal attempt rejected by WASI preopened scope, (3) CPU fuel exhaustion returns `ToolStatus::Timeout`, (4) memory limit triggers `ToolStatus::Failure`. |
| Sun | Benchmark: WASM tool execution overhead vs native. Target: < 5 ms overhead per tool call for typical workloads. |

**Deliverable**: All 3 Phase 1 tools run inside WASM. Resource limits are enforced. `cargo test wasm_sandbox` green.

---

### Week 13 — 3-Tier Permission Matrix & Key Injection

**Goal**: Every state-mutating action passes through the permission gate. Secrets are injected at the network boundary, never earlier.

| Day | Task |
|---|---|
| Mon | Update `hydragent-types`: add `PermissionTier` enum (`AutoApprove`, `Prompt`, `Deny`). Update `Tool` trait: add `fn permission_tier(&self) -> PermissionTier` method (default `AutoApprove`). |
| Tue | Implement `orchestrator.rs` permission gate: before `tool_registry.invoke(call)`, call `permission_gate.check(call, tier)`. For `AutoApprove` — proceed immediately. For `Prompt` — emit `PermissionRequest` event on bus, await `PermissionResponse` from channel adapter with timeout. For `Deny` — immediately return `ToolResult::denied()`. |
| Wed | Implement `KeyInjector` in `hydragent-vault/src/injector.rs`. It scans `LLMRequest.messages` for `{{VAULT_SCOPE_NAME}}` placeholders in *only* the tool-result and system fields (never user message fields). Replaces with the tainted secret. Immediately after HTTP dispatch, zeroes the replacement memory via `zeroize`. |
| Thu | Wire `KeyInjector` into the OpenRouter HTTP client. After `reqwest` builds the request body but before `.send()`, injector runs. Add integration test: verify placeholder present in orchestrator; verify raw key NOT present in any `tracing` log output at any log level. |
| Fri | Implement CLI permission prompt in `adapters/cli_adapter.py`: on `PermissionRequest` bus event, pause streaming, print action summary with yellow `[!] Approval Required` banner, await user `y/n`, send `PermissionResponse` back. |
| Sat | Implement Telegram permission prompt (stub): same flow via Telegram inline button `✅ Allow` / `❌ Deny` (inline keyboard). |
| Sun | Full ReAct loop integration test with a `file_write` tool (tier=Prompt): user asks to write a file → agent proposes → gate pauses → simulated `y` approval → file written. |

**Deliverable**: Permission gate functional. Key injection verified via integration test. All three tiers have corresponding test coverage.

---

### Week 14 — Docker Sandbox, Audit Log & Hardening — ⏸️ **DEFERRED**

> **Status:** Week 14 work has been **moved to a later phase** per the project owner. The Docker execution sandbox is **not implemented** in this tree. The Merkle-chain audit log, taint propagation, and audit CLI listed below are likewise deferred. The Phase 3 audit-log story is fully re-imagined in [Phase 6](./PHASE_6.md).

**Goal (deferred)**: `code_exec` runs in Docker with zero host exposure. Audit log records every sensitive event. Phase 3 is hardened and ready for Phase 4 channel expansion.

| Day | Task |
|---|---|
| Mon | ~~Implement `docker_runner.rs`: `DockerTool::run(code: &str, lang: &str, timeout_ms: u64) -> ToolResult`. Writes code to temp file, calls `docker run` with the hardened flags (Section 3.3). Streams stdout/stderr back as `ToolResult.output_json`.~~ **Deferred.** |
| Tue | ~~Build `hydragent-sandbox:latest` Docker image: based on `python:3.12-slim`, adds `nodejs`, `bash`. No network access baked in — enforced at `docker run` time via `--network none`.~~ **Deferred.** |
| Wed | ~~Implement `audit.rs` in `hydragent-core`: `AuditEvent` struct with fields `{event_type, actor, tool_id, permission_tier, decision, timestamp_ms}`. `emit_audit_event()` async fn writes to `audit_log` SQLite table.~~ **Deferred to Phase 6** (Merkle-chained variant). |
| Thu | ~~Apply `emit_audit_event()` at every decision point: (1) vault access, (2) tool invocation start, (3) permission gate decision, (4) key injection event (logs placeholder name, not value), (5) Docker container spawn/exit.~~ **Deferred.** |
| Fri | ~~Implement `./hydragent audit list` and `./hydragent audit export --json > audit.json` CLI subcommands.~~ **Deferred.** |
| Sat | ~~Full regression suite: `cargo test --workspace` (all crates including sandbox and vault) + `pytest adapters/` — must both exit 0 with zero failures.~~ Partially done (existing crates green; deferred items not yet testable). |
| Sun | ~~Performance profiling: ensure permission gate adds < 2 ms for auto-approve tier, vault get < 1 ms. Tag `v0.3.0` pre-release. Write CHANGELOG entry.~~ Tagging deferred until Docker work lands. |

**Deliverable (deferred)**: `v0.3.0` pre-release. All Phase 3 exit criteria met once Docker work returns to the schedule. Until then, see [Phase 6 §5.7](./PHASE_6.md#57-memory-encryption-at-rest) and [Phase 6 §5.2](./PHASE_6.md#52-merkle-audit-chain) for the planned home of the audit log.

---

## 5. Component Specifications

### 5.1 WASM Tool Sandbox

#### 5.1.1 Architecture

Every tool in Hydragent now exists in two forms:
1. **Native host tool**: The raw Rust `impl Tool` struct. Used only for bootstrapping and testing.
2. **WASM module**: The compiled `.wasm` binary in `sandbox/tools/`. Used in all production execution paths.

The `WasmTool` struct wraps both forms. On `execute()`, if a `.wasm` module is present, it runs inside `wasmtime`. If absent (e.g., in test environments), it falls back to the native host implementation.

```rust
// crates/hydragent-sandbox/src/wasm_tool.rs

use wasmtime::*;
use wasmtime_wasi::WasiCtxBuilder;
use anyhow::{Context, Result};
use hydragent_types::{ToolResult, ToolStatus};

pub struct WasmTool {
    engine: Engine,
    module: Module,
    limits: ResourceLimits,
}

impl WasmTool {
    pub fn load(wasm_path: &str, limits: ResourceLimits) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true); // Enable fuel metering
        config.wasm_component_model(false); // Classic WASM, not components (Phase 5+)
        
        let engine = Engine::new(&config)?;
        let wasm_bytes = std::fs::read(wasm_path)
            .with_context(|| format!("Failed to read WASM module at: {}", wasm_path))?;
        let module = Module::new(&engine, &wasm_bytes)?;
        
        Ok(Self { engine, module, limits })
    }

    pub async fn execute(&self, params_json: &str) -> Result<ToolResult> {
        let engine = self.engine.clone();
        let module = self.module.clone();
        let limits = self.limits.clone();
        let params = params_json.to_string();

        // Spawn blocking WASM execution on a dedicated thread pool thread.
        // This prevents blocking the Tokio async runtime.
        let result = tokio::task::spawn_blocking(move || {
            execute_wasm_sync(&engine, &module, &limits, &params)
        })
        .await??;
        
        Ok(result)
    }
}

fn execute_wasm_sync(engine: &Engine, module: &Module, limits: &ResourceLimits, params: &str) -> Result<ToolResult> {
    // Build sandboxed WASI context — only /workspace is accessible
    let wasi = WasiCtxBuilder::new()
        .inherit_stdout()
        .inherit_stderr()
        .build();

    // Create a Store (per-execution state container) with resource limits
    let store_limits = StoreLimitsBuilder::new()
        .memory_size(limits.max_memory_bytes as usize)
        .instances(1)
        .build();

    let mut store = Store::new(engine, wasi);
    store.limiter(|_| &mut store_limits);
    store.set_fuel(limits.max_fuel)?;

    // Link WASI host functions (filesystem, clocks — NO network)
    let mut linker: Linker<wasmtime_wasi::WasiCtx> = Linker::new(engine);
    wasmtime_wasi::add_to_linker(&mut linker, |cx| cx)?;
    // NOTE: We explicitly do NOT link any network functions.
    // wasmtime_wasi::tokio::add_to_linker is NOT called.

    let instance = linker.instantiate(&mut store, module)?;
    
    // Call the exported `tool_execute` function
    // ABI: (params_ptr: i32, params_len: i32) -> (result_ptr: i32, result_len: i32)
    let tool_execute: TypedFunc<(i32, i32), (i32, i32)> =
        instance.get_typed_func(&mut store, "tool_execute")?;
    
    // Write params into WASM linear memory
    let memory = instance.get_memory(&mut store, "memory").context("No memory export")?;
    let alloc: TypedFunc<i32, i32> = instance.get_typed_func(&mut store, "alloc")?;
    
    let params_bytes = params.as_bytes();
    let ptr = alloc.call(&mut store, params_bytes.len() as i32)?;
    memory.write(&mut store, ptr as usize, params_bytes)?;
    
    // Invoke the function
    let (result_ptr, result_len) = tool_execute.call(&mut store, (ptr, params_bytes.len() as i32))?;
    
    // Read result from WASM linear memory
    let mut result_bytes = vec![0u8; result_len as usize];
    memory.read(&store, result_ptr as usize, &mut result_bytes)?;
    
    let result_str = std::str::from_utf8(&result_bytes)?;
    let tool_result: ToolResult = serde_json::from_str(result_str)?;
    
    Ok(tool_result)
}
```

#### 5.1.2 Resource Limits Configuration

```rust
// crates/hydragent-sandbox/src/limits.rs

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Fuel units consumed per WASM instruction. 1 billion ≈ 1 second of CPU.
    pub max_fuel: u64,
    /// Maximum linear memory the WASM module can allocate.
    pub max_memory_bytes: u64,
    /// Wall-clock timeout enforced by Tokio, independent of fuel.
    pub max_exec_ms: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_fuel: 1_000_000_000,    // ~1 s CPU
            max_memory_bytes: 64 * 1024 * 1024, // 64 MB
            max_exec_ms: 10_000,        // 10 s wall clock
        }
    }
}

impl ResourceLimits {
    /// Strict limits for untrusted user-provided tool inputs.
    pub fn strict() -> Self {
        Self {
            max_fuel: 100_000_000,      // ~100 ms CPU
            max_memory_bytes: 16 * 1024 * 1024, // 16 MB
            max_exec_ms: 5_000,         // 5 s wall clock
        }
    }

    /// Relaxed limits for first-party trusted tools (web_search, file_read).
    pub fn trusted() -> Self {
        Self::default()
    }
}
```

---

### 5.2 Encrypted Credential Vault

#### 5.2.1 Cryptographic Design

The vault uses a layered encryption scheme:

```
User Passphrase
      │
      ▼ Argon2id(m=65536, t=3, p=1, salt=OsRng[32B])
Master Key (256-bit)
      │
      ▼ XChaCha20-Poly1305(key=Master, nonce=OsRng[24B])
Encrypted Payload
      │
      ▼ base64-url encode
Vault File (.hydravault)
```

The `.hydravault` binary file has the following layout:
```
[4B magic "HVLT"] [1B version] [32B argon2 salt] [24B nonce] [variable encrypted payload] [16B AEAD tag]
```

```rust
// crates/hydragent-vault/src/crypto.rs

use argon2::{Argon2, Algorithm, Version, Params};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use anyhow::{Context, Result};

const MAGIC: &[u8; 4] = b"HVLT";
const VERSION: u8 = 1;
const ARGON2_MEMORY_KB: u32 = 65536; // 64 MB memory hardness
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;

pub struct Crypto;

impl Crypto {
    /// Derive a 256-bit master key from the user passphrase.
    pub fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
        let params = Params::new(
            ARGON2_MEMORY_KB,
            ARGON2_ITERATIONS,
            ARGON2_PARALLELISM,
            Some(32)
        ).context("Invalid Argon2 params")?;
        
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = [0u8; 32];
        argon2.hash_password_into(passphrase.as_bytes(), salt, &mut key)
            .map_err(|e| anyhow::anyhow!("Argon2 KDF failed: {}", e))?;
        
        Ok(key)
    }

    /// Encrypt a plaintext payload using XChaCha20-Poly1305.
    /// Returns `(nonce, ciphertext_with_tag)`.
    pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let cipher = XChaCha20Poly1305::new(key.into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        
        let ciphertext = cipher.encrypt(&nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;
        
        Ok((nonce.to_vec(), ciphertext))
    }

    /// Decrypt a ciphertext payload using XChaCha20-Poly1305.
    pub fn decrypt(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
        let cipher = XChaCha20Poly1305::new(key.into());
        let nonce = XNonce::from_slice(nonce);
        
        cipher.decrypt(nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("Decryption failed — wrong passphrase or corrupted vault"))
    }
}
```

#### 5.2.2 Vault API

```rust
// crates/hydragent-vault/src/vault.rs

use std::collections::HashMap;
use std::path::Path;
use rand::RngCore;
use rand::rngs::OsRng;
use zeroize::Zeroize;

use crate::crypto::Crypto;
use crate::taint::TaintedString;

pub struct Vault {
    /// The in-memory decrypted store. Zeroized on drop.
    store: HashMap<String, Vec<u8>>,
    /// The 256-bit master key. Zeroized on drop.
    master_key: [u8; 32],
    /// Path to the vault file on disk.
    vault_path: String,
}

impl Vault {
    /// Create a new vault at `path`. Generates a new random salt.
    pub fn create(vault_path: &str, passphrase: &str) -> anyhow::Result<Self> {
        let mut salt = [0u8; 32];
        OsRng.fill_bytes(&mut salt);
        
        let master_key = Crypto::derive_key(passphrase, &salt)?;
        let store: HashMap<String, Vec<u8>> = HashMap::new();
        
        let mut vault = Self { store, master_key, vault_path: vault_path.to_string() };
        vault.persist(&salt)?;
        
        tracing::info!(vault_path, "New vault created");
        Ok(vault)
    }

    /// Open an existing vault.
    pub fn open(vault_path: &str, passphrase: &str) -> anyhow::Result<Self> {
        let file_bytes = std::fs::read(vault_path)
            .map_err(|_| anyhow::anyhow!("Vault file not found at: {}. Run `vault init` first.", vault_path))?;
        
        // Parse header
        if &file_bytes[0..4] != b"HVLT" {
            anyhow::bail!("Invalid vault magic bytes");
        }
        // version = file_bytes[4]
        let salt = &file_bytes[5..37];
        let nonce = &file_bytes[37..61];
        let ciphertext = &file_bytes[61..];
        
        let master_key = Crypto::derive_key(passphrase, salt)?;
        let plaintext = Crypto::decrypt(&master_key, nonce, ciphertext)?;
        let store: HashMap<String, Vec<u8>> = serde_json::from_slice(&plaintext)?;
        
        tracing::info!(vault_path, "Vault opened successfully");
        Ok(Self { store, master_key, vault_path: vault_path.to_string() })
    }

    /// Store a secret. Value is stored as raw bytes.
    pub fn set(&mut self, scope: &str, value: &str) -> anyhow::Result<()> {
        self.store.insert(scope.to_string(), value.as_bytes().to_vec());
        self.save()?;
        tracing::info!(scope, "Vault entry set");
        Ok(())
    }

    /// Retrieve a secret as a TaintedString. Returns None if not found.
    pub fn get(&self, scope: &str) -> anyhow::Result<Option<TaintedString>> {
        match self.store.get(scope) {
            None => Ok(None),
            Some(bytes) => {
                let s = std::str::from_utf8(bytes)
                    .map_err(|_| anyhow::anyhow!("Vault entry for '{}' is not valid UTF-8", scope))?;
                Ok(Some(TaintedString::new(s.to_string())))
            }
        }
    }

    /// List all stored scope names (never shows values).
    pub fn list(&self) -> Vec<String> {
        self.store.keys().cloned().collect()
    }

    /// Delete a specific secret.
    pub fn delete(&mut self, scope: &str) -> anyhow::Result<bool> {
        let existed = self.store.remove(scope).is_some();
        if existed { self.save()?; }
        tracing::info!(scope, existed, "Vault entry deleted");
        Ok(existed)
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut salt = [0u8; 32];
        OsRng.fill_bytes(&mut salt);
        self.persist(&salt)
    }

    fn persist(&self, salt: &[u8]) -> anyhow::Result<()> {
        let plaintext = serde_json::to_vec(&self.store)?;
        let (nonce, ciphertext) = Crypto::encrypt(&self.master_key, &plaintext)?;
        
        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(b"HVLT");
        file_bytes.push(1u8); // version
        file_bytes.extend_from_slice(salt);
        file_bytes.extend_from_slice(&nonce);
        file_bytes.extend_from_slice(&ciphertext);
        
        std::fs::write(&self.vault_path, &file_bytes)?;
        Ok(())
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        // Zeroize the master key and store values from memory on drop.
        self.master_key.zeroize();
        for v in self.store.values_mut() {
            v.zeroize();
        }
        self.store.clear();
    }
}
```

---

### 5.3 3-Tier Permission Matrix

#### 5.3.1 Type Definitions

```rust
// crates/hydragent-types/src/lib.rs (additions for Phase 3)

/// Every tool declares a tier. Tier escalation is possible via config/permissions.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionTier {
    /// Execute immediately, no user prompt.
    AutoApprove,
    /// Pause the ReAct loop, present the action to the user, wait for explicit approval.
    Prompt,
    /// Always reject this action. Log the attempt.
    Deny,
}

impl Default for PermissionTier {
    fn default() -> Self { PermissionTier::AutoApprove }
}

/// Emitted by the orchestrator on the bus when a `Prompt` tier action is requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: String,     // UUID
    pub session_id: String,
    pub tool_id: String,
    pub params_summary: String, // Human-readable description of the action
    pub tier: PermissionTier,
    pub expires_at_ms: i64,     // Timestamp after which auto-deny triggers
}

/// Response from the channel adapter (user decision).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub request_id: String,
    pub approved: bool,
}
```

#### 5.3.2 Permission Gate Implementation

```rust
// crates/hydragent-core/src/orchestrator.rs (additions)

use std::collections::HashMap;
use tokio::sync::oneshot;
use hydragent_types::{PermissionTier, PermissionRequest, PermissionResponse};

pub struct PermissionGate {
    /// Pending decisions awaiting user response. Key: request_id.
    pending: HashMap<String, oneshot::Sender<bool>>,
    bus_tx: mpsc::Sender<BusEvent>,
    timeout_ms: u64,
}

impl PermissionGate {
    pub fn new(bus_tx: mpsc::Sender<BusEvent>, timeout_ms: u64) -> Self {
        Self { pending: HashMap::new(), bus_tx, timeout_ms }
    }

    /// Called before each tool invocation. Returns Ok(true) if approved, Ok(false) if denied.
    pub async fn check(
        &mut self, 
        tool_id: &str, 
        params_summary: &str, 
        tier: PermissionTier,
        session_id: &str,
    ) -> anyhow::Result<bool> {
        match tier {
            PermissionTier::AutoApprove => {
                tracing::debug!(tool_id, "Permission: auto-approved");
                Ok(true)
            },
            PermissionTier::Deny => {
                tracing::warn!(tool_id, "Permission: permanently denied by policy");
                Ok(false)
            },
            PermissionTier::Prompt => {
                let request_id = uuid::Uuid::new_v4().to_string();
                let expires_at_ms = current_timestamp_ms() + self.timeout_ms as i64;
                
                let request = PermissionRequest {
                    request_id: request_id.clone(),
                    session_id: session_id.to_string(),
                    tool_id: tool_id.to_string(),
                    params_summary: params_summary.to_string(),
                    tier,
                    expires_at_ms,
                };
                
                // Send the permission request to the channel adapter via bus
                self.bus_tx.send(BusEvent::PermissionRequest(request)).await?;
                
                // Set up a oneshot channel to receive the user's decision
                let (tx, rx) = oneshot::channel::<bool>();
                self.pending.insert(request_id, tx);
                
                // Wait for user response, with timeout
                match tokio::time::timeout(
                    std::time::Duration::from_millis(self.timeout_ms),
                    rx
                ).await {
                    Ok(Ok(approved)) => {
                        tracing::info!(tool_id, approved, "Permission: user decision received");
                        Ok(approved)
                    },
                    Ok(Err(_)) => {
                        tracing::warn!(tool_id, "Permission: response channel dropped, auto-denying");
                        Ok(false)
                    },
                    Err(_) => {
                        tracing::warn!(tool_id, "Permission: timed out, auto-denying for safety");
                        Ok(false)
                    }
                }
            }
        }
    }

    /// Called by the bus router when a `PermissionResponse` arrives from a channel adapter.
    pub fn receive_response(&mut self, response: PermissionResponse) {
        if let Some(tx) = self.pending.remove(&response.request_id) {
            let _ = tx.send(response.approved);
        }
    }
}
```

---

### 5.4 Key Injection at Network Boundary

The `KeyInjector` runs *inside* `openrouter.rs`, immediately before `reqwest::Client::post(...).send()`. It replaces `{{VAULT_SCOPE}}` placeholders in the serialized request body with the raw secret value from the vault. After the HTTP call returns, the tainted value is `zeroize`d.

```rust
// crates/hydragent-vault/src/injector.rs

use crate::taint::TaintedString;
use zeroize::Zeroize;
use std::collections::HashMap;

pub struct KeyInjector {
    vault_values: HashMap<String, TaintedString>,
}

impl KeyInjector {
    pub fn new(vault_values: HashMap<String, TaintedString>) -> Self {
        Self { vault_values }
    }

    /// Inject vault secrets into the serialized JSON request body.
    /// Input: JSON string with `{{SCOPE_NAME}}` placeholders.
    /// Output: JSON string with secrets injected.
    ///
    /// SAFETY: The output string is `TaintedString` and must be zeroized after use.
    pub fn inject(&self, mut body: String) -> (TaintedString, Vec<String>) {
        let mut injected_scopes = Vec::new();
        
        for (scope, tainted_value) in &self.vault_values {
            let placeholder = format!("{{{{{}}}}}", scope);
            
            if body.contains(&placeholder) {
                // SAFETY: TaintedString::expose_secret() is the only way to get raw &str.
                // This call is auditable and intentional.
                body = body.replace(&placeholder, tainted_value.expose_secret());
                injected_scopes.push(scope.clone());
                tracing::debug!(scope, "Key injected at network boundary");
            }
        }
        
        (TaintedString::new(body), injected_scopes)
    }
}

impl Drop for KeyInjector {
    fn drop(&mut self) {
        // TaintedString implements Zeroize — values are wiped on drop.
        self.vault_values.clear();
    }
}
```

Usage in `openrouter.rs`:

```rust
// crates/hydragent-model/src/openrouter.rs (updated)

pub async fn chat_stream_with_injection(
    &self,
    request: &LLMRequest,
    tx: mpsc::Sender<String>,
    injector: &KeyInjector,
) -> Result<String> {
    // 1. Serialize request to JSON string (still contains {{PLACEHOLDER}}s)
    let json_body = serde_json::to_string(request)?;
    
    // 2. Inject keys at the network boundary
    let (injected_body, injected_scopes) = injector.inject(json_body);
    
    // 3. Emit audit event: "keys injected for scopes: [github.token, serpapi.key]"
    tracing::info!(scopes = ?injected_scopes, "Performing key injection");
    
    // 4. Send raw HTTP request using the injected body
    let response = self.client
        .post(format!("{}/chat/completions", self.base_url))
        .bearer_auth(&self.api_key)
        .header("Content-Type", "application/json")
        .body(injected_body.expose_secret().to_string()) // Only here is it exposed as raw string
        .send()
        .await?;
    
    // injected_body is dropped here → TaintedString zeroizes automatically via Drop

    // 5. Parse SSE stream as before...
    parse_sse_stream(response, tx).await
}
```

---

### 5.5 Sandbox-Aware Tool Registry

The tool registry is updated to understand both native tools and WASM-sandboxed tools, routing to the appropriate runtime.

```rust
// crates/hydragent-tools/src/registry.rs (updated)

use hydragent_types::{ToolCall, ToolResult, ToolStatus, PermissionTier};
use hydragent_sandbox::WasmTool;

pub enum ToolRuntime {
    Native(Arc<dyn Tool>),
    Wasm(WasmTool),
}

pub struct ToolRegistry {
    tools: HashMap<String, (ToolRuntime, PermissionTier)>,
}

impl ToolRegistry {
    pub fn new() -> Self { Self { tools: HashMap::new() } }

    pub fn register_native(&mut self, tool: impl Tool + 'static, tier: PermissionTier) {
        let name = tool.name().to_string();
        tracing::info!(tool = %name, ?tier, "Native tool registered");
        self.tools.insert(name, (ToolRuntime::Native(Arc::new(tool)), tier));
    }

    pub fn register_wasm(&mut self, name: &str, wasm_path: &str, tier: PermissionTier) -> anyhow::Result<()> {
        let wasm_tool = WasmTool::load(wasm_path, ResourceLimits::trusted())?;
        tracing::info!(tool = %name, ?tier, wasm_path, "WASM tool registered");
        self.tools.insert(name.to_string(), (ToolRuntime::Wasm(wasm_tool), tier));
        Ok(())
    }

    pub fn get_tier(&self, tool_id: &str) -> PermissionTier {
        self.tools.get(tool_id).map(|(_, t)| *t).unwrap_or(PermissionTier::Deny)
    }

    pub async fn invoke(&self, call: &ToolCall) -> ToolResult {
        match self.tools.get(&call.tool_id) {
            None => ToolResult {
                call_id: call.call_id.clone(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some(format!("Tool '{}' not found in registry", call.tool_id)),
            },
            Some((runtime, _)) => match runtime {
                ToolRuntime::Native(tool) => tool.execute(&call.params_json).await,
                ToolRuntime::Wasm(wasm_tool) => wasm_tool.execute(&call.params_json).await
                    .unwrap_or_else(|e| ToolResult {
                        call_id: call.call_id.clone(),
                        output_json: "{}".into(),
                        status: ToolStatus::Failure,
                        execution_ms: 0,
                        error_message: Some(format!("WASM execution error: {}", e)),
                    }),
            }
        }
    }
}
```

---

### 5.6 Audit Log & Merkle Chain (Stub)

#### 5.6.1 Schema (Migration `003_audit_log.sql`)

```sql
-- Migration: 003_audit_log.sql

CREATE TABLE IF NOT EXISTS audit_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type   TEXT    NOT NULL,  -- e.g. 'vault_access', 'tool_invoke', 'permission_decision', 'key_injection'
    actor        TEXT    NOT NULL,  -- 'agent' or 'user:{user_id}'
    session_id   TEXT,
    tool_id      TEXT,
    vault_scope  TEXT,              -- Scope name if vault was accessed (never the value)
    permission_tier TEXT,
    decision     TEXT,              -- 'approved', 'denied', 'auto_approved', 'timed_out'
    detail       TEXT,              -- JSON blob with additional context
    timestamp_ms INTEGER NOT NULL DEFAULT (unixepoch('now','subsec') * 1000),
    
    -- Phase 6 will fill prev_hash for full Merkle chaining
    prev_hash    TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_log_session ON audit_log(session_id, timestamp_ms);
CREATE INDEX IF NOT EXISTS idx_audit_log_type ON audit_log(event_type, timestamp_ms);
```

#### 5.6.2 AuditEmitter

```rust
// crates/hydragent-core/src/audit.rs

use sqlx::SqlitePool;
use serde_json::Value;

#[derive(Debug)]
pub enum AuditEventType {
    VaultAccess,
    ToolInvoke,
    ToolComplete,
    PermissionPrompted,
    PermissionApproved,
    PermissionDenied,
    KeyInjection,
    SandboxSpawn,
    SandboxExit,
}

pub struct AuditEmitter {
    db: SqlitePool,
}

impl AuditEmitter {
    pub fn new(db: SqlitePool) -> Self { Self { db } }

    pub async fn emit(
        &self,
        event_type: AuditEventType,
        actor: &str,
        session_id: Option<&str>,
        tool_id: Option<&str>,
        vault_scope: Option<&str>,  // NEVER the vault value
        decision: Option<&str>,
        detail: Option<Value>,
    ) -> anyhow::Result<()> {
        let event_str = format!("{:?}", event_type);
        let detail_str = detail.map(|v| v.to_string());
        
        sqlx::query!(
            r#"
            INSERT INTO audit_log (event_type, actor, session_id, tool_id, vault_scope, decision, detail)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
            event_str, actor, session_id, tool_id, vault_scope, decision, detail_str
        )
        .execute(&self.db)
        .await?;
        
        Ok(())
    }
}
```

---

### 5.7 Docker Execution Sandbox

The `code_exec` tool delegates to Docker. We use `tokio::process::Command` to spawn the container and stream output.

```rust
// crates/hydragent-sandbox/src/docker_runner.rs

use tokio::process::Command;
use std::time::Duration;

pub struct DockerRunner {
    image: String,
    timeout: Duration,
}

impl DockerRunner {
    pub fn new(image: &str, timeout_ms: u64) -> Self {
        Self {
            image: image.to_string(),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    pub async fn run_code(&self, code: &str, language: &str) -> anyhow::Result<String> {
        // Write code to a temporary file
        let tmp_dir = tempfile::tempdir()?;
        let code_path = tmp_dir.path().join(match language {
            "python" => "code.py",
            "javascript" | "node" => "code.js",
            "bash" | "sh" => "code.sh",
            _ => anyhow::bail!("Unsupported language: {}", language),
        });
        tokio::fs::write(&code_path, code).await?;
        
        let code_str = code_path.to_string_lossy().to_string();

        // Execute inside Docker with hardened flags
        let output = tokio::time::timeout(self.timeout, Command::new("docker")
            .args([
                "run",
                "--rm",
                "--network", "none",          // Zero network access
                "--read-only",                 // Root fs is read-only
                "--tmpfs", "/tmp:size=32m",   // Only /tmp writable
                "--memory", "256m",            // 256 MB RAM limit
                "--cpus", "0.5",              // 0.5 CPU core
                "--pids-limit", "50",         // Block fork bombs
                "--cap-drop", "ALL",          // Drop all capabilities
                "--security-opt", "no-new-privileges",
                "--user", "1001:1001",        // Non-root
                "-v", &format!("{}:/workspace/code:ro", code_str), // Code mounted read-only
                &self.image,
                "/usr/bin/timeout", "8",      // Inner timeout guard
                match language {
                    "python" => "python3",
                    "javascript" | "node" => "node",
                    "bash" | "sh" => "bash",
                    _ => unreachable!(),
                },
                "/workspace/code",
            ])
            .output()
        ).await??;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        
        if !output.status.success() && !stderr.is_empty() {
            Ok(format!("STDOUT:\n{}\n\nSTDERR:\n{}", stdout, stderr))
        } else {
            Ok(stdout)
        }
    }
}
```

---

### 5.8 Taint Tracking Subsystem

`TaintedString` is a newtype that makes it *impossible* to accidentally log or serialize a secret.

```rust
// crates/hydragent-vault/src/taint.rs

use zeroize::Zeroize;

/// A string that holds sensitive data. Does NOT implement:
///  - `Display` (prints "[REDACTED]")
///  - `Debug` (prints "TaintedString([REDACTED])")
///  - `Serialize` (compile error to serialize)
///
/// Use `expose_secret()` deliberately when you MUST use the raw value.
/// The call site of `expose_secret()` is your injection-point audit surface.
pub struct TaintedString(String);

impl TaintedString {
    pub fn new(s: String) -> Self { Self(s) }
    
    /// The only way to access the raw string value.
    /// Every call to this function is an intentional, auditable exposure.
    pub fn expose_secret(&self) -> &str { &self.0 }
}

impl std::fmt::Display for TaintedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl std::fmt::Debug for TaintedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TaintedString([REDACTED])")
    }
}

impl Drop for TaintedString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
```

---

### 5.9 Security Vault CLI Interface

The full `vault` subcommand tree is registered with `clap` in `main.rs`.

```
./hydragent vault init                         # Create new vault at $VAULT_PATH
./hydragent vault set <scope> <value>          # Store a secret
./hydragent vault get <scope>                  # Retrieve a secret (shows masked: ghp_xxx...xxx)
./hydragent vault list                         # List all stored scope names (no values shown)
./hydragent vault delete <scope>               # Remove a secret
./hydragent vault rotate-passphrase            # Re-encrypts vault with a new passphrase
./hydragent vault export --format json         # Export scopes (NO values) as JSON manifest
```

```rust
// main.rs vault subcommand handling

#[derive(Subcommand)]
enum VaultCommands {
    /// Initialize a new encrypted vault
    Init,
    /// Store a credential in the vault
    Set { scope: String, value: String },
    /// Retrieve and display a masked credential
    Get { scope: String },
    /// List all stored scope names
    List,
    /// Delete a specific credential
    Delete { scope: String },
    /// Re-encrypt the vault with a new passphrase
    RotatePassphrase,
}

fn handle_vault(cmd: VaultCommands, vault_path: &str) -> anyhow::Result<()> {
    match cmd {
        VaultCommands::Init => {
            let passphrase = rpassword::prompt_password("Enter new vault passphrase: ")?;
            let confirm = rpassword::prompt_password("Confirm passphrase: ")?;
            if passphrase != confirm {
                anyhow::bail!("Passphrases do not match");
            }
            Vault::create(vault_path, &passphrase)?;
            println!("✅ Vault created at: {}", vault_path);
        },
        VaultCommands::Set { scope, value } => {
            let passphrase = rpassword::prompt_password("Vault passphrase: ")?;
            let mut vault = Vault::open(vault_path, &passphrase)?;
            vault.set(&scope, &value)?;
            println!("✅ Secret '{}' stored successfully.", scope);
        },
        // ... other commands
    }
    Ok(())
}
```

---

### 5.10 Permission Gate UI (Python CLI & Telegram)

```python
# adapters/cli_adapter.py (updated)

import asyncio, sys
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Confirm

console = Console()

async def handle_permission_request(request: dict, bus: BusClient):
    """Display a permission gate prompt to the user and send back their decision."""
    tool_id = request["tool_id"]
    summary = request["params_summary"]
    request_id = request["request_id"]
    
    console.print()
    console.print(Panel(
        f"[bold yellow]⚠️  Approval Required[/bold yellow]\n\n"
        f"Tool: [bold]{tool_id}[/bold]\n"
        f"Action: {summary}\n\n"
        f"[dim]This action will modify state. Approve?[/dim]",
        border_style="yellow",
        title="[bold yellow]Permission Gate[/bold yellow]"
    ))
    
    try:
        approved = Confirm.ask("[cyan]Allow this action?[/cyan]", default=False)
    except (KeyboardInterrupt, EOFError):
        approved = False
        console.print("[red]Interrupted — action denied.[/red]")
    
    await bus.send_permission_response({
        "request_id": request_id,
        "approved": approved
    })
    
    if approved:
        console.print("[green]✅ Action approved.[/green]")
    else:
        console.print("[red]❌ Action denied.[/red]")
    console.print()
```

---

## 6. Built-in Tools (Phase 3 Additions)

Phase 3 adds two powerful but tightly controlled tools.

### `code_exec`

```yaml
name: code_exec
description: "Execute a code snippet in a sandboxed Docker container with no network access and no host filesystem access. Supported languages: python, javascript, bash."
tier: prompt  # Always requires explicit user approval
params_schema:
  type: object
  required: [code, language]
  properties:
    code:
      type: string
      description: "The code to execute."
    language:
      type: string
      enum: [python, javascript, bash]
      description: "Programming language for the code snippet."

security:
  - Runs in Docker with: --network none --read-only --memory 256m --cpus 0.5 --pids-limit 50 --cap-drop ALL
  - Maximum execution time: 10 seconds
  - No host filesystem access (code mounted read-only at /workspace/code)
  - Requires user approval (PermissionTier::Prompt)
```

**Output format**:
```json
{
  "exit_code": 0,
  "stdout": "Hello, World!\n",
  "stderr": "",
  "execution_ms": 823,
  "language": "python"
}
```

---

### `vault_get` (internal tool, LLM-accessible with placeholder awareness)

The agent can reference vault secrets via placeholders in tool params — but never retrieves raw values itself.

```yaml
name: vault_get
description: "Insert a vault credential placeholder into the next tool call's parameters. The actual value is injected at the network boundary — never exposed to the LLM."
tier: prompt  # Still requires user consent to use credentials
params_schema:
  type: object
  required: [scope, target_param]
  properties:
    scope:
      type: string
      description: "The vault scope name (e.g., 'github.token')"
    target_param:
      type: string
      description: "The parameter name in the next tool call to inject into"
```

---

## 7. Configuration & Environment

### Updated `.env.example`

```ini
# ── Phase 3: Security Vault ────────────────────────────────────────────────

# Path to the encrypted vault file
VAULT_PATH=./config/vault/.hydravault

# Vault passphrase loaded from environment (or use `hydragent vault` CLI)
# WARNING: if set here, use a secrets manager in production, never commit this file
VAULT_PASSPHRASE=

# ── Phase 3: Permission Gate ───────────────────────────────────────────────

# Seconds before an un-answered permission prompt auto-denies
PERMISSION_TIMEOUT_SEC=60

# ── Phase 3: WASM Sandbox ──────────────────────────────────────────────────

# Directory where .wasm tool modules are stored
SANDBOX_TOOLS_DIR=./sandbox/tools

# Maximum fuel per WASM tool execution (1B ≈ 1 s CPU)
WASM_MAX_FUEL=1000000000

# Maximum wall-clock time per WASM tool execution
WASM_TIMEOUT_MS=10000

# Maximum memory per WASM tool execution
WASM_MAX_MEMORY_MB=64

# ── Phase 3: Docker Sandbox ────────────────────────────────────────────────

# Docker image for code_exec tool
CODE_EXEC_DOCKER_IMAGE=hydragent-sandbox:latest

# Max execution time for Docker code runs
CODE_EXEC_TIMEOUT_MS=10000
```

### `config/permissions.yaml` — Per-Tool Tier Overrides

This file lets operators change the default tier for any tool without modifying code.

```yaml
# config/permissions.yaml
# Override the default permission tier for any registered tool.
# Valid values: auto_approve | prompt | deny

tools:
  web_search: auto_approve      # No user approval needed for web searches
  file_read: auto_approve       # Safe: scoped to workspace
  file_write: prompt            # Requires approval: modifies disk state
  code_exec: prompt             # Requires approval: executes code
  memory_store: auto_approve    # Passive action, no approval needed
  memory_forget: prompt         # Destructive: requires approval
  vault_get: prompt             # Credential usage requires approval
  system_shutdown: deny         # Always blocked
```

---

## 8. Testing Strategy

### 8.1 Unit Tests

| Test File | What It Covers |
|---|---|
| `vault_test.rs` | Encrypt/decrypt round-trip; Argon2id KDF determinism with fixed salt; wrong-passphrase returns Err; TaintedString never exposes in Debug/Display; Vault::drop zeroizes memory. |
| `wasm_sandbox_test.rs` | Module loads successfully; `echo.wasm` returns correct output; network syscall from inside WASM returns EBADF; fuel exhaustion returns `ToolStatus::Timeout`; memory overuse returns `ToolStatus::Failure`. |
| `taint_test.rs` | `TaintedString::fmt()` outputs `[REDACTED]`; `serde_json::to_string(&TaintedString::new("secret"))` is a compile error; Drop zeroes the inner string. |
| `permission_gate_test.rs` | AutoApprove returns `true` without waiting; Deny returns `false` immediately; Prompt with simulated approval returns `true`; Prompt with timeout auto-denies within 100 ms test window. |
| `injector_test.rs` | Placeholder `{{github.token}}` replaced in serialized JSON; LLM message body never contains raw key after injection; audit log records injection event. |

### 8.2 Integration Tests

```bash
# tests/integration/vault_cli_test.sh

#!/bin/bash
set -e

# 1. Init vault
echo "mypassphrase" | ./hydragent vault init --non-interactive

# 2. Store a secret
./hydragent vault set github.token "ghp_faketoken" --passphrase mypassphrase

# 3. Retrieve (verify masked output)
OUTPUT=$(./hydragent vault get github.token --passphrase mypassphrase)
echo "$OUTPUT" | grep -q "ghp_" && echo "FAIL: raw token visible" && exit 1
echo "$OUTPUT" | grep -q "\*\*\*" && echo "PASS: token masked"

# 4. Delete
./hydragent vault delete github.token --passphrase mypassphrase

# 5. Verify deletion
OUTPUT=$(./hydragent vault get github.token --passphrase mypassphrase 2>&1)
echo "$OUTPUT" | grep -q "not found" && echo "PASS: deletion confirmed"
```

```rust
// tests/integration/sandbox_e2e_test.rs

#[tokio::test]
async fn test_code_exec_no_network() {
    // This test verifies that network syscalls from within Docker are blocked.
    let runner = DockerRunner::new("hydragent-sandbox:latest", 10000);
    let result = runner.run_code(
        "import urllib.request; urllib.request.urlopen('http://example.com')", 
        "python"
    ).await.unwrap();
    
    // Should fail with network error, not succeed
    assert!(result.contains("Network is unreachable") || result.contains("OSError") || result.contains("ConnectionRefusedError"));
}

#[tokio::test]
async fn test_code_exec_hello_world() {
    let runner = DockerRunner::new("hydragent-sandbox:latest", 10000);
    let result = runner.run_code("print('hello from sandbox')", "python").await.unwrap();
    assert!(result.contains("hello from sandbox"));
}
```

### 8.3 Manual QA Checklist (Phase 3 Sign-off)

```
[ ] Run `./hydragent vault init` → `.hydravault` file created
[ ] Run `./hydragent vault set serpapi.key sk_test_123` → "Secret stored"
[ ] Run `./hydragent vault list` → shows "serpapi.key" (not the value)
[ ] Ask agent: "Search for Rust news" → agent uses {{serpapi.key}} placeholder → web_search succeeds with key injected
[ ] Grep ALL tracing logs for "sk_test_123" → MUST find zero matches
[ ] Ask agent: "Write a file called output.txt with the word hello"
    → Permission gate appears with yellow banner
    → Type "n" → agent says "Action denied"
    → Type "y" → file is written
[ ] Ask agent: "Run Python: print('hello')" → Docker container spawns → output visible
[ ] In Docker test: "Run Python: import socket; socket.connect(('google.com', 80))" → error, no network
[ ] Check audit_log table → all events present for above test runs
[ ] `cargo test --workspace` exits 0 with zero warnings
[ ] `pytest adapters/` exits 0
```

---

## 9. Performance Targets

| Metric | Target | Measurement Method |
|---|---|---|
| Vault open (cold, disk read) | < 500 ms | Argon2id with m=65536 takes ~300 ms on modern CPU |
| Vault get (after open, warm) | < 1 ms | In-memory HashMap lookup |
| WASM tool invocation overhead | < 5 ms | Benchmark: `echo.wasm` vs native `EchoTool` |
| Permission gate auto-approve path | < 1 ms | Unit test timing |
| Permission gate prompt path | Human I/O limited | N/A (bounded by 60 s timeout) |
| Key injection overhead | < 1 ms | String replace on serialized JSON body |
| Docker container cold start | < 3 s | `docker run` latency on developer machine |
| Audit log write | < 5 ms | SQLite WAL mode insert |
| Taint propagation check | < 0.1 ms | Rust type system (compile-time, no runtime cost) |

---

## 10. Security Threat Model

### 10.1 Assets to Protect

| Asset | Sensitivity | Protection Mechanism |
|---|---|---|
| API keys, OAuth tokens | Critical | XChaCha20-Poly1305 vault, TaintedString, zeroize on drop |
| Vault passphrase (in memory) | Critical | `rpassword` (no echo), never stored in vault file, `zeroize` after KDF |
| User conversation content | High | Local SQLite only, never logged to external services |
| Session history | Medium | SQLite WAL, encrypted at rest if `VAULT_ENCRYPT_DB=true` |
| Audit log | Medium | Append-only SQLite, future Merkle chaining in Phase 6 |

### 10.2 Threat Actors

| Threat | Attack Vector | Mitigation |
|---|---|---|
| **Prompt Injection** | Malicious tool output containing `{{vault_scope}}` placeholders | KeyInjector ONLY runs on `system` and `tool_result` fields, never `user` message fields |
| **Path Traversal via Tool** | `file_read` with `../../etc/passwd` | WASI preopened directory scope + native canonicalize check |
| **LLM Exfiltration** | Agent logs secrets in its response | TaintedString compile-time type prevents serialization; no raw key ever enters LLMRequest |
| **Fork Bomb in code_exec** | `while True: os.fork()` | `--pids-limit 50` in Docker |
| **Network Exfiltration from sandbox** | Code reads `/etc/secrets` and POSTs externally | `--network none` Docker flag; WASI no network linker |
| **Vault Brute Force** | Offline dictionary attack on `.hydravault` | Argon2id m=65536 makes each guess ~300 ms even on GPU clusters |
| **Memory Scraping** | Core dump analysis after crash | `zeroize` wipes all secrets on Drop; future Phase 6: mlock |

---

## 11. Risks & Mitigations

| Risk | Category | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| **`wasmtime` JIT overhead** | Performance | Medium | Medium | Pre-compile modules to `.cwasm` AOT format at startup. Cache `Module` instances in an `Arc`. |
| **Docker unavailable in CI** | DevEx | High | Low | Unit tests mock `DockerRunner` via trait injection. Docker tests are gated with `#[cfg(feature="docker_tests")]`. |
| **Argon2id taking >1 s on vault open** | UX | Medium | Low | Allow configuring `m` and `t` params via `.env`. Default is tuned for ~300 ms on modern hardware. |
| **WASM WASI version drift** | Build | Low | Medium | Pin `wasmtime` and `wasmtime-wasi` to the same minor version. Use `[workspace.dependencies]` to enforce this. |
| **Permission timeout UX** | UX | Low | Low | 60 s default is generous. If agent is processing a multi-step task, we emit a heartbeat to keep the connection alive. |
| **TaintedString compile-time complexity** | DevEx | Low | Low | Provide a clear `CONTRIBUTING.md` section on the taint API. `expose_secret()` call sites must have a `// SAFETY:` comment. |

---

## 12. Definition of Done

Phase 3 is complete when **all** of the following conditions are met:

### Code Quality
- [ ] `cargo build --workspace` and `cargo test --workspace` both exit 0 with `RUSTFLAGS="-D warnings"`.
- [ ] `cargo clippy --workspace -- -D warnings` exits 0.
- [ ] All `unsafe` blocks have a `// SAFETY:` comment justifying them. No new `unsafe` in Phase 3 code paths (WASM runtime handles unsafe internally).
- [ ] Every `expose_secret()` call has a `// SECURITY:` comment explaining the injection point.

### Security Verification
- [ ] `grep -r "expose_secret" crates/` returns ≤ 3 results (one in `injector.rs`, one in vault CLI `get` display, one in test).
- [ ] A full-text scan of all `tracing::*` call sites confirms no raw vault value can leak into logs.
- [ ] TaintedString does not implement `Serialize` — verified by trying `serde_json::to_string(&TaintedString::new("x"))` and confirming compile error.

### Functional
- [ ] `./hydragent vault {init,set,get,list,delete}` all work correctly.
- [ ] All 3 Phase 1 tools (`web_search`, `file_read`, `echo`) execute inside `wasmtime` WASM in production path.
- [ ] `code_exec` tool runs Python hello-world correctly in Docker.
- [ ] `code_exec` with network call inside Docker produces expected error (not network success).
- [ ] Permission gate: auto-approve tier executes without pause; prompt tier waits for user; deny tier always rejects.
- [ ] Audit log table populated with correct event types after a full ReAct turn.

### Documentation
- [ ] `ARCHITECTURE.md` updated with the new 7-layer security diagram showing WASM and vault layers.
- [ ] `crates/hydragent-vault/VAULT_FORMAT.md` documents the binary wire format.
- [ ] `crates/hydragent-sandbox/WASM_ABI.md` documents the `tool_execute` WASM function ABI.
- [ ] `PHASE_3.md` (this file) reviewed and reflects actual implementation.

### Release
- [ ] `v0.3.0` git tag created.
- [ ] `CHANGELOG.md` entry for v0.3.0 written.
- [ ] All Phase 1 and Phase 2 tests remain green (zero regressions).

---

*Previous phase: [PHASE_2.md](PHASE_2.md) — Hierarchical Memory & Retrieval-Augmented Thinking (Weeks 7–10)*
*Next phase: [PHASE_4.md](PHASE_4.md) — 40+ Channel Gateway, Proactive Heartbeat & Work IQ (Weeks 15–18)*
