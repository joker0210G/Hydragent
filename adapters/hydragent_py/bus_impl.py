# hydragent_py.bus_impl — JSON-RPC bus client.
#
# This is the canonical implementation. The `bus.py` module re-exports
# `BusClient` from here so callers can write:
#
#     from hydragent_py import BusClient
#     from hydragent_py.bus import BusClient          # same thing
#
# The implementation matches `adapters/bus_client.py` byte-for-byte
# for the on-the-wire behaviour, with these improvements:
#   • host/port are constructor arguments (not globals)
#   • graceful `close()` that flushes pending writes
#   • no implicit `load_dotenv()` at import time — the SDK should
#     not require a `.env` file to be present

from __future__ import annotations

import asyncio
import json
import os
import uuid
from typing import Awaitable, Callable, Optional, Union


PermissionCb = Callable[[dict], Union[bool, Awaitable[bool]]]


class BusClient:
    """Persistent JSON-RPC over TCP client to the Hydragent bus.

    One connection per client. Frames are newline-delimited JSON. Every
    JSON-RPC *request* we send has a unique `id`; the server mirrors that
    `id` in its terminal `result` / `error` response. Notifications
    (`response.token`, `response.status`, `response.permission_request`,
    `response.complete`) have no `id` and are interleaved with the final
    response.

    The `send_intent()` method is the workhorse: it sends one
    `intent.submit` request, streams the tokens, status messages, and
    permission requests back to user-supplied callbacks, and returns the
    final accumulated text once the matching-id `result` arrives.
    """

    def __init__(
        self,
        host: Optional[str] = None,
        port: Optional[int] = None,
    ):
        self.host = host or os.getenv("HYDRA_BUS_HOST", "127.0.0.1")
        self.port = int(port or os.getenv("HYDRA_BUS_PORT") or os.getenv("BUS_PORT") or "5000")
        self.reader: Optional[asyncio.StreamReader] = None
        self.writer: Optional[asyncio.StreamWriter] = None

    async def connect(self) -> None:
        """Open the TCP connection."""
        self.reader, self.writer = await asyncio.open_connection(self.host, self.port)

    async def close(self) -> None:
        """Close the TCP connection. Safe to call multiple times."""
        if self.writer is None:
            return
        try:
            self.writer.close()
            try:
                await self.writer.wait_closed()
            except Exception:
                pass
        except Exception:
            pass
        finally:
            self.reader = None
            self.writer = None

    async def send_intent(
        self,
        event: dict,
        token_callback: Optional[Callable[[str], None]] = None,
        status_callback: Optional[Callable[[str], None]] = None,
        permission_callback: Optional[PermissionCb] = None,
    ) -> str:
        """Send an IntentEvent and return the final assistant reply.

        Bug history: the previous version returned on the FIRST `result` or
        on `response.complete` regardless of `id`. If a prior turn's `result`
        frame was still sitting in the TCP read buffer when a new turn
        started, the new turn consumed the old `result` and replayed the
        previous Hydra response verbatim. Fix: track the current request's
        `id` and only return when a `result` / `error` with the matching
        `id` is seen. `response.complete` is now treated as just another
        notification.
        """
        if self.writer is None or self.reader is None:
            raise RuntimeError("BusClient: not connected — call connect() first")

        request_id = str(uuid.uuid4())
        request = {
            "jsonrpc": "2.0",
            "method": "intent.submit",
            "params": event,
            "id": request_id,
        }
        self.writer.write((json.dumps(request) + "\n").encode())
        await self.writer.drain()

        tokens: list[str] = []
        frame_count = 0
        while True:
            line = await self.reader.readline()
            frame_count += 1
            if not line:
                break
            raw = line.decode().strip()
            if not raw:
                continue
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                # Unknown / unparseable frame — ignore and keep listening.
                continue

            msg_id = msg.get("id")
            method = msg.get("method")

            # ── Notifications (no `id`) ─────────────────────────────
            if method == "response.token":
                token = msg["params"]["token"]
                tokens.append(token)
                if token_callback:
                    token_callback(token)
            elif method == "response.status":
                status = msg["params"]["status"]
                if status_callback:
                    status_callback(status)
            elif method == "response.permission_request":
                params = msg["params"]
                if permission_callback:
                    approved = await permission_callback(params)
                    resp = {
                        "jsonrpc": "2.0",
                        "method": "permission.respond",
                        "params": {
                            "request_id": params["request_id"],
                            "approved": approved,
                        },
                        "id": str(uuid.uuid4()),
                    }
                    self.writer.write((json.dumps(resp) + "\n").encode())
                    await self.writer.drain()
            elif method == "response.complete":
                # End-of-stream signal from the orchestrator. Do NOT
                # return here — the canonical JSON-RPC `result` for
                # this turn is still on its way.
                pass

            # ── Terminal response (MUST match our request id) ──────
            elif msg_id == request_id and "result" in msg:
                result = msg["result"]
                if isinstance(result, dict) and "content" in result:
                    return result["content"]
                return "".join(tokens)
            elif msg_id == request_id and "error" in msg:
                raise RuntimeError(f"Bus error: {msg['error']['message']}")

            # else: result/error for a DIFFERENT request id, or an
            # unknown notification — ignore and keep reading.

        # EOF on the socket — return whatever tokens we accumulated.
        return "".join(tokens)
