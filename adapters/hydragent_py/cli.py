# hydragent_py.cli — Console-script entry point.
#
# This module is the target of the `hydra-cli` entry script declared
# in `adapters/pyproject.toml`. Run as:
#
#   $ hydra-cli chat
#   $ hydra-cli send "summarise https://example.com"
#   $ hydra-cli repl --page abc-123
#
# Sub-commands:
#   chat    Start an interactive REPL (default)
#   send    Send a single message and print the reply
#   repl    Alias for `chat` (kept for symmetry with `hydragent chat`)

from __future__ import annotations

import argparse
import sys

from .client import HydraClient, HydraConfig, HydraError
from .repl import REPL


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="hydra-cli",
        description="Hydragent Python SDK — chat, send, and embed.",
    )
    p.add_argument("--page", type=str, help="Session id (default: random uuid)")
    p.add_argument("--host", type=str, help="Bus host (default: HYDRA_BUS_HOST or 127.0.0.1)")
    p.add_argument("--port", type=int, help="Bus port (default: HYDRA_BUS_PORT or 5000)")

    sub = p.add_subparsers(dest="cmd", required=False)

    sub.add_parser("chat", help="Start an interactive REPL (default)")
    sub.add_parser("repl", help="Alias for `chat`")

    send = sub.add_parser("send", help="Send a single message and exit")
    send.add_argument("message", type=str, help="The user-visible prompt to send")

    return p


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    config = HydraConfig.from_env()
    if args.page:
        config.page_id = args.page
    if args.host:
        config.bus_host = args.host
    if args.port:
        config.bus_port = args.port

    cmd = args.cmd or "chat"
    if cmd in ("chat", "repl"):
        return REPL(config).run()
    if cmd == "send":
        try:
            with HydraClient.connect(config) as client:
                print(client.chat(args.message))
        except HydraError as exc:
            print(f"hydra-cli: {exc}", file=sys.stderr)
            return 1
        return 0
    print(f"hydra-cli: unknown subcommand: {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
