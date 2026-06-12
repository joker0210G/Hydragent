"""
WebSocket Channel Adapter for Hydragent
======================================

This is Phase 4's new chat channel. It exposes a WebSocket endpoint on a
configurable port (default 8765) that any compliant WebSocket client (browser,
Node, Python `websockets`, the upcoming `miniapp`, etc.) can connect to. The
adapter:

  1. Opens one long-lived bus connection on startup, calls
     `gateway.register` with `channel_id: "websocket"`, and from then on
     listens for `gateway.push` events from the Rust core.
  2. For every WebSocket client that connects, it generates a unique
     `page_id` of the form `ws-<connection_id>` and forwards every push
     event addressed to that page (or to `*` for broadcasts) to that client
     as a JSON message of the form
        {"type":"push", "channel_id":"...", "page_id":"...", "content":"..."}
  3. Accepts inbound messages from WebSocket clients. Messages are expected
     to be JSON objects of the form
        {"content": "user prompt", "page_id": "ws-<conn>" | "<custom>"}
     The adapter sends them to the bus as `intent.submit` with
     `channel_id: "websocket"` and `user_id: "ws-user-<conn>"`. The
     streamed response (`response.token`, `response.status`,
     `response.complete`, `response.permission_request`) is sent back over
     the same WebSocket as JSON of the form
        {"type":"token", "token": "..."}
        {"type":"status", "status": "..."}
        {"type":"complete"}
        {"type":"permission_request", "request_id": "...", "tool_id": "..."}
        {"type":"permission_decision", "request_id": "...", "approved": ...}
     plus a final
        {"type":"result", "page_id": "...", "content": "..."}
     that the test client can match to its send.

The bus is JSON-RPC 2.0 over a newline-delimited TCP socket on `BUS_PORT`
(default 5000). See `crates/hydragent-bus/PROTOCOL.md` for details.

The design is intentionally minimal: it doesn't add auth, doesn't deduplicate
clients, and doesn't try to do streaming edit tricks like the Slack adapter
(browsers usually just want a single "Done" message anyway). The
deduplication, rate-limiting, and adapter registration all live in the Rust
`GatewayRouter` and the orchestrator — this file is purely the IO plumbing.

Usage:
    # default ports
    python adapters/websocket_adapter.py
    # override ports
    WEBSOCKET_PORT=9123 BUS_PORT=5000 python adapters/websocket_adapter.py
    # also exported for tests
    python -c "from adapters.websocket_adapter import WebSocketTestClient; ..."
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import sys
import time
import uuid
from typing import Any, Awaitable, Callable, Optional

from aiohttp import WSMsgType, web
from dotenv import load_dotenv

# Set up logging
logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO
)
logger = logging.getLogger("websocket_adapter")

load_dotenv()
BUS_PORT = int(os.getenv("BUS_PORT", 5000))
WEBSOCKET_PORT = int(os.getenv("WEBSOCKET_PORT", 8765))
WEBSOCKET_HOST = os.getenv("WEBSOCKET_HOST", "0.0.0.0")
CHANNEL_ID = "websocket"

# Active WebSocket client set. Each entry is (page_id, ws). The set is
# guarded by an asyncio.Lock so we can safely add/remove across tasks.
_clients: set[tuple[str, "web.WebSocketResponse"]] = set()
_clients_lock = asyncio.Lock()


# ---------------------------------------------------------------------------
# Bus connection (mirrors the pattern used by webhook/slack adapters)
# ---------------------------------------------------------------------------


class BusConnection:
    """One bus connection. Used both for push-listener (long-lived) and for
    intent.submit (short-lived, one per inbound WS message)."""

    def __init__(self, reader, writer):
        self.reader = reader
        self.writer = writer

    @classmethod
    async def connect(cls) -> "BusConnection":
        reader, writer = await asyncio.open_connection("127.0.0.1", BUS_PORT)
        return cls(reader, writer)

    async def register(self, channel_id: str = CHANNEL_ID) -> None:
        req = {
            "jsonrpc": "2.0",
            "method": "gateway.register",
            "params": {"channel_id": channel_id},
            "id": "reg-" + uuid.uuid4().hex[:8],
        }
        self.writer.write((json.dumps(req) + "\n").encode())
        await self.writer.drain()
        line = await self.reader.readline()
        try:
            decoded = line.decode().strip()
        except Exception:
            decoded = "<binary>"
        logger.info("Registered on Event Bus as channel=%s: %s", channel_id, decoded)

    async def close(self) -> None:
        try:
            self.writer.close()
            await self.writer.wait_closed()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Outbound: push events from the bus → all connected WebSocket clients
# ---------------------------------------------------------------------------


def _extract_content(raw: Any) -> str:
    """Pushes sometimes wrap their content in a JSON object. Best-effort
    unwrap: if it parses as JSON with a `content` or `message` field, return
    that, otherwise return the string as-is."""
    if not isinstance(raw, str):
        return str(raw)
    s = raw.strip()
    if s.startswith("{"):
        try:
            data = json.loads(s)
            if isinstance(data, dict):
                return data.get("content") or data.get("message") or raw
        except Exception:
            pass
    return raw


async def _broadcast_push(push_params: dict) -> None:
    """Fan a single push out to every connected WebSocket client whose
    `page_id` matches (or to all clients on a broadcast)."""
    channel_id = push_params.get("channel_id", CHANNEL_ID)
    target_page = push_params.get("page_id")
    content = _extract_content(push_params.get("content", ""))

    payload = json.dumps({
        "type": "push",
        "channel_id": channel_id,
        "page_id": target_page,
        "content": content,
        "timestamp": int(time.time() * 1000),
    }, ensure_ascii=False)

    async with _clients_lock:
        targets = list(_clients)

    delivered = 0
    matched_pids = []
    for page_id, ws in targets:
        # If the push is addressed to a specific page, only deliver to that
        # client. Broadcasts (`page_id == "*"` or None) go to all.
        if target_page and target_page != "*" and target_page != page_id:
            logger.debug("push SKIP page_id=%s (target=%s)", page_id, target_page)
            continue
        try:
            await ws.send_str(payload)
            delivered += 1
            matched_pids.append(page_id)
        except Exception as e:
            logger.warning("push delivery failed for %s: %s", page_id, e)
    if delivered:
        logger.info("Push delivered to %d client(s) page_ids=%r: %r",
                    delivered, matched_pids, content[:80])
    else:
        logger.warning("Push had NO recipients. target_page=%r, _clients=%r",
                       target_page, [pid for pid, _ in targets])


async def listen_for_pushes() -> None:
    """Long-lived task: re-establish the bus connection if it drops, route
    every `gateway.push` event to connected WebSocket clients."""
    backoff = 1.0
    while True:
        try:
            logger.info("Opening long-lived bus connection for push notifications...")
            bus = await BusConnection.connect()
            await bus.register()
            backoff = 1.0
            while True:
                line = await bus.reader.readline()
                if not line:
                    logger.warning("Bus push connection lost; will reconnect.")
                    break
                try:
                    msg = json.loads(line.decode().strip())
                except Exception as e:
                    logger.error("Bad push JSON: %s", e)
                    continue
                if msg.get("method") == "gateway.push":
                    push_params = msg.get("params") or {}
                    await _broadcast_push(push_params)
        except Exception as e:
            logger.error("Push listener error: %s; reconnect in %.1fs", e, backoff)
        try:
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2.0, 30.0)
        except asyncio.CancelledError:
            return


# ---------------------------------------------------------------------------
# Inbound: WebSocket client → bus
# ---------------------------------------------------------------------------


async def _send_intent_and_stream(
    ws: "web.WebSocketResponse",
    page_id: str,
    user_id: str,
    content: str,
    permission_cb: Optional[Callable[[dict], Awaitable[bool]]] = None,
    push_cb: Optional[Callable[[dict], Awaitable[None]]] = None,
) -> str:
    """Open a bus connection, send `intent.submit`, stream the response
    back to the WebSocket. Returns the final response text."""
    bus = None
    try:
        bus = await BusConnection.connect()
    except Exception as e:
        err = {"type": "error", "message": f"core engine offline: {e}"}
        await ws.send_str(json.dumps(err, ensure_ascii=False))
        return ""

    req_id = str(uuid.uuid4())
    req = {
        "jsonrpc": "2.0",
        "method": "intent.submit",
        "params": {
            "page_id": page_id,
            "channel_id": CHANNEL_ID,
            "user_id": user_id,
            "content": content,
            "attachments": [],
            "metadata": {"transport": "websocket"},
            "timestamp": int(time.time() * 1000),
            "priority": "normal",
        },
        "id": req_id,
    }

    accumulated_tokens: list[str] = []
    try:
        bus.writer.write((json.dumps(req) + "\n").encode())
        await bus.writer.drain()

        while True:
            line = await bus.reader.readline()
            if not line:
                break
            try:
                msg = json.loads(line.decode().strip())
            except Exception:
                continue
            logger.debug("WS bus line: method=%s id=%s keys=%s",
                         msg.get("method"), msg.get("id"),
                         list(msg.keys()))
            method = msg.get("method")
            # The bus may also send us responses to OTHER requests we
            # issued on this same connection (e.g. permission.respond).
            # Those have a "result" field too, but their `id` differs
            # from our `intent.submit` id, so we must ignore them here
            # and keep streaming until the real final answer arrives.
            if method is None and "result" in msg and msg.get("id") != req_id:
                logger.debug("WS bus: ignoring result for other id=%s",
                             msg.get("id"))
                continue
            if method == "response.token":
                token = msg["params"]["token"]
                accumulated_tokens.append(token)
                await ws.send_str(json.dumps({
                    "type": "token",
                    "page_id": page_id,
                    "token": token,
                }, ensure_ascii=False))
            elif method == "response.status":
                await ws.send_str(json.dumps({
                    "type": "status",
                    "page_id": page_id,
                    "status": msg["params"]["status"],
                }, ensure_ascii=False))
            elif method == "response.permission_request":
                params = msg["params"]
                await ws.send_str(json.dumps({
                    "type": "permission_request",
                    "page_id": page_id,
                    "request_id": params["request_id"],
                    "tool_id": params.get("tool_id", ""),
                    "tier": params.get("tier", "Prompt"),
                    "summary": params.get("params_summary", ""),
                }, ensure_ascii=False))
                if permission_cb is not None:
                    approved = await permission_cb(params)
                    resp = {
                        "jsonrpc": "2.0",
                        "method": "permission.respond",
                        "params": {
                            "request_id": params["request_id"],
                            "approved": approved,
                        },
                        "id": str(uuid.uuid4()),
                    }
                    bus.writer.write((json.dumps(resp) + "\n").encode())
                    await bus.writer.drain()
            elif method == "response.complete":
                await ws.send_str(json.dumps({
                    "type": "complete",
                    "page_id": page_id,
                }, ensure_ascii=False))
            elif "result" in msg:
                result = msg["result"] or {}
                if isinstance(result, dict) and "content" in result:
                    final_text = result["content"]
                else:
                    final_text = "".join(accumulated_tokens)
                logger.info("WS bus result: content_len=%d, accum_tokens_len=%d, "
                            "result_keys=%s",
                            len(final_text), sum(len(t) for t in accumulated_tokens),
                            list(result.keys()) if isinstance(result, dict) else type(result).__name__)
                await ws.send_str(json.dumps({
                    "type": "result",
                    "page_id": page_id,
                    "content": final_text,
                }, ensure_ascii=False))
                return final_text
            elif "error" in msg and msg.get("id") in (None, req_id):
                err_text = msg["error"].get("message", "unknown error")
                await ws.send_str(json.dumps({
                    "type": "error",
                    "page_id": page_id,
                    "message": err_text,
                }, ensure_ascii=False))
                return err_text
    except Exception as e:
        logger.error("bus stream error: %s", e)
        try:
            await ws.send_str(json.dumps({
                "type": "error",
                "page_id": page_id,
                "message": f"stream error: {e}",
            }, ensure_ascii=False))
        except Exception:
            pass
    finally:
        if bus is not None:
            await bus.close()

    final = "".join(accumulated_tokens)
    if final:
        await ws.send_str(json.dumps({
            "type": "result",
            "page_id": page_id,
            "content": final,
        }, ensure_ascii=False))
    return final


async def _ws_handler(request: web.Request) -> "web.WebSocketResponse":
    """Aiohttp WebSocket endpoint. Each connection is identified by a
    `page_id` of `ws-<conn>`. Clients can override the page_id by sending
    a JSON `{"set_page_id": "..."}` first, but most won't bother."""
    ws = web.WebSocketResponse(heartbeat=30.0, max_msg_size=2 * 1024 * 1024)
    await ws.prepare(request)
    conn_id = uuid.uuid4().hex[:12]
    page_id = f"ws-{conn_id}"
    user_id = f"ws-user-{conn_id}"
    # Per-connection state (mutable from within the handler)
    ws_conn_state: dict = {"auto_approve": True}
    logger.info("WS connect: conn=%s page=%s remote=%s",
                conn_id, page_id, request.remote)

    async with _clients_lock:
        _clients.add((page_id, ws))

    # Greet the client so they know we're live
    try:
        await ws.send_str(json.dumps({
            "type": "hello",
            "channel_id": CHANNEL_ID,
            "page_id": page_id,
            "user_id": user_id,
            "timestamp": int(time.time() * 1000),
        }, ensure_ascii=False))
    except Exception:
        pass

    try:
        async for raw in ws:
            if raw.type == WSMsgType.TEXT:
                try:
                    payload = json.loads(raw.data)
                except Exception:
                    await ws.send_str(json.dumps({
                        "type": "error",
                        "page_id": page_id,
                        "message": f"invalid JSON: {raw.data[:120]!r}",
                    }, ensure_ascii=False))
                    continue
                if not isinstance(payload, dict):
                    await ws.send_str(json.dumps({
                        "type": "error",
                        "page_id": page_id,
                        "message": "expected JSON object",
                    }, ensure_ascii=False))
                    continue
                # Allow client to override page_id (so they can do multi-tab
                # sessions or reconnect with the same id).
                if "set_page_id" in payload:
                    new_pid = str(payload["set_page_id"]).strip()
                    if new_pid:
                        async with _clients_lock:
                            _clients.discard((page_id, ws))
                            page_id = new_pid
                            _clients.add((page_id, ws))
                        logger.info("WS set_page_id: conn=%s new_page=%s",
                                    conn_id, page_id)
                        await ws.send_str(json.dumps({
                            "type": "page_set",
                            "page_id": page_id,
                        }, ensure_ascii=False))
                    continue
                # Allow client to toggle auto-approve (e.g. prompt user)
                if "set_auto_approve" in payload:
                    ws_conn_state["auto_approve"] = bool(payload["set_auto_approve"])
                    await ws.send_str(json.dumps({
                        "type": "auto_approve_set",
                        "auto_approve": ws_conn_state["auto_approve"],
                    }, ensure_ascii=False))
                    continue
                # Allow client to ping (e.g. heartbeat)
                if payload.get("type") == "ping":
                    await ws.send_str(json.dumps({
                        "type": "pong",
                        "page_id": page_id,
                        "timestamp": int(time.time() * 1000),
                    }, ensure_ascii=False))
                    continue
                # Otherwise treat it as an intent
                content = str(payload.get("content", "")).strip()
                if not content:
                    await ws.send_str(json.dumps({
                        "type": "error",
                        "page_id": page_id,
                        "message": "content is required",
                    }, ensure_ascii=False))
                    continue
                # Per-connection auto_approve flag (default True so headless
                # clients like the miniapp, the test harness, and curl work
                # out of the box; set to False via {"set_auto_approve":false}
                # for a client that wants to prompt the user).
                async def _perm_cb(params: dict) -> bool:
                    return bool(ws_conn_state.get("auto_approve", True))
                await _send_intent_and_stream(ws, page_id, user_id, content,
                                              permission_cb=_perm_cb)
            elif raw.type == WSMsgType.ERROR:
                logger.warning("WS error conn=%s: %s", conn_id, ws.exception())
                break
            elif raw.type == WSMsgType.CLOSE:
                break
    finally:
        async with _clients_lock:
            _clients.discard((page_id, ws))
        logger.info("WS disconnect: conn=%s page=%s", conn_id, page_id)
    return ws


# ---------------------------------------------------------------------------
# HTTP handlers (used for the Phase 4 stress test)
# ---------------------------------------------------------------------------


async def _healthz(request: web.Request) -> web.Response:
    async with _clients_lock:
        n = len(_clients)
    return web.json_response({
        "ok": True,
        "channel_id": CHANNEL_ID,
        "clients": n,
        "websocket_port": WEBSOCKET_PORT,
    })


# ---------------------------------------------------------------------------
# App lifecycle
# ---------------------------------------------------------------------------


async def _on_startup(app: web.Application) -> None:
    app["push_listener"] = asyncio.create_task(listen_for_pushes())


async def _on_cleanup(app: web.Application) -> None:
    task = app.get("push_listener")
    if task is not None:
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
    # Close all clients cleanly
    async with _clients_lock:
        clients = list(_clients)
        _clients.clear()
    for _pid, ws in clients:
        try:
            await ws.close(code=1001, message=b"server shutdown")
        except Exception:
            pass


def build_app() -> web.Application:
    app = web.Application()
    app.on_startup.append(_on_startup)
    app.on_cleanup.append(_on_cleanup)
    app.router.add_get("/ws", _ws_handler)
    app.router.add_get("/healthz", _healthz)
    return app


# ---------------------------------------------------------------------------
# Test client (exported for the stress test)
# ---------------------------------------------------------------------------


class WebSocketTestClient:
    """Minimal async WebSocket client for tests. Wraps aiohttp's client
    WebSocket so the stress test can:
        - connect
        - send a prompt
        - collect streamed tokens
        - collect the final `result` message
        - receive `push` events as they arrive
    The same instance can be reused for many messages.
    """

    def __init__(self, url: str, page_id: Optional[str] = None,
                 auto_approve: bool = True):
        self.url = url
        self.page_id = page_id
        self.auto_approve = auto_approve
        self._session: Optional[Any] = None
        self._ws: Optional[Any] = None
        # Per-request future table: when a `result` message comes in, we
        # resolve the future stored under page_id.
        self._pending: dict[str, asyncio.Future] = {}
        # Push events received since the last pop.
        self._pushes: asyncio.Queue = asyncio.Queue()
        # All messages received since connect() (for debugging).
        self._all_received: list[dict] = []
        self._reader_task: Optional[asyncio.Task] = None
        self._connected = False

    async def __aenter__(self) -> "WebSocketTestClient":
        await self.connect()
        return self

    async def __aexit__(self, *exc) -> None:
        await self.close()

    async def connect(self) -> None:
        import aiohttp
        self._session = aiohttp.ClientSession()
        self._ws = await self._session.ws_connect(self.url, autoclose=False)
        # Wait for the hello
        hello = await self._ws.receive(timeout=10.0)
        if hello.type != aiohttp.WSMsgType.TEXT:
            raise RuntimeError(f"expected hello, got {hello.type}")
        hello_data = json.loads(hello.data)
        if hello_data.get("type") != "hello":
            raise RuntimeError(f"expected hello message, got {hello_data!r}")
        # The server gives us a default page_id; we keep it unless the
        # caller overrode it.
        if not self.page_id:
            self.page_id = hello_data.get("page_id")
        else:
            # Tell the server to switch to our chosen page_id
            await self._ws.send_str(json.dumps({"set_page_id": self.page_id}))
            ack = await self._ws.receive(timeout=5.0)
            ack_data = json.loads(ack.data)
            if ack_data.get("type") != "page_set":
                raise RuntimeError(f"expected page_set, got {ack_data!r}")
        self._connected = True
        self._reader_task = asyncio.create_task(self._reader_loop())
        print(f"[WS_CONNECT] DONE pid={self.page_id} reader_task={self._reader_task}", flush=True)

    async def close(self) -> None:
        self._connected = False
        if self._reader_task is not None:
            self._reader_task.cancel()
            try:
                await self._reader_task
            except (asyncio.CancelledError, Exception):
                pass
        if self._ws is not None:
            try:
                await self._ws.close()
            except Exception:
                pass
        if self._session is not None:
            await self._session.close()

    async def _reader_loop(self) -> None:
        """Background task: pull messages off the socket, fan them out to
        per-request futures or the push queue."""
        print(f"[WS_READER] STARTED pid={self.page_id}", flush=True)
        try:
            while not self._ws.closed:
                try:
                    raw = await self._ws.receive(timeout=1.0)
                except asyncio.TimeoutError:
                    continue
                if raw.type == aiohttp.WSMsgType.CLOSE:
                    print(f"[WS_READER] CLOSE pid={self.page_id}", flush=True)
                    break
                if raw.type == aiohttp.WSMsgType.CLOSED:
                    print(f"[WS_READER] CLOSED pid={self.page_id}", flush=True)
                    break
                if raw.type != aiohttp.WSMsgType.TEXT:
                    print(f"[WS_READER] non-text type={raw.type} pid={self.page_id}", flush=True)
                    continue
                try:
                    msg = json.loads(raw.data)
                except Exception as e:
                    print(f"[WS_READER] bad JSON: {e!r} data={raw.data[:80]!r}", flush=True)
                    continue
                mtype = msg.get("type")
                self._all_received.append(msg)
                print(f"[WS_READER] GOT type={mtype} page_id={msg.get('page_id')!r} pid={self.page_id}", flush=True)
                if mtype == "result":
                    pid = msg.get("page_id")
                    fut = self._pending.pop(pid, None)
                    if fut and not fut.done():
                        fut.set_result(msg)
                elif mtype == "push":
                    print(f"[WS_READER] PUSH RECEIVED pid={msg.get('page_id')!r} content={str(msg.get('content',''))[:60]!r}", flush=True)
                    await self._pushes.put(msg)
                # token/status/complete/permission_request are ignored
                # at this layer (the caller may not care about them).
        except asyncio.CancelledError:
            print(f"[WS_READER] CANCELLED pid={self.page_id}", flush=True)
            return
        except Exception as e:
            print(f"[WS_READER] ERROR pid={self.page_id}: {e!r}", flush=True)
            import traceback
            traceback.print_exc()
            logger.debug("WS reader loop ended: %s", e)
        print(f"[WS_READER] ENDED pid={self.page_id}", flush=True)

    async def send(self, content: str, timeout: float = 60.0,
                   expected_page_id: Optional[str] = None) -> dict:
        """Send a prompt and wait for the final `result` message. Returns
        the result dict (with `type`, `page_id`, `content`)."""
        if not self._connected:
            raise RuntimeError("not connected")
        target_pid = expected_page_id or self.page_id
        if target_pid != self.page_id:
            await self._ws.send_str(json.dumps({"set_page_id": target_pid}))
            # drain ack
            ack = await self._ws.receive(timeout=5.0)
            json.loads(ack.data)
            self.page_id = target_pid
        fut: asyncio.Future = asyncio.get_event_loop().create_future()
        self._pending[target_pid] = fut
        await self._ws.send_str(json.dumps({
            "content": content,
            "page_id": target_pid,
        }))
        try:
            return await asyncio.wait_for(fut, timeout=timeout)
        except asyncio.TimeoutError:
            self._pending.pop(target_pid, None)
            raise

    async def wait_for_push(self, timeout: float = 10.0) -> Optional[dict]:
        """Pop the next push event from the queue, or None on timeout."""
        try:
            return await asyncio.wait_for(self._pushes.get(), timeout=timeout)
        except asyncio.TimeoutError:
            return None

    def pending_push_count(self) -> int:
        """Return how many pushes are currently queued (for tests)."""
        return self._pushes.qsize()

    def all_received(self) -> list[dict]:
        """Return a snapshot of all messages received since connect()."""
        return list(self._all_received)

    async def drain_pushes(self, timeout: float = 0.5) -> list[dict]:
        """Drain everything currently in the push queue."""
        out: list[dict] = []
        try:
            while True:
                out.append(await asyncio.wait_for(self._pushes.get(), timeout=timeout))
        except asyncio.TimeoutError:
            pass
        return out


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    app = build_app()
    logger.info("Starting WebSocket adapter on %s:%d (ws path: /ws)...",
                WEBSOCKET_HOST, WEBSOCKET_PORT)
    web.run_app(app, host=WEBSOCKET_HOST, port=WEBSOCKET_PORT)


if __name__ == "__main__":
    main()
