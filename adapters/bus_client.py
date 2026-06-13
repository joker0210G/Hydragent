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
        """Send an IntentEvent and get back the full AgentResponse content."""
        request = {
            "jsonrpc": "2.0",
            "method": "intent.submit",
            "params": event,
            "id": str(uuid.uuid4()),
        }
        self.writer.write((json.dumps(request) + "\n").encode())
        await self.writer.drain()

        tokens = []
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

            # Handle streamed token/status/permission notifications
            if msg.get("method") == "response.token":
                token = msg["params"]["token"]
                tokens.append(token)
                if token_callback:
                    token_callback(token)
            elif msg.get("method") == "response.status":
                status = msg["params"]["status"]
                if status_callback:
                    status_callback(status)
            elif msg.get("method") == "response.permission_request":
                params = msg["params"]
                if permission_callback:
                    approved = await permission_callback(params)
                    resp = {
                        "jsonrpc": "2.0",
                        "method": "permission.respond",
                        "params": {
                            "request_id": params["request_id"],
                            "approved": approved
                        },
                        "id": str(uuid.uuid4())
                    }
                    self.writer.write((json.dumps(resp) + "\n").encode())
                    await self.writer.drain()
            elif msg.get("method") == "response.complete":
                # Streaming completed. If the orchestrator already sent a
                # final result payload, the `elif "result" in msg` branch
                # above would have returned. If we get here, fall back to
                # whatever tokens we accumulated so the caller doesn't hang.
                if tokens:
                    return "".join(tokens)
                # Otherwise keep reading — the final result may still arrive.
            elif "result" in msg:
                if isinstance(msg["result"], dict) and "content" in msg["result"]:
                    # The final response containing our complete AgentResponse struct
                    return msg["result"]["content"]
            elif "error" in msg:
                raise Exception(f"Bus error: {msg['error']['message']}")

        return "".join(tokens)
