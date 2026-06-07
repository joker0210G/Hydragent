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

    async def send_intent(self, event: dict, token_callback=None) -> str:
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
            msg = json.loads(line.decode().strip())
            
            # Handle streamed token notifications
            if msg.get("method") == "response.token":
                token = msg["params"]["token"]
                tokens.append(token)
                if token_callback:
                    token_callback(token)
            elif msg.get("method") == "response.complete":
                # Streaming completed, wait for final response payload
                pass
            elif "result" in msg:
                # The final response containing our complete AgentResponse struct
                return msg["result"]["content"]
            elif "error" in msg:
                raise Exception(f"Bus error: {msg['error']['message']}")
                
        return "".join(tokens)
