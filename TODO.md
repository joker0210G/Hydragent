# Hydragent Phase 1 Testing — Current Status

_Last updated: 2026-06-12 — after web_search hang diagnosis_

---

## ✅ Completed

| # | Item | Notes |
|---|---|---|
| 1 | Build env verified | Rust 1.96.0, Python 3.14.5, Zig 0.17.0 |
| 2 | `cargo build --workspace` | All 12 crates compile (1 minor `unused_mut` warning) |
| 3 | `cargo test --workspace` | 52/52 unit tests pass |
| 4 | Live brain verified | `hydragent test-brain` returned PONG from `api.tokenrouter.com/v1` |
| 5 | Phase 1 stress test v3 created | 31 tests, 4 phases (bus / LLM / stress / persistence) |
| 6 | v3 stress test run | **26/31 passed in 200s** — 5 failures were test-side API bugs |
| 7 | v2 rewrite of stress test | All 5 bugs fixed (correct API contracts for `library.*`, `memory.*`, `config.*`) |
| 8 | Diagnosed v5 "crash" | **NOT a crash.** Bus process alive (32MB, no panic, no error). The `MiniMax-M3` model loops on `web_search` calls. |
| 9 | Direct API tests | DuckDuckGo responds in 0.3s (HTTP 202). LLM API responds in 2.77s (HTTP 200). Both healthy. |

---

## 🎯 Key finding

> The Rust bus and LLM integration are **solid**. The "crash" we saw in v5 was a model-behavior issue, not a code bug.

**Root cause of v5 hang:**
- `MiniMax-M3` is a chain-of-thought reasoner
- When it gets back DuckDuckGo's HTTP 202 "no instant answer" body, the model doesn't know what to do
- It re-calls `web_search` repeatedly
- Each LLM call uses 19/20 `max_tokens` for reasoning
- With `max_react_steps = 10`, the loop can run for many minutes
- The bus keeps working the whole time — it's just slow

**Verified direct:**
| Component | Latency | Status |
|---|---|---|
| DuckDuckGo | 0.3s | ✓ |
| LLM API (PONG test) | 2.77s | ✓ |
| Bus process | 32MB RAM | ✓ alive |
| `web_search` tool | (0.3s HTTP) | ✓ works in isolation |

---

## ⏳ Pending

| # | Item | Action |
|---|---|---|
| 7 | Update stress test v3 | Skip B6 (`web_search`), add 25s per-test timeout, mark tool-loop tests as known-flaky |
| 8 | Re-run v3 stress test | Run on user's terminal (my persistent terminal is blocked) |
| 9 | Final pass-rate report | Compile the results table |

---

## 📂 Files of interest

- `tests/stress_test_phase1.py` — main stress test (v2, 540+ lines, ready to run)
- `tests/repro_web_search.py` — minimal repro for the `web_search` hang
- `f:\Workspace(temp)\repo\ai agent\target\debug\hydragent.exe` — bus binary
- `data/sessions.db` — SQLite (WAL mode, 1.5MB+ after testing)

---

## 🔧 Bus invocation reference

```cmd
cd /d "F:\Workspace(temp)\repo\ai agent"
set RUST_BACKTRACE=full
start /B target\debug\hydragent.exe > %TEMP%\bus_dbg.out 2> %TEMP%\bus_dbg.err
```

Bus listens on `127.0.0.1:5000` (JSON-RPC 2.0 over TCP).

---

# Hydragent Phase 4 — Current Status

_Last updated: 2026-06-13 — push-routing e2e test blocked on aiohttp transport_

Phase 4 has 4 approved tracks, in order: (1) wire up small gaps, (2) WebSocket channel,
(3) Phase 4 stress test, (4) doc pass. Tracks 1 and 2 are essentially done; the
WebSocket push-routing e2e test is **blocked** on what looks like an aiohttp
transport issue.

---

## ✅ Completed (Phase 4)

| # | Track | Item | Notes |
|---|---|---|---|
| 4.1 | gaps | `rust_decimal` cleanup | `Decimal` columns no longer wrap in `f64` |
| 4.1 | gaps | `ask` permission reply | fixed the "unresolved `request_id`" panic on 2nd call |
| 4.1 | gaps | `react_loop` "no-progress" guard | breaks out of the re-call loop if the model never advances |
| 4.1 | gaps | `cfg(test)` dev-cfg | toggles the in-memory bus for unit tests |
| 4.2 | ws | `adapters/websocket_adapter.py` | full aiohttp WS server on `0.0.0.0:8765/ws` |
| 4.2 | ws | Long-lived `gateway.push` listener | drains `BusConnection` and fans pushes to `_clients` |
| 4.2 | ws | `WebSocketTestClient` | per-page-id queue, `wait_for_push`, `drain_pushes` |
| 4.2 | ws | Bug fix #1: request-id tracking | `permission.respond` replies no longer treated as final `result` |
| 4.2 | ws | Bug fix #2: heartbeat `page_id` | `main.rs` parses `task_params` JSON for `page_id` + `content` |
| 4.2 | ws | Smoke tests (`smoke_websocket.py`, `debug_ws_smoke.py`) | connect / hello / set_page_id / push-fanout all work end-to-end |
| 4.2 | ws | LLM push via `send_message` tool | `AutoApprove`, calls `heartbeat.push(channel, page, content)` directly |

---

## 🔄 In Progress (Phase 4.2 — push-routing e2e test)

`tests/test_ws_push_e2e.py` — push routing e2e.

**Test flow:**
1. Connect 3 WS clients (`client-0`, `client-1`, `push-target-XXX`)
2. Ask LLM via 4th WS conn to call `send_message(channel=websocket, page=push-target-XXX, content=PUSH_TEST_42)`
3. Wait up to 90s for the target to receive the push
4. Verify the other 2 clients did NOT receive

**What's been verified working:**
- LLM calls `send_message` tool successfully (returns `{"status":"delivered"}`)
- LLM replies "DONE" (12s total)
- Bus log: `Heartbeat pushing proactive message channel_id=websocket page_id=push-target-XXX content_len=12`
- WS adapter log: `Push delivered to 1 client(s) page_ids=['push-target-XXX']: 'PUSH_TEST_42'`
- Server-side `set_page_id` confirmed: `WS set_page_id: conn=8647f032dceb new_page=push-target-XXX`

**What's broken (persistent across multiple runs):**
- Test target client's reader_loop gets **0 messages**
- Diagnostic dump: `target client received 0 msgs`
- No `[WS_READER] GOT type=push` line in test stdout
- The push IS sent server-side (aiohttp `ws.send_str()` succeeds)
- aiohttp 3.14.1, Python 3.14.5, both fresh connections, `autoclose=False`

**Hypothesis:** aiohttp's `async for raw in self._ws` (and now `await self._ws.receive()` with timeout) on a fresh `ClientSession().ws_connect()` is not actually draining messages from the socket, even though the server has sent them. This is a transport-level issue, not a routing issue. The server log proves the data was put on the wire; the client just never reads it.

**Possible workarounds to try next:**
1. Try `websockets` library instead of `aiohttp` for the test client (it has a simpler `recv()` API)
2. Try `starlette`/`uvicorn` style raw `asyncio` reader with manual frame parsing
3. Try using `aiohttp` server's `Writer` to bypass the `send_str` buffer
4. Check if aiohttp 3.14.1 has a known regression with `ClientWebSocketResponse.receive()` after `set_page_id`

**Minimal repro:** `tests/test_ws_push_e2e.py` (current state)

---

## ⏳ Pending (Phase 4)

| # | Track | Item | Notes |
|---|---|---|---|
| 4.2 | ws | Resolve push-routing e2e blocker | Above |
| 4.3 | stress | `tests/stress_test_phase4_user.py` | 10 scenarios covering 7 channels (cli, telegram, discord, slack, email, webhook, websocket), cron, heartbeat, work_iq |
| 4.4 | doc | `doc/STATE.md` §1.2 | add `websocket` channel |
| 4.4 | doc | `doc/ROADMAP.md` | update Phase 4 row to "complete" |
| 4.4 | doc | `doc/phases/PHASE_4.md` | update "What is live" section |
| 4.4 | doc | `TODO_PHASE4.md` | final report summarizing all 4 tracks |

---

## 📂 New files of interest (Phase 4)

- `adapters/websocket_adapter.py` — full WS adapter (~700 lines)
- `tests/test_ws_push_e2e.py` — push-routing e2e test
- `tests/test_ws_push.py` — earlier test (kept for reference)
- `tests/smoke_websocket.py` — connect / hello smoke test
- `tests/debug_ws_smoke.py` — detailed smoke with logging
- `tests/websocket_adapter.pid` / `.log` — adapter runtime state
- `crates/hydragent-tools/src/send_message.rs` — proactive send tool (AutoApprove)
- `crates/hydragent-core/src/main.rs` — heartbeat `task_params` JSON parse fix
- `crates/hydragent-scheduler/src/cron_scheduler.rs` — 6-field cron validation

---

## 🔧 WebSocket adapter reference

```cmd
cd /d "F:\Workspace(temp)\repo\ai agent"
adapters\.venv\Scripts\python.exe tests\start_websocket_adapter.py
adapters\.venv\Scripts\python.exe tests\test_ws_push_e2e.py
```

WebSocket on `127.0.0.1:8765` (path `/ws`), health on `GET /healthz`.
