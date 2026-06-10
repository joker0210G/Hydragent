import os
import sys
import asyncio
import json
import uuid
import time
import logging
from dotenv import load_dotenv
from slack_sdk.web.async_client import AsyncWebClient
from slack_sdk.socket_mode.aiohttp import SocketModeClient
from slack_sdk.socket_mode.request import SocketModeRequest
from slack_sdk.socket_mode.response import SocketModeResponse

logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO
)
logger = logging.getLogger("slack_adapter")

load_dotenv()
BUS_PORT = int(os.getenv("BUS_PORT", 5000))
SLACK_BOT_TOKEN = os.getenv("SLACK_BOT_TOKEN", "").strip("\"'")
SLACK_APP_TOKEN = os.getenv("SLACK_APP_TOKEN", "").strip("\"'")

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
            "params": {"channel_id": "slack"},
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

async def send_intent_to_bus(channel_id, user_id, content, client, ts):
    try:
        bus = await BusConnection.connect()
    except Exception as e:
        logger.error(f"Failed to connect to Event Bus: {e}")
        await client.chat_postMessage(channel=channel_id, thread_ts=ts, text="❌ Error: Core engine is offline.")
        return

    req = {
        "jsonrpc": "2.0",
        "method": "intent.submit",
        "params": {
            "page_id": f"slack-{channel_id}",
            "channel_id": "slack",
            "user_id": f"slack-{user_id}",
            "content": content,
            "attachments": [],
            "metadata": {},
            "timestamp": int(time.time() * 1000),
            "priority": "normal"
        },
        "id": str(uuid.uuid4())
    }

    placeholder = await client.chat_postMessage(channel=channel_id, thread_ts=ts, text="⏳ Thinking...")
    reply_ts = placeholder["ts"]

    text_buffer = ""
    current_status = "Thinking..."
    last_edit_time = 0.0
    last_edited_text = ""

    async def update_msg():
        nonlocal last_edit_time, last_edited_text
        now = time.time()
        new_text = f"⚙️ *Status:* {current_status}\n\n{text_buffer}" if text_buffer else f"⚙️ {current_status}"
        if now - last_edit_time >= 1.2 and new_text != last_edited_text:
            try:
                await client.chat_update(channel=channel_id, ts=reply_ts, text=new_text)
                last_edited_text = new_text
                last_edit_time = now
            except Exception:
                pass

    try:
        bus.writer.write((json.dumps(req) + "\n").encode())
        await bus.writer.drain()

        while True:
            line = await bus.reader.readline()
            if not line:
                break
            msg = json.loads(line.decode().strip())
            if msg.get("method") == "response.token":
                text_buffer += msg["params"]["token"]
                await update_msg()
            elif msg.get("method") == "response.status":
                current_status = msg["params"]["status"].strip()
                await update_msg()
            elif "result" in msg:
                break

        final_text = f"✅ *Hydragent:*\n\n{text_buffer}" if text_buffer else "✅ Done."
        await client.chat_update(channel=channel_id, ts=reply_ts, text=final_text)
    except Exception as e:
        logger.error(f"Error in slack-bus stream: {e}")
    finally:
        await bus.close()

async def process_event(client: SocketModeClient, req: SocketModeRequest):
    if req.type != "events_api":
        return

    response = SocketModeResponse(envelope_id=req.envelope_id)
    await client.send_socket_mode_response(response)

    event = req.payload.get("event", {})
    event_type = event.get("type")

    if event_type in ("app_mention", "message") and not event.get("bot_id"):
        channel = event["channel"]
        user = event.get("user")
        text = event.get("text", "")
        ts = event.get("thread_ts") or event.get("ts")
        
        is_dm = event.get("channel_type") == "im" or channel.startswith("D")
        is_mention = event_type == "app_mention"

        if is_dm or is_mention:
            clean_text = text.replace(f"<@{client.web_client}>", "").strip()
            asyncio.create_task(send_intent_to_bus(channel, user, clean_text, client.web_client, ts))

async def listen_for_pushes(web_client: AsyncWebClient):
    """Long-lived connection that listens for push events from the Event Bus and routes them to Slack."""
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
                        channel_id = push_params.get("channel_id")
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
                        
                        # Parse target channel (expecting "slack-channelId" format from page_id)
                        target_id_str = push_params.get("page_id", "")
                        if target_id_str.startswith("slack-"):
                            target_id = target_id_str.replace("slack-", "")
                        else:
                            target_id = channel_id
                        
                        if target_id:
                            try:
                                await web_client.chat_postMessage(channel=target_id, text=content)
                            except Exception as e:
                                logger.error(f"Failed to forward push message to channel {target_id}: {e}")
                except Exception as e:
                    logger.error(f"Error parsing push message: {e}")
            
            await bus.close()
        except Exception as e:
            logger.error(f"Error in push listener: {e}")
            await asyncio.sleep(5)

async def main():
    if not SLACK_BOT_TOKEN or not SLACK_APP_TOKEN:
        logger.error("SLACK_BOT_TOKEN or SLACK_APP_TOKEN is missing.")
        sys.exit(1)

    web_client = AsyncWebClient(token=SLACK_BOT_TOKEN)
    socket_client = SocketModeClient(app_token=SLACK_APP_TOKEN, web_client=web_client)
    socket_client.socket_mode_request_listeners.append(process_event)
    
    logger.info("Connecting to Slack via Socket Mode...")
    await socket_client.connect()
    
    # Start push listener in background
    asyncio.create_task(listen_for_pushes(web_client))
    
    await asyncio.Event().wait()

if __name__ == "__main__":
    asyncio.run(main())
