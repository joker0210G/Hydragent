# Hydragent Onboarding, Installation & Contributing Guide

> The comprehensive guide to installing, configuring, and contributing to **Hydragent** — your local-first, self-improving AI assistant.

---

## 1. Mental Model (90 seconds)

Hydragent has three runnable parts working together:

1. **The Core (`hydragent` binary)** — A Rust-based orchestrator. It runs the event bus, memory layer (desk / draft / pages / books / shelves), tools registry, vault lockbox, and dreaming engine. Communicates via JSON-RPC on TCP port `5000`.
2. **Channel Adapters (`adapters/channels/`)** — Python gateways that translate platform events (Telegram, Discord, Slack, CLI) into JSON-RPC and send replies back. Each adapter is supervised and auto-respawns on crash.
3. **Python SDK (`adapters/hydragent_py/`)** — The official Python client for scripting and automation.

Setup is local-first. Nothing is sent anywhere without your explicit choice.

---

## 2. Prerequisites

Install these before running the installer:

| Tool | Version | Why |
|---|---|---|
| **Rust** | ≥ 1.78 | Compiles the core runtime binary |
| **Python** | ≥ 3.11 | Runs channel adapters and graph tooling |
| **Git** | any | Updates, skill versioning, Curator rollbacks |
| **Astral `uv`** | optional | Fast virtual env (installer bootstraps it if absent) |
| **MinGW-w64** | Windows only | `dlltool.exe` for certain build steps |

The installer handles the boring work. You do not need to install `uv` manually.

---

## 3. Install

Use the platform installer. Do not run a hand-rolled setup sequence.

**Windows (PowerShell 5.1+):**
```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | iex
```
Or from a local clone:
```powershell
.\Hydragent.cmd install
```

**macOS / Linux / Termux:**
```bash
curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
```
Or from a local clone:
```bash
./install.sh
```

### What the installer does

The installer is idempotent. Re-running it updates without losing your existing config.

1. **OS & arch detection** — macOS (Apple Silicon / Intel), Linux (musl / glibc), Windows, or Termux.
2. **Downloader probe** — checks for `curl` first, falls back to `wget`; fails clearly if neither is available.
3. **Interrupt cleanup** — registers `trap cleanup EXIT INT TERM` so partial downloads and temp folders are wiped if you press Ctrl+C or if the network drops.
4. **uv bootstrap** — checks for `uv` on PATH; if missing, installs it via the official bootstrap script.
5. **Binary install** — downloads prebuilt release tarball or falls back to `cargo build --release` from source.
6. **Launcher shim** — writes `hydragent` (Unix) or `Hydragent.cmd` (Windows) to `~/.hydragent/bin`.
7. **PATH entry** — adds the bin dir to your shell profile (`.zshrc`, `.bashrc`, `.profile`) if absent.
8. **`.env` init** — copies `.env.example → .env` only when `.env` is not already present.

The installer prefers clear failure over silent partial setup.

### Installer flags

**Windows `install.ps1`:**
```
-Source          Force build from source
-SkipOnboard     Skip the onboarding wizard
-Force           Overwrite existing installation
-Version <tag>   Pin a specific release (e.g. v0.7.2)
-InstallRoot <p> Custom install directory
```

**macOS/Linux `install.sh` env vars:**
```
HYDRAGENT_SOURCE=1          Force source build
HYDRAGENT_SKIP_ONBOARD=1    Skip onboarding
HYDRAGENT_FORCE=1           Overwrite existing install
HYDRAGENT_VERSION=<tag>     Pin a release tag
HYDRAGENT_INSTALL_ROOT=<p>  Custom install directory
HYDRAGENT_REPO=owner/repo   Override GitHub repo (for forks)
```

---

## 4. First-Run Onboarding Wizard

After install, run the guided setup:

```bash
hydragent onboard
```

The onboarding flow is short, ordered, and local. It writes canonical config files. It does not send anything to a remote service.

### Stage A — Vault Setup (Lockbox)

Hydragent asks for the lockbox unlock method first.

- **Passphrase / PIN** for interactive use
- **Machine-bound key file** for unattended or server startup

This stage only establishes the lockbox. The model is never shown raw secret values — only placeholders. The vault keeps API keys separate from memory.

### Stage B — Brain Setup (Model Provider)

Hydragent asks which provider or model path to use.

- **Local-only** — Ollama or LM Studio running on your machine (no network calls, no cost)
- **Hosted provider** — OpenRouter, OpenAI, Anthropic, Groq, Together AI (API key required)
- **Direct endpoint** — any OpenAI-compatible `/v1/chat/completions` URL

The chosen provider path is saved to config. The secret key goes into the vault, not into memory.

### Stage C — Memory & Skill Sources

Hydragent configures the durable work surfaces:

- **Desk** — active work context
- **Draft paper** — in-flight admissions queue
- **Pages, books, shelves** — durable long-term memory (many-to-many linked via Graphify)
- **Skill discovery roots** — where reusable tricks are loaded from
- **Graphify** — relationship mapping engine (enable/disable here)

This stage also decides whether the operator wants bundled skills only, optional skill sources, or broader discovery paths.

### Stage D — Safety Posture

The final step chooses the default safety posture:

- **Default-deny** for risky shell, network, and file operations
- **Sandboxed execution** for higher-risk work (Docker / WASM container)
- **Approval prompts** for operations that can touch sensitive data

The safety posture is written to config visibly, not buried in a hidden side effect.

---

## 5. What the Wizard Writes

The wizard writes only canonical local state:

- Config files in `~/.hydragent/`
- Vault metadata (not raw secret values)
- Selected provider and model settings
- Enabled skill discovery sources
- Safety defaults and sandbox preferences

It does not write setup conversation history into durable memory unless you explicitly ask it to keep a note.

---

## 6. Health Diagnostics

If something looks wrong, run:

```bash
hydragent doctor
```

The doctor check answers the questions that matter operationally:

| Check | What it verifies |
|---|---|
| **Runtime** | Rust binary installed and executable |
| **Toolchain** | Python, Rust, compiler versions meet minimums |
| **Database** | SQLite core can read and write state |
| **Vault** | Lockbox is unlocked and API keys are reachable |
| **Model ping** | Chosen provider endpoint responds to a test request |
| **Sandbox** | Docker or WASM environment is available for risky execution |
| **Skills** | Configured skill discovery sources are readable |

The doctor reports failures in plain language and points to the broken layer. It does not just say "setup failed."

---

## 7. Recovery & Reconfiguration

Hydragent setup is reversible without being sloppy:

- **Rerun onboarding** — updates selected settings without destroying unrelated local state
- **Vault changes** — stay local and explicit; provider changes do not silently rewire memory
- **Interrupted setup** — the next run picks up cleanly from the last safe point or restarts from Stage A
- **Update** — `hydragent update` fetches the latest release binary in place
- **Uninstall** — `hydragent uninstall` removes the binary and launcher; your `.env`, vault, and memory are preserved unless you explicitly delete them

---

## 8. Setup State Machine

```text
not installed
    → installer runs
    → local environment created
    → vault configured           (Stage A)
    → provider selected          (Stage B)
    → memory and skills sourced  (Stage C)
    → safety posture chosen      (Stage D)
    → doctor passes
    → ready

ready
    → rerun onboarding for changes     (settings update, no identity loss)
    → hydragent doctor for checks
    → hydragent update for upgrades
```

---

## 9. Practical Defaults

- Default to **local-first** behavior (Ollama if available).
- Default to **explicit approvals** for risky operations.
- Default to **bundled skills only** until the operator adds more.
- Default to **readable diagnostics** over clever summaries.
- Default to **preserving existing local state** on rerun.

---

## 10. Contributor Guide

### Codebase Layout

| Path | What it contains |
|---|---|
| `crates/hydragent-core/` | Core kernel binary |
| `crates/hydragent-tools/` | Tools accessible by the LLM |
| `crates/hydragent-bus/` | TCP JSON-RPC event bus |
| `crates/hydragent-memory/` | SQLite, FTS5, vector retrieval, Graphify bridge |
| `crates/hydragent-vault/` | Cryptographic lockbox (secrets storage) |
| `crates/hydragent-skills/` | Skill auto-induction and 7-day Curator |
| `crates/hydragent-swarm/` | Subagent spawning and Model Council |
| `adapters/channels/` | Channel adapters (Telegram, Discord, Slack, CLI) |
| `adapters/hydragent_py/` | Python SDK |
| `skills/builtin/` | Built-in YAML skill manifests |
| `config/` | `SOUL.md`, `USER.md`, `model_council.yaml` |

### Two CLIs

- **Rust CLI (`hydragent`)** — core engine, server daemon, administrator. Use for `onboard`, `doctor`, `serve`, `chat`, `update`, `uninstall`.
- **Python CLI (`hydra-cli`)** — lightweight client adapter. Use for `send` (from scripts/CI) and `chat` with a remote/background gateway.

### Development Loop

```bash
# Build all crates
cargo build

# Build the kernel only
cargo build -p hydragent-core

# Run kernel unit tests
cargo test -p hydragent-core --bin hydragent

# Run specific crate tests
cargo test -p hydragent-vault
cargo test -p hydragent-skills

# Run Python SDK tests
python -m unittest discover -s adapters/tests
```

### Contribution Rules

1. **No credential logging** — mask all secrets in debug and log output.
2. **Keep binary footprint small** — avoid large dependencies in core crates.
3. **Write tests** — every new feature or tool needs a unit or integration test.
4. **Document changes** — update `doc/ARCHITECTURE.md` or `doc/FEATURES.md` if a change alters system design.

---

## 11. Changelog Summary

### v0.7.1 & v0.7.2 (June 2026)
- **Security**: Masked sensitive API keys in all startup and debug logs.
- **REPL**: Added streaming, token-by-token incremental markdown rendering in the terminal.
- **CLI**: Added `hydragent update` and `hydragent uninstall` subcommands.
- **Skills**: Shipped the `hydragent-skills` crate and the `hydragent-bench` evaluation harness.

### v0.6.0 (May 2026)
- **Security**: Upgraded to the V2 Dual-Slot Vault (Passphrase PIN + Admin Key File).
- **Core**: Integrated the `hydragent-security` Merkle chain audit logging and taint tracker.
- **Adapters**: Shipped Telegram, Discord, and Slack adapters.
- **Memory**: Implemented the Bounded Markdown Memory system (`USER.md` and `SOUL.md` limits).
