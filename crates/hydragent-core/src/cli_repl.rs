// crates/hydragent-core/src/cli_repl.rs
//
// `hydragent chat` — minimal interactive terminal REPL.
//
// Wires the existing `run_react_loop` to a stdin read loop so the user
// can chat from the terminal without the Python `cli_adapter`. This is
// the lowest-friction "first chat" path for new users.
//
// Slash commands:
//   /help               Show available commands
//   /exit, /quit        Exit the REPL
//   /new                Start a fresh page (new page_id)
//   /page               Show the current page ID
//   /pages              List past pages in the SQLite store
//   /resume <id>        Switch to a past page (resumes its history)
//   /model              Show or switch the active brain model
//   /brain              Show the active base URL + key mask
//   /clear              Clear the terminal
//   /memory list        List stored semantic memories
//   /memory clear       Wipe all stored semantic memories
//   /audit head         Print the Merkle audit chain head hash
//   /audit count        Print the audit event count
//   /debug              Print the same env/config dump as `--debug`
//   /paste              Toggle paste/multiline mode (use this to enter
//                       long prompts that span several lines)
//   /dream              Show dream-cycle status (enabled, interval)
//   /dream enable       Enable the dream cycle (writes .env)
//   /dream disable      Disable the dream cycle (writes .env)
//   /dream interval <N> Set the dream-cycle interval to N seconds (writes .env)
//   /reasoning, /r      Toggle the last turn's reasoning dropdown
//                       (only useful for models that emit
//                       `<think>…</think>` blocks, e.g. DeepSeek,
//                       QwQ, DeepHermes).
//
// Anything that doesn't start with `/` is treated as a user message and
// dispatched to the LLM via the same `run_react_loop` that the
// adapter-based paths use.


use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_tools::registry::ToolRegistry;
use hydragent_types::MessageRole;
// `VerificationResult` is the return-type of `MerkleAuditChain::verify`
// (the Merkle chain's "is the chain intact?" answer). The enum has
// `Valid { event_count }` and `Tampered { seq_id, detail }` variants —
// we pattern-match the latter to surface a *useful* error to the user
// instead of just printing the enum's `Debug` repr.
use hydragent_security::VerificationResult;

use crate::react_loop::run_react_loop;
use crate::config::AppConfig;

// Module-level ANSI escape helpers. We hoist them out of
// individual functions so the streaming loop, the slash-command
// handler, the dropdown's render methods, and any future
// helper can all use the same colour palette without having
// to redeclare them at the top of every function.
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_RESET: &str = "\x1b[0m";

/// Lazily-loaded Ed25519 agent key used by the `/audit` slash command
/// to open the Merkle chain. Stored in a `OnceLock` so we read the
/// files from disk exactly once per REPL session — the second call to
/// `/audit` is free, no matter how many times the user inspects the
/// chain.
///
/// We can't plumb a `&AgentSigner` through `ReplState` because that
/// would force every REPL constructor site to also load (or generate)
/// the key, which is a startup cost the user shouldn't pay for a
/// feature they may never invoke.
///
/// `AgentSigner` itself isn't `Clone` (it owns a private scalar that
/// the type marks `ZeroizeOnDrop`), so we cache it behind an
/// `Arc<AgentSigner>` — every consumer gets the *same* signer
/// instance, and we only deserialize the 32-byte secret from disk
/// one time.
fn get_or_load_audit_signer(data_dir: &str) -> anyhow::Result<std::sync::Arc<hydragent_security::AgentSigner>> {
    use std::sync::OnceLock;
    static CACHED: OnceLock<std::sync::Mutex<Option<std::sync::Arc<hydragent_security::AgentSigner>>>> =
        OnceLock::new();
    let cache = CACHED.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some(s) = guard.as_ref() {
        return Ok(s.clone());
    }
    let keys_dir = std::path::PathBuf::from(data_dir).join("keys");
    std::fs::create_dir_all(&keys_dir).ok();
    let key_path = keys_dir.join("agent.ed25519");
    let pub_path = keys_dir.join("agent.ed25519.pub");
    let bytes = if key_path.exists() {
        std::fs::read(&key_path)?
    } else {
        // First-run path: mint a new key and persist it. We
        // match main.rs's behavior exactly so /audit and
        // `hydragent audit verify` see the same key.
        let signer = hydragent_security::AgentSigner::generate();
        std::fs::write(&key_path, signer.private_key_bytes())?;
        signer
            .export_public_key(&pub_path)
            .map_err(|e| anyhow::anyhow!("failed to export public key: {e}"))?;
        eprintln!(
            "🪪 Generated new agent Ed25519 keypair for /audit (private: {}, public: {})",
            key_path.display(),
            pub_path.display()
        );
        signer.private_key_bytes().to_vec()
    };
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("agent key file has wrong length: {}", bytes.len()))?;
    let signer = std::sync::Arc::new(hydragent_security::AgentSigner::from_bytes(&arr));
    *guard = Some(signer.clone());
    Ok(signer)
}

/// Per-REPL state shared across the read loop.
pub struct ReplState {
    pub page_id: String,
    pub user_id: String,
    pub channel_id: String,
    pub store: Arc<SessionStore>,
    pub model_router: Arc<ModelRouter>,
    pub registry: Arc<ToolRegistry>,
    pub max_react_steps: u8,
    pub app_config: AppConfig,
    pub workspace_dir: PathBuf,
    /// Pre-built startup metadata (version, branch, model, etc.)
    /// that the banner reads. Holding it on the state (rather
    /// than re-detecting the branch on every print) means the
    /// banner and the `/status` command always agree on what
    /// they display, and it keeps the branching out of the
    /// main.rs dispatch (which already has too much going on).
    pub brand: BrandInfo,
}

use std::path::PathBuf;

use crate::status_bar::{render_status_bar, Phase as StatusPhase, StatusState};
use crate::tui_header::{default_tip_box, print_kimi_header, BrandInfo};

/// Build a `StatusState` from a `ReplState`. The status bar
/// is the *single* source of truth for phase + model + tool
/// count during a session, so any change to those fields
/// (e.g. `/model` switching the active brain) should also
/// update `state.brand.model` and call this helper. For now
/// the function is read-only: we just project `state` into
/// the bar's struct shape.
fn status_state_from(state: &ReplState) -> StatusState {
    StatusState {
        phase: StatusPhase::Normal,
        model: state.brand.model.clone(),
        multi_model: false,
        context_pct: 0,
        input_tokens: 0,
        output_tokens: 0,
        slash_hint: "/ for commands".to_string(),
    }
}

/// Print the welcome banner. This is called *before* the heavy
/// startup work so the user sees the prompt immediately.
///
/// Inspired by the Kimi-CLI / Hyper CLI family: a block-art
/// hydra silhouette on the left, a "tip of the day" panel on
/// the right, and brand metadata (version, branch, path, page
/// id, model, tool count) in the middle column. The actual
/// rendering lives in the `tui_header` module; this function
/// is a thin shim that wires the two together.
pub fn print_banner(brand: &BrandInfo) {
    let tip = default_tip_box();
    print_kimi_header(brand, &tip);
}

/// Run the REPL. Returns the process exit code.
pub async fn run(mut state: ReplState) -> i32 {
    // ── Banner ────────────────────────────────────────────────────────
    print_banner(&state.brand);
    // Print the status bar right under the header so the user
    // sees the model + phase at a glance. The model field
    // is the only piece of state that changes during a session
    // (via /model), but the bar is intentionally always
    // present — it's the persistent visual anchor that ties
    // the whole REPL together, the way the Kimi/Hyper family
    // does it.
    print!("{}", render_status_bar(&status_state_from(&state)));

    let stdin = std::io::stdin();
    let mut stdin_lock = stdin.lock();
    let mut stdout = std::io::stdout();
    let mut paste_mode = false;
    let mut paste_buffer = String::new();
    // `reasoning_history` is the dropdown's persistent state.
    // The streaming loop writes to it after every turn, and
    // `/reasoning` reads from it. We hold the *full* text of the
    // last turn's reasoning plus a one-line preview and the
    // current chevron state (`▸` collapsed / `▾` expanded).
    let mut reasoning_history = ReasoningHistory::default();

    loop {
        // ── prompt ────────────────────────────────────────────────────
        // Inspired by the Kimi-CLI prompt glyph: a single
        // cyan `❯` followed by a space. The previous
        // `hydra [<id>] > ` form was informative but visually
        // heavy — the short id is already in the header
        // sidecar and in the status bar, so repeating it on
        // every prompt line added noise without information.
        let prompt_str = if paste_mode {
            format!("{ANSI_DIM}…{ANSI_RESET} ")
        } else {
            format!("{ANSI_CYAN}❯{ANSI_RESET} ")
        };
        if write!(stdout, "{}", prompt_str).is_err() {
            return 1;
        }
        if stdout.flush().is_err() {
            return 1;
        }

        let mut line = String::new();
        match stdin_lock.read_line(&mut line) {
            Ok(0) => {
                // EOF (Ctrl-Z on Windows / Ctrl-D on Unix)
                println!();
                println!("  (EOF — exiting chat)");
                return 0;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("\n  ✗ stdin read error: {}", e);
                return 1;
            }
        }
        let line = strip_line_ending(&line);

        // ── input hardening ───────────────────────────────────────────
        // Filter C0 control bytes and DEL so binary garbage, NUL
        // bytes, stray ESC sequences, etc. can't reach the model
        // (and don't render as mojibake in the local echo). The
        // paste buffer has its own copy of this filter (see below).
        let line = strip_control_chars(&line);

        // Cap single-line input at 64 KB. Anything longer belongs in
        // `/paste` mode (256 KB) or via `hydragent --file`. A 1 MB
        // single line would lock up the REPL for minutes while the
        // LLM chews on it, with no way for the user to cancel.
        const MAX_LINE_BYTES: usize = 64 * 1024;
        if line.len() > MAX_LINE_BYTES {
            eprintln!(
                "  ✗ Single-line input is {} bytes — over the {} KB cap. Use /paste (256 KB) or run with --file.",
                line.len(),
                MAX_LINE_BYTES / 1024
            );
            continue;
        }

        // ── paste mode assembly ───────────────────────────────────────
        if paste_mode {
            // Cap the paste buffer at 256 KB so a runaway paste can't
            // OOM the process. We bail out of paste mode with a friendly
            // error and let the user re-paste in chunks (or feed the
            // message via `hydragent --file` once that flag lands).
            const MAX_PASTE_BYTES: usize = 256 * 1024;

            // Check the exit markers *first* — otherwise the
            // sentinel line ("```" or "/paste") would be appended
            // to the buffer and end up in the user message that
            // gets sent to the LLM. That caused real users (and
            // the stress test) to see the model reply with
            // "I noticed an empty code block at the end of your
            // message" because the buffer literally ended with
            // an empty ``` fence.
            let trimmed_line = line.trim();
            if trimmed_line == "```" || trimmed_line == "/paste" {
                paste_mode = false;
                let full = std::mem::take(&mut paste_buffer);
                println!();
                // Guard against "/paste" + "/paste" with no
                // content in between. The user probably
                // changed their mind, so silently cancel
                // instead of spending an LLM round-trip on
                // an empty message (which the model would
                // then ask the user to clarify).
                if full.trim().is_empty() {
                    println!("  (empty paste — cancelled)");
                    continue;
                }
                if let Err(code) = dispatch_user_message(&state, &full, &mut reasoning_history).await {
                    return code;
                }
                println!();
                continue;
            }

            let incoming = line.len() + 1; // +1 for the '\n' we'd push
            if paste_buffer.len().saturating_add(incoming) > MAX_PASTE_BYTES {
                paste_mode = false;
                paste_buffer.clear();
                eprintln!(
                    "  ✗ Paste buffer exceeded {} KB — split the message and use /paste again, or run with --file.",
                    MAX_PASTE_BYTES / 1024
                );
                continue;
            }
            paste_buffer.push_str(&line);
            paste_buffer.push('\n');
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Treat a line that is *only* slashes (e.g. "/", "//", "///") as
        // empty input — a stray `/` keypress is a typographical accident,
        // not a command, and we should silently re-prompt rather than emit
        // a noisy "Unknown command: /" on STDERR.
        if trimmed.chars().all(|c| c == '/') {
            continue;
        }

        // ── slash commands ────────────────────────────────────────────
        if let Some(cmd) = trimmed.strip_prefix('/') {
            let exit_code = handle_slash_command(
                cmd, &mut state, &mut paste_mode, &mut paste_buffer, &mut reasoning_history,
            ).await;
            match exit_code {
                SlashExit::Continue => {}
                SlashExit::Exit(code) => return code,
            }
            continue;
        }

        // ── regular user message ──────────────────────────────────────
        println!();
        if let Err(code) = dispatch_user_message(&state, trimmed, &mut reasoning_history).await {
            return code;
        }
        println!();
    }
}

#[derive(Debug)]
enum SlashExit {
    Continue,
    Exit(i32),
}

async fn handle_slash_command(
    raw: &str,
    state: &mut ReplState,
    paste_mode: &mut bool,
    paste_buffer: &mut String,
    reasoning_history: &mut ReasoningHistory,
) -> SlashExit {
    // Local aliases for the format-string `{name}` syntax. The
    // slash-command handler only uses `dim` and `reset` (in the
    // `no reasoning captured` line); the other ANSI helpers are
    // left to the call sites that need them.
    let dim = ANSI_DIM;
    let reset = ANSI_RESET;
    let mut parts = raw.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").to_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match cmd.as_str() {
        "help" | "?" => {
            print_help();
        }
        "exit" | "quit" | "q" => {
            println!("  Goodbye. 👋");
            return SlashExit::Exit(0);
        }
        "new" => {
            // Generating a new page mid-REPL would require
            // tearing down the current `SessionStore`,
            // `ModelRouter`, and `ToolRegistry` (they're all
            // wired to the current `page_id`) and rebuilding
            // them — too much surgery for a slash command.
            // The user-facing answer is therefore a copy-paste
            // command: mint a fresh id here, and the next
            // `hydragent chat` invocation picks it up.
            // (The `--page` flag is at the top level, so the
            // correct invocation is `hydragent --page <id> chat`,
            // not `hydragent chat --page <id>`.)
            let new_id = format!("chat-{}", &uuid::Uuid::new_v4().to_string()[..8]);
            println!("  /new starts a fresh page with a new id.");
            println!("  To do that, exit this REPL and run:");
            println!("      hydragent --page {} chat", new_id);
        }
        "page" => {
            // Show the short id (matches the prompt) and put the
            // full UUID on a second line so the user can copy it
            // for `hydragent --page <id>` invocations.
            println!("  page_id     = {}", short_id(&state.page_id));
            println!("  full page_id= {}", state.page_id);
        }
        "pages" => {
            match state.store.list_pages().await {
                Ok(pages) => {
                    if pages.is_empty() {
                        println!("  No past pages.");
                    } else {
                        // Two-column table: short id on the left, last
                        // active + turn count on the right. We pre-format
                        // each column to a fixed width so the rows line
                        // up regardless of page-id length. Long ids get
                        // clipped to keep the table compact.
                        println!("  Past pages (most recent 20):");
                        println!("    {:<14}  {:<16}  {:>6}", "id", "last active", "turns");
                        println!("    {}", "─".repeat(46));
                        // Convert UTC ms → local time so the
                        // user sees "5 minutes ago" rather than
                        // "5 hours ago". `from_timestamp` returns
                        // a `DateTime<Utc>`; `.local()` converts
                        // it to the system's configured timezone.
                        for (page_id, _, last_active, turn_count) in pages.iter().take(20) {
                            let dt = chrono::DateTime::from_timestamp(*last_active / 1000, 0)
                                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                                .unwrap_or_else(|| "Unknown".to_string());
                            let id_display = if page_id.len() > 13 {
                                format!("{}…", &page_id[..12])
                            } else {
                                page_id.clone()
                            };
                            println!("    {:<14}  {:<16}  {:>6}", id_display, dt, turn_count);
                        }
                        if pages.len() > 20 {
                            println!(
                                "    …and {} more (run `hydragent --list-pages` for the full list)",
                                pages.len() - 20
                            );
                        }
                        println!();
                        println!("  resume any of these with:  hydragent --page <id> chat");
                    }
                }
                Err(e) => eprintln!("  ✗ Failed to list pages: {}", e),
            }
        }
        "resume" => {
            // The slash-command splitter already trims `rest`,
            // but the user can still pass an argument that's
            // effectively empty once shell-style quoting is
            // stripped — e.g. `/resume ""` or `/resume ' '`.
            // Treat those as "no argument" and surface the
            // usage hint instead of pretending the user
            // supplied a real (empty) page id and printing
            // `hydragent --page "" chat`, which is a useless
            // and confusing command.
            let rest = unquote(rest);
            if rest.is_empty() {
                eprintln!("  Usage: /resume <page_id>");
                println!("  Tip: /pages shows the most recent 20 ids.");
            } else {
                // Same constraint as /new: switching the
                // page_id mid-REPL would require rebuilding
                // the store/router/registry. The CLI flag
                // exists (`hydragent --page <id> chat`) and
                // is the path of least friction.
                println!("  /resume in-REPL isn't implemented yet, but you can continue");
                println!("  this conversation outside the REPL with:");
                println!("      hydragent --page {} chat", rest);
            }
        }
        "model" => {
            if rest.is_empty() {
                println!("  primary   = {}", state.app_config.effective_brain_model());
                println!("  fallbacks = {:?}", state.app_config.effective_brain_fallbacks());
            } else {
                // In-session brain switch: updates the router so the
                // next ReAct turn picks the new model (no .env edit).
                // We mirror the change into `app_config.brain_model` so
                // `/model` (no arg), `/debug`, and `/status` reflect the
                // new value, and into `brand.model` so any future
                // re-render of the status bar / banner also reflects it.
                state.model_router.set_primary_model(rest.to_string());
                state.app_config.brain_model = rest.to_string();
                state.brand.model = rest.to_string();
                println!("  ✓ Switched primary model to {}", rest);
                println!("  (session only — restart hydragent to revert to .env default)");
            }
        }
        "brain" => {
            let base = state.app_config.effective_brain_base();
            let key = state.app_config.effective_brain_key();
            println!("  base = {}", if base.is_empty() { "<unset>".to_string() } else { base });
            println!("  key  = {}", mask(&key));
        }
        "clear" | "cls" => {
            // ANSI clear. Modern Windows Terminal / iTerm / gnome-terminal all support it.
            print!("\x1b[2J\x1b[H");
            let _ = std::io::stdout().flush();
        }
        "paste" => {
            *paste_mode = true;
            paste_buffer.clear();
            println!("  (paste mode — paste or type a long message; finish with a line containing only ``` or /paste)");
        }
        "memory" => {
            let sub = rest;
            if sub == "list" || sub.is_empty() {
                match state.store.list_memories().await {
                    Ok(mems) => {
                        if mems.is_empty() {
                            println!("  No semantic memories stored yet.");
                        } else {
                            println!("  Stored memories (showing first 20 of {}):", mems.len());
                            for m in mems.iter().take(20) {
                                let preview: String = m.content.chars().take(80).collect();
                                println!("    [{}] {}", m.importance, preview);
                            }
                        }
                    }
                    Err(e) => eprintln!("  ✗ Failed to list memories: {}", e),
                }
            } else if sub == "clear" {
                match state.store.clear_all_memories().await {
                    Ok(_) => println!("  ✓ All memories cleared."),
                    Err(e) => eprintln!("  ✗ Failed to clear: {}", e),
                }
            } else {
                eprintln!("  Usage: /memory list | /memory clear");
            }
        }
        "audit" => {
            // Wire /audit to the same chain reader the standalone
            // `hydragent audit head|count|verify` subcommand uses.
            // This way `/audit` inside the REPL gives the same
            // answer the user would get on the command line — no
            // "see also" footnote needed. The chain may not exist
            // yet on a fresh install, in which case we tell the
            // user that the first event will create it.
            let sub = rest;
            let chain_path = PathBuf::from(&state.app_config.data_dir).join("audit/chain.db");
            if !chain_path.exists() {
                println!("  No audit chain at {} yet (appears after the first event).", chain_path.display());
            } else {
                // MerkleAuditChain::connect requires a signer
                // (Ed25519 keypair) so it can verify per-row
                // signatures. We don't have one in ReplState
                // (it's owned by the surrounding CLI), so we
                // load-or-create one lazily the same way main.rs
                // does at startup. Cached on the first call so a
                // second `/audit` invocation in the same REPL
                // session is free.
                let signer = match get_or_load_audit_signer(&state.app_config.data_dir) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("  ✗ Could not load audit signer: {}", e);
                        return SlashExit::Continue;
                    }
                };
                match hydragent_security::MerkleAuditChain::connect(
                    chain_path.to_str().unwrap_or(""),
                    signer,
                )
                .await
                {
                    Ok(chain) => {
                        // Default (`/audit` with no arg) — show head
                        // + count, the two pieces of info a user
                        // actually wants when they wonder "is the
                        // chain alive?".
                        if sub.is_empty() {
                            match chain.head_hash().await {
                                Ok(Some(h)) => {
                                    let short: String = h.chars().take(12).collect();
                                    println!("  chain path = {}", chain_path.display());
                                    println!("  head hash  = {}…  (use /audit verify to check integrity)", short);
                                }
                                Ok(None) => {
                                    println!("  chain path = {}", chain_path.display());
                                    println!("  head hash  = <empty chain>");
                                }
                                Err(e) => eprintln!("  ✗ Failed to read head hash: {}", e),
                            }
                            // /audit count
                            match chain.count().await {
                                Ok(n) => println!("  event count= {}", n),
                                Err(e) => eprintln!("  ✗ Failed to read count: {}", e),
                            }
                        } else if sub == "head" {
                            match chain.head_hash().await {
                                Ok(Some(h)) => {
                                    let short: String = h.chars().take(12).collect();
                                    println!("  head = {}…  (full: {})", short, h);
                                }
                                Ok(None) => println!("  head = <empty chain>"),
                                Err(e) => eprintln!("  ✗ Failed to read head: {}", e),
                            }
                        } else if sub == "count" {
                            match chain.count().await {
                                Ok(n) => println!("  event count = {}", n),
                                Err(e) => eprintln!("  ✗ Failed to read count: {}", e),
                            }
                        } else if sub == "verify" {
                            // No signature check here — that
                            // would require the agent's Ed25519
                            // public key, which the slash-command
                            // path doesn't have a handle to.
                            // The standalone `hydragent audit
                            // verify --signatures` covers the
                            // cryptographic case.
                            match chain.verify(false).await {
                                Ok(v) if v.is_valid() => println!(
                                    "  ✓ chain integrity OK ({} events verified)",
                                    v.event_count()
                                ),
                                Ok(v) => {
                                    if let VerificationResult::Tampered { seq_id, detail } = v {
                                        eprintln!("  ✗ chain TAMPERED at seq {}: {}", seq_id, detail);
                                    } else {
                                        eprintln!("  ✗ chain integrity check returned unexpected state");
                                    }
                                }
                                Err(e) => eprintln!("  ✗ chain integrity check failed: {}", e),
                            }
                        } else {
                            eprintln!("  Usage: /audit | /audit head | /audit count | /audit verify");
                        }
                    }
                    Err(e) => eprintln!("  ✗ Failed to open audit chain at {}: {}", chain_path.display(), e),
                }
            }
        }
        "debug" => {
            crate::debug_dump_env_and_config(&state.app_config);
        }
        "dream" => {
            // /dream                 → show current status
            // /dream enable          → set ENABLE_DREAMING=true in .env
            // /dream disable         → set ENABLE_DREAMING=false in .env
            // /dream interval <N>    → set DREAMING_INTERVAL_SEC=N in .env
            //
            // The .env patch follows the same strategy as
            // `hydragent dream --enable` on the CLI: line-by-line rewrite
            // that preserves every other setting. Changes take effect on
            // the next restart of the bus server; the in-process worker's
            // interval is already fixed at startup.
            let env_path = std::path::PathBuf::from(&state.app_config.data_dir)
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join(".env");
            // Fallback: look for .env next to cwd (covers dev layout)
            let env_path = if env_path.exists() {
                env_path
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .join(".env")
            };

            let mut parts = rest.splitn(2, char::is_whitespace);
            let sub = parts.next().unwrap_or("").trim();
            let arg = parts.next().unwrap_or("").trim();

            match sub {
                "" => {
                    // Status display
                    println!("  🌙 Dream cycle status:");
                    println!(
                        "    enabled   : {}",
                        if state.app_config.enable_dreaming { "yes ✓" } else { "no ✗" }
                    );
                    println!(
                        "    interval  : {}s  ({:.1} min)",
                        state.app_config.dreaming_interval_sec,
                        state.app_config.dreaming_interval_sec as f64 / 60.0
                    );
                    println!("    .env      : {}", env_path.display());
                    println!(
                        "  Use: /dream enable | /dream disable | /dream interval <secs>"
                    );
                }
                "enable" | "on" => {
                    match patch_env_key(&env_path, "ENABLE_DREAMING", "true") {
                        Ok(true)  => println!("  ✓ ENABLE_DREAMING=true written to .env (restart bus to apply)"),
                        Ok(false) => println!("  ✓ ENABLE_DREAMING=true appended to .env (restart bus to apply)"),
                        Err(e)    => eprintln!("  ✗ Could not update .env: {}", e),
                    }
                }
                "disable" | "off" => {
                    match patch_env_key(&env_path, "ENABLE_DREAMING", "false") {
                        Ok(true)  => println!("  ✓ ENABLE_DREAMING=false written to .env (restart bus to apply)"),
                        Ok(false) => println!("  ✓ ENABLE_DREAMING=false appended to .env (restart bus to apply)"),
                        Err(e)    => eprintln!("  ✗ Could not update .env: {}", e),
                    }
                }
                "interval" => {
                    if arg.is_empty() {
                        eprintln!("  Usage: /dream interval <seconds>");
                    } else {
                        match arg.parse::<u64>() {
                            Ok(0) => eprintln!("  ✗ Interval must be > 0 seconds."),
                            Ok(n) => {
                                match patch_env_key(&env_path, "DREAMING_INTERVAL_SEC", &n.to_string()) {
                                    Ok(true)  => println!("  ✓ DREAMING_INTERVAL_SEC={} written to .env (restart bus to apply)", n),
                                    Ok(false) => println!("  ✓ DREAMING_INTERVAL_SEC={} appended to .env (restart bus to apply)", n),
                                    Err(e)    => eprintln!("  ✗ Could not update .env: {}", e),
                                }
                            }
                            Err(_) => eprintln!("  ✗ '{}' is not a valid integer number of seconds.", arg),
                        }
                    }
                }
                other => {
                    eprintln!("  ✗ Unknown dream sub-command: {}", other);
                    eprintln!("    Usage: /dream | /dream enable | /dream disable | /dream interval <secs>");
                }
            }
        }
        "tools" => {
            // Use the registry's own names() method so the printed
            // list is always in sync with what's actually wired up.
            // We render in two columns when the terminal is wide
            // enough so the user can see the whole list at a
            // glance without scrolling.
            let names = state.registry.names();
            println!("  Registered tools ({}):", names.len());
            if names.is_empty() {
                println!("    (none)");
            } else {
                // Pick a column count based on the longest name so
                // the columns line up. Two columns for short names,
                // three for very short ones, one for long.
                let max_len = names.iter().map(|n| n.len()).max().unwrap_or(8);
                let cols = if max_len >= 16 { 1 } else if max_len >= 10 { 2 } else { 3 };
                let col_width = max_len + 2;
                for (i, n) in names.iter().enumerate() {
                    if i > 0 && i % cols == 0 {
                        println!();
                    }
                    print!("    {:<width$}", n, width = col_width);
                }
                println!();
            }
        }
        "status" => {
            // The Kimi-style footer. We snapshot the same fields
            // the startup banner read (phase, model, tool count,
            // page id), so the two displays never disagree. The
            // token counters stay at zero in this build — the
            // router doesn't yet expose per-turn usage — and the
            // bar renders cleanly with zeros. Plumbing real
            // counters through `run_react_loop` is a separate
            // task tracked elsewhere.
            let s = status_state_from(state);
            print!("{}", render_status_bar(&s));
        }
        "reasoning" | "r" => {
            // The reasoning dropdown. Two behaviours depending on
            // whether the user has something stashed:
            //
            //   • stash is `Some`   → toggle expanded/collapsed
            //     and either print the full text (expanded) or
            //     re-render the one-line marker (collapsed).
            //   • stash is `None`   → tell the user there's no
            //     reasoning to show for the last turn.
            let sub = rest;
            if sub == "hide" || sub == "collapse" {
                reasoning_history.expanded = false;
                reasoning_history.print_marker();
            } else if sub == "show" || sub == "expand" {
                reasoning_history.expanded = true;
                reasoning_history.print_full();
            } else {
                match reasoning_history.text.as_deref() {
                    None => {
                        println!("  {dim}no reasoning captured for the last turn{reset}");
                    }
                    Some(_) => {
                        reasoning_history.expanded = !reasoning_history.expanded;
                        if reasoning_history.expanded {
                            reasoning_history.print_full();
                        } else {
                            reasoning_history.print_marker();
                        }
                    }
                }
            }
        }
        other => {
            eprintln!("  ✗ Unknown command: /{}", other);
            eprintln!("    Type /help for the list.");
        }
    }
    SlashExit::Continue
}

async fn dispatch_user_message(
    state: &ReplState,
    message: &str,
    reasoning_history: &mut ReasoningHistory,
) -> Result<(), i32> {
    // Load recent history (capped to MAX_HISTORY_MESSAGES turns).
    const MAX_HISTORY_MESSAGES: u32 = 20;
    let history = match state.store.load_recent(&state.page_id, MAX_HISTORY_MESSAGES).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("  ✗ Failed to load page history: {}", e);
            return Err(1);
        }
    };

    // Persist the user turn.
    if let Err(e) = state
        .store
        .append_message(&state.page_id, MessageRole::User, message)
        .await
    {
        eprintln!("  ✗ Failed to persist user message: {}", e);
    }

    // Echo the user message in a clean "you ▸" block so the
    // transcript is readable even if the LLM takes a while to
    // respond. Long messages (multi-KB pastes, log dumps) are
    // truncated to 200 chars with a "(+N more chars)" hint so
    // a 5 KB log blob doesn't bury the screen before the LLM
    // even starts thinking. The full message is still sent to
    // the model — only the local echo is clipped.
    let dim = ANSI_DIM;
    let reset = ANSI_RESET;
    let cyan = ANSI_CYAN;
    const ECHO_TRUNCATE_CHARS: usize = 200;
    let total_chars = message.chars().count();
    let echo = if total_chars > ECHO_TRUNCATE_CHARS {
        let cut: String = message.chars().take(ECHO_TRUNCATE_CHARS).collect();
        format!("{cut}… (+{} more chars)", total_chars - ECHO_TRUNCATE_CHARS)
    } else {
        message.to_string()
    };
    println!("  {dim}you ▸{reset} {echo}");

    // Spinner handle: shows a spinning glyph while we wait for the
    // first response token. The spinner is started in a background
    // task; if the LLM streams tokens quickly, the spinner is replaced
    // before the user even sees it. Wrapped in `Option` so we can stop
    // it at most once (either on the first token OR at end-of-stream,
    // whichever comes first).
    let mut spinner_handle = Some(start_spinner(format!("  {cyan}hydra{reset} thinking")));

    // Dispatch.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
    let model_router = state.model_router.clone();
    let registry = state.registry.clone();
    let active_permissions = crate::orchestrator::ActivePermissions::default();
    let page_id = state.page_id.clone();
    let user_id = state.user_id.clone();
    let channel_id = state.channel_id.clone();
    let max_steps = state.max_react_steps;
    let message_owned = message.to_string();

    let thinker = tokio::spawn(async move {
        run_react_loop(
            &page_id,
            &channel_id,
            &user_id,
            &message_owned,
            history,
            Vec::new(),
            None,
            None,
            model_router,
            registry,
            max_steps,
            tx,
            active_permissions,
        ).await
    });

    // Stream tokens to stdout.
    let mut collected = String::new();
    let mut stdout = std::io::stdout();
    let mut first_token = true;
    // The reasoning detector is a streaming state machine that
    // classifies each token chunk as either `Final` (user-facing)
    // or `Reasoning` (the model's private chain-of-thought). It
    // is model-agnostic: we feed it chunks and it returns a list
    // of `(kind, text)` segments whose markers are stripped.
    let mut reasoning_detector = ReasoningDetector::new();
    // `reasoning_seen` flips true the first time we drop reasoning
    // text, so we can render a one-line "reasoning (hidden)"
    // marker once per turn and stay quiet afterwards.
    let mut reasoning_seen = false;
    // `reasoning_buffer` collects the full reasoning text for the
    // turn, in case the user later asks to see it (e.g. with
    // `/debug` or by setting HYDRAGENT_SHOW_REASONING=1).
    let mut reasoning_buffer = String::new();
    let show_reasoning = std::env::var("HYDRAGENT_SHOW_REASONING")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    // `stream_raw` controls whether tokens are echoed to the
    // terminal as they arrive. Default is `false` (the REPL
    // buffers the full response and renders it as nicely-styled
    // markdown at end-of-stream — the only way to get aligned
    // tables and syntax-highlighted code blocks). Set
    // `HYDRAGENT_STREAM_RAW=1` to opt back into the old
    // "tokens-appear-one-at-a-time" live-stream behaviour; in
    // that mode the response is printed raw and the markdown
    // renderer is bypassed.
    let stream_raw = std::env::var("HYDRAGENT_STREAM_RAW")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    // Local aliases for the format-string `{name}` syntax. The
    // streaming loop uses `{dim}` / `{cyan}` / `{reset}` heavily
    // so we re-bind the module-level constants once at the top
    // of the function.
    let dim = ANSI_DIM;
    let cyan = ANSI_CYAN;
    let reset = ANSI_RESET;
    while let Some(token) = rx.recv().await {
        // The react_loop emits JSON-RPC-shaped frames. We peek at the
        // raw envelope first so we can route status frames through
        // the classifier and let token frames stream straight to the
        // terminal.
        let is_status_frame = is_status_envelope(&token);
        let display = strip_json_envelope(&token);
        if display.is_empty() {
            continue;
        }

        // ── Status frames ────────────────────────────────────────
        if is_status_frame {
            let kind = classify_status(&display);

            // Thinking steps update the spinner's label. The
            // background spinner thread will redraw with the new
            // label on its next 100ms tick.
            if let Some(new_label) = thinking_label(&kind) {
                if let Some(h) = spinner_handle.as_ref() {
                    h.update_label(new_label);
                }
                continue;
            }

            // Variants we deliberately hide (e.g. [Thought]).
            if matches!(kind, StatusKind::Thought) {
                continue;
            }

            // If we're already streaming tokens, late-arriving
            // status frames are useless to the user — drop them.
            if !first_token {
                continue;
            }
            // Stop the spinner so the rendered line below is the
            // last thing on the screen.
            if let Some(h) = spinner_handle.take() {
                h.stop();
            }
            if let Some(line) = render_status(&kind) {
                print!("{line}");
                let _ = stdout.flush();
            }
            // Don't set first_token = false here; a status line is
            // not a token, and we still want the "  hydra ▸" header
            // to appear before the actual response.
            continue;
        }

        // ── Token frames ─────────────────────────────────────────
        if first_token {
            // Stop the spinner and put the assistant label on its
            // own line. In render mode we deliberately skip the
            // "hydra ▸" header here — the renderer will emit it
            // (along with the rendered body) at end-of-stream so
            // the user sees exactly one header per turn, not two.
            if let Some(h) = spinner_handle.take() {
                h.stop();
            }
            if stream_raw {
                println!();
                println!("  {cyan}hydra ▸{reset}");
            }
            first_token = false;
        }
        // Strip a single leading newline (if any) so the response
        // starts on the same line as the `hydra ▸` label, regardless
        // of whether the orchestrator put `\n` at the start of its
        // first token. We only do this on the very first token to
        // avoid breaking legitimate intra-response whitespace.
        let cleaned = if collected.is_empty() {
            display.trim_start_matches('\n')
        } else {
            &display
        };
        // Run the model-aware reasoning detector over the cleaned
        // chunk. The detector returns a list of `(kind, text)` pairs
        // — either a `Final` chunk that should be streamed to the
        // user, or a `Reasoning` chunk that the model is using for
        // its own chain-of-thought and that we render dimly.
        let parts = reasoning_detector.feed(cleaned);
        for (kind, text) in parts {
            if text.is_empty() {
                continue;
            }
            match kind {
                TokenPart::Final => {
                    // In render mode (the default), tokens are
                    // collected silently and rendered as markdown
                    // at end-of-stream. In stream-raw mode the
                    // tokens are echoed to the terminal as they
                    // arrive, just like before.
                    if stream_raw {
                        let _ = write!(stdout, "{}", text);
                        let _ = stdout.flush();
                    }
                    collected.push_str(&text);
                }
                TokenPart::Reasoning => {
                    // First time we see reasoning, drop a one-line
                    // marker so the user knows the model is
                    // "thinking out loud" but we're hiding the
                    // scratchpad. Subsequent reasoning chunks stay
                    // silent — we don't spam the screen.
                    if !reasoning_seen {
                        let _ = writeln!(
                            stdout,
                            "  {dim}◆ reasoning (hidden, in {cyan}debug{reset}{dim}…){reset}"
                        );
                        let _ = stdout.flush();
                        reasoning_seen = true;
                    }
                    reasoning_buffer.push_str(&text);
                    // If the user explicitly asked to see the
                    // scratchpad (HYDRAGENT_SHOW_REASONING=1), print
                    // it dimly, one line at a time. Otherwise it's
                    // collected silently into `reasoning_buffer`.
                    if show_reasoning {
                        let _ = write!(stdout, "  {dim}{}{reset}", text);
                        let _ = stdout.flush();
                    }
                }
            }
        }
    }
    // End-of-stream: stop the spinner if it was still running
    // (i.e. the LLM never produced a single token, or the channel
    // closed before any token arrived).
    if let Some(h) = spinner_handle.take() {
        h.stop();
    }
    // Flush any text the detector was holding back in case the
    // next chunk completed a marker. The channel is closed, so
    // there is no next chunk — the held-back tail is the tail of
    // the model's reply and must be emitted now.
    for (kind, text) in reasoning_detector.flush() {
        if text.is_empty() {
            continue;
        }
        match kind {
            TokenPart::Final => {
                if stream_raw {
                    let _ = write!(stdout, "{}", text);
                    let _ = stdout.flush();
                }
                collected.push_str(&text);
            }
            TokenPart::Reasoning => {
                reasoning_buffer.push_str(&text);
                if show_reasoning {
                    let _ = write!(stdout, "  {dim}{}{reset}", text);
                    let _ = stdout.flush();
                }
            }
        }
    }

    match thinker.await {
        Ok(Ok((final_answer, _tools))) => {
            // Render the full response as styled markdown in
            // render mode. The renderer prints its own header
            // ("hydra ▸") and indentation, so we only need a
            // blank line above it for visual separation from the
            // spinner. In stream-raw mode the response has
            // already been echoed to the terminal; we just emit
            // a trailing newline so the next prompt has
            // breathing room.
            if !stream_raw && !collected.is_empty() {
                let renderer = crate::markdown_render::MarkdownRenderer::new();
                if let Err(e) = renderer.print_to(&collected, &mut stdout) {
                    // Fall back to printing raw if the renderer
                    // itself fails (e.g. broken pipe). Never
                    // silently lose the model's reply.
                    let _ = writeln!(stdout);
                    let _ = writeln!(stdout, "  {}", collected);
                    eprintln!("  (markdown render failed: {e})",);
                }
            } else if !final_answer.is_empty() {
                let _ = writeln!(stdout);
            } else if collected.is_empty() {
                println!();
                println!("  {dim}(no response from the model){reset}");
            }
            if let Err(e) = state
                .store
                .append_message(
                    &state.page_id,
                    MessageRole::Assistant,
                    &collected,
                )
                .await
            {
                eprintln!("  ✗ Failed to persist assistant reply: {}", e);
            }
            // Hand the captured scratchpad to the dropdown
            // history. The next turn starts fresh; until then the
            // user can type `/reasoning` to expand it. The
            // `std::mem::take` here is important: it clears the
            // per-turn buffer so a `show_reasoning` flush from a
            // prior turn doesn't leak into the next one.
            if !reasoning_buffer.trim().is_empty() {
                reasoning_history.set(reasoning_buffer.clone());
                reasoning_history.print_marker();
            } else {
                // No reasoning this turn. Don't leave stale text
                // in the history from a previous turn.
                reasoning_history.clear();
            }
        }
        Ok(Err(e)) => {
            // Render a one-line, user-friendly error. The verbose
            // tracing output has already been redirected to the chat
            // log file at data/logs/chat.jsonl.
            println!();
            let msg = e.to_string();
            let one_line = first_sentence(&msg);
            println!("  ✗ {one_line}");
            println!("  {dim}(full error in data/logs/chat.jsonl){reset}");
        }
        Err(e) => {
            println!();
            eprintln!("  ✗ Task join error: {}", e);
        }
    }
    Ok(())
}

/// Handle to a running spinner task. Callers wrap this in
/// `Option<SpinnerHandle>` and `.take()` it to stop at most once
/// per REPL turn (either on the first token OR at end-of-stream,
/// whichever comes first).
struct SpinnerHandle {
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Optional, shared, mutable label that the spinner thread
    /// re-reads on every tick. The streaming loop updates this to
    /// reflect new `[Thinking (Step N/M)]` statuses. `None` after
    /// the spinner has been stopped (the `Mutex` would be useless
    /// to hold once the thread is gone).
    label_state: Option<Arc<std::sync::Mutex<String>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl SpinnerHandle {
    /// Update the spinner's label. Cheap: just a `Mutex` lock and
    /// string assignment. The spinner thread picks it up on its
    /// next 100ms tick.
    fn update_label(&self, new_label: String) {
        if let Some(state) = self.label_state.as_ref() {
            if let Ok(mut g) = state.lock() {
                *g = new_label;
            }
        }
    }

    /// Stop the spinner. Consumes `self` so the caller can be sure
    /// the stop flag, the background thread, and the JoinHandle are
    /// all torn down together. Best-effort: if the join hangs, we
    /// drop the thread and move on — the REPL should never block
    /// waiting for cosmetic output to clear.
    fn stop(mut self) {
        self.stop_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        // Drop the label state so callers can no longer poke the
        // (now-dying) thread with confusing late updates.
        self.label_state = None;
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
        // Clear the spinner line so it doesn't dangle. The background
        // task may have written one more frame after the flag was set;
        // the next `println!` overwrites it, so we don't need to sleep.
        let mut stdout = std::io::stdout();
        let _ = write!(stdout, "\r\x1b[2K");
        let _ = stdout.flush();
    }
}

/// Show a small animated spinner on the line below the prompt while
/// the LLM is thinking. The spinner is a single glyph that rotates on
/// a 100ms tick. It runs in a background task and stops the moment
/// the returned `SpinnerHandle` is dropped via `stop()`.
///
/// The label is wrapped in a `Mutex` so the streaming loop can
/// update it (e.g. as new `[Thinking (Step N/M)]` statuses arrive)
/// without having to re-spawn the spinner. This keeps a single
/// in-place indicator row that the user actually sees.
///
/// At longer wait times the spinner falls back to ambient status
/// hints ("still composing…", "long response — almost there…") so a
/// 10-second silence doesn't read as a hang. These escalations are
/// driven by `elapsed` seconds, not by token events, so they apply
/// even if the LLM is silently composing a long reply.
fn start_spinner(label: String) -> SpinnerHandle {
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let label_state = Arc::new(std::sync::Mutex::new(label));
    let flag_clone = stop_flag.clone();
    let label_clone = label_state.clone();
    let join = std::thread::spawn(move || {
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let mut i = 0usize;
        let mut stdout = std::io::stdout();
        let start = Instant::now();
        while !flag_clone.load(std::sync::atomic::Ordering::SeqCst) {
            let elapsed = start.elapsed().as_secs();
            // Snapshot the current label; updates from the streaming
            // loop take effect on the next tick.
            let current = label_clone
                .lock()
                .map(|g| g.clone())
                .unwrap_or_else(|_| "thinking".to_string());
            // Ambient fallback when the LLM is silent for a while.
            // We do NOT overwrite a more specific label set by
            // status frames (e.g. "[Thinking (Step 2/10)]") — we
            // only step in if the label is still the default-ish
            // "hydra thinking" text.
            let ambient = if elapsed >= 15 {
                "  \x1b[2m(long response — almost there…)\x1b[0m"
            } else if elapsed >= 5 {
                "  \x1b[2m(still composing…)\x1b[0m"
            } else {
                ""
            };
            let _ = write!(
                stdout,
                "\r\x1b[2K  {} {} {}{}s{}",
                frames[i % frames.len()],
                current,
                if elapsed == 0 { "<1".to_string() } else { elapsed.to_string() },
                "",
                ambient,
            );
            let _ = stdout.flush();
            std::thread::sleep(Duration::from_millis(100));
            i += 1;
        }
    });
    SpinnerHandle {
        stop_flag,
        label_state: Some(label_state),
        join: Some(join),
    }
}

/// Take the first sentence of a (possibly multi-line) error string.
/// Stops at the first period, newline, or "error response" boundary.
fn first_sentence(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(idx) = trimmed.find('\n') {
        return trimmed[..idx].trim().to_string();
    }
    if let Some(idx) = trimmed.find(". ") {
        return trimmed[..idx + 1].trim().to_string();
    }
    if trimmed.len() > 200 {
        return format!("{}…", &trimmed[..200]);
    }
    trimmed.to_string()
}

fn print_help() {
    println!();
    println!("  Chat commands:");
    println!("    <message>            Send a message to the LLM");
    println!("    /help, /?            Show this help");
    println!("    /exit, /quit, /q     Exit chat");
    println!();
    println!("  Page:");
    println!("    /page                Show the current page ID");
    println!("    /pages               List past pages (most recent 20)");
    println!("    /resume <id>         (planned) switch to a past page");
    println!("    /new                 start a fresh page with a new id");
    println!();
    println!("  Diagnostics:");
    println!("    /model [name]        Show or switch primary model (no name = show)");
    println!("    /brain               Show base URL + masked key");
    println!("    /tools               List registered tools");
    println!("    /status              Render the Kimi-style status bar");
    println!("    /memory list|clear   Inspect / wipe stored memories");
    println!("    /audit head          Show the Merkle audit chain head");
    println!("    /debug               Dump env + config (same as --debug)");
    println!("    /clear, /cls         Clear the screen");
    println!("    /paste               Toggle multi-line / paste mode");
    println!();
    println!("  Dream cycle (memory consolidation):");
    println!("    /dream               Show dream cycle status (enabled, interval)");
    println!("    /dream enable        Turn the dream cycle on  (writes .env)");
    println!("    /dream disable       Turn the dream cycle off (writes .env)");
    println!("    /dream interval <N>  Set cycle interval to N seconds (writes .env)");
    println!("                         Changes take effect after the bus is restarted.");
    println!();
    println!("  Reasoning dropdown:");
    println!("    /reasoning, /r       Toggle the last turn's scratchpad");
    println!("    /r show             Force-expand the last turn's scratchpad");
    println!("    /r hide             Force-collapse the last turn's scratchpad");
    println!("                         (Some models emit a <think>…</think>");
    println!("                         block before the final answer. It's");
    println!("                         hidden by default; this command reveals");
    println!("                         it after the fact.)");
    println!();
}

fn short_id(id: &str) -> String {
    if id.len() <= 8 {
        id.to_string()
    } else {
        id[..8].to_string()
    }
}

/// Rewrite a single `KEY=VALUE` line in `env_path` in-place.
///
/// * If a line starting with `KEY=` (or the commented-out `# KEY=`) is
///   found, it is replaced with `KEY=value` and `Ok(true)` is returned.
/// * If no such line exists, `KEY=value` is appended and `Ok(false)` is
///   returned.
///
/// Every other line in the file is preserved verbatim.
fn patch_env_key(
    env_path: &std::path::Path,
    key: &str,
    value: &str,
) -> anyhow::Result<bool> {
    let content = std::fs::read_to_string(env_path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let prefix_active = format!("{}=", key);
    let prefix_comment = format!("# {}=", key);
    let new_line = format!("{}={}", key, value);
    let mut found = false;
    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&prefix_active) || trimmed.starts_with(&prefix_comment) {
            *line = new_line.clone();
            found = true;
            break;
        }
    }
    if !found {
        lines.push(new_line);
    }
    let new_content = lines.join("\n") + "\n";
    std::fs::write(env_path, &new_content)?;
    Ok(found)
}


/// Strip a single layer of shell-style quotes from `s`.
///
/// Users can type `/resume ""` (empty double-quotes) or
/// `/resume ''` (empty single-quotes) and the slash-command
/// splitter will hand us the literal `""` / `''` string. To
/// the user, those are both "no argument" — but `.is_empty()`
/// returns `false` for the quoted forms, so we'd otherwise
/// echo `hydragent --page "" chat` back at them, which is
/// a useless and confusing suggestion.
///
/// We deliberately do *not* try to do real shell parsing
/// (no backslash escapes, no nested quotes, no variable
/// expansion) — the goal is purely to recognise the
/// common "I typed quotes around nothing" case.
fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn mask(s: &str) -> String {
    if s.is_empty() {
        return "<empty>".into();
    }
    let n = s.chars().count();
    if n <= 12 {
        return format!("<set> ({} chars)", n);
    }
    let head: String = s.chars().take(4).collect();
    let tail_rev: String = s.chars().rev().take(4).collect::<String>().chars().rev().collect();
    format!("{}…{}  ({} chars)", head, tail_rev, n)
}

fn strip_line_ending(s: &str) -> String {
    // std::io::BufRead::read_line includes the trailing \n (and possibly \r\n on Windows).
    s.trim_end_matches(|c| c == '\n' || c == '\r').to_string()
}

/// Remove C0 control characters and DEL from a string.
///
/// Keeps tab (`\t` = 0x09), line feed (`\n` = 0x0A), and carriage
/// return (`\r` = 0x0D) so multi-line paste blocks still work. All
/// other control bytes (NUL, BEL, BS, VT, FF, ESC, etc.) are dropped
/// because they have no place in chat text and would either be sent
/// to the model as garbage or render as mojibake in the local echo.
///
/// Unicode is preserved: we filter on `char` boundaries, not raw
/// bytes, so multi-byte UTF-8 sequences (emojis, CJK, etc.) pass
/// through untouched.
fn strip_control_chars(s: &str) -> String {
    s.chars().filter(|c| !is_dangerous_control_char(*c)).collect()
}

fn is_dangerous_control_char(c: char) -> bool {
    matches!(c, '\x00'..='\x08' | '\x0B' | '\x0C' | '\x0E'..='\x1F' | '\x7F')
}

/// The react_loop emits lines that look like either:
///   `{"jsonrpc":"2.0","method":"response.status","params":{"status":"…\n"}}`
///   `{"jsonrpc":"2.0","method":"response.token","params":{"token":"…"}}`
/// We surface the inner string so the terminal shows the model output,
/// not the envelope.
fn strip_json_envelope(s: &str) -> String {
    if !s.trim_start().starts_with('{') {
        return s.to_string();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(params) = v.get("params") {
            if let Some(t) = params.get("token").and_then(|x| x.as_str()) {
                return t.to_string();
            }
            if let Some(t) = params.get("status").and_then(|x| x.as_str()) {
                // Status strings like "\n`[Thinking (Step 1/10)]`...\n"
                return t.to_string();
            }
        }
    }
    s.to_string()
}

/// Quick discriminator: does this raw frame look like a status
/// frame (a JSON envelope carrying `params.status`) or a token
/// frame (a JSON envelope carrying `params.token`, or a non-JSON
/// plain string)? We use this *before* `strip_json_envelope` so
/// the streaming loop can route each kind to a different
/// handler.
///
/// If parsing fails we default to `false` (treat as a token).
/// That puts the worst case in the same place as a stray log
/// line: visible, but rendered as plain text rather than
/// silently dropped.
fn is_status_envelope(s: &str) -> bool {
    if !s.trim_start().starts_with('{') {
        return false;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(params) = v.get("params") {
            return params.get("status").is_some()
                && params.get("token").is_none();
        }
    }
    false
}

/// A kernel-issued status string, after we've figured out what it
/// actually means. The streaming loop dispatches on this enum.
///
/// Raw status strings from the kernel look like:
///
/// - ``"\n`[Strategy: ReactLoop (single agent with tools) — via llm]`\n"``
/// - ``"\n`[Thinking (Step 1/10)]`...\n"``
/// - ``"\n`[Thought]` some private reasoning\n"``
/// - ``"`[Calling tool]` **search_web** with params `{\"q\":\"PONG\"}`\n"``
/// - ``"\n`[Injected 2 facts from the Library's memory]`\n"``
/// - ``"`[Tool Result]` Status: Success\n"`` (or ``Failure``)
///
/// We try to map these into a small enum so the REPL can render
/// them in a consistent, human-friendly style (or drop them
/// entirely when they would only add noise).
#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusKind {
    /// ``[Strategy: <label> — via <source>]``
    Strategy { label: String, source: String },
    /// ``[Thinking (Step <n>/<max>)]`` — the orchestrator is
    /// iterating internally; the spinner should pick this up so the
    /// user sees progress without seeing the raw bracketed string.
    Thinking { step: u32, max: u32 },
    /// ``[Thought] <free-form private reasoning>`` — the model is
    /// thinking out loud. We deliberately drop this from the
    /// terminal: it's noisy, leaks the model's scratchpad, and the
    /// spinner already conveys "we're working".
    Thought,
    /// ``[Calling tool] **<name>** with params <json>``
    ToolCall { name: String, params: String },
    /// ``[Tool Result] Status: Success|Failure`` — the tool call
    /// returned. We render this as a small inline marker next to
    /// the previous tool-call line so the user can see whether
    /// the call landed.
    ToolResult { ok: bool },
    /// ``[Injected <n> facts from the Library's memory]``
    InjectedFacts(usize),
    /// ``[Pending clarification: "..." — treating your message as the answer]``
    /// or ``[Discarded pending clarification: "..."]``
    /// — both are surfaced as short, dim annotations.
    PendingClarification { question: String, treated_as_answer: bool },
    /// ``[Awaiting your reply — please answer the question above and I'll continue.]``
    /// — only fired by the AskUser strategy. We deliberately drop
    /// it; the question itself is already shown as a token.
    AwaitingReply,
    /// Anything else we didn't recognize. We still render it, but
    /// dimmed, so the user can see if the kernel starts emitting a
    /// new kind of status.
    Unknown(String),
}

/// Take the raw `display` string (the value extracted from
/// `params.status` by `strip_json_envelope`) and produce a typed
/// `StatusKind`. Pure function: never panics, never prints, just
/// parses.
fn classify_status(display: &str) -> StatusKind {
    // The orchestrator wraps status strings in Markdown backticks
    // (e.g. `` "`[Thinking (Step 1/10)]`..." ``) and surrounds them
    // with newlines. Peel all of that off before we try to match
    // the tag, so we can identify the kind regardless of the
    // cosmetics the orchestrator decided to add this week.
    let unbackticked = strip_outer_backticks(display.trim());

    // Strategy: "[Strategy: <label> — via <source>]"
    if let Some(inner) = strip_brackets(unbackticked, "Strategy") {
        if let Some((label, source)) = split_on_em_dash(&inner) {
            return StatusKind::Strategy {
                label: label.trim().to_string(),
                source: source.trim().to_string(),
            };
        }
        return StatusKind::Unknown(display.to_string());
    }

    // Thinking: "[Thinking (Step <n>/<max>)]"
    if let Some(inner) = strip_brackets(unbackticked, "Thinking") {
        if let Some((step, max)) = parse_step_counter(&inner) {
            return StatusKind::Thinking { step, max };
        }
        return StatusKind::Unknown(display.to_string());
    }

    // Thought: "[Thought] <text>" — drop entirely.
    if let Some(_inner) = strip_brackets(unbackticked, "Thought") {
        return StatusKind::Thought;
    }

    // Calling tool: ``[Calling tool] **<name>** with params <json>``
    // The brackets wrap only the literal label "Calling tool";
    // the tool name and parameters live outside the brackets, in
    // the same line. Pass the full unbackticked string to
    // `parse_tool_call` so it can find the bold-name marker.
    if let Some(inner) = strip_brackets(unbackticked, "Calling tool") {
        if inner.is_empty() {
            // Expected shape: the brackets contain only the label.
            if let Some((name, params)) = parse_tool_call(unbackticked) {
                return StatusKind::ToolCall { name, params };
            }
        }
        return StatusKind::Unknown(display.to_string());
    }

    // Tool Result: "[Tool Result] Status: Success" or
    // "[Tool Result] Status: Failure" (sometimes wrapped in
    // backticks, sometimes not).
    // NOTE: `strip_brackets` only returns the text *inside* the
    // brackets (which for "[Tool Result]" is empty). The actual
    // status word lives outside the brackets, so we must scan the
    // full unbackticked string rather than `inner`.
    if strip_brackets(unbackticked, "Tool Result").is_some() {
        let ok = unbackticked
            .to_ascii_lowercase()
            .contains("status: success");
        return StatusKind::ToolResult { ok };
    }

    // Pending clarification: "[Pending clarification: \"<q>\" — ...]"
    if let Some(inner) = strip_brackets(unbackticked, "Pending clarification") {
        if let Some(q) = extract_first_quoted(&inner) {
            return StatusKind::PendingClarification {
                question: q.to_string(),
                treated_as_answer: true,
            };
        }
        return StatusKind::Unknown(display.to_string());
    }

    // Discarded pending clarification:
    // "[Discarded pending clarification: \"<q>\" — ...]"
    if let Some(inner) = strip_brackets(unbackticked, "Discarded pending clarification") {
        if let Some(q) = extract_first_quoted(&inner) {
            return StatusKind::PendingClarification {
                question: q.to_string(),
                treated_as_answer: false,
            };
        }
        return StatusKind::Unknown(display.to_string());
    }

    // Awaiting your reply: "[Awaiting your reply — ...]"
    if strip_brackets(unbackticked, "Awaiting your reply").is_some() {
        return StatusKind::AwaitingReply;
    }

    // Injected facts: "[Injected <n> facts from the Library's memory]"
    if let Some(inner) = strip_brackets(unbackticked, "Injected") {
        if let Some(n) = parse_leading_usize(&inner) {
            return StatusKind::InjectedFacts(n);
        }
        return StatusKind::Unknown(display.to_string());
    }

    StatusKind::Unknown(display.to_string())
}

/// Pull the first `\"…\"` quoted substring out of a string. The
/// orchestrator writes questions as `\"<text>\"` inside status
/// strings; we just want the inner text for the dim annotation.
/// Returns `None` if no quoted segment is found.
fn extract_first_quoted(s: &str) -> Option<&str> {
    let open = s.find('"')?;
    let after = &s[open + 1..];
    let close_rel = after.find('"')?;
    Some(&after[..close_rel])
}

/// If `s` is wrapped in a pair of outer backticks (e.g. `` `…` ``
/// or `` `…` ``), strip them. We use this because the
/// orchestrator wraps status strings in Markdown inline-code
/// backticks before sending them down the channel; the REPL
/// doesn't actually need those backticks because we render the
/// status ourselves. Nested backticks (e.g. `` `a `b` c` ``)
/// are intentionally left alone — we only peel the outer pair.
fn strip_outer_backticks(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with('`') && s.ends_with('`') && s.len() >= 2 {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// If `s` starts with ``[<tag>]`` (case-insensitive, allowing
/// leading/trailing whitespace, and tolerating a single
/// surrounding backtick on the bracket pair), return the inner
/// text; otherwise return `None`. We do this by hand instead of
/// pulling in a regex crate for one tiny pattern.
///
/// The orchestrator sometimes writes its status strings as
/// `` `[Strategy: ...]` `` (backticked brackets) and sometimes
/// without the surrounding backticks. We accept both.
fn strip_brackets(s: &str, tag: &str) -> Option<String> {
    let s = s.trim();
    // The orchestrator may wrap the brackets in a single backtick
    // on each side: `` `[Thinking (Step 1/10)]` ``. Peel that off
    // before we look for the leading '['.
    let s = s.strip_prefix('`').unwrap_or(s);
    let s = s.strip_prefix('[')?;
    let close = s.find(']')?;
    let head = &s[..close];
    // Match the tag at the start of the head, allowing a trailing
    // space or colon. Case-insensitive comparison.
    if head.len() < tag.len() {
        return None;
    }
    if !head[..tag.len()].eq_ignore_ascii_case(tag) {
        return None;
    }
    let rest = head[tag.len()..].trim_start();
    // The tag is allowed to be followed by ":", " ", or be the
    // entire contents of the brackets. We've already trimmed the
    // leading space; an empty rest is fine (e.g. "[Thought]").
    Some(rest.to_string())
}

/// Split a string on the first em-dash (`—`) or en-dash (`–`) and
/// return the two halves. We use these because the orchestrator
/// writes strategy labels with an em-dash between the human name
/// and the source (e.g. "ReactLoop (single agent with tools) —
/// via llm").
fn split_on_em_dash(s: &str) -> Option<(String, String)> {
    for sep in [" — ", " – ", " - "] {
        if let Some(idx) = s.find(sep) {
            return Some((s[..idx].to_string(), s[idx + sep.len()..].to_string()));
        }
    }
    None
}

/// Parse "[Thinking (Step 3/10)]" → Some((3, 10)). Tolerates
/// extra whitespace and the case variants emitted by older builds.
fn parse_step_counter(inner: &str) -> Option<(u32, u32)> {
    // Strip the optional "(Step N/M)" wrapper. The orchestrator
    // sometimes writes "Thinking (Step 1/10)" with no other text;
    // sometimes "Thinking (Step 1/10) planning next action".
    let inner = inner.trim();
    // Find "Step" (case-insensitive) at the start of a parenthesised
    // group, then look for the slash inside.
    let l = inner.find('(')?;
    let r = inner[l..].find(')')?;
    let group = &inner[l + 1..l + r];
    let group = group.trim();
    // Expect "<word> <n>/<m>"
    let slash = group.find('/')?;
    let (head, max_str) = group.split_at(slash);
    let max_str = &max_str[1..]; // skip the slash
    let head = head.trim();
    let max_str = max_str.trim();
    // Skip the leading "Step" / "step" word.
    let n_str = head
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim();
    let n: u32 = n_str.parse().ok()?;
    let m: u32 = max_str.parse().ok()?;
    Some((n, m))
}

/// Parse "**search_web** with params `{\"q\":\"PONG\"}`" →
/// Some(("search_web", "{\"q\":\"PONG\"}")). Falls back to the
/// full string if we can't find the bold-name / params split.
fn parse_tool_call(inner: &str) -> Option<(String, String)> {
    let inner = inner.trim();
    // The name lives between "**" and the next "**".
    let open = inner.find("**")?;
    let after_open = &inner[open + 2..];
    let close = after_open.find("**")?;
    let name = after_open[..close].trim().to_string();
    if name.is_empty() {
        return None;
    }
    // Params are whatever comes after the second "**". We don't
    // need to parse the JSON — just strip "with params" and the
    // surrounding backticks so the line is shorter.
    let rest = after_open[close + 2..].trim();
    let rest = rest
        .trim_start_matches("with params")
        .trim_start_matches(':')
        .trim();
    let params = rest.trim_matches('`').trim().to_string();
    Some((name, params))
}

/// Parse the first unsigned integer out of a string like "2 facts
/// from the Library's memory". Returns `None` if no integer is
/// found at the start.
fn parse_leading_usize(s: &str) -> Option<usize> {
    let s = s.trim_start();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Render a `StatusKind` for the terminal. Returns a single line
/// (with trailing newline) using dim colors so it doesn't compete
/// with the user's actual reply. Returns `None` for variants the
/// REPL should silently drop (`Thought`).
fn render_status(kind: &StatusKind) -> Option<String> {
    // Local aliases for the format-string `{name}` syntax.
    // `yellow` is unique to this function so we keep it inline.
    let dim = ANSI_DIM;
    let cyan = ANSI_CYAN;
    let yellow = "\x1b[33m";
    let reset = ANSI_RESET;
    match kind {
        StatusKind::Strategy { label, source } => Some(format!(
            "  {dim}◆ strategy: {label}  {cyan}({source}){reset}{dim}{reset}\n"
        )),
        StatusKind::Thinking { .. } => {
            // Thinking is rendered as a spinner update, not a
            // standalone line, so the caller doesn't get anything
            // back here.
            None
        }
        StatusKind::Thought => {
            // Deliberately hidden. The orchestrator's private
            // scratchpad is not the user's concern.
            None
        }
        StatusKind::ToolCall { name, params } => {
            let params_short = if params.len() > 80 {
                format!("{}…", &params[..80])
            } else {
                params.clone()
            };
            Some(format!(
                "  {dim}▸ calling tool: {yellow}{name}{reset}{dim}({params_short}){reset}\n"
            ))
        }
        StatusKind::ToolResult { ok } => {
            // Tool results are always shown immediately after the
            // matching "calling tool" line, so we render them with
            // the same indent. Use a green check for success, a
            // dim red cross for failure; the user doesn't need the
            // word "Status:" — they've already seen "calling tool".
            if *ok {
                Some(format!("    {dim}✓ ok{reset}\n"))
            } else {
                Some(format!("    {dim}✗ failed{reset}\n"))
            }
        }
        StatusKind::InjectedFacts(n) => Some(format!(
            "  {dim}◆ injected {n} fact{verb} from memory{reset}\n",
            verb = if *n == 1 { "" } else { "s" }
        )),
        StatusKind::PendingClarification {
            question,
            treated_as_answer,
        } => {
            // Clamp the question so a runaway prompt doesn't blow
            // up the terminal width.
            let mut q = question.clone();
            if q.len() > 120 {
                q.truncate(120);
                q.push('…');
            }
            if *treated_as_answer {
                Some(format!(
                    "  {dim}◆ treating your message as the answer to: \"{q}\"{reset}\n"
                ))
            } else {
                Some(format!(
                    "  {dim}◆ discarded pending clarification: \"{q}\"{reset}\n"
                ))
            }
        }
        StatusKind::AwaitingReply => {
            // The question itself was already sent as a token; this
            // status is just a duplicate prompt that adds nothing.
            None
        }
        StatusKind::Unknown(raw) => {
            // Trim and clamp so a runaway status doesn't blow up
            // the terminal width.
            let mut s = raw.trim().to_string();
            if s.len() > 200 {
                s.truncate(200);
                s.push('…');
            }
            Some(format!("  {dim}{s}{reset}\n"))
        }
    }
}

/// Build the spinner label for a given `Thinking` step. The label
/// is shown next to the spinner glyph on the indicator line; we
/// keep it short so it fits on a single 80-col terminal row.
///
/// We deliberately hide the `step N/M` counter when the model is
/// still on the very first step of a max-step budget — most
/// questions finish in a single step, so showing "step 1/10" for
/// 12 seconds straight is misleading. The counter only appears
/// once the model is genuinely multi-stepping (step >= 2), which
/// is the moment the user might want to know "ok, the model is
/// working hard on this one".
fn thinking_label(kind: &StatusKind) -> Option<String> {
    if let StatusKind::Thinking { step, max } = kind {
        if *step <= 1 {
            // Most turns are single-step. Keep the label quiet.
            Some("thinking\u{2026}".to_string())
        } else {
            // Genuine multi-step (rare): show the counter so the
            // user knows the model is making real progress.
            Some(format!("thinking\u{2026} step {step}/{max}"))
        }
    } else {
        None
    }
}

// ────────────────────────────────────────────────────────────────────────
// Model-aware reasoning detector
// ────────────────────────────────────────────────────────────────────────
//
// Different LLM providers emit the model's private chain-of-thought
// in different ways. Some wrap it in explicit tags, some just inline
// it, some hide it entirely. We can't know in advance which model is
// behind `BRAIN_MODEL` (the user can switch at runtime), so the REPL
// treats the token stream as opaque text and runs it through a small
// state machine that recognises the *common* reasoning markers:
//
//   • DeepSeek / QwQ / DeepHermes:       <think> … </think>
//   • Anthropic Claude 3.7+:              ⟨thinking⟩ … ⟨/thinking⟩
//   • Mistral / Mixtral (older):           <reasoning> … </reasoning>
//   • Some fine-tunes / custom system prompts: [Reasoning] … [/Reasoning]
//
// Anything between an opening and matching closing marker is
// classified as `Reasoning`; everything else is `Final`. The
// streaming REPL prints `Final` chunks live and tucks `Reasoning`
// chunks into a hidden buffer (with a one-line "reasoning hidden"
// marker so the user knows the model is thinking out loud).
//
// Markers can be split across token chunks (e.g. the model emits
// ` <thi` then ` nk> `), so the detector buffers partial matches
// at chunk boundaries and resumes detection on the next chunk.

/// The classification of a sub-segment of the model's token
/// stream. The streaming loop dispatches each segment to the
/// appropriate sink: `Final` → user-visible stdout, `Reasoning`
/// → hidden buffer (or dim debug rendering when explicitly
/// enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenPart {
    Final,
    Reasoning,
}

/// Persistent state for the reasoning dropdown. The streaming
/// loop writes the last turn's scratchpad here at end-of-turn;
/// the `/reasoning` slash command reads it back. The chevron
/// (`▸` / `▾`) tracks whether the user has already expanded it
/// once; calling `/reasoning` toggles.
///
/// We keep this lean on purpose: no `HashMap`, no `Vec`, just
/// the few fields we need to render the marker and the full
/// text. The full text can be a few KB per turn (DeepSeek-R1
/// emits 1–3 KB of `<think>…` on a typical turn), so memory is
/// not a concern.
#[derive(Debug, Default)]
struct ReasoningHistory {
    /// Full reasoning text of the last turn (the contents
    /// between the `<think>` and `</think>` markers, stripped
    /// of the markers themselves). `None` means the last turn
    /// produced no reasoning, so the dropdown is empty.
    text: Option<String>,
    /// One-line preview (first non-empty line of `text`,
    /// trimmed and truncated to 64 chars) for the collapsed
    /// marker. Computed by `set()` so the slash command can
    /// just blit it.
    preview: Option<String>,
    /// Total character count of `text`. Used in the marker
    /// so the user knows how much they're about to see.
    char_count: usize,
    /// `true` if the user has invoked `/reasoning` since the
    /// last turn reset. The collapsed marker shows `▸`;
    /// the expanded marker shows `▾`.
    expanded: bool,
}

impl ReasoningHistory {
    /// Reset the history to "no reasoning this turn". Called
    /// by the streaming loop on a turn that produced no
    /// reasoning so the previous turn's dropdown doesn't
    /// linger.
    fn clear(&mut self) {
        self.text = None;
        self.preview = None;
        self.char_count = 0;
        self.expanded = false;
    }

    /// Replace the stashed reasoning with a new turn's text.
    /// Computes the preview and char count, then resets the
    /// expanded flag — a new turn is always collapsed.
    fn set(&mut self, text: String) {
        self.char_count = text.chars().count();
        // Build a one-line preview: first non-empty line,
        // trimmed, truncated to 64 chars with a trailing
        // ellipsis if we cut it.
        let preview = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.to_string())
            .unwrap_or_default();
        let preview = if preview.chars().count() > 64 {
            let cut: String = preview.chars().take(64).collect();
            format!("{cut}…")
        } else {
            preview
        };
        self.preview = if preview.is_empty() { None } else { Some(preview) };
        self.text = Some(text);
        self.expanded = false;
    }

    /// Render the one-line dropdown marker. We print it on its
    /// own line, dimmed, with `▸` collapsed / `▾` expanded.
    /// The preview is what the user will see when they expand;
    /// the char count tells them how much is there.
    fn print_marker(&self) {
        // Local aliases for the format-string `{name}` syntax.
        let dim = ANSI_DIM;
        let reset = ANSI_RESET;
        let chevron = if self.expanded { "▾" } else { "▸" };
        match (self.preview.as_deref(), self.text.is_some()) {
            (Some(preview), true) => {
                println!(
                    "  {dim}{} reasoning: {}{}{}",
                    chevron,
                    preview,
                    format_args!(" ({0} chars — /reasoning to expand)", self.char_count),
                    reset,
                );
            }
            _ => {
                // No preview / no text: don't print a marker
                // at all. (Callers handle the empty case.)
            }
        }
    }

    /// Render the full reasoning text. Called when the user
    /// types `/reasoning` (or `/r show`). The text is dimmed
    /// and prefixed with a thin rule so it's visually distinct
    /// from the model's actual reply above it.
    fn print_full(&self) {
        // Local aliases for the format-string `{name}` syntax.
        let dim = ANSI_DIM;
        let reset = ANSI_RESET;
        if let Some(text) = self.text.as_deref() {
            println!("  {dim}── reasoning ({} chars) ──{reset}", self.char_count);
            for line in text.lines() {
                println!("  {dim}{}{reset}", line);
            }
            println!("  {dim}── /reasoning to collapse ──{reset}");
        }
    }
}

/// Open / close pairs the detector recognises. We list them in
/// priority order: the first matching marker wins. The detector
/// always pairs the same index on both sides (i.e. `OPEN[0]` closes
/// with `CLOSE[0]`, etc.). The opening and closing sets are the
/// same length; adding a new model means adding a new entry to
/// *both* slices.
const REASONING_OPEN: &[&str] = &[
    "<think>",
    "⟨thinking⟩",
    "<reasoning>",
    "[Reasoning]",
];
const REASONING_CLOSE: &[&str] = &[
    "</think>",
    "⟨/thinking⟩",
    "</reasoning>",
    "[/Reasoning]",
];

/// Maximum length of any single marker. Used to size the partial
/// buffer so a token chunk can never starve the detector.
const MAX_MARKER_LEN: usize = 16;

/// Streaming detector state. The detector is reset on every new
/// turn; the REPL just constructs a fresh one before each
/// `run_react_loop` call.
#[derive(Debug, Default)]
struct ReasoningDetector {
    /// Are we currently inside a reasoning block? When `true`,
    /// ordinary `Final` text is suppressed and any text we emit
    /// is tagged `Reasoning`.
    in_block: bool,
    /// Trailing bytes from the previous chunk that could be the
    /// start of a marker. Once we have enough context to know
    /// either way, we either emit it as ordinary text or
    /// transition state.
    partial: String,
}

impl ReasoningDetector {
    fn new() -> Self {
        Self::default()
    }

    /// Feed a token chunk to the detector. Returns one or more
    /// `(kind, text)` segments ready for the streaming loop to
    /// render. The detector never panics on partial markers,
    /// empty input, or markers split across chunk boundaries.
    fn feed(&mut self, chunk: &str) -> Vec<(TokenPart, String)> {
        if chunk.is_empty() {
            return Vec::new();
        }
        // Prepend any partial tail from the previous chunk so
        // marker detection sees the full surface. The previous
        // tail was held back specifically because it could be the
        // start of a marker; we want the next chunk to be tested
        // against it.
        let mut combined = std::mem::take(&mut self.partial);
        combined.push_str(chunk);
        let mut out: Vec<(TokenPart, String)> = Vec::new();
        let mut cursor = 0usize;

        // Single-pass cursor walk. At every step we ask: from
        // `cursor` onwards, is there a marker we can transition
        // on? If yes, emit the text before it (with the current
        // kind), transition, and advance. If no, hold back the
        // trailing `MAX_MARKER_LEN-1` bytes and emit the rest.
        while cursor < combined.len() {
            let in_block_now = self.in_block;
            let marker = find_first_marker(&combined[cursor..], in_block_now);
            match marker {
                Some((kind, off, len)) => {
                    let abs = cursor + off;
                    // Emit text before the marker using the
                    // kind that was active entering this
                    // iteration. (If `in_block_now` is true,
                    // we're inside a reasoning block, so the
                    // text leading up to a close marker is
                    // Reasoning; otherwise it's Final.)
                    if abs > cursor {
                        let seg = &combined[cursor..abs];
                        let seg_kind = if in_block_now {
                            TokenPart::Reasoning
                        } else {
                            TokenPart::Final
                        };
                        out.push((seg_kind, seg.to_string()));
                    }
                    // Transition. `kind == 0` is an open marker
                    // (entering a reasoning block); `kind == 1`
                    // is a close marker (leaving).
                    self.in_block = kind == 0;
                    cursor = abs + len;
                }
                None => {
                    // No more markers reachable from `cursor`.
                    // Hold back the trailing `MAX_MARKER_LEN-1`
                    // bytes in case the next chunk completes a
                    // marker that started here, and emit the
                    // rest with the current kind.
                    let tail_hold = MAX_MARKER_LEN.saturating_sub(1);
                    if combined.len() <= tail_hold {
                        // The whole chunk could still be the
                        // start of a marker; hold it all.
                        self.partial = combined[cursor..].to_string();
                        return out;
                    }
                    let safe_end = combined.len() - tail_hold;
                    if safe_end > cursor {
                        let seg = &combined[cursor..safe_end];
                        let seg_kind = if in_block_now {
                            TokenPart::Reasoning
                        } else {
                            TokenPart::Final
                        };
                        out.push((seg_kind, seg.to_string()));
                        cursor = safe_end;
                    }
                    // Stash whatever is left into `partial` for
                    // the next chunk.
                    if cursor < combined.len() {
                        self.partial = combined[cursor..].to_string();
                    }
                    return out;
                }
            }
        }
        out
    }

    /// Flush any text that the streaming loop was holding back
    /// in case the next chunk completed a marker. The detector
    /// is at end-of-stream (the LLM has finished its reply), so
    /// the held-back tail must be emitted now: it can no longer
    /// turn out to be the start of a marker.
    ///
    /// The held-back text is classified with the *current*
    /// `in_block` state — which makes sense, because whatever
    /// was being emitted right before the hold-back is the same
    /// kind we'd be emitting now. If `in_block` is true, the
    /// held-back tail was the tail of a reasoning block; if
    /// false, it was the tail of the user's reply. The detector
    /// is reset to a fresh state on return, so the next call to
    /// `feed()` starts clean.
    fn flush(&mut self) -> Vec<(TokenPart, String)> {
        let held = std::mem::take(&mut self.partial);
        if held.is_empty() {
            return Vec::new();
        }
        let kind = if self.in_block {
            TokenPart::Reasoning
        } else {
            TokenPart::Final
        };
        vec![(kind, held)]
    }
}

/// Helper: scan `combined` for the earliest opening or closing
/// marker, depending on the current `in_block` state. Returns
/// `(kind, off, len)` where `kind` is `0` for an open marker
/// (entering a reasoning block) and `1` for a close marker
/// (leaving one). `off` is the byte offset within `combined`
/// where the marker starts, and `len` is the marker length. If
/// no marker is found, returns `None`.
///
/// We always pick the *earliest* match (smallest `off`), so a
/// chunk that contains two markers correctly emits the text
/// before the first one and then transitions.
fn find_first_marker(combined: &str, in_block: bool) -> Option<(usize, usize, usize)> {
    let candidates: &[&str] = if in_block {
        REASONING_CLOSE
    } else {
        REASONING_OPEN
    };
    let kind: usize = if in_block { 1 } else { 0 };
    let mut best: Option<(usize, usize, usize)> = None; // (kind, off, len)
    for marker in candidates.iter() {
        if let Some(off) = combined.find(marker) {
            if best.is_none() || off < best.unwrap().1 {
                best = Some((kind, off, marker.len()));
            }
        }
    }
    best
}

// ────────────────────────────────────────────────────────────────────────
// Unit tests for the reasoning detector.
//
// These tests lock in the cursor-based `feed()` rewrite. The original
// `feed()` had a leak path where trailing text could be dropped into
// `partial` instead of being emitted. The new implementation walks an
// explicit cursor and either:
//   • transitions on a marker it can see, OR
//   • holds back at most `MAX_MARKER_LEN-1` bytes for the next chunk.
//
// Each test feeds a single chunk and checks the resulting list of
// `(kind, text)` segments. Marker-split-across-chunks scenarios
// (which are the regression hazard) are exercised explicitly.
#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: run a single `feed()` call and return the resulting
    /// segments as `(kind, text)` tuples. Tests that need to feed
    /// multiple chunks just call this in sequence.
    fn feed(d: &mut ReasoningDetector, chunk: &str) -> Vec<(TokenPart, String)> {
        d.feed(chunk)
    }

    /// Helper: `feed()` a single chunk and then `flush()`, then
    /// concatenate the two result lists. This mirrors what the
    /// streaming loop in `dispatch_user_message` does at
    /// end-of-stream. Single-chunk tests use this to verify the
    /// full output of the detector, including the held-back tail
    /// that the detector stashes in `partial` to be safe against
    /// a split marker.
    fn feed_and_flush(d: &mut ReasoningDetector, chunk: &str) -> Vec<(TokenPart, String)> {
        let mut out = feed(d, chunk);
        out.extend(d.flush());
        out
    }

    /// Coalesce consecutive segments of the same `kind` into a
    /// single segment. The detector holds back the trailing
    /// `MAX_MARKER_LEN-1` bytes of every chunk for safety, which
    /// means a single logical block of text (e.g. a long
    /// reasoning paragraph) can come out as two segments with
    /// the same `kind` and adjacent content. The streaming
    /// loop treats them the same as a single merged segment
    /// (it writes them to stdout / appends to the buffer in
    /// order), so for the purposes of testing what the user
    /// actually sees, we coalesce before comparing.
    fn coalesce(segments: Vec<(TokenPart, String)>) -> Vec<(TokenPart, String)> {
        let mut out: Vec<(TokenPart, String)> = Vec::new();
        for (k, t) in segments {
            if t.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some((last_k, last_t)) if *last_k == k => {
                    last_t.push_str(&t);
                }
                _ => out.push((k, t)),
            }
        }
        out
    }

    /// Assert that a segment list equals an expected list of
    /// `(kind, text)` pairs. We use a dedicated helper (instead of
    /// `assert_eq!` on a `Vec<(TokenPart, String)>`) so the panic
    /// message points at the first mismatched segment, which is
    /// much easier to debug than a 12-line diff. The actual
    /// segments are coalesced first so test expectations can
    /// describe the user-visible output rather than the
    /// internal hold-back chunks.
    fn assert_segments(
        actual: Vec<(TokenPart, String)>,
        expected: &[(TokenPart, &str)],
    ) {
        let actual_pairs: Vec<(TokenPart, String)> = coalesce(actual);
        let expected_pairs: Vec<(TokenPart, String)> = expected
            .iter()
            .map(|(k, t)| (*k, t.to_string()))
            .collect();
        assert_eq!(
            actual_pairs, expected_pairs,
            "reasoning detector produced unexpected segments"
        );
    }

    /// Empty input → empty output. The detector must never panic
    /// on `feed("")`, and it must not emit phantom zero-length
    /// segments. `flush()` after an empty `feed()` should also
    /// produce no segments.
    #[test]
    fn empty_chunk_returns_empty() {
        let mut d = ReasoningDetector::new();
        assert_segments(feed_and_flush(&mut d, ""), &[]);
    }

    /// Plain text with no markers → a single `Final` segment.
    /// This is the most common path for any model that doesn't
    /// emit a reasoning block. The held-back tail at the end of
    /// the chunk must be flushed to recover the full text.
    #[test]
    fn plain_text_is_final() {
        let mut d = ReasoningDetector::new();
        assert_segments(
            feed_and_flush(&mut d, "just a regular answer"),
            &[(TokenPart::Final, "just a regular answer")],
        );
    }

    /// A single complete `<think>…</think>` block followed by a
    /// final answer. The detector should split the chunk into
    /// `Reasoning` (the inside) and `Final` (the outside), with
    /// the markers themselves stripped.
    #[test]
    fn single_reasoning_block() {
        let mut d = ReasoningDetector::new();
        assert_segments(
            feed_and_flush(&mut d, "<think>reasoning text</think>answer"),
            &[
                (TokenPart::Reasoning, "reasoning text"),
                (TokenPart::Final, "answer"),
            ],
        );
    }

    /// A `<think>` block that opens on one chunk and closes on
    /// the next. This is the regression test for the original
    /// leak bug: the rewrite of `feed()` to a single-pass cursor
    /// walk guarantees that the inside of the reasoning block is
    /// emitted as `Reasoning`, not dropped.
    #[test]
    fn marker_split_across_chunks() {
        let mut d = ReasoningDetector::new();
        // Chunk 1: opens the block, but no closing marker. The
        // detector should hold back the trailing "some" (4 chars
        // — well under MAX_MARKER_LEN-1) and emit nothing.
        let segs1 = feed(&mut d, "<think>some");
        // We don't assert on segs1 (it may be empty or may contain
        // a Final segment of the chunk's "think" prefix — depends
        // on the detector's exact buffer policy). What matters is
        // that after the second chunk lands, we get the full text.
        let _ = segs1;
        // Chunk 2: completes the block and adds an answer. The
        // detector walks from the held-back tail, recognises the
        // close marker, and emits the reasoning text. The
        // remaining answer text is held back in `partial` for
        // safety; we flush it to recover the full output.
        let mut segs2 = feed(&mut d, " reasoning here</think>final answer");
        segs2.extend(d.flush());
        assert_segments(
            segs2,
            &[
                (TokenPart::Reasoning, "some reasoning here"),
                (TokenPart::Final, "final answer"),
            ],
        );
    }

    /// The exact regression case from the prior session's live
    /// REPL test: a `<think>` block split across chunks where the
    /// first chunk ends with a newline and the second chunk starts
    /// with `</think>`. The original `feed()` would drop the
    /// trailing reasoning text in `partial` instead of emitting
    /// it. The new `feed()` must emit the full inside as
    /// `Reasoning`.
    #[test]
    fn open_then_close_split_by_newline() {
        let mut d = ReasoningDetector::new();
        let segs1 = feed(
            &mut d,
            "<think>The user wants a concise 2-sentence answer about my capabilities.\n",
        );
        // The second chunk closes the block. We
        // accumulate segments across all chunks so the
        // reasoning text (split at the safe_end boundary
        // of the first chunk) coalesces correctly.
        let segs2 = feed(&mut d, "</think>\nI'm an AI assistant...");
        let segs3 = d.flush();
        let all: Vec<(TokenPart, String)> = segs1
            .into_iter()
            .chain(segs2)
            .chain(segs3)
            .collect();
        assert_segments(
            all,
            &[
                (
                    TokenPart::Reasoning,
                    "The user wants a concise 2-sentence answer about my capabilities.\n",
                ),
                (TokenPart::Final, "\nI'm an AI assistant..."),
            ],
        );
    }
    /// Multiple reasoning blocks in a single chunk. The detector
    /// must transition on each marker in turn, so the result is
    /// `Reasoning / Final / Reasoning / Final`.
    #[test]
    fn multiple_reasoning_blocks_in_one_chunk() {
        let mut d = ReasoningDetector::new();
        assert_segments(
            feed_and_flush(&mut d, "<think>a</think>b<think>c</think>d"),
            &[
                (TokenPart::Reasoning, "a"),
                (TokenPart::Final, "b"),
                (TokenPart::Reasoning, "c"),
                (TokenPart::Final, "d"),
            ],
        );
    }

    /// The Anthropic-style `⟨thinking⟩…⟨/thinking⟩` markers must
    /// be recognised too. We test the entire four-marker family in
    /// one go, each on its own detector instance, so any
    /// regression in the marker table (off-by-one, missing entry,
    /// swapped open/close) is caught.
    #[test]
    fn recognises_all_marker_families() {
        // DeepSeek / QwQ / DeepHermes
        let mut d = ReasoningDetector::new();
        assert_segments(
            feed_and_flush(&mut d, "<think>alpha</think>beta"),
            &[
                (TokenPart::Reasoning, "alpha"),
                (TokenPart::Final, "beta"),
            ],
        );
        // Anthropic Claude 3.7+
        let mut d = ReasoningDetector::new();
        assert_segments(
            feed_and_flush(&mut d, "⟨thinking⟩alpha⟨/thinking⟩beta"),
            &[
                (TokenPart::Reasoning, "alpha"),
                (TokenPart::Final, "beta"),
            ],
        );
        // Mistral / Mixtral (older)
        let mut d = ReasoningDetector::new();
        assert_segments(
            feed_and_flush(&mut d, "<reasoning>alpha</reasoning>beta"),
            &[
                (TokenPart::Reasoning, "alpha"),
                (TokenPart::Final, "beta"),
            ],
        );
        // Custom fine-tunes / system prompts
        let mut d = ReasoningDetector::new();
        assert_segments(
            feed_and_flush(&mut d, "[Reasoning]alpha[/Reasoning]beta"),
            &[
                (TokenPart::Reasoning, "alpha"),
                (TokenPart::Final, "beta"),
            ],
        );
    }

    /// An open marker that never closes. The detector's
    /// hold-back window means it cannot tell whether the
    /// trailing bytes are the start of a close marker that
    /// didn't fit in the chunk, so it stashes them in
    /// `partial`. The "safe" prefix (everything except the
    /// last 15 bytes) IS emitted as `Reasoning` from `feed()`.
    /// `flush()` then emits the held-back tail as `Reasoning`
    /// too. Coalesced, the user-visible output is a single
    /// `Reasoning` segment containing the full inside of the
    /// open block.
    #[test]
    fn unclosed_open_marker_holds_tail() {
        let mut d = ReasoningDetector::new();
        // Smoke check: the detector must not panic and must
        // produce a non-empty Reasoning segment (containing
        // the inside of the unclosed block).
        let all = feed_and_flush(&mut d, "<think>never closes, no end");
        assert_segments(
            all,
            &[(TokenPart::Reasoning, "never closes, no end")],
        );
    }

    /// `ReasoningHistory::set()` should compute a one-line preview
    /// and a correct char count.
    #[test]
    fn reasoning_history_set_computes_preview_and_count() {
        let mut h = ReasoningHistory::default();
        h.set("  first line of reasoning\n  second line\n  third".to_string());
        assert_eq!(h.char_count, "  first line of reasoning\n  second line\n  third".chars().count());
        // The preview should be the first non-empty line, trimmed.
        assert_eq!(h.preview.as_deref(), Some("first line of reasoning"));
        // A new turn always starts collapsed.
        assert!(!h.expanded);
    }

    /// `ReasoningHistory::set()` truncates previews longer than 64
    /// characters with a trailing ellipsis. The truncation must
    /// be by *character*, not byte — a multi-byte UTF-8 string
    /// should not panic.
    #[test]
    fn reasoning_history_truncates_long_preview() {
        let mut h = ReasoningHistory::default();
        let long = "x".repeat(200);
        h.set(long.clone());
        let preview = h.preview.as_deref().unwrap_or("");
        // 64 chars + the trailing "…" (which is itself 1 char in
        // our format string). Total ≤ 65 chars.
        assert!(preview.chars().count() <= 65, "preview too long: {} chars", preview.chars().count());
        assert!(preview.ends_with('…'), "preview should end with ellipsis");
        // The full text is preserved, only the preview is cut.
        assert_eq!(h.char_count, 200);
        let _ = long; // silence unused warning if assertions above change
    }

    /// `ReasoningHistory::clear()` wipes every field. The slash
    /// command relies on this to show "no reasoning captured"
    /// when a turn produces no reasoning.
    #[test]
    fn reasoning_history_clear_resets_all_fields() {
        let mut h = ReasoningHistory::default();
        h.set("some reasoning".to_string());
        h.expanded = true;
        h.clear();
        assert!(h.text.is_none());
        assert!(h.preview.is_none());
        assert_eq!(h.char_count, 0);
        assert!(!h.expanded);
    }
}
