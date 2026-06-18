# hydragent_py — Hydragent Python SDK
#
# This is the official Python SDK for the Hydragent agent kernel. It gives
# Python developers a stable, well-typed surface to:
#
#   • Build channel adapters (Telegram, Discord, Slack, custom HTTP) on top
#     of the JSON-RPC bus.
#   • Embed Hydragent as a library inside their own agent scripts.
#   • Talk to a remote Hydragent kernel from a Jupyter notebook, a custom
#     web UI, or any other Python process.
#   • Write plugins that hook into the REPL or augment the tool registry.
#
# The architecture follows the "kernel / frontend / SDK" split used by
# production agents like Open Interpreter, Aider, and Letta:
#
#   ┌──────────────────────────────────────────────────────────┐
#   │  Hydragent kernel (Rust, hydragent-core)                  │
#   │  ── LLM routing, tool registry, ReAct loop, memory,      │
#   │     audit, vault, sandbox, swarm                          │
#   └──────▲─────────────────────────────────────────┬─────────┘
#          │ JSON-RPC over TCP (port 5000)            │
#          │                                          │
#   ┌──────┴────────────────────────┐   ┌────────────▼─────────────┐
#   │  Frontend (any of):           │   │  hydragent_py SDK         │
#   │  • Rust REPL (hydragent chat) │   │  ── public API for Python │
#   │  • TUI (hydragent tui)        │   │     adapters, notebooks,  │
#   │  • Web mini-app (miniapp/)    │   │     plugins, custom apps  │
#   │  • CLI (cli_adapter.py)       │   │                           │
#   └───────────────────────────────┘   └───────────────────────────┘
#
# Quick start:
#
#   >>> from hydragent_py import HydraClient
#   >>> client = HydraClient.connect()
#   >>> reply = client.chat("What's the weather in Paris?")
#   >>> print(reply)
#
#   Or for a rich interactive REPL:
#
#   >>> from hydragent_py import REPL
#   >>> REPL().run()
#
#   Or as a console script:
#
#   $ hydra-cli chat
#   $ hydra-cli send "summarise this URL: https://example.com"

from .client import HydraClient, HydraConfig, HydraError
from .bus import BusClient
from . import plugins

__version__ = "0.1.0"


def __getattr__(name: str):
    """Lazy attribute access for optional submodules.

    `REPL` and `run_repl` require the `rich` package. They are imported
    on first access so that callers who only need the bus / client /
    plugins API do not have to install `rich` as a dependency.
    """
    if name in ("REPL", "run_repl"):
        from . import repl as _repl  # local import: requires `rich`
        return getattr(_repl, name)
    raise AttributeError(f"module 'hydragent_py' has no attribute {name!r}")


__all__ = [
    "HydraClient",
    "HydraConfig",
    "HydraError",
    "REPL",
    "run_repl",
    "BusClient",
    "plugins",
    "__version__",
]
