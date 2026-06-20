# Onboarding — Zero to First Chat

> **Audience**: Someone who just `git clone`'d the repo, has never run the
> agent, and wants to send their first message in under 10 minutes.
>
> **Last verified against working tree**: 2026-06-16.
> For the deep architecture read [`doc/ARCHITECTURE.md`](doc/ARCHITECTURE.md).
> For the actual ground-truth state of the code, read [`doc/STATE.md`](doc/STATE.md).

> **🆕 Just want to install Hydragent?**
> End-users should use the **one-command installer** (the same UX as
> Ollama / OpenClaw) — see **[INSTALL.md](INSTALL.md)**.
> This document is for **contributors working from a checkout**.

---

## 1. The 90-second mental model

Hydragent has three runnable pieces:

| Piece | What it is | When you use it |
|---|---|---|
| **`hydragent` binary** (Rust, in `target/`) | The kernel — orchestrator, bus, memory, tools, security. Talks JSON-RPC over TCP. | Always-on. Started by `Hydragent.cmd chat` or the bus-mode (no subcommand). |
| **Python channel adapters** (in `adapters/`) | Thin gateways that fan inbound chat-platform messages into the kernel and fan agent replies back out. | Only if you want Telegram/Discord/Slack/etc. The CLI adapter is one of them. |
| **Python SDK** (`adapters/hydragent_py/`) | A typed Python client over the same JSON-RPC bus. | When you write your own scripts or apps that talk to a running kernel. |

> **You do NOT need a chat-platform token for the first chat.** The CLI
> adapter is built in, and `hydragent chat` uses it directly. Telegram /
> Discord / Slack are *optional* and covered in §6.

The single-file entry point is **`Hydragent.cmd`** in the repo root.
Double-click it (or run it from any shell) and it auto-detects what state
you're in and does the right thing. This is the one file you need to
remember exists.

---

## 2. Prerequisites (Windows, dev machine)

The `Hydragent.cmd install` flow auto-installs everything below, but if
you prefer to do it by hand:

| Prereq | Why | Install |
|---|---|---|
| **Rust ≥ 1.78** (`cargo`) | Builds the kernel | `winget install Rustlang.Rustup` (or [rustup.rs](https://rustup.rs)) |
| **MinGW-w64 at `C:\mingw64`** | `dlltool.exe` for some build steps | Download a `winlibs` `.7z` from [winlibs](https://winlibs.com/) and extract to `C:\` |
| **Python ≥ 3.11** | Adapters and SDK | `winget install Python.Python.3.12` |
| **Git** | Obviously | `winget install Git.Git` |

> **What you do NOT need (despite what the README used to say):**
> - ❌ **Docker** — the execution sandbox is Wasmtime-only; Docker is *not* implemented (see `doc/STATE.md` Phase 3 row).
> - ❌ **Zig** — only required if you're building the RISC-V / ESP32-S3 edge binary (out of scope for normal use).
> - ❌ **An LLM API key in the box** — the guided onboarding walks you through picking one.

PATH requirements (after install): `C:\Users\<you>\.cargo\bin` and
`C:\mingw64\bin` must be on `PATH` for the build to find `dlltool`.

---

## 3. The 4-step first run

### Step 1 — Clone, build, install (one shot)

```powershell
git clone https://github.com/joker0210G/Hydragent.git
cd Hydragent
.\Hydragent.cmd install
```

The `install` flow:
1. Detects missing Rust → downloads `rustup-init.exe` and runs it silently.
2. Detects missing MinGW → downloads a pre-built `winlibs` UCRT 7z (~150 MB) and extracts to `C:\mingw64`.
3. Persists both onto your **user** `PATH`.
4. Runs `cargo build -p hydragent-core` (first build = 2–5 minutes).

> **The script tells you to close the window and re-run after install.**
> That's because the `PATH` update is per-process; new windows pick it up.
> The same `.\Hydragent.cmd` with no argument will then auto-route to
> `chat` (or `onboard` first if you have no `.env`).

### Step 2 — Guided onboarding (writes `.env`)

```powershell
.\Hydragent.cmd onboard
```

This launches the Rust binary's interactive setup wizard. It walks you
through four steps:

1. **Pick a provider** — arrow-key menu of `openrouter | openai | together | groq | fireworks | ollama | lmstudio | custom`. Type a digit to quick-select; `q` cancels.
2. **Paste your API key** (or leave blank for local Ollama / LM Studio).
3. **Pick a primary model** — same arrow-key menu, with `custom` at the end.
4. **Verify the connection** — runs `hydragent test-brain` for you.

The wizard writes `.env` at `~/.hydragent/.env` (top level, not inside
`data/`), preserving any keys you already had. Non-interactive flags
(for CI) are documented in the binary's `--help`:

```powershell
# Using a preset provider
hydragent onboard `
    --provider openrouter `
    --api-key "$env:OPENROUTER_API_KEY" `
    --model openai/gpt-4o-mini `
    --non-interactive --no-verify

# Using any custom OpenAI-compatible endpoint (vLLM, TGI, etc.)
hydragent onboard `
    --base-url https://my-api.com/v1 `
    --api-key sk-my-key `
    --model my-model `
    --non-interactive --no-verify

# Or pass the URL directly as --provider
hydragent onboard `
    --provider https://my-api.com/v1 `
    --api-key sk-my-key `
    --model my-model `
    --non-interactive --no-verify
```

### Step 3 — First chat

```powershell
.\Hydragent.cmd chat
```

You should see the REPL prompt within a few seconds. Type `Hello!` and
hit Enter. The first turn is slow (cold-start; warms the model client's
TLS pool); subsequent turns are sub-second.

Inside the REPL, slash commands you should know about for the first 5
minutes:

| Command | What it does |
|---|---|
| `/help` | Full slash-command list |
| `/model` | Show / switch the live `BRAIN_MODEL` |
| `/memory list` | Show what's in the semantic store |
| `/clear` | Wipe the current page's history |
| `/exit` | Quit (Ctrl-C also works) |

### Step 4 — Verify the live brain (optional, but recommended)

```powershell
.\Hydragent.cmd doctor      # file-based diagnostics (no network)
.\Hydragent.cmd test-brain  # one-shot round-trip to your BRAIN_BASE
.\target\debug\hydragent.exe examples   # discover what to ask
```

If `test-brain` says `OK, brain is alive`, you're done. The agent
remembers nothing across pages by default — re-runs of `chat` start a
fresh page each time.

---

## 4. What just got created (so you can reason about it)

After a successful first run, the on-disk layout is:

```
hydragent/
├── target/
│   └── debug/
│       └── hydragent.exe      # the kernel (the only binary you ever run)
├── .hydragent/                # user-data home (auto-created on first run)
│   ├── .env                   # BRAIN_BASE / BRAIN_KEY / BRAIN_MODEL (gitignored)
│   └── data/
│       ├── sessions.db        # SQLite, WAL mode — page history + audit
│       └── skill_library.sqlite   # skill engine storage (Phase 7)
└── data/                      # legacy fallback (used only if HYDRAGENT_HOME is set to repo root)
    ├── sessions.db
    └── skill_library.sqlite
```

> **Layout note**: when you run from a source checkout, Hydragent uses
> `./.hydragent/` as its home directory (cwd-anchored fallback), so
> `.env` and `data/` both end up inside that folder rather than
> alongside `Cargo.toml`. Once you run `install.{ps1,sh}`, the home
> relocates to `~/.hydragent/`. See [`paths.rs`](crates/hydragent-core/src/paths.rs)
> for the exact resolution rules.

The `.env` is the **only** thing the user controls. Everything else is
derived. See §5 for the full key reference and common pitfalls.

---

## 5. `.env` reference (the keys that matter)

The "brain" is the agent's single live LLM. There are four keys, in
priority order. All four are required to actually chat, but `BRAIN_KEY`
can be empty for local providers:

```ini
BRAIN_BASE      = https://api.openrouter.ai/v1     # any OpenAI-compatible /v1 URL
BRAIN_KEY       = sk-or-...                         # leave empty for Ollama / LM Studio
BRAIN_MODEL     = openai/gpt-4o-mini               # primary model
BRAIN_FALLBACKS = openai/gpt-4o,anthropic/claude-3-haiku  # comma-separated, tried in order on failure
```

**Common pitfalls — read these before you file a "doesn't work" issue:**

> ⚠️ **Trailing slash in `BRAIN_BASE`.** Use `https://x.y/v1`, **not**
> `https://x.y/v1/`. The kernel appends `/chat/completions` itself.

> ⚠️ **`BRAIN_FALLBACKS` is fallback order, not a parallel race.** Models
> are tried one at a time, in the order listed, on error. Put your
> **cheapest reliable** model first, the **most capable** model last.

> ⚠️ **Some models don't emit `<think>…</mm:think>` blocks.** If your
> `BRAIN_MODEL` is a non-reasoning model (e.g. `MiniMax-M3`, plain
> `gpt-4o`, or most non-DeepSeek/QwQ models), the reasoning detector
> in the REPL is a no-op. The chat still works — you just won't see
> the dim-grey thinking trace. Models that DO emit reasoning: DeepSeek
> R1, QwQ, DeepHermes, and others that advertise thinking-mode output.
> If you want the trace, set `BRAIN_MODEL` to one of them and
> `HYDRAGENT_SHOW_REASONING=1` to make the dim text visible.

> ⚠️ **Legacy aliases still work but the new names are preferred.**
> `OPENROUTER_API_KEYS` → `BRAIN_KEY`, `PRIMARY_MODEL` → `BRAIN_MODEL`,
> `FALLBACK_MODELS` → `BRAIN_FALLBACKS`. If you set both, the new
> `BRAIN_*` wins.

> ⚠️ **Vault passphrase is optional for first chat.** The `.env.example`
> lists `HYDRAGENT_VAULT_PASSPHRASE` but it is **only** required if you
> want encrypted secret storage (Phase 3 vault). For a first chat, you
> can ignore it.

> ⚠️ **Channel tokens (`TELEGRAM_BOT_TOKEN`, `DISCORD_*`, etc.) are
> only required to run those adapters.** The CLI REPL ignores them.

The full list of every supported key is in
[`.env.example`](.env.example) at the repo root.

---

## 6. Adding a chat channel (Telegram, Discord, …) — optional

The CLI REPL is fine for solo use, but the real value of the kernel
is that the same `hydragent.exe` serves many channels. To add one:

1. Get a bot token from the platform (e.g. `@BotFather` for Telegram).
2. Add the token to `.env` (the platform-specific section).
3. In a *second* terminal, start the bus:
   ```powershell
   .\Hydragent.cmd           # no subcommand = bus server mode (port 5000)
   ```
4. In a *third* terminal, start the channel adapter:
   ```powershell
   python adapters\telegram_adapter.py
   ```

The bus listens on `127.0.0.1:5000` over JSON-RPC 2.0. All adapters
speak the same protocol. See [`crates/hydragent-bus/PROTOCOL.md`](crates/hydragent-bus/PROTOCOL.md)
for the wire format if you want to write your own client.

For the **Telegram Mini App** (the graph dashboard), see
[`adapters/miniapp/README.md`](adapters/miniapp/README.md) and set
`TELEGRAM_WEBAPP_URL` in `.env`. Use `pyngrok` for a one-command
HTTPS tunnel during development.

---

## 7. The dev loop (when you start changing code)

Most contributors touch one of three areas. The table below maps an
intent to the file you actually need to open:

| I want to… | Open this | And this |
|---|---|---|
| Add a tool the LLM can call | `crates/hydragent-tools/src/<your_tool>.rs` (mirror `echo.rs`) | Register the module in `crates/hydragent-tools/src/lib.rs` and wire the tool in `crates/hydragent-core/src/main.rs` |
| Add a built-in skill | `skills/builtin/<your-skill>.yaml` | Nothing else — the skill loader picks up YAML at startup. See `skills/builtin/debug-rust-error.yaml` for the schema. |
| Add a channel adapter | `adapters/<your>_adapter.py` | The adapter is just a Python script that talks JSON-RPC to `127.0.0.1:5000`. Use `adapters/bus_client.py` as the SDK. |
| Add a bus RPC method | `crates/hydragent-core/src/main.rs` (router) | Add a handler in the same file, register it in the dispatch table. |
| Add a CLI subcommand (`hydragent foo`) | `crates/hydragent-core/src/<your_cmd>.rs` | Add the `foo` variant to the `Commands` enum in `main.rs` and wire it into the top-level `match`. Mirror `update.rs` / `uninstall.rs` for the standard `run()` signature + CLI flag shape. |
| Change the agent's personality | `config/SOUL.md` | Restart `hydragent chat` — no rebuild. |
| Change your user profile / memory seed | `config/USER.md` | Same — restart REPL. |
| Tweak model routing | `config/model_council.yaml` | 20+ profiles live here. Restart for changes to take effect. |
| Add a Python SDK plugin | `adapters/hydragent_py/builtin/<your_plugin>.py` | Plugins are auto-discovered. See [`adapters/README.md`](adapters/README.md) for the `PluginContext` API. |

For a deeper walkthrough of any of these, see
[`CONTRIBUTING.md`](CONTRIBUTING.md).

### Test loop

The Rust kernel has **no library target** — all tests live in the
`hydragent-core` binary. The canonical test command is:

```powershell
$env:Path = 'C:\mingw64\bin;' + $env:Path
& "$env:USERPROFILE\.cargo\bin\cargo.exe" test -p hydragent-core --bin hydragent
```

(That explicit PATH + cargo path is what `Hydragent.cmd` does for you
internally; from a normal `cmd.exe` shell you can just `cargo test
-p hydragent-core --bin hydragent`.)

Python tests are end-to-end and need a **running bus** on
`127.0.0.1:5000`. The helper scripts in `tests/` make that one command:

```powershell
python tests\start_bus.py           # background, writes tests\.bus.pid
python tests\cli_user_pov.py        # 4-prompt smoke test
```

For the eval harness (SKILL-BENCH 80 tasks / Golden Set 30 pairs):

```powershell
cargo test -p hydragent-bench
```

---

## 8. What the README still doesn't tell you (read this once)

- **The "567 tests" number is from v0.7.0.** Recount after every release
  — the kernel binary alone is 49+ tests as of v0.7.1.
- **`hydragent` is a single binary**, not separate `hydragent` /
  `hydragent-server` / `hydragent-edge` binaries. The architecture doc
  lists all three but only the first exists in the build. The Zig edge
  is stubbed.
- **`page_id` is canonical; `session_id` is a doc-only alias.** Don't
  introduce a new `session_id` field — see `doc/STATE.md` §2.1.
- **The "16-layer security pipeline" is a roadmap target.** What is
  actually live (Phases 6.1–6.4) is in `crates/hydragent-security`; SQLCipher
  at-rest (Track 6.5) is deferred post-MVP.
- **Dreaming / consolidation is scaffolded, not running.** The
  `hydragent-memory` crate has the hooks; the nightly job does not
  fire automatically. See `doc/STATE.md` Phase 2 row.
- **`hydragent chat` log level defaults to `error`** (set via
  `HYDRAGENT_CHAT_LOG=warn|info|debug`) so the REPL stays quiet. The
  full kernel uses `LOG_LEVEL=warn` from `.env`.

---

## 9. Where to read more

- **First repo read**: [`doc/STATE.md`](doc/STATE.md) — ground truth on
  what's actually in the code, in a single table.
- **Before adding code**: [`CONTRIBUTING.md`](CONTRIBUTING.md) — code
  conventions and the exact files to touch.
- **Deep design**: [`doc/ARCHITECTURE.md`](doc/ARCHITECTURE.md) — the
  full 9-phase target architecture (not all of it is built yet).
- **What's planned next**: [`doc/ROADMAP.md`](doc/ROADMAP.md) and
  [`doc/PHASE_8_PLAN.md`](doc/PHASE_8_PLAN.md).
- **The bus protocol**: [`crates/hydragent-bus/PROTOCOL.md`](crates/hydragent-bus/PROTOCOL.md)
  — if you want to write a non-Python client.
- **The Python SDK reference**: [`adapters/hydragent_py/README.md`](adapters/hydragent_py/README.md).

If something here is wrong or out of date, the file to update is
**this one** (and then `doc/STATE.md` if the actual code state changed).
Both are the first things a newcomer reads.
