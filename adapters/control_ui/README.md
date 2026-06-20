# Hydragent Control UI

A browser-based **control panel** for the Hydragent gateway. Modeled on
the OpenClaw Control UI but adapted to Hydragent's existing transport:

* Pure Python (`aiohttp`) — no extra Rust changes, reuses the existing
  JSON-RPC bus protocol that `adapters/websocket_adapter.py` already
  uses.
* Vanilla JS frontend served from the same process — no Vite/Webpack
  build step required, the bundle is plain `index.html` + `assets/*`.
* Token / password / proxy / loopback auth, device pairing, optional
  VAPID-based Web Push, optional inbound hooks, optional admin HTTP RPC.

```
Browser  ───── http(s)://host:port/      (static index.html + assets)
              │
              ▼
control_ui_adapter.py  ───── ws://…/ws    (same wire format as
              │                                adapters/websocket_adapter.py)
              │
              ▼
        hydragent-bus   ────── 127.0.0.1:5000  (Rust kernel)
```

## Quick start

```bash
# 1. Install deps for the adapter
python -m pip install -r adapters/control_ui/requirements.txt

# 2. Run the adapter (default 0.0.0.0:8765)
HYDRA_GATEWAY_TOKEN=changeme python -m adapters.control_ui

# 3. Open the UI
open http://127.0.0.1:8765/
```

## Configuration

All settings are environment variables. They mirror the OpenClaw
`gateway.controlUi` section so operators familiar with that surface
can map them directly.

| Variable | Default | Purpose |
| --- | --- | --- |
| `HYDRA_CONTROL_UI_HOST` | `0.0.0.0` | bind address |
| `HYDRA_CONTROL_UI_PORT` | `8765` | bind port |
| `HYDRA_CONTROL_UI_PUBLIC_HOST` | `127.0.0.1` | host baked into `control-ui-config.json` |
| `HYDRA_CONTROL_UI_PUBLIC_PORT` | `8765` | port baked into the runtime config |
| `HYDRA_CONTROL_UI_BASE_PATH` | `/` | if you mount the UI under `/admin`, set this to `/admin` |
| `HYDRA_CONTROL_UI_ALLOWED_ORIGINS` | _(empty)_ | comma-separated full origins for non-loopback browsers |
| `HYDRA_GATEWAY_AUTH_MODE` | `token` | `token` / `password` / `none` / `trusted-proxy` |
| `HYDRA_GATEWAY_TOKEN` | _(empty)_ | static token for `token` mode |
| `HYDRA_GATEWAY_PASSWORD` | _(empty)_ | static password for `password` mode |
| `HYDRA_GATEWAY_ALLOW_TAILSCALE` | `false` | trust `Tailscale-User-*` headers as identity |
| `HYDRA_HOOKS_TOKEN` | _(empty)_ | enables `/hooks/<name>` when set |
| `HYDRA_ADMIN_RPC_TOKEN` | _(empty)_ | enables `POST /api/v1/admin/rpc` when set |
| `HYDRA_VAPID_PUBLIC_KEY` / `HYDRA_VAPID_PRIVATE_KEY` | auto-generated | Web Push keys, pinned from env |
| `HYDRA_VAPID_SUBJECT` | `https://hydragent.ai` | VAPID `mailto:` / `https:` subject |
| `HYDRA_LOG_LEVEL` | `INFO` | Python logging level |

The runtime config endpoint `/control-ui-config.json` is auth-gated and
returns:

```jsonc
{
  "version": "0.1.0",
  "channelId": "control-ui",
  "authMode": "token",
  "basePath": "/",
  "publicPort": 8765,
  "publicHost": "127.0.0.1",
  "websocketUrl": "ws://127.0.0.1:8765/ws",
  "tlsEnabled": false,
  "tailscale": { "allowTailscale": false, "mode": "off" },
  "vapidPublicKey": "BNc…",
  "vapidSubject": "https://hydragent.ai",
  "features": {
    "hooks": true,
    "adminRpc": true,
    "pwa": true,
    "webPush": true,
    "i18n": true,
    "themes": true
  },
  "locales": ["en", "zh-CN", "de", "es", "fr", "ja-JP", "pt-BR"],
  "themes": ["hydra-dark", "hydra-light", "abyss", "aurora"]
}
```

## Auth modes

* **`token`** (default) — the browser stores the token in
  `localStorage` and sends `Authorization: Bearer …` plus `?token=…` on
  the WebSocket URL (defensive double-channel).
* **`password`** — the browser prompts once for a password and stores it
  in `sessionStorage` as a Basic-auth credential. Browsers cannot set
  `Authorization` on the WebSocket handshake, so the upgrade is
  authenticated by the HTTP-side cookie the adapter issues for the
  page-load (same-origin Basic auth over HTTPS in front).
* **`none`** — auth is disabled. The adapter only accepts loopback
  connections in this mode (anything else is rejected with HTTP 403).
* **`trusted-proxy`** — the adapter trusts `X-Forwarded-User` /
  `X-Forwarded-Email` from a reverse proxy. Pair with
  `tailscale serve`/`tailscale funnel`, Caddy, oauth2-proxy, etc.

## Device pairing

Every browser/device sends an `X-Hydra-Device-Id` header (or
`?deviceId=…` query) which is generated on first load and persisted in
`localStorage`. The adapter records it in
`~/.hydragent/control-ui/devices.json` on first use. Loopback devices
are auto-approved; non-loopback devices are rejected with HTTP 409
(`PAIRING_REQUIRED`) until the operator approves them.

```bash
# Approve the latest pairing (CLI placeholder — implement in
# crates/hydragent-security when you wire the formal CLI):
hydragent devices approve --latest
hydragent devices list
hydragent devices revoke <device-id>
```

## Endpoints

| Path | Auth | Purpose |
| --- | --- | --- |
| `GET /` | public (Origin guard) | the SPA shell |
| `GET /assets/*` | public (Origin guard) | static UI assets |
| `GET /manifest.webmanifest` | public (Origin guard) | PWA manifest |
| `GET /sw.js` | public (Origin guard) | service worker |
| `GET /control-ui-config.json` | token/password/proxy | runtime config |
| `GET /healthz` | public | health probe |
| `GET /ws` | token/password/proxy | browser WebSocket |
| `GET /push/web/vapidPublicKey` | token/password/proxy | VAPID pub key |
| `POST /push/web/subscribe` | token/password/proxy | register a Web Push subscription |
| `POST /push/web/unsubscribe` | token/password/proxy | remove a subscription |
| `POST /push/web/test` | token/password/proxy | smoke test (delivers via the same hook the adapter uses) |
| `POST /hooks/<name>` | `HYDRA_HOOKS_TOKEN` | inbound webhook → `intent.submit` |
| `POST /api/v1/admin/rpc` | `HYDRA_ADMIN_RPC_TOKEN` | admin-only RPC (read-only methods by default) |

The WebSocket wire format is identical to `adapters/websocket_adapter.py`:

```jsonc
// outbound (browser → server)
{ "content": "hello hydra", "page_id": "ctrl-abc" }
{ "set_page_id": "ctrl-def" }
{ "type": "ping" }

// inbound (server → browser)
{ "type": "hello", "channel_id": "control-ui", "page_id": "ctrl-abc", "user_id": "ctrl-user-…" }
{ "type": "token", "page_id": "ctrl-abc", "token": "Hel" }
{ "type": "status", "page_id": "ctrl-abc", "status": "thinking" }
{ "type": "permission_request", "page_id": "ctrl-abc", "request_id": "...", "tool_id": "...", "tier": "Prompt", "summary": "..." }
{ "type": "complete", "page_id": "ctrl-abc" }
{ "type": "result", "page_id": "ctrl-abc", "content": "…" }
{ "type": "push", "page_id": "ctrl-abc", "content": "…", "timestamp": 1700000000000 }
{ "type": "error", "page_id": "ctrl-abc", "message": "…" }
```

## Themes

Four built-in themes are exposed via `themes.json` and the runtime
config:

* `hydra-dark` — default, matches the rest of Hydragent's chrome.
* `hydra-light` — high-contrast light theme.
* `abyss` — purple/indigo "deep sea" theme.
* `aurora` — gradient teal/violet background.

Themes are pure CSS custom-property swaps, so adding a new one is just
adding a `[data-theme="…"]` block in `style.css`.

## PWA / Web Push

The UI registers `sw.js` on first load so the SPA shell is cached and
can be opened offline. The service worker deliberately does **not**
intercept `/ws` — WebSocket connections are always live.

For Web Push, the adapter auto-generates a VAPID keypair the first
time `/push/web/vapidPublicKey` is hit (saved to
`~/.hydragent/control-ui/vapid-keys.json`, `0600`). The actual
push-send path (VAPID JWT + ECE encryption + relay to FCM/APNS) lives
in `crates/hydragent-gateway/src/push/` and is invoked from the
`_broadcast_push` hook so any push that goes to a browser can also go
to a sleeping device.

## Tailscale / reverse-proxy guidance

Tailscale is **not** baked into the adapter (Hydragent is designed to
be deployed without a Tailscale account). To expose the UI over your
tailnet:

```bash
# Option A: Tailscale Serve — proxy through your local node only.
tailscale serve --bg --https=443 http://localhost:8765

# Option B: Tailscale Funnel — public HTTPS endpoint.
tailscale funnel --bg 443 http://localhost:8765

# Then run the adapter with:
HYDRA_GATEWAY_AUTH_MODE=trusted-proxy \
HYDRA_GATEWAY_ALLOW_TAILSCALE=true \
python -m adapters.control_ui
```

The adapter does **not** import `tailscale.com/…` or any Tailscale-only
library — it trusts whatever proxy is in front of it.

## Build / dev loop

The UI is plain static files — there is no bundler. Edit
`dist/index.html` / `dist/assets/*` directly and refresh. If you want
to introduce a Vite build later, the layout (`index.html` at the root,
assets under `assets/`) is already compatible with a `vite build`
output — just point `base: './'` and copy `dist/` over.

## Where to read next

* [../../doc/CONTROL_UI.md](../../doc/CONTROL_UI.md) — full feature doc
  mirrored after OpenClaw's structure.
* [../websocket_adapter.py](../websocket_adapter.py) — the wire format
  this adapter re-uses.
* [../../crates/hydragent-bus/PROTOCOL.md](../../crates/hydragent-bus/PROTOCOL.md) — bus methods.
* [../../config/SOUL.md](../../config/SOUL.md) / [USER.md](../../config/USER.md) — soul + user identity.