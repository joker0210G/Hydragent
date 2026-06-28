import os
import sys
import asyncio
# Inject paths for parent directory (adapters) and utils/
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "../utils")))
import json
import uuid
import time
import re
import logging
import sqlite3
from dataclasses import dataclass
from typing import Optional
from telegram import Update, InlineKeyboardButton, InlineKeyboardMarkup, BotCommand, WebAppInfo, MenuButtonWebApp
from telegram.ext import Application, CommandHandler, MessageHandler, CallbackQueryHandler, filters, ContextTypes
from aiohttp import web
import urllib.parse
from logging.handlers import RotatingFileHandler

# === Telegram Bot API 10.1 (June 11, 2026) — Rich message formatting ===
# We default to MarkdownV2 because it exposes the full entity set (underline,
# strikethrough, spoiler, expandable blockquote, custom emoji, date_time)
# AND is the required mode for the new sendMessageDraft streaming API. Legacy
# "Markdown" parse mode is kept as a fallback alias for callers that build
# strings without re-escaping. The new Rich Message transport (sendRichMessage,
# sendRichMessageDraft) is exposed via the safe_* helpers below.
# Docs: https://core.telegram.org/bots/api#rich-message-formatting-options
PARSE_MODE = "MarkdownV2"
# Per the MarkdownV2 spec, every character below must be escaped with a
# preceding backslash outside of `pre` and `code` entities.
_MARKDOWNV2_SPECIAL = set("_*[]()~`>#+-=|{}.!\\")

# Heuristic: text patterns that only Rich Messages can render. When we detect
# any of these in the LLM's final output we route the send through
# `sendRichMessage` instead of plain `sendMessage`.
_RICH_MARKDOWN_MARKERS = (
    re.compile(r"(?m)^\s*#{1,6}\s"),                        # ATX heading
    re.compile(r"(?m)^\s*>\s"),                             # block quote
    re.compile(r"(?m)^\s*[-*+]\s\[[ xX]\]"),                # task list
    re.compile(r"(?m)^\s*\|.*\|.*\|"),                      # GFM table row
    re.compile(r"\$\$"),                                    # math block
    re.compile(r"```math\b", re.IGNORECASE),                # math fenced
    re.compile(r"<details\b", re.IGNORECASE),              # details block
    re.compile(r"\[\^[^\]]+\]"),                            # footnote ref
)


def escape_markdown(text: str) -> str:
    """Escapes characters reserved by Telegram MarkdownV2 parse mode.

    MarkdownV2 is a strict superset of legacy Markdown. Anywhere outside
    `pre` and `code` blocks the characters
    ``_ * [ ] ( ) ~ ` > # + - = | { } . ! \\`` must be escaped with a
    preceding backslash, otherwise the Bot API rejects the message.
    """
    if not isinstance(text, str):
        text = str(text)
    return "".join("\\" + ch if ch in _MARKDOWNV2_SPECIAL else ch for ch in text)


def _looks_like_rich_content(text: str) -> bool:
    """Return True if ``text`` contains constructs only Rich Messages can render."""
    if not text:
        return False
    return any(marker.search(text) for marker in _RICH_MARKDOWN_MARKERS)


def _apply_link_preview(kwargs: dict) -> dict:
    """Inject `link_preview_options.is_disabled` if the global toggle is on.

    P12.8 — when DISABLE_LINK_PREVIEW is True, every outbound message
    that contains a URL is sent with the preview suppressed. Telegram
    renders the URL as a plain link instead. We do this centrally here
    so the rest of the adapter never has to remember to set the flag.
    """
    if not DISABLE_LINK_PREVIEW:
        return kwargs
    if "link_preview_options" in kwargs:
        return kwargs
    kwargs["link_preview_options"] = {"is_disabled": True}
    return kwargs


async def safe_send_message(bot, chat_id, text, *, parse_mode=PARSE_MODE, **kwargs):
    """Send a message and gracefully fall back to plain text on parse errors.

    P12.7 — short-circuits to a logged stub return when DRY_RUN is set,
    so test / CI runs never hit the real Bot API. The caller still
    receives an object that quacks like the real Message (with a
    ``message_id`` and ``chat``) so downstream code keeps working.
    """
    if DRY_RUN:
        logger.debug(f"[DRY_RUN] safe_send_message skipped → chat={chat_id}: {text[:80]!r}")
        return _DryRunMessage(chat_id=chat_id, text=text or "")
    try:
        return await bot.send_message(
            chat_id=chat_id, text=text, parse_mode=parse_mode,
            **_apply_link_preview(kwargs),
        )
    except Exception as e:
        err = str(e).lower()
        if "parse" in err or "entity" in err or "can't parse" in err:
            logger.debug(f"safe_send_message: falling back to plain text ({e})")
            return await bot.send_message(
                chat_id=chat_id, text=text,
                **_apply_link_preview(kwargs),
            )
        raise


async def safe_edit_message(bot, chat_id, message_id, text, *, parse_mode=PARSE_MODE, **kwargs):
    """Edit a message text and gracefully fall back to plain text on parse errors.

    Honours DRY_RUN and DISABLE_LINK_PREVIEW the same way as
    ``safe_send_message``.
    """
    if DRY_RUN:
        logger.debug(f"[DRY_RUN] safe_edit_message skipped → chat={chat_id} msg={message_id}")
        return _DryRunMessage(chat_id=chat_id, message_id=message_id, text=text or "")
    try:
        return await _safe_edit_message_impl(
            bot, chat_id, message_id, text, parse_mode, kwargs,
        )
    except Exception as e:
        err = str(e).lower()
        if "parse" in err or "entity" in err or "can't parse" in err:
            logger.debug(f"safe_edit_message: falling back to plain text ({e})")
            return await _safe_edit_message_impl(
                bot, chat_id, message_id, text, None, kwargs,
            )
        raise


async def _safe_edit_message_impl(bot, chat_id, message_id, text, parse_mode, kwargs):
    """Internal: thin wrapper used by safe_edit_message for the actual edit call.

    Kept separate so safe_edit_message stays readable and so the
    parse-error fallback path doesn't duplicate kwarg-handling code.
    """
    return await bot.edit_message_text(
        chat_id=chat_id, message_id=message_id, text=text,
        parse_mode=parse_mode, **_apply_link_preview(kwargs),
    )


class _DryRunMessage:
    """Stub message object returned by safe_* when DRY_RUN is enabled.

    Implements just enough of the ``telegram.Message`` interface that
    downstream code (``sent_msg.message_id``, ``sent_msg.chat.id``,
    ``await sent_msg.edit_text(...)``) keeps working in tests.
    """

    def __init__(self, chat_id, message_id=None, text=""):
        self.message_id = message_id if message_id is not None else 0
        self.text = text
        # ``chat`` is accessed by the streaming loop to read chat_id
        # back; expose a minimal stand-in.
        self.chat = type("_ChatStub", (), {"id": chat_id})()

    async def edit_text(self, text=None, **kwargs):
        logger.debug(f"[DRY_RUN] edit_text skipped → msg={self.message_id}: {text!r}")
        return self

    async def reply_text(self, text=None, **kwargs):
        logger.debug(f"[DRY_RUN] reply_text skipped → chat={self.chat.id}: {text!r}")
        return _DryRunMessage(chat_id=self.chat.id, text=text or "")

    async def delete(self, **kwargs):
        logger.debug(f"[DRY_RUN] delete skipped → msg={self.message_id}")
        return True


async def _do_api(bot, method, **api_kwargs):
    """Call any Bot API method via ``Bot.do_api_request``.

    Used for Bot API 10.1 methods that are not yet wrapped by
    python-telegram-bot (sendRichMessage, sendMessageDraft, etc.).
    """
    return await bot.do_api_request(method, api_kwargs=api_kwargs)


async def send_rich_message(bot, chat_id, *, markdown=None, html=None, **kwargs):
    """Send a Rich Message via Bot API 10.1 ``sendRichMessage``.

    Pass exactly one of ``markdown`` (Rich Markdown) or ``html`` (Rich HTML).
    """
    if (markdown is None) == (html is None):
        raise ValueError("send_rich_message requires exactly one of `markdown` or `html`")
    rich_message = {"markdown" if markdown is not None else "html": markdown or html}
    api_kwargs = {"chat_id": chat_id, "rich_message": rich_message, **kwargs}
    return await _do_api(bot, "sendRichMessage", **api_kwargs)


async def send_message_draft(bot, chat_id, draft_id, text, *, parse_mode=PARSE_MODE, **kwargs):
    """Stream a partial text message draft (Bot API 10.1 ``sendMessageDraft``).

    Drafts are 30-second ephemeral previews visible only in 1:1 chats.
    They are NOT persisted — once output is finalized, call
    ``sendMessage`` (or ``safe_send_message``) to persist the final text.
    """
    api_kwargs = {
        "chat_id": chat_id,
        "draft_id": draft_id,
        "text": text,
    }
    if parse_mode:
        api_kwargs["parse_mode"] = parse_mode
    api_kwargs.update(kwargs)
    return await _do_api(bot, "sendMessageDraft", **api_kwargs)


async def send_rich_draft(bot, chat_id, draft_id, *, markdown=None, html=None, **kwargs):
    """Stream a partial Rich Message draft (Bot API 10.1 ``sendRichMessageDraft``)."""
    if (markdown is None) == (html is None):
        raise ValueError("send_rich_draft requires exactly one of `markdown` or `html`")
    rich_message = {"markdown" if markdown is not None else "html": markdown or html}
    api_kwargs = {
        "chat_id": chat_id,
        "draft_id": draft_id,
        "rich_message": rich_message,
    }
    api_kwargs.update(kwargs)
    return await _do_api(bot, "sendRichMessageDraft", **api_kwargs)


# === Bot API 10.1 — Rich message & update-side helpers ===
# Bot API 10.1 added the ``rich_message`` field to the ``Message`` object and
# a ``rich_message`` parameter to ``editMessageText``. These helpers let us
# (a) accept rich inbound messages from users, (b) edit rich outbound
# messages in place, and (c) transparently fall back to MarkdownV2 / plain
# text when the rich transport is rejected by the server.

def _rich_text_to_string(rt) -> str:
    """Recursively flatten a RichText value to its plain-text representation.

    ``RichText`` is a recursive union — it can be a plain string, a list of
    RichText values, or one of the ``RichText*`` objects. We only need the
    textual content for logging, parenthetical-remark capture, and feeding
    the agent's text pipeline. ``RichTextMathematicalExpression`` keeps its
    LaTeX source so the model still sees the formula on the way to the bus.
    """
    if rt is None:
        return ""
    if isinstance(rt, str):
        return rt
    if isinstance(rt, list):
        return "".join(_rich_text_to_string(x) for x in rt)
    if isinstance(rt, dict):
        if "text" in rt:
            return _rich_text_to_string(rt["text"])
        if "name" in rt:            # RichTextAnchor
            return ""
        if "expression" in rt:      # RichTextMathematicalExpression
            return rt.get("expression", "")
        if "alternative_text" in rt:  # RichTextCustomEmoji
            return rt.get("alternative_text", "")
    return ""


def _rich_message_to_text(rich_message) -> str:
    """Best-effort plain-text rendering of a ``RichMessage`` for downstream use.

    Walks the top-level ``blocks`` list and concatenates their textual
    content. Used to (a) feed rich user messages into the agent's text
    pipeline and (b) recover a MarkdownV2 source string we can hand off to
    ``sendMessage`` / ``sendRichMessage`` for outbound persistence.
    """
    if not rich_message:
        return ""
    if isinstance(rich_message, dict):
        blocks = rich_message.get("blocks", [])
    else:
        blocks = getattr(rich_message, "blocks", None) or []
    parts: list[str] = []
    for block in blocks:
        if isinstance(block, dict):
            text = block.get("text")
        else:
            text = getattr(block, "text", None)
        rendered = _rich_text_to_string(text)
        if rendered:
            parts.append(rendered)
            continue
        # Fall back to caption / credit if the block has no body text.
        for key in ("caption", "credit"):
            v = block.get(key) if isinstance(block, dict) else getattr(block, key, None)
            rendered = _rich_text_to_string(v)
            if rendered:
                parts.append(rendered)
                break
    return "\n\n".join(p for p in parts if p)


async def safe_edit_rich_message(bot, chat_id, message_id, *, markdown=None, html=None, **kwargs):
    """Edit an existing message with rich content via ``editMessageText``.

    Bot API 10.1 made the ``text`` and ``rich_message`` parameters on
    ``editMessageText`` mutually optional — exactly one must be supplied.
    On failure we fall back to ``safe_edit_message`` which itself retries
    without MarkdownV2 if the parse fails.
    """
    if (markdown is None) == (html is None):
        raise ValueError("safe_edit_rich_message requires exactly one of `markdown` or `html`")
    rich_payload = markdown if markdown is not None else html
    rich_message = {"markdown" if markdown is not None else "html": rich_payload}
    try:
        return await _do_api(
            bot, "editMessageText",
            chat_id=chat_id, message_id=message_id, rich_message=rich_message, **kwargs,
        )
    except Exception as e:
        logger.debug(f"safe_edit_rich_message: falling back to safe_edit_message ({e})")
        return await safe_edit_message(bot, chat_id, message_id, rich_payload or "")


async def safe_send_rich_message(bot, chat_id, *, markdown=None, html=None, **kwargs):
    """Send a Rich Message with graceful fallback to plain MarkdownV2.

    On a ``sendRichMessage`` failure we re-issue the request via
    ``safe_send_message`` with the raw markdown source so the user still
    sees a usable (if unstyled) reply.
    """
    if (markdown is None) == (html is None):
        raise ValueError("safe_send_rich_message requires exactly one of `markdown` or `html`")
    rich_payload = markdown if markdown is not None else html
    try:
        return await send_rich_message(bot, chat_id, markdown=markdown, html=html, **kwargs)
    except Exception as e:
        logger.debug(f"safe_send_rich_message: falling back to safe_send_message ({e})")
        return await safe_send_message(bot, chat_id, rich_payload or "")


async def safe_send_rich_draft(bot, chat_id, draft_id, *, markdown=None, html=None, **kwargs):
    """Best-effort ``sendRichMessageDraft`` wrapper.

    Rich drafts are ephemeral 30-second previews so there is no durable
    fallback — on failure we re-raise and let the caller decide whether to
    degrade to a plain ``sendMessageDraft`` or to the legacy edit loop.
    """
    if (markdown is None) == (html is None):
        raise ValueError("safe_send_rich_draft requires exactly one of `markdown` or `html`")
    return await send_rich_draft(bot, chat_id, draft_id, markdown=markdown, html=html, **kwargs)


async def send_thinking_rich_draft(bot, chat_id, draft_id, *, status_text="Thinking…"):
    """Show a rich "thinking" placeholder via ``sendRichMessageDraft``.

    Uses the new ``RichBlockThinking`` block which is *only* legal in
    ``sendRichMessageDraft`` and renders the client-side "Thinking…"
    animation. See https://t.me/addemoji/AIActions for the recommended
    custom-emoji set, e.g. ``🧠`` or ``💭``.
    """
    thinking_block = {
        "type": "thinking",
        "text": status_text,  # plain RichText (string) is permitted here
    }
    rich_message = {"blocks": [thinking_block]}
    return await _do_api(
        bot, "sendRichMessageDraft",
        chat_id=chat_id, draft_id=draft_id, rich_message=rich_message,
    )

try:
    from pyngrok import ngrok
except ImportError:
    ngrok = None

try:
    from generate_library_graph import generate_graph
except ImportError:
    generate_graph = lambda: None

from dotenv import load_dotenv

# Load environment variables from .env FIRST so `LOG_LEVEL` is honoured
# by `setup_logging()` below. `load_dotenv()` is idempotent and never
# overwrites a pre-set value, so an explicit shell export always wins.
load_dotenv()

logger = logging.getLogger("telegram_adapter")


# === P0.2 — Structured logging with secret redaction (P0 of TELEGRAM_FEATURES_ROADMAP) ===
# Drop-in replacement for `logging.basicConfig` that:
#   1. Routes output to BOTH the console and a rotating file at
#      `data/logs/telegram_adapter.log` (10 MB × 5 backups).
#   2. Installs a `RedactingFilter` on every handler that scrubs bot
#      tokens, initData hashes, webhook `secret_token` headers/JSON
#      fields, and any explicit secrets registered through env vars.
#   3. Lets the verbosity be tuned at runtime via the `LOG_LEVEL` env
#      var (`DEBUG`, `INFO`, `WARNING`, `ERROR`; default `INFO`).
class RedactingFilter(logging.Filter):
    """Filter that scrubs secrets from log records before they reach handlers.

    Auto-discovers sensitive values from environment variables at log time
    so it works regardless of when the bot token / webhook secret is
    configured. Also matches well-known Bot API patterns (bot token format,
    initData ``hash=`` parameters, ``secret_token`` JSON fields, the
    ``X-Telegram-Bot-API-Secret-Token`` HTTP header) for defense in depth.
    """

    # Bot token format: 8-10 digits, colon, then 30+ alphanumeric chars
    _BOT_TOKEN_RE = re.compile(r"\b\d{8,10}:[A-Za-z0-9_-]{30,}\b")
    # initData `hash=...` query parameter (64 hex chars)
    _INITDATA_HASH_RE = re.compile(r"(hash=)[A-Fa-f0-9]{64}\b")
    # Webhook JSON body: "secret_token": "<value>"
    _SECRET_TOKEN_JSON_RE = re.compile(r'("secret_token"\s*:\s*)"[^"]+"')
    # HTTP header value
    _SECRET_HEADER_RE = re.compile(
        r"(X-Telegram-Bot-API-Secret-Token:\s*)([^\s,;\"']+)",
        re.IGNORECASE,
    )

    def filter(self, record: logging.LogRecord) -> bool:
        try:
            msg = record.getMessage()
        except Exception:
            # Never break logging because of a redaction bug.
            return True

        # Pattern-based scrubbing
        msg = self._BOT_TOKEN_RE.sub("[REDACTED_TOKEN]", msg)
        msg = self._INITDATA_HASH_RE.sub(r"\1[REDACTED_HASH]", msg)
        msg = self._SECRET_TOKEN_JSON_RE.sub(r'\1"[REDACTED]"', msg)
        msg = self._SECRET_HEADER_RE.sub(r"\1[REDACTED]", msg)

        # Explicit env-var secrets (auto-discovered at log time)
        for key in (
            "TELEGRAM_BOT_TOKEN",
            "TELEGRAM_WEBHOOK_SECRET",
            "NGROK_AUTH_TOKEN",
        ):
            value = os.getenv(key, "").strip("\"'")
            if value and len(value) >= 8:
                msg = msg.replace(value, "[REDACTED]")

        # Mutate the record so the formatter picks up the scrubbed text.
        record.msg = msg
        record.args = ()
        return True


def setup_logging():
    """Configure structured logging with redaction and rotating file output.

    Idempotent: re-invoking removes existing root handlers before adding
    new ones, so it is safe to call again from tests or after a runtime
    `LOG_LEVEL` change.
    """
    log_level_name = os.getenv("LOG_LEVEL", "INFO").upper()
    log_level = getattr(logging, log_level_name, None) or logging.INFO

    log_dir = "data/logs"
    os.makedirs(log_dir, exist_ok=True)
    log_file = os.path.join(log_dir, "telegram_adapter.log")

    fmt = "%(asctime)s - %(name)s - %(levelname)s - %(message)s"
    formatter = logging.Formatter(fmt)

    redactor = RedactingFilter()

    # Console handler (always)
    console = logging.StreamHandler()
    console.setFormatter(formatter)
    console.addFilter(redactor)

    # Rotating file handler (10 MB × 5 backups)
    file_handler = RotatingFileHandler(
        log_file,
        maxBytes=10 * 1024 * 1024,
        backupCount=5,
        encoding="utf-8",
    )
    file_handler.setFormatter(formatter)
    file_handler.addFilter(redactor)

    root = logging.getLogger()
    root.setLevel(log_level)
    # Remove any previously-installed handlers (e.g. from a prior basicConfig).
    for h in list(root.handlers):
        root.removeHandler(h)
    root.addHandler(console)
    root.addHandler(file_handler)

    # Quiet down chatty libraries; keep INFO+ for python-telegram-bot.
    logging.getLogger("httpx").setLevel(logging.WARNING)
    logging.getLogger("aiohttp.access").setLevel(logging.WARNING)


# Configure logging at import time so module-level errors (e.g. failed
# PageManager migrations, missing dependencies) land in the rotating log
# file with redaction instead of disappearing into lastResort-stderr.
setup_logging()

# === P0.1 — Settings dataclass (P0 of TELEGRAM_FEATURES_ROADMAP) ===
# Centralises the ~12 scattered `os.getenv` calls into a single typed
# configuration object. The module-level constants below are still the
# primary access pattern (for backward compatibility with the rest of
# the file) and are initialised from the dataclass in `_load_settings()`.
# New code should prefer `settings` over the bare constants so we have
# one canonical place to discover what is tunable.
@dataclass
class Settings:
    """Static, immutable runtime configuration for the Telegram adapter.

    The dataclass is frozen so the rest of the adapter can rely on these
    values never changing after startup. Anything truly dynamic (e.g.
    per-chat page routing) belongs in a different store.
    """
    bus_port: int = 5000
    webapp_url: str = ""
    ngrok_auth_token: str = ""
    miniapp_port: int = 5001
    permission_timeout_seconds: float = 300.0
    ws_session_timeout: float = 60.0
    # P12.7 — DRY_RUN short-circuits all outbound Telegram sends. Useful
    # for unit tests, CI smoke runs, and staging environments that should
    # never hit the real Bot API. Set TELEGRAM_DRY_RUN=1 to enable.
    dry_run: bool = False
    # P12.8 — when set, every outbound message that would normally have
    # a web-page preview attached (any text containing a URL) is sent
    # with `link_preview_options.is_disabled = True`. Saves bandwidth
    # and prevents accidental link-preview leakage in shared chats.
    disable_link_preview: bool = False
    # P7.1 — persistent quick-action keyboard. Set to True (default)
    # to send a ReplyKeyboardMarkup with Library / New Page / Soul /
    # Help buttons on /start. The keyboard can be hidden by
    # `/hidekeyboard` and reshown by `/start`.
    persistent_keyboard_enabled: bool = True
    # P9 — bot profile text. Override via env vars in production
    # (TELEGRAM_BOT_NAME / TELEGRAM_BOT_SHORT_DESC / TELEGRAM_BOT_DESC /
    # TELEGRAM_ADMIN_RIGHTS_FOR_CHANNELS=1).
    bot_name: str = "Hydragent"
    bot_short_description: str = "Local AI agent in your pocket."
    bot_description: str = (
        "🐉 Hydragent — your local AI agent. Reasoning, web search, "
        "long-term memory, file actions, and a knowledge graph. "
        "Send any message to begin; /start for the command list."
    )
    request_admin_rights_for_channels: bool = False
    # P2.2 — auto-acknowledge with a 👍 reaction when the final
    # response is delivered. Set TELEGRAM_REACT_ON_COMPLETE=0 to disable.
    react_on_complete: bool = True
    reaction_emoji: str = "👍"


def _coerce_bool(value: Optional[str], default: bool = False) -> bool:
    """Coerce an env-var string into a bool.

    Empty / unset → ``default``. Recognised truthy values: ``1``, ``true``,
    ``yes``, ``on`` (case-insensitive). Everything else is False.
    """
    if value is None:
        return default
    return value.strip().lower() in ("1", "true", "yes", "on")


def _load_settings() -> Settings:
    """Build the Settings dataclass from environment variables.

    Called once at import time. The dataclass is the source of truth
    for everything below this point — the legacy module-level
    constants are initialised from it for backward compatibility.
    """
    return Settings(
        bus_port=int(os.getenv("BUS_PORT", "5000")),
        webapp_url=os.getenv("TELEGRAM_WEBAPP_URL", "").strip("\"'"),
        ngrok_auth_token=os.getenv("NGROK_AUTH_TOKEN", "").strip("\"'"),
        miniapp_port=int(os.getenv("MINIAPP_PORT", "5001")),
        permission_timeout_seconds=float(os.getenv("PERMISSION_TIMEOUT_SECONDS", "300")),
        ws_session_timeout=float(os.getenv("WS_SESSION_TIMEOUT", "60")),
        dry_run=_coerce_bool(os.getenv("TELEGRAM_DRY_RUN"), default=False),
        disable_link_preview=_coerce_bool(
            os.getenv("TELEGRAM_DISABLE_LINK_PREVIEW"), default=False
        ),
        persistent_keyboard_enabled=_coerce_bool(
            os.getenv("TELEGRAM_PERSISTENT_KEYBOARD"), default=True
        ),
        bot_name=os.getenv("TELEGRAM_BOT_NAME", "Hydragent").strip("\"'") or "Hydragent",
        bot_short_description=os.getenv(
            "TELEGRAM_BOT_SHORT_DESC", "Local AI agent in your pocket."
        ).strip("\"'") or "Local AI agent in your pocket.",
        bot_description=os.getenv("TELEGRAM_BOT_DESC", Settings.bot_description).strip("\"'"),
        request_admin_rights_for_channels=_coerce_bool(
            os.getenv("TELEGRAM_ADMIN_RIGHTS_FOR_CHANNELS"), default=False
        ),
        react_on_complete=_coerce_bool(
            os.getenv("TELEGRAM_REACT_ON_COMPLETE"), default=True
        ),
        reaction_emoji=os.getenv("TELEGRAM_REACTION_EMOJI", "👍").strip() or "👍",
    )


# Singleton — read by every subsystem that needs the config.
settings = _load_settings()

# P12.7 — convenience constant for the "are we in dry-run mode?" check
# that runs on every outbound send. Inlined to avoid an attribute
# lookup on the hot path.
DRY_RUN = settings.dry_run

# Backward-compatible module-level constants. These are the
# values every other module already imports; we keep them so
# the existing call sites keep working without edits.
BUS_PORT = settings.bus_port
TELEGRAM_WEBAPP_URL = settings.webapp_url
NGROK_AUTH_TOKEN = settings.ngrok_auth_token
MINIAPP_PORT = settings.miniapp_port
# P0.5 — Permission request timeout (seconds). After this elapses with no
# Approve/Deny callback, the request is auto-denied and the bus is notified
# so the agent does not hang on a user who walked away. Default 300 s.
PERMISSION_TIMEOUT_SECONDS = settings.permission_timeout_seconds
# P0.7 — WebSocket dead-session timeout (seconds). A Mini App WebSocket
# that has not sent any inbound message for this long is considered dead
# and is closed by the periodic sweep. Default 60 s. Set to 0 or a
# negative value to disable the sweep entirely (not recommended in
# production).
WS_SESSION_TIMEOUT = settings.ws_session_timeout

# P12.8 — forwarded into the ``link_preview_options`` parameter of every
# outbound ``sendMessage`` / ``editMessageText`` call. When True,
# Telegram suppresses the page preview that would otherwise be rendered
# under any message containing a URL.
DISABLE_LINK_PREVIEW = settings.disable_link_preview

# Global variables for config and active permission futures
allowed_chats = set()
# P0.6 — three-tier chat access policy. Priority: blocked > readonly > allowed.
#   * blocked_chats : silently dropped (no reply, no signal that the bot exists).
#   * readonly_chats: can receive push notifications but cannot send messages
#                     or trigger callback actions.
#   * allowed_chats : full access.
# Both lists are populated from env vars (TELEGRAM_READONLY_CHAT_IDS,
# TELEGRAM_BLOCKED_CHAT_IDS) by `load_config()`. A chat appearing in BOTH
# `allowed_chats` and `blocked_chats` is treated as a misconfiguration and
# causes `load_config()` to hard-exit so the operator notices the conflict.
readonly_chats = set()
blocked_chats = set()
bot_token = ""
pending_permissions = {}
background_tasks = set()
# P0.7 — `active_websocket_sessions` was a `set[WebSocketResponse]`. It is
# now a `dict[WebSocketResponse, float]` mapping each live WebSocket to
# the unix timestamp of its most recent inbound message. `broadcast_to_webviews`
# and a periodic sweep drop entries whose `last_seen` is older than
# `WS_SESSION_TIMEOUT` so dead clients do not accumulate forever.
active_websocket_sessions = {}
telegram_app = None

# P0.3 — Health state surfaced via /health and /ready. Mutated by the
# push listener (bus_connected), by main_async on startup (bot_username,
# bot_id, started_at) and the permission timeout counter; read by
# handle_health / handle_ready and by /health responses.
health_status = {
    "bot_username": "",
    "bot_id": 0,
    "bus_connected": False,
    "started_at": 0.0,
    "permission_timeouts": 0,  # cumulative count of timed-out permission requests
}


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
    """Push a JSON-RPC notification to every live Mini App WebSocket.

    P0.7 — `active_websocket_sessions` is now a `dict[ws, last_seen]`.
    The function:
      1. Iterates over a snapshot of the keys so it is safe to mutate
         the dict while we are broadcasting.
      2. Skips entries whose `last_seen` is older than `WS_SESSION_TIMEOUT`
         (stale clients; the periodic sweep will close them).
      3. Schedules the send as a fire-and-forget task. The task removes
         its own session from the registry on failure so dead clients
         are dropped on the very next push (we cannot `await` from this
         sync function without blocking the bus-pump loop).

    P12.7 — short-circuits when DRY_RUN is enabled. The payload is
    logged at DEBUG level for assertions in tests; no tasks are
    scheduled and no WebSockets are touched.
    """
    if DRY_RUN:
        logger.debug(f"[DRY_RUN] broadcast_to_webviews skipped → {msg}")
        return
    data_str = json.dumps(msg)
    now = time.time()
    stale_threshold = (
        now - WS_SESSION_TIMEOUT if WS_SESSION_TIMEOUT > 0 else float("inf")
    )

    async def _send_and_cleanup(ws, payload):
        try:
            await ws.send_str(payload)
        except Exception as e:
            logger.debug(f"Failed to broadcast to webview ws; dropping session: {e}")
            active_websocket_sessions.pop(ws, None)

    for ws in list(active_websocket_sessions.keys()):
        last_seen = active_websocket_sessions.get(ws, 0.0)
        if last_seen < stale_threshold:
            # Skip stale clients; the sweep task will close them.
            continue
        try:
            asyncio.create_task(_send_and_cleanup(ws, data_str))
        except RuntimeError:
            # No running event loop (e.g. this was called from a sync
            # shutdown path). Nothing we can do without an awaitable
            # context; the sweep task will eventually close the dead
            # client anyway.
            logger.debug("broadcast_to_webviews called without a running loop; skipping push")


def load_config():
    global bot_token, allowed_chats, readonly_chats, blocked_chats
    bot_token = os.getenv("TELEGRAM_BOT_TOKEN", "").strip("\"'")
    if not bot_token:
        logger.error("TELEGRAM_BOT_TOKEN environment variable is missing or empty")
        sys.exit(1)

    def _parse_chat_list(env_value, label):
        """Parse a comma-separated chat-id env var into a `set[int]`.

        Returns an empty set when the value is empty/unset. A malformed
        entry triggers `sys.exit(1)` so the operator notices the typo
        before the bot starts serving traffic.
        """
        if not env_value:
            return set()
        try:
            return set(int(cid.strip()) for cid in env_value.split(",") if cid.strip())
        except ValueError as e:
            logger.error(f"Failed to parse {label} '{env_value}': {e}")
            sys.exit(1)

    allowed_chats = _parse_chat_list(
        os.getenv("TELEGRAM_ALLOWED_CHAT_IDS", "").strip("\"'"),
        "TELEGRAM_ALLOWED_CHAT_IDS",
    )
    # P0.6 — read-only and blocked lists. See the globals block for
    # the priority order. A chat present in BOTH allowed and blocked
    # is treated as a hard misconfiguration.
    readonly_chats = _parse_chat_list(
        os.getenv("TELEGRAM_READONLY_CHAT_IDS", "").strip("\"'"),
        "TELEGRAM_READONLY_CHAT_IDS",
    )
    blocked_chats = _parse_chat_list(
        os.getenv("TELEGRAM_BLOCKED_CHAT_IDS", "").strip("\"'"),
        "TELEGRAM_BLOCKED_CHAT_IDS",
    )

    conflicts = allowed_chats & blocked_chats
    if conflicts:
        logger.error(
            f"Chat(s) appear in BOTH allowed and blocked lists: {conflicts}. "
            "Remove them from one list before starting the adapter."
        )
        sys.exit(1)

    if not allowed_chats:
        logger.warning("TELEGRAM_ALLOWED_CHAT_IDS is empty. Nobody will be authorized to use the bot!")

    logger.info(
        f"Loaded config. Whitelisted chats: {allowed_chats}; "
        f"readonly: {readonly_chats}; blocked: {blocked_chats}"
    )


# === P0.6 — Three-tier chat access policy (P0 of TELEGRAM_FEATURES_ROADMAP) ===
# Resolves a chat_id to one of four policies:
#   - "blocked"   : silently drop the message / callback. Highest priority.
#   - "readonly"  : accept the message / callback but do not act on it.
#   - "allowed"   : normal handling.
#   - "denied"    : the chat is not on the allowlist at all.
# Priority is `blocked > readonly > allowed > denied`. A chat present in
# both `allowed_chats` and `blocked_chats` is treated as blocked because
# `load_config` refuses to start in that misconfiguration.
def get_chat_policy(chat_id: int) -> str:
    if chat_id in blocked_chats:
        return "blocked"
    if chat_id in readonly_chats:
        return "readonly"
    if chat_id in allowed_chats:
        return "allowed"
    return "denied"

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
    """Forward a user intent to the Event Bus and stream the agent response.

    Streaming strategy (Bot API 10.1, June 11 2026):
      1. While tokens arrive, push them to ``sendMessageDraft`` as an
         ephemeral 30-second preview. This avoids Telegram's per-chat
         edit-rate-limit and renders as a smooth in-place animation.
      2. The legacy edit-on-place loop runs in parallel as a safety net for
         chats where drafts are not honoured (e.g. group/supergroup chats).
      3. On completion, if the LLM output contains rich-only constructs
         (``# heading``, GFM tables, ``$$math$$``, ``<details>``, …) we
         delete the placeholder and persist the final text via
         ``sendRichMessage``. Otherwise we edit the placeholder in place
         using the safe MarkdownV2 helpers.
    """
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
    last_draft_text = ""
    edit_task = None
    stream_complete = False
    # Bot API 10.1 sendMessageDraft is only honoured in 1:1 (private) chats.
    # We probe with the first draft call and fall back to the legacy
    # edit-on-place loop if the API rejects the call.
    draft_supported = chat_id not in (None, "default-chat")
    # Stable draft id for this transaction — re-using the same id animates the
    # existing draft in place on the client side.
    draft_id = int(time.time() * 1000) & 0x7FFFFFFF

    async def update_telegram_message():
        nonlocal last_edit_time, edit_task, last_edited_text, last_draft_text, draft_supported
        while not stream_complete:
            await asyncio.sleep(0.8)
            now = time.time()

            # Try the new draft API first (Bot API 10.1) — it avoids Telegram's
            # edit-rate-limit and renders as a smooth in-place animation.
            # Empty text is now allowed and shows the native "Thinking…"
            # placeholder until real tokens arrive.
            if draft_supported and text_buffer != last_draft_text:
                try:
                    await send_message_draft(
                        context.bot,
                        chat_id,
                        draft_id,
                        text_buffer,  # "" → "Thinking…" placeholder; tokens → preview
                        parse_mode=None,  # drafts are streamed raw; final persist does escaping
                    )
                    last_draft_text = text_buffer
                except Exception as draft_err:
                    err = str(draft_err).lower()
                    if "chat not found" in err or "private" in err or "not supported" in err or "bad request" in err:
                        logger.debug(f"sendMessageDraft unavailable for chat {chat_id}: {draft_err}")
                        draft_supported = False
                    else:
                        logger.debug(f"sendMessageDraft transient error: {draft_err}")

            # Fallback: legacy edit-on-place with a formatted header
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

    # Bot API 10.1 (June 11 2026): a ``sendMessageDraft`` call with an empty
    # ``text`` field renders the client-side "Thinking…" placeholder. We
    # probe immediately (outside the 0.8 s editor-loop sleep) so the
    # placeholder appears as soon as we know the chat accepts drafts.
    # The periodic loop will then take over and push the streamed tokens.
    if draft_supported:
        try:
            await send_message_draft(
                context.bot, chat_id, draft_id, "", parse_mode=None,
            )
            last_draft_text = ""
        except Exception as probe_err:
            err = str(probe_err).lower()
            if "chat not found" in err or "private" in err or "not supported" in err or "bad request" in err:
                logger.debug(f"sendMessageDraft unavailable for chat {chat_id}: {probe_err}")
                draft_supported = False
            else:
                logger.debug(f"sendMessageDraft probe error: {probe_err}")

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
                    perm_msg = await safe_send_message(
                        context.bot, chat_id, text_content, parse_mode=PARSE_MODE,
                        reply_markup=reply_markup,
                    )
                except Exception as e:
                    logger.warning(f"safe_send_message failed for permission prompt: {e}. Retrying without Markdown.")
                    perm_msg = await context.bot.send_message(
                        chat_id=chat_id,
                        text=f"⚠️ Approval Required\n\nTool: {tool_id}\nAction: {summary}\n\nPlease approve or deny below:",
                        reply_markup=reply_markup
                    )

                # Create future to wait for button click. P0.5 wraps the
                # await with `asyncio.wait_for` so a user who walks away
                # cannot block the entire transaction forever; the default
                # 300-second window is configurable via
                # `PERMISSION_TIMEOUT_SECONDS`.
                fut = asyncio.get_running_loop().create_future()
                pending_permissions[req_id] = fut
                timeout_seconds = PERMISSION_TIMEOUT_SECONDS
                timed_out = False

                try:
                    try:
                        approved = await asyncio.wait_for(fut, timeout=timeout_seconds)
                    except asyncio.TimeoutError:
                        logger.warning(
                            f"Permission request {req_id} timed out after "
                            f"{timeout_seconds:.0f}s; auto-denying"
                        )
                        approved = False
                        timed_out = True
                        health_status["permission_timeouts"] += 1
                finally:
                    pending_permissions.pop(req_id, None)

                # Update permission prompt text to show action taken
                if timed_out:
                    status_text = escape_markdown(
                        f"⏱️ Timed out after {timeout_seconds:.0f}s (denied)"
                    )
                else:
                    status_text = "✅ Approved" if approved else "❌ Denied"
                try:
                    await safe_edit_message(
                        context.bot, chat_id, perm_msg.message_id,
                        f"⚠️ *Approval Required*\n\n*Tool:* `{escape_markdown(tool_id)}`\n*Action:* {escape_markdown(summary)}\n\n*Result:* {status_text}",
                    )
                except Exception as e:
                    logger.debug(f"Failed to update permission prompt text with MarkdownV2: {e}")
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

        # === Final response routing ===
        # If the LLM output contains rich-only constructs (headings, GFM tables,
        # math blocks, task lists, details blocks, …) we route through
        # `sendRichMessage` so the client renders the full structure. The
        # ephemeral "Thinking…" placeholder is deleted first so it doesn't
        # sit alongside the formatted response.
        final_text = text_buffer if text_buffer else "(No response content returned)"
        try:
            if _looks_like_rich_content(final_text):
                # Remove the placeholder spinner — the rich message is a new send.
                try:
                    await context.bot.delete_message(chat_id=chat_id, message_id=sent_msg.message_id)
                except Exception:
                    pass
                # safe_send_rich_message transparently falls back to a plain
                # MarkdownV2 sendMessage if the server rejects the rich payload.
                await safe_send_rich_message(context.bot, chat_id, markdown=final_text)
            else:
                # Plain MarkdownV2 path — reuse the placeholder to avoid an
                # extra "Message sent" notification on the user's device.
                try:
                    if final_text != last_edited_text:
                        await safe_edit_message(
                            context.bot, chat_id, sent_msg.message_id, final_text
                        )
                except Exception as e:
                    logger.debug(f"Final edit MarkdownV2 parse failure: {e}. Retrying in plain text.")
                    try:
                        await context.bot.edit_message_text(
                            chat_id=chat_id,
                            message_id=sent_msg.message_id,
                            text=final_text
                        )
                    except Exception as final_err:
                        logger.error(f"Failed to write final text: {final_err}")
        except Exception as outer_err:
            logger.error(f"Unhandled error in final-response routing: {outer_err}")

async def handle_message(update: Update, context: ContextTypes.DEFAULT_TYPE):
    msg = update.message if update.message else update.edited_message
    # Bot API 10.1: ``Message.rich_message`` is populated when the user sends
    # a structured Rich Message. In that case ``msg.text`` may be empty, so
    # we accept the message as long as *some* payload is present and walk
    # the blocks to recover a plain-text rendering for the agent bus.
    rich_message = getattr(msg, "rich_message", None) if msg else None
    if not msg or (not msg.text and not rich_message):
        return

    chat_id = update.effective_chat.id
    user_id = update.effective_user.id
    text = msg.text or _rich_message_to_text(rich_message)

    # P0.6 — three-tier access policy (blocked > readonly > allowed > denied).
    # Blocked users get NO reply (avoids information leakage); read-only
    # users get a one-line status update; denied users get the standard
    # "Unauthorized" message; only `allowed` chats fall through to the
    # normal handler logic below.
    policy = get_chat_policy(chat_id)
    if policy == "blocked":
        logger.warning(f"Dropped message from blocked chat_id: {chat_id}")
        return
    if policy == "readonly":
        logger.info(f"Read-only chat {chat_id} attempted to send message; ignoring")
        try:
            await msg.reply_text(
                "🔒 This chat is in read-only mode. You can receive "
                "notifications but cannot send new messages or trigger commands."
            )
        except Exception:
            pass
        return
    if policy == "denied":
        logger.warning(f"Blocked unauthorized request from chat_id: {chat_id}")
        await msg.reply_text("⛔ Unauthorized. You do not have permission to access this agent.")
        return
    # policy == "allowed" — continue with normal handling

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
    # P0.6 — apply the same chat policy to callback button presses so
    # blocked / read-only / denied chats cannot trigger approval flows
    # (or any other action) by re-pressing an old inline button.
    callback_chat_id = query.message.chat.id
    policy = get_chat_policy(callback_chat_id)
    if policy in ("blocked", "denied"):
        logger.warning(f"Dropped callback from {policy} chat_id: {callback_chat_id}")
        await query.answer()
        return
    if policy == "readonly":
        try:
            await query.answer("🔒 Read-only mode", show_alert=True)
        except Exception:
            pass
        return

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
        await safe_edit_message(
            context.bot,
            chat_id,
            query.message.message_id,
            f"🔄 *Page Context Switched\\!*\n\n"
            f"📂 Now talking in: *{escape_markdown(title)}*\n\n"
            f"Any new messages will load this Page's specific context.",
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
                await safe_edit_message(
                    context.bot, chat_id, sent_msg.message_id,
                    f"✅ Page compacted successfully!\n\n*New Summary:*\n{escape_markdown(summary)}",
                )
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
        await safe_edit_message(
            context.bot, chat_id, sent_msg.message_id,
            f"✅ Page compacted successfully!\n\n*New Summary:*\n{escape_markdown(summary)}",
        )
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
            # P0.3 — flag the bus as connected for the /ready probe. The
            # flag is reset to False in the `finally` below whenever the
            # outer loop reconnects.
            health_status["bus_connected"] = True
            logger.info("Push listener connected to Event Bus.")
            
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
                                await safe_send_message(
                                    app.bot, chat_id,
                                    f"📢 *Notification*\n\n{escape_markdown(content)}",
                                    parse_mode=PARSE_MODE,
                                )
                            except Exception as e:
                                logger.error(f"Failed to forward push message to chat {chat_id}: {e}")
                except Exception as e:
                    logger.error(f"Error parsing push message: {e}")
            
            await bus.close()
        except Exception as e:
            logger.error(f"Push listener failed: {e}. Retrying in 5 seconds...")
        finally:
            # P0.3 — /ready reports `bus_connected: false` while we are
            # between reconnect attempts.
            health_status["bus_connected"] = False

        await asyncio.sleep(5)

# Aiohttp web server handlers for Mini App
def no_cache_response(file_path):
    headers = {
        "Cache-Control": "no-store, no-cache, must-revalidate, max-age=0",
        "Pragma": "no-cache",
        "Expires": "0"
    }
    return web.FileResponse(file_path, headers=headers)

def _get_miniapp_path():
    # 1. Check HYDRAGENT_HOME
    if "HYDRAGENT_HOME" in os.environ:
        path = os.path.join(os.environ["HYDRAGENT_HOME"], "miniapp")
        if os.path.exists(path):
            return path
    # 2. Check ~/.hydragent/miniapp
    home = os.path.expanduser("~")
    path = os.path.join(home, ".hydragent", "miniapp")
    if os.path.exists(path):
        return path
    # 3. Local fallback
    return os.path.abspath(os.path.join(os.path.dirname(__file__), "miniapp"))

async def handle_index(request):
    return no_cache_response(os.path.join(_get_miniapp_path(), "index.html"))

async def handle_style(request):
    return no_cache_response(os.path.join(_get_miniapp_path(), "style.css"))

async def handle_app(request):
    return no_cache_response(os.path.join(_get_miniapp_path(), "app.js"))

def _get_graph_path():
    # 1. Check HYDRAGENT_HOME
    if "HYDRAGENT_HOME" in os.environ:
        path = os.path.join(os.environ["HYDRAGENT_HOME"], "data", "graph.html")
        if os.path.exists(path):
            return path
    # 2. Check ~/.hydragent/data/graph.html
    home = os.path.expanduser("~")
    path = os.path.join(home, ".hydragent", "data", "graph.html")
    if os.path.exists(path):
        return path
    # 3. Local fallback
    return os.path.abspath(os.path.join(os.path.dirname(__file__), "miniapp", "graph.html"))

async def handle_graph(request):
    try:
        generate_graph()
    except Exception as e:
        logger.error(f"Failed to generate graph dynamically: {e}")
    return no_cache_response(_get_graph_path())


# === P0.3 — Healthcheck endpoints (P0 of TELEGRAM_FEATURES_ROADMAP) ===
# Exposed on the same aiohttp server as the Mini App. Two endpoints:
#   GET /health  — liveness probe; always returns 200 if the process is up
#   GET /ready   — readiness probe; returns 200 only when the bot is
#                  registered with Telegram AND the push-notification bus
#                  is connected. Returns 503 otherwise.
# The split mirrors Kubernetes / Docker Swarm probe semantics so this
# adapter can be orchestrated without code changes.
async def handle_health(request):
    """Liveness probe: the process is up and the adapter is serving HTTP."""
    uptime = (
        time.time() - health_status["started_at"]
        if health_status["started_at"]
        else 0
    )
    return web.json_response({
        "ok": True,
        "bot": {
            "username": health_status["bot_username"],
            "id": health_status["bot_id"],
        },
        "bus_connected": health_status["bus_connected"],
        "uptime_seconds": round(uptime, 1),
        "permission_timeouts": health_status["permission_timeouts"],
    })


async def handle_ready(request):
    """Readiness probe: 200 only when both Telegram and the bus are wired up."""
    if not health_status["bot_username"]:
        return web.json_response(
            {"ok": False, "reason": "bot not yet registered with Telegram"},
            status=503,
        )
    if not health_status["bus_connected"]:
        return web.json_response(
            {"ok": False, "reason": "event bus push listener not connected"},
            status=503,
        )
    return web.json_response({"ok": True})


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
    # P0.7 — record the initial `last_seen` timestamp so the periodic
    # sweep can age this session out if the client never sends a ping.
    active_websocket_sessions[ws] = time.time()

    try:
        async for msg in ws:
            # P0.7 — refresh `last_seen` on EVERY inbound frame. A
            # healthy Mini App sends a heartbeat / RPC every few seconds;
            # a dead one stops sending entirely and is reaped by the
            # sweep task in start_web_server.
            active_websocket_sessions[ws] = time.time()
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
                                    text = (
                                        f"🔄 *Page Context Switched\\!*\n\n"
                                        f"📄 Now talking in: *{escaped_title}*\n"
                                        f"🆔 Page ID: `{escaped_room_id}`\n\n"
                                        f"💬 Send a message below to continue writing on this Page."
                                    )
                                    asyncio.create_task(safe_send_message(
                                        telegram_app.bot, target_chat_id, text, parse_mode=PARSE_MODE
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
        # P0.7 — drop the session from the registry so future broadcasts
        # do not attempt to send on a closed socket. The periodic sweep
        # would catch it eventually; doing it eagerly avoids one round
        # of log spam.
        active_websocket_sessions.pop(ws, None)
        logger.info(f"WebSocket session closed for chat_id: {chat_id}")

    return ws


async def _websocket_dead_session_sweeper():
    """Periodically close WebSocket sessions that have gone silent.

    P0.7 — runs forever, sleeping for half of `WS_SESSION_TIMEOUT`
    between sweeps. Sessions whose `last_seen` is older than the
    timeout are closed with status code 1000 (normal closure) and
    removed from `active_websocket_sessions`. Disabled when the
    timeout is <= 0.
    """
    if WS_SESSION_TIMEOUT <= 0:
        logger.info("WS dead-session sweep disabled (WS_SESSION_TIMEOUT <= 0)")
        return
    sleep_for = max(5.0, WS_SESSION_TIMEOUT / 2.0)
    while True:
        try:
            await asyncio.sleep(sleep_for)
            now = time.time()
            stale = [
                ws
                for ws, last_seen in active_websocket_sessions.items()
                if now - last_seen > WS_SESSION_TIMEOUT
            ]
            for ws in stale:
                last_seen = active_websocket_sessions.pop(ws, None)
                idle = now - last_seen if last_seen else 0.0
                logger.info(
                    f"Closing dead WebSocket (idle for {idle:.0f}s, "
                    f"threshold {WS_SESSION_TIMEOUT:.0f}s)"
                )
                try:
                    await ws.close(code=1000)
                except Exception as e:
                    logger.debug(f"Dead-session close failed: {e}")
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.error(f"WebSocket sweeper iteration failed: {e}")


async def start_web_server():
    app_server = web.Application()
    app_server.router.add_get("/", handle_index)
    app_server.router.add_get("/index.html", handle_index)
    app_server.router.add_get("/style.css", handle_style)
    app_server.router.add_get("/app.js", handle_app)
    app_server.router.add_get("/graph.html", handle_graph)
    # P0.3 — healthcheck routes (liveness + readiness). See
    # handle_health / handle_ready. Both are un-authenticated by design
    # so orchestrators (k8s, Docker, systemd watchdog) can probe them
    # without sharing the bot token.
    app_server.router.add_get("/health", handle_health)
    app_server.router.add_get("/ready", handle_ready)
    app_server.router.add_get("/ws", websocket_handler)

    runner = web.AppRunner(app_server)
    await runner.setup()
    site = web.TCPSite(runner, "0.0.0.0", MINIAPP_PORT)
    await site.start()
    logger.info(f"Serving Mini App at http://localhost:{MINIAPP_PORT}")

    # P0.7 — launch the dead-session sweeper as a fire-and-forget
    # background task on the same loop as the web server. The task is
    # returned in a small "cleanup bag" so main_async can cancel it
    # on shutdown alongside `web_runner.cleanup()`.
    sweeper = asyncio.create_task(_websocket_dead_session_sweeper())
    return _WebServerHandles(runner=runner, sweeper=sweeper)


class _WebServerHandles:
    """Bundle the aiohttp runner and the P0.7 sweeper task for shutdown."""

    def __init__(self, runner, sweeper):
        self.runner = runner
        self.sweeper = sweeper

    async def cleanup(self):
        try:
            self.sweeper.cancel()
        except Exception:
            pass
        try:
            await self.runner.cleanup()
        except Exception as e:
            logger.debug(f"Failed to cleanup web runner: {e}")

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

# === P0.4 — Lifecycle hooks (P0 of TELEGRAM_FEATURES_ROADMAP) ===
# `post_init` runs after `Application.initialize()` has wired up the bot
# and the update queue but before polling starts. We use it to populate
# the health-check state with the bot's identity and the process start
# time so the very first `/health` probe returns useful data instead of
# empty strings.
#
# `post_stop` runs after polling has stopped and the update queue is
# drained. It is the right place to:
#   * Auto-deny any permission requests that are still in flight, so the
#     bus does not hang on a future that will never be resolved.
#   * Flush the in-memory active-pages map to disk so a SIGTERM during
#     a user switch does not lose the "which chat is talking to which
#     page" state.
#   * Log a clear "shutting down" line so operators can correlate the
#     graceful exit with the rest of the log.
async def post_init(app: Application):
    """Populate health-check state before polling starts."""
    health_status["started_at"] = time.time()
    try:
        bot_me = await app.bot.get_me()
        health_status["bot_username"] = bot_me.username or ""
        health_status["bot_id"] = bot_me.id
        logger.info(
            f"Bot identity resolved: @{health_status['bot_username']} "
            f"(id={health_status['bot_id']})"
        )
    except Exception as me_err:
        logger.warning(f"Failed to resolve bot identity for /health: {me_err}")
    # Reflect the bus listener state too. It is still False at this
    # point — the listener opens its socket in a background task — but
    # we set started_at now so `/health` can compute uptime immediately.
    logger.info("Telegram adapter post_init complete; polling not yet started.")


async def post_stop(app: Application):
    """Graceful-shutdown handler wired to `Application.post_stop`."""
    logger.info("Telegram adapter post_stop invoked; flushing state...")

    # Auto-deny any permission futures that are still waiting on a
    # human Approve/Deny click. Using `False` (deny) is the
    # fail-closed choice: agents never run a tool the user did not
    # explicitly approve just because the adapter is shutting down.
    if pending_permissions:
        logger.warning(
            f"Auto-denying {len(pending_permissions)} in-flight permission "
            "request(s) on shutdown"
        )
        for req_id, fut in list(pending_permissions.items()):
            if not fut.done():
                fut.set_result(False)
        pending_permissions.clear()

    # Flush the active-pages map so the next start picks up the
    # "chat -> active page" routing without losing the in-memory
    # write that may have happened milliseconds before SIGTERM.
    try:
        page_manager.save_active_pages()
    except Exception as e:
        logger.error(f"Failed to save active pages during shutdown: {e}")

    logger.info("Telegram adapter post_stop complete.")


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

    # Build the Application. P0.4 wires post_init / post_stop so the
    # bot identity is captured in health_status BEFORE the first poll
    # and graceful-shutdown logic runs AFTER polling stops. The inline
    # get_me() call that used to live here was deleted because
    # post_init now owns that responsibility.
    app = (
        Application.builder()
        .token(bot_token)
        .post_init(post_init)
        .post_stop(post_stop)
        .build()
    )
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
