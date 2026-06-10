import os
import sys
import asyncio
import json
import uuid
import time
import logging
import discord
from discord.ext import commands
from dotenv import load_dotenv

logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO
)
logger = logging.getLogger("discord_adapter")

load_dotenv()
BUS_PORT = int(os.getenv("BUS_PORT", 5000))
DISCORD_BOT_TOKEN = os.getenv("DISCORD_BOT_TOKEN", "").strip("\"'")

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
            "params": {"channel_id": "discord"},
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

async def send_intent_to_bus(channel_id, user_id, content, status_msg, client):
    try:
        bus = await BusConnection.connect()
    except Exception as e:
        logger.error(f"Failed to connect to Event Bus: {e}")
        await status_msg.edit(content="❌ Error: Core engine is offline.")
        return

    req = {
        "jsonrpc": "2.0",
        "method": "intent.submit",
        "params": {
            "page_id": f"discord-{channel_id}",
            "channel_id": "discord",
            "user_id": f"discord-{user_id}",
            "content": content,
            "attachments": [],
            "metadata": {},
            "timestamp": int(time.time() * 1000),
            "priority": "normal"
        },
        "id": str(uuid.uuid4())
    }

    text_buffer = ""
    current_status = "Thinking..."
    last_edit_time = 0.0
    last_edited_text = ""

    async def update_msg():
        nonlocal last_edit_time, last_edited_text
        now = time.time()
        new_text = f"⚙️ **Status:** {current_status}\n\n{text_buffer}" if text_buffer else f"⚙️ {current_status}"
        if now - last_edit_time >= 1.2 and new_text != last_edited_text:
            try:
                await status_msg.edit(content=new_text[:1990])
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

        final_text = f"✅ **Hydragent:**\n\n{text_buffer}" if text_buffer else "✅ Done."
        await status_msg.edit(content=final_text[:1990])
    except Exception as e:
        logger.error(f"Error in discord-bus stream: {e}")
    finally:
        await bus.close()

async def listen_for_pushes(bot: commands.Bot):
    """Long-lived connection that listens for push events from the Event Bus and routes them to Discord."""
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
                        
                        # Parse target channel (expecting "discord-channelId" format from page_id)
                        target_id_str = push_params.get("page_id", "")
                        if target_id_str.startswith("discord-"):
                            target_id = target_id_str.replace("discord-", "")
                        else:
                            target_id = channel_id
                        
                        if target_id:
                            try:
                                discord_channel = bot.get_channel(int(target_id))
                                if not discord_channel:
                                    discord_channel = await bot.fetch_channel(int(target_id))
                                if discord_channel:
                                    await discord_channel.send(content)
                            except Exception as e:
                                logger.error(f"Failed to forward push message to channel {target_id}: {e}")
                except Exception as e:
                    logger.error(f"Error parsing push message: {e}")
            
            await bus.close()
        except Exception as e:
            logger.error(f"Error in push listener: {e}")
            await asyncio.sleep(5)

# discord bot setup
intents = discord.Intents.default()
intents.message_content = True
bot = commands.Bot(command_prefix="!", intents=intents)

@bot.event
async def on_ready():
    logger.info(f"Discord Bot is ready. Logged in as {bot.user}")
    # Start push listener task
    bot.loop.create_task(listen_for_pushes(bot))

@bot.event
async def on_message(message):
    if message.author.bot:
        return
    is_dm = isinstance(message.channel, discord.DMChannel)
    is_mention = bot.user in message.mentions
    
    if is_dm or is_mention:
        clean_content = message.content.replace(f"<@{bot.user.id}>", "").strip()
        if not clean_content:
            return
        status_msg = await message.reply("⏳ Thinking...")
        asyncio.create_task(send_intent_to_bus(message.channel.id, message.author.id, clean_content, status_msg, bot))

if __name__ == "__main__":
    if not DISCORD_BOT_TOKEN:
        logger.error("DISCORD_BOT_TOKEN environment variable is missing.")
        sys.exit(1)
    bot.run(DISCORD_BOT_TOKEN)
