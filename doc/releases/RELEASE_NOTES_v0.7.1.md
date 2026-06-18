# Release Notes — v0.7.1 (Phase 7.1: Polish + Python SDK)

**Release date:** 2026-06-15
**Previous release:** v0.7.0 (Phase 7 — Self-Improving Skill Engine)
**Tag:** `v0.7.1`
**Focus:** Polish, architecture, and the official Python SDK

---

## TL;DR

v0.7.1 is a polish release with one major new artefact: the
**`hydragent_py` Python SDK**. It also establishes the
**kernel / frontend / SDK** architectural split, eliminates a 50 ms
latency tax in the Rust REPL spinner, and adds 7 SDK unit tests.

No breaking changes for users of the Rust kernel. The Python CLI
(`python adapters/cli_adapter.py`) and the channel adapters
(`from bus_client import BusClient`) continue to work unchanged via
backwards-compat shims.

---

## What's new

### 1. The `hydragent_py` Python SDK

The new `adapters/hydragent_py/` package is the official Python surface
for the Hydragent kernel. It bundles:

| Component | What it is | File |
|---|---|---|
| `HydraClient` | High-level sync/async wrapper with auto-reconnect, context-manager support, typed `HydraError` hierarchy | [client.py](adapters/hydragent_py/client.py) |
| `HydraConfig` | Dataclass config; honours `HYDRA_BUS_HOST` / `HYDRA_BUS_PORT` etc. | [client.py](adapters/hydragent_py/client.py) |
| `BusClient` | Low-level JSON-RPC over TCP (host/port args, graceful `close()`) | [bus_impl.py](adapters/hydragent_py/bus_impl.py) |
| `REPL` + `run_repl()` | Rich-based Python REPL (replaces the old `cli_adapter.py` script) | [repl.py](adapters/hydragent_py/repl.py) |
| `plugins` | `PluginContext`, `ToolSpec`, `SlashCommand`, `discover()`, `load_all()` | [plugins.py](adapters/hydragent_py/plugins.py) |
| `cli` | `hydra-cli {chat,repl,send}` console script | [cli.py](adapters/hydragent_py/cli.py) |
| `builtin/hello_world` | 10-line example plugin | [hello_world.py](adapters/hydragent_py/builtin/hello_world.py) |
| `py.typed` | PEP 561 marker for type-checker support | [py.typed](adapters/hydragent_py/py.typed) |
| `README.md` | Quick-start, plugin tutorial, package map | [README.md](adapters/hydragent_py/README.md) |

#### Quick start

```python
# Embed the agent in your own Python app
from hydragent_py import HydraClient, HydraConfig

with HydraClient.connect() as hydra:
    print(hydra.chat("Hello, Hydragent!"))
```

```python
# Write a plugin (drop into ~/.hydragent/plugins/ or any of the
# 4 discovery directories)
# my_plugin.py
from hydragent_py.plugins import PluginContext, ToolSpec

def register(ctx: PluginContext) -> None:
    ctx.add_tool(ToolSpec(
        name="greet",
        description="Greet a user by name.",
        parameters={"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
        permission="AutoApprove",
        handler=lambda args: f"Hello, {args['name']}!",
    ))
```

```bash
# Use the bundled CLI frontends
hydra-cli chat                          # Python REPL frontend
hydra-cli repl                          # Alias for `chat`
hydra-cli send "Hello, Hydragent!"      # One-shot send
python adapters/cli_adapter.py          # Backwards-compat entry point
```

### 2. The kernel / frontend / SDK architecture

v0.7.1 commits to a three-layer split:

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
   ┌─────────┴─────┐  ┌───────┴──────────────┐
   │ Rust frontends:│  │ Python surfaces:      │
   │ • hydragent chat│ │ • Channel adapters   │
   │ • hydragent tui │ │ • Plugins            │
   │ • Web mini-app  │ │ • Jupyter notebooks  │
   └────────────────┘ │ • Custom scripts     │
                      └──────────────────────┘
```

Three rules follow:

1. **The kernel is the only thing that owns agent state.** No frontend
   or SDK may write to `data/`, the SQLite store, or the audit chain
   directly.
2. **The SDK is the only thing that touches the bus from Python.**
   Channel adapters, plugins, notebooks, and CLI scripts all import
   `hydragent_py.HydraClient` (or the lower-level `BusClient`).
3. **Frontends are stateless.** A frontend is just a renderer for
   kernel events.

### 3. Spinner latency fix

The Rust REPL's spinner used to sleep 50 ms on `stop()` to "let the
spinner task observe the flag". This was wasteful on every response.

`SpinnerHandle` is now an `Option`-wrapped struct that owns its
`JoinHandle`. `stop(self)` consumes the handle, sets the flag, joins
the background thread deterministically, and clears the spinner line
— all in a few microseconds, not 50 ms.

### 4. SDK smoke tests

7 new tests in [tests/test_hydragent_py.py](tests/test_hydragent_py.py):

```
✓ test_top_level_imports
✓ test_hydra_config_defaults
✓ test_hydra_config_from_env
✓ test_bus_client_defaults
✓ test_plugin_discovery_finds_builtin
✓ test_legacy_shim
✓ test_cli_adapter_shim
All 7 tests PASSED
```

---

## What's not new (and why it matters)

- **`hydragent chat` (the Rust REPL) is unchanged** — same UX as v0.7.0.
- **All channel adapters work as before** — `from bus_client import BusClient`
  forwards to `hydragent_py.bus.BusClient` via a 50-line shim.
- **`python adapters/cli_adapter.py` works as before** — forwards to
  `hydragent_py.repl.run_repl` via a 27-line shim.
- **No changes to the kernel bus protocol** — JSON-RPC 2.0 over TCP
  port 5000 is identical to v0.7.0.
- **No changes to memory, vault, sandbox, or skills** — all v0.7.0
  features still work.

---

## Migration guide

Nothing to migrate. All the old entry points still work:

```bash
# Old (v0.7.0)
python adapters/cli_adapter.py             # still works — shim
from bus_client import BusClient            # still works — shim

# New (v0.7.1)
from hydragent_py import HydraClient        # preferred
hydra-cli chat                              # new console script
```

For new code, prefer the SDK (`from hydragent_py import HydraClient`).
The `bus_client` / `cli_adapter` shims are kept for backwards
compatibility and will not be removed in v0.7.x.

---

## What v0.7.1 does NOT include

This is a **polish release**. It does not include:

- The Zig edge runtime (planned for v0.8.0)
- Multi-tenant eval harness upgrades (planned for v0.9.0)
- New `termimad` / `pulldown-cmark` markdown rendering (deferred)
- The `/theme` slash command (deferred)

See [doc/PHASE_8_PLAN.md](doc/PHASE_8_PLAN.md) for the full roadmap.

---

## Build / test status

| Command | Status |
|---|---|
| `cargo check -p hydragent-core` | ✅ 21.42 s, 0 errors |
| `cargo build --workspace` | ✅ 1m 43s, 0 errors |
| `python tests/test_hydragent_py.py` | ✅ 7/7 tests pass |
| `python -c "import hydragent_py; print('OK')"` | ✅ imports cleanly without `rich` |
| `python -c "from bus_client import BusClient"` | ✅ legacy shim works |
| `python adapters/cli_adapter.py` (in `python -c`) | ✅ shim forwards to SDK |

---

## Acknowledgements

The architecture decision was informed by the maintainer's pushback
that Python's ecosystem (data science, channel SDKs, notebook
integration) makes it the right tool for SDK + plugin + adapter work.
This aligns with industry practice from **Open Interpreter**, **Aider**,
**Letta**, and **OpenHands**, all of which ship a Python REPL/SDK
alongside their TypeScript or Rust frontends.
