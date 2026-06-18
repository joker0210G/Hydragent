# Hydragent — The Perfect Plan

> **Status**: Draft v1 — 2026-06-15
> **Audience**: The Hydragent maintainer (you), and future contributors
> **Sources**: `doc/ARCHITECTURE.md`, `doc/ROADMAP.md`, `doc/STATE.md`, `doc/FEATURES.md`,
> `doc/RaD/chatgpt.md`, `doc/RaD/claude.md`, `doc/RaD/deepseek.md`, `doc/RaD/gemini.md`,
> `PHASE_7_COMPLETION_SUMMARY.md`, `RELEASE_NOTES_v0.7.0.md`, the v0.7.0 chat-testing
> session findings, and a synthesis of 30+ current-gen agents in the wild.

This document is the **single source of truth** for the next two milestones
(v0.7.1 polish → v0.8.0 Edge) and the strategic plan for Phase 9 (Enterprise).
It does three things, in order:

1. **Resolves the open architectural question**: what to do with the Python
   adapters, and especially with `cli_adapter.py`.
2. **Lays out Phase 8 (Edge) and Phase 9 (Enterprise)** as concrete, shippable
   milestones with the smallest possible surface area.
3. **Defines the **v0.7.1 polish release** that must ship first** — the small,
   user-facing fixes (REPL output, file logging, E0382) that make v0.7.0
   feel finished and give us a clean base for the Edge work.

---

## 0. TL;DR

| Milestone | Codename | Scope | Ships |
|---|---|---|---|
| **v0.7.1** | *Hydra Shine* | REPL polish, log routing, refactor `cli_adapter.py` → `hydragent_py` SDK, wire skill_* to swarm, fix pre-existing test | ~3–4 days |
| **v0.8.0** | *Hydra Edge* | Zig edge compiles, PicoLM 4-bit GGUF, ESP32-S3 binary, offline skill subset, MQTT adapter, OTA push | ~3–4 weeks |
| **v0.9.0** | *Hydra Enterprise* | Multi-tenant, RBAC, HaluMem bench, SOC 2 controls, public GitHub release, Hydra Hub | ~4–6 weeks |

The strategic positioning — **Rust-first, multi-channel, security-hardened, edge-deployable** — is a niche nobody else is filling. OpenClaw is Node.js, ZeroClaw is 5 MB but single-language, NullClaw is 678 KB but Zig-only, Hermes is Rust + multi-channel but doesn't ship edge binaries, MimiClaw is bare-metal C. **Hydragent is the only project that does Rust-core + Zig-edge + Python-channels in a single repo with a real security model.**

The **kernel / frontend / SDK split** is the new architectural commitment for
v0.7.1: the Rust kernel is the single source of truth for the agent runtime;
the SDK is the canonical Python surface for every non-Rust extension point;
and every frontend (Rust REPL, TUI, web mini-app, Rich REPL, Jupyter, custom
script) is just a thin client of the kernel over the JSON-RPC bus.

---

## 1. The architectural question: Python adapters

### 1.1 What the research says

The RaD synthesis (`doc/RaD/chatgpt.md` + `doc/RaD/deepseek.md` + `doc/RaD/gemini.md`)
reviewed 30+ agents. Two clear patterns emerge:

- **Python is the lingua franca for channel adapters.** `python-telegram-bot`,
  `discord.py`, `slack-bolt` for Python, `imaplib`/`smtplib` in the stdlib — all
  the messaging SDKs are Python-first or Python-most-mature. Every multi-channel
  agent in the comparison table (OpenClaw, Hermes, Moltis, Vellum, QwenPaw)
  uses Python for its channel layer.
- **Python is *not* used for the core agent runtime** in any of the modern
  production agents. The runtime is Rust (ZeroClaw, OpenFang, Moltis, IronClaw),
  Go (PicoClaw, Goclaw), or TypeScript (OpenClaw, Manus). The reason is
  startup latency, memory footprint, and the inability to enforce compile-time
  security invariants in Python.

Hydragent's split is therefore **exactly aligned with the rest of the industry**:

| Layer | Language | Why |
|---|---|---|
| Core runtime | Rust (16 crates) | Type-safety, security gates, async, fast cold start |
| Edge binary | Zig | Tiny, statically-linked, cross-compiles to RISC-V / ARM / x86 |
| Channel adapters | Python | Ecosystem (python-telegram-bot, discord.py, slack-bolt) |

### 1.2 The `cli_adapter.py` exception

`cli_adapter.py` is **not a channel adapter** — it duplicates the Rust REPL's
user-facing surface (read-from-stdin, render markdown via `rich`, call
`bus_client.send_intent(...)`, handle `response.permission_request`).

The Rust REPL (`crates/hydragent-core/src/cli_repl.rs`) does the same thing
*with the additional advantages* that:

- It calls `run_react_loop` directly — no JSON-RPC marshalling, no bus round-trip
- It has access to the in-process `SessionStore`, `ModelRouter`, and
  `ToolRegistry` without any serialization
- It can stream tokens from `run_react_loop` directly into stdout without
  crossing an FFI / IPC boundary
- It works offline (no bus required)

### 1.3 Decision (revised after maintainer pushback)

The original draft said "**delete** `cli_adapter.py`" because the maintainer
asked the question as a binary choice (Rust vs Python). The maintainer
subsequently pushed back: *"python has large community and package library,
we can make more customization to the CLI then rust"*. That pushback is
correct — and the industry research supports it:

- **Open Interpreter** ships a Python REPL because the data-science community
  lives in notebooks, and Rich/Textual are Python-first.
- **Aider** keeps its CLI in Python for the same reason — fast iteration
  on prompt-rendering, theme plugins, repo-map visualizations.
- **Letta** exposes a Python SDK (its `letta-client`) in addition to its
  TypeScript UI because the agent's data lives in Python notebooks.
- **OpenHands** keeps a Python REPL alongside its TypeScript frontend for
  exactly the same reason: the Python community owns the data-science tools.

The right answer is therefore not "delete" but "**refactor into a real SDK**".

> **Refactor `adapters/cli_adapter.py` into the `hydragent_py` SDK package.**
> Keep the Rust REPL as the official, kernel-level CLI. Make the Python
> Rich-based REPL a *frontend* that ships with the SDK, not a duplicate of
> the kernel. Add a real Python client, plugin system, and Jupyter kernel
> on top of the same SDK.

### 1.4 What stays in Python

| File | Keep? | Why |
|---|---|---|
| `adapters/hydragent_py/` (NEW) | ✅ **Yes — the SDK** | `HydraClient`, `BusClient`, `REPL`, `plugins`, `cli` — the canonical Python surface |
| `adapters/hydragent_py/builtin/hello_world.py` (NEW) | ✅ **Yes — example plugin** | A 10-line worked example of how to write a plugin |
| `adapters/bus_client.py` | ✅ **Yes** | Backwards-compat shim → `hydragent_py.bus.BusClient` |
| `adapters/cli_adapter.py` | ✅ **Yes — as a shim** | Backwards-compat shim → `hydragent_py.repl.run_repl`; keeps `python cli_adapter.py` working |
| `adapters/telegram_adapter.py` | ✅ **Yes** | Real Telegram channel, uses `python-telegram-bot` |
| `adapters/discord_adapter.py` | ✅ **Yes** | Real Discord channel, uses `discord.py` |
| `adapters/slack_adapter.py` | ✅ **Yes** | Real Slack channel, uses `slack-bolt` |
| `adapters/email_adapter.py` | ✅ **Yes** | Real IMAP/SMTP channel |
| `adapters/webhook_adapter.py` | ✅ **Yes** | Generic inbound HTTP |
| `adapters/websocket_adapter.py` | ✅ **Yes** | Real-time UI surface |
| `adapters/agent_reach_runner.py` | ✅ **Yes** | Web scraper / fetcher |
| `adapters/searchxng.py` | ✅ **Yes** | SearXNG client used by the `web_search` tool |
| `adapters/formatter.py` | ✅ **Yes** | Channel-agnostic message rendering |
| `adapters/test_connection.py` | ✅ **Yes** | Adapter smoke test |
| `adapters/generate_library_graph.py` | ✅ **Yes** | Builds the D3 graph for the miniapp |
| `adapters/miniapp/` | ✅ **Yes** | The visual web app |
| `adapters/cli_repl.rs` (in `hydragent-core`) | ✅ **Yes (THE KERNEL CLI)** | The single source of truth for kernel-level terminal interaction. |

### 1.5 The kernel / frontend / SDK split

This is the architectural commitment that ties the whole plan together. The
Rust kernel is the agent runtime. Everything else is either a **frontend**
(a user-facing surface) or an **SDK** (a programmatic surface). Both frontends
and SDKs are *consumers* of the kernel — they never bypass the bus protocol
to reach into the agent's internals.

```
                ┌──────────────────────────────────────────┐
                │ Hydragent kernel (Rust, hydragent-core)  │
                │ ── LLM routing, ReAct loop, tools,       │
                │     memory, audit, vault, sandbox, swarm │
                └────────────▲────────────────┬────────────┘
                             │ JSON-RPC over TCP (5000)
                             │
                ┌────────────┴────────────────────────────┐
                │ hydragent_py SDK                         │
                │ ── HydraClient, BusClient, REPL,         │
                │     plugins, console-script entry point  │
                └────────────▲────────────────┬────────────┘
                             │                │
        ┌────────────────────┴─┐  ┌───────────┴──────────────┐
        │ Rust frontends:       │  │ Python surfaces:         │
        │ • hydragent chat      │  │ • Channel adapters        │
        │ • hydragent tui       │  │ • Plugins                 │
        │ • Web mini-app        │  │ • Jupyter notebooks       │
        └───────────────────────┘  │ • Custom scripts          │
                                  └───────────────────────────┘
```

Three rules follow from the diagram:

1. **The kernel is the only thing that owns agent state.** No frontend or
   SDK may write to `data/`, the SQLite store, or the audit chain
   directly. They go through the bus.
2. **The SDK is the only thing that touches the bus from Python.** Channel
   adapters, plugins, notebooks, and CLI scripts all import
   `hydragent_py.HydraClient` (or the lower-level `BusClient` when they
   need fine-grained control).
3. **Frontends are stateless.** A frontend is just a renderer for kernel
   events. Closing it loses no data; the kernel keeps the conversation.

### 1.6 Migration impact

The migration is **trivially small** because `cli_adapter.py` already
worked, and the new SDK is a strict superset:

- `python adapters/cli_adapter.py` → still works (shim forwards to the SDK)
- `from bus_client import BusClient` → still works (shim forwards to the SDK)
- `hydragent chat` (the Rust REPL) → unchanged
- New: `from hydragent_py import HydraClient` for embedders
- New: `hydra-cli chat` console script for users who prefer the Python REPL

**Action items**:
1. ~~Delete `adapters/cli_adapter.py`~~ **→ Superseded by §1.3 revision**
2. Create `adapters/hydragent_py/` SDK package (DONE)
3. Refactor `cli_adapter.py` to a shim (DONE)
4. Refactor `bus_client.py` to a shim (DONE)
5. Update `adapters/pyproject.toml` to declare the package + `hydra-cli` entry point (DONE)
6. Update `doc/STATE.md` §1.2 to mark the SDK as the canonical Python surface
7. Add a one-line note in `CHANGELOG.md` under v0.7.1: *"Refactored `cli_adapter.py` into the `hydragent_py` SDK package. New `hydra-cli` console script. Backwards compatible — `python cli_adapter.py` still works."*

---

## 2. v0.7.1 — *Hydra Shine* (the polish release)

**Goal**: ship the small, user-visible improvements that make v0.7.0 feel finished
before we start the Edge work. Nothing ambitious — only what's already in flight.

**Estimated time**: 3–4 days.

### 2.1 REPL output polish

The current REPL output is noisy:
- `tracing` log lines hit the terminal on top of user input
- The `you ▸` echo is duplicated by the bus round-trip
- The LLM response streams raw JSON-RPC frames, not just tokens
- Errors print in Rust panic format, not user-friendly format

The fix is already 80% in place: `logger.rs` was changed to accept an optional
file sink, and `main.rs` was changed to route chat mode to `data_dir/logs/chat.jsonl`
while keeping stderr at "warn" level.

#### TODO

| # | Task | Files | Lines |
|---|---|---|---|
| 2.1.1 | **Fix the E0382 compile error** in `cli_repl.rs:392` (`use of moved value: spinner_handle`) | `crates/hydragent-core/src/cli_repl.rs` | ~5 |
| 2.1.2 | Wire chat mode to the file sink (already drafted in `main.rs` around `if let Some(Commands::Chat) = &args.command` — needs routing hookup) | `crates/hydragent-core/src/main.rs` | ~15 |
| 2.1.3 | Add `termimad = "0.30"` (or `pulldown-cmark = "1.0"` + a custom renderer) to render markdown in REPL | `crates/hydragent-core/Cargo.toml` + `cli_repl.rs` | ~40 |
| 2.1.4 | Handle `response.permission_request` events in the REPL with a Y/n prompt + `PermissionDecision` JSON-RPC reply | `cli_repl.rs` | ~30 |
| 2.1.5 | Replace the raw `tracing` lines that currently bleed into chat with a clean `you ▸ / hydra ▸` transcript format | `cli_repl.rs` | ~20 |
| 2.1.6 | Add `/theme [auto|light|dark]` slash command (writes a config file the renderer reads) | `cli_repl.rs` | ~15 |

**Acceptance**: a brand-new user can run `Hydragent.cmd`, type a prompt, and
see a clean transcript like OpenClaw or Claude Code — no `INFO tool registered`
or `INFO Dream cycle completed` lines.

### 2.2 Swarm tool inheritance

`PHASE_7_COMPLETION_SUMMARY.md` Finding 2 flagged that swarm sub-agents don't
inherit the main agent's `skill_list` / `skill_search` / `skill_run` tools.

#### TODO

| # | Task | Files | Lines |
|---|---|---|---|
| 2.2.1 | In `hydragent-swarm::SubAgentSpawner`, when constructing the sub-agent, share the parent's `Arc<ToolRegistry>` instead of building a fresh one | `crates/hydragent-swarm/src/lib.rs` | ~10 |
| 2.2.2 | Add a `planner_inherits_tools: bool` config flag (default `true`) so power users can opt out per-role | `crates/hydragent-core/src/main.rs` | ~5 |
| 2.2.3 | Add an integration test that spawns a swarm sub-agent, calls `skill_list`, and verifies the 3 builtins come back | `crates/hydragent-swarm/tests/skill_inheritance.rs` | ~50 |

**Acceptance**: the `DelegateToSwarm` test prompt from the v0.7.0 chat session
now actually triggers `skill_*` tool calls in the sub-agent.

### 2.3 Pre-existing test fix

`hydragent-model --test custom_openai_integration::custom_provider_streams_openai_chunks`
fails because the test expects a decoded-string accumulator but the code under
test returns raw JSON-RPC frames.

#### TODO

| # | Task | Files | Lines |
|---|---|---|---|
| 2.3.1 | Open `crates/hydragent-model/tests/custom_openai_integration.rs:80` and fix the fixture to match the real protocol (decode `response.token` events) | test file | ~10 |

### 2.4 SKILL-BENCH baseline

`reports/bench-v0.7.0.json` is all-zero (retriever stub).

#### TODO

| # | Task | Files | Lines |
|---|---|---|---|
| 2.4.1 | Implement `SkillRetriever` in `hydragent-bench` that takes a `SkillLibrary` and a query → returns the top-K skills by hybrid score (BM25 + cosine) | `crates/hydragent-bench/src/retriever.rs` (new) | ~80 |
| 2.4.2 | Wire it into `bin/bench.rs` so `hydragent-bench` produces a non-zero R@1, R@5, MRR score against `tests/bench/golden_set_v1.jsonl` | `crates/hydragent-bench/src/runner.rs` | ~20 |
| 2.4.3 | Re-run the bench, write `reports/bench-v0.7.1.json`, document the baseline numbers in `RELEASE_NOTES_v0.7.1.md` | `RELEASE_NOTES_v0.7.1.md` | doc only |

### 2.5 Refactor `cli_adapter.py` into the `hydragent_py` SDK (see §1.3)

The architectural decision in v0.7.1 is to **refactor, not delete**, `cli_adapter.py`.
The new `hydragent_py` SDK package becomes the canonical Python surface for
Hydragent. The original `cli_adapter.py` is reduced to a 25-line shim that
forwards to `hydragent_py.repl.run_repl`.

#### TODO

| # | Task | Files | Status |
|---|---|---|---|
| 2.5.1 | Create `adapters/hydragent_py/` package with `__init__.py`, `client.py`, `repl.py`, `bus.py`, `bus_impl.py`, `plugins.py`, `cli.py`, `builtin/__init__.py`, `builtin/hello_world.py`, `README.md` | new files | ✅ Done |
| 2.5.2 | Reduce `adapters/cli_adapter.py` to a shim (forwards to `hydragent_py.repl.run_repl`) | `cli_adapter.py` | ✅ Done |
| 2.5.3 | Reduce `adapters/bus_client.py` to a shim (forwards to `hydragent_py.bus.BusClient`) | `bus_client.py` | ✅ Done |
| 2.5.4 | Update `adapters/pyproject.toml` to declare the package and the `hydra-cli` console script | `pyproject.toml` | ✅ Done |
| 2.5.5 | Smoke-test the SDK: `import hydragent_py`, `BusClient()` defaults, `HydraConfig.from_env()`, plugin discovery, legacy shim path | n/a | ✅ Done |
| 2.5.6 | Update `doc/PHASE_8_PLAN.md` §1 with the new architecture diagram (kernel / frontend / SDK) | this file | ✅ Done |
| 2.5.7 | Add a `py.typed` marker and a `hydragent_py` section to `adapters/README.md` | docs | Pending |
| 2.5.8 | Add a unit test for the SDK (e.g. `tests/test_hydragent_py.py`): instantiating `HydraClient`, building events, plugin discovery | tests | Pending |
| 2.5.9 | Document the SDK in `CHANGELOG.md` and `RELEASE_NOTES_v0.7.1.md` | docs | Pending |

### 2.6 Test count target

After v0.7.1: **≥ 575 passing, 0 failing** (current: 567 passing, 1 failing).

| Delta | Tests |
|---|---|
| Swarm skill inheritance | +1 integration |
| Pre-existing test fix | +0 (existing now passes) |
| SKILL-BENCH retriever | +3–5 unit |
| REPL output polish | +0 (no new behaviour) |
| **Net new** | **+4–6** |

### 2.7 v0.7.1 release checklist

- [ ] `cargo test --workspace` → 0 failures
- [ ] `hydragent doctor` → all green on a clean machine
- [ ] `hydragent chat` → clean transcript (no `INFO` lines)
- [ ] `hydragent chat "use a skill"` → triggers `skill_search` then `skill_run`
- [ ] `Hydragent.cmd` → end-to-end smoke test
- [ ] `python -c "import hydragent_py; print(hydragent_py.__version__)"` → prints `0.1.0`
- [ ] `hydra-cli chat` → starts the Python REPL frontend
- [ ] `python adapters/cli_adapter.py` → still works via the shim
- [ ] `from bus_client import BusClient` → still works via the shim
- [ ] `CHANGELOG.md` v0.7.1 entry
- [ ] `RELEASE_NOTES_v0.7.1.md`
- [ ] Git tag `v0.7.1`

---

## 3. v0.8.0 — *Hydra Edge* (Phase 8)

**Goal**: take Hydragent to the edge. The Zig edge stub at `edge/` is already in
the repo (`build.zig`, `build.zig.zon`, `src/`) but doesn't compile a real model
yet. Phase 8 makes it run a 4-bit TinyLlama on a $10 board.

**Estimated time**: 3–4 weeks (Weeks 31–34 per `doc/ROADMAP.md`).

### 3.1 Strategic positioning

The research is unambiguous: **no other Rust-core agent ships an edge binary**.

| Agent | Core | Edge binary | Multilingual |
|---|---|---|---|
| OpenClaw | TypeScript | ❌ | ❌ |
| Hermes Agent | Rust | ❌ | ❌ |
| ZeroClaw | Rust | ❌ (RPi-class only) | ❌ |
| NullClaw | Zig | ✅ (the whole thing) | ❌ |
| PicoClaw | Go + C (PicoLM) | ✅ (RISC-V SG2002) | ❌ |
| MimiClaw | Bare-metal C | ✅ (ESP32-S3) | ❌ |
| **Hydragent** | **Rust** | **✅ (Zig + PicoLM)** | **✅ (Rust + Zig + Python)** |

This is the only project that combines all three. The narrative for v0.8.0 is:

> *"The same Hydragent that runs your Telegram bot and writes your Rust code
> can also run on a $10 ESP32 board, fully offline, with no Python and no
> Node.js."*

### 3.2 Milestones

| # | Milestone | Acceptance | Dependencies |
|---|---|---|---|
| 8.1 | **Zig edge compiles** | `zig build` produces a `hydragent-edge` binary that prints "Hydragent Edge v0.8.0" and exits 0 | none |
| 8.2 | **PicoLM C engine integrated** | The Zig binary loads a 4-bit GGUF file from `./models/` and runs a `forward()` pass producing a token | 8.1, vendored PicoLM source |
| 8.3 | **Offline skill subset** | A `SkillSubset::offline()` API on `SkillLibrary` returns only skills whose `requires_network == false`; 3 builtins qualify | Phase 7 work already done |
| 8.4 | **ESP32-S3 target binary** | `zig build -Dtarget=x86_64-linux-gnu` → `zig build -Dtarget=xtensa-esp32s3` produces a ~150 KB ELF that fits in PSRAM | 8.1 + 8.2 |
| 8.5 | **MQTT IoT adapter** | `adapters/mqtt_adapter.py` subscribes to a topic, runs a skill on each message, publishes the result | none |
| 8.6 | **Edge skill executor** | The Zig binary can load a skill YAML from flash, parse it, and execute it against a local tool (no LLM required for `Echo`/`Math` skills) | 8.2 + 8.3 |
| 8.7 | **OTA update mechanism** | `hydragent-edge ota --url <github_release_url>` downloads a signed binary, verifies the Ed25519 sig, and `exec()`s the new one | 8.4 + agent Ed25519 key (already in `config/keys/agent_ed25519.pub`) |
| 8.8 | **Power profile** | Documented < 0.5W sustained on ESP32-S3 (matching MimiClaw spec); measured with a USB power meter | 8.4 |
| 8.9 | **Cloud handoff** | If the local model's confidence < 0.6, the edge binary posts the query to the main Hydragent's `/api/edge/handoff` endpoint and returns the answer | 8.2 + a new endpoint in `hydragent-gateway` |
| 8.10 | **Handoff docs** | A runnable demo: flash `hydragent-edge` to an ESP32-S3, power it from a USB battery, type `"what time is it?"` over serial, see the response | 8.4 + 8.6 + 8.7 |

### 3.3 PicoLM integration

PicoLM is the C inference engine used by PicoClaw (`https://github.com/sipeed/picolm`).
It supports GGUF v3, has zero Python dependencies, and pages weights in/out via
`mmap()` so a 4-bit TinyLlama 1.1B (≈ 700 MB) fits in 256 MB of PSRAM with
~700 MB on the SD card.

The integration is straightforward:

```zig
// edge/src/main.zig
const picolm = @import("picolm");

pub fn main() !void {
    var ctx = try picolm.Context.init("./models/tinyllama-1.1b-q4.gguf", .{
        .ctx_size = 2048,
        .threads = 2,
    });
    defer ctx.deinit();

    const prompt = "Q: What is 2+2?\nA:";
    var token: u32 = 0;
    while (true) : (token = try ctx.next_token()) {
        const piece = try ctx.token_to_piece(token);
        try stdout.writeAll(piece);
        if (ctx.eos()) break;
    }
}
```

**Vendoring decision**: vendor PicoLM as a git submodule at `edge/vendor/picolm/`
rather than depending on a system package. This keeps the build hermetic.

### 3.4 Test targets

| # | Test | Where | Pass criteria |
|---|---|---|---|
| 8.1.t1 | Zig build succeeds on Linux | `edge/` | `zig build` exits 0 |
| 8.2.t1 | PicoLM loads a 4-bit GGUF and produces a token | `edge/tests/forward_test.zig` | matches expected token for `"hello"` |
| 8.4.t1 | ESP32-S3 ELF is < 200 KB | CI script | `size hydragent-edge.elf` shows text < 150 KB |
| 8.5.t1 | MQTT adapter subscribes + processes a message | `adapters/tests/mqtt_e2e.py` | mock broker receives a published answer |
| 8.6.t1 | Edge binary executes a `Math` skill locally | `edge/tests/skill_test.zig` | 2+2 returns 4 |
| 8.7.t1 | OTA download + verify succeeds | `edge/tests/ota_test.zig` | downloaded binary passes Ed25519 verify |
| 8.9.t1 | Low-confidence query is handed off | `edge/tests/handoff_test.zig` | handoff is attempted when confidence < 0.6 |
| **Edge test count** | | | **+8 tests, all passing on Linux target** |

### 3.5 Cross-compile pipeline

```yaml
# .github/workflows/edge.yml
name: Edge build
on: { push: { paths: ['edge/**'] } }
jobs:
  build:
    strategy:
      matrix:
        target: [x86_64-linux, aarch64-linux, xtensa-esp32s3-none]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { submodules: recursive }
      - uses: mlugg/setup-zig@v1
        with: { version: 0.13.0 }
      - run: zig build -Dtarget=${{ matrix.target }}
      - uses: actions/upload-artifact@v4
        with: { name: hydragent-edge-${{ matrix.target }}, path: zig-out/bin/ }
```

### 3.6 Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| PicoLM doesn't build with Zig's C importer | Medium | High | Fall back to a thin C wrapper + `extern "C"` from Zig (already the standard pattern) |
| ESP32-S3 toolchain missing `libc` for newlib | High | Medium | Pin to a known-working toolchain (espressif's `xtensa-esp32s3-elf` v14.2.0) and pin Zig 0.13.0 |
| 4-bit GGUF is too lossy for a useful agent | Medium | Medium | Pre-evaluate on the SKILL-BENCH golden set (30 tasks) before committing to 4-bit; fall back to 8-bit (≈ 1.4 GB) on hardware that supports it |
| MQTT adapter needs a broker for testing | Low | Low | Use `aiomqtt` + a dockerised Mosquitto in CI |

### 3.7 v0.8.0 release checklist

- [ ] `zig build` green on all 3 targets
- [ ] `cargo test --workspace` → 0 failures (≥ 583 passing)
- [ ] ESP32-S3 demo video / GIF in the README
- [ ] Power profile measured and documented
- [ ] `CHANGELOG.md` v0.8.0 entry
- [ ] `RELEASE_NOTES_v0.8.0.md`
- [ ] Git tag `v0.8.0`
- [ ] Blog post / r/LocalLLaMA post

---

## 4. v0.9.0 — *Hydra Enterprise* (Phase 9)

**Goal**: take Hydragent from "personal assistant you run at home" to
"production agent you deploy at work". The minimum viable feature set is:
multi-tenancy, RBAC, a public release, and a real evaluation harness.

**Estimated time**: 4–6 weeks (Weeks 35–40+ per `doc/ROADMAP.md`).

### 4.1 Strategic positioning

The research shows a clear gap in the **self-hosted, multi-tenant agent**
space. Most enterprise options are cloud-only (Manus, Perplexity Computer,
Taskade) or single-user (Hermes, Moltis, Vellum). The two closest competitors
are:

- **Moltis** — Rust, self-hosted, secure-by-design — but small community and
  no skill engine
- **Adopt AI** — open-source, but more of an infrastructure toolkit than a
  ready-to-use agent

**Hydragent v0.9.0's pitch is unique**: Rust-core + Zig-edge + Python-channels
+ Hermes-style skill engine + 16-layer security + **now multi-tenant**.

### 4.2 Milestones

| # | Milestone | Acceptance | Dependencies |
|---|---|---|---|
| 9.1 | **Tenant isolation** | Every `data/<tenant_id>/` path is namespaced; bus RPCs carry a `tenant_id`; SQLite row-level security | none |
| 9.2 | **RBAC** | A `role` enum (`Admin`, `Operator`, `Viewer`, `Agent`) gates tool and skill access; configurable in `config/rbac.yaml` | 9.1 |
| 9.3 | **Hydra Hub** | A public GitHub repo at `hydra-hub/skills` with 10+ community skills; a `hydragent skill install <name>` CLI | none (can start in v0.8.0) |
| 9.4 | **SOC 2 controls** | A `compliance/` folder with: (a) encryption-at-rest doc, (b) audit log retention policy, (c) access-control matrix, (d) incident-response runbook | 9.1 + 9.2 |
| 9.5 | **Evaluation harness** | `hydragent-bench` grows from SKILL-BENCH (80 tasks) to: HaluMem (memory), MMLU (reasoning), SWE-bench-lite (code), AgentBench (tool use) | none |
| 9.6 | **Public GitHub release** | `github.com/yourorg/hydragent` with full README, CONTRIBUTING.md, CODE_OF_CONDUCT.md, issue templates, CI, releases | none |
| 9.7 | **SGNL integration** | Every tool call is verified against a SGNL policy before execution; configurable in `config/security/sgnl.yaml` | 9.2 |
| 9.8 | **Merkle audit log hardening** | The Phase 6 Merkle chain is now backed by a remote witness (GitHub gist or S3) for tamper-evidence | Phase 6.1 work already done |
| 9.9 | **Documentation site** | A `docs.hydragent.dev` site (mkdocs or mdbook) with: getting-started, architecture, security, deployment, API reference | none |
| 9.10 | **Public bench dashboard** | A `bench.hydragent.dev` (GitHub Pages + JSONL artefact) that shows historical SKILL-BENCH / HaluMem / MMLU scores per release | 9.5 |

### 4.3 The 9.5 evaluation harness — the most important milestone

A multi-bench harness is the *only* way to credibly claim "Hydragent is
production-ready". Without it, every release is a vibe-check; with it, every
release is a measurable improvement (or regression).

```rust
// crates/hydragent-bench/src/harness.rs
pub trait Benchmark {
    fn name(&self) -> &str;
    fn dataset(&self) -> &Path;
    async fn run(&self, agent: &AgentHandle) -> Result<BenchReport>;
}

pub struct BenchReport {
    pub benchmark: String,
    pub model: String,
    pub metrics: HashMap<String, f64>, // e.g. "recall_at_1" -> 0.78
    pub per_task: Vec<TaskResult>,
    pub started_at: DateTime<Utc>,
    pub elapsed: Duration,
}
```

| Benchmark | Source | What it measures | Target for v0.9.0 |
|---|---|---|---|
| SKILL-BENCH | in-repo (80 tasks) | Skill retrieval | R@1 ≥ 0.80 |
| Golden Set | in-repo (30 pairs) | Multi-relevance | MRR ≥ 0.70 |
| HaluMem | 10K QA pairs | Long-term memory QA | ≥ 0.88 (QwenPaw ReMe baseline) |
| MMLU | 14K MCQ | General knowledge | ≥ 0.70 (GPT-4-class) |
| SWE-bench-lite | 300 real GitHub issues | Code change generation | ≥ 0.10 (Claude 3.5 baseline) |
| AgentBench | 8 task domains | End-to-end agentic | ≥ 0.40 |

### 4.4 Tenant isolation design

```sql
-- migrations/006_tenants.sql
CREATE TABLE tenants (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    plan TEXT NOT NULL DEFAULT 'free'  -- 'free', 'pro', 'enterprise'
);

ALTER TABLE messages ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE semantic_memories ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE skill_library ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';

CREATE INDEX idx_messages_tenant ON messages(tenant_id, created_at);
CREATE INDEX idx_memories_tenant ON semantic_memories(tenant_id, importance);
```

The Rust `SessionStore` gets a `with_tenant(id)` builder; every query gets a
`WHERE tenant_id = ?` clause. The bus protocol adds an optional `tenant_id`
field to every RPC envelope.

### 4.5 RBAC matrix

| Role | Read tools | Write tools | Network tools | Admin tools | Skill install |
|---|---|---|---|---|---|
| **Admin** | ✅ all | ✅ all | ✅ all | ✅ all | ✅ |
| **Operator** | ✅ all | ✅ all | ✅ all | ❌ | ✅ |
| **Viewer** | ✅ all | ❌ | ❌ | ❌ | ❌ |
| **Agent** (the LLM) | ✅ all | ⚠️ gated by 3-tier | ⚠️ gated by 3-tier | ❌ | ❌ |

The **3-tier permission gate** (Phase 3) becomes the per-tenant policy:
each `PermissionTier::Prompt` fires a `response.permission_request` JSON-RPC
to the tenant's primary channel (Telegram, Discord, CLI, etc.). The
operator's Y/n response is routed back through the bus.

### 4.6 Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Multi-tenant SQLite row-level security is brittle | High | High | Consider Postgres for tenants > 10; ship v0.9.0 with SQLite + tested isolation, ship v0.9.1 with Postgres option |
| SWE-bench-lite is too expensive to run in CI | High | Medium | Only run SWE-bench-lite on releases, not on every PR; cache results |
| SGNL integration requires an account + API key | High | Low | Make SGNL optional (no-SGNL = permissive mode with audit) |
| Hydra Hub is empty on day 1 | Medium | Medium | Seed with 5 skills from the in-repo `skills/builtin/`; recruit 3 friendly contributors before launch |

### 4.7 v0.9.0 release checklist

- [ ] All 10 milestones met
- [ ] `cargo test --workspace` → 0 failures (≥ 700 passing)
- [ ] Multi-tenant smoke test: 3 tenants in one bus, no cross-talk
- [ ] Public GitHub repo + releases page
- [ ] Documentation site live
- [ ] `CHANGELOG.md` v0.9.0 entry
- [ ] `RELEASE_NOTES_v0.9.0.md`
- [ ] Git tag `v0.9.0`
- [ ] HackerNews "Show HN" post

---

## 5. The complete roadmap view

```
v0.7.0  ────✅ SHIPPED────  Hermes skill engine + curator + bench
                            (Weeks 27-30, 86 net-new tests)

v0.7.1  ────NEXT────────  Hydra Shine: REPL polish, log routing, E0382,
                            delete cli_adapter.py, swarm tool inheritance,
                            SKILL-BENCH baseline, pre-existing test fix
                            (~3-4 days)

v0.8.0  ────Phase 8─────  Hydra Edge: Zig edge compiles, PicoLM 4-bit GGUF,
                            ESP32-S3 binary, MQTT adapter, OTA, handoff
                            (3-4 weeks)

v0.9.0  ────Phase 9─────  Hydra Enterprise: multi-tenant, RBAC, SOC 2,
                            eval harness, Hydra Hub, public release
                            (4-6 weeks)
```

---

## 6. Why this plan is "perfect" (in the spirit of `doc/RaD/claude.md`)

The original brief was *"combine the best of every agent into one personalized
agent for any use case"*. This plan delivers:

| From | What we took | Where it landed |
|---|---|---|
| **OpenClaw** | 6 messaging channels, multi-channel router, persona onboarding | Phase 4 (Telegram/Discord/Slack/Email/Webhook/WebSocket all shipped) |
| **Hermes Agent** | Self-improving skill loop, 7-day curator, multi-channel | Phase 7 (`hydragent-skills` is Hermes-style) |
| **ZeroClaw** | Rust single-binary, trait-based hot-swappable components | Phase 1-3 (16 Rust crates, all trait-driven) |
| **NanoClaw** | Per-agent Docker isolation, Agent Vault, no raw API keys | Phase 3 + 6.4 (vault is XChaCha20-Poly1305 + Argon2id) |
| **NullClaw** | Ultra-lightweight, 678 KB binary, mmap memory | Phase 8 (Zig edge, 4-bit GGUF mmap) |
| **PicoClaw** | Self-bootstrapping AI-driven optimisation, Gene Evolution | Phase 7 (skill induction is the "evolving" loop) |
| **MimiClaw** | Runs on $10 board, < 0.5W, offline-first | Phase 8 (ESP32-S3 + offline skill subset) |
| **OpenFang** | 16-layer security, Merkle audit, taint tracking | Phase 6 (Tracks 6.1-6.4 shipped) |
| **IronClaw** | Encrypted vault, WASM sandbox, no raw API keys | Phase 3 + 6.4 (vault + Wasmtime) |
| **Vellum** | 8-type hierarchical memory, persona file | Phase 2 (episodic + semantic + emotional, SOUL.md/USER.md) |
| **Moltis** | Rust server, secure-by-design, sandboxed by default | Phase 1-3 (Rust + Wasmtime) |
| **Khoj** | Long-term context from personal docs, semantic search | Phase 2 (BM25 + vector hybrid) |
| **QwenPaw (ReMe)** | HaluMem ≥ 88.78% accuracy target | Phase 9.5 (evaluation harness) |
| **memU** | Folder/file/mount hierarchical memory | Phase 2 (episodic/semantic split) + future |
| **AnythingLLM** | Local-first, no telemetry, BYO model | Phase 1-7 (BRAIN_BASE swaps any provider) |
| **Kimi swarm** | 300 concurrent sub-agents | Phase 5.3 (DagExecutionEngine + AgentMailbox) |
| **OpenHands CodeAct** | Plan/Build/Explore/Scout/Review sub-agent roles | Phase 5 (Model Council + role-based routing) |
| **Microsoft Scout** | 3-tier permission gate, enterprise policy | Phase 3 (PermissionTier shipped) |
| **Devin** | Self-healing replanner on tool errors | Phase 5.4 (deferred) |
| **Pi** | Emotional intelligence, persona file | Phase 2 (SOUL.md, USER.md) |
| **Manus** | Sandboxed Linux VM, autonomous web dev | Phase 3 (Wasmtime + future Docker) |
| **Perplexity Computer** | Multi-model routing, subagent orchestration | Phase 5 (Model Council) |
| **Operator** | Browser automation | Phase 4 (web_search tool + future Playwright) |
| **Claude Cowork** | File management, local file ops | Phase 1 (file_read tool) |
| **Claude Code** | Clean REPL, slash commands, /help | v0.7.1 (REPL polish) |
| **Aider** | Repo map, edit blocks | Phase 1 (file_read) + future |
| **Cline** | Plan/Act modes, MCP | Phase 5 (Plan/Build roles) |
| **Open Interpreter** | Python-first REPL, notebook support | v0.7.1 (kernel/frontend/SDK split) |
| **TrustClaw** | OAuth via Composio, 1000+ skills marketplace | Phase 9.3 (Hydra Hub) |
| **Adept ACT-1** | Research ideas, LLM+vision+code | Future vision tool |
| **Rabbit (LAM)** | Hardware controller via USB | Future (Zig edge is the software sibling) |
| **Humane CosmOS** | On-device persona | Future (Zig edge binary) |

**What we don't take** (deliberate non-goals):

- **OpenClaw's prompt-injection footgun** — the Cisco audit showed a third-party
  skill exfiltrated data invisibly. Hydragent's Phase 6.3 (sanitizer) and
  6.2 (taint) subsystems are the explicit counter-measure.
- **Manus's $200/month cloud lock-in** — Hydragent is local-first; the cloud
  is optional (BRAIN_BASE can be a local Ollama URL).
- **IronClaw's TEE dependency on NEAR** — Hydragent runs on the user's own
  machine; the vault is mlock-pinned, not TEE-pinned, so no third party is
  involved.
- **OpenClaw's 100k+ LoC TypeScript codebase** — Hydragent's Rust core is
  ~25k LoC across 16 crates, auditable in an afternoon.

---

## 7. Decision log

| Date | Decision | Rationale |
|---|---|---|
| 2026-06-15 | **Refactor** `adapters/cli_adapter.py` into the `hydragent_py` SDK (REVISED — original draft said "delete") | Industry research: Open Interpreter, Aider, Letta, OpenHands all ship a Python REPL/SDK alongside their TypeScript/Rust frontends. The maintainer pushback ("python has large community and package library") is correct: data-science and channel-adapter communities live in Python. |
| 2026-06-15 | Adopt the **kernel / frontend / SDK** architectural split | Three rules: (1) the kernel owns agent state; (2) the SDK is the only Python surface for the bus; (3) frontends are stateless renderers. Mirrors Open Interpreter / Aider / Letta. |
| 2026-06-15 | Keep all other Python adapters (telegram, discord, slack, email, webhook, websocket) | Industry-standard for channel SDKs (python-telegram-bot, discord.py, slack-bolt) |
| 2026-06-15 | v0.7.1 = polish only, v0.8.0 = edge, v0.9.0 = enterprise | Smallest shippable increments; preserves the option to pivot at any boundary |
| 2026-06-15 | Zig edge is a *sibling* of the Rust core, not a replacement | Different optimisations apply (size vs throughput); sharing them via the bus protocol |
| 2026-06-15 | PicoLM over llama.cpp for the edge | llama.cpp is C++; PicoLM is C; integrates more cleanly with Zig's C importer |
| 2026-06-15 | Multi-tenant first design (not retrofit) | Avoids the "we built it single-tenant and now we have to add WHERE clauses everywhere" trap |
| 2026-06-15 | Eval harness is the *only* way to ship "production-ready" | Vibe-checks are not enough; multi-bench reports are the credible signal |

---

## 8. Open questions for the maintainer

1. **Do you want v0.7.1 as a separate release, or fold it into v0.8.0?**
   Recommendation: separate release, because the polish fixes (E0382,
   `cli_adapter.py` deletion, swarm tool inheritance) are user-visible
   and worth celebrating as their own milestone.

2. **PicoLM vs llama.cpp — do you have a preference?**
   Recommendation: PicoLM (C, mmap-native, Zig-friendly).

3. **MQTT broker: Mosquitto + docker-compose, or brokerless?**
   Recommendation: Mosquitto in a `docker-compose.dev.yml` for local dev,
   but the adapter supports any RFC-compliant broker.

4. **Multi-tenant: SQLite first, or Postgres from day 1?**
   Recommendation: SQLite first (faster to ship, no infra), with a
   `tenant_id` column on every table so the Postgres migration is a
   schema change, not a rewrite.

5. **Public release under what license?**
   Recommendation: dual-license MIT (open-source) + commercial, but
   that's a business decision; pure MIT is also fine.

6. **Should we apply to YC / similar for v0.9.0?**
   Not in scope for this plan, but worth discussing.

---

## 9. What I'll do next (concrete steps in order)

1. **Write the top-level `TODO_PHASE8.md`** mirroring the v0.7.0/v0.7.1 structure,
   so the plan is visible without opening `doc/`.
2. **Update `doc/STATE.md` §1.2** to mark `cli_adapter.py` as removed (pending v0.7.1).
3. **Update `doc/ROADMAP.md`** to add v0.7.1 as a milestone, rebalance Phase 8
   and Phase 9 with the v0.8.0/v0.9.0 split.
4. **File issues / TODOs** for each task in §2.1, §2.2, §2.3, §2.4, §3, §4.
5. **Begin v0.7.1 implementation** — start with the E0382 fix (5 minutes),
   then the `cli_adapter.py` deletion (5 minutes), then the REPL polish
   (half a day), then the swarm tool inheritance (half a day), then the
   SKILL-BENCH retriever (one day).

---

## 10. Appendix: full v0.7.1 + v0.8.0 + v0.9.0 task list (single view)

### v0.7.1 (3-4 days)
- [x] Refactor `cli_adapter.py` into the `hydragent_py` SDK package
- [x] Reduce `cli_adapter.py` and `bus_client.py` to backwards-compat shims
- [x] Update `adapters/pyproject.toml` to declare the package + `hydra-cli` entry point
- [x] Smoke-test the SDK end-to-end
- [x] Update `doc/PHASE_8_PLAN.md` to reflect the kernel/frontend/SDK architecture
- [ ] Add `py.typed` marker + SDK section to `adapters/README.md`
- [ ] Add a unit test for the SDK
- [ ] Document the SDK in `CHANGELOG.md` and `RELEASE_NOTES_v0.7.1.md`
- [ ] Fix E0382 in `cli_repl.rs:392` (5 lines)
- [ ] Wire chat mode to file sink in `main.rs` (15 lines)
- [ ] Add markdown rendering (termimad or pulldown-cmark) (40 lines)
- [ ] Handle `response.permission_request` in REPL (30 lines)
- [ ] Clean transcript format (20 lines)
- [ ] Add `/theme` slash command (15 lines)
- [ ] Share parent's `Arc<ToolRegistry>` in swarm sub-agents (10 lines)
- [ ] Add `planner_inherits_tools` config flag (5 lines)
- [ ] Integration test: swarm sub-agent calls `skill_list` (50 lines)
- [ ] Fix pre-existing `custom_openai_integration` test (10 lines)
- [ ] Implement `SkillRetriever` in `hydragent-bench` (80 lines)
- [ ] Wire retriever into `bin/bench.rs` (20 lines)
- [ ] Tag v0.7.1

### v0.8.0 (3-4 weeks)
- [ ] Get `zig build` green in `edge/` (1 day)
- [ ] Vendor PicoLM as a submodule (half day)
- [ ] Integrate PicoLM into `edge/src/main.zig` (1 day)
- [ ] Add `SkillSubset::offline()` to `hydragent-skills` (half day)
- [ ] Cross-compile to xtensa-esp32s3-none (1 day)
- [ ] Write `adapters/mqtt_adapter.py` + tests (1 day)
- [ ] Implement edge skill executor (no LLM) (1 day)
- [ ] Implement OTA download + Ed25519 verify (1 day)
- [ ] Power profile measurement + docs (half day)
- [ ] Implement cloud handoff endpoint in `hydragent-gateway` (1 day)
- [ ] ESP32-S3 demo video (half day)
- [ ] Update `CHANGELOG.md` and `RELEASE_NOTES_v0.8.0.md`
- [ ] Tag v0.8.0

### v0.9.0 (4-6 weeks)
- [ ] Migration `006_tenants.sql` + tenant isolation in `SessionStore` (1 week)
- [ ] RBAC enum + `config/rbac.yaml` + per-tool policy lookup (1 week)
- [ ] `hydra-hub/skills` public repo + 5 seed skills (half week)
- [ ] `hydragent skill install <name>` CLI (half week)
- [ ] `compliance/` folder: encryption-at-rest, retention, RBAC matrix, IR runbook (half week)
- [ ] `hydragent-bench` grows: HaluMem, MMLU, SWE-bench-lite, AgentBench (2 weeks)
- [ ] Public GitHub repo + README + CONTRIBUTING + CI (half week)
- [ ] SGNL integration (optional, behind config flag) (1 week)
- [ ] Merkle audit log remote witness (half week)
- [ ] Documentation site at `docs.hydragent.dev` (1 week)
- [ ] Public bench dashboard at `bench.hydragent.dev` (half week)
- [ ] Update `CHANGELOG.md` and `RELEASE_NOTES_v0.9.0.md`
- [ ] Tag v0.9.0
- [ ] HackerNews "Show HN" post

---

*End of plan. Total estimated time to v0.9.0: ~10 weeks from now, with
v0.7.1 as the first shippable milestone in 3-4 days.*
