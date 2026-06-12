"""Smoke test the WebSocket adapter end-to-end:
  1. Connect a test client
  2. Send a prompt that should not need any tools ("What's 2+2?")
  3. Wait for the streamed `result` message
  4. Assert the response contains a number
  5. Disconnect cleanly
"""
import asyncio
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(REPO, "adapters"))

import websocket_adapter as ws_ad


async def main():
    url = f"ws://127.0.0.1:8765/ws"
    page_id = f"smoke-{int(time.time())}"
    print(f"Connecting to {url} with page_id={page_id}...")
    async with ws_ad.WebSocketTestClient(url, page_id=page_id) as client:
        print(f"Connected, default page_id={client.page_id}")
        # Send a trivial prompt
        print("Sending prompt: 'What is 2+2? Just reply with the number, nothing else.'")
        t0 = time.time()
        result = await client.send(
            "What is 2+2? Just reply with the number, nothing else.",
            timeout=45.0,
        )
        elapsed = time.time() - t0
        print(f"Got result in {elapsed:.1f}s:")
        print(f"  type={result.get('type')!r} page_id={result.get('page_id')!r}")
        content = result.get("content", "")
        print(f"  content[:200]={content[:200]!r}")
        if not content.strip():
            print("FAIL: empty content")
            return 1
        # Should contain '4' somewhere
        if "4" not in content:
            print(f"WARN: '4' not in content, but got a response ({len(content)} chars)")
            return 1
        print("OK: response contained '4'")
        return 0


if __name__ == "__main__":
    rc = asyncio.run(main())
    sys.exit(rc)
