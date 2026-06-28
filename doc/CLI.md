# Hydragent CLI Reference & Use Cases

Hydragent provides two distinct command-line interfaces (CLIs), each optimized for different use cases and environments. Understanding the division of labor between them is key to integrating Hydragent into your workflow.

---

## 1. The Architecture at a Glance

Hydragent uses a **client-server (daemon-adapter) architecture**:

```
 ┌────────────────────────────────────────────────────────┐
 │                 Rust Core (The Server)                 │
 │                                                        │
 │  ┌─────────────────┐             ┌──────────────────┐  │
 │  │   Local Admin   │             │  Gateway Daemon  │  │
 │  │  (doctor, etc.) │             │ (serve on :5000) │  │
 │  └─────────────────┘             └────────┬─────────┘  │
 └───────────────────────────────────────────┼────────────┘
                                             ▲
                                  JSON-RPC   │ (TCP Loopback)
                                             ▼
 ┌────────────────────────────────────────────────────────┐
 │                 Python SDK (The Client)                │
 │                                                        │
 │  ┌─────────────────┐             ┌──────────────────┐  │
 │  │    hydra-cli    │             │ Channel Adapters │  │
 │  │  (send, chat)   │             │ (Telegram, etc.) │  │
 │  └─────────────────┘             └──────────────────┘  │
 └────────────────────────────────────────────────────────┘
```

---

## 2. Rust CLI: `hydragent` (The Engine & Admin Tool)

The Rust CLI is the **core engine and local administration tool**. It compiles to a single, fast, self-contained binary.

### Key Use Cases
- **Daemon Hosting**: Starting the gateway (`hydragent serve`) which runs the background event bus, model router, memory layers, and tools.
- **Local Setup & Diagnostics**: Guiding first-time configuration (`onboard`) and verifying system health/dependencies (`doctor`).
- **Low-Latency Terminal Chat**: Direct local REPL session (`chat`) running directly inside the core orchestrator without needing an external event bus.
- **Data & Security Management**: Creating/reading credentials in the encrypted `vault`, indexing/wiping long-term `memory`, or running security audits (`security`).
- **Process Management**: Checking running instances (`ps`) and stopping them (`stop`).

### Command Reference

| Command | Usage | Description |
|---|---|---|
| `hydragent onboard` | `hydragent onboard [--provider <name>] [--api-key <key>]` | Guided first-time setup (creates `.env`, configures LLM, verifies connection). |
| `hydragent doctor` | `hydragent doctor` | Runs ~10 local diagnostic checks (health, paths, environment) and prints a color-coded report. |
| `hydragent serve` | `hydragent serve` | Starts the gateway daemon (bus + dream worker + tool registry) on port `5000`. (Default if `.env` exists). |
| `hydragent chat` | `hydragent chat [--page <id>]` | Starts a direct, local interactive REPL in your terminal. |
| `hydragent ps` | `hydragent ps` | Lists all active `hydragent` processes, their PIDs, ports, and uptimes. |
| `hydragent stop` | `hydragent stop [pid]` | Stops a specific gateway instance, or all instances if no PID is provided. |
| `hydragent status` | `hydragent status` | Displays a single-screen live dashboard of the gateway, memory, and active pages. |
| `hydragent vault` | `hydragent vault <action>` | Manages the secure credential vault (`set`, `get`, `list`, `delete`). |
| `hydragent memory` | `hydragent memory <action>` | Manages long-term semantic memory (`query`, `purge`, `status`). |
| `hydragent security` | `hydragent security <action>` | Runs manual security scans, inspects the Merkle audit chain, or lists taint patterns. |

---

## 3. Python CLI: `hydra-cli` (The Client & Integration Tool)

The Python CLI is a **lightweight client adapter** that connects to a running Hydragent gateway daemon over TCP.

### Key Use Cases
- **Scripting & CI/CD**: Sending a single prompt to the running agent from a shell script or CI pipeline and receiving the plain-text answer (`hydra-cli send "prompt"`).
- **Remote / Decoupled Interaction**: Interacting with a Hydragent gateway running on a different port, container, or machine.
- **Session Sharing**: Connecting multiple terminal sessions to the same running gateway page session (`hydra-cli --page <id> chat`).

### Command Reference

| Command / Option | Usage | Description |
|---|---|---|
| `--page <id>` | `hydra-cli --page my-session chat` | Sets the session/page ID (resumes or starts a specific conversation). |
| `--host <host>` | `hydra-cli --host 192.168.1.50 chat` | Connects to a gateway running on a specific IP (default: `127.0.0.1`). |
| `--port <port>` | `hydra-cli --port 5001 chat` | Connects to a gateway running on a specific port (default: `5000`). |
| `chat` / `repl` | `hydra-cli chat` | Starts an interactive client REPL session. |
| `send` | `hydra-cli send "summarise the current folder"` | Sends a single prompt to the gateway, prints the response, and exits. |

---

## 4. Summary: Which one should I use?

- Use **`hydragent`** when you want to start the agent server, configure the system, manage your database/vault, or run a standalone local chat.
- Use **`hydra-cli`** when you want to automate agent prompts via scripts, run quick command-line queries, or connect to a gateway running in another process/machine.
