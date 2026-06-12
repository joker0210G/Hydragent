"""Test push routing end-to-end via LLM-issued send_message:
  1. Ask LLM to use the send_message tool to push a message
  2. Connect 3 WS clients (1 = target, 2 = non-target)
  3. Wait for the push to reach the target
  4. Verify other clients did NOT receive

This exercises the FULL e2e path:
  LLM -> send_message tool -> HeartbeatEngine.push -> GatewayRouter.push
  -> EventBusChannelBridge.send_push -> bus
  -> WS adapter listen_for_pushes -> _broadcast_push -> WS clients
"""
import asyncio
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(REPO, "adapters"))

import aiohttp
import websocket_adapter as ws_ad


async def ask_to_push(ws_url, target_page_id):
    """Use the LLM via the WS adapter to push a message to the target
    page. Uses the send_message tool which is AutoApprove tier and
    bypasses the cron wait — the push is delivered immediately."""
    async with aiohttp.ClientSession() as session:
        async with session.ws_connect(ws_url, autoclose=False) as ws:
            hello = await ws.receive(timeout=5.0)
            print(f"[HELLO] {hello.data[:80]!r}")
            prompt = (
                "Use the send_message tool to push a message. "
                f"channel_id='websocket', page_id='{target_page_id}', "
                "content='PUSH_TEST_42'. "
                "After you confirm the message was sent, reply with the single word: DONE"
            )
            print(f"[SEND] asking LLM to send push message")
            await ws.send_str(json.dumps({"content": prompt}))
            t0 = time.time()
            while time.time() - t0 < 60.0:
                try:
                    msg = await ws.receive(timeout=2.0)
                except asyncio.TimeoutError:
                    continue
                if msg.type == aiohttp.WSMsgType.TEXT:
                    d = json.loads(msg.data)
                    if d.get("type") == "result":
                        print(f"[LLM RESULT] {d.get('content', '')[:300]!r}")
                        return d.get("content", "")
                elif msg.type in (aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.ERROR):
                    break
            return None


async def main():
    url = "ws://127.0.0.1:8765/ws"
    target_pid = f"push-target-{int(time.time())}"
    print(f"target page_id for push = {target_pid}")

    # Step 1: Connect WS clients FIRST (push needs a listener)
    print("\n=== Step 1: Connect WS clients ===")
    clients = []
    pids = [f"client-{i}-{int(time.time())}" for i in range(2)]
    pids.append(target_pid)  # ← the target
    try:
        for pid in pids:
            c = ws_ad.WebSocketTestClient(url, page_id=pid)
            await c.connect()
            clients.append(c)
            print(f"  Connected {pid}")
        target_idx = pids.index(target_pid)
        target_client = clients[target_idx]

        # Step 2: Ask LLM to send a push message
        print(f"\n=== Step 2: Send push via LLM (send_message tool) ===")
        llm_resp = await ask_to_push(url, target_pid)
        if not llm_resp:
            print("LLM did not respond")
            return 1
        print(f"LLM response (truncated): {llm_resp[:300]!r}")

        # Step 3: Wait up to 90s for the target to receive the push
        print(f"\n=== Step 3: Wait for push to {target_pid} (up to 90s) ===")
        print(f"  debug: target_client.page_id={target_client.page_id}, "
              f"pending_pushes={target_client.pending_push_count()}")
        push = await target_client.wait_for_push(timeout=90.0)
        if not push:
            print(f"FAIL: no push received on {target_pid} in 90s")
            # Diagnostic dump
            all_msgs = target_client.all_received()
            print(f"  diagnostic: target client received {len(all_msgs)} msgs")
            for i, m in enumerate(all_msgs[-10:]):
                print(f"    [{i}] type={m.get('type')} "
                      f"page_id={m.get('page_id')!r} "
                      f"content={str(m.get('content',''))[:60]!r}")
            # Also dump all other clients' queues
            for i, c in enumerate(clients):
                if i == target_idx:
                    continue
                q = c.pending_push_count()
                if q:
                    print(f"  diagnostic: client {pids[i]} has {q} pending push(es)")
            return 1
        print(f"  ✓ Got push: {push.get('content', '')[:200]!r}")
        if "PUSH_TEST_42" not in push.get("content", ""):
            print(f"FAIL: push content doesn't contain 'PUSH_TEST_42'")
            return 1
        print(f"  ✓ Push content contains expected marker")

        # Step 4: Verify other clients did NOT receive
        print(f"\n=== Step 4: Verify other clients did NOT receive ===")
        for i, c in enumerate(clients):
            if i == target_idx:
                continue
            np = await c.wait_for_push(timeout=1.0)
            if np is not None:
                print(f"FAIL: client {pids[i]} received push: {np}")
                return 1
            print(f"  ✓ {pids[i]} correctly did NOT receive")

        print("\nAll push routing tests PASSED")
        return 0
    finally:
        for c in clients:
            await c.close()


if __name__ == "__main__":
    rc = asyncio.run(main())
    sys.exit(rc)
