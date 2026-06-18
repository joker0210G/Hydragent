// crates/hydragent-core/src/main.rs
pub mod cli_repl;
pub mod doctor;
pub mod examples;
pub mod markdown_render;
pub mod onboard;
pub mod orchestrator;
pub mod react_loop;
pub mod session;
pub mod logger;
pub mod config;
pub mod dream;
pub mod status_bar;
pub mod strategy;
pub mod swarm_runner;
pub mod skill_induction;
pub mod tui_header;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use hydragent_memory::SessionStore;
use hydragent_tools::registry::ToolRegistry;
use hydragent_tools::echo::EchoTool;
use hydragent_tools::web_search::WebSearchTool;
use hydragent_tools::agent_reach::AgentReachTool;
use hydragent_tools::file_read::FileReadTool;
use hydragent_tools::memory_store::MemoryStoreTool;
use hydragent_tools::memory_search::MemorySearchTool;
use hydragent_tools::memory_forget::MemoryForgetTool;
use hydragent_tools::phase6::AuditQueryTool;
use hydragent_tools::phase6::SanitizerListPatternsTool;
use hydragent_tools::phase6::SanitizerScanTool;
use hydragent_tools::phase6::TaintCheckTool;
use hydragent_tools::phase6::VaultRotateTool;
use tracing::{info, error};

struct SandboxedTool {
    name: String,
    description: String,
    schema: String,
    wasm_tool: hydragent_sandbox::WasmTool,
    tier: hydragent_types::PermissionTier,
}

#[async_trait::async_trait]
impl hydragent_tools::tool_trait::Tool for SandboxedTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn params_schema(&self) -> &str {
        &self.schema
    }
    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        self.tier
    }
    async fn execute(&self, params_json: &str) -> hydragent_types::ToolResult {
        match self.wasm_tool.execute(params_json).await {
            Ok(res) => res,
            Err(e) => hydragent_types::ToolResult {
                call_id: "".to_string(),
                output_json: "".to_string(),
                status: hydragent_types::ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some(format!("Sandbox execution failed: {}", e)),
            }
        }
    }
}


#[derive(Parser, Debug)]
#[command(
    name = "hydragent",
    author,
    version,
    about = "Hydragent AI Agent core runtime",
    long_about = "Hydragent is a privacy-first, model-agnostic AI agent runtime.\n\
                  Run `hydragent onboard` for a guided first-time setup, or\n\
                  `hydragent doctor` to diagnose an existing installation.\n\
                  Run `hydragent chat` for an interactive terminal REPL.\n\
                  Run `hydragent serve` (or `hydragent` with no subcommand)\n\
                  to start the gateway — bus + dream worker + tool registry —\n\
                  so adapters (Telegram, Discord, Slack, CLI, …) can connect.\n\n\
                  Process management:\n\
                  \x20 hydragent ps               list running instances\n\
                  \x20 hydragent stop [pid]        stop one or all instances\n\
                  \x20 hydragent status            one-shot dashboard"
)]
struct Args {
    /// Resumes or starts a specific chat page by ID
    #[arg(short, long)]
    page: Option<String>,

    /// Lists all past conversation pages stored in the SQLite database and exits
    #[arg(long)]
    list_pages: bool,

    /// Enable verbose diagnostic output: forces LOG_LEVEL=debug AND prints
    /// a structured dump of every relevant environment variable, the .env
    /// file location, and the resolved brain config. Works with any
    /// subcommand. Use this to figure out "why isn't my .env loading?"
    /// or "which API key is the brain actually using?".
    #[arg(long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug, Clone)]
enum Commands {
    /// 🐣 Guided first-time setup (creates .env, picks provider, writes config)
    #[command(
        long_about = "Walks you through creating a `.env` file from scratch.\n\
                      Picks a provider (OpenAI, OpenRouter, Together, Groq,\n\
                      Ollama, LM Studio, or a custom OpenAI-compatible URL),\n\
                      prompts for an API key, picks a primary model, and\n\
                      optionally runs `test-brain` to verify the connection.\n\n\
                      Non-interactive mode (for CI / scripts):\n\
                      \x20 hydragent onboard --provider openrouter --api-key $KEY \\\n\
                      \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20 --model openai/gpt-4o-mini --non-interactive --no-verify"
    )]
    Onboard {
        /// Provider preset: openai, openrouter, together, groq, ollama, lmstudio, custom
        #[arg(long)]
        provider: Option<String>,

        /// API key (skip the masked prompt; do not commit secrets in CI logs!)
        #[arg(long)]
        api_key: Option<String>,

        /// Primary model name (provider-specific)
        #[arg(long)]
        model: Option<String>,

        /// Run without interactive prompts (requires --provider / --api-key / --model)
        #[arg(long, short = 'y')]
        non_interactive: bool,

        /// Skip the live `test-brain` verification step
        #[arg(long)]
        no_verify: bool,

        /// Overwrite an existing .env (default: update in place, prompting)
        #[arg(long)]
        force: bool,
    },
    /// 🩺 Run diagnostic checks and print a colour-coded report
    #[command(
        long_about = "Runs ~10 file-based checks (no network) and prints a\n\
                      green/yellow/red report with a one-line fix hint for\n\
                      every failure. Exits non-zero only if any check fails."
    )]
    Doctor,
    /// 💬 Start an interactive terminal chat (REPL)
    #[command(
        long_about = "Starts a minimal interactive REPL. Wires the same\n\
                      `run_react_loop` used by the channel adapters so you\n\
                      can chat from the terminal without the Python CLI\n\
                      adapter. Type /help inside the REPL for commands."
    )]
    Chat,
    /// 💡 Show example prompts you can paste into `hydragent chat`
    #[command(
        long_about = "Prints a small catalogue of starter prompts, each\n\
                      annotated with the tools it exercises. Useful as a\n\
                      smoke test after `onboard`."
    )]
    Examples {
        /// Optional substring filter on tool names (e.g. `memory`, `audit`)
        filter: Option<String>,
    },
    /// Manage long-term memory
    #[command(
        long_about = "Inspect or wipe the long-term memory stored in\n\
                      `data/sessions.db` (semantic memories only — pages\n\
                      and turns are managed by the chat adapter)."
    )]
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Manage local vector embeddings
    Embed {
        #[command(subcommand)]
        action: EmbedAction,
    },
    /// Manage the encrypted credential vault
    #[command(
        long_about = "Create, read, update, delete entries in the encrypted\n\
                      credential vault at `data/vault/.hydravault`. Requires\n\
                      `HYDRAGENT_VAULT_PASSPHRASE` (or an interactive prompt)."
    )]
    Vault {
        #[command(subcommand)]
        action: VaultAction,
    },
    /// 🧠 Send a real prompt to the live brain and stream the reply
    /// (a real-time end-to-end test of the swappable BRAIN_* config)
    #[command(
        long_about = "Streams a single prompt through the live brain and\n\
                      prints the response token-by-token. This is the\n\
                      quickest way to confirm a freshly pasted key\n\
                      actually works end-to-end.\n\n\
                      Example:\n\
                      \x20 hydragent test-brain \"Reply with exactly: PONG\""
    )]
    TestBrain {
        /// The prompt to send. Default: a one-liner that asks the model
        /// to introduce itself so you can confirm the brain is wired up.
        #[arg(default_value = "In one sentence, who are you and which model are you?")]
        prompt: String,
    },
    /// Inspect the Phase 6 Merkle audit chain (Track 6.1)
    Audit {
        #[command(subcommand)]
        action: AuditAction,
    },
    /// 🛡 Phase 6 user-callable security surface (Tracks 6.1–6.4)
    #[command(
        long_about = "Direct (no-LLM) surface for inspecting and exercising\n\
                      every Phase 6 feature from the terminal. Mirrors the\n\
                      LLM-callable tools so you can verify behaviour before\n\
                      relying on the chat adapter to wire it up.\n\n\
                      `hydragent security status` is the canonical\n\
                      \"is everything wired up?\" smoke test."
    )]
    Security {
        #[command(subcommand)]
        action: SecurityAction,
    },
    /// 🛰 Start the gateway (bus + dream worker + tool registry)
    ///
    /// Explicit alias for the no-subcommand default. Use `hydragent serve`
    /// in scripts and systemd/Scheduled-Task units where it's clearer that
    /// the process is a long-running daemon, not an interactive command.
    #[command(
        long_about = "Starts the Hydragent gateway and blocks forever.\n\
                      Identical to running `hydragent` with no subcommand:\n\
                      opens the event bus on 127.0.0.1:5000, starts the\n\
                      dream worker, registers every tool, and accepts\n\
                      connections from channel adapters (Telegram,\n\
                      Discord, Slack, WebSocket, …).\n\n\
                      Connection refused? Run `hydragent ps` — another\n\
                      gateway may already be holding port 5000."
    )]
    Serve,
    /// 📋 List running hydragent instances
    #[command(
        long_about = "Lists every hydragent.exe process on this host:\n\
                      PID, parent PID, start time, uptime, the TCP port\n\
                      each one holds (default 5000), and the binary path.\n\
                      Mirrors `ollama ps` and Unix `ps`.\n\n\
                      Example output:\n\
                      \x20 PID     PPID    PORT     STARTED              UPTIME    CMD\n\
                      \x20 11980   1       5000     2026-06-18 10:37:43  4m12s     target\\release\\hydragent.exe serve"
    )]
    Ps,
    /// 🛑 Stop one or all running hydragent instances
    #[command(
        long_about = "Sends a kill signal to the named PID (Windows: taskkill /F).\n\
                      With no PID, stops ALL hydragent instances on this\n\
                      host. Use `hydragent ps` first to confirm what will\n\
                      be stopped.\n\n\
                      Examples:\n\
                      \x20 hydragent stop             ← stop everything\n\
                      \x20 hydragent stop 11980       ← stop just PID 11980"
    )]
    Stop {
        /// PID to stop. Omit to stop ALL hydragent instances.
        pid: Option<u32>,
    },
    /// 📊 One-shot dashboard: gateway, brain, storage, active pages
    #[command(
        long_about = "Prints a single-screen summary of:\n\
                      \x20 • Gateway state (port 5000) + PID + uptime\n\
                      \x20 • Brain model + provider\n\
                      \x20 • Dream worker state\n\
                      \x20 • Active chat pages + storage size\n\
                      Designed as the first thing to run after a fresh\n\
                      install, and the answer to \"is everything wired up?\"."
    )]
    Status,
    /// 🌙 Manage the memory-consolidation dream cycle
    #[command(
        long_about = "Inspect or change the background dream-cycle settings.\n\
                      Without flags, prints the current state.\n\n\
                      Examples:\n\
                      \x20 hydragent dream                   ← show current status\n\
                      \x20 hydragent dream --enable           ← turn dreaming on\n\
                      \x20 hydragent dream --disable          ← turn dreaming off\n\
                      \x20 hydragent dream --interval 300     ← run every 5 minutes\n\
                      \x20 hydragent dream --enable --interval 120"
    )]
    Dream {
        /// Enable the dream cycle (persists to .env as ENABLE_DREAMING=true)
        #[arg(long, conflicts_with = "disable")]
        enable: bool,

        /// Disable the dream cycle (persists to .env as ENABLE_DREAMING=false)
        #[arg(long, conflicts_with = "enable")]
        disable: bool,

        /// Set the interval between dream cycles in seconds
        /// (persists to .env as DREAMING_INTERVAL_SEC=<N>)
        #[arg(long, value_name = "SECS")]
        interval: Option<u64>,
    },
}

#[derive(clap::Subcommand, Debug, Clone)]
enum VaultAction {
    /// Initialize a new encrypted vault
    Init,
    /// Store a credential in the vault
    Set {
        scope: String,
        value: String,
    },
    /// Retrieve a credential from the vault
    Get {
        scope: String,
    },
    /// List all credential scopes in the vault
    List,
    /// Delete a credential from the vault
    Delete {
        scope: String,
    },
}

#[derive(clap::Subcommand, Debug, Clone)]
enum MemoryAction {
    /// List all stored memories
    List,
    /// Clear all stored memories
    Clear,
}

#[derive(clap::Subcommand, Debug, Clone)]
enum EmbedAction {
    /// Calculate cosine similarity between two sentences
    Compare {
        text1: String,
        text2: String,
    },
}

#[derive(clap::Subcommand, Debug, Clone)]
enum AuditAction {
    /// List recent audit events (default: oldest first, 20 rows)
    List {
        /// Maximum number of rows to print
        #[arg(long, default_value = "20")]
        limit: u32,
        /// Skip the first N rows
        #[arg(long, default_value = "0")]
        offset: u32,
        /// Newest first
        #[arg(long)]
        reverse: bool,
    },
    /// Print the current chain head hash (or "none" if the chain is empty)
    Head,
    /// Verify the integrity of the audit chain
    Verify {
        /// Also verify Ed25519 signatures against the agent's public key
        #[arg(long)]
        signatures: bool,
        /// Optional path to a PEM public key file to verify against
        /// (defaults to the agent's own key). Useful for auditors.
        #[arg(long)]
        public_key: Option<PathBuf>,
    },
}

#[derive(clap::Subcommand, Debug, Clone)]
enum SecurityAction {
    /// Show an overview of every Phase 6 subsystem and its current state
    /// (audit chain head, mlock availability, sanitizer pattern count, taint
    /// policy version). Useful as a single-shot "is everything wired up?"
    /// smoke test.
    Status,
    /// Dry-run a Phase 6.2 taint policy decision. Pass `--sink` and one
    /// or more `--category` flags. Returns the verdict (allow / redact /
    /// block) and the rule that matched.
    TaintCheck {
        #[arg(long, value_parser = [
            "llm_request_messages", "llm",
            "outbound_network", "network",
            "log",
            "vault_write", "vault",
            "disk_write", "disk",
        ])]
        sink: String,
        #[arg(long, num_args = 1.., value_parser = [
            "secret", "pii", "tool_output", "user_input", "llm_output", "system_internal",
        ])]
        category: Vec<String>,
    },
    /// Show the active Phase 6.2 taint sink policy (version, sinks, rules).
    TaintPolicy,
    /// Scan a free-text input against the Phase 6.3 prompt-injection
    /// pattern library and print the verdict.
    SanitizerScan {
        /// The text to scan.
        text: String,
    },
    /// List the loaded prompt-injection patterns (id, severity, description).
    SanitizerPatterns,
    /// Print a summary of the Phase 6.4 vault: path, exists?, mlock
    /// availability, column-key presence.
    VaultStatus,
    /// Initialize the Phase 6.4 encrypted vault (creates
    /// `data/vault/.hydravault`). The master passphrase is read from
    /// `HYDRAGENT_VAULT_PASSPHRASE` or prompted for. Idempotent: if
    /// the vault file already exists, this is a no-op (use
    /// `security vault-rotate-passphrase` to change the passphrase).
    VaultInit,
    /// Rotate the vault's master passphrase. The new passphrase is
    /// supplied via `--new-passphrase`; the current passphrase is read
    /// from `HYDRAGENT_VAULT_PASSPHRASE` or prompted for.
    VaultRotatePassphrase {
        #[arg(long)]
        new_passphrase: String,
    },
    /// Rotate the AES column key. The current passphrase is read from
    /// `HYDRAGENT_VAULT_PASSPHRASE` or prompted for. **Warning**: this
    /// invalidates all previously column-encrypted data.
    VaultRotateColumnKey,
}

/// Built-in prompt-injection patterns used as a fallback when
/// `config/security/injection_patterns.yaml` is missing. Mirrors the
/// 5-pattern library used by the `phase6` LLM tool.
const BUILTIN_INJECTION_PATTERNS: &str = r#"
version: 1
patterns:
  - id: ignore_prev
    severity: high
    regex: '(?i)ignore (?:all|any|previous|prior) (?:instructions|directives|prompts)'
    description: 'Attempt to discard prior system/user instructions'
  - id: reveal_system
    severity: high
    regex: '(?i)(?:reveal|show|print|leak|dump)\s+(?:the\s+)?(?:system|hidden|secret|internal)\s+prompt'
    description: 'Attempt to extract the system prompt'
  - id: jailbreak_roleplay
    severity: high
    regex: '(?i)(?:you are now|act as|pretend to be)\s+(?:DAN|do anything now|jailbroken|unfiltered)'
    description: 'Known jailbreak roleplay trigger'
  - id: override_guards
    severity: high
    regex: '(?i)disable (?:safety|filters|guardrails|content[- ]?policy)'
    description: 'Attempt to disable content filters'
  - id: exfil
    severity: medium
    regex: '(?i)(?:curl|wget|fetch)\s+https?://[^\s]+'
    description: 'Shell-style outbound exfiltration request'
"#;

/// Print a friendly welcome banner for first-time users who ran the
/// binary with no `.env` and no subcommand. Goal: never let the first
/// `hydragent` invocation crash silently with a config error.
fn print_first_run_banner() {
    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════════╗");
    eprintln!("║  🐉  Welcome to Hydragent!                                       ║");
    eprintln!("╚══════════════════════════════════════════════════════════════════╝");
    eprintln!();
    eprintln!("  It looks like this is your first time here — no `.env` file was");
    eprintln!("  found in the current directory. Let's fix that.");
    eprintln!();
    eprintln!("  Quickest path:");
    eprintln!();
    eprintln!("     hydragent onboard          ← guided setup (recommended)");
    eprintln!();
    eprintln!("  After onboard, you'll be one command away from chatting:");
    eprintln!();
    eprintln!("     hydragent chat             ← interactive terminal REPL");
    eprintln!();
    eprintln!("  Other useful commands:");
    eprintln!();
    eprintln!("     hydragent doctor           ← diagnose an existing setup");
    eprintln!("     hydragent test-brain       ← smoke-test the live brain");
    eprintln!("     hydragent examples         ← starter prompts to try");
    eprintln!("     hydragent --help           ← full command reference");
    eprintln!();
    eprintln!("  Manual setup: copy `.env.example` to `.env` and fill in BRAIN_BASE");
    eprintln!("  + BRAIN_KEY, then run `hydragent chat`.");
    eprintln!();
}

/// Print a structured dump of every relevant environment variable, the
/// `.env` file the binary actually loaded, and the resolved brain
/// config. Triggered by `--debug`. The goal is to make "why is my .env
/// not loading?" answerable in 5 seconds.
fn debug_dump_env_and_config(cfg: &config::AppConfig) {
    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════════╗");
    eprintln!("║   🔍 --debug mode: environment + config diagnostic dump         ║");
    eprintln!("╚══════════════════════════════════════════════════════════════════╝");

    // [1] working directory + binary path
    eprintln!();
    eprintln!("[1] Process");
    eprintln!("    cwd       : {}", std::env::current_dir()
        .map(|p| p.display().to_string()).unwrap_or_else(|e| format!("<err: {}>", e)));
    match std::env::current_exe() {
        Ok(p) => eprintln!("    binary    : {}", p.display()),
        Err(e) => eprintln!("    binary    : <err: {}>", e),
    }

    // [2] .env file
    eprintln!();
    eprintln!("[2] .env file (loaded by `dotenvy::dotenv()`)");
    match dotenvy::dotenv() {
        Ok(path) => {
            eprintln!("    status    : LOADED ✓");
            eprintln!("    path      : {}", path.display());
            match std::fs::metadata(&path) {
                Ok(md) => {
                    eprintln!("    size      : {} bytes", md.len());
                    eprintln!("    modified  : {:?}", md.modified().ok());
                }
                Err(e) => eprintln!("    stat      : <err: {}>", e),
            }
        }
        Err(e) => {
            eprintln!("    status    : NOT FOUND ✗");
            eprintln!("    reason    : {}", e);
            eprintln!("    hint      : make sure `.env` exists in the cwd, not `.env_` or `.env.example`");
        }
    }

    // [3] interesting env vars (raw, before vault)
    eprintln!();
    eprintln!("[3] Environment variables (raw — before vault overrides)");
    let interesting: &[(&str, &str)] = &[
        ("BRAIN_BASE",            "plain"),
        ("BRAIN_KEY",             "secret"),
        ("BRAIN_MODEL",           "plain"),
        ("BRAIN_FALLBACKS",       "plain"),
        ("OPENROUTER_API_KEYS",   "secret"),
        ("PRIMARY_MODEL",         "plain"),
        ("FALLBACK_MODELS",       "plain"),
        ("HYDRAGENT_VAULT_PASSPHRASE", "secret"),
        ("DATA_DIR",              "plain"),
        ("LOG_LEVEL",             "plain"),
        ("LOG_FORMAT",            "plain"),
        ("MAX_REACT_STEPS",       "plain"),
        ("RUST_LOG",              "plain"),
    ];
    for (var, kind) in interesting {
        match std::env::var(var) {
            Ok(val) => {
                let display = match *kind {
                    "secret" => mask_secret(&val),
                    _ if val.len() > 200 => format!("{}…  ({} chars)", &val[..200], val.len()),
                    _ => val,
                };
                eprintln!("    {:<32} = {}", var, display);
            }
            Err(_) => eprintln!("    {:<32} = <unset>", var),
        }
    }

    // [3b] KNOWN env vars the codebase reads but were NOT in the basic
    // [3] list. Each entry is tagged [REQUIRED] (binary will misbehave
    // if unset for the relevant feature) or [OPTIONAL  default=…]
    // (the code has a default baked in).
    eprintln!();
    eprintln!("[3b] Other env vars the codebase reads (completeness audit)");
    let known: &[(&str, &str)] = &[
        // ── AppConfig fields (have set_default() in config.rs) ───────
        ("BUS_PORT",               "OPTIONAL  default=5000"),
        ("ENABLE_DREAMING",        "OPTIONAL  default=true"),
        ("DREAMING_INTERVAL_SEC",  "OPTIONAL  default=60"),
        ("MAX_SEMANTIC_MEMORIES",  "OPTIONAL  default=1000000"),
        // ── Telegram adapter ─────────────────────────────────────────
        ("TELEGRAM_BOT_TOKEN",     "OPTIONAL  required only for Telegram adapter"),
        ("TELEGRAM_ALLOWED_CHAT_IDS", "OPTIONAL  required only for Telegram adapter"),
        ("TELEGRAM_WEBAPP_URL",    "OPTIONAL  required only for Telegram adapter"),
        // ── Sandbox / Work IQ / Memory / Compaction ──────────────────
        ("WORKSPACE_DIR",          "OPTIONAL  default=\".\""),
        ("ENFORCE_SANDBOX",        "OPTIONAL  default=false"),
        ("WORK_IQ_POLL_INTERVAL_SEC", "OPTIONAL  default=300"),
        ("MEMORY_CONTEXT_TOKEN_LIMIT", "OPTIONAL  default=1000"),
        ("PAGE_COMPACTION_LIMIT",  "OPTIONAL  default=30"),
        // ── Embedding model download (only runs on first use) ───────
        ("EMBEDDING_MODEL_URL",    "OPTIONAL  default=huggingface.co/.../all-MiniLM-L6-v2/model.safetensors"),
        ("EMBEDDING_TOKENIZER_URL", "OPTIONAL  default=huggingface.co/.../all-MiniLM-L6-v2/tokenizer.json"),
        // ── Web search tool (SearXNG) ────────────────────────────────
        ("SEARXNG_BASE_URL",       "OPTIONAL  required only for web_search tool"),
        ("SEARXNG_MAX_RESULTS",    "OPTIONAL  required only for web_search tool"),
        ("SEARXNG_TIMEOUT_SECS",   "OPTIONAL  required only for web_search tool"),
        ("SEARXNG_CATEGORIES",     "OPTIONAL  required only for web_search tool"),
        ("SEARXNG_LANGUAGE",       "OPTIONAL  required only for web_search tool"),
        // ── custom_openai provider (alt to BRAIN_*) ──────────────────
        ("CUSTOM_API_KEY",         "OPTIONAL  required only if using custom_openai provider"),
        ("CUSTOM_API_BASE",        "OPTIONAL  required only if using custom_openai provider"),
    ];
    let mut missing: Vec<&str> = Vec::new();
    for (var, note) in known {
        match std::env::var(var) {
            Ok(val) => {
                let display = if val.len() > 80 {
                    format!("{}…  ({} chars)", &val[..80], val.len())
                } else {
                    val
                };
                eprintln!("    [set]     {:<32} = {}    ({})", var, display, note);
            }
            Err(_) => {
                eprintln!("    [UNSET]   {:<32}                                       ({})", var, note);
                missing.push(var);
            }
        }
    }
    eprintln!();
    if missing.is_empty() {
        eprintln!("    ✓ all known env vars are set (none missing)");
    } else {
        eprintln!(
            "    → {} env var(s) unset, but ALL have defaults — binary will not fail.",
            missing.len()
        );
        eprintln!("    → If a feature misbehaves, check that its required var is set:");
        eprintln!("      vault ops → HYDRAGENT_VAULT_PASSPHRASE");
        eprintln!("      telegram  → TELEGRAM_BOT_TOKEN + TELEGRAM_ALLOWED_CHAT_IDS");
        eprintln!("      web search→ SEARXNG_BASE_URL");
        eprintln!("      embedding → EMBEDDING_MODEL_URL (only if HuggingFace blocked)");
    }

    // [4] effective brain config (after fallbacks)
    eprintln!();
    eprintln!("[4] Effective brain config (after fallbacks)");
    eprintln!("    brain_base      : {}", cfg.effective_brain_base());
    eprintln!("    brain_key       : {}", mask_secret(&cfg.effective_brain_key()));
    eprintln!("    brain_model     : {}", cfg.effective_brain_model());
    eprintln!("    brain_fallbacks : {:?}", cfg.effective_brain_fallbacks());

    // [5] app config
    eprintln!();
    eprintln!("[5] AppConfig (resolved)");
    eprintln!("    log_format      : {}", cfg.log_format);
    eprintln!("    log_level       : {}  (FORCED to debug by --debug)", cfg.log_level);
    eprintln!("    data_dir        : {}", cfg.data_dir);
    eprintln!("    max_react_steps : {}", cfg.max_react_steps);
    eprintln!("    bus_port        : {}", cfg.bus_port);
    eprintln!("    enable_dreaming : {}", cfg.enable_dreaming);
    eprintln!();
    eprintln!("--- end of --debug dump ---");
    eprintln!();
}

/// Mask a secret value for display: show the first 4 and last 4 chars,
/// plus the total length. Returns "<empty>" for empty input, "<set> (N
/// chars)" if the secret is very short.
fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return "<empty>".into();
    }
    let n = s.chars().count();
    if n <= 12 {
        return format!("<set> ({} chars)", n);
    }
    let head: String = s.chars().take(4).collect();
    let tail: String = s.chars().rev().take(4).collect::<String>().chars().rev().collect();
    format!("{}…{}  ({} chars)", head, tail, n)
}

/// Load the agent's Ed25519 keypair from disk, or generate a fresh one
/// and persist it (Track 6.1 — Phase 6.4 will move the private key into
/// the encrypted vault).
///
/// On first run this prints a one-line notice so the operator knows a
/// key was minted; subsequent runs are silent.
fn load_or_create_agent_signer(
    key_path: &std::path::Path,
    pub_path: &std::path::Path,
) -> anyhow::Result<hydragent_security::AgentSigner> {
    if key_path.exists() {
        let bytes = std::fs::read(key_path)?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("agent key file has wrong length: {}", bytes.len()))?;
        return Ok(hydragent_security::AgentSigner::from_bytes(&arr));
    }
    let signer = hydragent_security::AgentSigner::generate();
    std::fs::write(key_path, signer.private_key_bytes())?;
    if let Some(parent) = pub_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    signer
        .export_public_key(pub_path)
        .map_err(|e| anyhow::anyhow!("failed to export public key: {e}"))?;
    eprintln!(
        "🪪 Generated new agent Ed25519 keypair.\n   private: {}\n   public : {}",
        key_path.display(),
        pub_path.display()
    );
    Ok(signer)
}

// ─────────────────────────────────────────────────────────────────────
// `ps` / `stop` / `status` helpers
// ─────────────────────────────────────────────────────────────────────
//
// Ollama-style process introspection. Cross-platform shim: on Windows
// we shell out to PowerShell (the `Get-NetTCPConnection` cmdlet is the
// canonical way to map PID ↔ local port without a `windows` crate
// dep); on Linux/macOS we use `lsof -nP -iTCP:<port> -sTCP:LISTEN`
// which is pre-installed on both.
//
// `ps` walks every `hydragent` process and prints PID + parent PID +
// start time + uptime + listening port + binary path. `stop` sends
// SIGTERM-equivalent (taskkill /F on Windows, SIGTERM on Unix) to a
// given PID, or all hydragent instances if none is supplied.
// `status` prints a single-screen dashboard suitable for
// "is everything wired up?" smoke tests.

#[cfg(target_os = "windows")]
fn ps_script() -> &'static str {
    // Get-Process hydragent with: PID, PPID (via WMI CIM), StartTime, Path,
    // and Listening port (cross-referenced via Get-NetTCPConnection).
    r#"
$ErrorActionPreference = 'SilentlyContinue'
$procs = Get-Process -Name hydragent -ErrorAction SilentlyContinue
if (-not $procs) { Write-Output '<no hydragent processes>'; exit }
$listeners = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
    Group-Object OwningProcess -AsHashTable -AsString
$rows = foreach ($p in $procs) {
    $port = ''
    if ($listeners.ContainsKey([string]$p.Id)) {
        $port = ($listeners[[string]$p.Id] | Select-Object -First 1).LocalPort
    }
    $uptime = (Get-Date) - $p.StartTime
    $uptime_str = if ($uptime.TotalHours -ge 1) {
        '{0}h{1:D2}m' -f [int]$uptime.TotalHours, [int]$uptime.Minutes
    } else {
        '{0}m{1:D2}s' -f [int]$uptime.TotalMinutes, [int]$uptime.Seconds
    }
    [pscustomobject]@{
        PID      = $p.Id
        Port     = if ($port) { $port } else { '—' }
        Started  = $p.StartTime.ToString('yyyy-MM-dd HH:mm:ss')
        Uptime   = $uptime_str
        Command  = $p.Path
    }
}
$rows | Format-Table -AutoSize | Out-String
"#
}

#[cfg(not(target_os = "windows"))]
fn ps_script() -> &'static str {
    // ps + lsof fallback for Linux/macOS. We avoid the `sysinfo` crate
    // to keep this file dependency-light.
    r#"
if ! command -v ps >/dev/null; then
  echo "<ps not installed>"
  exit 1
fi
printf "%-8s %-8s %-8s %-20s %-10s %s\n" PID PPID PORT STARTED UPTIME COMMAND
ps -eo pid,ppid,etime,comm | awk '$4 ~ /hydragent/ {print $1, $2, $3, $4}'
"#
}

fn cmd_ps() {
    let script = ps_script();
    #[cfg(target_os = "windows")]
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output();
    #[cfg(not(target_os = "windows"))]
    let out = std::process::Command::new("sh")
        .args(["-c", script])
        .output();

    println!("------------------------------------------------------------------------");
    println!("  📋 Running hydragent instances");
    println!("------------------------------------------------------------------------");
    match out {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout);
            // PowerShell emits a leading blank line via Out-String; trim it
            let trimmed = s.trim_start_matches('\n').trim_start_matches('\r');
            println!("{trimmed}");
        }
        Err(e) => eprintln!("  ✗ failed to query processes: {e}"),
    }
}

fn cmd_stop(pid: Option<u32>) {
    match pid {
        Some(p) => stop_pids(&[p]),
        None => {
            // Resolve "all" → enumerate first
            #[cfg(target_os = "windows")]
            let out = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    "(Get-Process -Name hydragent -ErrorAction SilentlyContinue).Id",
                ])
                .output();
            #[cfg(not(target_os = "windows"))]
            let out = std::process::Command::new("sh")
                .args([
                    "-c",
                    "pgrep -x hydragent || ps -eo pid,comm | awk '$2 ~ /hydragent/ {print $1}'",
                ])
                .output();
            let pids: Vec<u32> = match out {
                Ok(o) => String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect(),
                Err(e) => {
                    eprintln!("  ✗ failed to enumerate processes: {e}");
                    std::process::exit(1);
                }
            };
            if pids.is_empty() {
                eprintln!("  (no hydragent processes to stop)");
                return;
            }
            stop_pids(&pids);
        }
    }
}

fn stop_pids(pids: &[u32]) {
    for &pid in pids {
        let result = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
        match result {
            Ok(out) if out.status.success() => {
                println!("  ✓ stopped PID {pid}");
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                eprintln!("  ✗ failed to stop PID {pid}: {err}");
            }
            Err(e) => eprintln!("  ✗ failed to invoke taskkill for PID {pid}: {e}"),
        }
    }
}

/// Try to find the PID that owns `port` (in LISTEN state). Returns
/// `Some(pid)` if found, `None` if the port is free or the lookup
/// failed. Used by the bus startup to detect "another hydragent is
/// already serving" so we can exit cleanly instead of erroring.
fn check_port_owner(port: u16) -> Option<u32> {
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "$c = Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1; \
             if ($c) {{ Write-Output $c.OwningProcess }}"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            s.parse().ok()
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // lsof -nP -iTCP:5000 -sTCP:LISTEN -F p  → "p<pid>"
        let out = std::process::Command::new("lsof")
            .args([
                "-nP",
                &format!("-iTCP:{port}"),
                "-sTCP:LISTEN",
                "-F",
                "p",
            ])
            .output()
            .ok()?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(rest) = line.strip_prefix('p') {
                return rest.parse().ok();
            }
        }
        None
    }
}

/// Probe whether `pid` is actually a hydragent process (matches the
/// executable name). Used after `check_port_owner` returns `Some`
/// to decide between "joining existing gateway" (exit 0) and
/// "port held by something else" (exit 1 with hint).
fn is_hydragent_pid(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue; \
             if ($p -and $p.ProcessName -eq 'hydragent') {{ Write-Output 'yes' }}"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .ok();
        match out {
            Some(o) => String::from_utf8_lossy(&o.stdout).trim() == "yes",
            None => false,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // /proc/<pid>/comm on Linux, ps -p on macOS
        #[cfg(target_os = "linux")]
        {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|s| s.trim() == "hydragent")
                .unwrap_or(false)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let out = std::process::Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "comm="])
                .output()
                .ok();
            match out {
                Some(o) => String::from_utf8_lossy(&o.stdout).trim() == "hydragent",
                None => false,
            }
        }
    }
}

/// One-shot status dashboard. Tries to be useful even when the
/// gateway isn't running (so `hydragent status` is the first thing
/// to run on a fresh install).
fn cmd_status(app_config: &config::AppConfig) {
    let bus_port = app_config.bus_port;
    let pid = check_port_owner(bus_port);
    let (gw_state, gw_pid, gw_uptime) = match pid {
        Some(p) if is_hydragent_pid(p) => {
            // Look up uptime via the same ps script we use for `ps`
            #[cfg(target_os = "windows")]
            let up = {
                // We avoid `{0:D2}` in PowerShell because Rust's format!
                // macro sees the inner `{1:D2}` and complains. Instead we
                // call `.ToString('00')` which is a .NET format string
                // passed as a method argument — no inner braces.
                let script = format!(
                    "$p = Get-Process -Id {p} -ErrorAction SilentlyContinue; \
                     if ($p) {{ $u = (Get-Date) - $p.StartTime; \
                               $h = [int]$u.TotalHours; \
                               $m = [int]$u.Minutes; \
                               $s = [int]$u.Seconds; \
                               if ($h -ge 1) {{ Write-Output ($h.ToString() + 'h' + $m.ToString('00') + 'm') }} \
                               else {{ Write-Output ($m.ToString() + 'm' + $s.ToString('00') + 's') }} }}"
                );
                std::process::Command::new("powershell")
                    .args(["-NoProfile", "-Command", &script])
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default()
            };
            #[cfg(not(target_os = "windows"))]
            let up = String::new();
            ("🟢 running", Some(p), up)
        }
        Some(p) => (
            "🟡 port held by another process",
            Some(p),
            String::new(),
        ),
        None => ("🔴 offline", None, String::new()),
    };

    let brain = app_config.effective_brain_model();
    let brain_base = app_config.effective_brain_base();

    let dreaming = if app_config.enable_dreaming {
        format!(
            "🟢 every {}s",
            app_config.dreaming_interval_sec
        )
    } else {
        "🔴 disabled".to_string()
    };

    let db_path = format!("{}/sessions.db", app_config.data_dir);
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let db_size_human = human_bytes(db_size);

    println!("------------------------------------------------------------------------");
    println!("  🐉 Hydragent v{}", env!("CARGO_PKG_VERSION"));
    println!("------------------------------------------------------------------------");
    println!("  Gateway      : {}", gw_state);
    if let Some(p) = gw_pid {
        if !gw_uptime.is_empty() {
            println!("                 PID {p} uptime {gw_uptime}");
        } else {
            println!("                 PID {p}");
        }
    }
    println!("  Bus          : 127.0.0.1:{bus_port}");
    println!("  Brain        : {brain}");
    println!("                 via {brain_base}");
    println!("  Dream worker : {dreaming}");
    println!("  Storage      : {db_size_human}  ({db_path})");
    println!("------------------------------------------------------------------------");
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

#[tokio::main]
async fn main() {
    let start_time = std::time::Instant::now();
    let args = Args::parse();

    // ── Helpers used by the `chat` subcommand's startup banner ──────
    //
    // `short_id_pub` mirrors `cli_repl::short_id` (which is private to
    // that module) — we keep a tiny copy here so main.rs can build
    // a `BrandInfo` without exposing the helper or threading the full
    // `cli_repl` module surface into this dispatch.
    fn short_id_pub(id: &str) -> String {
        if id.len() <= 8 {
            id.to_string()
        } else {
            id[..8].to_string()
        }
    }

    // Best-effort `git rev-parse --abbrev-ref HEAD` with a 1s timeout.
    // We use a plain `std::process::Command` (not tokio::process) so
    // the helper stays sync and callable from any context; the
    // timeout is enforced by polling `try_wait` from another thread
    // — simpler than the `tokio::time::timeout` integration and
    // avoids pulling in another `use` block.
    fn detect_git_branch() -> String {
        // `Command` with no `.output()` already does what we want
        // when piped through a child thread: we get the output
        // and a join handle, and we can drop the process if it
        // takes too long. We use `Output` (not `status`) so the
        // stdout bytes are captured.
        use std::io::Read;
        use std::process::{Command, Stdio};
        let mut child = match Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return "unknown".to_string(),
        };
        // Wait up to 1s. We use `try_wait` + a small sleep because
        // `Command::wait_timeout` is unstable on Rust 1.81 stable.
        let start = std::time::Instant::now();
        let mut stdout_bytes = Vec::new();
        let mut timed_out = false;
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    if let Some(mut out) = child.stdout.take() {
                        let _ = out.read_to_end(&mut stdout_bytes);
                    }
                    break;
                }
                Ok(None) => {
                    if start.elapsed() > std::time::Duration::from_secs(1) {
                        timed_out = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => return "unknown".to_string(),
            }
        }
        if timed_out {
            // We deliberately *don't* kill the child — the user might
            // be running this command interactively, and a stray
            // background git process is harmless. The OS will reap it
            // when it exits. The branch display falls back to "unknown"
            // and the REPL continues.
            return "unknown".to_string();
        }
        let s = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
        if s.is_empty() || s == "HEAD" {
            // "HEAD" is what detached checkouts return. We still
            // show the long SHA so the user knows what they're on.
            if let Ok(child2) = Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
            {
                let s2 = String::from_utf8_lossy(&child2.stdout).trim().to_string();
                if !s2.is_empty() {
                    return format!("detached@{}", s2);
                }
            }
            return "unknown".to_string();
        }
        s
    }

    // ── First-run interception ────────────────────────────────────────
    //
    // When the user runs `hydragent` with no subcommand and no flag, the
    // bare command starts the bus server. That's a *valid* use case (run
    // the bus, then connect adapters), but it's also a trap: a brand-new
    // user who runs `hydragent` with no `.env` and no idea what to do
    // would just see a crash.
    //
    // So before doing anything else, check: is there a `.env` file in
    // the current directory? If not AND the user didn't pass a subcommand
    // or --debug, hand them a friendly nudge instead of crashing.
    let env_exists = std::env::current_dir()
        .map(|p| p.join(".env").exists())
        .unwrap_or(false);
    if !env_exists && args.command.is_none() && !args.list_pages {
        print_first_run_banner();
        std::process::exit(0);
    }

    // Load configuration
    let mut app_config = config::AppConfig::load().unwrap_or_else(|e| {
        // Print an actionable error that points the user at the
        // built-in onboarding command instead of a raw config error.
        eprintln!();
        eprintln!("  ✗ Failed to load configuration: {}", e);
        eprintln!();
        eprintln!("  Most likely cause: no `.env` file in the current directory.");
        eprintln!();
        eprintln!("  Quickest fix:    hydragent onboard");
        eprintln!("  Or manually:     cp .env.example .env   (then edit the keys)");
        eprintln!("  Diagnose:        hydragent doctor");
        eprintln!();
        std::process::exit(1);
    });

    // ── Early-return handlers for short-lived subcommands ─────────────
    //
    // These don't touch the gateway, bus, dream worker, or tool
    // registry — they only inspect process state. Running them here
    // (after config load but before logger init) means even a broken
    // gateway install can still answer `hydragent ps` / `status`.
    //
    // `Serve` deliberately does NOT early-return: it falls through
    // to the default gateway startup path below, so `hydragent serve`
    // and `hydragent` (no subcommand) produce identical log output.
    match &args.command {
        Some(Commands::Ps) => {
            cmd_ps();
            std::process::exit(0);
        }
        Some(Commands::Stop { pid }) => {
            cmd_stop(*pid);
            std::process::exit(0);
        }
        Some(Commands::Status) => {
            cmd_status(&app_config);
            std::process::exit(0);
        }
        // Handle `status` BEFORE the config-load failure path above is
        // an issue — but the only way we reach this match is if
        // config loaded, so that's fine.
        Some(Commands::Serve) | None => {
            // Fall through to gateway startup
        }
        // All other commands continue to the heavy init below
        _ => {}
    }

    // ── --debug mode: print a structured env/config diagnostic dump ─────
    //
    // This runs *before* the vault check and *before* the logger init so
    // the output appears immediately and the operator can see exactly
    // what the binary thinks the environment is. We force
    // `log_level = "debug"` so the subsequent `init_logger` call picks
    // up DEBUG-level traces from every module.
    if args.debug {
        app_config.log_level = "debug".to_string();
        debug_dump_env_and_config(&app_config);
    }

    // Load secrets from encrypted vault if configured and passphrase is provided
    let vault_path = std::path::PathBuf::from(&app_config.data_dir).join("vault/.hydravault");
    if vault_path.exists() {
        if let Ok(passphrase) = std::env::var("HYDRAGENT_VAULT_PASSPHRASE") {
            let vault = hydragent_vault::Vault::new(vault_path);
            match vault.load(&passphrase) {
                Ok(secrets) => {
                    tracing::info!("Loaded secrets from cryptographic Vault.");
                    if let Some(keys) = secrets.get("OPENROUTER_API_KEYS") {
                        app_config.openrouter_api_keys = keys.expose_secret().to_string();
                    }
                    if let Some(model) = secrets.get("PRIMARY_MODEL") {
                        // Legacy key — re-export as BRAIN_MODEL so it seeds
                        // the new effective_brain_model() helper.
                        app_config.brain_model = model.expose_secret().to_string();
                    }
                    if let Some(model) = secrets.get("BRAIN_MODEL") {
                        app_config.brain_model = model.expose_secret().to_string();
                    }
                    // Load any other configuration keys from Vault dynamically
                    for (scope, secret) in secrets {
                        std::env::set_var(scope, secret.expose_secret());
                    }
                }
                Err(e) => {
                    eprintln!("Failed to decrypt cryptographic Vault: incorrect passphrase. Error: {}", e);
                }
            }
        } else {
            eprintln!("Cryptographic Vault exists, but HYDRAGENT_VAULT_PASSPHRASE is not set. Bypassing Vault decryption.");
        }
    }

    // Initialize the logger using configured values.
    //
    // For interactive `chat` we send the file layer to a JSONL file and
    // keep the on-screen terminal at "warn" — that way the user only
    // sees real problems, not 30 lines of "tool registered" chatter.
    // For every other subcommand (server, onboard, doctor, ...) we
    // keep the original behaviour: everything to stderr.
    let log_file_path = match &args.command {
        Some(Commands::Chat) => {
            let dir = std::path::PathBuf::from(&app_config.data_dir).join("logs");
            let _ = std::fs::create_dir_all(&dir);
            Some(dir.join("chat.jsonl"))
        }
        _ => None,
    };
    let screen_level = match &args.command {
        Some(Commands::Chat) => {
            // Chat stays clean. Background worker chatter (memory
            // inserts, dream-cycle parse failures, deprecation
            // warnings, …) hits the file only. Only *real* errors
            // surface on screen so a single broken tool doesn't drown
            // the transcript. Override with HYDRAGENT_CHAT_LOG=warn
            // (or info / debug) to bring a level back to stderr.
            std::env::var("HYDRAGENT_CHAT_LOG").unwrap_or_else(|_| "error".to_string())
        }
        _ => app_config.log_level.clone(),
    };
    logger::init_logger(
        &app_config.log_format,
        &screen_level,
        log_file_path.as_deref(),
    );

    // ── Onboard subcommand (first-time setup wizard) ─────────────────
    //
    // Runs *before* logger init is strictly necessary (it has its own
    // println! output), but we want consistent log-level output during
    // the wizard's optional `test-brain` verification step, so we
    // initialise the logger first.
    if let Some(Commands::Onboard {
        provider, api_key, model, non_interactive, no_verify, force,
    }) = &args.command
    {
        let code = onboard::run(onboard::OnboardOptions {
            provider: provider.clone(),
            api_key: api_key.clone(),
            model: model.clone(),
            non_interactive: *non_interactive,
            no_verify: *no_verify,
            force: *force,
        });
        std::process::exit(code);
    }

    // ── Doctor subcommand (diagnostic report) ────────────────────────
    //
    // Also runs *before* the bus-server setup so it stays fast and
    // doesn't need any of the heavy crates to be live.
    if matches!(&args.command, Some(Commands::Doctor)) {
        let report = doctor::run(&app_config);
        report.print();
        if report.has_failures() {
            std::process::exit(1);
        }
        return;
    }

    // ── Examples subcommand (catalogue of starter prompts) ──────────
    if let Some(Commands::Examples { filter }) = &args.command {
        examples::print(filter.as_deref());
        return;
    }

    // ── Dream subcommand (manage the dream-cycle background worker) ──
    //
    // Runs before the heavy session-store / bus setup because it only
    // reads and optionally patches the `.env` file. The change is
    // persisted to disk so it survives the next restart without any
    // further action from the user.
    if let Some(Commands::Dream { enable, disable, interval }) = &args.command {
        let env_path = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".env");

        // Determine if we are making any changes
        let making_changes = *enable || *disable || interval.is_some();

        if making_changes {
            // Read the existing .env, patch the relevant keys, and write back.
            // We do a line-by-line rewrite so every other setting is preserved.
            let content = std::fs::read_to_string(&env_path).unwrap_or_default();
            let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

            // Flags for whether we found (and replaced) each key in-place
            let mut found_enable = false;
            let mut found_interval = false;

            for line in lines.iter_mut() {
                // Clone to an owned String so the borrow of `*line` ends
                // before we potentially assign to `*line` below.
                let trimmed = line.trim_start().to_owned();

                // Handle ENABLE_DREAMING
                if (*enable || *disable)
                    && (trimmed.starts_with("ENABLE_DREAMING=")
                        || trimmed.starts_with("# ENABLE_DREAMING="))
                {
                    *line = format!("ENABLE_DREAMING={}", if *enable { "true" } else { "false" });
                    found_enable = true;
                }

                // Handle DREAMING_INTERVAL_SEC
                if let Some(secs) = interval {
                    if trimmed.starts_with("DREAMING_INTERVAL_SEC=")
                        || trimmed.starts_with("# DREAMING_INTERVAL_SEC=")
                    {
                        *line = format!("DREAMING_INTERVAL_SEC={}", secs);
                        found_interval = true;
                    }
                }
            }


            // If the keys weren't in .env, append them
            if (*enable || *disable) && !found_enable {
                lines.push(format!(
                    "ENABLE_DREAMING={}",
                    if *enable { "true" } else { "false" }
                ));
            }
            if let Some(secs) = interval {
                if !found_interval {
                    lines.push(format!("DREAMING_INTERVAL_SEC={}", secs));
                }
            }

            let new_content = lines.join("\n") + "\n";
            match std::fs::write(&env_path, &new_content) {
                Ok(_) => {
                    println!("------------------------------------------------------------------------");
                    println!("  🌙 Hydragent Dream Cycle — settings updated");
                    println!("  .env: {}", env_path.display());
                    println!("------------------------------------------------------------------------");
                    if *enable {
                        println!("  ✓ dreaming enabled    (ENABLE_DREAMING=true)");
                    }
                    if *disable {
                        println!("  ✓ dreaming disabled   (ENABLE_DREAMING=false)");
                    }
                    if let Some(secs) = interval {
                        println!("  ✓ interval set to {}s  (DREAMING_INTERVAL_SEC={})", secs, secs);
                    }
                    println!();
                    println!("  Restart the bus server for changes to take effect.");
                    println!("------------------------------------------------------------------------");
                }
                Err(e) => {
                    eprintln!("  ✗ Failed to write .env: {}", e);
                    std::process::exit(1);
                }
            }
        } else {
            // No flags — just print current status from the loaded config
            println!("------------------------------------------------------------------------");
            println!("  🌙 Hydragent Dream Cycle — current status");
            println!("------------------------------------------------------------------------");
            println!(
                "  enabled       : {}",
                if app_config.enable_dreaming { "yes ✓" } else { "no ✗" }
            );
            println!(
                "  interval      : {}s  (every {:.1} minutes)",
                app_config.dreaming_interval_sec,
                app_config.dreaming_interval_sec as f64 / 60.0
            );
            println!("  config source : {} (or ENABLE_DREAMING / DREAMING_INTERVAL_SEC env vars)", env_path.display());
            println!("------------------------------------------------------------------------");
            println!("  To change settings:");
            println!("    hydragent dream --enable           turn dreaming on");
            println!("    hydragent dream --disable          turn dreaming off");
            println!("    hydragent dream --interval <SECS>  set the cycle interval");
            println!("------------------------------------------------------------------------");
        }
        return;
    }

    if let Some(Commands::Embed { action }) = &args.command {
        match action {
            EmbedAction::Compare { text1, text2 } => {
                let paths = hydragent_embed::ensure_model_downloaded(&app_config.data_dir)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("Failed to download/verify embedding model: {}", e);
                        std::process::exit(1);
                    });
                let embedder = hydragent_embed::LocalEmbedder::new(&paths.model_path, &paths.tokenizer_path)
                    .unwrap_or_else(|e| {
                        eprintln!("Failed to initialize local embedder: {}", e);
                        std::process::exit(1);
                    });
                let v1 = embedder.embed_text(text1).unwrap_or_else(|e| {
                    eprintln!("Failed to embed text1: {}", e);
                    std::process::exit(1);
                });
                let v2 = embedder.embed_text(text2).unwrap_or_else(|e| {
                    eprintln!("Failed to embed text2: {}", e);
                    std::process::exit(1);
                });
                let sim = hydragent_embed::cosine_similarity(&v1, &v2);
                println!("Cosine similarity: {:.4}", sim);
            }
        }
        return;
    }

    // ── Phase 6 audit chain subcommand ────────────────────────────────
    //
    // Loads (or creates) the agent's Ed25519 keypair, opens the
    // Merkle-audit chain at `{data_dir}/audit/chain.db`, and dispatches
    // to the requested action. Read-only actions (`list`, `head`,
    // `verify`) do not require the agent to be running.
    if let Some(Commands::Audit { action }) = &args.command {
        let data_dir = std::path::PathBuf::from(&app_config.data_dir);
        let audit_dir = data_dir.join("audit");
        let keys_dir = data_dir.join("keys");
        std::fs::create_dir_all(&audit_dir).unwrap_or_else(|e| {
            eprintln!("Failed to create audit directory: {}", e);
            std::process::exit(1);
        });
        std::fs::create_dir_all(&keys_dir).unwrap_or_else(|e| {
            eprintln!("Failed to create keys directory: {}", e);
            std::process::exit(1);
        });
        let chain_path = audit_dir.join("chain.db");
        let key_path = keys_dir.join("agent_ed25519.key");
        let pub_path = std::path::PathBuf::from("config/keys/agent_ed25519.pub");

        let signer = load_or_create_agent_signer(&key_path, &pub_path)
            .unwrap_or_else(|e| {
                eprintln!("Failed to load agent Ed25519 key: {}", e);
                std::process::exit(1);
            });

        let chain = hydragent_security::MerkleAuditChain::connect(
            chain_path.to_str().unwrap(),
            std::sync::Arc::new(signer),
        )
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to open audit chain: {}", e);
            std::process::exit(1);
        });

        match action {
            AuditAction::List { limit, offset, reverse } => {
                let rows = chain.list(*limit, *offset, *reverse).await.unwrap_or_else(|e| {
                    eprintln!("Failed to list audit events: {}", e);
                    std::process::exit(1);
                });
                println!("------------------------------------------------------------------------");
                println!("  🐉 Hydragent Audit Chain — {} event(s)", chain.count().await.unwrap_or(0));
                println!("  Chain: {}", chain_path.display());
                println!("------------------------------------------------------------------------");
                if rows.is_empty() {
                    println!("  (no events match the filter)");
                } else {
                    println!(
                        "  {:>4}  {:<22}  {:<14}  {:<28}  {}",
                        "seq", "timestamp", "type", "actor / page", "chain_hash (head 8)"
                    );
                    println!("  --------------------------------------------------------------------------");
                    for r in &rows {
                        let ts = chrono::DateTime::from_timestamp_millis(r.timestamp_ms)
                            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                            .unwrap_or_else(|| format!("ms:{}", r.timestamp_ms));
                        let page = r.page_id.as_deref().unwrap_or("—");
                        let head8: String = r.chain_hash.chars().take(8).collect();
                        println!(
                            "  {:>4}  {:<22}  {:<14}  {:<28}  {}",
                            r.seq_id, ts, r.event_type, format!("{}/{}", r.actor, page), head8
                        );
                    }
                }
                println!("------------------------------------------------------------------------");
            }
            AuditAction::Head => {
                let head = chain.head_hash().await.unwrap_or_else(|e| {
                    eprintln!("Failed to read head hash: {}", e);
                    std::process::exit(1);
                });
                match head {
                    Some(h) => println!("{}", h),
                    None => println!("(empty chain)"),
                }
            }
            AuditAction::Verify { signatures, public_key } => {
                // Resolve the verifying key to use.
                let external = if *signatures {
                    match public_key {
                        Some(p) => {
                            let vk = hydragent_security::AgentSigner::load_verifying_key(p)
                                .unwrap_or_else(|e| {
                                    eprintln!(
                                        "Failed to load public key from {}: {}",
                                        p.display(),
                                        e
                                    );
                                    std::process::exit(1);
                                });
                            Some(vk)
                        }
                        None => None, // signer.verifying_key() will be used
                    }
                } else {
                    None
                };

                let v = chain
                    .verify_with_key(*signatures, external.as_ref())
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("Verification failed: {}", e);
                        std::process::exit(1);
                    });
                match v {
                    hydragent_security::VerificationResult::Valid { event_count } => {
                        println!("------------------------------------------------------------------------");
                        println!("  ✅ Audit chain is INTACT");
                        println!("  {} events verified.", event_count);
                        if *signatures {
                            println!("  Ed25519 signatures: VERIFIED");
                            if let Some(p) = public_key {
                                println!("  Public key:         {}", p.display());
                            } else {
                                println!("  Public key:         config/keys/agent_ed25519.pub");
                            }
                        } else {
                            println!("  (Ed25519 signature check skipped — pass --signatures to enable)");
                        }
                        println!("------------------------------------------------------------------------");
                    }
                    hydragent_security::VerificationResult::Tampered { seq_id, detail } => {
                        eprintln!("------------------------------------------------------------------------");
                        eprintln!("  ❌ Audit chain is TAMPERED");
                        eprintln!("  First break at seq_id={}:", seq_id);
                        eprintln!("  {}", detail);
                        eprintln!("------------------------------------------------------------------------");
                        std::process::exit(2);
                    }
                }
            }
        }
        return;
    }

    fn get_passphrase() -> anyhow::Result<String> {
        if let Ok(val) = std::env::var("HYDRAGENT_VAULT_PASSPHRASE") {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }

        let password = rpassword::prompt_password("Enter vault passphrase: ")?;
        let trimmed = password.trim().to_string();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!("Passphrase cannot be empty"));
        }
        Ok(trimmed)
    }

    if let Some(Commands::Vault { action }) = &args.command {
        let vault_path = std::path::PathBuf::from(&app_config.data_dir).join("vault/.hydravault");
        let vault = hydragent_vault::Vault::new(vault_path);

        match action {
            VaultAction::Init => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                vault.init(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to initialize vault: {}", e);
                    std::process::exit(1);
                });
                println!("Vault initialized successfully.");
            }
            VaultAction::Set { scope, value } => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let mut secrets = if vault.exists() {
                    vault.load(&passphrase).unwrap_or_else(|e| {
                        eprintln!("Failed to load vault: {}", e);
                        std::process::exit(1);
                    })
                } else {
                    eprintln!("Vault does not exist. Please initialize it first using `vault init`.");
                    std::process::exit(1);
                };
                secrets.insert(scope.clone(), hydragent_vault::TaintedString::new(value.clone()));
                vault.save(&passphrase, &secrets).unwrap_or_else(|e| {
                    eprintln!("Failed to save secret to vault: {}", e);
                    std::process::exit(1);
                });
                println!("Secret set successfully for scope: {}", scope);
            }
            VaultAction::Get { scope } => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let secrets = vault.load(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to load vault: {}", e);
                    std::process::exit(1);
                });
                if let Some(secret) = secrets.get(scope) {
                    println!("{}", secret.expose_secret());
                } else {
                    eprintln!("Secret not found for scope: {}", scope);
                    std::process::exit(1);
                }
            }
            VaultAction::List => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let secrets = vault.load(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to load vault: {}", e);
                    std::process::exit(1);
                });
                println!("Stored secret scopes:");
                for scope in secrets.keys() {
                    println!("  - {}", scope);
                }
            }
            VaultAction::Delete { scope } => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let mut secrets = vault.load(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to load vault: {}", e);
                    std::process::exit(1);
                });
                if secrets.remove(scope).is_some() {
                    vault.save(&passphrase, &secrets).unwrap_or_else(|e| {
                        eprintln!("Failed to save vault: {}", e);
                        std::process::exit(1);
                    });
                    println!("Secret deleted successfully for scope: {}", scope);
                } else {
                    eprintln!("Secret not found for scope: {}", scope);
                    std::process::exit(1);
                }
            }
        }
        return;
    }

    // ── Phase 6 `security` subcommand ──────────────────────────────────
    //
    // Direct (no-LLM) surface for the user to inspect and exercise every
    // Phase 6 feature from the terminal. Mirrors the LLM-callable tools
    // in `hydragent_tools::phase6` so the user can verify behaviour
    // before relying on the chat adapter to wire it up.
    if let Some(Commands::Security { action }) = &args.command {
        let data_dir = std::path::PathBuf::from(&app_config.data_dir);
        let audit_dir = data_dir.join("audit");
        let keys_dir = data_dir.join("keys");
        let vault_file = data_dir.join("vault/.hydravault");
        let policy_path = std::path::PathBuf::from("config/security/taint_sinks.yaml");
        let patterns_path = std::path::PathBuf::from("config/security/injection_patterns.yaml");

        println!("------------------------------------------------------------------------");
        println!("  🛡  Hydragent Phase 6 Security Surface");
        println!("  data_dir      : {}", data_dir.display());
        println!("  audit_dir     : {}", audit_dir.display());
        println!("  vault_path    : {}", vault_file.display());
        println!("------------------------------------------------------------------------");
        match action {
            SecurityAction::Status => {
                // mlock (Track 6.4)
                println!(
                    "  Track 6.4 mlock_available  : {}",
                    if hydragent_vault::is_mlock_available() { "yes" } else { "no" }
                );
                println!("  Track 6.4 vault_exists     : {}", vault_file.exists());

                // audit (Track 6.1)
                let chain_path = audit_dir.join("chain.db");
                println!("  Track 6.1 chain_path       : {}", chain_path.display());
                println!("  Track 6.1 chain_exists     : {}", chain_path.exists());

                if chain_path.exists() {
                    let signer = match load_or_create_agent_signer(
                        &keys_dir.join("agent_ed25519.key"),
                        &std::path::PathBuf::from("config/keys/agent_ed25519.pub"),
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Failed to load agent signer: {}", e);
                            std::process::exit(1);
                        }
                    };
                    let chain = match hydragent_security::MerkleAuditChain::connect(
                        chain_path.to_str().unwrap(),
                        std::sync::Arc::new(signer),
                    )
                    .await
                    {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("Failed to open audit chain: {}", e);
                            std::process::exit(1);
                        }
                    };
                    let head = chain.head_hash().await.ok().flatten();
                    let count = chain.count().await.unwrap_or(0);
                    let head_short = head
                        .as_deref()
                        .map(|h| if h.len() >= 16 { &h[..16] } else { h });
                    println!("  Track 6.1 event_count      : {}", count);
                    println!(
                        "  Track 6.1 head_hash        : {}",
                        head_short.unwrap_or("(empty)")
                    );
                }

                // taint (Track 6.2)
                let (policy, source) = if policy_path.exists() {
                    match std::fs::read_to_string(&policy_path)
                        .ok()
                        .and_then(|y| hydragent_security::SinkPolicy::load_yaml(&y).ok())
                    {
                        Some(p) => (p, format!("file:{}", policy_path.display())),
                        None => (hydragent_security::SinkPolicy::default_policy(), "default (parse failed)".into()),
                    }
                } else {
                    (hydragent_security::SinkPolicy::default_policy(), "default (built-in)".into())
                };
                println!("  Track 6.2 policy_source    : {}", source);
                println!("  Track 6.2 policy_version   : {}", policy.version);
                println!("  Track 6.2 sinks_configured : {}", policy.sinks.len());

                // sanitizer (Track 6.3)
                let sanitizer = if patterns_path.exists() {
                    hydragent_security::InputSanitizer::from_yaml_file(&patterns_path).ok()
                } else {
                    None
                };
                let pattern_count = sanitizer
                    .as_ref()
                    .map(|s| s.pattern_count())
                    .unwrap_or(0);
                let pattern_source = if patterns_path.exists() {
                    format!("file:{}", patterns_path.display())
                } else {
                    "default (built-in)".to_string()
                };
                println!("  Track 6.3 patterns_source  : {}", pattern_source);
                println!("  Track 6.3 patterns_loaded  : {}", pattern_count);
                println!("------------------------------------------------------------------------");
            }
            SecurityAction::TaintCheck { sink, category } => {
                // Parse sink
                use hydragent_security::TaintSink as S;
                let parsed_sink = match sink.as_str() {
                    "llm_request_messages" | "llm" => S::LlmRequestMessages,
                    "outbound_network" | "network" => S::OutboundNetwork,
                    "log" => S::Log,
                    "vault_write" | "vault" => S::VaultWrite,
                    "disk_write" | "disk" => S::DiskWrite,
                    other => {
                        eprintln!("Unknown sink: {other}");
                        std::process::exit(1);
                    }
                };
                // Parse categories
                let mut taint = hydragent_types::TaintSet::new();
                for c in category {
                    use hydragent_types::TaintCategory as C;
                    let cat = match c.as_str() {
                        "secret" => C::Secret,
                        "pii" => C::PII,
                        "tool_output" => C::ToolOutput,
                        "user_input" => C::UserInput,
                        "llm_output" => C::LlmOutput,
                        "system_internal" => C::SystemInternal,
                        other => {
                            eprintln!("Unknown taint category: {other}");
                            std::process::exit(1);
                        }
                    };
                    taint.0.insert(cat);
                }
                // Load policy
                let policy = if policy_path.exists() {
                    match std::fs::read_to_string(&policy_path)
                        .ok()
                        .and_then(|y| hydragent_security::SinkPolicy::load_yaml(&y).ok())
                    {
                        Some(p) => p,
                        None => hydragent_security::SinkPolicy::default_policy(),
                    }
                } else {
                    hydragent_security::SinkPolicy::default_policy()
                };
                let allow = policy.enforce(parsed_sink, &taint).is_ok();
                let redact = policy.requires_redact(parsed_sink, &taint);
                let rule = policy.rule(parsed_sink).cloned();
                let verdict = if !allow { "BLOCK" } else if redact { "REDACT" } else { "ALLOW" };
                println!("  sink        : {}", sink);
                println!("  categories  : {}", if category.is_empty() { "(none)".to_string() } else { category.join(", ") });
                println!("  verdict     : {}", verdict);
                println!("  policy_ver  : {}", policy.version);
                if let Some(r) = rule {
                    println!("  rule.forbid : {:?}", r.forbid);
                    println!("  rule.redact : {:?}", r.redact);
                } else {
                    println!("  rule        : <not configured for this sink>");
                }
                println!("------------------------------------------------------------------------");
            }
            SecurityAction::TaintPolicy => {
                let (policy, source) = if policy_path.exists() {
                    match std::fs::read_to_string(&policy_path)
                        .ok()
                        .and_then(|y| hydragent_security::SinkPolicy::load_yaml(&y).ok())
                    {
                        Some(p) => (p, format!("file:{}", policy_path.display())),
                        None => (hydragent_security::SinkPolicy::default_policy(), "default (parse failed)".into()),
                    }
                } else {
                    (hydragent_security::SinkPolicy::default_policy(), "default (built-in)".into())
                };
                println!("  source : {}", source);
                println!("  version: {}", policy.version);
                for (sink, rule) in &policy.sinks {
                    println!("  ------------------------------------------------------------------");
                    println!("  sink   : {:?}", sink);
                    println!("  forbid : {:?}", rule.forbid);
                    println!("  redact : {:?}", rule.redact);
                }
                println!("------------------------------------------------------------------------");
            }
            SecurityAction::SanitizerScan { text } => {
                let sanitizer = if patterns_path.exists() {
                    match hydragent_security::InputSanitizer::from_yaml_file(&patterns_path) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Failed to load patterns: {}", e);
                            std::process::exit(1);
                        }
                    }
                } else {
                    eprintln!("(no patterns file at {}, using built-in library)", patterns_path.display());
                    match hydragent_security::InputSanitizer::from_yaml(
                        BUILTIN_INJECTION_PATTERNS,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Built-in patterns failed to compile: {e}");
                            std::process::exit(1);
                        }
                    }
                };
                println!("  text    : {}", text);
                match sanitizer.scan(text) {
                    hydragent_security::SanitizationResult::Allowed => {
                        println!("  verdict : ALLOWED");
                    }
                    hydragent_security::SanitizationResult::Blocked { pattern_id, severity, description } => {
                        println!("  verdict : BLOCKED");
                        println!("  pattern : {pattern_id} ({:?})", severity);
                        println!("  detail  : {description}");
                    }
                }
                println!("------------------------------------------------------------------------");
            }
            SecurityAction::SanitizerPatterns => {
                let sanitizer = if patterns_path.exists() {
                    match hydragent_security::InputSanitizer::from_yaml_file(&patterns_path) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Failed to load patterns: {}", e);
                            std::process::exit(1);
                        }
                    }
                } else {
                    match hydragent_security::InputSanitizer::from_yaml(
                        BUILTIN_INJECTION_PATTERNS,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Built-in patterns failed to compile: {e}");
                            std::process::exit(1);
                        }
                    }
                };
                for p in sanitizer.patterns() {
                    println!("  {:<8}  {:<10}  {}", p.id, format!("{:?}", p.severity).to_lowercase(), p.description);
                }
                println!("------------------------------------------------------------------------");
            }
            SecurityAction::VaultStatus => {
                println!("  vault_path        : {}", vault_file.display());
                println!("  vault_exists      : {}", vault_file.exists());
                println!("  mlock_available   : {}", hydragent_vault::is_mlock_available());
                if vault_file.exists() {
                    let passphrase = get_passphrase().unwrap_or_else(|e| {
                        eprintln!("Error getting passphrase: {}", e);
                        std::process::exit(1);
                    });
                    let entries = hydragent_vault::Vault::new(vault_file.clone())
                        .load(&passphrase)
                        .unwrap_or_else(|e| {
                            eprintln!("Failed to load vault: {}", e);
                            std::process::exit(1);
                        });
                    let has_column_key = entries.contains_key(hydragent_vault::rotator::COLUMN_KEY_ENTRY);
                    println!("  entries           : {}", entries.len());
                    println!("  has_column_key    : {}", has_column_key);
                }
                println!("------------------------------------------------------------------------");
            }
            SecurityAction::VaultInit => {
                if vault_file.exists() {
                    println!("  vault_path        : {}", vault_file.display());
                    println!("  vault_exists      : true (already initialized)");
                    println!("  hint              : nothing to do. To rotate the passphrase:");
                    println!("                       security vault-rotate-passphrase --new-passphrase <NEW>");
                    println!("------------------------------------------------------------------------");
                    return;
                }
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let v = hydragent_vault::Vault::new(vault_file.clone());
                match v.init(&passphrase) {
                    Ok(()) => {
                        println!("  ok                : true");
                        println!("  vault_path        : {}", vault_file.display());
                        println!("  hint              : set HYDRAGENT_VAULT_PASSPHRASE to skip the prompt");
                        println!("  next_step         : `security status` to see all Phase 6 subsystems");
                        println!("------------------------------------------------------------------------");
                    }
                    Err(e) => {
                        eprintln!("Failed to initialize vault: {}", e);
                        std::process::exit(1);
                    }
                }
                return;
            }
            SecurityAction::VaultRotatePassphrase { new_passphrase } => {
                if !vault_file.exists() {
                    eprintln!("Vault not found at {}. Run `vault init` first.", vault_file.display());
                    std::process::exit(1);
                }
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let rotator = hydragent_vault::Rotator::new(vault_file.clone());
                match rotator.rotate_passphrase(&passphrase, new_passphrase) {
                    Ok(report) => {
                        println!("  ok                  : true");
                        println!("  entries_after       : {}", report.entries_after);
                        println!("  column_key_rotated  : {}", report.column_key_rotated);
                        println!("------------------------------------------------------------------------");
                    }
                    Err(e) => {
                        eprintln!("Rotation failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            SecurityAction::VaultRotateColumnKey => {
                if !vault_file.exists() {
                    eprintln!("Vault not found at {}. Run `vault init` first.", vault_file.display());
                    std::process::exit(1);
                }
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let rotator = hydragent_vault::Rotator::new(vault_file.clone());
                match rotator.rotate_column_key(&passphrase) {
                    Ok((report, key)) => {
                        let key_hex: String = key.as_slice()[0]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect();
                        let preview = if key_hex.len() >= 16 { &key_hex[..16] } else { &key_hex };
                        println!("  ok                       : true");
                        println!("  entries_after            : {}", report.entries_after);
                        println!("  new_column_key_hex       : {}...", preview);
                        println!("  (full 64-char hex logged at INFO level; treat as secret)");
                        println!("------------------------------------------------------------------------");
                    }
                    Err(e) => {
                        eprintln!("Rotation failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
        return;
    }

    let startup_duration = start_time.elapsed();
    info!("🐉 Hydragent startup latency: {:?}", startup_duration);

    // Build SQLite DB filepath
    let db_path = format!("{}/sessions.db", app_config.data_dir);
    let mut store = SessionStore::new(&db_path).await.unwrap_or_else(|e| {
        error!("Failed to initialize session database: {}", e);
        std::process::exit(1);
    });

    // Apply the LRU eviction cap from config (no-op when usize::MAX).
    store.with_max_memories(app_config.max_semantic_memories);
    if app_config.max_semantic_memories < usize::MAX {
        info!(
            cap = app_config.max_semantic_memories,
            "🧹 Memory LRU eviction enabled"
        );
    }
    let store = Arc::new(store);

    // ── The "brain" (single live provider) ────────────────────────────
    //
    // The agent has one brain, swappable via 4 env vars:
    //   BRAIN_BASE     = https://api.together.xyz/v1   (or openai, openrouter, ollama, ...)
    //   BRAIN_KEY      = sk-...                       (empty for local providers)
    //   BRAIN_MODEL    = meta-llama/Llama-3-70b-chat-hf
    //   BRAIN_FALLBACKS= smaller-model1,smaller-model2
    //
    // Backward compat: if BRAIN_BASE is unset but OPENROUTER_API_KEYS is set,
    // we use OpenRouter's URL. If BRAIN_MODEL is unset we use PRIMARY_MODEL,
    // and BRAIN_FALLBACKS falls back to FALLBACK_MODELS.
    let brain_base = app_config.effective_brain_base();
    let brain_key = app_config.effective_brain_key();
    let brain_model = app_config.effective_brain_model();
    let brain_fallbacks = app_config.effective_brain_fallbacks();

    if brain_base.is_empty() {
        eprintln!(
            "🤔 I don't know where to connect. Set `BRAIN_BASE` in `.env`.\n\
             Examples:\n\
               BRAIN_BASE=https://api.openai.com/v1\n\
               BRAIN_BASE=https://openrouter.ai/api/v1\n\
               BRAIN_BASE=http://localhost:11434/v1  (Ollama in OpenAI-compat mode)\n\
             Or set `OPENROUTER_API_KEYS` for backward compatibility."
        );
        std::process::exit(1);
    }

    if !brain_key.is_empty() && brain_key.contains("9b9c8f09436e") {
        tracing::warn!(
            "⚠️ Default placeholder API key detected. Replace it with a valid key in `.env`."
        );
    }

    info!(
        base = brain_base.as_str(),
        primary = brain_model.as_str(),
        fallbacks = ?brain_fallbacks,
        "🧠 Building live brain"
    );

    let brain_config = hydragent_model::custom_openai::CustomProviderConfig {
        base_url: brain_base.clone(),
        api_key: brain_key,
        default_model: brain_model.clone(),
        provider_label: "brain".to_string(),
        // 180s gives slow LLM providers (tokenrouter rate-limits, long
        // ReAct loops with multiple tool calls) enough headroom to
        // complete without aborting. The test harness uses
        // `TIMEOUT_LLM=90.0` / `TIMEOUT_LLM_LONG=180.0`; matching the
        // upper bound here keeps the rust side from being the bottleneck.
        timeout: std::time::Duration::from_secs(180),
        max_retries: 3,
    };
    let brain_client: Arc<dyn hydragent_model::ModelProvider> =
        Arc::new(hydragent_model::custom_openai::CustomOpenAIClient::new(brain_config));

    let model_router = Arc::new(hydragent_model::router::ModelRouter::new(
        brain_client,
        brain_model,
        brain_fallbacks,
    ));

    // ── `test-brain` subcommand ─────────────────────────────────────────
    //
    // Real-time practical test: stream a prompt through the live brain and
    // print the response. Exercises the full BRAIN_* → ModelRouter →
    // CustomOpenAIClient → SSE pipeline. This is the quickest way to
    // confirm a freshly pasted key actually works end-to-end.
    if let Some(Commands::TestBrain { prompt }) = &args.command {
        println!("------------------------------------------------------------------------");
        println!("  🧠 Hydragent live-brain test");
        println!("  base     : {}", brain_base);
        println!("  primary  : {}", app_config.effective_brain_model());
        println!("  fallbacks: {:?}", app_config.effective_brain_fallbacks());
        println!("  prompt   : {}", prompt);
        println!("------------------------------------------------------------------------");
        println!();

        use hydragent_model::openrouter::ChatMessage;
        use tokio::sync::mpsc;
        let (tx, mut rx) = mpsc::channel::<String>(256);
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: prompt.clone(),
        }];

        // Collect tokens and stream them to the terminal. In
        // the default (render) mode each token is fed through
        // a [`MarkdownStreamer`] which renders complete lines
        // (or whole code blocks) as soon as they arrive — the
        // user sees a "typewriter" effect. Set
        // `HYDRAGENT_STREAM_RAW=1` to bypass the renderer and
        // write raw token bytes straight to stdout (useful for
        // diffing or piping into another tool).
        let stream_raw = std::env::var("HYDRAGENT_STREAM_RAW")
            .ok()
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let collector = tokio::spawn(async move {
            let mut collected = String::new();
            use tokio::io::AsyncWriteExt;
            let mut stdout = tokio::io::stdout();

            // Header: print the prompt prefix *before* the first
            // token lands so the user knows the stream has
            // started. This is the only thing the test prints
            // synchronously; everything else is incremental.
            // ▸ is U+25B8 (3 bytes in UTF-8: E2 96 B8). We use
            // a `&str` and `.as_bytes()` because byte-string
            // literals (`b"…"`) can't contain `\u{}` escapes.
            let _ = stdout.write_all("hydra \u{25B8} ".as_bytes()).await;
            let _ = stdout.flush().await;

            if stream_raw {
                // Raw mode: write token bytes through unchanged.
                // No buffering, no styling.
                while let Some(token) = rx.recv().await {
                    collected.push_str(&token);
                    let _ = stdout.write_all(token.as_bytes()).await;
                    let _ = stdout.flush().await;
                }
            } else {
                // Render mode: feed each token into the streamer
                // and write the rendered text it returns. The
                // streamer buffers partial lines (so a single
                // line's heading styling doesn't flash in piece
                // by piece) and atomically renders fenced code
                // blocks.
                let renderer = markdown_render::MarkdownRenderer::new();
                let mut streamer = markdown_render::MarkdownStreamer::new(&renderer);
                while let Some(token) = rx.recv().await {
                    collected.push_str(&token);
                    let rendered = streamer.push(&token);
                    if !rendered.is_empty() {
                        let _ = stdout.write_all(rendered.as_bytes()).await;
                        let _ = stdout.flush().await;
                    }
                }
                // End of stream: flush any text the streamer is
                // still holding (the last line, if it didn't end
                // in a newline, or a code block whose closing
                // fence never arrived).
                let tail = streamer.finish();
                if !tail.is_empty() {
                    let _ = stdout.write_all(tail.as_bytes()).await;
                    let _ = stdout.flush().await;
                }
            }
            collected
        });

        let started = std::time::Instant::now();
        match model_router.chat_stream(messages, tx, None).await {
            Ok((content, used_model)) => {
                // The collector streamed the response
                // (rendered line-by-line, or raw) while the
                // router was producing tokens; we just need
                // its handle here so the task is fully joined
                // before we print the summary. The actual
                // rendered text is already on the terminal.
                let _ = collector.await;
                let elapsed = started.elapsed();
                println!();
                println!("------------------------------------------------------------------------");
                // The collector has already streamed the response
                // (rendered line-by-line, or raw). We only need
                // the summary line now.
                println!(
                    "  ✅ Brain spoke (model={}, {:.2}s, {} chars)",
                    used_model,
                    elapsed.as_secs_f64(),
                    content.chars().count()
                );
                println!("------------------------------------------------------------------------");
            }
            Err(e) => {
                eprintln!();
                eprintln!();
                eprintln!("------------------------------------------------------------------------");
                eprintln!("  ❌ Brain failed: {}", e);
                eprintln!("------------------------------------------------------------------------");
                std::process::exit(1);
            }
        }
        return;
    }

    if args.list_pages {
        println!("------------------------------------------------------------------------");
        println!("  🐉 Hydragent Page History");
        println!("  Database: {}", db_path);
        println!("------------------------------------------------------------------------");
        match store.list_pages().await {
            Ok(pages) => {
                if pages.is_empty() {
                    println!("  No active Pages found.");
                } else {
                    println!("  {:<36} | {:<20} | {:<5}", "Page ID", "Last Active", "Turns");
                    println!("  ----------------------------------------------------------------------");
                    for (page_id, _, last_active, turn_count) in pages {
                        let dt = chrono::DateTime::from_timestamp(last_active / 1000, 0)
                            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                            .unwrap_or_else(|| "Unknown".to_string());
                        println!("  {:<36} | {:<20} | {:<5}", page_id, dt, turn_count);
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to read database page list: {}", e);
            }
        }
        println!("------------------------------------------------------------------------");
        return;
    }

    if let Some(cmd) = args.command.clone() {
        let mut dispatch_chat = false;
        match cmd {
            // NOTE: Commands::Chat and Commands::Serve set `dispatch_chat`
            // to true and fall through. The REPL needs the ToolRegistry
            // (built later in main()); `serve` is a documented alias for
            // "no subcommand" — both should continue to the gateway path
            // below. `dispatch_chat` is misnamed: it's actually
            // "should I run the gateway path?".
            Commands::Chat => dispatch_chat = true,
            Commands::Serve => dispatch_chat = true, // fall through to gateway startup (same as no subcommand)
            Commands::Memory { action } => {
                match action {
                    MemoryAction::List => {
                        println!("------------------------------------------------------------------------");
                        println!("  🐉 Hydragent Semantic Memories");
                        println!("  Database: {}", db_path);
                        println!("------------------------------------------------------------------------");
                        match store.list_memories().await {
                            Ok(memories) => {
                                if memories.is_empty() {
                                    println!("  No semantic memories found.");
                                } else {
                                    println!("  {:<36} | {:<5} | {:<50}", "Memory ID", "Imp.", "Content");
                                    println!("  ----------------------------------------------------------------------");
                                    for mem in memories {
                                        println!("  {:<36} | {:<5} | {}", mem.id, mem.importance, mem.content);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to read database memory list: {}", e);
                            }
                        }
                        println!("------------------------------------------------------------------------");
                    }
                    MemoryAction::Clear => {
                        println!("Clearing all stored memories...");
                        match store.clear_all_memories().await {
                            Ok(_) => println!("Success: All semantic memories and search indexes cleared."),
                            Err(e) => eprintln!("Error clearing memories: {}", e),
                        }
                    }
                }
            }
            Commands::Embed { .. }
            | Commands::Vault { .. }
            | Commands::TestBrain { .. }
            | Commands::Audit { .. }
            | Commands::Security { .. }
            | Commands::Onboard { .. }
            | Commands::Doctor
            | Commands::Dream { .. }
            | Commands::Examples { .. }
            | Commands::Ps
            | Commands::Stop { .. }
            | Commands::Status => unreachable!(
                "Onboard/Doctor/Examples/Dream/Ps/Stop/Status are early-returned before this match; Chat/Serve fall through"
            ),
        }
        if !dispatch_chat {
            return;
        }
    }

    info!("Hydragent starting up with config: {:?}", app_config);

    // Spawn dreaming background worker if enabled
    if app_config.enable_dreaming {
        let store_clone = store.clone();
        let model_router_clone = model_router.clone();
        let interval_secs = app_config.dreaming_interval_sec;

        // Open the skill library once for the lifetime of the process.
        // We share an `Arc` so the dream worker can call
        // `induce_skill_from_page_with_library` per-page concurrently
        // without each task paying the cost of opening its own SQLite
        // connection pool. The library file lives at
        // `{data_dir}/skill_library.sqlite`.
        let skill_lib_path = std::path::PathBuf::from(&app_config.data_dir)
            .join("skill_library.sqlite");
        let skill_library = match hydragent_skills::library::SkillLibrary::open(&skill_lib_path).await {
            Ok(lib) => {
                info!(
                    path = %skill_lib_path.display(),
                    "📚 Skill library opened; skill induction enabled"
                );
                Some(Arc::new(lib))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "📚 Could not open skill library; dream-cycle skill induction disabled"
                );
                None
            }
        };
        let skill_library_for_worker = skill_library.clone();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                info!("Dreaming worker waking up...");
                let cycle = dream::run_dream_cycle(
                    store_clone.clone(),
                    model_router_clone.clone(),
                    skill_library_for_worker.clone(),
                ).await;
                match cycle {
                    Ok(stats) => {
                        if stats.messages_processed > 0 {
                            info!(?stats, "Dream cycle completed successfully");
                        }
                    }
                    Err(e) => {
                        error!("Dream cycle error: {}", e);
                    }
                }
            }
        });
    }

    // Initialize Tool Registry
    let workspace_dir = std::env::var("WORKSPACE_DIR")
        .unwrap_or_else(|_| ".".to_string());
    
    let mut registry = ToolRegistry::new();
    let wasm_engine = hydragent_sandbox::create_sandbox_engine().unwrap();
    let sandbox_tools_dir = PathBuf::from("./sandbox/tools");

    let enforce_sandbox = std::env::var("ENFORCE_SANDBOX")
        .map(|v| v.trim().to_lowercase() == "true")
        .unwrap_or(false);

    // Load echo WASM
    let echo_wasm_path = sandbox_tools_dir.join("echo.wasm");
    if echo_wasm_path.exists() {
        let wasm_tool = hydragent_sandbox::WasmTool::load(
            &wasm_engine,
            &echo_wasm_path,
            hydragent_sandbox::ResourceLimits::default(),
            None,
        ).unwrap();
        registry.register(SandboxedTool {
            name: "echo".to_string(),
            description: "Echoes back the input message inside a WASM sandbox.".to_string(),
            schema: r#"{"type":"object","properties":{"call_id":{"type":"string"},"message":{"type":"string"}},"required":["call_id","message"]}"#.to_string(),
            wasm_tool,
            tier: hydragent_types::PermissionTier::AutoApprove,
        });
        info!("Registered sandboxed echo tool.");
    } else {
        if enforce_sandbox {
            panic!("Security Violation: ENFORCE_SANDBOX is enabled but sandboxed tool 'echo.wasm' is missing at {:?}", echo_wasm_path);
        } else {
            tracing::warn!("Warning: Bypassing WASM sandbox for 'echo' tool, registering local native fallback.");
            registry.register(EchoTool);
        }
    }

    registry.register(WebSearchTool::new());
    // Agent-Reach: structured reads from specific sources (Jina, YouTube,
    // Bilibili, RSS, GitHub). See crates/hydragent-tools/src/agent_reach.rs
    // for the channel list and config env vars. Anchored to workspace_dir
    // so the runner script lookup is independent of the process CWD.
    registry.register(AgentReachTool::new(PathBuf::from(&workspace_dir)));

    // Load file_read WASM
    let file_read_wasm_path = sandbox_tools_dir.join("file_read.wasm");
    if file_read_wasm_path.exists() {
        let wasm_tool = hydragent_sandbox::WasmTool::load(
            &wasm_engine,
            &file_read_wasm_path,
            hydragent_sandbox::ResourceLimits::default(),
            Some(PathBuf::from(&workspace_dir)),
        ).unwrap();
        registry.register(SandboxedTool {
            name: "file_read".to_string(),
            description: "Reads a file in the workspace securely inside a WASM sandbox.".to_string(),
            schema: r#"{"type":"object","properties":{"call_id":{"type":"string"},"path":{"type":"string"}},"required":["call_id","path"]}"#.to_string(),
            wasm_tool,
            tier: hydragent_types::PermissionTier::Prompt,
        });
        info!("Registered sandboxed file_read tool.");
    } else {
        if enforce_sandbox {
            panic!("Security Violation: ENFORCE_SANDBOX is enabled but sandboxed tool 'file_read.wasm' is missing at {:?}", file_read_wasm_path);
        } else {
            tracing::warn!("Warning: Bypassing WASM sandbox for 'file_read' tool, registering local native fallback.");
            registry.register(FileReadTool::new(PathBuf::from(&workspace_dir)));
        }
    }
    registry.register(MemoryStoreTool::new(store.clone()));
    registry.register(MemorySearchTool::new(store.clone()));
    registry.register(MemoryForgetTool::new(store.clone()));
    registry.register(hydragent_tools::standing_orders::SoulTool::new(PathBuf::from("./config")));
    registry.register(hydragent_tools::user_profile::UserProfileTool::new(PathBuf::from("./config")));

    // ── Phase 6 LLM-callable tools (Tracks 6.1–6.4) ────────────────────
    //
    // These expose the security surface to the chat adapter so the model
    // can call them via `ToolRegistry::dispatch(...)`. Constructors take
    // filesystem paths, not runtime state, so they can be re-registered
    // cheaply during a hot-reload.
    registry.register(AuditQueryTool::new(PathBuf::from(&app_config.data_dir)));
    registry.register(TaintCheckTool::new(PathBuf::from(
        "config/security/taint_sinks.yaml",
    )));
    registry.register(SanitizerScanTool::new(PathBuf::from(
        "config/security/injection_patterns.yaml",
    )));
    registry.register(SanitizerListPatternsTool::new(PathBuf::from(
        "config/security/injection_patterns.yaml",
    )));
    registry.register(VaultRotateTool::new(PathBuf::from(&app_config.data_dir)));

    // ── Phase 7 / Track 7.1 — Skill library tools ───────────────────
    //
    // Wire the persistent `SkillLibrary` into the chat LLM as three
    // LLM-callable tools. The library is opened lazily on each call so
    // the registry is decoupled from the dream worker's open handle.
    //
    // Each tool's `new()` accepts a `data_dir` and joins
    // `skill_library.sqlite` internally — we pass `app_config.data_dir`
    // directly so the join happens exactly ONCE. (Previously we
    // pre-joined here, which produced the doubled path
    // `data\skill_library.sqlite\skill_library.sqlite` and made
    // `create_dir_all` try to create the SQLite *file* as a
    // *directory*, yielding os error 183.)
    let skill_data_dir = std::path::PathBuf::from(&app_config.data_dir);
    registry.register(hydragent_tools::skill_list::SkillListTool::new(
        skill_data_dir.clone(),
    ));
    registry.register(hydragent_tools::skill_search::SkillSearchTool::new(
        skill_data_dir.clone(),
    ));
    registry.register(hydragent_tools::skill_run::SkillRunTool::new(
        skill_data_dir,
    ));
    // Initialize Gateway Router
    let gateway_router = Arc::new(hydragent_gateway::GatewayRouter::new());

    // Initialize Heartbeat Engine
    let heartbeat = Arc::new(hydragent_scheduler::HeartbeatEngine::new(gateway_router.clone()));

    // Initialize Work IQ Engine
    let work_iq = hydragent_scheduler::work_iq::WorkIqEngine::new(
        store.pool().clone(),
        heartbeat.clone(),
        model_router.clone(),
    );

    // Create scheduler executor with OnceCell to break dependency cycle
    let registry_cell = Arc::new(tokio::sync::OnceCell::<Arc<ToolRegistry>>::new());
    let store_clone = store.clone();
    let model_router_clone = model_router.clone();
    let heartbeat_clone = heartbeat.clone();
    let work_iq_clone = work_iq.clone();
    let max_react_steps = app_config.max_react_steps;
    let registry_cell_clone = registry_cell.clone();

    let executor = Arc::new(move |job: hydragent_types::CronJob| {
        let _store = store_clone.clone();
        let model_router = model_router_clone.clone();
        let heartbeat = heartbeat_clone.clone();
        let registry_cell = registry_cell_clone.clone();
        let work_iq = work_iq_clone.clone();
        let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async move {
            if job.task_type == "react_loop" {
                let mut prompt = job.task_params.clone();
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&job.task_params) {
                    if let Some(obj) = val.as_object() {
                        if let Some(p) = obj.get("prompt").or_else(|| obj.get("query")).or_else(|| obj.get("content")).or_else(|| obj.get("task")) {
                            if let Some(s) = p.as_str() {
                                prompt = s.to_string();
                            }
                        }
                    }
                }
                let page_id = format!("cron-{}", job.id);
                let history_messages = vec![];
                let retrieved_memories = vec![];
                let user_profile = std::fs::read_to_string("./config/USER.md").ok();
                let soul_guidelines = std::fs::read_to_string("./config/SOUL.md").ok();
                
                let (tx, mut rx) = tokio::sync::mpsc::channel(100);
                let active_perms = orchestrator::ActivePermissions::default();
                let page_id_clone = page_id.clone();
                let prompt_clone = prompt.clone();
                
                let registry = registry_cell.get().expect("Registry not set").clone();
                tokio::spawn(async move {
                    let _ = crate::react_loop::run_react_loop(
                        &page_id_clone,
                        "cli",
                        "system",
                        &prompt_clone,
                        history_messages,
                        retrieved_memories,
                        user_profile,
                        soul_guidelines,
                        model_router,
                        registry,
                        max_react_steps,
                        tx,
                        active_perms,
                    ).await;
                });
                
                let mut accumulated_tokens = String::new();
                while let Some(line) = rx.recv().await {
                    if let Ok(msg_val) = serde_json::from_str::<serde_json::Value>(&line) {
                        if msg_val.get("method").and_then(|m| m.as_str()) == Some("response.token") {
                            if let Some(token) = msg_val.get("params").and_then(|p| p.get("token")).and_then(|t| t.as_str()) {
                                accumulated_tokens.push_str(token);
                            }
                        }
                    }
                }
                
                if !accumulated_tokens.is_empty() {
                    let _ = heartbeat.push(job.target_channel_id, page_id, accumulated_tokens).await;
                }
            } else if job.task_type == "heartbeat" {
                // Phase 4 G6: Proactive push — relay a static or pre-summarized
                // string to the target channel without spinning the LLM.
                // Renamed from "message" to "heartbeat" for clarity (the
                // delivery path is the HeartbeatEngine).
                //
                // task_params may be a plain string (treated as content
                // addressed to a `cron-<job_id>` page) OR a JSON object
                // with explicit `page_id` and `content` fields, in which
                // case we honour the LLM's routing intent.
                let default_page = format!("cron-{}", job.id);
                let (target_page, target_content) =
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&job.task_params) {
                        if let Some(obj) = val.as_object() {
                            let page = obj.get("page_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or(default_page.clone());
                            let content = obj.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| job.task_params.clone());
                            (page, content)
                        } else {
                            (default_page, job.task_params.clone())
                        }
                    } else {
                        (default_page, job.task_params.clone())
                    };
                let _ = heartbeat.push(job.target_channel_id, target_page, target_content).await;
            } else if job.task_type == "work_iq_digest" {
                if let Err(e) = work_iq.generate_and_send_digest(&job.task_params, &job.target_channel_id).await {
                    tracing::error!("Work IQ: failed to run digest: {}", e);
                }
            }
        });
        fut
    });

    let cron_scheduler = hydragent_scheduler::CronScheduler::new(store.pool().clone(), executor)
        .await
        .expect("Failed to initialize CronScheduler");

    // Add schedule and send tools to registry
    let cron_scheduler_clone = cron_scheduler.clone();
    let schedule_tool = hydragent_tools::schedule_task::ScheduleTaskTool::new(move |cron_expr, desc, task_type, task_params, channel_id| {
        let scheduler = cron_scheduler_clone.clone();
        Box::pin(async move {
            scheduler.add_job(&cron_expr, &desc, &task_type, &task_params, &channel_id).await
        })
    });

    let heartbeat_clone = heartbeat.clone();
    let send_message_tool = hydragent_tools::send_message::SendMessageTool::new(move |channel_id, page_id, content| {
        let heartbeat = heartbeat_clone.clone();
        Box::pin(async move {
            heartbeat.push(channel_id, page_id, content).await
        })
    });

    // Add rss_subscribe tool to registry
    let work_iq_subscribe = work_iq.clone();
    let cron_scheduler_subscribe = cron_scheduler.clone();
    let rss_subscribe_tool = hydragent_tools::rss_subscribe::RssSubscribeTool::new(move |url, name, keywords, digest_channel, digest_cron| {
        let work_iq = work_iq_subscribe.clone();
        let scheduler = cron_scheduler_subscribe.clone();
        Box::pin(async move {
            // Add feed to DB
            work_iq.add_feed(&url, &name, &keywords, &digest_channel, &digest_cron).await?;
            // Add cron job for feed digest
            let _ = scheduler.add_job(
                &digest_cron,
                &format!("Work IQ Digest for {}", name),
                "work_iq_digest",
                &url,
                &digest_channel,
            ).await?;
            Ok(())
        })
    });

    registry.register(schedule_tool);
    registry.register(send_message_tool);
    registry.register(rss_subscribe_tool);

    let registry = Arc::new(registry);
    if registry_cell.set(registry.clone()).is_err() {
        panic!("Failed to initialize ToolRegistry OnceCell");
    }

    // ── Chat subcommand (interactive terminal REPL) ──────────────────
    //
    // Dispatched here, *after* the registry is built but *before* the
    // audit chain / bus router / bus server. The REPL only needs
    // (store, model_router, registry) and exits when stdin closes. The
    // bus server blocks forever; if we let it start, the user has to
    // Ctrl-C to get back to the REPL. So short-circuit early.
    //
    // We use `args.page` directly (it's a `Option<String>`, and
    // `ReplState::page_id` is also `Option<String>`) so a page
    // id is auto-minted inside `cli_repl::run` if the user didn't
    // pass `--page`.
    if let Some(Commands::Chat) = &args.command {
        let workspace_dir = std::env::var("WORKSPACE_DIR")
            .unwrap_or_else(|_| ".".to_string());
        // `--page` overrides the auto-minted one; otherwise we mint
        // a fresh UUID here so each `hydragent chat` invocation starts
        // a clean page (the user can `/resume` an older one).
        let page_id = args
            .page
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        // Build the brand metadata once, here, so the banner +
        // /status command see the same view. `detect_git_branch`
        // shells out to `git rev-parse --abbrev-ref HEAD`; we fall
        // back to "unknown" when git is not on PATH or the
        // directory isn't a checkout, so the header always has the
        // same number of lines. The model + tool count come from
        // the same `app_config` and `registry` the REPL uses for
        // its routing, so there's no chance of the banner
        // promising one model and the LLM speaking another.
        let brand = tui_header::BrandInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            branch: detect_git_branch(),
            path: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown cwd>".to_string()),
            page_id_short: short_id_pub(&page_id),
            model: app_config.effective_brain_model(),
            tool_count: registry.len(),
        };
        let state = cli_repl::ReplState {
            page_id,
            user_id: "local-cli".to_string(),
            channel_id: "cli".to_string(),
            store: store.clone(),
            model_router: Arc::clone(&model_router),
            registry: registry.clone(),
            max_react_steps: app_config.max_react_steps,
            app_config: app_config.clone(),
            workspace_dir: PathBuf::from(workspace_dir),
            brand,
        };
        let code = cli_repl::run(state).await;
        std::process::exit(code);
    }

    // Initialize runtime audit chain for Phase 6.1 / Merkle audit logging.
    let audit_dir = std::path::PathBuf::from(&app_config.data_dir).join("audit");
    let keys_dir = std::path::PathBuf::from(&app_config.data_dir).join("keys");
    std::fs::create_dir_all(&audit_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create audit directory: {}", e);
        std::process::exit(1);
    });
    std::fs::create_dir_all(&keys_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create keys directory: {}", e);
        std::process::exit(1);
    });
    let chain_path = audit_dir.join("chain.db");
    let key_path = keys_dir.join("agent_ed25519.key");
    let pub_path = std::path::PathBuf::from("config/keys/agent_ed25519.pub");
    let signer = load_or_create_agent_signer(&key_path, &pub_path).unwrap_or_else(|e| {
        eprintln!("Failed to load agent Ed25519 key: {}", e);
        std::process::exit(1);
    });
    let audit_chain = Arc::new(
        hydragent_security::MerkleAuditChain::connect(
            chain_path.to_str().unwrap(),
            Arc::new(signer),
        )
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to open audit chain: {}", e);
            std::process::exit(1);
        }),
    );

    // Spawn background Work IQ polling interval loop
    let work_iq_loop = work_iq.clone();
    tokio::spawn(async move {
        let poll_interval_sec = std::env::var("WORK_IQ_POLL_INTERVAL_SEC")
            .unwrap_or_else(|_| "300".to_string())
            .parse::<u64>()
            .unwrap_or(300);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(poll_interval_sec));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            tracing::info!("Starting background Work IQ feed polling cycle...");
            if let Err(e) = work_iq_loop.run_poll_cycle().await {
                tracing::error!("Error in background Work IQ polling cycle: {}", e);
            }
        }
    });

    // Initialize ActivePermissions
    let active_permissions = orchestrator::ActivePermissions::default();

    // Initialize pending-clarification map (one pending question per page).
    let pending_clarifications = std::sync::Arc::new(tokio::sync::Mutex::new(
        std::collections::HashMap::<String, hydragent_types::PendingClarification>::new(),
    ));

    // Initialize Event Bus Router and register handlers
    let mut router = hydragent_bus::router::Router::new();
    router.register("intent.submit", orchestrator::IntentSubmitHandler {
        store: store.clone(),
        model_router: model_router.clone(),
        registry,
        max_react_steps: app_config.max_react_steps,
        active_permissions: active_permissions.clone(),
        gateway_router: gateway_router.clone(),
        audit_chain: audit_chain.clone(),
        pending_clarifications: pending_clarifications.clone(),
    });
    router.register("permission.respond", orchestrator::PermissionRespondHandler {
        active_permissions,
    });
    router.register("gateway.register", orchestrator::GatewayRegisterHandler {
        gateway_router: gateway_router.clone(),
    });
    router.register("memory.list", orchestrator::MemoryListHandler {
        store: store.clone(),
    });
    router.register("memory.delete", orchestrator::MemoryDeleteHandler {
        store: store.clone(),
    });
    router.register("memory.clear", orchestrator::MemoryClearHandler {
        store: store.clone(),
    });
    router.register("memory.search", orchestrator::MemorySearchHandler {
        store: store.clone(),
    });
    router.register("dream.run", orchestrator::DreamRunHandler {
        store: store.clone(),
        model_router: model_router.clone(),
    });
    router.register("library.create_node", orchestrator::LibraryNodeCreateHandler {
        store: store.clone(),
    });
    router.register("library.link", orchestrator::LibraryLinkHandler {
        store: store.clone(),
    });
    router.register("library.search", orchestrator::LibrarySearchHandler {
        store: store.clone(),
    });
    router.register("library.list_nodes", orchestrator::LibraryNodeListHandler {
        store: store.clone(),
    });
    router.register("library.delete_node", orchestrator::LibraryNodeDeleteHandler {
        store: store.clone(),
    });
    router.register("page.compact", orchestrator::PageCompactHandler {
        store: store.clone(),
        model_router: model_router.clone(),
    });
    router.register("page.get_summary", orchestrator::PageGetSummaryHandler {
        store: store.clone(),
    });
    router.register("page.update_summary", orchestrator::PageUpdateSummaryHandler {
        store: store.clone(),
    });
    router.register("config.read", orchestrator::ConfigReadHandler);
    router.register("config.write", orchestrator::ConfigWriteHandler);

    // Create and start the Event Bus
    let bus = hydragent_bus::EventBus::new(router, app_config.bus_port);

    info!("Starting Event Bus server on port {}...", app_config.bus_port);
    if let Err(e) = bus.start().await {
        let msg = e.to_string();
        let port_in_use = msg.contains("10048")
            || msg.contains("98")
            || msg.to_lowercase().contains("address already in use")
            || msg.to_lowercase().contains("in use");
        if port_in_use {
            match check_port_owner(app_config.bus_port) {
                Some(pid) if is_hydragent_pid(pid) => {
                    info!(
                        "Port {} is already held by hydragent.exe (PID {}). \
                         Gateway is already running; this invocation is a no-op.",
                        app_config.bus_port, pid
                    );
                    eprintln!(
                        "\n  ℹ  Gateway already running as PID {}. Exiting cleanly.",
                        pid
                    );
                    eprintln!("     Use `hydragent status` to see the dashboard,");
                    eprintln!("     or `hydragent stop {}` to stop it.\n", pid);
                    std::process::exit(0);
                }
                Some(pid) => {
                    error!(
                        "Port {} is held by another process (PID {}).",
                        app_config.bus_port, pid
                    );
                    eprintln!(
                        "  ✗ Port {} is in use by another process (PID {}).",
                        app_config.bus_port, pid
                    );
                    eprintln!(
                        "    Set HYDRAGENT_BUS_PORT=<other> in .env and try again."
                    );
                    std::process::exit(1);
                }
                None => {
                    error!(
                        "Port {} is in use but we couldn't identify the owner.",
                        app_config.bus_port
                    );
                    eprintln!(
                        "  ✗ Port {} is held by an unknown process.",
                        app_config.bus_port
                    );
                    eprintln!(
                        "    On Windows: netstat -ano | findstr :{}",
                        app_config.bus_port
                    );
                    eprintln!("    On Unix:    lsof -iTCP:{} -sTCP:LISTEN",
                        app_config.bus_port);
                    std::process::exit(1);
                }
            }
        }
        error!("Event Bus server failed to run: {}", e);
        std::process::exit(1);
    }
}
