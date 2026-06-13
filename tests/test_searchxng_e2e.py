"""
test_searchxng_e2e.py — End-to-end test of the SearXNG/ddgs-backed web_search.

Connects to the bus, sends four prompts that exercise:
  1. ReactLoop  (simple math, no tools needed)
  2. ReactLoop + web_search  (factual query that should trigger the tool)
  3. DelegateToSwarm  (compound query that should fan out to the swarm,
                       and a sub-agent should call web_search)
  4. AskUser  (ambiguous / under-specified query that should trigger the
               LLM to ask a clarifying question back)

Reports the final answer from each.

Usage:
    .\\adapters\\.venv\\Scripts\\python.exe tests\\test_searchxng_e2e.py
"""
from __future__ import annotations

import asyncio
import json
import sys
import time
import uuid
from pathlib import Path
from urllib import request as urlrequest
from urllib.error import URLError

# Make the adapters package importable
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from adapters.bus_client import BusClient


SHIM_HEALTHZ = "http://127.0.0.1:7777/healthz"


def shim_alive(timeout: float = 2.0) -> bool:
    """Quick TCP/HTTP check that the local searchxng shim is up."""
    try:
        with urlrequest.urlopen(SHIM_HEALTHZ, timeout=timeout) as r:
            return r.status == 200
    except (URLError, OSError):
        return False


def make_intent(prompt: str, label: str) -> dict:
    """Build a properly-shaped IntentEvent for the orchestrator.

    The orchestrator deserialises the JSON-RPC `params` directly into
    `hydragent_types::IntentEvent` which requires:
        page_id, channel_id, user_id, content, timestamp, priority
    """
    return {
        "page_id": f"searxng-{label}-{int(time.time())}",
        "channel_id": "cli:test",
        "user_id": "fabla5",
        "content": prompt,
        "attachments": [],
        "metadata": {},
        "timestamp": int(time.time() * 1000),
        "priority": "normal",
    }


async def ask(client: BusClient, prompt: str, label: str) -> dict:
    print(f"\n{'=' * 72}\n[{label}] prompt: {prompt!r}")
    print("-" * 72)
    # Close and reopen the connection for each intent so the buffered
    # read on a reused socket can't accidentally return the previous
    # response.
    try:
        if client.writer is not None:
            try:
                client.writer.close()
            except Exception:
                pass
        await client.connect()
    except Exception:
        pass
    event = make_intent(prompt, label)
    print(f"  page_id={event['page_id']}")
    t0 = time.time()
    try:
        content = await client.send_intent(event)
    except Exception as e:
        print(f"  ✗ exception after {time.time()-t0:.1f}s: {e!r}")
        return {"label": label, "prompt": prompt, "error": repr(e)}
    elapsed = time.time() - t0
    if not isinstance(content, str):
        content = json.dumps(content, ensure_ascii=False)
    print(f"  OK  {elapsed:.1f}s  len={len(content)}")
    # Print a short ASCII-safe preview (the orchestrator sometimes prefixes
    # with an emoji like U+2753 that mojibakes on cp1252 terminals).
    safe = content.encode("ascii", "backslashreplace").decode("ascii")
    print("  response (first 200 chars):")
    print("    " + safe[:200].replace("\n", " "))
    return {"label": label, "prompt": prompt, "elapsed": elapsed, "content": content}


async def main() -> int:
    if not shim_alive():
        print("ERROR: searchxng shim not reachable at", SHIM_HEALTHZ)
        print("       start it with:")
        print("         .\\adapters\\.venv\\Scripts\\python.exe adapters\\searchxng.py --port 7777")
        return 1

    client = BusClient()
    await client.connect()
    print("connected to bus on port 5000\n")

    results = []
    # 1) ReactLoop simple math — should NOT call web_search
    results.append(await ask(client, "What is 17 times 23?", "react-math"))
    # 2) ReactLoop + web_search — factual query, sub-agent may call the tool
    results.append(await ask(client, "What is the current Rust language version as of 2026?", "react-fact"))
    # 3) DelegateToSwarm — compound query, swarm sub-agents should call web_search
    results.append(
        await ask(
            client,
            "Research the three most popular multi-agent AI frameworks in 2026, "
            "compare their tool-calling support, and recommend which one is best "
            "for a Rust runtime.",
            "swarm-research",
        )
    )
    # 4) AskUser — ambiguous / under-specified query, LLM should ask back
    results.append(
        await ask(
            client,
            "Fix it the way we discussed earlier, please.",
            "ask-user",
        )
    )

    print("\n" + "=" * 72)
    print("SUMMARY")
    print("=" * 72)
    for r in results:
        elapsed = r.get("elapsed", 0)
        err = r.get("error")
        raw = (r.get("content") or "").replace("\n", " ")[:80]
        # Render preview as ASCII (escape any non-ASCII bytes) so the
        # output is readable on Windows cp1252 terminals.
        preview = raw.encode("ascii", "backslashreplace").decode("ascii")
        print(f"  [{r['label']:<14}] {elapsed:5.1f}s  err={err}  preview={preview!r}")

    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
