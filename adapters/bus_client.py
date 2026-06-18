#!/usr/bin/env python3
"""bus_client.py — Backwards-compatible shim.

The canonical implementation now lives in
`adapters/hydragent_py/bus_impl.py` and is re-exported by
`adapters/hydragent_py/bus.py` (and by the top-level
`hydragent_py` package).

This shim keeps the old import path working so that channel
adapters (telegram, discord, slack, email, webhook, websocket)
do not need to be updated in lockstep with the SDK refactor.

New code should prefer:

    from hydragent_py import BusClient
"""
import os
import sys

# Allow running this file from a checkout (without `pip install -e .`)
# by putting the SDK on sys.path.
_THIS_DIR = os.path.dirname(os.path.abspath(__file__))
if _THIS_DIR not in sys.path:
    sys.path.insert(0, _THIS_DIR)

from hydragent_py.bus import BusClient  # noqa: E402,F401

__all__ = ["BusClient"]


if __name__ == "__main__":
    # Tiny smoke test: connect, send "ping", print the reply.
    import asyncio

    async def _main() -> int:
        c = BusClient()
        await c.connect()
        try:
            reply = await c.send_intent(
                {
                    "page_id": "smoke",
                    "channel_id": "cli:smoke",
                    "user_id": "local-user",
                    "content": "ping",
                    "attachments": [],
                    "metadata": {},
                    "timestamp": 0,
                    "priority": "normal",
                }
            )
            print(reply)
            return 0
        finally:
            await c.close()

    sys.exit(asyncio.run(_main()))
