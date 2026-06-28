# Hydragent Onboarding, Installation & Contributing Guide

> The comprehensive guide to installing, running, and contributing to the **Hydragent Unified AI Agent**.

---

## 1. Quick Start (Onboarding)

### 1.1 The 90-Second Mental Model
Hydragent consists of three runnable components:
1. **The Kernel (`hydragent` binary)**: A Rust-based core orchestrator, event bus, memory layer, tools registry, and security vault. It communicates over JSON-RPC on TCP port `5000`.
2. **Channel Adapters (in `adapters/channels/`)**: Python-based gateways that translate external platform events (Telegram, Discord, Slack, etc.) into JSON-RPC messages for the kernel, and vice versa.
3. **Python SDK (`adapters/hydragent_py/`)**: The official Python client wrapper.

### 1.2 Dev Machine Prerequisites
- **Rust $\ge 1.78$**: For compiling the Rust kernel.
- **MinGW-w64**: Required for `dlltool.exe` during certain build steps.
- **Python $\ge 3.11$**: For running adapters and the SDK.
- **Git**: For version control.

### 1.3 Running Your First Chat
1. **Clone the repository**:
   ```bash
   git clone https://github.com/joker0210G/Hydragent.git
   cd Hydragent
   ```
2. **Install dependencies and build**:
   ```bash
   .\Hydragent.cmd install
   ```
3. **Run the guided onboarding**:
   ```bash
   .\Hydragent.cmd onboard
   ```
   This will prompt you to select an LLM provider, enter your API key, choose a model, and verify the connection.
4. **Start the chat**:
   ```bash
   .\Hydragent.cmd chat
   ```

### 1.4 The Two CLIs (Rust vs. Python)
Hydragent provides two CLIs tailored for different tasks:
- **Rust CLI (`hydragent`)**: The core engine, server daemon, and local administrator. Use this to run onboarding (`onboard`), run diagnostics (`doctor`), host the gateway (`serve`), or manage local security/memory.
- **Python CLI (`hydra-cli`)**: A lightweight client adapter. Use this to send commands to a running gateway from scripts/CI (`send`), or to chat with a remote/background gateway.

For a detailed breakdown and command reference, see the **[CLI Guide](doc/CLI.md)**.

---

## 2. Installation Guide (End-Users)

For production or non-dev environments, Hydragent provides a single-command installer.

### 2.1 One-Command Installation

#### Windows (PowerShell 5.1+)
```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | iex
```

#### macOS / Linux
```bash
curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
```

### 2.2 Installer Customization

#### Windows `install.ps1` Flags:
- `-Source`: Force building from source instead of downloading pre-built binaries.
- `-SkipOnboard`: Skip the interactive onboarding wizard.
- `-Force`: Overwrite any existing installation.
- `-Version <tag>`: Pin to a specific release version (e.g., `v0.7.2`).
- `-InstallRoot <path>`: Custom installation directory.

#### macOS/Linux `install.sh` Env Vars:
- `HYDRAGENT_VERSION`: Pin a release tag.
- `HYDRAGENT_INSTALL_ROOT`: Custom installation directory.
- `HYDRAGENT_REPO`: Forked GitHub repository path (`owner/repo`).

---

## 3. Contributor Guide

### 3.1 Codebase Layout
- `crates/hydragent-core/`: The core kernel binary.
- `crates/hydragent-tools/`: Tools accessible by the LLM.
- `crates/hydragent-bus/`: The TCP JSON-RPC event bus.
- `crates/hydragent-memory/`: SQLite, FTS5, and vector retrieval.
- `crates/hydragent-vault/`: Cryptographic secrets storage.
- `crates/hydragent-skills/`: Skill auto-induction and the 7-day Curator.
- `crates/hydragent-swarm/`: Subagent spawning and Model Council.
- `adapters/channels/`: All channel adapters (Telegram, Discord, Slack, etc.).
- `adapters/utils/`: All background utilities and helper scripts.
- `skills/builtin/`: Built-in YAML skill manifests.
- `config/`: Configuration files (`SOUL.md`, `USER.md`, `model_council.yaml`).

### 3.2 Development Loop

#### Building the Workspace
```bash
cargo build                      # Build all crates
cargo build -p hydragent-core    # Build the kernel only
```

#### Running Tests
```bash
# Run kernel unit tests
cargo test -p hydragent-core --bin hydragent

# Run specific crate tests
cargo test -p hydragent-vault
cargo test -p hydragent-skills

# Run Python SDK tests
python -m unittest discover -s adapters/tests
```

### 3.3 Contribution Rules
1. **No direct credential logging**: All secrets must be masked in debug/log outputs.
2. **Keep the binary footprint small**: Avoid adding large dependencies to the core crates.
3. **Write tests**: Every new feature or tool must have corresponding unit or integration tests.
4. **Document changes**: Update `doc/ARCHITECTURE.md` or `doc/FEATURES.md` if your change alters the system's design or capabilities.

---

## 4. Changelog Summary

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
