# Hydragent Control UI

> Browser-based control panel for the Hydragent gateway. Mirrors the
> OpenClaw Control UI surface but adapted to Hydragent's Rust kernel
> + Python adapter architecture.

This document walks through every feature of the Control UI in the
same order as the upstream "Control UI" doc so it's easy to map the
two systems when porting dashboards, scripts, or behaviour.

## TL;DR

```bash
python -m pip install -r adapters/control_ui/requirements.txt
HYDRA_GATEWAY_TOKEN=changeme python -m adapters.control_ui
# open http://127.0.0.1:8765/
```

The adapter binds to `0.0.0.0:8765` by default and connects to the
shared Event Bus on `127.0.0.1:5000`. Everything else is just HTML/CSS/JS
served from `adapters/control_ui/dist/`.

---

## 1. Architecture

The Control UI runs as a single Python process (the
**adapter**) that sits between the browser and the **Event Bus**
(Rust kernel). It mirrors `adapters/websocket_adapter.py` and
deliberately introduces **no new Rust code** so the kernel stays a
clean TCP JSON-RPC service.

```
┌────────────────────────────────┐
│ Browser (PWA installable)      │
│   - index.html                 │
│   - assets/app.js + i18n.js    │
│   - assets/style.css           │
│   - sw.js  (service worker)    │
│   - manifest.webmanifest       │
└────────────────────────────────┘
              │  https / http + ws://
              ▼
┌────────────────────────────────┐
│ adapters/control_ui/__init__.py│
│  - aiohttp                     │
│  - /           (static shell)  │
│  - /ws         (WebSocket)     │
│  - /control-ui-config.json     │
│  - /healthz                    │
│  - /push/web/*                 │
│  - /hooks/<name>   (optional)  │
│  - /api/v1/admin/rpc           │
│  - Auth + pairing + CORS       │
└────────────────────────────────┘
              │  jsonrpc/2.0 over TCP
              ▼
┌────────────────────────────────┐
│ hydragent-bus (Rust)           │
│   127.0.0.1:5000               │
└────────────────────────────────┘
```

### Why no Rust changes?

* The wire format on the bus (`intent.submit`, `response.token`,
  `response.status`, `response.permission_request`, `response.complete`,
  `gateway.push`) is already what `websocket_adapter.py` consumes and
  what the kernel emits — we just register as `channel_id="control-ui"`
  instead of `"websocket"`.
* Push fan-out already broadcasts to `channel_id="*"` — the Control
  UI listens on the same long-lived connection the WebSocket adapter
  opens.
* All admin methods (`memory.*`, `library.*`, `page.*`, `config.*`,
  `skill.*`, `dream.*`, `cron.*`, `session.*`) are already exposed by
  the kernel and called by the existing CLI / REPL — the Control UI
  just wraps them in a typed RPC endpoint.

---

## 2. Quick start

### Install

```bash
python -m pip install -r adapters/control_ui/requirements.txt
```

### Run

```bash
HYDRA_GATEWAY_TOKEN=changeme python -m adapters.control_ui
```

### Open

* Local: <http://127.0.0.1:8765/>
* LAN:   `http://<lan-ip>:8765/` (allowed automatically for RFC1918
  hosts that match the request Host header).

### Smoke test

```bash
curl http://127.0.0.1:8765/healthz
# {"ok":true,"channel_id":"control-ui","auth_mode":"token",...}

curl -H "Authorization: Bearer changeme" \
     http://127.0.0.1:8765/control-ui-config.json
# {"version":"0.1.0","websocketUrl":"ws://127.0.0.1:8765/ws",...}
```

---

## 3. Surfaces

### Static UI shell — `GET /`

* `index.html` — single-page app with the 8 tabs (Chat, Memory,
  Library, Skills, Dreams, Cron, Pages, Config).
* `assets/style.css` — four themes (`hydra-dark`, `hydra-light`,
  `abyss`, `aurora`).
* `assets/app.js` — bootstrap, WebSocket client, tab router,
  permission modal, push-notification wiring, admin-RPC client.
* `assets/i18n.js` — locale loader (English plus six more locales).
* `manifest.webmanifest` — PWA manifest.
* `sw.js` — service worker (caches the SPA shell, never caches `/ws`).

### Runtime config — `GET /control-ui-config.json`

Auth-gated. The browser hits this on first load to learn:

* the WebSocket URL,
* the auth mode in effect for this request,
* the VAPID public key,
* the supported themes and locales,
* feature flags (`pwa`, `webPush`, `i18n`, `themes`, `hooks`,
  `adminRpc`).

This is what allows the SPA to be a single bundle that adapts to the
operator's deployment (loopback-only LAN, Tailscale, public HTTPS,
etc.).

### Health — `GET /healthz`

Always public. Returns a small JSON payload useful for `tailscale
ping` / `curl --fail` / Kubernetes probes.

### WebSocket — `GET /ws`

The browser-side wire format is byte-identical to
`adapters/websocket_adapter.py` so any existing tooling that talks
to the WS adapter will work against the Control UI:

```jsonc
// Browser → server
{ "content": "hi", "page_id": "ctrl-abc" }
{ "set_page_id": "ctrl-def" }
{ "type": "ping" }

// Server → browser
{ "type": "hello", "channel_id": "control-ui", "page_id": "ctrl-abc", "user_id": "ctrl-user-…" }
{ "type": "token", "page_id": "ctrl-abc", "token": "Hel" }
{ "type": "status", "page_id": "ctrl-abc", "status": "thinking" }
{ "type": "permission_request", "page_id": "ctrl-abc", "request_id": "...", "tool_id": "...", "tier": "Prompt", "summary": "..." }
{ "type": "complete", "page_id": "ctrl-abc" }
{ "type": "result", "page_id": "ctrl-abc", "content": "…" }
{ "type": "push", "page_id": "ctrl-abc", "content": "…", "timestamp": 1700000000000 }
{ "type": "error", "page_id": "ctrl-abc", "message": "…" }
```

### Push — `GET /push/web/vapidPublicKey`, `POST /push/web/subscribe|unsubscribe|test`

The adapter auto-generates a VAPID keypair on first run
(`~/.hydragent/control-ui/vapid-keys.json`, `0600`). Operators can
pin one with `HYDRA_VAPID_PUBLIC_KEY` / `HYDRA_VAPID_PRIVATE_KEY` for
production deployments. The browser registers the subscription via
`pushManager.subscribe(...)` and POSTs the result here; the adapter
persists it under `~/.hydragent/control-ui/push_subscriptions.json`.

The actual push delivery (VAPID JWT + ECE encryption + relay to
FCM/APNS) is wired from `_broadcast_push` so any push that the kernel
emits to `channel_id="control-ui"` will reach the user's phone even
when the tab is in the background.

### Hooks — `POST /hooks/<name>`

Off by default. Enable with `HYDRA_HOOKS_TOKEN=<secret>` (or
explicitly `HYDRA_HOOKS_ENABLED=true`). Authenticated with
`Authorization: Bearer $HYDRA_HOOKS_TOKEN`. The body is forwarded
straight to the kernel as `intent.submit` with `channel_id="hook"`
and `transport="hook"` metadata.

Use it for:

```bash
curl -X POST http://127.0.0.1:8765/hooks/github \
  -H "Authorization: Bearer $HYDRA_HOOKS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"content":"new commit pushed: feat: add dark mode","page_id":"hook-gh"}'
```

### Admin HTTP RPC — `POST /api/v1/admin/rpc`

Off by default. Enable with `HYDRA_ADMIN_RPC_TOKEN=<secret>`. The
allow-list of methods matches what the CLI exposes and is restricted
to read-mostly operations by default (memory write methods, page
compact, config.write, cron.add/remove are all on the allow-list —
adjust `ADMIN_ALLOWED_METHODS` in `control_ui/__init__.py` to tighten
further).

```bash
curl -X POST http://127.0.0.1:8765/api/v1/admin/rpc \
  -H "Authorization: Bearer $HYDRA_ADMIN_RPC_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"method":"memory.list","params":{"limit":50}}'
```

---

## 4. Auth & pairing

The auth story mirrors OpenClaw's `gateway.auth` block:

* `token` (default) — shared secret in `HYDRA_GATEWAY_TOKEN`, sent as
  `Authorization: Bearer` (and as `?token=` for WebSocket-unsafe
  browsers). Constant-time compared.
* `password` — shared password in `HYDRA_GATEWAY_PASSWORD`. Sent as
  HTTP Basic Auth; stored in `sessionStorage` so it doesn't survive a
  tab close.
* `none` — auth is disabled. The adapter still requires loopback
  connections so a non-loopback client is rejected with HTTP 403.
* `trusted-proxy` — the adapter trusts identity headers
  (`X-Forwarded-User`, `X-Forwarded-Email`, or Tailscale Serve's
  `Tailscale-User-Login`). Pair with `tailscale serve`, `caddy`,
  `oauth2-proxy`, etc.

### Device pairing

The first time a browser opens the UI it generates a UUID stored in
`localStorage`. It is sent on every WebSocket upgrade via
`X-Hydra-Device-Id` (or `?deviceId=`). The adapter records the device
in `~/.hydragent/control-ui/devices.json`.

Loopback devices are auto-approved (no UI friction for local
operators). Non-loopback devices receive HTTP 409 with a
`PAIRING_REQUIRED` payload and the WebSocket is closed; the operator
must approve them.

The future CLI surface (track in `doc/ROADMAP.md`):

```bash
hydragent devices list
hydragent devices approve --latest
hydragent devices revoke <device-id>
```

Until that ships, the registry can be hand-edited:

```jsonc
{
  "<uuid>": {
    "deviceId": "<uuid>",
    "label": "Operator laptop",
    "role": "operator",
    "publicKey": "ed25519:…",
    "approvedAt": 1700000000,
    "lastSeen":  1700000000,
    "scopes":    ["operator"],
    "userAgent": "Mozilla/5.0 …"
  }
}
```

### Allowed origins

* Loopback names (`127.0.0.1`, `::1`, `localhost`) — always.
* RFC1918 / link-local / `.local` / `.ts.net` / Tailscale CGNAT
  (`100.x`) — allowed only when the Origin host matches the
  request Host header (same-origin private LAN loads).
* Tailscale Serve/Funnel identity headers — accepted when
  `HYDRA_GATEWAY_ALLOW_TAILSCALE=true`.
* Everything else — must be in the comma-separated
  `HYDRA_CONTROL_UI_ALLOWED_ORIGINS`.

There are two env-controlled footguns (`off` by default and loudly
named so operators don't enable them by accident):

* `HYDRA_CONTROL_UI_DANGEROUSLY_DISABLE_DEVICE_AUTH=true`
* `HYDRA_CONTROL_UI_DANGEROUSLY_ALLOW_HOST_HEADER_FALLBACK=true`

---

## 5. Tailscale / reverse-proxy

Hydragent does not import any Tailscale-only library. The adapter
trusts whatever identity-aware proxy you put in front of it.

### Tailscale Serve (tailnet-only)

```bash
tailscale serve --bg --https=443 http://localhost:8765
HYDRA_GATEWAY_AUTH_MODE=trusted-proxy \
HYDRA_GATEWAY_ALLOW_TAILSCALE=true \
python -m adapters.control_ui
```

### Tailscale Funnel (public)

```bash
tailscale funnel --bg 443 http://localhost:8765
# same env as above
```

### Caddy / nginx / oauth2-proxy

Set `HYDRA_GATEWAY_AUTH_MODE=trusted-proxy` and ensure the proxy sends
`X-Forwarded-User` (and optionally `X-Forwarded-Email`) on every
request.

---

## 6. Themes & i18n

### Themes

Four themes are bundled:

| id | description |
| --- | --- |
| `hydra-dark` | default, dark base |
| `hydra-light` | high-contrast light |
| `abyss` | purple / indigo |
| `aurora` | gradient teal / violet |

Adding a new theme is just adding a `[data-theme="my-theme"] { --var: …; }`
block in `dist/assets/style.css` and listing it in `themes.json`.

### Locales

Seven locales ship out of the box:

* `en` (default)
* `zh-CN`
* `de`
* `es`
* `fr`
* `ja-JP`
* `pt-BR`

The locale is detected from `navigator.language` and overridable from
the top-bar selector. New locales are just `dist/lang/<locale>.json`
files with the same keys as `en.json`.

---

## 7. PWA & Web Push

The UI registers a service worker that caches the static shell so the
UI loads even when the kernel is offline. WebSocket traffic is
intentionally not cached — connections are always live.

Web Push uses the auto-generated VAPID keypair. The actual delivery
(VAPID JWT, ECE encryption, FCM/APNS relay) is invoked from
`_broadcast_push` so any kernel push on `channel_id="control-ui"` can
fan out to the user's phone.

The browser exposes `Notification.permission` — the UI only requests
it after the user clicks a "Notify me" toggle (no surprise prompts).

---

## 8. Panels

Each tab is a small panel that uses the admin RPC for data fetching:

| Tab | What it does | Methods |
| --- | --- | --- |
| **Chat** | Live WebSocket conversation; permission modals; cancel button | WS only |
| **Memory** | Search/list/delete semantic memories | `memory.list/search/delete/clear` |
| **Library** | Browse/search the long-term reference library | `library.list_nodes`, `library.search` |
| **Skills** | List/search/run installed skills | `skill.list/search/run` |
| **Dreams** | Trigger / inspect dream consolidation cycles | `dream.run`, `dream.status` |
| **Cron** | List / run / add / remove scheduled jobs | `cron.list/run/add/remove` |
| **Pages** | Browse / switch / compact conversation pages | `page.list/get_summary/update_summary/compact` |
| **Config** | Read-only runtime config dump | `config.read` (via runtime-config endpoint) |

The permission modal in the **Chat** tab surfaces
`response.permission_request` notifications from the kernel (tier
`Prompt` and above). The decision (`once` / `session` / `deny`) is
sent back over the WebSocket; the adapter forwards it to the bus as
`permission.respond` so the agent continues the conversation with the
operator's choice.

---

## 9. Security notes

* All secrets are **never logged**. The adapter redacts
  `Authorization` headers before logging any request.
* `HMAC.compare_digest` is used for token/password comparisons.
* WebSocket upgrades require a valid token / password / proxy
  identity — anonymous loopback is allowed only because
  `devices.json` is the loopback device registry.
* The admin RPC allow-list is restricted to non-destructive methods
  by default. `memory.clear` requires a `confirm: true` flag at the
  protocol level; `cron.add` and `cron.remove` are gated by the admin
  token.
* The static shell is Origin-guarded but **not** token-guarded so the
  initial HTML can load over HTTPS without a prior auth round-trip
  (and so the service worker can cache it offline). Auth happens
  before the WebSocket connects and before any admin RPC call.
* VAPID private keys are stored at `0600` on disk; the file is the
  only place the private key is written, never logged.
* No third-party analytics, no third-party CDN — every script is
  served from the same origin as the UI.

---

## 10. Where to extend

| If you want to… | Look here |
| --- | --- |
| Add a new panel (e.g. `Models`) | `dist/index.html` (add `.tab` + `.panel`) and `dist/assets/app.js` (add `refresh…` + wire button) |
| Add a new theme | `dist/assets/style.css` (`[data-theme="…"]`) + `dist/themes.json` |
| Add a new locale | `dist/lang/<locale>.json` (copy from `en.json`) |
| Tighten the auth | `control_ui/__init__.py` → `auth_middleware`, `_request_auth_mode` |
| Add a new admin RPC method | `control_ui/__init__.py` → `ADMIN_ALLOWED_METHODS` (allow-list stays deny-by-default) |
| Add a new hook | just POST to `/hooks/<your-name>`; no code changes |
| Stream a different bus method to the browser | add it to `onWsMessage` in `app.js` and forward from `_send_intent_and_stream` / `_broadcast_push` in `control_ui/__init__.py` |
| Expose over Tailscale | `HYDRA_GATEWAY_AUTH_MODE=trusted-proxy` + `tailscale serve` |

See also:

* [adapters/control_ui/README.md](../../adapters/control_ui/README.md) — install / run / config cheatsheet.
* [adapters/websocket_adapter.py](../../adapters/websocket_adapter.py) — wire format reference.
* [crates/hydragent-bus/PROTOCOL.md](../../crates/hydragent-bus/PROTOCOL.md) — bus protocol reference.
* [doc/ARCHITECTURE.md](ARCHITECTURE.md) — kernel architecture.
* [config/SOUL.md](../../config/SOUL.md) and [USER.md](../../config/USER.md) — soul + identity.