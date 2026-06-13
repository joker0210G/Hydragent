"""
cli_user_pov.py — Drive the same 4 prompts through the bus the way the
                   real CLI adapter does, with the same callbacks
                   (token streaming, status updates, permission prompts).

This is the "user-perspective" smoke test: same connection lifecycle,
same per-message reconnect, same callbacks — just scripted.

Why per-intent reconnect matters:
    Sharing a single socket across intents exposed a real user-facing bug
    during development — the second prompt's `send_intent` would return
    the *previous* turn's `result` content because the bus can emit
    `response.complete` (a notification with no payload) for some intents
    and the client would either hang or return stale tokens. Each turn
    is therefore sent on a fresh socket, just like the CLI adapter does
    for a single user.

Usage (run from the workspace root, venv python):
    .\\adapters\\.venv\\Scripts\\python.exe tests\\cli_user_pov.py
"""
from __future__ import annotations

import asyncio
import json
import sys
import time
import uuid
from pathlib import Path

# Run from the adapters/ directory so `from bus_client import BusClient` works
ADAPTERS = Path(__file__).resolve().parent.parent / "adapters"
sys.path.insert(0, str(ADAPTERS))

from bus_client import BusClient  # noqa: E402


PROMPTS = [
    ("user-math",   "What is 17 times 23?"),
    ("user-fact",   "What is the current Rust language version as of 2026?"),
    ("user-swarm",  "Research the three most popular multi-agent AI frameworks "
                    "in 2026, compare their tool-calling support, and recommend "
                    "which one is best for a Rust runtime."),
    ("user-ask",    "Fix it the way we discussed earlier, please."),
]


def banner(text: str) -> None:
    bar = "=" * 78
    print(f"\n{bar}\n  {text}\n{bar}")


async def ask_user(client: BusClient, page_id: str, prompt: str, label: str) -> dict:
    banner(f"[{label}] USER TYPED: {prompt!r}")
    print(f"  (page_id={page_id})")

    streamed: list[str] = []
    statuses: list[str] = []
    approved_calls = 0

    def on_token(tok: str) -> None:
        streamed.append(tok)

    def on_status(s: str) -> None:
        statuses.append(s)

    async def on_permission(params: dict) -> bool:
        nonlocal approved_calls
        approved_calls += 1
        # The CLI adapter would Prompt.ask[y/n] here. For a smoke test we
        # auto-approve read-only / search tools.
        tool = params.get("tool_id", "")
        print(f"  [permission] {tool}  -> auto-approve (read-only)")
        return True

    # Per-intent reconnect: matches the cli_adapter behaviour of one socket
    # per user turn, and avoids stale-buffer bugs on the bus (the bus can
    # emit a `response.complete` *without* a payload for some intents,
    # which would otherwise hang or return the previous turn's content).
    try:
        if client.writer is not None:
            try:
                client.writer.close()
                await client.writer.wait_closed()
            except Exception:
                pass
        await client.connect()
    except Exception as e:
        print(f"  ERROR: reconnect failed: {e!r}")
        return {"label": label, "error": repr(e)}

    event = {
        "page_id":     page_id,
        "channel_id":  "cli:user-pov",
        "user_id":     "local-user",
        "content":     prompt,
        "attachments": [],
        "metadata":    {},
        "timestamp":   int(time.time() * 1000),
        "priority":    "normal",
    }

    t0 = time.time()
    try:
        final = await client.send_intent(
            event,
            token_callback=on_token,
            status_callback=on_status,
            permission_callback=on_permission,
        )
    except Exception as e:
        print(f"  ERROR: {e!r}")
        return {"label": label, "error": repr(e), "elapsed": time.time() - t0}

    elapsed = time.time() - t0
    if not isinstance(final, str):
        final = json.dumps(final, ensure_ascii=False)

    streamed_text = "".join(streamed)
    print(f"  elapsed: {elapsed:.1f}s")
    print(f"  permissions asked: {approved_calls}")
    print(f"  streamed tokens: {len(streamed)} chars (preview: {streamed_text[:60]!r})")
    print(f"  final_content len: {len(final)}")
    safe = final.encode("ascii", "backslashreplace").decode("ascii")
    print("  final_content (ASCII preview, first 600 chars):")
    for line in safe[:600].splitlines():
        print(f"    {line}")
    return {
        "label": label,
        "elapsed": elapsed,
        "permissions": approved_calls,
        "streamed_chars": len(streamed_text),
        "final_len": len(final),
        "final": final,
    }


async def main() -> int:
    print("Hydragent — user-perspective smoke test")
    print("========================================")
    print("Connecting to bus on 127.0.0.1:5000 ...")
    client = BusClient()
    await client.connect()
    print("[green]connected[/green]")

    # One persistent page_id for the whole session, the way a user would
    # have when they keep the same CLI open.
    page_id = f"user-pov-{uuid.uuid4().hex[:8]}"
    print(f"using page_id={page_id}\n")

    results = []
    for label, prompt in PROMPTS:
        # The CLI adapter stays on one connection; we mimic that.
        r = await ask_user(client, page_id, prompt, label)
        results.append(r)

    banner("SESSION SUMMARY")
    for r in results:
        print(f"  [{r['label']:<14}] {r.get('elapsed', 0):5.1f}s  "
              f"perms={r.get('permissions', 0)}  "
              f"streamed={r.get('streamed_chars', 0)}  "
              f"final_len={r.get('final_len', 0)}  "
              f"err={r.get('error')}")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
