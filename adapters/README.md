# Hydragent Adapters

This directory contains all Python-side code that talks to the
Hydragent kernel: the official Python SDK, channel adapters, and the
JSON-RPC bus client.

---

## `hydragent_py/` — the official Python SDK

The `hydragent_py` package is the canonical Python surface for the
Hydragent kernel. If you're writing Python code that talks to a
running `hydragent` instance, you should be importing from this
package.

```bash
pip install -e adapters/
# or just add `adapters/` to PYTHONPATH
```

### Quick start

```python
from hydragent_py import HydraClient, HydraConfig

# One-shot
with HydraClient.connect() as hydra:
    print(hydra.chat("Hello!"))

# Configured
cfg = HydraConfig.from_env()
with HydraClient.connect(cfg) as hydra:
    for token in hydra.stream("Tell me a story."):
        print(token, end="", flush=True)
```

### What's inside

| Module | Purpose |
|---|---|
| `hydragent_py.HydraClient` | High-level sync/async wrapper. Use this. |
| `hydragent_py.HydraConfig` | Dataclass config (`from_env()` factory). |
| `hydragent_py.HydraError` | Typed exception hierarchy. |
| `hydragent_py.BusClient` | Low-level JSON-RPC client (still exported for back-compat). |
| `hydragent_py.REPL` / `run_repl()` | Rich-based REPL frontend. |
| `hydragent_py.plugins` | Plugin discovery and registration. |
| `hydragent_py.builtin` | Bundled plugins (e.g. `hello_world.py`). |
| `hydra-cli` | Console script (`hydra-cli {chat,repl,send}`). |

### Plugin authoring

```python
# ~/.hydragent/plugins/greet.py
from hydragent_py.plugins import PluginContext, ToolSpec

def register(ctx: PluginContext) -> None:
    ctx.add_tool(ToolSpec(
        name="greet",
        description="Greet a user.",
        parameters={"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
        permission="AutoApprove",
        handler=lambda args: f"Hello, {args['name']}!",
    ))
```

`plugins.discover()` looks in (in order):

1. `$HYDRAGENT_PLUGINS_DIR` (env var)
2. `<data_dir>/plugins/`
3. `~/.hydragent/plugins/`
4. `<this-package>/builtin/`

See [hydragent_py/README.md](hydragent_py/README.md) for the full
SDK reference.

---

## Channel adapters

These are thin wrappers that forward messages from external chat
platforms to the Hydragent kernel via the bus.

| Adapter | Platform | Library |
|---|---|---|
| `telegram_adapter.py` | Telegram | `python-telegram-bot` |
| `discord_adapter.py` | Discord | `discord.py` |
| `slack_adapter.py` | Slack | `slack-bolt` |
| `email_adapter.py` | IMAP/SMTP | stdlib |
| `webhook_adapter.py` | Generic HTTP | stdlib |
| `websocket_adapter.py` | WebSocket | `websockets` |

All adapters import `BusClient` from `bus_client.py` (which is a
shim → `hydragent_py.bus.BusClient`).

### Running an adapter

```bash
# Start the kernel first
hydragent serve &

# Then start an adapter
python -m adapters.telegram_adapter
python -m adapters.discord_adapter
python -m adapters.slack_adapter
```

Each adapter reads its credentials from environment variables —
see the top of each file for the exact names.

---

## Legacy shims (back-compat)

For backwards compatibility with code written against the v0.7.0
API, two thin shims remain:

| File | Lines | Forwards to |
|---|---|---|
| `cli_adapter.py` | 27 | `hydragent_py.repl.run_repl` |
| `bus_client.py` | 50 | `hydragent_py.bus.BusClient` |

These shims will be kept for the v0.7.x line. New code should import
from `hydragent_py` directly.

---

## Utilities

| File | Purpose |
|---|---|
| `formatter.py` | Channel-agnostic message rendering |
| `agent_reach_runner.py` | Web scraper / fetcher (powers the `agent_reach` tool) |
| `searchxng.py` | SearXNG client (powers the `web_search` tool) |
| `test_connection.py` | Adapter smoke test |
| `generate_library_graph.py` | Builds the D3 graph for the miniapp |
| `miniapp/` | The web-based visual UI (D3 + Chart.js) |

---

## Development

```bash
# Run the SDK tests
python tests/test_hydragent_py.py

# Try the Python REPL
hydra-cli chat

# Connect to a running kernel
python -c "from hydragent_py import HydraClient; \
  with HydraClient.connect() as h: print(h.chat('hi'))"
```

See the top-level [README.md](../README.md) for installation and
[doc/PHASE_8_PLAN.md](../doc/PHASE_8_PLAN.md) §1.5 for the
kernel/frontend/SDK architecture.
