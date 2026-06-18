# Telegram Adapter — Feature Roadmap

> **Status:** Living document
> **Scope:** `adapters/telegram_adapter.py` + `adapters/miniapp/*` + companion helpers
> **Target Bot API:** 10.1 (June 11 2026) + `python-telegram-bot` v21+
> **Last updated:** 2026-06-16 — P0 foundation block complete (items 0.2–0.7)

This document is the implementation playbook for pushing the Hydragent Telegram adapter from "text-only chat agent" to "full-featured Telegram-native experience". Features are ordered by *implementation dependency* (foundation first, polish last) and by *value-to-risk ratio* within each phase.

---

## 0. Current State (audit snapshot)

A full audit lives in `audit_telegram_adapter.md` (see session resources). Highlights:

| Area | Status |
|------|--------|
| **Text / MarkdownV2** | ✅ solid; full escape + safe fallbacks |
| **Rich Message sending** (sendRichMessage, sendMessageDraft, sendRichMessageDraft, RichBlockThinking) | ✅ implemented in this conversation |
| **Inline keyboards** | ✅ permissions, page switch, library, summary |
| **Bot commands** | ✅ 18 commands globally registered |
| **Web App menu button** | ✅ `MenuButtonWebApp` + `ngrok` tunnel |
| **Mini App** | 🟡 static HTML (`index.html`, `app.js`, `style.css`, `graph.html`); no SDK init, no theme, no storage |
| **Mini App server** | ✅ aiohttp on `MINIAPP_PORT`; serves static + WebSocket |
| **Event Bus integration** | ✅ `intent.submit` / `response.token` / `response.permission_request` / `gateway.push` |
| **Permission approval** | ✅ `auth_approve:` / `auth_deny:` callbacks, but **no timeout** |
| **Push notifications** | ✅ long-lived bus listener, 5 s reconnect |
| **Per-chat / per-page state** | ✅ `PageManager` (SQLite) + `telegram_active_pages.json` |
| **Group chat support** | 🟡 mention-only; no admin / topic / moderation features |
| **Media (photo, voice, document, video, sticker, location)** | ❌ not handled |
| **Inline mode** | ❌ not enabled |
| **Reactions** | ❌ not read or sent |
| **Forum topics** | ❌ no `is_topic_message` routing |
| **Webhook mode** | ❌ polling only |
| **Polls / quizzes / checklists / dice** | ❌ not used |
| **Payments (Stars) / Gifts** | ❌ not used |
| **Profile / chat customization** | 🟡 basic menu button; no per-chat profile, descriptions, default admin rights |
| **Telegram Passport** | ❌ not used (probably never will) |
| **Business accounts** | ❌ not used |
| **Suggested posts (channels)** | ❌ not used |
| **Channel broadcast / paid broadcast** | ❌ not used |
| **Story sharing from Mini App** | ❌ not used |
| **i18n / locale** | ❌ English only |
| **Persistent keyboards / force reply** | 🟡 `ForceReply` shown for re-reasoning; no persistent menus |
| **ConversationHandler for stateful flows** | ❌ uses ad-hoc `context.user_data` flags instead |
| **JobQueue for scheduled tasks** | ❌ no cron, no reminders |
| **Healthcheck endpoint** | 🟡 no `/health` on web server |
| **Database connection pooling** | ❌ opens a new SQLite connection per call (fine for now, but doesn't scale) |
| **Log redaction / token scrub** | ❌ bot token may leak into logs |
| **i18n (multi-language replies)** | ❌ hard-coded English |
| **Voice / video notes** | ❌ not handled |
| **Contact / location / venue** | ❌ not handled |
| **Scheduled messages** | ❌ not used |
| **Link preview customization** | ❌ default only |
| **Custom emoji as status / reactions** | 🟡 `RichBlockThinking` can take custom emoji text but not `custom_emoji_id` |
| **Animation / live photo / paid media** | ❌ not used |
| **Voice transcription (Whisper)** | ❌ not wired |
| **Image description (VLM)** | ❌ not wired |
| **PDF / document text extraction** | ❌ not wired |

---

## 1. Design principles for the roadmap

1. **Foundation before features.** Defensive wrappers, error handling, structured logging, and config come first — every later feature benefits.
2. **Each phase must ship a usable end-user improvement.** No phase is purely internal.
3. **Feature flags via env vars** where there is risk (`TELEGRAM_ENABLE_*`). Default ON for safe features, OFF for opt-in ones.
4. **Telegram-side state, not adapter state.** Prefer Telegram UI (keyboards, reactions) over adapter-side menus.
5. **Mini App is a first-class citizen**, not a dashboard. Every phase should consider whether the Mini App should grow alongside.
6. **python-telegram-bot idioms first.** Use the library's `Application`, `JobQueue`, `Persistence`, `ConversationHandler`. Only fall back to `_do_api` for genuinely new Bot API 10.1 methods.
7. **Backwards compatibility.** All current commands, callbacks, and SQLite schemas must keep working.
8. **One pull request per phase.** Each phase below is sized to land as a single reviewable change.

---

## 2. Phase plan (sequential)

```
P0 ── Foundation (1-2 days)
P1 ── Multi-modal input (2-3 days)
P2 ── Reactions & feedback (1 day)
P3 ── Inline mode (1-2 days)
P4 ── Forum topics & group admin (1-2 days)
P5 ── Mini App v2 (3-5 days)
P6 ── Webhook production deployment (1-2 days)
P7 ── Persistent keyboards & settings (1-2 days)
P8 ── Polls, dice, checklists (0.5 day)
P9 ── Profile & bot customisation (0.5 day)
P10 ─ Payments (Stars), gifts, subscriptions (1-2 days)
P11 ─ Business accounts, suggested posts, stories (1 day)
P12 ─ Cross-cutting: i18n, observability, hardening (1-2 days)
```

Total estimate: **~3-4 weeks** of focused work. Phases can ship in any order if priorities shift, but the recommended sequence minimises risk of needing to refactor earlier work.

---

## Phase 0 — Foundation (ship first)

> **Goal:** Make the adapter harder to break, easier to debug, and easier to extend.
> **Complexity:** S
> **Touch points:** `telegram_adapter.py` (top-level config + helper section), `data/` schemas, `miniapp/` healthcheck

### 0.1  Centralised configuration module
- Move all `os.getenv(...)` reads into a `Settings` dataclass with defaults, validation, and `.env` documentation.
- Reject startup on missing required vars (`TELEGRAM_BOT_TOKEN`, `BUS_PORT`).
- Surface all settings via a `/status` command in the bot (admin-only).
- **Why:** Every later phase adds config; doing it now keeps the diff small.

### 0.2  Structured logging + token redaction
> **Status:** ✅ DONE 2026-06-16
- Replace `logging.basicConfig` with a JSON-friendly formatter.
- Add a `RedactingFilter` that scrubs bot tokens, HMAC secrets, and `initData` hashes.
- Log to `data/logs/telegram_adapter.log` with rotation (10 MB × 5 files).
- Add a `get_logger(__name__)` helper that respects a `LOG_LEVEL` env var.
- **Why:** Without this, every incident in production becomes a guessing game.

### 0.3  Healthcheck endpoint
> **Status:** ✅ DONE 2026-06-16
- Add `GET /health` to the aiohttp web server: returns `200 {"ok": true, "bot": "<username>", "bus": "connected|disconnected"}`.
- `GET /ready` returns `200` only when both bot and bus are connected.
- **Why:** Required for P6 (webhook deploy) and any container orchestration.

### 0.4  `Application` post-init / post-stop hooks
> **Status:** ✅ DONE 2026-06-16
- Register `post_init` and `post_stop` callbacks to:
  - Set `set_my_commands`, `set_chat_menu_button`, `set_my_name`, `set_my_short_description` on startup.
  - Delete webhook on stop, save active pages on stop.
  - Close all `pending_permissions` futures cleanly on shutdown.
- **Why:** Prevents zombie futures, ensures clean restarts.

### 0.5  Permission request timeout
> **Status:** ✅ DONE 2026-06-16
- Wrap the `await fut` in `send_intent_to_bus` with `asyncio.wait_for(fut, timeout=300)`.
- On timeout, send `permission.respond { approved: false, reason: "timeout" }` to the bus and continue.
- **Why:** The audit found an unbounded `await fut` — a single user who walks away blocks the whole transaction forever.

### 0.6  Refuse-message policy on chat_id
> **Status:** ✅ DONE 2026-06-16
- Split `allowed_chats` whitelist into:
  - `ALLOWED_CHATS` (default): full functionality
  - `READONLY_CHATS`: can receive but not act
  - `BLOCKED_CHATS`: silently drop
- **Why:** Lets admins share announcements to "everyone" without granting the bot write access to broadcast groups.

### 0.7  WebSocket dead-session cleanup
> **Status:** ✅ DONE 2026-06-16
- Track `last_seen` timestamp on each `active_websocket_sessions` entry.
- Drop sessions older than 60 s with no ping in `broadcast_to_webviews`.
- **Why:** The audit flagged dead-session detection as missing.

### 0.8  Schema migrations
- Move all `CREATE TABLE IF NOT EXISTS ...` calls into `migrations/` SQL files, versioned.
- Track applied versions in a `schema_version` table.
- **Why:** Required before P1 (multi-modal adds new tables) and P5 (miniapp v2 adds more state).

### 0.9  Unit-test scaffolding
- Add `tests/test_telegram_adapter.py` with `pytest-asyncio` for the pure helpers:
  - `escape_markdown` (MarkdownV2 spec coverage)
  - `_looks_like_rich_content` (true positive / true negative)
  - `_rich_message_to_text` (recursive RichText)
  - `validate_init_data` (HMAC)
- **Why:** Locks in current behaviour before refactoring.

---

## Phase 1 — Multi-modal input (high user value)

> **Goal:** Accept voice, photos, documents, stickers, locations, contacts, and video notes; transcribe / describe / extract where useful; pipe into the same intent flow.
> **Complexity:** M
> **Touch points:** `handle_message`, new `media/` subpackage, optional local Whisper / VLM sidecar

### 1.1  Voice & audio transcription
- Add a `MessageHandler(filters.VOICE | filters.AUDIO, ...)` that:
  1. Downloads the file (max 20 MB / 50 MB).
  2. Pipes it to a local Whisper sidecar (HTTP `POST /transcribe` on `WHISPER_PORT`, default 5050).
  3. Treats the transcript as the user text and sends to the bus.
  4. Replies with a small 🎙️ sticker (or a typed status) showing bytes / duration.
- **Why:** Voice is the #1 user request for a hands-free AI agent.

### 1.2  Photo description (vision)
- `MessageHandler(filters.PHOTO, ...)` — download the highest-resolution variant, send to a local VLM sidecar (`VLM_PORT`, default 5051).
- Vision caption becomes the text; original image is added as an attachment metadata entry (`attachments[0] = { type: "image", path, caption }`).
- The agent bus can decide whether to re-describe, OCR, or pass through.

### 1.3  Document text extraction
- `MessageHandler(filters.Document.ALL, ...)` — route by MIME:
  - `application/pdf` → `pdfplumber` or `pypdf` extract
  - `text/*` → decode UTF-8
  - other → base64 attach and pass to bus
- For files larger than the 20 MB Bot API limit, return a friendly "too large" message.

### 1.4  Video / video note
- Download the file, run a short captioning pass (e.g. first-frame VLM + duration metadata), send the caption to the bus.
- The video itself is stored locally and referenced by `file_id` for the agent to re-access via `getFile`.

### 1.5  Sticker interpretation
- Read `sticker.emoji`, `sticker.set_name`, and `is_animated` / `is_video`.
- Send the emoji + set name as a tiny natural-language message ("Sticker: 🔥 from set NightVibes") so the LLM has *some* signal.
- If `is_custom_emoji` (`sticker.type == "custom_emoji"`), look up the emoji ID via `getCustomEmojiStickers` and pass the `custom_emoji_id` along.

### 1.6  Location & venue
- `MessageHandler(filters.LOCATION | filters.VENUE, ...)`: format as `📍 <lat>, <lng> (±<accuracy>m)` for live locations and `🏛 <title> at <address>` for venues.

### 1.7  Contact
- `MessageHandler(filters.CONTACT, ...)`: format as `👤 <first> <last>, 📞 <phone>, 💬 <vcard_excerpt>`. Only used if user explicitly shares — no auto-fetching.

### 1.8  Reply-to-message threading
- When `update.message.reply_to_message` is set, prepend the quoted message to the content sent to the bus:
  ```
  [Replying to @user (msg_id 12345)]:
  > <quoted text>
  ---
  <new text>
  ```
- **Why:** Lets the agent see conversational context without the user re-typing.

### 1.9  Forwarded-message origin
- When `forward_origin` is present, include a `[Forwarded from <origin>]` line so the LLM can weigh source reliability.

### 1.10  Live location & live photo (new types)
- `live_period` and `live_photo`: pass through the first sample, mark as `streaming: true` in attachments.

---

## Phase 2 — Reactions & feedback (lightweight)

> **Goal:** Read incoming reactions, send outgoing reactions for ack, and let users "react to confirm" without typing.
> **Complexity:** S
> **Touch points:** `MessageReactionHandler` registration, status messages

### 2.1  Listen to reactions
- `MessageReactionHandler` updates `MessageReactionUpdated`. Log the emoji (or `custom_emoji_id`) + user.
- Optionally trigger a permission auto-approve on a designated reaction (e.g. user reacts with 👍 → approve the pending request).

### 2.2  Acknowledge with a reaction
- After persisting the final response, call `setMessageReaction(chat_id, message_id, [ReactionTypeEmoji("👀")])` so the user sees the bot "saw" it.
- Switch to a different emoji based on confidence / length (`🤔` for drafts, `🧠` for thinking, `✅` for complete).

### 2.3  Reaction-based menu
- The miniapp/dashboard can pin a "react with emoji to choose" message. E.g. send `Choose: 🅰 🅱 🅲` and let reactions act as buttons — useful for very long option lists where inline keyboards overflow.

### 2.4  Paid reactions (premium)
- If user is `is_premium`, accept paid reactions and treat them as "thank you" signals; surface in `user_insights`.

---

## Phase 3 — Inline mode

> **Goal:** Make the bot discoverable from any chat via `@Hydragent query…` and return useful inline results (article, photo, link, share-as-rich).
> **Complexity:** S-M
> **Touch points:** `Application.bot_data["inline_results"]` cache, `InlineQueryHandler`, `ChosenInlineResultHandler`, `answerInlineQuery`

### 3.1  Enable inline mode
- Document BotFather `/setinline` step in `README.md`.
- Register `InlineQueryHandler` and `ChosenInlineResultHandler`.

### 3.2  Article results (text)
- On `@Hydragent <query>`, return up to 50 `InlineQueryResultArticle` results, ranked by relevance.
- "Hydragent" article: send the same streaming response you would in private chat, but using the **inline** pathway — i.e. return a `PreparedInlineMessage` via `savePreparedInlineMessage` and let the user pick the chat.

### 3.3  Mini App inline result
- Use `InlineQueryResultsButton(text="Open editor", web_app=WebAppInfo(url=...))` to surface the Mini App in inline pickers.

### 3.4  Share-to-story from inline
- For article results with images, attach a `switch_inline_query` button to share to a story.

### 3.5  Inline caching
- Use `cache_time=300` (5 min) for stable queries; 0 for time-sensitive ones.

---

## Phase 4 — Forum topics & group admin

> **Goal:** Treat each forum topic as an isolated "page"; support group moderation primitives.
> **Complexity:** M
> **Touch points:** `handle_message`, `is_topic_message` routing, `create_forum_topic`, `close_forum_topic`

### 4.1  Topic-as-page routing
- When `update.message.is_topic_message` is true, key the active page by `(chat_id, message_thread_id)` instead of `chat_id` alone.
- The first message in a topic that has no `page_meta` row auto-creates a page titled from the topic name.

### 4.2  Topic commands
- `/topic_create <name>` → `create_forum_topic`; reply with the new `message_thread_id` link.
- `/topic_close`, `/topic_reopen`, `/topic_delete`, `/topic_rename`.
- `/topic_pin` to pin the current message in the topic.

### 4.3  Per-topic SOUL.md override
- A topic can have a `topic_soul` text in `page_meta` that is appended to the system prompt when the user is in that topic.

### 4.4  Group admin primitives
- `/kick <user>`, `/ban <user>`, `/mute <user> [duration]`, `/unmute <user>` — all gated by `can_restrict_members` on the bot.
- `/pin`, `/unpin`, `/unpinall` — gated by `can_pin_messages`.
- `/settitle`, `/setdescription` — gated by `can_change_info`.

### 4.5  Anti-spam in groups
- Rate-limit non-admin messages to e.g. 5 per 10 s per user.
- If the bot is admin, auto-delete obvious URL spam (configurable regex).

### 4.6  Channel broadcast mode
- If the chat is a channel and the bot is admin, expose `/broadcast <message>` that fans out to the channel subscriber list, optionally via `allow_paid_broadcast` for premium throughput.

---

## Phase 5 — Mini App v2

> **Goal:** Replace the static `miniapp/` with a full SPA that uses the Web App SDK, persistent storage, native theme, and rich interactions.
> **Complexity:** L
> **Touch points:** entire `miniapp/` directory, `miniapp/app.js` rewrite, new SDK loader, server-side initData validation

### 5.1  Modern SDK + tooling
- Replace vanilla JS with TypeScript + Vite + `@twa-dev/sdk` (or `@telegram-apps/sdk`).
- Build outputs to `miniapp/dist/`; serve from there.
- Add `miniapp/package.json` with build / lint / typecheck scripts.

### 5.2  Theme + safe area
- Read `Telegram.WebApp.colorScheme` and `themeParams` on load; subscribe to `themeChanged` and re-render.
- Apply CSS variables for all `--tg-theme-*` keys; respect `safeAreaInset` and `contentSafeAreaInset`.

### 5.3  Multi-page navigation
- Implement a router (e.g. `navigo`, `vaadin-router`, or hand-rolled) with:
  - `/` — Home (recent pages, quick actions)
  - `/page/:id` — Page detail (chat timeline + graph)
  - `/library` — Knowledge graph (D3.js / Cytoscape.js)
  - `/settings` — Bot settings, theme, storage
  - `/about` — Help / version

### 5.4  Main button + back button
- `MainButton` is the primary action on each page (e.g. "Send", "Save", "Compact").
- `BackButton` shows on non-home routes; pushes the previous route on click.
- `SettingsButton` opens the settings page from the Telegram context menu.

### 5.5  CloudStorage for state
- Save per-user preferences (theme override, default page, recent queries) via `CloudStorage`.
- Mirror to `DeviceStorage` for offline cache.
- Optional `SecureStorage` for sensitive values (API keys, encrypted notes).

### 5.6  Interactive graph
- Replace the static `graph.html` with a Cytoscape.js graph.
- Nodes are library nodes (`shelf`, `book`, `page`); edges are relations.
- Click a node → side panel with metadata + "Open in chat" button (deep link with `startapp`).
- Export → `shareToStory(media_url, { widget_link })` from the Web App.

### 5.7  Server-side initData validation
- Move the WebSocket handler's chat resolution off `initDataUnsafe` to a **server-side HMAC check**:
  - Compute `secret_key = HMAC_SHA256(bot_token, "WebAppData")`
  - Recompute `hash` from sorted params; reject mismatches with 401.
- Reject stale `auth_date` (> 5 min).

### 5.8  Fullscreen + orientation
- Add a "Present" button that calls `requestFullscreen()` for graph view.
- `lockOrientation()` while in fullscreen for landscape graph exploration.

### 5.9  Haptic feedback
- Wire `HapticFeedback.impactOccurred("light")` to button taps, `.notificationOccurred("success")` on save, `.selectionChanged()` on tab switch.

### 5.10  QR scanner for sharing
- "Share page" → `showScanQrPopup({ text: "Scan QR to open this page on another device" })`.
- Encode a `https://t.me/<bot>?startapp=page-<uuid>` deep link.

### 5.11  BiometricManager for sensitive settings
- Gate "export all data" behind a `BiometricManager.authenticate()` call.

### 5.12  Home screen shortcut
- On first successful login, call `addToHomeScreen()` (with a `show_alert`).

---

## Phase 6 — Webhook production deployment

> **Goal:** Run the bot in webhook mode for production stability and lower latency; keep polling for local dev.
> **Complexity:** M
> **Touch points:** aiohttp server, new `webhook.py` entrypoint, secret token validation

### 6.1  Webhook entrypoint
- Add a new script `adapters/telegram_webhook.py` that:
  - Builds the `Application`
  - Sets `bot.set_webhook(url=WEBHOOK_URL, secret_token=SECRET_TOKEN, allowed_updates=...)` at startup
  - Mounts `application.update_queue` listener onto aiohttp at `POST /telegram/<secret>`
  - Validates the `X-Telegram-Bot-API-Secret-Token` header

### 6.2  Local Bot API server (optional)
- For files > 20 MB, optionally point `TELEGRAM_API_BASE` at a self-hosted `tdlib/telegram-bot-api` instance (supports 2000 MB uploads, local file paths).

### 6.3  Drop-pending-upgrades option
- `set_webhook(..., drop_pending_updates=True)` on first deploy to avoid replay storms.

### 6.4  Healthchecks
- Already in P0.3. Webhook also adds `GET /webhook_info` returning a JSON dump of `bot.get_webhook_info()` for monitoring.

### 6.5  Restart-safe connection
- On boot, if `get_webhook_info().url == WEBHOOK_URL`, do nothing; if mismatched, call `set_webhook`; if empty, fall back to polling.

---

## Phase 7 — Persistent keyboards & settings

> **Goal:** Replace ad-hoc `context.user_data["summary_edit_mode"]` flags with a proper `ConversationHandler`-driven settings menu, plus a persistent reply keyboard for quick actions.
> **Complexity:** M
> **Touch points:** new `conversations/` subpackage, `settings_cmd`, `/start` rewrite

### 7.1  Persistent quick-action keyboard
- On `/start`, send a `ReplyKeyboardMarkup` with:
  - `[💬 Chat, 🗂 Pages, 📊 Graph]`
  - `[⚙️ Settings, 🧠 SOUL, ❓ Help]`
- Marked `resize_keyboard=True`, `is_persistent=True`.

### 7.2  ConversationHandler for settings
- States: `SET_MODEL`, `SET_TEMPERATURE`, `SET_COMPACT_TRIGGER`, `SET_THINKING_EMOJI`, `SET_NOTIFICATION_LEVEL`.
- Each step uses a `ForceReply` or quick keyboard; on cancel, jump to `END`.

### 7.3  Per-chat settings
- `SET_MODEL` and `SET_TEMPERATURE` write to `page_meta` so different pages (or topics) can use different configurations.

### 7.4  Per-user settings
- `SET_NOTIFICATION_LEVEL` (silent / on-complete / on-permission) lives in a new `user_settings` table keyed by `user_id`.

---

## Phase 8 — Polls, dice, checklists

> **Goal:** Native Telegram interactivity for voting, random selection, and todo lists.
> **Complexity:** S
> **Touch points:** new `polls.py` helpers, callback handlers for `poll_answer`, `checklist_tasks_done`

### 8.1  `/poll <question> | <opt1> | <opt2> | …`
- `sendPoll` with `is_anonymous=True`, `allows_multiple_answers=False` by default.
- Track results in a `polls` table; on close, summarise to the bus.

### 8.2  `/quiz` variant
- `is_quiz=True` with `correct_option_id`.

### 8.3  `/dice`
- `sendDice(emoji="🎲"|"🎯"|"🏀"|"⚽"|"🎳"|"🎰")`.
- The result of 🎰 (slot machine) can be used to seed a random number for "agent to choose for me".

### 8.4  `/checklist` (new in 10.1)
- Build an `InputChecklist` of `InputChecklistTask` items; `sendChecklist`. Track `ChecklistTasksDone` updates to mark items.

### 8.5  `/tossup` — multi-option picker
- When the LLM can't decide between N candidates, post a `sendPoll` and react to the answer with the chosen one.

---

## Phase 9 — Profile & bot customisation

> **Goal:** Polish the bot's presence via BotFather-equivalent API calls.
> **Complexity:** S
> **Touch points:** startup hooks, `/setname`, `/setdesc`, `/setshort` admin commands

### 9.1  `setMyName`, `setMyShortDescription`, `setMyDescription`
- Wire to env vars (`TELEGRAM_BOT_NAME`, `TELEGRAM_BOT_SHORT_DESC`, `TELEGRAM_BOT_DESC`).
- Run on every startup; expose admin command to refresh without restart.

### 9.2  `setMyProfilePhoto` (Bot API 8.0+)
- `/setavatar <file_id or URL>` — download, validate, upload.

### 9.3  `setMyDefaultAdministratorRights`
- On startup, declare the bot's default rights for groups (delete messages, pin messages, manage topics). Users adding the bot see this in the confirmation screen.

### 9.4  `setMyCommands` per scope
- Use `BotCommandScopeChat(chat_id)` to set per-chat aliases; `BotCommandScopeAllPrivateChats` for the main set; `BotCommandScopeDefault` as a fallback.

### 9.5  Emoji status
- `setUserEmojiStatus` from the Mini App settings page (after `requestEmojiStatusAccess`).

---

## Phase 10 — Payments (Stars), gifts, subscriptions

> **Goal:** Accept Telegram Stars as in-app currency; sell premium features; gift Premium.
> **Complexity:** M
> **Touch points:** new `billing.py` module, `PreCheckoutQueryHandler`, `SuccessfulPayment` routing

### 10.1  `/buy <plan>`
- `sendInvoice(title, description, payload, currency="XTR", prices=[LabeledPrice(label, stars)])`.
- `PreCheckoutQueryHandler` validates the `payload` and answers with `ok=True` or `ok=False, error_message=...`.
- On `SuccessfulPayment`, credit the user's `user_settings.premium_until` timestamp.

### 10.2  `sendSubscriptionPayment` (Bot API 10.1)
- For recurring subscriptions: `sendSubscriptionPayment(subscription_period, ...)`. The `SuccessfulPayment` arrives each cycle; check `is_recurring` and extend.

### 10.3  Refunds
- `/refund <transaction_id>` (admin only) → `refundStarPayment(user_id, telegram_payment_charge_id)`.

### 10.4  Gift flow
- `/gift <user_id|@username>` → `sendGift(gift_id, text=...)` from a curated list loaded via `getAvailableGifts()`.

### 10.5  Star balance dashboard
- `getMyStarBalance()` on startup, surface in `/status` for the admin.

### 10.6  Paid broadcasts
- For `>30 msg/sec` channels: `sendMessage(..., allow_paid_broadcast=True)`; cap with `MAX_PAID_BROADCAST_PER_DAY` env var to control cost.

---

## Phase 11 — Business accounts, suggested posts, stories

> **Goal:** Adopt the "Bot as personal assistant" surface Telegram added in 10.0.
> **Complexity:** M
> **Touch points:** new `business.py` module

### 11.1  `BusinessConnection` lifecycle
- Listen for `business_connection` updates; cache `connection_id` and the enabled `rights`.
- Apply `rights` to all bot-initiated actions in business chats.

### 11.2  `readBusinessMessage` after replying
- When the bot sends into a business chat, call `readBusinessMessage(business_connection_id, chat_id, message_ids=[...])` to mark the original as read.

### 11.3  `deleteBusinessMessages` for cleanup
- If a permission was denied or task was cancelled, sweep up the bot's last messages.

### 11.4  `setBusinessAccountBio`, `setBusinessAccountName`
- `/businessname <text>` admin command.

### 11.5  Suggested posts (channels)
- On `/suggest <post_text>`, send a message with `suggested_post_parameters=SuggestedPostParameters(...)`; listen for `SuggestedPostApproved` / `Declined` / `Refunded`.

### 11.6  Story sharing from Mini App
- After exporting a graph image, call `shareToStory(media_url, { widget_link: { url: MINIAPP_URL + "?startapp=graph", name: "View full graph" } })`.

### 11.7  `postStory` on behalf of business
- For a daily summary, post a story with `InputStoryContentPhoto` (a generated image) every morning at 09:00 via `JobQueue`.

---

## Phase 12 — Cross-cutting: i18n, observability, hardening

> **Goal:** Make the bot production-grade across locales, time zones, and observability.
> **Complexity:** M
> **Touch points:** string extraction, new `i18n.py`, `health` endpoint, OTel

### 12.1  i18n via `gettext` or `babel`
- Extract every English string into `adapters/locales/<lang>/LC_MESSAGES/telegram_adapter.po`.
- Use `context.user_data["lang"]` (from `User.language_code`) to pick the locale.

### 12.2  Time-zone aware JobQueue
- `JobQueue` schedules are UTC; convert using `User.language_code` → IANA tz map.

### 12.3  OpenTelemetry tracing
- Wrap `bot.do_api_request`, `safe_send_message`, and the streaming loop in spans.
- Export to OTLP if `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

### 12.4  Prometheus metrics
- Expose `/metrics` with counters for messages sent, drafts streamed, permissions requested, callbacks received, errors by type.

### 12.5  SQLite connection pooling
- Replace ad-hoc `sqlite3.connect` calls with a small `aiosqlite` pool (5 connections, 30 s idle timeout).

### 12.6  Rate-limit guard
- Centralise the `Retry-After` + 429 handling; apply to every `_do_api` call.

### 12.7  Dry-run mode
- `DRY_RUN=1` env var: log every outgoing API call instead of executing it. Used in staging.

### 12.8  Link preview customisation
- Per `sendMessage`, allow `link_preview_options={is_disabled: True, prefer_small_media: True, ...}`.

### 12.9  Persistent keyboard on /start
- See P7.1.

### 12.10  Crash-only supervisor
- Provide a `Dockerfile` + `docker-compose.yml` that runs the bot with `restart: always` and a separate `tdlib/telegram-bot-api` sidecar for the local API server.

---

## 3. Feature dependency graph

```
P0 (Foundation)
├── enables everything below
│
P1 (Multi-modal) ───────┐
P2 (Reactions) ─────────┤
P3 (Inline) ────────────┤
P4 (Topics) ────────────┤   ──► P12 (i18n, observability)
P5 (Mini App v2) ───────┤
P6 (Webhook) ───────────┤
P7 (Keyboards) ─────────┤
P8 (Polls) ─────────────┤
P9 (Profile) ───────────┤
P10 (Payments) ─────────┤
P11 (Business) ─────────┘
```

P0 must ship first. P1 and P6 are independent of everything except P0. P5 is the largest and most user-visible but also the most isolated. P10 and P11 depend on P0 only.

---

## 4. Quick-win checklist (start here)

If time is tight, ship these in order for the biggest impact-per-hour:

1. **P0.5** — permission timeout fix (one-line change, big robustness win)
2. **P0.3** — healthcheck endpoint (one route, enables monitoring)
3. **P0.2** — log redaction (security)
4. **P1.1** — voice transcription via local Whisper sidecar (high user value)
5. **P2.2** — acknowledge with a reaction (one new method call, delightful UX)
6. **P5.5** — CloudStorage wiring in the miniapp (cross-device persistence)
7. **P8.1** — `/poll` command (enables multi-user decisions in groups)
8. **P7.1** — persistent quick-action keyboard (one keyboard markup)

That's roughly **5-7 days of work for a 10x perceived quality lift**.

---

## 5. Out of scope (deliberate non-goals)

- **Telegram Passport** — adds identity-verification complexity without a clear Hydragent use case. Can revisit if a "verify your email / phone" flow becomes needed.
- **Games** (`sendGame`, `setGameScore`, `getGameHighScores`) — irrelevant to a chat agent.
- **HTML5 games as Mini Apps** — not aligned with the agent's purpose.
- **End-to-end custom UI inside Telegram** — Telegram's own client is the UI; we use the Mini App for supplementary views.
- **Owning the bot's payment provider account beyond Stars** — Stars is sufficient for digital-goods monetisation; physical-goods providers (Stripe, YooKassa, etc.) are deferred.

---

## 6. Open questions for the user

Before starting P1.1 (Whisper), P5.5 (CloudStorage), and P10.1 (Stars), we should confirm:

1. **Whisper sidecar:** local model or hosted API? (cost vs. privacy)
2. **VLM sidecar:** same question — local `llava`/`moondream` or hosted (OpenAI, Anthropic, Google)?
3. **Premium price points:** what should the `/buy` plans be? Free / Pro / Team tiers?
4. **Channel broadcast:** is the Hydragent operator a channel owner? (P4.6, P11.5 only matter if so)
5. **Multi-locale priorities:** which languages beyond English? (P12.1)
6. **Deployment target:** Docker on a single VPS, Kubernetes, or Cloud Run? (affects P6 design)
7. **Mini App hosting:** same domain as the bot backend, or static CDN (Vercel/Netlify)?

---

## 7. Change log

- **2026-06-16** — Initial roadmap created after comprehensive research sweep (4 parallel subagent reports).
- **2026-06-16** — P0 foundation block complete (items 0.2–0.7). Implemented in `adapters/telegram_adapter.py`:
  - **0.2 Structured logging + token redaction** — `RedactingFilter` scrubs bot tokens, HMAC secrets, `initData` hashes; logs to `data/logs/telegram_adapter.log` with rotation (10 MB × 5 files).
  - **0.3 Healthcheck endpoint** — `GET /health` + `GET /ready` on the aiohttp web server; `/health` reports bot username + bus connection state, `/ready` returns 200 only when both are connected.
  - **0.4 `Application` post-init / post-stop hooks** — `post_init` records `started_at` + bot identity; `post_stop` auto-denies in-flight permission futures (fail-closed) and flushes active pages.
  - **0.5 Permission request timeout** — `asyncio.wait_for(fut, timeout=PERMISSION_TIMEOUT_SECONDS)` (default 300 s); on timeout, sends `permission.respond { approved: false, reason: "timeout" }` to the bus and continues.
  - **0.6 Three-tier chat policy** — `ALLOWED_CHATS` / `READONLY_CHATS` / `BLOCKED_CHATS`; `get_chat_policy(chat_id)` returns "blocked" | "readonly" | "allowed" | "denied"; enforced at the top of `handle_message` and `handle_callback_query`.
  - **0.7 WebSocket dead-session cleanup** — `active_websocket_sessions` is now a `dict[ws, last_seen]`; `broadcast_to_webviews` skips sessions idle > `WS_SESSION_TIMEOUT` (default 60 s) and removes sessions whose `send_str` raises; a periodic `_websocket_dead_session_sweeper` task closes stale sockets.
  - **Smoke test** — `scratch/p0_4_6_7_smoke_test.py` (10 tests, all pass) caught 3 real bugs during development: (a) `asyncio.create_task` called without a running event loop, (b) `broadcast_to_webviews` swallowing exceptions from the wrapped task instead of the `send_str` call, (c) test state pollution from a shared `health_status` dict.
  - **Still pending in P0:** 0.1 (Settings dataclass), 0.8 (schema migrations), 0.9 (unit-test scaffolding). Recommended next: P1 (multi-modal) or close out 0.8 + 0.9 to fully ship P0.
