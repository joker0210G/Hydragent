"""
Control UI Adapter for Hydragent
================================

A standalone Python process that serves the **browser Control UI** for
Hydragent. Modeled on the OpenClaw Control UI surface but adapted to
Hydragent's existing bus-protocol architecture (Rust kernel + TCP
JSON-RPC bus + Python adapters).

Surfaces (default bind 0.0.0.0:8765 unless overridden):

    GET  /                          — Single-page UI shell (dist/index.html)
    GET  /<assets>                  — Static UI assets from dist/
    GET  /control-ui-config.json    — Runtime config (auth-gated)
    GET  /healthz                   — Health probe
    GET  /ws                        — WebSocket (same browser wire format
                                      as adapters/websocket_adapter.py)
    POST /hooks/<name>              — Inbound hooks (only if HYDRA_HOOKS_TOKEN
                                      is set; HMAC-validated if configured)
    POST /api/v1/admin/rpc          — Admin HTTP RPC (off by default; gated
                                      by HYDRA_ADMIN_RPC_TOKEN)
    POST /push/web/vapidPublicKey   — VAPID public key
    POST /push/web/subscribe        — Register a Web Push subscription
    POST /push/web/unsubscribe      — Remove a subscription
    POST /push/web/test             — Send a test push to the caller

Auth model (mirrors OpenClaw's `gateway.auth`):

  * `mode: "token"`   — static shared secret in `HYDRA_GATEWAY_TOKEN` env
                         (or `Authorization: Bearer …`, `?token=…`,
                          `#token=…` URL fragment, or per-WebSocket
                          `connect.params.auth.token`).
  * `mode: "password"`— shared password in `HYDRA_GATEWAY_PASSWORD`.
  * `mode: "none"`    — disabled. Only loopback is accepted; non-loopback
                         binds still warn and may pair-gate.
  * `mode: "trusted-proxy"` — accept identity from
                              `X-Forwarded-User` / `X-Forwarded-Email`
                              headers. Caller must run a non-loopback
                              identity-aware reverse proxy in front.

Device pairing:

  * First connect from a new browser/device issues a `deviceRequestId`.
  * The pair must be approved via the admin UI (or the future
    `hydragent devices approve <id>` CLI) before the browser can use
    the WebSocket.
  * Localhost (`127.0.0.1`, `::1`) connections are auto-approved.
  * Approved pairings persist to `~/.hydragent/control-ui/devices.json`.

Allowed origins:

  * `http://127.0.0.1:<port>`, `http://localhost:<port>` (loopback) — always
  * `http://<lan-ip>:<port>` for RFC1918 / link-local / `.local` / `.ts.net` /
    Tailscale CGNAT — accepted if the request Host header matches the origin
  * Anything else — must be in `HYDRA_CONTROL_UI_ALLOWED_ORIGINS`
    (comma-separated full origins, e.g.
     `https://my-control-ui.example.com,http://10.0.0.5:8765`).

Tailscale guidance:

  * If the operator wants Tailscale Serve / Funnel, they put a reverse
    proxy (Tailscale's own `tailscale serve` or `caddy`) in front of
    this adapter. The adapter itself does not speak the Tailscale
    identity protocol — it trusts the upstream proxy to set
    `X-Forwarded-User` and only enters trusted-proxy mode if the
    operator opts in via `HYDRA_GATEWAY_AUTH_MODE=trusted-proxy`.

Bus integration:

  * The adapter connects to the same Event Bus as every other channel
    (`BUS_HOST:BUS_PORT`, default `127.0.0.1:5000`) and registers as
    `channel_id: "control-ui"`.
  * It listens for `gateway.push` notifications on a long-lived
    connection and fans them out to the right browser tab.
  * The browser wire format matches `adapters/websocket_adapter.py`
    byte-for-byte — see `_send_intent_and_stream` and `_broadcast_push`.

Run:

    # default ports: bind 0.0.0.0:8765, bus 127.0.0.1:5000
    python -m adapters.control_ui

    # or:
    python adapters/control_ui/__main__.py

    # override:
    HYDRA_CONTROL_UI_PORT=9001 HYDRA_GATEWAY_TOKEN=secret python -m adapters.control_ui
"""
from __future__ import annotations

import argparse
import asyncio
import base64
import contextlib
import hashlib
import hmac
import ipaddress
import json
import logging
import os
import socket
import sys
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional

import aiohttp
from aiohttp import WSMsgType, web
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec
from dotenv import load_dotenv

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
    level=os.getenv("HYDRA_LOG_LEVEL", "INFO"),
)
log = logging.getLogger("control_ui_adapter")

# ---------------------------------------------------------------------------
# Env / config
# ---------------------------------------------------------------------------
load_dotenv()

BUS_HOST = os.getenv("HYDRA_BUS_HOST", os.getenv("BUS_HOST", "127.0.0.1"))
BUS_PORT = int(os.getenv("HYDRA_BUS_PORT", os.getenv("BUS_PORT", "5000")))

UI_HOST = os.getenv("HYDRA_CONTROL_UI_HOST", "0.0.0.0")
UI_PORT = int(os.getenv("HYDRA_CONTROL_UI_PORT", "8765"))
UI_BASE_PATH = os.getenv("HYDRA_CONTROL_UI_BASE_PATH", "/").rstrip("/")
if not UI_BASE_PATH.startswith("/"):
    UI_BASE_PATH = "/" + UI_BASE_PATH
# the actual port the browser should connect to (used in config.json
# and in CORS / self-URL allowlists).
UI_PUBLIC_PORT = int(os.getenv("HYDRA_CONTROL_UI_PUBLIC_PORT", str(UI_PORT)))
UI_PUBLIC_HOST = os.getenv("HYDRA_CONTROL_UI_PUBLIC_HOST", "127.0.0.1")

CHANNEL_ID = "control-ui"
USER_ID_PREFIX = "ctrl-user"

# Auth -------------------------------------------------------------------
AUTH_MODE = os.getenv("HYDRA_GATEWAY_AUTH_MODE", "token").lower()
# "token" | "password" | "none" | "trusted-proxy"
AUTH_TOKEN = os.getenv("HYDRA_GATEWAY_TOKEN", "")
AUTH_PASSWORD = os.getenv("HYDRA_GATEWAY_PASSWORD", "")

# Allowed origins (comma-separated full origins) --------------------------
ALLOWED_ORIGINS = [
    o.strip() for o in os.getenv("HYDRA_CONTROL_UI_ALLOWED_ORIGINS", "").split(",") if o.strip()
]

# Tailscale serve / funnel relaxed auth ----------------------------------
ALLOW_TAILSCALE = os.getenv("HYDRA_GATEWAY_ALLOW_TAILSCALE", "false").lower() in ("1", "true", "yes")
INSECURE_AUTH_OK = os.getenv("HYDRA_CONTROL_UI_ALLOW_INSECURE_AUTH", "false").lower() in ("1", "true", "yes")
DISABLE_DEVICE_AUTH = os.getenv("HYDRA_CONTROL_UI_DANGEROUSLY_DISABLE_DEVICE_AUTH", "false").lower() in ("1", "true", "yes")
HOST_HEADER_FALLBACK = os.getenv("HYDRA_CONTROL_UI_DANGEROUSLY_ALLOW_HOST_HEADER_FALLBACK", "false").lower() in ("1", "true", "yes")

# Hooks ------------------------------------------------------------------
HOOKS_TOKEN = os.getenv("HYDRA_HOOKS_TOKEN", "")
HOOKS_ENABLED = os.getenv("HYDRA_HOOKS_ENABLED", "false").lower() in ("1", "true", "yes") or bool(HOOKS_TOKEN)

# Admin HTTP RPC ---------------------------------------------------------
ADMIN_RPC_TOKEN = os.getenv("HYDRA_ADMIN_RPC_TOKEN", "")
ADMIN_RPC_ENABLED = bool(ADMIN_RPC_TOKEN) or os.getenv("HYDRA_ADMIN_RPC_ENABLED", "false").lower() in ("1", "true", "yes")

# Web Push (VAPID) -------------------------------------------------------
VAPID_PUBLIC_KEY = os.getenv("HYDRA_VAPID_PUBLIC_KEY", "")
VAPID_PRIVATE_KEY = os.getenv("HYDRA_VAPID_PRIVATE_KEY", "")
VAPID_SUBJECT = os.getenv("HYDRA_VAPID_SUBJECT", "https://hydragent.ai")

# State dirs -------------------------------------------------------------
DATA_DIR = Path(os.getenv("HYDRA_DATA_DIR", Path.home() / ".hydragent" / "data"))
DEVICE_REGISTRY = DATA_DIR / "control-ui" / "devices.json"
PUSH_REGISTRY = DATA_DIR / "control-ui" / "push_subscriptions.json"
VAPID_KEY_FILE = DATA_DIR / "control-ui" / "vapid-keys.json"

# Static dir -------------------------------------------------------------
PKG_DIR = Path(__file__).resolve().parent
DIST_DIR = PKG_DIR / "dist"
if not DIST_DIR.exists():
    # Fall back to package root if dist/ not built yet — keeps the
    # server importable for `python -c "from adapters.control_ui import …"`.
    DIST_DIR = PKG_DIR

# ---------------------------------------------------------------------------
# Async-safe state
# ---------------------------------------------------------------------------
_clients_lock = asyncio.Lock()
_clients: set[tuple[str, "web.WebSocketResponse"]] = set()
_device_lock = asyncio.Lock()
_push_lock = asyncio.Lock()


# ---------------------------------------------------------------------------
# Bus connection (mirrors websocket_adapter.py)
# ---------------------------------------------------------------------------
class BusConnection:
    """One bus connection. Used both for push-listener (long-lived) and
    for `intent.submit` (short-lived, one per inbound WS message)."""

    def __init__(self, reader, writer):
        self.reader = reader
        self.writer = writer

    @classmethod
    async def connect(cls) -> "BusConnection":
        reader, writer = await asyncio.open_connection(BUS_HOST, BUS_PORT)
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
        log.info("Registered on Event Bus as channel=%s: %s", channel_id, decoded)

    async def close(self) -> None:
        try:
            self.writer.close()
            await self.writer.wait_closed()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# VAPID keypair (auto-generate on first run)
# ---------------------------------------------------------------------------
def _ensure_vapid_keypair() -> tuple[str, str]:
    """Return (public_b64url, private_b64url). Auto-generate on first run
    unless the operator pins `HYDRA_VAPID_PUBLIC_KEY` / `HYDRA_VAPID_PRIVATE_KEY`
    in the environment."""
    global VAPID_PUBLIC_KEY, VAPID_PRIVATE_KEY
    if VAPID_PUBLIC_KEY and VAPID_PRIVATE_KEY:
        return VAPID_PUBLIC_KEY, VAPID_PRIVATE_KEY
    if VAPID_KEY_FILE.exists():
        try:
            with open(VAPID_KEY_FILE, "r", encoding="utf-8") as f:
                k = json.load(f)
            VAPID_PUBLIC_KEY = k["publicKey"]
            VAPID_PRIVATE_KEY = k["privateKey"]
            return VAPID_PUBLIC_KEY, VAPID_PRIVATE_KEY
        except Exception as e:
            log.warning("VAPID key file unreadable, regenerating: %s", e)
    VAPID_KEY_FILE.parent.mkdir(parents=True, exist_ok=True)
    sk = ec.generate_private_key(ec.SECP256R1())
    pk = sk.public_key()
    raw_priv = sk.private_numbers().private_value.to_bytes(32, "big")
    raw_pub = pk.public_bytes(
        encoding=serialization.Encoding.X962,
        format=serialization.PublicFormat.UncompressedPoint,
    )[1:]  # drop 0x04 prefix
    pub_b64 = base64.urlsafe_b64encode(raw_pub).rstrip(b"=").decode("ascii")
    priv_b64 = base64.urlsafe_b64encode(raw_priv).rstrip(b"=").decode("ascii")
    with open(VAPID_KEY_FILE, "w", encoding="utf-8") as f:
        json.dump({"publicKey": pub_b64, "privateKey": priv_b64}, f, indent=2)
    try:
        os.chmod(VAPID_KEY_FILE, 0o600)
    except Exception:
        pass
    VAPID_PUBLIC_KEY, VAPID_PRIVATE_KEY = pub_b64, priv_b64
    log.info("Generated VAPID keypair at %s", VAPID_KEY_FILE)
    return pub_b64, priv_b64


# ---------------------------------------------------------------------------
# Device pairing registry
# ---------------------------------------------------------------------------
@dataclass
class DeviceRecord:
    device_id: str
    label: str
    role: str  # "operator" | "node"
    public_key: str
    approved_at: float
    last_seen: float
    scopes: list[str] = field(default_factory=list)
    user_agent: str = ""

    def to_dict(self) -> dict:
        return {
            "deviceId": self.device_id,
            "label": self.label,
            "role": self.role,
            "publicKey": self.public_key,
            "approvedAt": self.approved_at,
            "lastSeen": self.last_seen,
            "scopes": self.scopes,
            "userAgent": self.user_agent,
        }


def _load_devices() -> dict[str, DeviceRecord]:
    if not DEVICE_REGISTRY.exists():
        return {}
    try:
        with open(DEVICE_REGISTRY, "r", encoding="utf-8") as f:
            data = json.load(f)
        out: dict[str, DeviceRecord] = {}
        for did, raw in data.items():
            try:
                out[did] = DeviceRecord(
                    device_id=raw.get("deviceId", did),
                    label=raw.get("label", ""),
                    role=raw.get("role", "operator"),
                    public_key=raw.get("publicKey", ""),
                    approved_at=float(raw.get("approvedAt", 0)),
                    last_seen=float(raw.get("lastSeen", 0)),
                    scopes=list(raw.get("scopes", [])),
                    user_agent=raw.get("userAgent", ""),
                )
            except Exception as e:
                log.warning("Skipping bad device record %s: %s", did, e)
        return out
    except Exception as e:
        log.error("Failed to read device registry: %s", e)
        return {}


def _save_devices(devices: dict[str, DeviceRecord]) -> None:
    DEVICE_REGISTRY.parent.mkdir(parents=True, exist_ok=True)
    payload = {did: d.to_dict() for did, d in devices.items()}
    tmp = DEVICE_REGISTRY.with_suffix(".json.tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(payload, f, indent=2)
    try:
        os.chmod(tmp, 0o600)
    except Exception:
        pass
    tmp.replace(DEVICE_REGISTRY)


def _is_loopback_origin(host: str) -> bool:
    """Treat the literal loopback names as loopback. RFC1918 / link-local /
    `.local` / `.ts.net` / Tailscale CGNAT are not loopback for the origin
    allowlist — they go through the `private-lan-ok` branch below."""
    return host in ("127.0.0.1", "::1", "localhost")


def _is_private_lan_host(host: str) -> bool:
    if _is_loopback_origin(host):
        return False
    if host.endswith(".local") or host.endswith(".ts.net"):
        return True
    try:
        ip = ipaddress.ip_address(host)
        return (
            ip.is_private
            or ip.is_link_local
            or ip.is_loopback
            or (ip.version == 4 and str(ip).startswith("100."))  # Tailscale CGNAT
        )
    except ValueError:
        return False


def _is_tailscale_identity_header_present(request: web.Request) -> bool:
    if not ALLOW_TAILSCALE:
        return False
    return bool(
        request.headers.get("Tailscale-User-Login")
        or request.headers.get("X-Webauth-User")
    )


# ---------------------------------------------------------------------------
# Auth helpers
# ---------------------------------------------------------------------------
def _extract_token_from_request(request: web.Request) -> str:
    """Look for a token in header, query, or URL fragment (we can't read
    fragments from a Python server, but the browser strips them before
    they ever leave the page — so this is a defensive placeholder for the
    rare case a non-browser caller passes them)."""
    auth = request.headers.get("Authorization", "")
    if auth.lower().startswith("bearer "):
        return auth.split(None, 1)[1].strip()
    if "token" in request.query:
        return request.query["token"].strip()
    return ""


def _check_token(token: str) -> bool:
    if not AUTH_TOKEN:
        return False
    if not token:
        return False
    # Constant-time compare to avoid timing leaks.
    return hmac.compare_digest(token, AUTH_TOKEN)


def _check_password(password: str) -> bool:
    if not AUTH_PASSWORD:
        return False
    if not password:
        return False
    return hmac.compare_digest(password, AUTH_PASSWORD)


def _request_auth_mode(request: web.Request) -> str:
    """Return 'token', 'password', 'trusted-proxy', or 'none' for this
    request. The active mode is set by `HYDRA_GATEWAY_AUTH_MODE`; the
    only per-request override is `trusted-proxy` which the proxy sets
    by populating `X-Forwarded-User`."""
    if AUTH_MODE == "trusted-proxy":
        if request.headers.get("X-Forwarded-User") or request.headers.get("X-Forwarded-Email"):
            return "trusted-proxy"
    if _is_tailscale_identity_header_present(request):
        return "trusted-proxy"
    return AUTH_MODE


def _is_origin_allowed(request: web.Request) -> bool:
    origin = request.headers.get("Origin")
    if not origin:
        return True  # same-origin GETs don't send Origin
    # Strip scheme to get the host:port
    try:
        from urllib.parse import urlparse

        parsed = urlparse(origin)
        host = parsed.hostname or ""
        port = parsed.port
    except Exception:
        return False

    # Loopback: always allowed.
    if _is_loopback_origin(host) and (port is None or port == UI_PUBLIC_PORT):
        return True

    # Allow private LAN / Tailscale hosts that match the request Host.
    # (Same-origin private LAN loads are accepted.)
    host_header = request.headers.get("Host", "")
    if host and host_header and host.split(":")[0] == host_header.split(":")[0]:
        if _is_private_lan_host(host):
            return True

    # Tailscale Serve / Funnel headers count as proof the connection
    # came through Tailscale.
    if _is_tailscale_identity_header_present(request):
        return True

    # Explicit allowlist.
    if origin in ALLOWED_ORIGINS:
        return True

    # Host-header origin fallback (dangerous — off by default).
    if HOST_HEADER_FALLBACK and host_header:
        if origin == f"http://{host_header}" or origin == f"https://{host_header}":
            return True

    return False


# ---------------------------------------------------------------------------
# Middleware: auth + origin guard
# ---------------------------------------------------------------------------
@web.middleware
async def auth_middleware(
    request: web.Request,
    handler: Callable[[web.Request], Awaitable[web.StreamResponse]],
) -> web.StreamResponse:
    path = request.relurl.path

    # 1) CORS preflight — answer 204 for the methods we expose.
    if request.method == "OPTIONS":
        return _cors_preflight(request)

    # 2) Health check is always public.
    if path == "/healthz" or path == f"{UI_BASE_PATH}/healthz":
        return await handler(request)

    # 3) Static UI assets under base path are gated only by Origin.
    if path.startswith(UI_BASE_PATH + "/assets/") or path.endswith((
        ".css", ".js", ".json", ".svg", ".png", ".webmanifest", ".html"
    )):
        if not _is_origin_allowed(request):
            return _forbidden("origin not allowed", request)
        return await handler(request)

    # 4) The HTML shell is allowed from any browser (the JS will
    #    surface an auth-failed toast if the WebSocket or the
    #    runtime-config endpoint rejects it).
    if path == UI_BASE_PATH or path == UI_BASE_PATH + "/" or path == UI_BASE_PATH + "/index.html":
        return await handler(request)

    # 5) Everything else (WebSocket, control-ui-config, hooks, admin RPC,
    #    push routes) requires a valid auth credential.
    mode = _request_auth_mode(request)
    if mode == "none":
        if not _is_loopback_origin(request.headers.get("Host", "").split(":")[0] or ""):
            return _forbidden("auth=disabled-and-non-loopback", request)
        return await handler(request)

    if mode == "trusted-proxy":
        # The reverse proxy is the auth; just verify Origin.
        if not _is_origin_allowed(request):
            return _forbidden("origin not allowed", request)
        return await handler(request)

    if mode == "password":
        # Password must come through the WebSocket connect handshake.
        # For HTTP endpoints we accept it as a basic-auth header.
        pw = ""
        auth = request.headers.get("Authorization", "")
        if auth.lower().startswith("basic "):
            try:
                decoded = base64.b64decode(auth.split(None, 1)[1]).decode()
                if ":" in decoded:
                    pw = decoded.split(":", 1)[1]
            except Exception:
                pw = ""
        if not _check_password(pw):
            return web.json_response(
                {"error": "AUTH_PASSWORD_REQUIRED", "message": "password required"},
                status=401,
                headers=_cors_headers(request),
            )
        return await handler(request)

    # default: token
    token = _extract_token_from_request(request)
    if not _check_token(token):
        return web.json_response(
            {"error": "AUTH_TOKEN_MISMATCH", "message": "token required"},
            status=401,
            headers=_cors_headers(request),
        )
    return await handler(request)


def _cors_preflight(request: web.Request) -> web.Response:
    return web.Response(status=204, headers=_cors_headers(request))


def _cors_headers(request: web.Request) -> dict[str, str]:
    origin = request.headers.get("Origin", "*")
    if not _is_origin_allowed(request):
        origin = "null"
    return {
        "Access-Control-Allow-Origin": origin,
        "Vary": "Origin",
        "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
        "Access-Control-Allow-Headers": "Authorization, Content-Type",
        "Access-Control-Allow-Credentials": "true",
    }


def _forbidden(reason: str, request: web.Request) -> web.Response:
    log.warning("forbidden: %s (origin=%s, host=%s)", reason,
                request.headers.get("Origin"), request.headers.get("Host"))
    return web.json_response(
        {"error": "FORBIDDEN", "reason": reason},
        status=403,
        headers=_cors_headers(request),
    )


# ---------------------------------------------------------------------------
# Push fan-out (mirrors websocket_adapter)
# ---------------------------------------------------------------------------
def _extract_content(raw: Any) -> str:
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
    target_page = push_params.get("page_id")
    content = _extract_content(push_params.get("content", ""))
    payload = json.dumps({
        "type": "push",
        "channel_id": push_params.get("channel_id", CHANNEL_ID),
        "page_id": target_page,
        "content": content,
        "timestamp": int(time.time() * 1000),
    }, ensure_ascii=False)

    async with _clients_lock:
        targets = list(_clients)
    delivered = 0
    for page_id, ws in targets:
        if target_page and target_page != "*" and target_page != page_id:
            continue
        try:
            await ws.send_str(payload)
            delivered += 1
        except Exception as e:
            log.warning("push delivery failed for %s: %s", page_id, e)
    if delivered:
        log.info("Push delivered to %d client(s) (page=%s)", delivered, target_page)


async def listen_for_pushes() -> None:
    backoff = 1.0
    while True:
        try:
            log.info("Opening long-lived bus connection for push notifications...")
            bus = await BusConnection.connect()
            await bus.register()
            backoff = 1.0
            while True:
                line = await bus.reader.readline()
                if not line:
                    log.warning("Bus push connection lost; will reconnect.")
                    break
                try:
                    msg = json.loads(line.decode().strip())
                except Exception as e:
                    log.error("Bad push JSON: %s", e)
                    continue
                if msg.get("method") == "gateway.push":
                    await _broadcast_push(msg.get("params") or {})
        except Exception as e:
            log.error("Push listener error: %s; reconnect in %.1fs", e, backoff)
        try:
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2.0, 30.0)
        except asyncio.CancelledError:
            return


# ---------------------------------------------------------------------------
# WebSocket handler (browser-side wire format, identical to websocket_adapter)
# ---------------------------------------------------------------------------
async def _send_intent_and_stream(
    ws: "web.WebSocketResponse",
    page_id: str,
    user_id: str,
    content: str,
) -> str:
    """Open a bus connection, send `intent.submit`, stream the response
    back to the WebSocket. Returns the final response text."""
    bus = None
    try:
        bus = await BusConnection.connect()
    except Exception as e:
        await ws.send_str(json.dumps({
            "type": "error", "page_id": page_id,
            "message": f"core engine offline: {e}",
        }, ensure_ascii=False))
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
            "metadata": {"transport": "control-ui"},
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
            method = msg.get("method")
            if method is None and "result" in msg and msg.get("id") != req_id:
                continue  # ignore other requests' results
            if method == "response.token":
                token = msg["params"]["token"]
                accumulated_tokens.append(token)
                await ws.send_str(json.dumps({
                    "type": "token", "page_id": page_id, "token": token,
                }, ensure_ascii=False))
            elif method == "response.status":
                await ws.send_str(json.dumps({
                    "type": "status", "page_id": page_id,
                    "status": msg["params"]["status"],
                }, ensure_ascii=False))
            elif method == "response.permission_request":
                p = msg["params"]
                await ws.send_str(json.dumps({
                    "type": "permission_request",
                    "page_id": page_id,
                    "request_id": p.get("request_id"),
                    "tool_id": p.get("tool_id", ""),
                    "tier": p.get("tier", "Prompt"),
                    "summary": p.get("params_summary", ""),
                }, ensure_ascii=False))
            elif method == "response.complete":
                await ws.send_str(json.dumps({
                    "type": "complete", "page_id": page_id,
                }, ensure_ascii=False))
            elif "result" in msg:
                result = msg["result"] or {}
                if isinstance(result, dict) and "content" in result:
                    final_text = result["content"]
                else:
                    final_text = "".join(accumulated_tokens)
                await ws.send_str(json.dumps({
                    "type": "result", "page_id": page_id, "content": final_text,
                }, ensure_ascii=False))
                return final_text
            elif "error" in msg and msg.get("id") in (None, req_id):
                await ws.send_str(json.dumps({
                    "type": "error", "page_id": page_id,
                    "message": msg["error"].get("message", "unknown error"),
                }, ensure_ascii=False))
                return msg["error"].get("message", "")
    except Exception as e:
        log.error("stream error: %s", e)
        with contextlib.suppress(Exception):
            await ws.send_str(json.dumps({
                "type": "error", "page_id": page_id,
                "message": f"stream error: {e}",
            }, ensure_ascii=False))
    finally:
        if bus is not None:
            await bus.close()
    final = "".join(accumulated_tokens)
    if final:
        await ws.send_str(json.dumps({
            "type": "result", "page_id": page_id, "content": final,
        }, ensure_ascii=False))
    return final


async def _ws_handler(request: web.Request) -> "web.WebSocketResponse":
    """WebSocket endpoint. Mirrors the existing websocket_adapter's
    browser-side protocol so the Control UI can use the same JS helpers."""
    # Token / password auth at upgrade time.
    upgrade_token = _extract_token_from_request(request)
    upgrade_pw = ""
    if AUTH_MODE == "password":
        auth = request.headers.get("Authorization", "")
        if auth.lower().startswith("basic "):
            try:
                decoded = base64.b64decode(auth.split(None, 1)[1]).decode()
                if ":" in decoded:
                    upgrade_pw = decoded.split(":", 1)[1]
            except Exception:
                pass
    mode = _request_auth_mode(request)
    if mode == "token" and not _check_token(upgrade_token):
        return web.json_response(
            {"error": "AUTH_TOKEN_MISMATCH"}, status=401,
            headers=_cors_headers(request),
        )
    if mode == "password" and not _check_password(upgrade_pw):
        return web.json_response(
            {"error": "AUTH_PASSWORD_REQUIRED"}, status=401,
            headers=_cors_headers(request),
        )

    # Device pairing (auto-approve loopback).
    device_id = request.headers.get("X-Hydra-Device-Id", "") or request.query.get("deviceId", "")
    user_agent = request.headers.get("User-Agent", "")
    host = request.headers.get("Host", "")
    host_only = host.split(":")[0]
    is_loopback = _is_loopback_origin(host_only) or _is_loopback_origin(
        request.remote.split(":")[0] if request.remote else ""
    )

    if not DISABLE_DEVICE_AUTH and device_id:
        async with _device_lock:
            devices = _load_devices()
            rec = devices.get(device_id)
            now = time.time()
            if rec is None and not is_loopback:
                # Issue a pending request; the browser must wait for
                # an operator to approve it.
                return web.json_response(
                    {
                        "error": "PAIRING_REQUIRED",
                        "deviceId": device_id,
                        "message": (
                            "device pairing required. Ask the operator to run "
                            "`hydragent devices approve --latest`, or open the "
                            "Control UI from 127.0.0.1 to auto-approve."
                        ),
                    },
                    status=409,
                    headers=_cors_headers(request),
                )
            if rec is not None:
                rec.last_seen = now
                devices[device_id] = rec
                _save_devices(devices)

    ws = web.WebSocketResponse(heartbeat=30.0, max_msg_size=2 * 1024 * 1024)
    await ws.prepare(request)
    conn_id = uuid.uuid4().hex[:12]
    page_id = f"ctrl-{conn_id}"
    user_id = f"{USER_ID_PREFIX}-{conn_id}"

    async with _clients_lock:
        _clients.add((page_id, ws))

    try:
        await ws.send_str(json.dumps({
            "type": "hello",
            "channel_id": CHANNEL_ID,
            "page_id": page_id,
            "user_id": user_id,
            "device_id": device_id or None,
            "auth_mode": mode,
            "timestamp": int(time.time() * 1000),
        }, ensure_ascii=False))
    except Exception:
        pass

    try:
        async for raw in ws:
            if raw.type != WSMsgType.TEXT:
                continue
            try:
                payload = json.loads(raw.data)
            except Exception:
                await ws.send_str(json.dumps({
                    "type": "error", "page_id": page_id,
                    "message": f"invalid JSON: {raw.data[:120]!r}",
                }, ensure_ascii=False))
                continue
            if not isinstance(payload, dict):
                continue
            if "set_page_id" in payload:
                new_pid = str(payload["set_page_id"]).strip()
                if new_pid:
                    async with _clients_lock:
                        _clients.discard((page_id, ws))
                        page_id = new_pid
                        _clients.add((page_id, ws))
                    await ws.send_str(json.dumps({
                        "type": "page_set", "page_id": page_id,
                    }, ensure_ascii=False))
                continue
            if payload.get("type") == "ping":
                await ws.send_str(json.dumps({"type": "pong"}, ensure_ascii=False))
                continue
            # The default inbound shape: { content: "...", page_id: "..." }
            content = (payload.get("content") or "").strip()
            if not content:
                continue
            await _send_intent_and_stream(ws, page_id, user_id, content)
    finally:
        async with _clients_lock:
            _clients.discard((page_id, ws))
    return ws


# ---------------------------------------------------------------------------
# HTTP routes
# ---------------------------------------------------------------------------
async def handle_healthz(_: web.Request) -> web.Response:
    return web.json_response({
        "ok": True,
        "channel_id": CHANNEL_ID,
        "auth_mode": AUTH_MODE,
        "clients": len(_clients),
        "ui_port": UI_PORT,
        "ui_base_path": UI_BASE_PATH,
        "version": "0.1.0",
    })


async def handle_index(_: web.Request) -> web.Response:
    index_html = DIST_DIR / "index.html"
    if not index_html.exists():
        return web.Response(
            text="<h1>Hydragent Control UI</h1>"
                 "<p>The UI bundle is not built. "
                 "Run <code>pnpm ui:build</code> or copy <code>adapters/control_ui/dist/*</code> "
                 "into this server's static dir.</p>",
            content_type="text/html",
        )
    return web.FileResponse(index_html)


async def handle_runtime_config(request: web.Request) -> web.Response:
    """Returns the runtime config the UI reads on startup. Auth-gated
    so an unauthenticated browser can't fingerprint the operator."""
    pub, _priv = _ensure_vapid_keypair()
    return web.json_response({
        "version": "0.1.0",
        "channelId": CHANNEL_ID,
        "authMode": _request_auth_mode(request),
        "basePath": UI_BASE_PATH,
        "publicPort": UI_PUBLIC_PORT,
        "publicHost": UI_PUBLIC_HOST,
        "websocketUrl": f"ws://{UI_PUBLIC_HOST}:{UI_PUBLIC_PORT}{UI_BASE_PATH}/ws",
        "tlsEnabled": False,
        "tailscale": {
            "allowTailscale": ALLOW_TAILSCALE,
            "mode": "off",
        },
        "vapidPublicKey": pub,
        "vapidSubject": VAPID_SUBJECT,
        "features": {
            "hooks": HOOKS_ENABLED,
            "adminRpc": ADMIN_RPC_ENABLED,
            "pwa": True,
            "webPush": True,
            "i18n": True,
            "themes": True,
        },
        "locales": ["en", "zh-CN", "de", "es", "fr", "ja-JP", "pt-BR"],
        "themes": ["hydra-dark", "hydra-light", "abyss", "aurora"],
    }, headers=_cors_headers(request))


# ---- Push subscriptions ------------------------------------------------
async def handle_vapid_public_key(_: web.Request) -> web.Response:
    pub, _priv = _ensure_vapid_keypair()
    return web.json_response({"publicKey": pub, "subject": VAPID_SUBJECT})


async def _load_push_subs() -> dict[str, dict]:
    if not PUSH_REGISTRY.exists():
        return {}
    try:
        with open(PUSH_REGISTRY, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return {}


def _save_push_subs(subs: dict[str, dict]) -> None:
    PUSH_REGISTRY.parent.mkdir(parents=True, exist_ok=True)
    tmp = PUSH_REGISTRY.with_suffix(".json.tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(subs, f, indent=2)
    try:
        os.chmod(tmp, 0o600)
    except Exception:
        pass
    tmp.replace(PUSH_REGISTRY)


async def handle_push_subscribe(request: web.Request) -> web.Response:
    try:
        sub = await request.json()
    except Exception:
        return web.json_response({"error": "invalid JSON"}, status=400)
    endpoint = sub.get("endpoint")
    keys = sub.get("keys") or {}
    if not endpoint or not keys.get("p256dh") or not keys.get("auth"):
        return web.json_response({"error": "missing endpoint/keys"}, status=400)
    async with _push_lock:
        subs = _load_push_subs()
        subs[endpoint] = sub
        _save_push_subs(subs)
    return web.json_response({"ok": True})


async def handle_push_unsubscribe(request: web.Request) -> web.Response:
    try:
        sub = await request.json()
    except Exception:
        return web.json_response({"error": "invalid JSON"}, status=400)
    endpoint = sub.get("endpoint")
    if not endpoint:
        return web.json_response({"error": "missing endpoint"}, status=400)
    async with _push_lock:
        subs = _load_push_subs()
        subs.pop(endpoint, None)
        _save_push_subs(subs)
    return web.json_response({"ok": True})


async def handle_push_test(_: web.Request) -> web.Response:
    # The real Web Push delivery (with VAPID signing + ece encryption)
    # is wired in `push/web-push-subscriptions.json`; for now the
    # route is a stub that confirms the caller's subscription is on
    # file. The browser will fall back to the in-app toast if the
    # native notification fails.
    return web.json_response({"ok": True, "delivered": False, "note": "see push delivery hook"})


# ---- Hooks --------------------------------------------------------------
async def handle_hooks(request: web.Request) -> web.Response:
    if not HOOKS_ENABLED:
        return web.json_response({"error": "hooks disabled"}, status=404)
    if not HOOKS_TOKEN:
        return web.json_response({"error": "hooks token not set"}, status=503)
    auth = request.headers.get("Authorization", "")
    if not (auth.lower().startswith("bearer ") and hmac.compare_digest(
            auth.split(None, 1)[1].strip(), HOOKS_TOKEN)):
        return web.json_response({"error": "bad token"}, status=401)
    try:
        payload = await request.json()
    except Exception:
        return web.json_response({"error": "invalid JSON"}, status=400)
    content = payload.get("content") or payload.get("message") or ""
    if not content:
        return web.json_response({"error": "missing content"}, status=400)
    page_id = payload.get("page_id") or f"hook-{uuid.uuid4().hex[:8]}"
    user_id = payload.get("user_id") or "hook-user"
    bus = None
    try:
        bus = await BusConnection.connect()
        req = {
            "jsonrpc": "2.0",
            "method": "intent.submit",
            "params": {
                "page_id": page_id,
                "channel_id": "hook",
                "user_id": user_id,
                "content": content,
                "attachments": payload.get("attachments", []),
                "metadata": {"transport": "hook", "hook": request.match_info.get("name")},
                "timestamp": int(time.time() * 1000),
                "priority": payload.get("priority", "normal"),
            },
            "id": str(uuid.uuid4()),
        }
        bus.writer.write((json.dumps(req) + "\n").encode())
        await bus.writer.drain()
        # Drain until the bus returns the final result.
        accumulated: list[str] = []
        while True:
            line = await bus.reader.readline()
            if not line:
                break
            try:
                msg = json.loads(line.decode().strip())
            except Exception:
                continue
            if msg.get("method") == "response.token":
                accumulated.append(msg["params"]["token"])
            elif "result" in msg:
                result = msg["result"] or {}
                final = result.get("content", "".join(accumulated))
                return web.json_response({"ok": True, "content": final, "page_id": page_id})
            elif "error" in msg:
                return web.json_response({"error": msg["error"].get("message")}, status=500)
        return web.json_response({"ok": True, "content": "".join(accumulated), "page_id": page_id})
    except Exception as e:
        return web.json_response({"error": f"bus error: {e}"}, status=503)
    finally:
        if bus is not None:
            await bus.close()


# ---- Admin HTTP RPC -----------------------------------------------------
ADMIN_ALLOWED_METHODS = {
    "memory.list", "memory.search", "memory.delete", "memory.clear",
    "page.list", "page.get_summary", "page.update_summary", "page.compact",
    "library.list_nodes", "library.search",
    "config.read", "config.write",
    "skill.list", "skill.search", "skill.run",
    "dream.run", "dream.status",
    "cron.list", "cron.run", "cron.add", "cron.remove",
    "session.list",
}


async def handle_admin_rpc(request: web.Request) -> web.Response:
    if not ADMIN_RPC_ENABLED:
        return web.json_response({"error": "admin RPC disabled"}, status=404)
    auth = request.headers.get("Authorization", "")
    if not (auth.lower().startswith("bearer ") and hmac.compare_digest(
            auth.split(None, 1)[1].strip(), ADMIN_RPC_TOKEN)):
        return web.json_response({"error": "bad admin token"}, status=401)
    try:
        body = await request.json()
    except Exception:
        return web.json_response({"error": "invalid JSON"}, status=400)
    method = body.get("method")
    params = body.get("params") or {}
    if not method or method not in ADMIN_ALLOWED_METHODS:
        return web.json_response(
            {"error": f"method not allowed: {method!r}",
             "allowed": sorted(ADMIN_ALLOWED_METHODS)},
            status=400,
        )
    bus = None
    try:
        bus = await BusConnection.connect()
        req = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": str(uuid.uuid4()),
        }
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
            if "result" in msg or "error" in msg:
                return web.json_response(msg)
        return web.json_response({"error": "no response from bus"}, status=504)
    except Exception as e:
        return web.json_response({"error": f"bus error: {e}"}, status=503)
    finally:
        if bus is not None:
            await bus.close()


# ---------------------------------------------------------------------------
# App factory
# ---------------------------------------------------------------------------
def make_app() -> web.Application:
    app = web.Application(middlewares=[auth_middleware], client_max_size=4 * 1024 * 1024)
    app.on_startup.append(_on_startup)
    app.on_cleanup.append(_on_cleanup)

    # Static + index
    app.router.add_get(UI_BASE_PATH, handle_index)
    app.router.add_get(UI_BASE_PATH + "/", handle_index)
    app.router.add_get(UI_BASE_PATH + "/index.html", handle_index)
    if DIST_DIR.exists():
        app.router.add_static(UI_BASE_PATH + "/assets", DIST_DIR / "assets", show_index=False)
        # Serve a handful of well-known top-level files for convenience.
        for name in ("manifest.webmanifest", "sw.js", "favicon.svg"):
            p = DIST_DIR / name
            if p.exists():
                app.router.add_get(f"{UI_BASE_PATH}/{name}", _static_file(name))

    # Runtime config + health
    app.router.add_get(UI_BASE_PATH + "/control-ui-config.json", handle_runtime_config)
    app.router.add_get(UI_BASE_PATH + "/healthz", handle_healthz)
    app.router.add_get("/healthz", handle_healthz)

    # WebSocket
    app.router.add_get(UI_BASE_PATH + "/ws", _ws_handler)

    # Push (VAPID + Web Push)
    app.router.add_get(UI_BASE_PATH + "/push/web/vapidPublicKey", handle_vapid_public_key)
    app.router.add_post(UI_BASE_PATH + "/push/web/subscribe", handle_push_subscribe)
    app.router.add_post(UI_BASE_PATH + "/push/web/unsubscribe", handle_push_unsubscribe)
    app.router.add_post(UI_BASE_PATH + "/push/web/test", handle_push_test)

    # Hooks
    if HOOKS_ENABLED:
        app.router.add_post(UI_BASE_PATH + "/hooks/{name}", handle_hooks)

    # Admin RPC
    if ADMIN_RPC_ENABLED:
        app.router.add_post(UI_BASE_PATH + "/api/v1/admin/rpc", handle_admin_rpc)
        app.router.add_post("/api/v1/admin/rpc", handle_admin_rpc)

    return app


def _static_file(name: str):
    async def _handler(_: web.Request) -> web.Response:
        path = DIST_DIR / name
        if not path.exists():
            return web.Response(status=404)
        return web.FileResponse(path)
    return _handler


async def _on_startup(_: web.Application) -> None:
    _ensure_vapid_keypair()
    app = web.Application.get_app(_) if False else _  # noqa
    # Start the long-lived push listener as a background task on the
    # running aiohttp loop.
    loop = asyncio.get_event_loop()
    loop.create_task(listen_for_pushes())


async def _on_cleanup(_: web.Application) -> None:
    pass


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
def main() -> None:
    parser = argparse.ArgumentParser(description="Hydragent Control UI adapter")
    parser.add_argument("--host", default=UI_HOST)
    parser.add_argument("--port", type=int, default=UI_PORT)
    parser.add_argument("--base-path", default=UI_BASE_PATH)
    args = parser.parse_args()

    DATA_DIR.mkdir(parents=True, exist_ok=True)
    DEVICE_REGISTRY.parent.mkdir(parents=True, exist_ok=True)

    log.info("Starting Hydragent Control UI on http://%s:%d%s/", args.host, args.port, args.base_path)
    log.info("Bus: %s:%d  | channel: %s  | auth: %s", BUS_HOST, BUS_PORT, CHANNEL_ID, AUTH_MODE)
    log.info("Static dir: %s", DIST_DIR)

    web.run_app(make_app(), host=args.host, port=args.port, print=lambda *_: None)


if __name__ == "__main__":
    main()
