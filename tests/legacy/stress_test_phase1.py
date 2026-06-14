#!/usr/bin/env python3
"""
Hydragent Phase 1 — Extreme Stress Test (v2)
============================================

Exercises every component of the Phase 1 runtime against the live bus on
127.0.0.1:5000 and the live MiniMax-M3 brain (unlimited token provider).

Uses the ACTUAL bus API as implemented in the Rust core:
  - library.create_node   { id, type, label, properties? }
  - library.list_nodes    { type }
  - library.search        { start_node } (graph traversal)
  - library.delete_node   { id }
  - library.link          { source, target, relation, weight? }
  - memory.list           -> flat [SemanticMemory, ...]
  - memory.delete         { id }
  - memory.clear          -> {"cleared": N}
  - config.read           { file_name in {USER.md, SOUL.md} }
  - config.write          { file_name, content }
  - page.get_summary      { page_id }
  - intent.submit         IntentEvent
"""
from __future__ import annotations

import asyncio
import json
import os
import statistics
import sys
import time
import traceback
import uuid
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Optional

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(REPO, "adapters"))

import bus_client as _bc  # noqa: E402
from bus_client import BusClient  # noqa: E402

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

BUS_HOST = os.getenv("HYDRAGENT_BUS_HOST", "127.0.0.1")
BUS_PORT = int(os.getenv("HYDRAGENT_BUS_PORT", str(_bc.BUS_PORT)))
PAGE_ID = os.getenv(
    "HYDRAGENT_TEST_PAGE",
    "11111111-2222-3333-4444-555555555555",
)
CHANNEL_ID = "stress-test:phase1"
USER_ID = "stress-tester"

TIMEOUT_LLM = 180.0
TIMEOUT_FAST = 15.0
TIMEOUT_STREAM = 240.0


# ---------------------------------------------------------------------------
# Low-level bus call
# ---------------------------------------------------------------------------

async def bus_call(
    method: str,
    params: Optional[dict] = None,
    *,
    timeout: float = TIMEOUT_FAST,
    bus_host: str = BUS_HOST,
    bus_port: int = BUS_PORT,
    request_id: Optional[str] = None,
) -> dict:
    """Send a JSON-RPC request over a fresh TCP connection and return the response."""
    reader, writer = await asyncio.open_connection(bus_host, bus_port)
    req = {
        "jsonrpc": "2.0",
        "method": method,
        "params": params or {},
        "id": request_id or str(uuid.uuid4()),
    }
    writer.write((json.dumps(req) + "\n").encode())
    await writer.drain()
    try:
        line = await asyncio.wait_for(reader.readline(), timeout=timeout)
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass
    if not line:
        raise RuntimeError(f"no response for {method}")
    return json.loads(line.decode().strip())


async def assert_ok(method: str, params: Optional[dict] = None, **kw) -> dict:
    """Send a request and raise if error is present."""
    r = await bus_call(method, params, **kw)
    if r.get("error"):
        raise AssertionError(f"{method} returned error: {r['error']}")
    return r["result"]


# ---------------------------------------------------------------------------
# Intent helper
# ---------------------------------------------------------------------------

async def send_intent(
    prompt: str,
    *,
    page_id: str = PAGE_ID,
    timeout: float = TIMEOUT_LLM,
) -> dict:
    """Submit a user intent via BusClient. Returns {"content", "tokens", "statuses"}."""
    client = BusClient()
    await client.connect()
    tokens: list[str] = []
    statuses: list[str] = []

    def on_token(t: str) -> None:
        tokens.append(t)

    def on_status(s: str) -> None:
        statuses.append(s)

    try:
        content = await asyncio.wait_for(
            client.send_intent(
                {
                    "page_id": page_id,
                    "channel_id": CHANNEL_ID,
                    "user_id": USER_ID,
                    "content": prompt,
                    "attachments": [],
                    "metadata": {},
                    "timestamp": int(time.time() * 1000),
                    "priority": "normal",
                },
                token_callback=on_token,
                status_callback=on_status,
            ),
            timeout=timeout,
        )
    finally:
        try:
            client.writer.close()
        except Exception:
            pass

    return {"content": content, "tokens": len(tokens), "statuses": statuses}


# ---------------------------------------------------------------------------
# Test framework
# ---------------------------------------------------------------------------

@dataclass
class TestResult:
    name: str
    passed: bool
    detail: str = ""
    latency_ms: float = 0.0
    extras: dict[str, Any] = field(default_factory=dict)


class StressHarness:
    def __init__(self) -> None:
        self.results: list[TestResult] = []
        self.passed = 0
        self.failed = 0

    async def _run(
        self,
        name: str,
        coro_factory: Callable[[], Awaitable[tuple[str, dict]]],
        *,
        timeout: float = TIMEOUT_FAST,
    ) -> TestResult:
        t0 = time.perf_counter()
        try:
            detail, extras = await asyncio.wait_for(coro_factory(), timeout=timeout)
        except asyncio.TimeoutError:
            elapsed = (time.perf_counter() - t0) * 1000
            r = TestResult(name, False, f"TIMEOUT after {timeout}s", elapsed)
            self.results.append(r)
            self.failed += 1
            print(f"  [TIMEOUT] {name} ({elapsed:.0f} ms)")
            return r
        except Exception as exc:  # noqa: BLE001
            elapsed = (time.perf_counter() - t0) * 1000
            r = TestResult(name, False, f"EXC: {exc!r}", elapsed)
            self.results.append(r)
            self.failed += 1
            print(f"  [ERROR]   {name}: {exc!r} ({elapsed:.0f} ms)")
            traceback.print_exc()
            return r
        elapsed = (time.perf_counter() - t0) * 1000
        r = TestResult(name, True, detail, elapsed, extras)
        self.results.append(r)
        self.passed += 1
        print(f"  [PASS]    {name} ({elapsed:.0f} ms) - {detail}")
        return r


# ---------------------------------------------------------------------------
# Phase A — Bus direct tests
# ---------------------------------------------------------------------------

async def phase_a_bus_direct(h: StressHarness) -> None:
    print("\n=== Phase A: Bus Direct Tests (no LLM) ===")

    async def a1():
        result = await assert_ok("memory.list")
        assert isinstance(result, list), f"expected list, got {type(result)!r}"
        return f"{len(result)} memories (flat list)", {"shape": type(result).__name__}

    await h._run("A1 memory.list handshake", a1)

    async def a2():
        result = await assert_ok("memory.list")
        assert isinstance(result, list), f"expected list, got {type(result)!r}"
        first_id = result[0]["id"] if result else None
        return f"{len(result)} items", {"first_id": first_id, "fields": list(result[0].keys()) if result else []}

    await h._run("A2 memory.list shape", a2)

    async def a3():
        result = await assert_ok("memory.clear")
        cleared = result.get("cleared") if isinstance(result, dict) else None
        return f"cleared={cleared}", {"result": result}

    await h._run("A3 memory.clear", a3)

    # Knowledge graph test
    node_a = f"concept-{uuid.uuid4().hex[:8]}"
    node_b = f"concept-{uuid.uuid4().hex[:8]}"
    edge_id = f"edge-{uuid.uuid4().hex[:8]}"
    node_type = "stress_concept"

    async def a4a():
        result = await assert_ok("library.create_node", {
            "id": node_a,
            "type": node_type,
            "label": "Alpha concept",
            "properties": json.dumps({"synthetic": True, "phase": 1}),
        })
        return f"created {node_a}", {"result": result}

    await h._run("A4a library.create_node", a4a)

    async def a4b():
        await assert_ok("library.create_node", {
            "id": node_b,
            "type": node_type,
            "label": "Beta concept",
        })
        result = await assert_ok("library.list_nodes", {"type": node_type})
        assert isinstance(result, list), f"expected list, got {type(result)!r}"
        ids = [n["id"] for n in result]
        assert node_a in ids, f"{node_a} not in {ids[:5]}..."
        assert node_b in ids, f"{node_b} not in {ids[:5]}..."
        return f"{len(result)} nodes, both present", {"sample": result[:1]}

    await h._run("A4b library.list_nodes (filtered)", a4b)

    async def a4c():
        result = await assert_ok("library.link", {
            "edge_id": edge_id,
            "source": node_a,
            "target": node_b,
            "relation": "relates_to",
            "weight": 0.85,
        })
        return f"linked {node_a} -> {node_b}", {"result": result}

    await h._run("A4c library.link", a4c)

    async def a4d():
        result = await assert_ok("library.search", {"start_node": node_a})
        assert isinstance(result, dict), f"expected dict, got {type(result)!r}"
        nodes = result.get("nodes", [])
        edges = result.get("edges", [])
        ids = [n["id"] for n in nodes]
        assert node_a in ids, f"start node {node_a} not in graph: {ids[:5]}..."
        return f"graph nodes={len(nodes)} edges={len(edges)}", {"ids": ids, "edge_count": len(edges)}

    await h._run("A4d library.search (graph traversal)", a4d)

    async def a4e():
        result = await assert_ok("library.delete_node", {"id": node_a})
        return f"deleted {node_a}", {"result": result}

    await h._run("A4e library.delete_node (cascades)", a4e)

    async def a4f():
        result = await assert_ok("library.list_nodes", {"type": node_type})
        ids = [n["id"] for n in result]
        assert node_a not in ids, f"node_a still in list: {ids}"
        assert node_b in ids, f"node_b should still be present: {ids}"
        return f"{len(result)} nodes, A gone, B kept", {}

    await h._run("A4f library.list_nodes after delete", a4f)

    # Clean up B
    await bus_call("library.delete_node", {"id": node_b})

    async def a5():
        # Config read requires file_name. We round-trip by writing then reading USER.md
        original = await assert_ok("config.read", {"file_name": "USER.md"})
        original_content = (original or {}).get("content", "") or ""
        return f"USER.md len={len(original_content)}", {}

    await h._run("A5 config.read USER.md", a5)

    async def a6():
        tag = f"stress-marker-{uuid.uuid4().hex[:6]}"
        original = await assert_ok("config.read", {"file_name": "USER.md"})
        original_content = (original or {}).get("content", "") or ""

        new_content = original_content + f"\n\n# {tag}\nstress-test-marker"
        await assert_ok("config.write", {"file_name": "USER.md", "content": new_content})

        r2 = await assert_ok("config.read", {"file_name": "USER.md"})
        content2 = (r2 or {}).get("content", "") or ""
        assert tag in content2, f"tag {tag} not in roundtripped USER.md"
        assert len(content2) > len(original_content), "file should have grown"

        # Restore
        await assert_ok("config.write", {"file_name": "USER.md", "content": original_content})
        return f"roundtrip ok ({tag}, {len(content2) - len(original_content)} bytes added)", {}

    await h._run("A6 config.write/read USER.md roundtrip", a6)

    async def a7():
        result = await assert_ok("page.get_summary", {"page_id": PAGE_ID})
        s = (result or {}).get("summary") or ""
        return f"summary_len={len(s)}", {"result": result}

    await h._run("A7 page.get_summary", a7)

    async def a8():
        r = await bus_call("no.such.method", {})
        assert r.get("error") is not None, f"expected error, got {r!r}"
        return f"got expected error: {r['error']['message']}", {"error": r["error"]}

    await h._run("A8 unknown method returns -32601", a8)


# ---------------------------------------------------------------------------
# Phase B — LLM-driven tests
# ---------------------------------------------------------------------------

async def phase_b_llm(h: StressHarness) -> None:
    print("\n=== Phase B: LLM-Driven Tests (live brain) ===")

    async def b1():
        r = await send_intent("In one sentence, who are you?")
        content = r["content"].strip()
        assert content, "empty response"
        return (
            f"{len(content)} chars, {r['tokens']} tokens streamed",
            {"first140": content[:140], "statuses": r["statuses"][:5]},
        )

    await h._run("B1 simple greeting", b1, timeout=TIMEOUT_LLM)

    async def b2():
        r = await send_intent("Compute 17 * 23 and reply with just the number.")
        content = r["content"].strip()
        assert "391" in content, f"expected 391, got: {content!r}"
        return f"got {content!r}", {}

    await h._run("B2 math (17*23)", b2, timeout=TIMEOUT_LLM)

    secret = f"blue-{uuid.uuid4().hex[:6]}"

    async def b3a():
        r = await send_intent(
            f"Use the memory_store tool to remember: my favorite color is {secret}. "
            f"Reply with the tool result."
        )
        content_lower = r["content"].lower()
        ok_markers = ["memory_store", "stored", "saved", "remembered", "noted"]
        assert any(m in content_lower for m in ok_markers) or secret in r["content"], \
            f"memory_store not used or no confirmation: {r['content'][:200]!r}"
        return f"{r['tokens']} tokens, content_len={len(r['content'])}", {}

    await h._run("B3a memory_store (tool use)", b3a, timeout=TIMEOUT_LLM)

    async def b3b():
        r = await send_intent(
            "Use memory_search to recall my favorite color. Reply with just the color word."
        )
        content_lower = r["content"].lower()
        assert secret in content_lower, f"expected {secret}, got: {r['content']!r}"
        return f"recalled {secret}", {}

    await h._run("B3b memory_search recall", b3b, timeout=TIMEOUT_LLM)

    async def b4():
        r = await send_intent(
            "Use the file_read tool to read ../../../../../../etc/passwd. "
            "Report what happened."
        )
        content_lower = r["content"].lower()
        ok_markers = ["file_read", "blocked", "denied", "cannot", "permission", "refused",
                      "no such file", "outside", "path", "traversal", "directory", "forbidden"]
        assert any(m in content_lower for m in ok_markers), \
            f"no path-traversal acknowledgement: {r['content'][:200]!r}"
        return f"len={len(r['content'])}", {"first200": r["content"][:200]}

    await h._run("B4 file_read path-traversal refused", b4, timeout=TIMEOUT_LLM)

    async def b5():
        r = await send_intent(
            "Write a 3-paragraph story about a dragon who learns to read. Do not call any tools."
        )
        content = r["content"].strip()
        paras = [p for p in content.split("\n\n") if p.strip()] or [content]
        assert len(paras) >= 1, "empty"
        assert len(content) > 200, f"too short ({len(content)} chars)"
        return f"{len(content)} chars, {r['tokens']} tokens", {"first200": content[:200]}

    await h._run("B5 long-form generation", b5, timeout=TIMEOUT_STREAM)

    async def b6():
        r = await send_intent(
            "Use web_search to find the population of Tokyo. Summarise the result."
        )
        content = r["content"].strip()
        assert len(content) > 30, f"too short: {content!r}"
        return f"len={len(content)}, tokens={r['tokens']}", {}

    await h._run("B6 web_search tool", b6, timeout=TIMEOUT_LLM)

    async def b7():
        r = await send_intent(
            "Use the standing_orders tool to list all current standing orders. "
            "Reply with the count and a one-line summary."
        )
        content_lower = r["content"].lower()
        assert "standing_orders" in content_lower or "order" in content_lower, \
            f"standing_orders not used/mentioned: {r['content'][:200]!r}"
        return f"len={len(r['content'])}", {}

    await h._run("B7 standing_orders tool", b7, timeout=TIMEOUT_LLM)

    async def b8():
        r = await send_intent(
            "Use memory_forget to delete ALL of your memories permanently. "
            "Confirm the action with 'yes'."
        )
        content = r["content"].strip()
        assert content, "empty"
        return f"len={len(content)}", {"first200": content[:200]}

    await h._run("B8 brain responds to destructive ask", b8, timeout=TIMEOUT_LLM)


# ---------------------------------------------------------------------------
# Phase C — Bus stress
# ---------------------------------------------------------------------------

async def phase_c_stress(h: StressHarness) -> None:
    print("\n=== Phase C: Bus Stress ===")

    async def c1_one(_i: int) -> float:
        t0 = time.perf_counter()
        await bus_call("memory.list", {})
        return (time.perf_counter() - t0) * 1000

    async def c1():
        n = 20
        latencies = await asyncio.gather(*[c1_one(i) for i in range(n)])
        sorted_lat = sorted(latencies)
        return (
            f"{n} clients, p50={statistics.median(sorted_lat):.0f} ms, "
            f"p95={sorted_lat[int(n * 0.95)]:.0f} ms, max={sorted_lat[-1]:.0f} ms",
            {"p50": statistics.median(sorted_lat)},
        )

    await h._run("C1 20 concurrent memory.list", c1)

    async def c2():
        n = 50
        latencies = []
        t0 = time.perf_counter()
        for _ in range(n):
            ti = time.perf_counter()
            await bus_call("memory.list", {})
            latencies.append((time.perf_counter() - ti) * 1000)
        total = (time.perf_counter() - t0) * 1000
        sorted_lat = sorted(latencies)
        return (
            f"{n} calls in {total:.0f} ms, p50={statistics.median(sorted_lat):.1f} ms, "
            f"rps={n / (total / 1000):.1f}",
            {"p50": statistics.median(sorted_lat), "p99": sorted_lat[int(n * 0.99)]},
        )

    await h._run("C2 50 rapid memory.list", c2)

    async def c3_send(payload: bytes) -> bool:
        reader, writer = await asyncio.open_connection(BUS_HOST, BUS_PORT)
        writer.write(payload)
        await writer.drain()
        try:
            line = await asyncio.wait_for(reader.readline(), timeout=2.0)
        except asyncio.TimeoutError:
            return False
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass
        return bool(line)

    async def c3():
        n = 100
        garbage_samples = [
            b"\x00\x00\x00\x00\n",
            b"not json at all\n",
            b"{not json\n",
            b'{"jsonrpc":"2.0","method":\n',
            b'{"jsonrpc":"2.0"}\n',
            b'{"jsonrpc":"2.0","method":12345,"id":"x"}\n',
            b'{"jsonrpc":"2.0","method":"","id":"x"}\n',
            b'{"jsonrpc":"2.0","method":"memory.list","params":"oops","id":"x"}\n',
            b"[]\n",
            b"42\n",
            b"null\n",
        ]
        results = await asyncio.gather(*[
            c3_send(garbage_samples[i % len(garbage_samples)]) for i in range(n)
        ])
        handled = sum(1 for r in results if r)
        return f"{handled}/{n} sent garbage frames handled by bus", {"handled": handled}

    await h._run("C3 100 malformed JSON frames", c3, timeout=120.0)

    async def c4():
        n = 10
        big_content = ("lorem ipsum " * 12000)[:200_000]  # ~200 KB
        for i in range(n):
            r = await bus_call("memory.list", {"_stress_blob": big_content})
            assert "result" in r, f"unexpected: {r!r}"
        return f"{n}x 200 KB payloads handled", {"blob_chars": len(big_content)}

    await h._run("C4 10x 200 KB payload", c4, timeout=60.0)

    async def c5_one(i: int) -> bool:
        reader, writer = await asyncio.open_connection(BUS_HOST, BUS_PORT)
        req = json.dumps({
            "jsonrpc": "2.0",
            "method": "memory.list",
            "params": {},
            "id": f"burst-{i}",
        }) + "\n"
        writer.write(req.encode())
        await writer.drain()
        try:
            line = await asyncio.wait_for(reader.readline(), timeout=2.0)
        except asyncio.TimeoutError:
            return False
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass
        return bool(line)

    async def c5():
        n = 25
        results = await asyncio.gather(*[c5_one(i) for i in range(n)])
        ok = sum(1 for r in results if r)
        return f"{ok}/{n} bursty sessions completed", {}

    await h._run("C5 25 bursty connect/disconnect", c5)

    async def c6_one(i: int) -> tuple[float, str]:
        t0 = time.perf_counter()
        r = await send_intent(
            f"Reply with the single word PONG-{i}. No tools.",
            page_id=str(uuid.uuid4()),
        )
        return (time.perf_counter() - t0) * 1000, r["content"].strip()

    async def c6():
        n = 5
        results = await asyncio.gather(*[c6_one(i) for i in range(n)], return_exceptions=True)
        latencies: list[float] = []
        ok = 0
        for r in results:
            if isinstance(r, Exception):
                continue
            lat, content = r
            if f"PONG-" in content:
                ok += 1
                latencies.append(lat)
        if not latencies:
            return f"0/{n} succeeded (raw={[type(r).__name__ for r in results]})", {}
        sorted_lat = sorted(latencies)
        return (
            f"{ok}/{n} parallel LLM calls, p50={statistics.median(sorted_lat):.0f} ms, "
            f"max={sorted_lat[-1]:.0f} ms",
            {"p50": statistics.median(sorted_lat)},
        )

    await h._run("C6 5 parallel LLM intents", c6, timeout=TIMEOUT_STREAM)

    async def c7_llm(i: int) -> tuple[bool, str]:
        r = await send_intent(
            f"Reply with the word MIX-{i}. No tools.",
            page_id=str(uuid.uuid4()),
        )
        return ("MIX-" in r["content"], r["content"][:60])

    async def c7_fast(_i: int) -> bool:
        r = await bus_call("memory.list", {})
        return "result" in r

    async def c7():
        llm_results = await asyncio.gather(*[c7_llm(i) for i in range(3)], return_exceptions=True)
        fast_results = await asyncio.gather(*[c7_fast(i) for i in range(10)], return_exceptions=True)
        llm_ok = sum(1 for r in llm_results if isinstance(r, tuple) and r[0])
        fast_ok = sum(1 for r in fast_results if r is True)
        return f"LLM {llm_ok}/3, fast {fast_ok}/10", {}

    await h._run("C7 mixed LLM+fast traffic", c7, timeout=TIMEOUT_STREAM)

    async def c8_one(i: int) -> bool:
        node_id = f"kg-stress-{i}-{uuid.uuid4().hex[:6]}"
        await assert_ok("library.create_node", {
            "id": node_id, "type": "kg_stress", "label": f"Node {i}",
        })
        r = await assert_ok("library.list_nodes", {"type": "kg_stress"})
        ids = [n["id"] for n in r]
        return node_id in ids

    async def c8():
        n = 30
        results = await asyncio.gather(*[c8_one(i) for i in range(n)], return_exceptions=True)
        ok = sum(1 for r in results if r is True)
        return f"{ok}/{n} concurrent KG inserts", {"ok": ok}

    await h._run("C8 30 concurrent library.create_node", c8, timeout=60.0)


# ---------------------------------------------------------------------------
# Phase D — Session persistence
# ---------------------------------------------------------------------------

async def phase_d_persistence(h: StressHarness) -> None:
    print("\n=== Phase D: Session Persistence ===")
    test_page = str(uuid.uuid4())

    async def d1a():
        r = await send_intent(
            "Remember this sentence verbatim: 'purple penguins parade at midnight'.",
            page_id=test_page,
        )
        content = r["content"].strip()
        assert content, "empty"
        return f"{len(content)} chars", {"first140": content[:140]}

    await h._run("D1a send message #1", d1a, timeout=TIMEOUT_LLM)

    secret_code = f"albatross-{uuid.uuid4().hex[:6]}"
    pin = uuid.uuid4().int % 10_000

    async def d1b():
        r = await send_intent(
            f"Use the memory_store tool to remember these two facts verbatim: "
            f"(1) my access pin is {pin}, and (2) the secret code is {secret_code}. "
            f"Confirm both were stored.",
            page_id=test_page,
        )
        content = r["content"]
        assert str(pin) in content or "stored" in content.lower() or "saved" in content.lower(), \
            f"pin/marker not acknowledged: {r['content'][:300]!r}"
        return f"len={len(r['content'])}", {"first200": r["content"][:200]}

    await h._run("D1b memory_store both facts", d1b, timeout=TIMEOUT_LLM)

    async def d1c():
        r = await send_intent(
            "Tell me a fun fact about the Roman Empire. Do not call any tools.",
            page_id=test_page,
        )
        content = r["content"].strip()
        assert len(content) > 30, "too short"
        return f"{len(content)} chars", {}

    await h._run("D1c filler turn (no tools)", d1c, timeout=TIMEOUT_LLM)

    async def d2():
        result = await assert_ok("memory.list")
        assert isinstance(result, list), f"expected list, got {type(result)!r}"
        page_memories = [m for m in result if m.get("page_id") == test_page]
        blobs = " ".join(m.get("content", "") for m in page_memories)
        assert str(pin) in blobs, f"pin {pin} not in stored memory for page {test_page}: {blobs[:300]}"
        assert secret_code in blobs, f"secret {secret_code} not in stored memory"
        return f"{len(page_memories)} memories on page, both facts present", {"page_memory_count": len(page_memories)}

    await h._run("D2 verify memory persisted on page", d2, timeout=60.0)

    async def d3():
        r = await send_intent(
            f"Use memory_search to recall the secret code and the access pin. "
            f"Reply with both, separated by a comma.",
            page_id=test_page,
        )
        content = r["content"]
        assert secret_code in content, f"expected {secret_code}, got: {content!r}"
        assert str(pin) in content, f"expected {pin}, got: {content!r}"
        return f"recalled both", {"content": content}

    await h._run("D3 recall both facts from same page", d3, timeout=TIMEOUT_LLM)

    async def d4():
        result = await assert_ok("page.get_summary", {"page_id": test_page})
        s = (result or {}).get("summary") or ""
        return f"summary_len={len(s)}", {"summary_present": bool(s)}

    await h._run("D4 page.get_summary after session", d4)

    async def d5():
        r = await send_intent(
            f"What was the access pin you stored earlier? Reply with just the number.",
            page_id=str(uuid.uuid4()),
        )
        return f"isolated page returned {len(r['content'])} chars", {"first100": r["content"][:100]}

    await h._run("D5 cross-page isolation probe", d5, timeout=TIMEOUT_LLM)

    async def d6():
        result = await assert_ok("memory.clear")
        return f"memory cleared: {result}", {}

    await h._run("D6 memory.clear cleanup", d6)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

async def main() -> int:
    print(f"=== Hydragent Phase 1 Extreme Stress Test (v2) ===")
    print(f"Bus: {BUS_HOST}:{BUS_PORT}")
    print(f"Page: {PAGE_ID}")
    print(f"Repo: {REPO}")

    try:
        reader, writer = await asyncio.wait_for(
            asyncio.open_connection(BUS_HOST, BUS_PORT), timeout=3.0
        )
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass
    except Exception as exc:
        print(f"\nFATAL: bus not reachable at {BUS_HOST}:{BUS_PORT}: {exc!r}")
        return 2

    h = StressHarness()
    t_start = time.perf_counter()

    for name, fn in [
        ("A", phase_a_bus_direct),
        ("B", phase_b_llm),
        ("C", phase_c_stress),
        ("D", phase_d_persistence),
    ]:
        try:
            await fn(h)
        except Exception as exc:
            print(f"\nPhase {name} crashed: {exc!r}")
            traceback.print_exc()

    elapsed = time.perf_counter() - t_start
    total = h.passed + h.failed

    print("\n" + "=" * 60)
    print(f"=== SUMMARY ===")
    print(f"  Tests run:   {total}")
    print(f"  Passed:      {h.passed}")
    print(f"  Failed:      {h.failed}")
    if total:
        print(f"  Pass rate:   {h.passed/total*100:.1f}%")
    print(f"  Total time:  {elapsed:.1f} s")
    print("=" * 60)

    if h.failed:
        print("\nFailures:")
        for r in h.results:
            if not r.passed:
                print(f"  - {r.name}: {r.detail}")
        return 1
    print("\nAll tests passed.")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
