import os
import sys
import asyncio
import json
import uuid
import time
import logging
from aiohttp import web
from dotenv import load_dotenv

# Set up logging
logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO
)
logger = logging.getLogger("webhook_adapter")

load_dotenv()
BUS_PORT = int(os.getenv("BUS_PORT", 5000))
WEBHOOK_PORT = int(os.getenv("WEBHOOK_PORT", 8080))

async def send_intent_to_bus(content, session_id, user_id):
    reader, writer = await asyncio.open_connection("127.0.0.1", BUS_PORT)
    req = {
        "jsonrpc": "2.0",
        "method": "intent.submit",
        "params": {
            "page_id": session_id,
            "channel_id": "webhook",
            "user_id": user_id,
            "content": content,
            "attachments": [],
            "metadata": {},
            "timestamp": int(time.time() * 1000),
            "priority": "normal"
        },
        "id": str(uuid.uuid4())
    }
    writer.write((json.dumps(req) + "\n").encode())
    await writer.drain()
    
    final_text = ""
    while True:
        line = await reader.readline()
        if not line:
            break
        msg = json.loads(line.decode().strip())
        if msg.get("method") == "response.token":
            final_text += msg["params"]["token"]
        elif "result" in msg:
            # ReAct loop finished
            break
            
    writer.close()
    await writer.wait_closed()
    return final_text

async def handle_webhook(request):
    try:
        payload = await request.json()
    except Exception:
        return web.json_response({"error": "Invalid JSON"}, status=400)
        
    content = payload.get("content", "").strip()
    # Accept both `page_id` (canonical) and `session_id` (legacy alias) on input.
    session_id = payload.get("page_id") or payload.get("session_id", "default-webhook-session")
    user_id = payload.get("user_id", "default-webhook-user")

    if not content:
        return web.json_response({"error": "content is required"}, status=400)

    logger.info(f"Received webhook event for session {session_id}")
    try:
        reply = await send_intent_to_bus(content, session_id, user_id)
        return web.json_response({"response": reply, "page_id": session_id})
    except Exception as e:
        logger.error(f"Error communicating with Event Bus: {e}")
        return web.json_response({"error": f"Core engine offline or error: {e}"}, status=503)

class BusConnection:
    def __init__(self, reader, writer):
        self.reader = reader
        self.writer = writer

    @classmethod
    async def connect(cls):
        reader, writer = await asyncio.open_connection("127.0.0.1", BUS_PORT)
        return cls(reader, writer)

    async def register(self):
        req = {
            "jsonrpc": "2.0",
            "method": "gateway.register",
            "params": {"channel_id": "webhook"},
            "id": "reg-" + str(uuid.uuid4())[:8]
        }
        self.writer.write((json.dumps(req) + "\n").encode())
        await self.writer.drain()
        line = await self.reader.readline()
        logger.info(f"Registered on Event Bus: {line.decode().strip()}")

    async def close(self):
        try:
            self.writer.close()
            await self.writer.wait_closed()
        except Exception:
            pass

async def listen_for_pushes():
    """Long-lived connection that listens for push events from the Event Bus and routes them to Webhook."""
    while True:
        try:
            logger.info("Opening long-lived Event Bus connection for push notifications...")
            bus = await BusConnection.connect()
            await bus.register()
            
            while True:
                line = await bus.reader.readline()
                if not line:
                    logger.warning("Event Bus push connection lost.")
                    break
                
                try:
                    msg = json.loads(line.decode().strip())
                    if msg.get("method") == "gateway.push":
                        push_params = msg.get("params", {})
                        content = push_params.get("content", "")
                        
                        # Extract content if wrapped inside JSON
                        if isinstance(content, str) and content.strip().startswith("{"):
                            try:
                                data = json.loads(content)
                                if isinstance(data, dict):
                                    content = data.get("content") or data.get("message") or content
                            except Exception:
                                pass
                        
                        logger.info(f"Received push notification: {push_params}")
                        
                        # Forward to a mock client URL if configured
                        forward_url = os.getenv("WEBHOOK_FORWARD_URL")
                        if forward_url:
                            import aiohttp
                            async with aiohttp.ClientSession() as session:
                                try:
                                    async with session.post(forward_url, json={"content": content, "params": push_params}) as resp:
                                        logger.info(f"Forwarded push notification to {forward_url}: status {resp.status}")
                                except Exception as fe:
                                    logger.error(f"Failed to forward webhook push: {fe}")
                except Exception as e:
                    logger.error(f"Error parsing push message: {e}")
            
            await bus.close()
        except Exception as e:
            logger.error(f"Error in push listener: {e}")
            await asyncio.sleep(5)

async def start_background_tasks(app):
    app['push_listener'] = asyncio.create_task(listen_for_pushes())

async def cleanup_background_tasks(app):
    app['push_listener'].cancel()
    try:
        await app['push_listener']
    except asyncio.CancelledError:
        pass

app = web.Application()
app.on_startup.append(start_background_tasks)
app.on_cleanup.append(cleanup_background_tasks)
app.router.add_post("/webhook", handle_webhook)

if __name__ == "__main__":
    logger.info(f"Starting Webhook adapter on port {WEBHOOK_PORT}...")
    web.run_app(app, port=WEBHOOK_PORT)
