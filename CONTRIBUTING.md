# Contributing to Hydragent

> **Welcome!** This guide is for people who have already gotten
> `hydragent chat` working and now want to add code. If you haven't
> gotten that far yet, start with [`ONBOARDING.md`](ONBOARDING.md).
>
> **Last verified against working tree**: 2026-06-16.
> **Ground truth for what's in the code**: [`doc/STATE.md`](doc/STATE.md).

---

## 0. The 30-second shape of the codebase

```
hydragent/
├── crates/
│   ├── hydragent-core/        ← THE kernel binary. No library target.
│   ├── hydragent-tools/       ← ReAct tools the LLM can invoke.
│   ├── hydragent-bus/         ← JSON-RPC server + protocol.
│   ├── hydragent-memory/      ← SQLite + FTS5 + vector retrieval.
│   ├── hydragent-security/    ← 16-layer pipeline (Tracks 6.1–6.4).
│   ├── hydragent-vault/       ← XChaCha20-Poly1305 + Argon2id.
│   ├── hydragent-skills/      ← Skill library + 7-day Curator.
│   ├── hydragent-planner/     ← DAG task decomposition.
│   ├── hydragent-swarm/       ← Subagent runtime + Model Council routing.
│   ├── hydragent-bench/       ← SKILL-BENCH 80 tasks + Golden Set 30 pairs.
│   └── (6 more — see doc/STATE.md §1.1)
├── adapters/                  ← Python channel adapters + the Python SDK
│   ├── hydragent_py/          ← Official Python SDK package
│   ├── telegram_adapter.py    ← Channel adapter (one per platform)
│   ├── discord_adapter.py
│   ├── slack_adapter.py
│   ├── email_adapter.py
│   ├── webhook_adapter.py
│   ├── websocket_adapter.py
│   ├── cli_adapter.py
│   └── bus_client.py          ← Shim re-exporting hydragent_py.BusClient
├── skills/builtin/            ← Drop a YAML here to ship a built-in skill
├── config/
│   ├── SOUL.md                ← Agent personality (read at startup)
│   ├── USER.md                ← User profile (read at startup)
│   └── model_council.yaml     ← 20+ model profiles for routing
├── doc/                       ← All design + state docs
├── Hydragent.cmd              ← Windows single-entry point
├── ONBOARDING.md              ← (you are here → onboarding next)
└── .env.example               ← Every supported env var
```

**Four rules of thumb** for figuring out where a change belongs:

1. **Does the LLM need to invoke it as a tool?** → Rust tool in `crates/hydragent-tools/`.
2. **Is it a Telegram/Discord/Slack/… message gateway?** → Python adapter in `adapters/`.
3. **Is it a prompt the LLM should follow in a known pattern?** → YAML skill in `skills/builtin/`.
4. **Is it a top-level CLI command (`hydragent foo`)?** → New module `crates/hydragent-core/src/<foo>.rs`, wired into the `Commands` enum in `main.rs`. Mirror `update.rs` / `uninstall.rs` for the standard `run()` signature + CLI flag shape.

Everything else (memory, security, swarm, planner, bus, vault) has a
single owning crate. Open it, follow the module names, you can't get lost.

---

## 1. Building, testing, and the dev loop

### Build

```powershell
.\Hydragent.cmd           # auto-detects: builds if missing, otherwise launches
# or
cargo build               # full workspace (slower)
cargo build -p hydragent-core   # kernel only (what the cmd file does)
cargo build --release     # optimised, ~76 MB binary in target\release\
```

The kernel binary lives at `target\debug\hydragent.exe` (or
`target\release\hydragent.exe`). It is the only binary you ever run.
There is no `hydragent-server` or `hydragent-edge` binary despite what
older docs claim — see [`doc/STATE.md`](doc/STATE.md) §2 for the truth.

### Test

```powershell
# Rust kernel — there is NO library target, all tests are in the binary
cargo test -p hydragent-core --bin hydragent

# A specific crate
cargo test -p hydragent-vault
cargo test -p hydragent-security

# Bench harness
cargo test -p hydragent-bench

# Python e2e — needs a running bus on 127.0.0.1:5000
python tests\start_bus.py            # background, writes tests\.bus.pid
python tests\cli_user_pov.py         # 4-prompt smoke
python tests\test_searchxng_e2e.py   # web_search + React loop
```

**49 unit tests** live in the kernel binary at the time of writing
(see `crates/hydragent-core/src/cli_repl.rs::tests` for the reasoning
detector suite). Add new `#[cfg(test)] mod tests` blocks at the
bottom of whatever file you're editing.

### Run

```powershell
# Interactive REPL (CLI adapter built in)
.\target\debug\hydragent.exe chat

# Headless bus server (Python adapters connect to 127.0.0.1:5000)
.\target\debug\hydragent.exe

# One-shot brain check
.\target\debug\hydragent.exe test-brain

# Diagnostics (no network)
.\target\debug\hydragent.exe doctor

# Self-update to the latest GitHub Release
.\target\debug\hydragent.exe update

# Uninstall (interactive confirmation; -y skips it)
.\target\debug\hydragent.exe uninstall
```

---

## 2. Code conventions

### Rust

- **Edition 2021**, `resolver = "2"`, workspace deps in root `Cargo.toml` only.
- **No library target for `hydragent-core`**. Tests live in the binary's
  `#[cfg(test)] mod tests`. If you find yourself adding a `lib.rs` to
  `hydragent-core` to expose a helper, the helper belongs in one of the
  other crates instead.
- **Module names use `snake_case`**; struct/enum names use
  `PascalCase`; constants use `SCREAMING_SNAKE_CASE`; methods on the
  `Tool` trait return `&'static str` for name/description/schema.
- **Async traits use `#[async_trait]`** (we're on tokio 1.x stable;
  not yet on stable async-fn-in-trait).
- **Errors**: `anyhow::Result` at module boundaries, typed enums in
  the hot path. Don't `unwrap()` in production paths — use `.context()`
  with anyhow.
- **Logging**: `tracing` macros (`info!`, `warn!`, `error!`,
  `debug!`). The kernel's chat-mode log level defaults to `error` so
  the REPL stays quiet; override with `HYDRAGENT_CHAT_LOG=warn|info|debug`.
- **JSON Schema for tool params** is a `&'static str` literal returned
  from `params_schema()`. It is injected verbatim into the system
  prompt — keep it readable, the LLM reads it raw.
- **No raw credentials in any `params_json`**. The vault injects
  secrets at the network boundary; the LLM only ever sees
  `Authorization: Bearer {{GITHUB_TOKEN}}` placeholders. The dispatcher
  resolves them. This is non-negotiable — see Phase 3 / axiom 3.

### Python

- **3.11+ syntax** (`from __future__ import annotations` if you want
  forward refs), `asyncio` for I/O.
- **Talk to the kernel through `hydragent_py`** (the SDK), not raw
  sockets. The legacy `adapters/bus_client.py` shim is kept for
  back-compat — new code should `from hydragent_py import HydraClient`.
- **One file per channel** in `adapters/<platform>_adapter.py`.
- **No global mutable state** — adapters are CLI-run scripts that
  own a single `HydraClient` in `__main__`.
- **Type hints** on public functions; runtime validation only where
  the LLM could send a malformed payload.
- **No `print()` for logging** — use `logging.getLogger(__name__)`.

### YAML (skills)

- **`id` must be unique and prefixed `skill-builtin-*`** for shipped
  skills (e.g. `skill-builtin-debug-rust-error`). User-authored
  skills get a different prefix from the loader.
- **`tier` is `"active"`** (auto-runnable) or `"draft"` (not yet
  eligible for the curator). New skills ship as `"active"`.
- **`required_tools`** is a hint to the LLM — list the tool names
  the skill is *likely* to need. The runtime doesn't enforce it; the
  LLM uses the hint to decide what context to pull.
- **`prompt_template` is the entire prompt** the LLM sees. Variables
  are `{{param_name}}` and must match the keys declared under `params`.
- **No raw secrets in the prompt** — same rule as Rust. If the skill
  needs a token, the tool it calls pulls it from the vault.

### Cross-cutting

- **`page_id` is canonical.** The field is `page_id` everywhere in
  Rust types, the bus wire, and the adapters. **Do not introduce a
  `session_id` field.** (`doc/STATE.md` §2.1 documents why; older
  diagrams that use `session_id` are out of date.)
- **Replies to LLM tool calls return `ToolResult { status, output_json,
  error_message, … }`**. The `status` is one of `Success | Failure |
  Timeout`. Don't synthesise a fake success with the error in the body
  — the orchestrator routes on `status`.
- **Audit everything that has a `PermissionTier::Prompt` or
  `Deny` outcome.** Phase 6 Track 6.1 (Merkle chain) is live; taint
  propagation is tracked in the security crate.

---

## 3. Adding a Rust tool (the most common contribution)

A "tool" is something the LLM can invoke during the ReAct loop. There
are 12 shipped today (`echo`, `web_search`, `file_read`,
`memory_store`, `memory_search`, `memory_forget`, `standing_orders`,
`user_profile`, `send_message`, `schedule_task`, `rss_subscribe`, and
the Phase 6 security tools). Phase 7 added three skill-library tools.

### File map

| File | Purpose |
|---|---|
| `crates/hydragent-tools/src/<your_tool>.rs` | The tool implementation. |
| `crates/hydragent-tools/src/lib.rs` | Add `pub mod <your_tool>;` (alphabetical is fine). |
| `crates/hydragent-core/src/main.rs` | Import + `registry.register(...)` next to peers. |
| `crates/hydragent-core/src/cli_repl.rs::tests` | Add unit tests if the tool has interesting pure-Rust logic. |

### Skeleton

Mirror `echo.rs` (the smallest possible tool). The full skeleton is:

```rust
// crates/hydragent-tools/src/my_tool.rs
use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus};
use crate::tool_trait::Tool;
use serde_json::Value;

pub struct MyTool {
    // share state via Arc — the registry holds Arc<dyn Tool>
    // so anything heavy must be wrapped in Arc.
}

impl MyTool {
    pub fn new() -> Self { Self {} }
}

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "One-line description the LLM sees." }
    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "input": { "type": "string", "description": "..." }
            },
            "required": ["input"]
        }"#
    }
    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::AutoApprove   // or Prompt / Deny
    }
    async fn execute(&self, params_json: &str) -> ToolResult {
        let val: Value = match serde_json::from_str(params_json) {
            Ok(v) => v,
            Err(e) => return ToolResult {
                call_id: String::new(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some(format!("Invalid parameters: {}", e)),
            },
        };
        // … do the work …
        ToolResult {
            call_id: String::new(),
            output_json: serde_json::to_string(&serde_json::json!({
                "result": "..."
            })).unwrap_or_default(),
            status: ToolStatus::Success,
            execution_ms: 0,
            error_message: None,
        }
    }
}
```

### Registration

In `crates/hydragent-core/src/main.rs`, add next to the existing
`registry.register(...)` lines (see lines 1734–1814 for the
`register_tool()` function):

```rust
use hydragent_tools::my_tool::MyTool;
// … in the tool-build function …
registry.register(MyTool::new());
```

That's it. The next `cargo build` picks it up; the next chat turn
shows the tool in the system prompt; the LLM can call it.

### When the tool does filesystem or network I/O

If your tool touches the host filesystem or makes an outbound request,
read the `permission_tier` documentation first:

| Tier | Behaviour |
|---|---|
| `AutoApprove` | Runs immediately. Use only for read-only or fully-reversible actions. |
| `Prompt` | The REPL will show a y/N prompt *before* the call. Use for state-mutating actions (writes, posts, deletes, sends). |
| `Deny` | Hard-blocked. Use sparingly — for things that are never safe (e.g. raw-shell if you had it). |

`echo` is `AutoApprove`. `send_message` is `Prompt`. There is no
`Deny` tool shipped today.

---

## 4. Adding a built-in skill (no code required)

Skills are pure YAML. The loader at `crates/hydragent-skills/` walks
`skills/builtin/*.yaml` at startup and inserts each into the SQLite
skill library. No rebuild required for YAML-only changes — restart the
REPL.

### File

Create `skills/builtin/<your-skill>.yaml`. Follow the schema from
[`skills/builtin/debug-rust-error.yaml`](skills/builtin/debug-rust-error.yaml)
or the other two shipped skills. Required keys:

| Key | Notes |
|---|---|
| `id` | Unique, `skill-builtin-*` prefix. |
| `name` | Snake-case display name (no spaces). |
| `version` | Bump on prompt changes. |
| `description` | One line. The LLM uses this to decide *when* to invoke the skill — write it for a model that has never seen it. |
| `tier` | `"active"` to ship, `"draft"` to hide from curator. |
| `capability_tags` | For retrieval. 2-5 tags, snake-case. |
| `params` | Each entry: `name`, `type` (string/number/object), `description`, `required`. |
| `prompt_template` | The whole prompt. Use `{{param_name}}` for variables. |
| `required_tools` | Hint to the LLM. Don't lie here. |
| `success_examples` | 1-3 worked examples. The induction engine uses these. |
| `author`, `created_at`, `last_updated`, `success_rate`, `execution_count` | Bookkeeping; the loader fills some of them in. |

### After adding

Restart `hydragent chat`. The skill is now queryable via the
`skill_list` / `skill_search` / `skill_run` tools. The 7-day Curator
(`hydragent-skills/src/curator.rs`) will start collecting success
metrics from the first invocation.

---

## 5. Adding a channel adapter (Python)

A channel adapter is a long-running script that:

1. Connects to the bus on `127.0.0.1:5000` via `HydraClient`.
2. Receives inbound messages from the platform.
3. Wraps them in `IntentEvent`s and sends `intent.submit`.
4. Receives `AgentResponse`s and renders them back through the platform.

The canonical skeleton is `adapters/cli_adapter.py` (the simplest
one) and the most featureful is `adapters/telegram_adapter.py`
(MarkdownV2 escaping, Mini App, etc.).

### File

Create `adapters/<platform>_adapter.py`. Use `adapters/bus_client.py`
or the higher-level `hydragent_py.HydraClient` — both are equivalent
in v0.7.1; new code should prefer the SDK.

### Wire the env var

Add the required tokens to `.env.example` (see §7 below) and read
them via `os.environ`. Document the new env var in the
`adapters/README.md` "Channel adapters" section.

### Don't forget the `__main__`

Every adapter must be runnable directly: `python
adapters/<platform>_adapter.py`. This is how `Hydragent.cmd` (or
production deployments) launch it.

---

## 6. Adding a bus RPC method (Rust)

The bus speaks JSON-RPC 2.0 over TCP on `127.0.0.1:5000`. If you want
to expose a kernel capability to non-LLM clients (adapters, the SDK,
custom scripts), you add a router method.

### File map

| File | Purpose |
|---|---|
| `crates/hydragent-bus/PROTOCOL.md` | Document the new method (params schema, return schema, errors). |
| `crates/hydragent-core/src/orchestrator.rs` (or a sibling) | Add the handler struct + `Handler` impl. |
| `crates/hydragent-core/src/main.rs` | `router.register("my.method", MyHandler { … });` next to the existing entries (lines 2086+). |

### Skeleton

Mirror the existing `MemoryListHandler` pattern in
`crates/hydragent-core/src/orchestrator.rs`. The handler implements
the bus `Handler` trait (deserialize `params`, do work, serialize
`Response`). Errors are JSON-RPC standard error codes
(`-32601` method-not-found, `-32602` invalid-params, etc.).

### After adding

Document the new method in `crates/hydragent-bus/PROTOCOL.md`. The
Python SDK in `adapters/hydragent_py/` doesn't auto-generate a typed
wrapper for new methods; if you want one, add a method to
`adapters/hydragent_py/client.py` that wraps the bus call.

---

## 7. Updating `.env.example` and the env contract

Whenever you add a new env var:

1. Add it to `.env.example` with **a comment explaining the default,
   valid values, and a common pitfall**. Look at the existing
   comments — they're terse but concrete.
2. Add a `pub const DEFAULT: &str = …` (or equivalent) in
   `crates/hydragent-core/src/config.rs` and consume it where the
   kernel reads the var.
3. If the env var is consumed by a channel adapter, document it in
   `adapters/README.md` too.
4. The `.env.example` is the **only** place env vars are documented
   centrally. Don't add a second copy anywhere else.

---

## 8. Testing policy

- **Every new Rust tool must have at least one unit test** in the
  binary's `#[cfg(test)] mod tests` that exercises the success path
  and at least one failure path (e.g. malformed `params_json`).
- **Skill YAML changes don't need a test**, but if you add a new
  *required param* or change the prompt semantics, the
  `crates/hydragent-skills/tests/` integration tests may need
  updating.
- **Bus RPC additions** should add a Python e2e in `tests/` (the
  pattern from `test_ws_push_e2e.py` is the template).
- **Adapter changes** that touch the wire format should be covered
  by the corresponding `tests/smoke_*.py` if one exists.

The full test count is the count after `cargo test --workspace`. As
of v0.7.1 the kernel binary alone runs 49 tests; the workspace total
is higher. **Don't update the README's "X tests" number — that
auto-fills from `cargo test` output in CI.** If a doc claims a
specific count, the doc is wrong, not the count.

---

## 9. Before you open a PR

Run the full kernel test suite and a manual chat smoke:

```powershell
cargo test -p hydragent-core --bin hydragent 2>&1 | Select-String "test result"
.\target\debug\hydragent.exe doctor
.\target\debug\hydragent.exe test-brain
```

If your change touches the REPL, the bus protocol, or a tool, also:

```powershell
# Start the bus and the CLI adapter
python tests\start_bus.py
python adapters\cli_adapter.py
# In another shell:
python tests\cli_user_pov.py
```

Your PR description should answer:

1. **What does this change?** (one sentence)
2. **Why?** (link to the issue or `doc/ROADMAP.md` entry)
3. **What did you test?** (paste the test command + pass count)
4. **Does it need a doc update?** (If you touched a public env var,
   a tool's behaviour, or a bus method, yes.)

---

## 10. Where to ask

- **Open an issue** on the GitHub repo with a minimal repro and the
  output of `hydragent doctor`.
- **`doc/STATE.md`** is the ground truth — if your change conflicts
  with it, update the doc, not the code (unless the doc is what's
  wrong, in which case update the doc and call it out in the PR).
- **`doc/ROADMAP.md`** for what *should* exist (vs. what *does*
  exist) — useful for "is this already planned?" questions.
