"""
Web Control UI Adapter for Hydragent
===================================
Serves the HTML/CSS/JS dashboard and hosts the WebSocket bridge to the Rust core.
"""
import asyncio
import json
import logging
import os
import sys
import uuid
import time
from aiohttp import WSMsgType, web
from dotenv import load_dotenv

# Set up logging
logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO
)
logger = logging.getLogger("web_adapter")

load_dotenv()
BUS_PORT = int(os.getenv("BUS_PORT", 5000))
WEB_PORT = int(os.getenv("GATEWAY_PORT", 18789))
WEB_HOST = os.getenv("GATEWAY_BIND", "127.0.0.1")
CHANNEL_ID = "web"

_clients = set()
_clients_lock = asyncio.Lock()

class BusConnection:
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
        await self.reader.readline()

    async def close(self) -> None:
        try:
            self.writer.close(); await self.writer.wait_closed()
        except Exception: pass

async def _send_intent_and_stream(ws, page_id, user_id, content):
    bus = None
    try:
        bus = await BusConnection.connect()
    except Exception as e:
        await ws.send_str(json.dumps({"type": "error", "message": f"core offline: {e}"}))
        return

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
            "metadata": {"transport": "web"},
            "timestamp": int(time.time() * 1000),
        },
        "id": req_id,
    }

    try:
        bus.writer.write((json.dumps(req) + "\n").encode())
        await bus.writer.drain()

        while True:
            line = await bus.reader.readline()
            if not line: break
            try:
                msg = json.loads(line.decode().strip())
            except Exception: continue
            
            method = msg.get("method")
            if method == "response.token":
                await ws.send_str(json.dumps({
                    "type": "token",
                    "token": msg["params"]["token"],
                }))
            elif method == "response.status":
                await ws.send_str(json.dumps({
                    "type": "status",
                    "status": msg["params"]["status"],
                }))
            elif method == "response.complete":
                await ws.send_str(json.dumps({"type": "complete"}))
                break
    except Exception as e:
        logger.error("Bus stream error: %s", e)
    finally:
        if bus: await bus.close()

async def _ws_handler(request: web.Request) -> web.WebSocketResponse:
    ws = web.WebSocketResponse(heartbeat=30.0)
    await ws.prepare(request)
    conn_id = uuid.uuid4().hex[:8]
    page_id = f"web-{conn_id}"
    user_id = f"web-user-{conn_id}"

    async with _clients_lock:
        _clients.add(ws)

    try:
        async for raw in ws:
            if raw.type == WSMsgType.TEXT:
                try:
                    payload = json.loads(raw.data)
                except Exception: continue
                
                content = payload.get("params", {}).get("content", "").strip()
                if content:
                    await _send_intent_and_stream(ws, page_id, user_id, content)
    finally:
        async with _clients_lock:
            _clients.discard(ws)
    return ws

async def _index_handler(request: web.Request) -> web.Response:
    current_dir = os.path.dirname(os.path.abspath(__file__))
    index_path = os.path.join(current_dir, "index.html")
    return web.FileResponse(index_path)

async def _graph_handler(request: web.Request) -> web.Response:
    graph_path = "C:\\Users\\DELL-L5420\\.hydragent\\data\\graph.html"
    if os.path.exists(graph_path):
        return web.FileResponse(graph_path)
    return web.Response(text="Graph not found", status=404)

async def _graph_css_handler(request: web.Request) -> web.Response:
    path = "C:\\Users\\DELL-L5420\\.hydragent\\data\\library_graph.css"
    if os.path.exists(path):
        return web.FileResponse(path)
    return web.Response(text="CSS not found", status=404)

async def _graph_js_handler(request: web.Request) -> web.Response:
    path = "C:\\Users\\DELL-L5420\\.hydragent\\data\\library_graph.js"
    if os.path.exists(path):
        return web.FileResponse(path)
    return web.Response(text="JS not found", status=404)

def main() -> None:
    app = web.Application()
    app.router.add_get("/ws", _ws_handler)
    app.router.add_get("/", _index_handler)
    app.router.add_get("/graph.html", _graph_handler)
    app.router.add_get("/library_graph.css", _graph_css_handler)
    app.router.add_get("/library_graph.js", _graph_js_handler)
    
    logger.info("Starting Web Control UI on http://%s:%d ...", WEB_HOST, WEB_PORT)
    web.run_app(app, host=WEB_HOST, port=WEB_PORT)

if __name__ == "__main__":
    main()

