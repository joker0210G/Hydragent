# hydragent_py — The official Python SDK for Hydragent

`hydragent_py` is the Python surface of the Hydragent AI agent. It lets you
embed Hydragent inside your own tools, write custom channel adapters, build
notebooks that talk to a remote kernel, and extend the agent with plugins.

## Why this package exists

Hydragent's core is a Rust kernel that owns the LLM routing, tool registry,
ReAct loop, memory store, audit chain, vault, sandbox, and swarm. The Rust
kernel is fast and safe, but Rust is not the right language for every
extension point:

- Channel adapters (Telegram, Discord, Slack, custom webhooks) are
  dominated by I/O and existing Python libraries (`python-telegram-bot`,
  `discord.py`, `slack-sdk`, `aiohttp`).
- Data-science users want to drive Hydragent from a Jupyter notebook.
- Power users want a one-file plugin that adds a custom tool or a
  slash command.

`hydragent_py` is the well-typed, well-documented seam between the Rust
kernel and all of those surfaces. It is the only place where Python code
should reach for the JSON-RPC bus.

## Architecture

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

## Quick start

```python
from hydragent_py import HydraClient

with HydraClient.connect() as hydra:
    print(hydra.chat("Summarise today's news in 3 bullet points."))
```

Or run the interactive Rich-based REPL:

```python
from hydragent_py import REPL
REPL().run()
```

Or use the `hydra-cli` console script (after `pip install -e adapters`):

```bash
$ hydra-cli chat
$ hydra-cli send "What's the weather in Paris?"
```

## Writing a plugin

Drop a `*.py` file into `~/.hydragent/plugins/` (or any of the other
discovery locations — see `hydragent_py.plugins`). The file must expose a
top-level `register(ctx)` function:

```python
# ~/.hydragent/plugins/hello_world.py
from hydragent_py.plugins import PluginContext

def register(ctx: PluginContext) -> None:
    ctx.add_tool(
        name="hello",
        description="Print a friendly greeting.",
        parameters={"type": "object", "properties": {}},
        handler=lambda **_: "hello, world 🐉",
        permission_tier="AutoApprove",
    )
```

The `PluginContext` also exposes slash-command registration and
`pre_send` / `post_receive` hooks for mutating messages on the way
through the REPL.

## What ships in this package

| Module                | Purpose                                              |
|-----------------------|------------------------------------------------------|
| `hydragent_py`        | Top-level re-exports                                 |
| `hydragent_py.client` | `HydraClient`, `HydraConfig`, `HydraError`           |
| `hydragent_py.repl`   | Rich-based interactive REPL                          |
| `hydragent_py.bus`    | `BusClient` (low-level JSON-RPC)                     |
| `hydragent_py.bus_impl` | Implementation of `BusClient`                      |
| `hydragent_py.plugins` | Plugin discovery and loading                        |
| `hydragent_py.cli`    | Console-script entry point (`hydra-cli`)             |
| `hydragent_py.builtin`| Bundled plugins (e.g. `hello_world`)                 |

## Backwards compatibility

The legacy `python adapters/cli_adapter.py` and
`from adapters.bus_client import BusClient` paths continue to work —
they are thin shims that forward into the new SDK.

## Versioning

`hydragent_py` is versioned together with the Hydragent kernel. A
breaking change to the SDK is always paired with a kernel release that
documents the change in `CHANGELOG.md`.
