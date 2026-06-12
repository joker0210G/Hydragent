import os
import sys
import asyncio
import json
import uuid
import time
import logging
import sqlite3
from telegram import Update, InlineKeyboardButton, InlineKeyboardMarkup, BotCommand, WebAppInfo, MenuButtonWebApp
from telegram.ext import Application, CommandHandler, MessageHandler, CallbackQueryHandler, filters, ContextTypes
from aiohttp import web
import urllib.parse

try:
    from pyngrok import ngrok
except ImportError:
    ngrok = None

try:
    from generate_library_graph import generate_graph
except ImportError:
    generate_graph = lambda: None

from dotenv import load_dotenv

# Set up logging
logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO
)
logger = logging.getLogger("telegram_adapter")

# Load environment configuration from .env file
load_dotenv()

BUS_PORT = int(os.getenv("BUS_PORT", 5000))
TELEGRAM_WEBAPP_URL = os.getenv("TELEGRAM_WEBAPP_URL", "").strip("\"'")
NGROK_AUTH_TOKEN = os.getenv("NGROK_AUTH_TOKEN", "").strip("\"'")
MINIAPP_PORT = int(os.getenv("MINIAPP_PORT", 5001))

# Global variables for config and active permission futures
allowed_chats = set()
bot_token = ""
pending_permissions = {}
background_tasks = set()
active_websocket_sessions = set()
telegram_app = None


ACTIVE_PAGES_FILE = "data/telegram_active_pages.json"
DB_PATH = "data/sessions.db"

def escape_markdown(text: str) -> str:
    """Escapes special characters for Telegram legacy Markdown parse mode."""
    if not isinstance(text, str):
        text = str(text)
    # Characters to escape: \, *, _, `, [
    return text.replace("\\", "\\\\").replace("*", "\\*").replace("_", "\\_").replace("`", "\\`").replace("[", "\\[")

class PageManager:
    def __init__(self):
        self.active_page = {}  # chat_id -> active_page_id
        # Ensure user_insights table exists
        self._query_db(
            "CREATE TABLE IF NOT EXISTS user_insights ("
            "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
            "  page_id TEXT NOT NULL,"
            "  insight TEXT NOT NULL,"
            "  timestamp INTEGER NOT NULL"
            ")"
        )
        self.load_active_pages()
        self.migrate_old_pages()

    def load_active_pages(self):
        if os.path.exists(ACTIVE_PAGES_FILE):
            try:
                with open(ACTIVE_PAGES_FILE, "r") as f:
                    self.active_page = json.load(f)
            except Exception as e:
                logger.error(f"Failed to load active pages file: {e}")

    def save_active_pages(self):
        os.makedirs(os.path.dirname(ACTIVE_PAGES_FILE), exist_ok=True)
        try:
            with open(ACTIVE_PAGES_FILE, "w") as f:
                json.dump(self.active_page, f, indent=2)
        except Exception as e:
            logger.error(f"Failed to save active pages file: {e}")

    def _query_db(self, query, params=(), fetch=False):
        os.makedirs(os.path.dirname(DB_PATH), exist_ok=True)
        conn = sqlite3.connect(DB_PATH)
        try:
            conn.execute("PRAGMA foreign_keys = ON")
            cursor = conn.cursor()
            cursor.execute(query, params)
            if fetch:
                return cursor.fetchall()
            conn.commit()
        except Exception as e:
            logger.error(f"PageManager DB error: {e}")
            if fetch:
                return []
        finally:
            conn.close()
        return None

    def migrate_old_pages(self):
        old_rooms_file = "data/telegram_rooms.json"
        if os.path.exists(old_rooms_file):
            try:
                with open(old_rooms_file, "r") as f:
                    data = json.load(f)
                    rooms = data.get("rooms", {})
                    active = data.get("active_room", {})
                    
                for chat_id_str, room_list in rooms.items():
                    for r in room_list:
                        room_id = r["id"]
                        title = r["title"]
                        existing = self._query_db("SELECT node_id FROM nodes WHERE node_id = ?", (room_id,), fetch=True)
                        if not existing:
                            self._query_db(
                                "INSERT INTO nodes (node_id, type, label, properties) VALUES (?, 'page', ?, ?)",
                                (room_id, title, json.dumps({"chat_id": chat_id_str, "created_at": int(time.time() * 1000)}))
                            )
                
                for chat_id_str, active_id in active.items():
                    if chat_id_str not in self.active_page:
                        self.active_page[chat_id_str] = active_id
                self.save_active_pages()
                
                try:
                    os.rename(old_rooms_file, "data/telegram_rooms_migrated.json")
                    logger.info("Successfully migrated old rooms from telegram_rooms.json to SQLite nodes table.")
                except Exception as file_err:
                    logger.warning(f"Could not rename old rooms file: {file_err}")
            except Exception as e:
                logger.error(f"Error migrating old rooms: {e}")

    def get_pages(self, chat_id):
        chat_id_str = str(chat_id)
        rows = self._query_db("SELECT node_id, label, properties FROM nodes WHERE type = 'page'", fetch=True)
        
        pages = []
        for node_id, label, props_str in rows:
            chat_match = True
            if props_str:
                try:
                    props = json.loads(props_str)
                    if "chat_id" in props and str(props["chat_id"]) != chat_id_str:
                        chat_match = False
                except Exception:
                    pass
            if chat_match:
                pages.append({"id": node_id, "title": label, "label": label})
                
        if not pages:
            page_uuid = f"telegram-room-{uuid.uuid4()}"
            self._query_db(
                "INSERT INTO nodes (node_id, type, label, properties) VALUES (?, 'page', ?, ?)",
                (page_uuid, "General Chat", json.dumps({"chat_id": chat_id_str, "created_at": int(time.time() * 1000)}))
            )
            pages = [{"id": page_uuid, "title": "General Chat", "label": "General Chat"}]
            self.active_page[chat_id_str] = page_uuid
            self.save_active_pages()
            try:
                generate_graph()
            except Exception:
                pass
                
        if chat_id_str not in self.active_page or not any(p["id"] == self.active_page[chat_id_str] for p in pages):
            self.active_page[chat_id_str] = pages[0]["id"]
            self.save_active_pages()
            
        return pages

    def create_page(self, chat_id, title="New Chat"):
        chat_id_str = str(chat_id)
        page_uuid = f"telegram-room-{uuid.uuid4()}"
        self._query_db(
            "INSERT INTO nodes (node_id, type, label, properties) VALUES (?, 'page', ?, ?)",
            (page_uuid, title, json.dumps({"chat_id": chat_id_str, "created_at": int(time.time() * 1000)}))
        )
        self.active_page[chat_id_str] = page_uuid
        self.save_active_pages()
        try:
            generate_graph()
        except Exception as e:
            logger.error(f"Failed to generate graph: {e}")
        return page_uuid

    def get_active_page_id(self, chat_id):
        chat_id_str = str(chat_id)
        self.get_pages(chat_id)
        return self.active_page[chat_id_str]

    def get_active_page_title(self, chat_id):
        pages = self.get_pages(chat_id)
        active_id = self.get_active_page_id(chat_id)
        for p in pages:
            if p["id"] == active_id:
                return p["title"]
        return "Unknown Page"

    def set_active_page(self, chat_id, page_uuid):
        chat_id_str = str(chat_id)
        self.active_page[chat_id_str] = page_uuid
        self.save_active_pages()

    def rename_page(self, chat_id, page_uuid, new_title):
        self._query_db(
            "UPDATE nodes SET label = ? WHERE node_id = ?",
            (new_title, page_uuid)
        )
        try:
            generate_graph()
        except Exception as e:
            logger.error(f"Failed to generate graph: {e}")

    def delete_page(self, chat_id, page_uuid):
        chat_id_str = str(chat_id)
        pages = self.get_pages(chat_id)
        if len(pages) <= 1:
            return False

        self._query_db(
            "DELETE FROM nodes WHERE node_id = ?",
            (page_uuid,)
        )
        # Canonical column name is `page_id` (see crates/hydragent-memory/src/session_store.rs).
        # The `session_meta` table was renamed to `page_meta` when the Pages concept was adopted.
        self._query_db(
            "DELETE FROM messages WHERE page_id = ?",
            (page_uuid,)
        )
        self._query_db(
            "DELETE FROM tool_calls WHERE page_id = ?",
            (page_uuid,)
        )
        self._query_db(
            "DELETE FROM page_meta WHERE page_id = ?",
            (page_uuid,)
        )
        self._query_db(
            "DELETE FROM user_insights WHERE page_id = ?",
            (page_uuid,)
        )
        
        if self.active_page.get(chat_id_str) == page_uuid:
            remaining = [p for p in pages if p["id"] != page_uuid]
            self.active_page[chat_id_str] = remaining[0]["id"]
            self.save_active_pages()
            
        try:
            generate_graph()
        except Exception as e:
            logger.error(f"Failed to generate graph: {e}")
        return True

    # Backwards compatibility properties/methods to prevent crashes
    @property
    def active_room(self):
        return self.active_page
    
    @active_room.setter
    def active_room(self, val):
        self.active_page = val

    def load_active_rooms(self):
        return self.load_active_pages()

    def save_active_rooms(self):
        return self.save_active_pages()

    def migrate_old_rooms(self):
        return self.migrate_old_pages()

    def get_rooms(self, chat_id):
        return self.get_pages(chat_id)

    def create_room(self, chat_id, title="New Chat"):
        return self.create_page(chat_id, title)

    def get_active_room_id(self, chat_id):
        return self.get_active_page_id(chat_id)

    def get_active_room_title(self, chat_id):
        return self.get_active_page_title(chat_id)

    def set_active_room(self, chat_id, room_uuid):
        return self.set_active_page(chat_id, room_uuid)

    def rename_room(self, chat_id, room_uuid, new_title):
        return self.rename_page(chat_id, room_uuid, new_title)

    def delete_room(self, chat_id, room_uuid):
        return self.delete_page(chat_id, room_uuid)

# Instantiate both variables to ensure compatibility
page_manager = RoomManager = RoomManagerAlias = PageManager()
room_manager = page_manager

def broadcast_to_webviews(msg):
    data_str = json.dumps(msg)
    for ws in list(active_websocket_sessions):
        try:
            asyncio.create_task(ws.send_str(data_str))
        except Exception as e:
            logger.debug(f"Failed to broadcast to webview ws: {e}")


def load_config():
    global bot_token, allowed_chats
    bot_token = os.getenv("TELEGRAM_BOT_TOKEN", "").strip("\"'")
    if not bot_token:
        logger.error("TELEGRAM_BOT_TOKEN environment variable is missing or empty")
        sys.exit(1)
        
    allowed_list_str = os.getenv("TELEGRAM_ALLOWED_CHAT_IDS", "").strip("\"'")
    if allowed_list_str:
        try:
            allowed_chats = set(int(cid.strip()) for cid in allowed_list_str.split(",") if cid.strip())
        except ValueError as e:
            logger.error(f"Failed to parse TELEGRAM_ALLOWED_CHAT_IDS '{allowed_list_str}': {e}")
            sys.exit(1)
    else:
        logger.warning("TELEGRAM_ALLOWED_CHAT_IDS is empty. Nobody will be authorized to use the bot!")
    logger.info(f"Loaded config. Whitelisted chats: {allowed_chats}")

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
            "params": {"channel_id": "telegram"},
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

async def send_intent_to_bus(chat_id, user_id, content, sent_msg, context: ContextTypes.DEFAULT_TYPE):
    # Establish transient connection for this request transaction
    try:
        bus = await BusConnection.connect()
    except Exception as e:
        logger.error(f"Failed to connect to Event Bus: {e}")
        try:
            await sent_msg.edit_text("❌ Error: Core engine is offline. Please start the engine first.")
        except Exception:
            pass
        return

    # We will buffer updates to avoid hitting Telegram's rate limit on message editing
    text_buffer = ""
    current_status = "⏳ Thinking..."
    last_edit_time = 0.0
    last_edited_text = ""
    edit_task = None
    stream_complete = False

    async def update_telegram_message():
        nonlocal last_edit_time, edit_task, last_edited_text
        while not stream_complete:
            await asyncio.sleep(0.8)
            now = time.time()
            
            # Format the output beautifully showing active agent thinking steps
            if text_buffer:
                new_text = f"⚙️ *Status:* {current_status}\n\n{text_buffer} ▌"
            else:
                new_text = f"{current_status} ▌"

            if now - last_edit_time >= 1.0 and new_text != last_edited_text:
                try:
                    await context.bot.edit_message_text(
                        chat_id=chat_id,
                        message_id=sent_msg.message_id,
                        text=new_text,
                        parse_mode="Markdown"
                    )
                    last_edited_text = new_text
                    last_edit_time = now
                except Exception as e:
                    # Fallback to plain text if Markdown parsing fails (e.g., mismatched asterisk/markdown symbols from LLM stream)
                    try:
                        plain_text = new_text.replace("*", "").replace("`", "")
                        await context.bot.edit_message_text(
                            chat_id=chat_id,
                            message_id=sent_msg.message_id,
                            text=plain_text
                        )
                        last_edited_text = plain_text
                        last_edit_time = now
                    except Exception:
                        logger.debug(f"Throttled or failed message edit: {e}")

    # Start the periodic editor task
    edit_task = asyncio.create_task(update_telegram_message())

    # Send the intent request
    req = {
        "jsonrpc": "2.0",
        "method": "intent.submit",
        "params": {
            "page_id": room_manager.get_active_room_id(chat_id),
            "channel_id": "telegram",
            "user_id": f"telegram-{user_id}",
            "content": content,
            "attachments": [],
            "metadata": {},
            "timestamp": int(time.time() * 1000),
            "priority": "normal"
        },
        "id": str(uuid.uuid4())
    }

    try:
        bus.writer.write((json.dumps(req) + "\n").encode())
        await bus.writer.drain()

        while True:
            line = await bus.reader.readline()
            if not line:
                break
            msg = json.loads(line.decode().strip())

            # Handle JSON-RPC notifications and result
            if msg.get("method") == "response.token":
                token = msg["params"]["token"]
                text_buffer += token
                broadcast_to_webviews(msg)
            elif msg.get("method") == "response.status":
                # Print status step in log and update status variable
                status = msg["params"]["status"]
                current_status = status.strip()
                logger.info(f"Status update: {current_status}")
                broadcast_to_webviews(msg)
            elif msg.get("method") == "response.permission_request":
                # Prompt user for confirmation
                params = msg["params"]
                req_id = params["request_id"]
                tool_id = params.get("tool_id", "Unknown Tool")
                summary = params.get("params_summary", "No details provided")

                # Setup interactive keyboard
                keyboard = [
                    [
                        InlineKeyboardButton("Approve", callback_data=f"auth_approve:{req_id}"),
                        InlineKeyboardButton("Deny", callback_data=f"auth_deny:{req_id}")
                    ]
                ]
                reply_markup = InlineKeyboardMarkup(keyboard)

                # Send request description to Telegram with markdown escaping and plain-text fallback
                text_content = f"⚠️ *Approval Required*\n\n*Tool:* `{escape_markdown(tool_id)}`\n*Action:* {escape_markdown(summary)}\n\nPlease approve or deny below:"
                try:
                    perm_msg = await context.bot.send_message(
                        chat_id=chat_id,
                        text=text_content,
                        parse_mode="Markdown",
                        reply_markup=reply_markup
                    )
                except Exception as e:
                    logger.warning(f"Markdown send_message failed: {e}. Retrying without Markdown.")
                    plain_text = f"⚠️ Approval Required\n\nTool: {tool_id}\nAction: {summary}\n\nPlease approve or deny below:"
                    perm_msg = await context.bot.send_message(
                        chat_id=chat_id,
                        text=plain_text,
                        reply_markup=reply_markup
                    )

                # Create future to wait for button click
                fut = asyncio.get_running_loop().create_future()
                pending_permissions[req_id] = fut

                try:
                    approved = await fut
                finally:
                    pending_permissions.pop(req_id, None)

                # Update permission prompt text to show action taken
                status_text = "✅ Approved" if approved else "❌ Denied"
                try:
                    await context.bot.edit_message_text(
                        chat_id=chat_id,
                        message_id=perm_msg.message_id,
                        text=f"⚠️ *Approval Required*\n\n*Tool:* `{escape_markdown(tool_id)}`\n*Action:* {escape_markdown(summary)}\n\n*Result:* {status_text}",
                        parse_mode="Markdown"
                    )
                except Exception as e:
                    logger.debug(f"Failed to update permission prompt text with Markdown: {e}")
                    try:
                        await context.bot.edit_message_text(
                            chat_id=chat_id,
                            message_id=perm_msg.message_id,
                            text=f"⚠️ Approval Required\n\nTool: {tool_id}\nAction: {summary}\n\nResult: {status_text}"
                        )
                    except Exception as final_err:
                        logger.error(f"Failed to update permission prompt text completely: {final_err}")

                # Send back the consent response
                resp = {
                    "jsonrpc": "2.0",
                    "method": "permission.respond",
                    "params": {
                        "request_id": req_id,
                        "approved": approved
                    },
                    "id": str(uuid.uuid4())
                }
                bus.writer.write((json.dumps(resp) + "\n").encode())
                await bus.writer.drain()

            elif msg.get("method") == "response.complete":
                # Streaming done
                pass
            elif "result" in msg:
                # final response containing complete AgentResponse content
                if isinstance(msg["result"], dict) and "content" in msg["result"]:
                    text_buffer = msg["result"]["content"]
                break
            elif "error" in msg:
                text_buffer = f"❌ Error: {msg['error']['message']}"
                break
    except Exception as e:
        logger.error(f"Transaction failed: {e}")
        text_buffer = f"❌ Transaction error: {e}"
    finally:
        stream_complete = True
        if edit_task:
            await edit_task
        await bus.close()

        # Update message with the final absolute response (rendered as Markdown)
        try:
            if text_buffer != last_edited_text:
                await context.bot.edit_message_text(
                    chat_id=chat_id,
                    message_id=sent_msg.message_id,
                    text=text_buffer if text_buffer else "(No response content returned)",
                    parse_mode="Markdown"
                )
        except Exception as e:
            logger.debug(f"Final edit Markdown parse failure: {e}. Retrying in plain text.")
            try:
                await context.bot.edit_message_text(
                    chat_id=chat_id,
                    message_id=sent_msg.message_id,
                    text=text_buffer if text_buffer else "(No response content returned)"
                )
            except Exception as final_err:
                logger.error(f"Failed to write final text: {final_err}")

async def handle_message(update: Update, context: ContextTypes.DEFAULT_TYPE):
    msg = update.message if update.message else update.edited_message
    if not msg or not msg.text:
        return

    chat_id = update.effective_chat.id
    user_id = update.effective_user.id
    text = msg.text

    # Access Control Check
    if chat_id not in allowed_chats:
        logger.warning(f"Blocked unauthorized request from chat_id: {chat_id}")
        await msg.reply_text("⛔ Unauthorized. You do not have permission to access this agent.")
        return

    # Check if user is in summary edit or comment mode
    edit_mode = context.user_data.get("summary_edit_mode")
    if edit_mode:
        active_page_id = page_manager.get_active_page_id(chat_id)
        if text.strip().lower() == "/cancel":
            context.user_data.pop("summary_edit_mode", None)
            await msg.reply_text("❌ Summary edit cancelled.")
            return

        if edit_mode == "edit":
            try:
                page_manager._query_db("UPDATE page_meta SET summary = ? WHERE page_id = ?", (text, active_page_id))
                context.user_data.pop("summary_edit_mode", None)
                await msg.reply_text("✅ Page summary updated successfully.")
            except Exception as e:
                await msg.reply_text(f"❌ Failed to update summary: {e}")
            return

        elif edit_mode == "comment":
            try:
                import re
                rows = page_manager._query_db("SELECT summary FROM page_meta WHERE page_id = ?", (active_page_id,), fetch=True)
                current_summary = rows[0][0] if rows and rows[0][0] else ""
                
                # Check line-specific comment format like "3: Actually Linux"
                match_inline = re.match(r'^\s*(\d+)\s*:\s*(.*)$', text)
                if match_inline:
                    target_line = int(match_inline.group(1))
                    comment_content = match_inline.group(2)
                    lines = current_summary.split("\n")
                    updated_lines = []
                    found = False
                    for line in lines:
                        updated_lines.append(line)
                        match = re.match(r'^\s*(\d+)\s*[\.\)]\s*(.*)$', line)
                        if match and int(match.group(1)) == target_line:
                            updated_lines.append(f"   ↳ Note: {comment_content}")
                            found = True
                    if found:
                        new_summary = "\n".join(updated_lines)
                    else:
                        new_summary = current_summary + f"\n\n[Line {target_line} Note]: {comment_content}"
                else:
                    if current_summary:
                        new_summary = current_summary + f"\n\n[User Note]: {text}"
                    else:
                        new_summary = f"[User Note]: {text}"
                
                page_manager._query_db("UPDATE page_meta SET summary = ? WHERE page_id = ?", (new_summary, active_page_id))
                context.user_data.pop("summary_edit_mode", None)
                await msg.reply_text("✅ Comment added to page summary.")
            except Exception as e:
                await msg.reply_text(f"❌ Failed to add comment: {e}")
            return

    # Zero-latency regex parsing of parenthetical remarks
    import re
    remarks = re.findall(r'\(([^)]+)\)', text)
    for remark in remarks:
        remark_clean = remark.strip()
        if remark_clean:
            try:
                page_manager._query_db(
                    "INSERT INTO user_insights (page_id, insight, timestamp) VALUES (?, ?, ?)",
                    (page_manager.get_active_page_id(chat_id), remark_clean, int(time.time() * 1000))
                )
                logger.info(f"Logged parenthetical remark to user_insights: {remark_clean}")
            except Exception as e:
                logger.error(f"Failed to log user insight: {e}")

    # Check if this is a group chat
    chat_type = update.effective_chat.type
    is_group = chat_type in ("group", "supergroup")

    # Resolve bot username to handle tagged messages
    bot_info = await context.bot.get_me()
    bot_username = f"@{bot_info.username}"

    # Handle Group Mentions
    if is_group:
        if bot_username not in text:
            # Bot is not tagged in this group chat message, ignore silently
            return
        # Strip the mention tag from the text prompt
        text = text.replace(bot_username, "").strip()

    is_edit = update.edited_message is not None
    if is_edit:
        logger.info(f"Handling edited message from chat {chat_id}: {text}")
        sent_notice = await msg.reply_text("✏️ *You edited your message. Re-running reasoning...*", parse_mode="Markdown")
    else:
        logger.info(f"Handling message from chat {chat_id}: {text}")
        sent_notice = await msg.reply_text("⏳ Thinking...")

    # Run intention transaction in background
    task = asyncio.create_task(send_intent_to_bus(chat_id, user_id, text, sent_notice, context))
    background_tasks.add(task)
    task.add_done_callback(background_tasks.discard)

async def handle_callback_query(update: Update, context: ContextTypes.DEFAULT_TYPE):
    query = update.callback_query
    await query.answer()
    data = query.data

    if ":" not in data:
        return

    action, val = data.split(":", 1)
    
    if action == "auth_approve" or action == "auth_deny":
        if val in pending_permissions:
            approved = (action == "auth_approve")
            pending_permissions[val].set_result(approved)
            
    elif action == "room_switch":
        chat_id = query.message.chat.id
        room_manager.set_active_room(chat_id, val)
        title = room_manager.get_active_room_title(chat_id)
        await query.edit_message_text(
            f"🔄 *Page Context Switched!*\n\n"
            f"📂 Now talking in: *{title}*\n\n"
            f"Any new messages will load this Page's specific context.",
            parse_mode="Markdown"
        )

    elif action == "summary_action":
        chat_id = query.message.chat.id
        if val == "edit":
            context.user_data["summary_edit_mode"] = "edit"
            await query.message.reply_text("✏️ *Overwriting Page Summary*\n\nPlease type the new summary content below and send it. To abort, type `/cancel`.", parse_mode="Markdown")
        elif val == "comment":
            context.user_data["summary_edit_mode"] = "comment"
            await query.message.reply_text("💬 *Adding Note/Comment to Summary*\n\nTo comment on a specific line, format your message as `<line_number>: <comment>` (e.g. `3: Actually, I am on Linux`). Or, simply write a general comment. Send it below. To abort, type `/cancel`.", parse_mode="Markdown")
        elif val == "compact":
            sent_msg = await query.message.reply_text("🔄 Compacting conversation...")
            res = await rpc_call("page.compact", {"page_id": page_manager.get_active_page_id(chat_id)})
            if res and "result" in res:
                summary = res["result"].get("summary", "")
                await sent_msg.edit_text(f"✅ Page compacted successfully!\n\n*New Summary:*\n{summary}", parse_mode="Markdown")
            else:
                await sent_msg.edit_text("❌ Failed to compact page.")
        
    elif action == "room_menu":
        chat_id = query.message.chat.id
        if val == "create":
            await query.edit_message_text(
                "➕ *Create Page*\n\n"
                "Please type `/newpage <Page Title>` in the chat to create and swap to a new conversation Page.",
                parse_mode="Markdown"
            )
        elif val == "delete":
            rooms = room_manager.get_rooms(chat_id)
            active_id = room_manager.get_active_room_id(chat_id)
            
            if len(rooms) <= 1:
                await query.edit_message_text("❌ Cannot delete the only remaining Page.")
                return
                
            keyboard = []
            for r in rooms:
                is_active = (r["id"] == active_id)
                suffix = " (Active - switches context)" if is_active else ""
                keyboard.append([InlineKeyboardButton(f"❌ Delete: {r['title']}{suffix}", callback_data=f"room_delete:{r['id']}")])
                
            keyboard.append([InlineKeyboardButton("🔙 Back to Library", callback_data="room_menu:back")])
            reply_markup = InlineKeyboardMarkup(keyboard)
            await query.edit_message_text("🗑️ *Select a Page to Delete:*", parse_mode="Markdown", reply_markup=reply_markup)
            
        elif val == "back":
            rooms = room_manager.get_rooms(chat_id)
            active_id = room_manager.get_active_room_id(chat_id)
            
            text = "📄 *Your Active Library Pages*\n\n"
            keyboard = []
            
            for r in rooms:
                is_active = (r["id"] == active_id)
                prefix = "✅ " if is_active else "📄 "
                text += f"{prefix} *{r['title']}*\n`Page ID: {r['id'][-8:]}`\n\n"
                
                if not is_active:
                    keyboard.append([InlineKeyboardButton(f"Switch to: {r['title']}", callback_data=f"room_switch:{r['id']}")])
                    
            keyboard.append([
                InlineKeyboardButton("➕ Create Page", callback_data="room_menu:create"),
                InlineKeyboardButton("🗑️ Delete Page", callback_data="room_menu:delete")
            ])
            
            reply_markup = InlineKeyboardMarkup(keyboard)
            await query.edit_message_text(text, parse_mode="Markdown", reply_markup=reply_markup)

    elif action == "room_delete":
        chat_id = query.message.chat.id
        success = room_manager.delete_room(chat_id, val)
        if success:
            active_title = room_manager.get_active_room_title(chat_id)
            await query.edit_message_text(
                f"🗑️ *Page Deleted!*\n\n"
                f"Your active Page has been adjusted.\n"
                f"📂 Current active Page: *{active_title}*",
                parse_mode="Markdown"
            )
        else:
            await query.edit_message_text("❌ Failed to delete Page.")

async def rpc_call(method, params):
    try:
        reader, writer = await asyncio.open_connection("127.0.0.1", BUS_PORT)
        payload = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": "rpc-" + str(uuid.uuid4())[:8]
        }
        writer.write((json.dumps(payload) + "\n").encode())
        await writer.drain()
        line = await reader.readline()
        writer.close()
        await writer.wait_closed()
        if line:
            return json.loads(line.decode().strip())
    except Exception as e:
        logger.error(f"RPC Call failed: {e}")
    return None

async def start_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    
    reply_markup = None
    if TELEGRAM_WEBAPP_URL:
        keyboard = [[InlineKeyboardButton("Open Dashboard 🐉", web_app=WebAppInfo(url=TELEGRAM_WEBAPP_URL))]]
        reply_markup = InlineKeyboardMarkup(keyboard)

    await update.message.reply_text(
        "🐉 *Welcome to Hydragent!*\n\n"
        "I am your local AI Agent, equipped to perform reasoning steps, web searches, long-term memory operations, and file actions.\n\n"
        "💬 *Commands Available:*\n"
        "• `/library` - List, switch, and delete active Pages\n"
        "• `/newpage <Title>` - Create and switch to a new Page\n"
        "• `/renamepage <New Title>` - Rename the active Page\n"
        "• `/clear` - Reset context (wipes active Page memory)\n\n"
        "Send me a message to begin reasoning!",
        parse_mode="Markdown",
        reply_markup=reply_markup
    )

async def reset_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    active_id = room_manager.get_active_room_id(chat_id)
    room_manager.rename_room(chat_id, active_id, "General Chat")
    new_sess = room_manager.create_room(chat_id, "General Chat")
    await update.message.reply_text(
        f"🔄 *Page Context Reset!*\n\nStarted a fresh Page ID: `{new_sess[-8:]}`",
        parse_mode="Markdown"
    )

async def library_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
        
    rooms = room_manager.get_rooms(chat_id)
    active_id = room_manager.get_active_room_id(chat_id)
    
    text = "📄 *Your Active Library Pages*\n\n"
    keyboard = []
    
    for r in rooms:
        is_active = (r["id"] == active_id)
        prefix = "✅ " if is_active else "📄 "
        text += f"{prefix} *{r['title']}*\n`Page ID: {r['id'][-8:]}`\n\n"
        
        if not is_active:
            keyboard.append([InlineKeyboardButton(f"Switch to: {r['title']}", callback_data=f"room_switch:{r['id']}")])
            
    keyboard.append([
        InlineKeyboardButton("➕ Create Page", callback_data="room_menu:create"),
        InlineKeyboardButton("🗑️ Delete Page", callback_data="room_menu:delete")
    ])
    
    if TELEGRAM_WEBAPP_URL:
        keyboard.append([InlineKeyboardButton("Open Dashboard 🐉", web_app=WebAppInfo(url=TELEGRAM_WEBAPP_URL))])
        
    reply_markup = InlineKeyboardMarkup(keyboard)
    await update.message.reply_text(text, parse_mode="Markdown", reply_markup=reply_markup)

async def new_page_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
        
    title = " ".join(context.args) if context.args else "New Page"
    new_id = room_manager.create_room(chat_id, title)
    
    await update.message.reply_text(
        f"➕ *Page Created successfully!*\n\n"
        f"📄 *Active Page:* `{title}`\n"
        f"🆔 *Page ID:* `{new_id[-8:]}`\n\n"
        f"You are now talking in this Page. History and memory are isolated here.",
        parse_mode="Markdown"
    )

async def rename_page_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
        
    if not context.args:
        await update.message.reply_text("❌ Please specify a new title. Usage: `/renamepage <New Title>`")
        return
        
    new_title = " ".join(context.args)
    active_id = room_manager.get_active_room_id(chat_id)
    room_manager.rename_room(chat_id, active_id, new_title)
    
    await update.message.reply_text(f"✏️ Renamed active Page to: *{new_title}*", parse_mode="Markdown")

async def shelves_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    res = await rpc_call("library.list_nodes", {"type": "shelf"})
    if res and "result" in res:
        nodes = res["result"]
        if not nodes:
            await update.message.reply_text("📚 No shelves found in the library.")
            return
        text = "📚 *Library Shelves:*\n\n"
        for n in nodes:
            text += f"• *{n['label']}*\n`ID: {n['id'][-8:]}`\n\n"
        await update.message.reply_text(text, parse_mode="Markdown")
    else:
        await update.message.reply_text("❌ Failed to query shelves.")

async def summary_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
        
    active_page_id = page_manager.get_active_page_id(chat_id)
    rows = page_manager._query_db("SELECT summary FROM page_meta WHERE page_id = ?", (active_page_id,), fetch=True)
    summary = rows[0][0] if rows and rows[0][0] else ""
    
    if not summary:
        summary = "(No summary yet for this Page. Start chatting or run `/compact` to generate one.)"
        
    keyboard = [
        [
            InlineKeyboardButton("✏️ Edit", callback_data="summary_action:edit"),
            InlineKeyboardButton("💬 Comment", callback_data="summary_action:comment"),
            InlineKeyboardButton("🔄 Compact", callback_data="summary_action:compact")
        ]
    ]
    reply_markup = InlineKeyboardMarkup(keyboard)
    
    await update.message.reply_text(
        f"📄 *Active Page Summary:*\n\n{summary}",
        parse_mode="Markdown",
        reply_markup=reply_markup
    )

async def soul_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    
    file_path = "./config/SOUL.md"
    content = ""
    if os.path.exists(file_path):
        try:
            with open(file_path, "r", encoding="utf-8") as f:
                content = f.read().strip()
        except Exception as e:
            logger.error(f"Failed to read SOUL.md: {e}")
            await update.message.reply_text("❌ Error: Could not read agent SOUL.")
            return

    if not content:
        await update.message.reply_text("🧩 SOUL.md is empty.")
        return

    await update.message.reply_text(
        f"🧩 *Agent Soul & Guidelines (SOUL.md):*\n\n{content}",
        parse_mode="Markdown"
    )

async def add_rule_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return

    if not context.args:
        await update.message.reply_text("❌ Please specify a rule to add. Usage: `/addrule <rule text>`")
        return

    new_rule = " ".join(context.args).strip()
    file_path = "./config/SOUL.md"
    os.makedirs(os.path.dirname(file_path), exist_ok=True)

    try:
        content = ""
        if os.path.exists(file_path):
            with open(file_path, "r", encoding="utf-8") as f:
                content = f.read()
        
        if not content.endswith('\n') and content:
            content += '\n'
        if "# Behavior Rules" not in content:
            content += "\n# Behavior Rules\n"
        content += f"* {new_rule}\n"

        with open(file_path, "w", encoding="utf-8") as f:
            f.write(content)
        
        await update.message.reply_text(f"✅ Behavior rule added to SOUL.md:\n`{new_rule}`")
    except Exception as e:
        logger.error(f"Failed to write SOUL.md: {e}")
        await update.message.reply_text("❌ Failed to add rule.")

async def remove_rule_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return

    if not context.args:
        await update.message.reply_text("❌ Please specify a rule to remove. Usage: `/removerule <rule text>`")
        return

    target_rule = " ".join(context.args).strip()
    file_path = "./config/SOUL.md"

    if not os.path.exists(file_path):
        await update.message.reply_text("❌ SOUL.md not found.")
        return

    try:
        with open(file_path, "r", encoding="utf-8") as f:
            lines = f.readlines()
        
        new_lines = []
        found = False
        for line in lines:
            normalized = line.strip().lstrip('*').lstrip('-').strip()
            if normalized == target_rule and not found:
                found = True
                continue
            new_lines.append(line)

        if found:
            with open(file_path, "w", encoding="utf-8") as f:
                f.writelines(new_lines)
            await update.message.reply_text(f"✅ Behavior rule removed from SOUL.md:\n`{target_rule}`")
        else:
            await update.message.reply_text("❌ Rule not found in SOUL.md.")
    except Exception as e:
        logger.error(f"Failed to update SOUL.md: {e}")
        await update.message.reply_text("❌ Failed to remove rule.")

async def compact_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    
    sent_msg = await update.message.reply_text("🔄 Compacting conversation...")
    res = await rpc_call("page.compact", {"page_id": page_manager.get_active_page_id(chat_id)})
    if res and "result" in res:
        summary = res["result"].get("summary", "")
        await sent_msg.edit_text(f"✅ Page compacted successfully!\n\n*New Summary:*\n{summary}", parse_mode="Markdown")
    else:
        await sent_msg.edit_text("❌ Failed to compact page.")

async def books_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    res = await rpc_call("library.list_nodes", {"type": "book"})
    if res and "result" in res:
        nodes = res["result"]
        if not nodes:
            await update.message.reply_text("📘 No books found in the library.")
            return
        text = "📘 *Library Books:*\n\n"
        for n in nodes:
            text += f"• *{n['label']}*\n`ID: {n['id'][-8:]}`\n\n"
        await update.message.reply_text(text, parse_mode="Markdown")
    else:
        await update.message.reply_text("❌ Failed to query books.")

async def new_shelf_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    if not context.args:
        await update.message.reply_text("❌ Please specify a shelf title. Usage: `/newshelf <Title>`")
        return
    title = " ".join(context.args)
    shelf_id = str(uuid.uuid4())
    res = await rpc_call("library.create_node", {
        "id": shelf_id,
        "type": "shelf",
        "label": title,
        "properties": json.dumps({"created_at": int(time.time() * 1000)})
    })
    if res and "result" in res:
        try:
            generate_graph()
        except Exception as graph_err:
            logger.error(f"Failed to generate graph: {graph_err}")
        await update.message.reply_text(f"📚 *Shelf Created!* \nName: *{title}*\nID: `{shelf_id[-8:]}`", parse_mode="Markdown")
    else:
        await update.message.reply_text("❌ Failed to create shelf.")

async def new_book_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    if not context.args:
        await update.message.reply_text("❌ Please specify a book title. Usage: `/newbook <Title>`")
        return
    title = " ".join(context.args)
    book_id = str(uuid.uuid4())
    res = await rpc_call("library.create_node", {
        "id": book_id,
        "type": "book",
        "label": title,
        "properties": json.dumps({"created_at": int(time.time() * 1000)})
    })
    if res and "result" in res:
        try:
            generate_graph()
        except Exception as graph_err:
            logger.error(f"Failed to generate graph: {graph_err}")
        await update.message.reply_text(f"📘 *Book Created!* \nName: *{title}*\nID: `{book_id[-8:]}`", parse_mode="Markdown")
    else:
        await update.message.reply_text("❌ Failed to create book.")

async def link_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    if len(context.args) < 3:
        await update.message.reply_text("❌ Usage: `/link <source_id_or_label> <relation> <target_id_or_label>`\nExample: `/link Romance sits_on Romedy` or select UUID endings.")
        return
        
    source_query = context.args[0]
    relation = context.args[1]
    target_query = context.args[2]
    
    async def resolve_node(query):
        for nt in ("shelf", "book", "page"):
            r = await rpc_call("library.list_nodes", {"type": nt})
            if r and "result" in r:
                for n in r["result"]:
                    if n["id"].endswith(query) or n["label"].lower() == query.lower():
                        return n["id"], n["label"]
        return None, None
        
    src_id, src_label = await resolve_node(source_query)
    tgt_id, tgt_label = await resolve_node(target_query)
    
    if not src_id:
        await update.message.reply_text(f"❌ Could not resolve source item matching: `{source_query}`")
        return
    if not tgt_id:
        await update.message.reply_text(f"❌ Could not resolve target item matching: `{target_query}`")
        return
        
    edge_id = str(uuid.uuid4())
    res = await rpc_call("library.link", {
        "edge_id": edge_id,
        "source": src_id,
        "relation": relation,
        "target": tgt_id,
        "weight": 1.0
    })
    
    if res and "result" in res:
        try:
            generate_graph()
        except Exception as graph_err:
            logger.error(f"Failed to generate graph: {graph_err}")
        await update.message.reply_text(f"🔗 *Linked Items!* \n*{src_label}* —({relation})—> *{tgt_label}*", parse_mode="Markdown")
    else:
        await update.message.reply_text("❌ Failed to link items.")

async def delete_node_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    chat_id = update.effective_chat.id
    if chat_id not in allowed_chats:
        await update.message.reply_text("⛔ Unauthorized.")
        return
    if not context.args:
        await update.message.reply_text("❌ Please specify ID or label. Usage: `/deletenode <id_or_label>`")
        return
    query = " ".join(context.args)
    
    node_id = None
    node_label = None
    for nt in ("shelf", "book", "page"):
        r = await rpc_call("library.list_nodes", {"type": nt})
        if r and "result" in r:
            for n in r["result"]:
                if n["id"].endswith(query) or n["label"].lower() == query.lower():
                    node_id = n["id"]
                    node_label = n["label"]
                    break
            if node_id:
                break
                
    if not node_id:
        await update.message.reply_text(f"❌ Could not resolve library item matching: `{query}`")
        return
        
    res = await rpc_call("library.delete_node", {"id": node_id})
    if res and "result" in res:
        try:
            generate_graph()
        except Exception as graph_err:
            logger.error(f"Failed to generate graph: {graph_err}")
        await update.message.reply_text(f"🗑️ *Deleted item:* *{node_label}*", parse_mode="Markdown")
    else:
        await update.message.reply_text("❌ Failed to delete item.")

async def listen_for_pushes(app: Application):
    """Long-lived connection that listens for push events from the Event Bus and routes them to Telegram."""
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
                        
                        # Forward pushes to whitelisted chats
                        for chat_id in allowed_chats:
                            try:
                                try:
                                    await app.bot.send_message(
                                        chat_id=chat_id,
                                        text=f"📢 *Notification*\n\n{content}",
                                        parse_mode="Markdown"
                                    )
                                except Exception as markdown_err:
                                    logger.warning(f"Failed to send push as Markdown: {markdown_err}. Retrying as plain text.")
                                    await app.bot.send_message(
                                        chat_id=chat_id,
                                        text=f"📢 Notification\n\n{content}"
                                    )
                            except Exception as e:
                                logger.error(f"Failed to forward push message to chat {chat_id}: {e}")
                except Exception as e:
                    logger.error(f"Error parsing push message: {e}")
            
            await bus.close()
        except Exception as e:
            logger.error(f"Push listener failed: {e}. Retrying in 5 seconds...")
        
        await asyncio.sleep(5)

# Aiohttp web server handlers for Mini App
def no_cache_response(file_path):
    headers = {
        "Cache-Control": "no-store, no-cache, must-revalidate, max-age=0",
        "Pragma": "no-cache",
        "Expires": "0"
    }
    return web.FileResponse(file_path, headers=headers)

async def handle_index(request):
    return no_cache_response(os.path.join(os.path.dirname(__file__), "miniapp", "index.html"))

async def handle_style(request):
    return no_cache_response(os.path.join(os.path.dirname(__file__), "miniapp", "style.css"))

async def handle_app(request):
    return no_cache_response(os.path.join(os.path.dirname(__file__), "miniapp", "app.js"))

async def handle_graph(request):
    return no_cache_response(os.path.join(os.path.dirname(__file__), "miniapp", "graph.html"))

async def websocket_handler(request):
    ws = web.WebSocketResponse()
    await ws.prepare(request)
    
    chat_id = None
    
    # Check initData query parameter
    init_data_str = request.query.get("initData")
    if init_data_str:
        try:
            params = urllib.parse.parse_qs(init_data_str)
            if "user" in params:
                user_data = json.loads(params["user"][0])
                chat_id = user_data.get("id")
            if "chat" in params:
                chat_data = json.loads(params["chat"][0])
                chat_id = chat_data.get("id")
        except Exception as e:
            logger.error(f"Failed to parse initData: {e}")
            
    # Check fallback direct chat_id parameter
    if not chat_id:
        chat_id_param = request.query.get("chat_id")
        if chat_id_param:
            try:
                chat_id = int(chat_id_param)
            except ValueError:
                pass
                
    # Fallback to the first whitelisted chat
    if not chat_id:
        if allowed_chats:
            chat_id = list(allowed_chats)[0]
        else:
            chat_id = "default-chat"
            
    logger.info(f"WebSocket session established for chat_id: {chat_id}")
    active_websocket_sessions.add(ws)
    
    try:
        async for msg in ws:
            if msg.type == web.WSMsgType.TEXT:
                try:
                    data = json.loads(msg.data)
                    method = data.get("method")
                    req_id = data.get("id")
                    params = data.get("params", {})
                    
                    if not method:
                        continue
                        
                    # Handle local page/room commands
                    if method in ("room.list", "page.list"):
                        rooms = room_manager.get_rooms(chat_id)
                        active_room = room_manager.get_active_room_id(chat_id)
                        await ws.send_json({
                            "jsonrpc": "2.0",
                            "result": {
                                "rooms": rooms,
                                "pages": rooms,
                                "active_room": active_room,
                                "active_page": active_room
                            },
                            "id": req_id
                        })
                        
                    elif method in ("room.switch", "page.switch"):
                        room_id = params.get("room_id") or params.get("page_id") or params.get("id")
                        if room_id:
                            room_manager.set_active_room(chat_id, room_id)
                            title = room_manager.get_active_room_title(chat_id)
                            
                            # Send message notification to Telegram chat thread
                            if telegram_app and chat_id:
                                try:
                                    target_chat_id = int(chat_id)
                                    escaped_title = escape_markdown(title)
                                    escaped_room_id = escape_markdown(room_id[-8:])
                                    asyncio.create_task(telegram_app.bot.send_message(
                                        chat_id=target_chat_id,
                                        text=f"🔄 *Page Context Switched!*\n\n"
                                             f"📄 Now talking in: *{escaped_title}*\n"
                                             f"🆔 Page ID: `{escaped_room_id}`\n\n"
                                             f"💬 Send a message below to continue writing on this Page.",
                                        parse_mode="Markdown"
                                    ))
                                except Exception as notify_err:
                                    logger.error(f"Failed to send switch notification to Telegram: {notify_err}")
                            
                            await ws.send_json({
                                "jsonrpc": "2.0",
                                "result": {"status": "switched"},
                                "id": req_id
                            })
                            
                    elif method in ("room.create", "page.create"):
                        title = params.get("title", "New Chat Page")
                        room_manager.create_room(chat_id, title)
                        await ws.send_json({
                            "jsonrpc": "2.0",
                            "result": {"status": "created"},
                            "id": req_id
                        })
                        
                    elif method in ("room.delete", "page.delete"):
                        room_id = params.get("room_id") or params.get("page_id") or params.get("id")
                        if room_id:
                            room_manager.delete_room(chat_id, room_id)
                            await ws.send_json({
                                "jsonrpc": "2.0",
                                "result": {"status": "deleted"},
                                "id": req_id
                            })
                            
                    elif method in ("room.rename", "page.rename"):
                        room_id = params.get("room_id") or params.get("page_id") or params.get("id")
                        title = params.get("title")
                        if room_id and title:
                            room_manager.rename_room(chat_id, room_id, title)
                            await ws.send_json({
                                "jsonrpc": "2.0",
                                "result": {"status": "renamed"},
                                "id": req_id
                            })
                            
                    # Forward memory and library commands to the Rust Event Bus on port 5000
                    elif method in ("memory.list", "memory.delete", "memory.clear") or method.startswith("library.") or method.startswith("page.") or method.startswith("config."):
                        if method == "library.create_node":
                            node_id = params.get("id")
                            node_type = params.get("type")
                            
                            # Read existing properties from SQLite
                            existing = room_manager._query_db(
                                "SELECT properties FROM nodes WHERE node_id = ?",
                                (node_id,),
                                fetch=True
                            )
                            
                            props = {}
                            if existing and existing[0][0]:
                                try:
                                    props = json.loads(existing[0][0])
                                except Exception:
                                    pass
                                    
                            incoming_props_str = params.get("properties")
                            if incoming_props_str:
                                try:
                                    incoming_props = json.loads(incoming_props_str)
                                    props.update(incoming_props)
                                except Exception:
                                    pass
                                    
                            if node_type == "page":
                                props["chat_id"] = str(chat_id)
                                
                            params["properties"] = json.dumps(props)

                        try:
                            reader, writer = await asyncio.open_connection("127.0.0.1", BUS_PORT)
                            payload = {
                                "jsonrpc": "2.0",
                                "method": method,
                                "params": params,
                                "id": req_id
                            }
                            writer.write((json.dumps(payload) + "\n").encode())
                            await writer.drain()
                            
                            line = await reader.readline()
                            writer.close()
                            await writer.wait_closed()
                            
                            if line:
                                resp = json.loads(line.decode().strip())
                                if method.startswith("library.") and "error" not in resp:
                                    try:
                                        generate_graph()
                                    except Exception as graph_err:
                                        logger.error(f"Failed to generate graph: {graph_err}")
                                await ws.send_json(resp)
                            else:
                                raise ConnectionError("Rust Event Bus closed connection abruptly")
                        except Exception as conn_err:
                            logger.warning(f"Event Bus connection failed for {method}: {conn_err}. Falling back to local SQLite execution.")
                            
                            if method == "library.list_nodes":
                                node_type = params.get("type")
                                rows = room_manager._query_db(
                                    "SELECT node_id, type, label, properties FROM nodes WHERE type = ?",
                                    (node_type,),
                                    fetch=True
                                )
                                nodes = []
                                for node_id, t, label, props_str in rows:
                                    props = {}
                                    if props_str:
                                        try:
                                            props = json.loads(props_str)
                                        except Exception:
                                            pass
                                    nodes.append({"id": node_id, "type": t, "label": label, "properties": props})
                                await ws.send_json({
                                    "jsonrpc": "2.0",
                                    "result": nodes,
                                    "id": req_id
                                })
                            elif method == "library.create_node":
                                node_id = params.get("id") or str(uuid.uuid4())
                                node_type = params.get("type")
                                label = params.get("label")
                                props_str = params.get("properties") or "{}"
                                
                                room_manager._query_db(
                                    "INSERT OR REPLACE INTO nodes (node_id, type, label, properties) VALUES (?, ?, ?, ?)",
                                    (node_id, node_type, label, props_str)
                                )
                                try:
                                    generate_graph()
                                except Exception as graph_err:
                                    logger.error(f"Failed to generate graph: {graph_err}")
                                    
                                await ws.send_json({
                                    "jsonrpc": "2.0",
                                    "result": {"status": "created", "id": node_id},
                                    "id": req_id
                                })
                            elif method == "library.delete_node":
                                node_id = params.get("id")
                                room_manager.delete_room(chat_id, node_id)
                                try:
                                    generate_graph()
                                except Exception as graph_err:
                                    logger.error(f"Failed to generate graph: {graph_err}")
                                    
                                await ws.send_json({
                                    "jsonrpc": "2.0",
                                    "result": {"status": "deleted", "id": node_id},
                                    "id": req_id
                                })
                            elif method == "library.link":
                                source = params.get("source")
                                relation = params.get("relation")
                                target = params.get("target")
                                weight = params.get("weight", 1.0)
                                edge_id = str(uuid.uuid4())
                                
                                room_manager._query_db(
                                    "INSERT OR REPLACE INTO edges (edge_id, source_node_id, target_node_id, relation_type, weight) VALUES (?, ?, ?, ?, ?)",
                                    (edge_id, source, target, relation, weight)
                                )
                                try:
                                    generate_graph()
                                except Exception as graph_err:
                                    logger.error(f"Failed to generate graph: {graph_err}")
                                    
                                await ws.send_json({
                                    "jsonrpc": "2.0",
                                    "result": {"status": "linked", "edge_id": edge_id},
                                    "id": req_id
                                })
                            else:
                                await ws.send_json({
                                    "jsonrpc": "2.0",
                                    "error": {"code": -32603, "message": f"Core engine unreachable: {conn_err}"},
                                    "id": req_id
                                })
                            
                except Exception as parse_err:
                    logger.error(f"Error handling WebSocket message: {parse_err}")
                    
    finally:
        active_websocket_sessions.discard(ws)
        logger.info(f"WebSocket session closed for chat_id: {chat_id}")
        
    return ws

async def start_web_server():
    app_server = web.Application()
    app_server.router.add_get("/", handle_index)
    app_server.router.add_get("/index.html", handle_index)
    app_server.router.add_get("/style.css", handle_style)
    app_server.router.add_get("/app.js", handle_app)
    app_server.router.add_get("/graph.html", handle_graph)
    app_server.router.add_get("/ws", websocket_handler)
    
    runner = web.AppRunner(app_server)
    await runner.setup()
    site = web.TCPSite(runner, "0.0.0.0", MINIAPP_PORT)
    await site.start()
    logger.info(f"Serving Mini App at http://localhost:{MINIAPP_PORT}")
    return runner

async def start_ngrok_tunnel():
    global TELEGRAM_WEBAPP_URL
    if TELEGRAM_WEBAPP_URL == "pyngrok":
        if ngrok is None:
            logger.error("pyngrok package is not installed. Please run: pip install pyngrok")
            return
            
        if NGROK_AUTH_TOKEN:
            ngrok.set_auth_token(NGROK_AUTH_TOKEN)
            
        try:
            logger.info(f"Starting pyngrok tunnel on port {MINIAPP_PORT}...")
            loop = asyncio.get_running_loop()
            tunnel = await loop.run_in_executor(None, lambda: ngrok.connect(MINIAPP_PORT))
            TELEGRAM_WEBAPP_URL = tunnel.public_url
            logger.info(f"🚀 pyngrok tunnel established: {TELEGRAM_WEBAPP_URL}")
        except Exception as e:
            logger.error(f"Failed to start pyngrok tunnel: {e}")

async def main_async():
    load_config()
    try:
        generate_graph()
        logger.info("Generated initial library graph map on startup.")
    except Exception as graph_err:
        logger.error(f"Failed to generate startup graph: {graph_err}")
    await start_ngrok_tunnel()
    
    global telegram_app
    logger.info("Starting Telegram adapter...")

    # Build the Application
    app = Application.builder().token(bot_token).build()
    telegram_app = app

    # Register handlers
    app.add_handler(CommandHandler("start", start_cmd))
    app.add_handler(CommandHandler("reset", reset_cmd))
    app.add_handler(CommandHandler("clear", reset_cmd))
    app.add_handler(CommandHandler("library", library_cmd))
    app.add_handler(CommandHandler("newpage", new_page_cmd))
    app.add_handler(CommandHandler("renamepage", rename_page_cmd))
    app.add_handler(CommandHandler("shelves", shelves_cmd))
    app.add_handler(CommandHandler("books", books_cmd))
    app.add_handler(CommandHandler("newshelf", new_shelf_cmd))
    app.add_handler(CommandHandler("newbook", new_book_cmd))
    app.add_handler(CommandHandler("link", link_cmd))
    app.add_handler(CommandHandler("deletenode", delete_node_cmd))
    app.add_handler(CommandHandler("summary", summary_cmd))
    app.add_handler(CommandHandler("soul", soul_cmd))
    app.add_handler(CommandHandler("addrule", add_rule_cmd))
    app.add_handler(CommandHandler("removerule", remove_rule_cmd))
    app.add_handler(CommandHandler("compact", compact_cmd))
    app.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, handle_message))
    app.add_handler(MessageHandler(filters.UpdateType.EDITED_MESSAGE & filters.TEXT, handle_message))
    app.add_handler(CallbackQueryHandler(handle_callback_query))

    # Initialize and start application
    await app.initialize()
    await app.start()

    # Set bot commands in Telegram UI autocomplete list automatically
    await app.bot.set_my_commands([
        BotCommand("start", "Show welcome message and full command list"),
        BotCommand("library", "List, switch, and delete active library Pages"),
        BotCommand("newpage", "Create and switch to a new Page"),
        BotCommand("renamepage", "Rename the active Page"),
        BotCommand("shelves", "List all shelves in the library"),
        BotCommand("books", "List all books in the library"),
        BotCommand("newshelf", "Create a new shelf: /newshelf <name>"),
        BotCommand("newbook", "Create a new book: /newbook <name>"),
        BotCommand("link", "Link nodes: /link <src> <relation> <target>"),
        BotCommand("deletenode", "Delete node: /deletenode <name_or_id>"),
        BotCommand("summary", "View, edit, or comment on active Page summary"),
        BotCommand("soul", "View agent guidelines (SOUL.md)"),
        BotCommand("addrule", "Add behavior rule to SOUL.md: /addrule <rule>"),
        BotCommand("removerule", "Remove behavior rule: /removerule <rule>"),
        BotCommand("compact", "Compact chat history of this Page to summary"),
        BotCommand("clear", "Start a fresh General Chat Page context")
    ])

    if TELEGRAM_WEBAPP_URL:
        try:
            await app.bot.set_chat_menu_button(
                menu_button=MenuButtonWebApp(
                    text="Dashboard 🐉",
                    web_app=WebAppInfo(url=TELEGRAM_WEBAPP_URL)
                )
            )
            logger.info(f"Registered Chat Menu Button WebApp: {TELEGRAM_WEBAPP_URL}")
        except Exception as e:
            logger.error(f"Failed to set Chat Menu Button: {e}")

    # Start the web server serving the Mini App
    web_runner = await start_web_server()
    
    # Start polling updates
    await app.updater.start_polling()
    
    # Spawn background push listener task
    push_task = asyncio.create_task(listen_for_pushes(app))
    
    logger.info("Telegram bot listening. Press Ctrl+C to quit.")
    
    try:
        # Keep the coroutine running until cancelled or interrupted
        await push_task
    except (KeyboardInterrupt, asyncio.CancelledError):
        logger.info("Shutdown signal received.")
    finally:
        # Shutdown cleanly
        push_task.cancel()
        if app.updater.running:
            await app.updater.stop()
        await app.stop()
        await app.shutdown()
        try:
            await web_runner.cleanup()
        except Exception as e:
            logger.debug(f"Failed to cleanup web runner: {e}")

def main():
    try:
        asyncio.run(main_async())
    except KeyboardInterrupt:
        logger.info("Bot stopped by user.")

if __name__ == "__main__":
    main()
