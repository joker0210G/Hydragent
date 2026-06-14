import asyncio
import json
import os
import uuid
from dotenv import load_dotenv

# Load environment configuration from .env file
load_dotenv()

BUS_PORT = int(os.getenv("BUS_PORT", 5000))

class BusClient:
    def __init__(self):
        self.reader = None
        self.writer = None

    async def connect(self):
        """Establish connection to the Rust Event Bus TCP server."""
        self.reader, self.writer = await asyncio.open_connection("127.0.0.1", BUS_PORT)

    async def send_intent(self, event: dict, token_callback=None, status_callback=None, permission_callback=None) -> str:
        """Send an IntentEvent and get back the full AgentResponse content.

        The bus uses a single persistent TCP connection per client, with
        newline-delimited JSON frames. Every JSON-RPC *request* we send has
        a unique `id` (UUIDv4), and the server mirrors that `id` in its
        final `result` / `error` response. Notifications (`response.token`,
        `response.status`, `response.permission_request`, `response.complete`)
        have no `id` and are interleaved with the final response.

        Bug history: the previous version returned on the FIRST `result` or
        on `response.complete` regardless of `id`. If a prior turn's `result`
        frame was still sitting in the TCP read buffer when a new turn
        started, the new turn consumed the old `result` and replayed the
        previous Hydra response verbatim. Fix: track the current request's
        `id` and only return when a `result` / `error` with the matching
        `id` is seen. `response.complete` is now treated as just another
        notification.
        """
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
        while True:
            line = await self.reader.readline()
            if not line:
                break
            raw = line.decode().strip()
            # Defensive: the gateway may emit a bare LLM token that contains
            # a leading newline (so the line is empty after `.strip()`) or
            # an SSE keep-alive frame that isn't valid JSON. Skip those —
            # they're never the JSON-RPC response we care about.
            if not raw:
                continue
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                # Unknown / unparseable frame — ignore and keep listening
                # for the next one. The real JsonRpcResponse will arrive
                # when the orchestrator finishes.
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
                # this turn is still on its way (and the matching-id
                # branch below will pick it up). Kept as a notification
                # for future use; intentionally a no-op now.
                pass

            # ── Terminal response (MUST match our request id) ──────
            elif msg_id == request_id and "result" in msg:
                result = msg["result"]
                if isinstance(result, dict) and "content" in result:
                    return result["content"]
                # Result is present but has no `content` field.
                # Return the streamed tokens as a best-effort fallback.
                return "".join(tokens)
            elif msg_id == request_id and "error" in msg:
                raise Exception(f"Bus error: {msg['error']['message']}")

            # else: result/error for a DIFFERENT request id (shouldn't
            # happen on a single-connection client, but skip safely) or
            # an unknown notification — ignore and keep reading.

        # EOF on the socket — return whatever tokens we accumulated.
        return "".join(tokens)
