#!/usr/bin/env python3
"""Minimal direct test using BusClient + asyncio.run() in the main thread.

If this works, the issue is with HydraClient's background-thread pattern.
If it also hangs, the issue is with the kernel bus or BusClient.
"""
from __future__ import annotations
import asyncio
import sys
import time
from pathlib import Path

_THIS_DIR = Path(__file__).resolve().parent
_ADAPTERS = _THIS_DIR.parent / "adapters"
if str(_ADAPTERS) not in sys.path:
    sys.path.insert(0, str(_ADAPTERS))

from hydragent_py.bus_impl import BusClient  # type: ignore[import-not-found]


def build_event(content: str, page_id: str) -> dict:
    return {
        "page_id": page_id,
        "channel_id": "cli:default",
        "user_id": "local-user",
        "content": content,
        "attachments": [],
        "metadata": {},
        "timestamp": int(time.time() * 1000),
        "priority": "normal",
    }


async def main_async() -> str:
    client = BusClient(host="127.0.0.1", port=5000)
    print("[1/4] Connecting...", flush=True)
    await client.connect()
    print("[2/4] Connected", flush=True)

    page_id = "direct-test-" + str(int(time.time()))
    event = build_event("Reply with exactly: PONG. Nothing else.", page_id)
    print(f"[3/4] Sending: {event['content']!r} (page_id={page_id})", flush=True)

    result = await client.send_intent(event)
    print(f"[4/4] Got: {result!r}", flush=True)

    await client.close()
    return result


def main() -> int:
    try:
        result = asyncio.run(main_async())
        if "PONG" in result.upper():
            print("\n✓ Direct asyncio.run() path works", flush=True)
            return 0
        else:
            print(f"\n✗ Unexpected: {result!r}", flush=True)
            return 1
    except Exception as e:
        print(f"\n✗ Exception: {type(e).__name__}: {e}", flush=True)
        return 2


if __name__ == "__main__":
    sys.exit(main())
