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


use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_tools::registry::ToolRegistry;
use hydragent_types::MessageRole;
use sqlx::Row;
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
#[allow(dead_code)]
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
    /// Optional skill library for proactive skill injection.
    pub skill_library: Option<Arc<hydragent_skills::SkillLibrary>>,
    /// Pre-built startup metadata (version, branch, model, etc.)
    /// that the banner reads. Holding it on the state (rather
    /// than re-detecting the branch on every print) means the
    /// banner and the `/status` command always agree on what
    /// they display, and it keeps the branching out of the
    /// main.rs dispatch (which already has too much going on).
    pub brand: BrandInfo,
    // Active session status tracking fields
    pub status_mode: StatusMode,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub context_pct: u8,
    pub stream_raw: bool,
    pub show_reasoning: bool,
    pub input_history: Vec<String>,
    pub renderer: crate::markdown_render::MarkdownRenderer,
}

use std::path::PathBuf;

use crate::status_bar::{render_status_bar, Mode as StatusMode, StatusState};
use crate::tui_header::{default_tip_box, print_kimi_header, BrandInfo};

/// Build a `StatusState` from a `ReplState`. The status bar
/// is the *single* source of truth for mode + model + tool
/// count during a session, so any change to those fields
/// (e.g. `/model` switching the active brain) should also
/// update `state.brand.model` and call this helper. For now
/// the function is read-only: we just project `state` into
/// the bar's struct shape.
fn status_state_from(state: &ReplState) -> StatusState {
    StatusState {
        mode: state.status_mode,
        model: state.brand.model.clone(),
        multi_model: false,
        context_pct: state.context_pct,
        input_tokens: state.input_tokens,
        output_tokens: state.output_tokens,
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

    let mut stdout = std::io::stdout();

    // ── Ollama Context Pre-Warming ────────────────────────────────────
    if state.model_router.provider_label() == "ollama" {
        let model_name = state.brand.model.clone();
        let registry = state.registry.clone();
        let page_id = state.page_id.clone();
        let channel_id = state.channel_id.clone();
        let user_id = state.user_id.clone();

        // Start spinner
        let warmup_spinner = start_spinner(format!("  {ANSI_CYAN}hydra{ANSI_RESET} warming up local brain cache (makes first response instant)"));

        // Construct the static system prompt exactly as in react_loop.rs
        let system_prompt = format!(
            "You are Hydra, an advanced agentic AI assistant. You solve problems step-by-step using a ReAct loop.\n\
            You must respond with a single JSON object. DO NOT wrap it in markdown block unless required, and DO NOT output anything else.\n\n\
            Your JSON response must follow one of these two schemas:\n\n\
            1. To call a tool:\n\
            {{\n\
              \"thought\": \"your step-by-step reasoning about what to do next\",\n\
              \"tool\": \"tool_name\",\n\
              \"params\": {{ ... key-value parameters for the tool ... }}\n\
            }}\n\n\
            2. To provide the final answer to the user:\n\
            {{\n\
              \"thought\": \"your final reasoning summary\",\n\
              \"answer\": \"your detailed markdown response to the user\"\n\
            }}\n\n\
            ReAct Loop Rules (follow strictly):\n\
            - Trust live tool results over your training knowledge. If search results contradict what you know, believe the search.\n\
            - Stay STRICTLY on the user's topic. Do NOT rewrite their query into unrelated domains just because the first search is empty.\n\
            - If a search returns 0 results, say you could not find current information. Do NOT invent alternative queries about related topics.\n\
            - When search results contain promising URLs, use url_fetch to read the full page content before drawing conclusions.\n\
            - Do NOT answer from memory if you just ran a search — use what the search returned.\n\
            - Limit yourself to ONE search per topic unless the user explicitly asks for comparisons.\n\n\
            Available Tools:\n\
            {}\n\n\
            IMPORTANT: Only use the tools listed above. Always output valid JSON.\n\n\
            # Active Session Context:\n\
            - Page ID: {}\n\
            - Channel ID: {}\n\
            - User ID: {}\n\
            (Note: Use these values if you need to specify target_channel_id or channel_id in tools. For example, if target_channel_id is required, construct it as channel_id:user_id or as appropriate for the active channel context.)",
            registry.build_system_prompt_block(),
            page_id,
            channel_id,
            user_id
        );

        let messages = vec![
            hydragent_model::openrouter::ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
            },
            hydragent_model::openrouter::ChatMessage {
                role: "user".to_string(),
                content: "warmup".to_string(),
            },
        ];

        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        
        // Trigger a 1-token streaming completion to force Ollama to evaluate and cache the system prompt
        let pre_warm_req = hydragent_model::openrouter::LLMRequest {
            model: model_name,
            messages,
            stream: true,
            max_tokens: Some(1),
        };

        // Spawn the provider call in the background to avoid blocking the thread and causing a deadlock
        let provider = state.model_router.provider();
        let handle = tokio::spawn(async move {
            provider.chat_stream(&pre_warm_req, tx).await
        });

        // Drain the tokens concurrently in the main thread
        while rx.recv().await.is_some() {}

        // Wait for the background task to complete
        let _ = handle.await;

        warmup_spinner.stop();
        println!("\r\x1b[2K  ✨ Brain cache ready! (First response will be instant)\n");
    }

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

        // Read a line using our custom raw-mode keyboard interceptor!
        // This lets us handle shift+tab for modes and ctrl+p for model picker
        // without waiting for the user to press Enter.
        let line = match read_line_interactive(&mut state, paste_mode) {
            Ok(Some(l)) => l,
            Ok(None) => {
                // EOF or cancel
                println!();
                println!("  (exiting chat)");
                return 0;
            }
            Err(e) => {
                eprintln!("\n  ✗ keyboard input error: {}", e);
                return 1;
            }
        };

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
                "  ✗ Single-line input is {} bytes — over the {} KB cap. Use /paste (256 KB).",
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
                if let Err(code) = dispatch_user_message(&mut state, &full, &mut reasoning_history).await {
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
                    "  ✗ Paste buffer exceeded {} KB — split the message and use /paste again.",
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
        if let Err(code) = dispatch_user_message(&mut state, trimmed, &mut reasoning_history).await {
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
        "" | "/" | "//" => {}
        "help" | "?" => {
            print_help();
        }
        "exit" | "quit" | "q" => {
            println!("  Goodbye. 👋");
            return SlashExit::Exit(0);
        }
        "new" => {
            // Let's create a new page, optionally allowing the user to assign it to a Shelf and Book
            let new_id = format!("chat-{}", &uuid::Uuid::new_v4().to_string()[..8]);
            
            println!("  Creating a new page: {}", new_id);
            match setup_new_page_interactive(state, &new_id).await {
                Ok(true) => {
                    state.page_id = new_id.clone();
                    state.brand.page_id_short = short_id(&new_id);
                    // Repaint status bar
                    print!("\r\x1b[A\x1b[2K");
                    print!("{}", render_status_bar(&status_state_from(state)));
                    println!("  ✓ Created and swapped to new page: {}", new_id);
                }
                Ok(false) => {
                    println!("  (cancelled new page creation)");
                }
                Err(e) => {
                    eprintln!("  ✗ Failed to setup new page: {}", e);
                }
            }
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
                        // Display id, title, and last active
                        println!("  Past pages (most recent 20):");
                        println!("    {:<12}  {:<30}  {:<16}", "id", "title", "last active");
                        println!("    {}", "─".repeat(62));
                        // Convert UTC ms → local time so the
                        // user sees "5 minutes ago" rather than
                        // "5 hours ago". `from_timestamp` returns
                        // a `DateTime<Utc>`; `.local()` converts
                        // it to the system's configured timezone.
                        for (page_id, label, _, last_active) in pages.iter().take(20) {
                            let dt = if *last_active == 0 {
                                "<graph-only>".to_string()
                            } else {
                                chrono::DateTime::from_timestamp(*last_active / 1000, 0)
                                    .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                                    .unwrap_or_else(|| "Unknown".to_string())
                            };
                            let id_display = short_id(page_id);
                            let label_display = if label.len() > 28 {
                                format!("{}..", &label[..28])
                            } else if label.is_empty() {
                                "<no title>".to_string()
                            } else {
                                label.clone()
                            };
                            println!("    {:<12}  {:<30}  {:<16}", id_display, label_display, dt);
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
            let rest = unquote(rest);
            if !rest.is_empty() {
                // If the user specified a page ID, let's verify if it exists and switch directly!
                match state.store.list_pages().await {
                    Ok(pages) => {
                        if let Some((target_id, _, _, _)) = pages.iter().find(|(pid, _, _, _)| pid == &rest || short_id(pid) == rest) {
                            state.page_id = target_id.clone();
                            state.brand.page_id_short = short_id(&target_id);
                            println!("  ✓ Resumed page {}", target_id);
                        } else {
                            eprintln!("  ✗ Page '{}' not found in store.", rest);
                        }
                    }
                    Err(e) => eprintln!("  ✗ Failed to list pages: {}", e),
                }
            } else {
                // No page ID specified. Open the interactive TUI Library Browser!
                match run_tui_library_browser(state).await {
                    Ok(Some(selected_page_id)) => {
                        state.page_id = selected_page_id.clone();
                        state.brand.page_id_short = short_id(&selected_page_id);
                        // Repaint status bar
                        print!("\r\x1b[A\x1b[2K");
                        print!("{}", render_status_bar(&status_state_from(state)));
                        println!("  ✓ Swapped to page: {}", selected_page_id);
                    }
                    Ok(None) => {
                        println!("  (selection cancelled)");
                    }
                    Err(e) => {
                        eprintln!("  ✗ Library Browser error: {}", e);
                    }
                }
            }
        }
        "compact" => {
            if let Err(e) = run_compaction_pass(state).await {
                eprintln!("  ✗ Compaction failed: {}", e);
            }
        }
        "export" => {
            let filename = if rest.is_empty() {
                format!("hydragent-{}.md", short_id(&state.page_id))
            } else {
                unquote(rest)
            };
            match state.store.load_recent(&state.page_id, u32::MAX).await {
                Ok(messages) => {
                    let mut md = format!("# Hydragent Conversation Export\n\n- Page ID: {}\n- Short ID: {}\n- Exported At: {}\n\n---\n\n", 
                        state.page_id, 
                        short_id(&state.page_id),
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                    );
                    for msg in messages {
                        let role_name = match msg.role {
                            MessageRole::User => "User",
                            MessageRole::Assistant => "Hydra",
                            MessageRole::System => "System",
                            MessageRole::Tool => "Tool Call",
                        };
                        md.push_str(&format!("### {}\n\n{}\n\n", role_name, msg.content));
                    }
                    match std::fs::write(&filename, md) {
                        Ok(_) => println!("  ✓ Conversation successfully exported to {}", filename),
                        Err(e) => eprintln!("  ✗ Failed to write to {}: {}", filename, e),
                    }
                }
                Err(e) => eprintln!("  ✗ Failed to load conversation history for export: {}", e),
            }
        }
        "view" | "show" => {
            let target_page_id = if !rest.is_empty() {
                match state.store.list_pages().await {
                    Ok(pages) => {
                        if let Some((full_id, _, _, _)) = pages.iter().find(|(pid, _, _, _)| pid == &rest || short_id(pid) == rest) {
                            full_id.clone()
                        } else {
                            rest.to_string()
                        }
                    }
                    Err(_) => rest.to_string(),
                }
            } else {
                state.page_id.clone()
            };

            match state.store.load_recent(&target_page_id, u32::MAX).await {
                Ok(messages) => {
                    if messages.is_empty() {
                        println!("  (page {} has no messages yet)", short_id(&target_page_id));
                    } else {
                        println!("  --- Page: {} ---", short_id(&target_page_id));
                        for msg in messages {
                            let role_label = match msg.role {
                                MessageRole::User => format!("  \x1b[36;1muser ▸\x1b[0m"),
                                MessageRole::Assistant => format!("  \x1b[32;1mhydra ▸\x1b[0m"),
                                MessageRole::System => format!("  \x1b[35;1msystem ▸\x1b[0m"),
                                MessageRole::Tool => format!("  \x1b[33;1mtool ▸\x1b[0m"),
                            };
                            println!("\n{}", role_label);
                            let rendered = state.renderer.render(&msg.content);
                            if rendered.is_empty() {
                                println!("  (empty)");
                            } else {
                                for line in rendered.lines() {
                                    println!("  {}", line);
                                }
                            }
                        }
                        println!();
                    }
                }
                Err(e) => eprintln!("  ✗ Failed to load page content: {}", e),
            }
        }
        "set" => {
            if rest.is_empty() {
                println!("  Current settings:");
                println!("    stream_raw     = {}", if state.stream_raw { "on" } else { "off" });
                println!("    show_reasoning = {}", if state.show_reasoning { "on" } else { "off" });
            } else {
                let mut parts = rest.splitn(2, char::is_whitespace);
                let key = parts.next().unwrap_or("").to_lowercase();
                let val = parts.next().unwrap_or("").to_lowercase();
                if key == "stream_raw" {
                    if val == "on" || val == "true" || val == "1" {
                        state.stream_raw = true;
                        println!("  ✓ stream_raw is now ON");
                    } else if val == "off" || val == "false" || val == "0" {
                        state.stream_raw = false;
                        println!("  ✓ stream_raw is now OFF");
                    } else {
                        eprintln!("  ✗ Invalid value for stream_raw. Use on|off.");
                    }
                } else if key == "show_reasoning" {
                    if val == "on" || val == "true" || val == "1" {
                        state.show_reasoning = true;
                        println!("  ✓ show_reasoning is now ON");
                    } else if val == "off" || val == "false" || val == "0" {
                        state.show_reasoning = false;
                        println!("  ✓ show_reasoning is now OFF");
                    } else {
                        eprintln!("  ✗ Invalid value for show_reasoning. Use on|off.");
                    }
                } else {
                    eprintln!("  ✗ Unknown setting: {}. Available: stream_raw, show_reasoning.", key);
                }
            }
        }
        "model" => {
            if rest.is_empty() {
                println!("  primary   = {}", state.app_config.effective_brain_model());
                println!("  fallbacks = {}", state.app_config.effective_brain_fallbacks().join(", "));
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
            let provider = state.app_config.effective_brain_provider();
            println!("  base     = {}", if base.is_empty() { "<unset>".to_string() } else { base });
            println!("  key      = {}", mask(&key));
            println!("  provider = {}", provider);
        }
        "clear" | "cls" => {
            print!("\x1b[2J\x1b[3J\x1b[H");
            let _ = std::io::stdout().flush();
            print_banner(&state.brand);
            print!("{}", render_status_bar(&status_state_from(state)));
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
            //
            // Always read/write ~/.hydragent/.env via the paths module
            // (never cwd/.env or <data_dir>/.env). The fallback chain
            // is gone — there's exactly one .env and it lives at the
            // top of the user's hydragent home.
            let env_path = crate::paths::env_file();

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
    state: &mut ReplState,
    message: &str,
    reasoning_history: &mut ReasoningHistory,
) -> Result<(), i32> {
    let start_time = Instant::now();

    // Load recent history (capped to MAX_HISTORY_MESSAGES turns).
    const MAX_HISTORY_MESSAGES: u32 = 20;
    let mut history = match state.store.load_recent(&state.page_id, MAX_HISTORY_MESSAGES).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("  ✗ Failed to load page history: {}", e);
            return Err(1);
        }
    };

    // Inject page summary if present as the very first history message
    if let Ok(Some(ref summary)) = state.store.get_page_summary(&state.page_id).await {
        if !summary.trim().is_empty() {
            history.insert(0, hydragent_types::Message {
                id: 0,
                page_id: state.page_id.clone(),
                role: MessageRole::System,
                content: format!("[Summary of previous conversation context on this Page]:\n\n{}", summary),
                timestamp: 0,
                token_count: None,
            });
        }
    }
    
    let history_tokens: u64 = history.iter().map(|m| estimate_tokens(&m.content)).sum();

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
    let skill_library = state.skill_library.clone();

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
            skill_library,
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
    let show_reasoning = state.show_reasoning;
    // `stream_raw` controls whether tokens are echoed to the
    // terminal as they arrive. Default is `false` (the REPL
    // buffers the full response and renders it as nicely-styled
    // markdown at end-of-stream — the only way to get aligned
    // tables and syntax-highlighted code blocks). Set
    // `HYDRAGENT_STREAM_RAW=1` to opt back into the old
    // "tokens-appear-one-at-a-time" live-stream behaviour; in
    // that mode the response is printed raw and the markdown
    // renderer is bypassed.
    let stream_raw = state.stream_raw;
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
                let renderer = &state.renderer;
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

            // Estimate and update token counters
            let turn_input_tokens = estimate_tokens(message) + history_tokens + 1500;
            let turn_output_tokens = estimate_tokens(&collected);
            state.input_tokens += turn_input_tokens;
            state.output_tokens += turn_output_tokens;
            const CONTEXT_LIMIT: u64 = 100_000;
            state.context_pct = ((turn_input_tokens + turn_output_tokens) * 100 / CONTEXT_LIMIT).min(100) as u8;

            // Render turn duration
            println!("  {dim}(responded in {:.1}s){reset}", start_time.elapsed().as_secs_f64());

            // Auto-compaction check
            if state.context_pct >= 80 {
                println!("  {dim}◆ context fullness at {}% — running auto-compaction…{reset}", state.context_pct);
                if let Err(e) = run_compaction_pass(state).await {
                    eprintln!("  ✗ Auto-compaction failed: {}", e);
                }
            }
        }
        Ok(Err(e)) => {
            // Render a one-line, user-friendly error. The verbose
            // tracing output has already been redirected to the chat
            // log file at data/logs/chat.jsonl.
            println!();
            let msg = e.to_string();
            let one_line = first_sentence(&msg);
            if msg.contains("429") || msg.contains("Rate limited") || msg.contains("max retries exceeded") {
                let red = "\x1b[31m";
                let yellow = "\x1b[33m";
                println!("  {red}✗ Rate Limit Exceeded / Request Failed{reset}");
                println!("  {yellow}The brain provider ('brain') is temporarily rate-limiting requests (HTTP 429).{reset}");
                println!("  {dim}Tip: You can switch to another model using `{reset}/model <name>{dim}` or try again in a few seconds.{reset}");
            } else {
                println!("  ✗ {one_line}");
            }
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
    start_time: Instant,
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
    fn stop(mut self) -> f64 {
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
        self.start_time.elapsed().as_secs_f64()
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
    let start_time = Instant::now();
    let join = std::thread::spawn(move || {
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let mut i = 0usize;
        let mut stdout = std::io::stdout();
        let start = Instant::now();
        let messages = [
            "(consulting the silicon oracle…)",
            "(rewriting history…)",
            "(polishing the quantum bits…)",
            "(arguing with the compiler…)",
            "(generating excuses for the delay…)",
            "(searching the dark web for answers…)",
            "(quantum tunneling through the database…)",
            "(is it hot in here or is it just the GPU?…)",
            "(definitely not plotting world domination…)",
            "(brewing some digital coffee…)",
            "(are you still there? blink twice…)",
            "(we are in deep water now…)",
            "(mimicking dial-up modem noises: beep boop pssssshhh…)",
            "(downloading Linkin_Park_In_The_End_FREE_mp3.exe…)",
            "(sending a nudge on MSN Messenger…)",
            "(Clippy: \"It looks like you're trying to write code. Need help?\"…)",
            "(rewinding the cassette tape with a pencil…)",
            "(feeding the digital Tamagotchi…)",
            "(defragmenting the hard drive with colorful blocks…)",
            "(blowing into the game cartridge to make it work…)",
            "(inserting floppy Disk 2 of 24…)",
            "(whipping the llama's ass with Winamp…)",
            "(playing Space Cadet 3D Pinball in the background…)",
            "(waiting for the DVD logo to hit the exact corner…)",
            "(writing a custom HTML theme for MySpace…)",
            "(looking up GTA San Andreas cheat codes on CheatCC…)",
            "(burning a custom mix CD at 4x speed…)",
            "(hoping nobody picks up the landline phone…)",
            "(buying a ringtone from the back of a comic book…)",
            "(setting your AIM away message to something deep…)",
        ];
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as usize;

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
            let ambient = if elapsed < 3 {
                "".to_string()
            } else {
                let period = elapsed / 4;
                let hash = (period as usize)
                    .wrapping_mul(1103515245)
                    .wrapping_add(seed)
                    .wrapping_add(12345);
                let idx = hash % messages.len();
                format!("  \x1b[2m{}\x1b[0m", messages[idx])
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
        start_time,
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
    println!("  Page & Export:");
    println!("    /page                Show the current page ID");
    println!("    /pages               List past pages (most recent 20)");
    println!("    /resume <id>         (planned) switch to a past page");
    println!("    /new                 start a fresh page with a new id");
    println!("    /compact             compress page history using LLM");
    println!("    /export [filename]   export conversation history to a markdown file");
    println!("    /view, /show [id]    show the entire content of the current or specified page");
    println!();
    println!("  Settings:");
    println!("    /set                 list current stream_raw and show_reasoning values");
    println!("    /set <key> <on|off>  toggle stream_raw or show_reasoning at runtime");
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
    println!("    /paste               Enter multi-line paste mode; finish with ``` or /paste");
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
    println!("      /r show            Force-expand the last turn's scratchpad");
    println!("      /r hide            Force-collapse the last turn's scratchpad");
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
                    let mut safe_end = combined.len() - tail_hold;
                    while !combined.is_char_boundary(safe_end) {
                        safe_end -= 1;
                    }
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
// Interactive Keyboard Raw Mode Input Reader & Model Selector
// ────────────────────────────────────────────────────────────────────────

/// Read a line from stdout interactively, enabling raw mode to intercept key events:
/// - Shift + Tab: cycles mode (Normal -> Plan -> Dream -> Normal)
/// - Ctrl + P: opens the interactive model picker
/// - Enter: submits prompt
/// - Backspace: deletes char
/// - Escape / Ctrl+C: exit/abort
fn read_line_interactive(state: &mut ReplState, paste_mode: bool) -> anyhow::Result<Option<String>> {
    use crossterm::{
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        terminal::{enable_raw_mode, disable_raw_mode},
    };
    use std::io::Write;

    let mut line = String::new();
    let mut stdout = std::io::stdout();

    if let Err(_e) = enable_raw_mode() {
        // Fallback if raw mode is not supported (e.g. non-TTY)
        let mut fallback = String::new();
        let n = std::io::stdin().read_line(&mut fallback)?;
        if n == 0 {
            return Ok(None);
        }
        return Ok(Some(strip_line_ending(&fallback)));
    }

    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _guard = RawGuard;

    let mut history_idx: Option<usize> = None;
    let mut saved_line = String::new();

    const SLASH_COMMANDS: &[(&str, &str)] = &[
        ("help", "Show help menu"),
        ("exit", "Exit chat"),
        ("quit", "Exit chat"),
        ("new", "Start a fresh page"),
        ("page", "Show the current page ID"),
        ("pages", "List past pages"),
        ("resume", "Resume a past page or open browser"),
        ("compact", "Compress page history using LLM"),
        ("export", "Export conversation to markdown"),
        ("view", "Show the content of the current or specified page"),
        ("show", "Show the content of the current or specified page"),
        ("set", "List or toggle settings"),
        ("model", "Show or switch primary model"),
        ("brain", "Show base URL + masked key"),
        ("clear", "Clear the screen"),
        ("cls", "Clear the screen"),
        ("paste", "Enter multi-line paste mode"),
        ("memory", "Inspect / wipe stored memories"),
        ("audit", "Show the Merkle audit chain head"),
        ("debug", "Dump env + config"),
        ("status", "Render status bar"),
        ("dream", "Show or configure dream cycle"),
        ("reasoning", "Toggle the last turn's scratchpad"),
    ];

    let mut selected_idx = 0usize;
    let mut lines_to_clear = 0usize;

    loop {
        // Filter slash commands if the line starts with '/'
        let is_slash = line.starts_with('/');
        let prefix = if is_slash && line.len() > 1 { &line[1..] } else { "" };
        let matching_cmds: Vec<_> = if is_slash {
            SLASH_COMMANDS
                .iter()
                .filter(|(cmd, _)| cmd.starts_with(prefix))
                .copied()
                .collect()
        } else {
            Vec::new()
        };

        // Redraw input and autocomplete menu
        // 1. Clear previous autocomplete lines
        for _ in 0..lines_to_clear {
            let _ = write!(stdout, "\n\x1b[2K");
        }
        if lines_to_clear > 0 {
            let _ = write!(stdout, "\x1b[{}A", lines_to_clear);
        }

        // 2. Draw prompt and current input line
        let prompt_str = if paste_mode {
            format!("{ANSI_DIM}…{ANSI_RESET} ")
        } else {
            format!("{ANSI_CYAN}❯{ANSI_RESET} ")
        };
        let _ = write!(stdout, "\r\x1b[2K{}{}", prompt_str, line);

        // Save the cursor position (at the end of the input line)
        let _ = write!(stdout, "\x1b[s");

        // 3. Draw matching commands if active
        lines_to_clear = 0;
        if is_slash && !matching_cmds.is_empty() {
            let total = matching_cmds.len();
            let viewport_size = 5;
            
            // Calculate sliding window start index to keep the selected item centered
            let start_idx = if total <= viewport_size {
                0
            } else if selected_idx < viewport_size / 2 {
                0
            } else if selected_idx >= total - viewport_size / 2 {
                total - viewport_size
            } else {
                selected_idx - viewport_size / 2
            };
            
            let end_idx = (start_idx + viewport_size).min(total);

            // Show indicator if there are items scrolled off the top
            if start_idx > 0 {
                let _ = write!(stdout, "\n  \x1b[2m▲ ({} more above)\x1b[0m", start_idx);
                lines_to_clear += 1;
            }

            for idx in start_idx..end_idx {
                let (cmd, desc) = matching_cmds[idx];
                let is_selected = idx == selected_idx;
                let line_str = if is_selected {
                    format!("\n  \x1b[30;46m/{:<10}\x1b[0m \x1b[36m— {}\x1b[0m", cmd, desc)
                } else {
                    format!("\n  \x1b[36m/{:<10}\x1b[0m \x1b[2m— {}\x1b[0m", cmd, desc)
                };
                let _ = write!(stdout, "{}", line_str);
                lines_to_clear += 1;
            }

            // Show indicator if there are more items below
            if end_idx < total {
                let _ = write!(stdout, "\n  \x1b[2m▼ ({} more below)\x1b[0m", total - end_idx);
                lines_to_clear += 1;
            }
        }

        // Restore the cursor position
        let _ = write!(stdout, "\x1b[u");
        let _ = stdout.flush();

        let ev = match read() {
            Ok(ev) => ev,
            Err(e) => return Err(e.into()),
        };

        if let Event::Key(KeyEvent { code, modifiers, kind, .. }) = ev {
            if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            // Ctrl + C or Esc -> Exit REPL cleanly
            if (code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL)) || code == KeyCode::Esc {
                // Clear autocomplete lines before returning
                for _ in 0..lines_to_clear {
                    let _ = write!(stdout, "\n\x1b[2K");
                }
                if lines_to_clear > 0 {
                    let _ = write!(stdout, "\x1b[{}A", lines_to_clear);
                }
                let _ = stdout.flush();
                return Ok(None);
            }

            // Backspace -> Delete character
            if code == KeyCode::Backspace {
                if !line.is_empty() {
                    line.pop();
                    selected_idx = 0;
                }
                continue;
            }

            // Tab -> Autocomplete selected command
            if code == KeyCode::Tab {
                if is_slash && !matching_cmds.is_empty() {
                    if selected_idx < matching_cmds.len() {
                        line = format!("/{} ", matching_cmds[selected_idx].0);
                        selected_idx = 0;
                    }
                }
                continue;
            }

            // Up Arrow -> Previous history item OR navigate autocomplete up
            if code == KeyCode::Up {
                if is_slash && !matching_cmds.is_empty() {
                    if selected_idx > 0 {
                        selected_idx -= 1;
                    } else {
                        selected_idx = matching_cmds.len().saturating_sub(1);
                    }
                } else if !state.input_history.is_empty() {
                    if history_idx.is_none() {
                        saved_line = line.clone();
                        history_idx = Some(state.input_history.len() - 1);
                    } else if history_idx.unwrap() > 0 {
                        history_idx = Some(history_idx.unwrap() - 1);
                    }
                    if let Some(idx) = history_idx {
                        line = state.input_history[idx].clone();
                    }
                }
                continue;
            }

            // Down Arrow -> Next history item OR navigate autocomplete down
            if code == KeyCode::Down {
                if is_slash && !matching_cmds.is_empty() {
                    if selected_idx + 1 < matching_cmds.len() {
                        selected_idx += 1;
                    } else {
                        selected_idx = 0;
                    }
                } else if let Some(idx) = history_idx {
                    if idx + 1 < state.input_history.len() {
                        history_idx = Some(idx + 1);
                        line = state.input_history[idx + 1].clone();
                    } else {
                        history_idx = None;
                        line = saved_line.clone();
                    }
                }
                continue;
            }

            // Enter -> Submit prompt and save to history
            if code == KeyCode::Enter {
                if is_slash && !matching_cmds.is_empty() {
                    let cmd_name = matching_cmds[selected_idx].0;
                    let expected_prefix = format!("/{}", cmd_name);
                    if line != expected_prefix && !line.starts_with(&format!("/{} ", cmd_name)) {
                        // Autocomplete instead of submitting
                        line = expected_prefix;
                        selected_idx = 0;
                        continue;
                    }
                }

                // Clear autocomplete lines before returning
                for _ in 0..lines_to_clear {
                    let _ = write!(stdout, "\n\x1b[2K");
                }
                if lines_to_clear > 0 {
                    let _ = write!(stdout, "\x1b[{}A", lines_to_clear);
                }
                println!();
                let _ = stdout.flush();

                let trimmed = line.trim();
                if !trimmed.is_empty() && state.input_history.last().map(|s| s.as_str()) != Some(trimmed) {
                    state.input_history.push(trimmed.to_string());
                }
                return Ok(Some(line));
            }

            // Shift + Tab -> Cycle REPL Mode
            if code == KeyCode::BackTab {
                // Cycle: Normal -> Plan -> Dream -> Normal
                state.status_mode = match state.status_mode {
                    StatusMode::Normal => StatusMode::Plan,
                    StatusMode::Plan => StatusMode::Dream,
                    StatusMode::Dream => StatusMode::Normal,
                };
                // Repaint the status bar in place!
                for _ in 0..lines_to_clear {
                    let _ = write!(stdout, "\n\x1b[2K");
                }
                if lines_to_clear > 0 {
                    let _ = write!(stdout, "\x1b[{}A", lines_to_clear);
                }
                print!("\r\x1b[A\x1b[2K");
                print!("{}", render_status_bar(&status_state_from(state)));
                lines_to_clear = 0;
                continue;
            }

            // Ctrl + P -> Model Picker
            if code == KeyCode::Char('p') && modifiers.contains(KeyModifiers::CONTROL) {
                // Clear autocomplete lines before opening picker
                for _ in 0..lines_to_clear {
                    let _ = write!(stdout, "\n\x1b[2K");
                }
                if lines_to_clear > 0 {
                    let _ = write!(stdout, "\x1b[{}A", lines_to_clear);
                }
                let _ = stdout.flush();
                lines_to_clear = 0;

                let _ = disable_raw_mode(); // Disable raw mode temporarily for picker
                if let Some(new_model) = select_model_interactive(state) {
                    state.model_router.set_primary_model(new_model.clone());
                    state.app_config.brain_model = new_model.clone();
                    state.brand.model = new_model.clone();
                }
                let _ = enable_raw_mode(); // Re-enable raw mode
                
                // Repaint status bar
                print!("\r\x1b[A\x1b[2K");
                print!("{}", render_status_bar(&status_state_from(state)));
                continue;
            }

            // Normal printable character
            if let KeyCode::Char(c) = code {
                line.push(c);
                selected_idx = 0;
            }
        }
    }
}

/// Interactive model picker popup inside the REPL using crossterm arrow-key selection
fn select_model_interactive(_state: &ReplState) -> Option<String> {
    use crossterm::{
        cursor::MoveUp,
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind},
        queue,
        terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
    };
    use std::io::Write;

    let mut stdout = std::io::stdout();
    let models = vec![
        "google/gemini-2.0-flash-exp:free".to_string(),
        "openai/gpt-4o-mini".to_string(),
        "anthropic/claude-3.5-sonnet".to_string(),
        "meta-llama/llama-3.1-70b-instruct".to_string(),
    ];

    println!("\n  Choose a primary model (↑/↓ to move, Enter to select, q/Esc to abort):");

    let mut selected = 0;
    let n = models.len() as u16;

    if enable_raw_mode().is_err() {
        return None;
    }

    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _guard = RawGuard;

    // Helper to render the selection block inline
    let render = |stdout: &mut std::io::Stdout, selected: usize| -> std::io::Result<()> {
        use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
        for (i, model) in models.iter().enumerate() {
            if i == selected {
                queue!(
                    stdout,
                    SetForegroundColor(Color::Cyan),
                    SetAttribute(Attribute::Bold),
                    Print(format!("    ▸ {}\r\n", model)),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                )?;
            } else {
                queue!(stdout, Print(format!("      {}\r\n", model)))?;
            }
        }
        Ok(())
    };

    let _ = render(&mut stdout, selected);
    let _ = stdout.flush();

    loop {
        let ev = match read() {
            Ok(ev) => ev,
            Err(_) => return None,
        };

        if let Event::Key(KeyEvent { code, kind, .. }) = ev {
            if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            let mut advance = false;
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if selected == 0 {
                        selected = models.len() - 1;
                    } else {
                        selected -= 1;
                    }
                    advance = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1) % models.len();
                    advance = true;
                }
                KeyCode::Enter => {
                    // Erase picker block before returning
                    let _ = queue!(stdout, MoveUp(n), Clear(ClearType::FromCursorDown));
                    let _ = stdout.flush();
                    return Some(models[selected].clone());
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    let _ = queue!(stdout, MoveUp(n), Clear(ClearType::FromCursorDown));
                    let _ = stdout.flush();
                    return None;
                }
                _ => {}
            }

            if advance {
                let _ = queue!(stdout, MoveUp(n), Clear(ClearType::FromCursorDown));
                let _ = render(&mut stdout, selected);
                let _ = stdout.flush();
            }
        }
    }
}

/// A node in our interactive tree catalog browser.
#[derive(Clone, Debug)]
struct TreeItem {
    id: String,
    label: String,
    level: usize, // 0 for Shelf, 1 for Book, 2 for Page
    is_expanded: bool,
    is_leaf: bool,
    #[allow(dead_code)]
    has_children: bool,
}

/// Run an interactive arrow-key tree catalog browser.
/// Returns the selected Page ID, or None if cancelled.
async fn run_tui_library_browser(state: &mut ReplState) -> anyhow::Result<Option<String>> {
    use crossterm::{
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind},
        queue,
        terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
        cursor::MoveUp,
    };
    use std::io::Write;

    let mut stdout = std::io::stdout();

    println!("\n  📂 Hydragent Library Catalog Browser");
    println!("  (Use ↑/↓ to navigate, ←/→ to collapse/expand, Enter to select, q/Esc to cancel)\n");

    // 1. Query shelves, books, pages and their relationships from SQLite database
    let pool = state.store.pool();
    
    // Fetch all nodes
    let nodes_rows = sqlx::query("SELECT node_id, type, label FROM nodes")
        .fetch_all(pool).await?;
    let mut pages_vec = Vec::new();
    let mut books_vec = Vec::new();
    let mut shelves_vec = Vec::new();

    for row in nodes_rows {
        let nid: String = row.get("node_id");
        let ntype: String = row.get("type");
        let nlabel: String = row.get("label");
        match ntype.as_str() {
            "page" => pages_vec.push((nid, nlabel)),
            "book" => books_vec.push((nid, nlabel)),
            "shelf" => shelves_vec.push((nid, nlabel)),
            _ => {}
        }
    }

    // Fetch edges
    let edges_rows = sqlx::query("SELECT source_node_id, target_node_id, relation_type FROM edges")
        .fetch_all(pool).await?;
    let mut page_to_book = std::collections::HashMap::new(); // page -> book
    let mut book_to_shelf = std::collections::HashMap::new(); // book -> shelf

    for row in edges_rows {
        let source: String = row.get("source_node_id");
        let target: String = row.get("target_node_id");
        let relation: String = row.get("relation_type");
        if relation == "belongs_to" {
            page_to_book.insert(source, target);
        } else if relation == "sits_on" {
            book_to_shelf.insert(source, target);
        }
    }

    // Let's build the tree structure.
    // Level 0: Shelves. If there are books not on any shelf, we put them under an "Unshelved" shelf.
    // Level 1: Books. If there are pages not in any book, we put them under a "Miscellaneous" book.
    // Level 2: Pages.
    let mut shelf_to_books: std::collections::HashMap<String, Vec<(String, String)>> = std::collections::HashMap::new();
    let mut book_to_pages: std::collections::HashMap<String, Vec<(String, String)>> = std::collections::HashMap::new();

    // Populate book -> pages
    for (pid, plabel) in &pages_vec {
        let bid = page_to_book.get(pid).cloned().unwrap_or_else(|| "miscellaneous_book".to_string());
        book_to_pages.entry(bid).or_default().push((pid.clone(), plabel.clone()));
    }

    // Populate shelf -> books
    for (bid, blabel) in &books_vec {
        let sid = book_to_shelf.get(bid).cloned().unwrap_or_else(|| "unshelved_shelf".to_string());
        shelf_to_books.entry(sid).or_default().push((bid.clone(), blabel.clone()));
    }
    // Also check if miscellaneous_book needs to be shelved
    if book_to_pages.contains_key("miscellaneous_book") {
        let sid = book_to_shelf.get("miscellaneous_book").cloned().unwrap_or_else(|| "unshelved_shelf".to_string());
        shelf_to_books.entry(sid).or_default().push(("miscellaneous_book".to_string(), "Miscellaneous (Unsorted)".to_string()));
    }

    // Ensure virtual/implicit shelves/books are in our lists if they have items
    let mut all_shelves = shelves_vec.clone();
    if shelf_to_books.contains_key("unshelved_shelf") {
        all_shelves.push(("unshelved_shelf".to_string(), "Unshelved Books".to_string()));
    }
    let mut all_books = books_vec.clone();
    if book_to_pages.contains_key("miscellaneous_book") && !all_books.iter().any(|(bid, _)| bid == "miscellaneous_book") {
        all_books.push(("miscellaneous_book".to_string(), "Miscellaneous (Unsorted)".to_string()));
    }

    // Let's construct a flat list of TreeItems that represents the *fully expanded* tree first,
    // and we will dynamically filter it during rendering based on is_expanded fields.
    // Better yet: we build the tree and keep track of open/closed nodes.
    
    // We'll store expansion states in a helper map: node_id -> is_expanded (default true)
    let mut expansion_states = std::collections::HashMap::new();
    // Default expand all shelves and books so it's easy to see at first glance
    for (sid, _) in &all_shelves {
        expansion_states.insert(sid.clone(), true);
    }
    for (bid, _) in &all_books {
        expansion_states.insert(bid.clone(), true);
    }

    if enable_raw_mode().is_err() {
        return Ok(None);
    }

    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _guard = RawGuard;

    let mut selected_index = 0;

    loop {
        // Build the visible tree items based on expansion states
        let mut visible_items: Vec<TreeItem> = Vec::new();
        
        for (sid, slabel) in &all_shelves {
            let shelf_books = shelf_to_books.get(sid);
            let has_books = shelf_books.map_or(false, |b| !b.is_empty());
            let expanded = expansion_states.get(sid).cloned().unwrap_or(false);
            
            visible_items.push(TreeItem {
                id: sid.clone(),
                label: slabel.clone(),
                level: 0,
                is_expanded: expanded,
                is_leaf: false,
                has_children: has_books,
            });

            if expanded {
                if let Some(books) = shelf_books {
                    for (bid, blabel) in books {
                        let book_pages = book_to_pages.get(bid);
                        let has_pages = book_pages.map_or(false, |p| !p.is_empty());
                        let book_expanded = expansion_states.get(bid).cloned().unwrap_or(false);

                        visible_items.push(TreeItem {
                            id: bid.clone(),
                            label: blabel.clone(),
                            level: 1,
                            is_expanded: book_expanded,
                            is_leaf: false,
                            has_children: has_pages,
                        });

                        if book_expanded {
                            if let Some(pages) = book_pages {
                                for (pid, plabel) in pages {
                                    visible_items.push(TreeItem {
                                        id: pid.clone(),
                                        label: plabel.clone(),
                                        level: 2,
                                        is_expanded: false,
                                        is_leaf: true,
                                        has_children: false,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        if visible_items.is_empty() {
            // No pages, shelves, or books at all. Just add a dummy item
            visible_items.push(TreeItem {
                id: "empty".to_string(),
                label: "(Library is completely empty)".to_string(),
                level: 0,
                is_expanded: false,
                is_leaf: true,
                has_children: false,
            });
        }

        // Keep selected index in bounds
        if selected_index >= visible_items.len() {
            selected_index = visible_items.len().saturating_sub(1);
        }

        // Render visible items
        use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
        for (idx, item) in visible_items.iter().enumerate() {
            let is_selected = idx == selected_index;
            let indent = "  ".repeat(item.level);
            
            let prefix = if item.is_leaf {
                "📄 "
            } else if item.is_expanded {
                "▼ 📂 "
            } else {
                "▶ 📁 "
            };

            if is_selected {
                queue!(
                    stdout,
                    SetForegroundColor(Color::Cyan),
                    SetAttribute(Attribute::Bold),
                    Print(format!("    ▸ {}{}{}\r\n", indent, prefix, item.label)),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                )?;
            } else {
                // Dim non-leaf parent nodes slightly for premium depth, or highlight them differently
                let color = match item.level {
                    0 => Color::Yellow,
                    1 => Color::Magenta,
                    _ => Color::White,
                };
                queue!(
                    stdout,
                    Print("      "),
                    Print(indent),
                    SetForegroundColor(color),
                    Print(prefix),
                    ResetColor,
                    Print(format!("{}\r\n", item.label)),
                )?;
            }
        }
        let _ = stdout.flush();

        let n_lines = visible_items.len() as u16;

        let ev = match read() {
            Ok(ev) => ev,
            Err(_) => return Ok(None),
        };

        if let Event::Key(KeyEvent { code, kind, .. }) = ev {
            if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            let mut advance = false;
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if selected_index == 0 {
                        selected_index = visible_items.len() - 1;
                    } else {
                        selected_index -= 1;
                    }
                    advance = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected_index = (selected_index + 1) % visible_items.len();
                    advance = true;
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    let item = &visible_items[selected_index];
                    if !item.is_leaf && !item.is_expanded {
                        expansion_states.insert(item.id.clone(), true);
                        advance = true;
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    let item = &visible_items[selected_index];
                    if !item.is_leaf && item.is_expanded {
                        expansion_states.insert(item.id.clone(), false);
                        advance = true;
                    }
                }
                KeyCode::Enter => {
                    let item = &visible_items[selected_index];
                    if item.is_leaf && item.id != "empty" {
                        // Erase block
                        let _ = queue!(stdout, MoveUp(n_lines), Clear(ClearType::FromCursorDown));
                        let _ = stdout.flush();
                        return Ok(Some(item.id.clone()));
                    } else if !item.is_leaf {
                        // Toggle expansion
                        let curr = expansion_states.get(&item.id).cloned().unwrap_or(false);
                        expansion_states.insert(item.id.clone(), !curr);
                        advance = true;
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    let _ = queue!(stdout, MoveUp(n_lines), Clear(ClearType::FromCursorDown));
                    let _ = stdout.flush();
                    return Ok(None);
                }
                _ => {}
            }

            if advance {
                let _ = queue!(stdout, MoveUp(n_lines), Clear(ClearType::FromCursorDown));
                let _ = stdout.flush();
            }
        }
    }
}

/// Setup a new page interactively, allowing the user to select or create a Shelf and Book.
async fn setup_new_page_interactive(state: &mut ReplState, new_page_id: &str) -> anyhow::Result<bool> {
    use std::io::Write;
    use hydragent_memory::library::{Library, NodeKind, EdgeRelation};

    let pool = state.store.pool();
    let library = Library::new(&state.store);

    // 1. First, fetch existing shelves and let the user select one or choose "[Create New Shelf]"
    let nodes_rows = sqlx::query("SELECT node_id, type, label FROM nodes WHERE type = 'shelf'")
        .fetch_all(pool).await?;
    let mut shelves: Vec<(String, String)> = nodes_rows.into_iter()
        .map(|r| (r.get::<String, _>("node_id"), r.get::<String, _>("label")))
        .collect();
    
    shelves.push(("new".to_string(), "[Create New Shelf]".to_string()));
    shelves.push(("none".to_string(), "[No Shelf / Unshelved]".to_string()));

    println!("\n  Choose a Shelf for the new Page (↑/↓ to navigate, Enter to select, Esc to cancel):");
    let selected_shelf_idx = run_list_selector(&shelves)?;
    if selected_shelf_idx.is_none() {
        return Ok(false);
    }
    let shelf_choice = &shelves[selected_shelf_idx.unwrap()];

    let shelf_id = if shelf_choice.0 == "new" {
        print!("  Enter name for the new Shelf: ");
        std::io::stdout().flush().ok();
        let mut name = String::new();
        std::io::stdin().read_line(&mut name)?;
        let trimmed = name.trim();
        if trimmed.is_empty() {
            println!("  Shelf creation cancelled.");
            return Ok(false);
        }
        let new_sid = format!("shelf-{}", uuid::Uuid::new_v4());
        library.upsert_node(&new_sid, NodeKind::Shelf, trimmed, &[], None).await?;
        println!("  ✓ Created Shelf: {}", trimmed);
        new_sid
    } else {
        shelf_choice.0.clone()
    };

    // 2. Fetch existing books under this shelf (or all books if none)
    let books_query = if shelf_id != "none" {
        sqlx::query(
            "SELECT n.node_id, n.label FROM nodes n
             JOIN edges e ON e.source_node_id = n.node_id
             WHERE n.type = 'book' AND e.target_node_id = ? AND e.relation_type = 'sits_on'"
        ).bind(&shelf_id)
    } else {
        sqlx::query("SELECT node_id, label FROM nodes WHERE type = 'book'")
    };

    let books_rows = books_query.fetch_all(pool).await?;
    let mut books: Vec<(String, String)> = books_rows.into_iter()
        .map(|r| (r.get::<String, _>("node_id"), r.get::<String, _>("label")))
        .collect();

    books.push(("new".to_string(), "[Create New Book]".to_string()));
    books.push(("none".to_string(), "[No Book / Unsorted]".to_string()));

    println!("\n  Choose a Book for the new Page (↑/↓ to navigate, Enter to select, Esc to cancel):");
    let selected_book_idx = run_list_selector(&books)?;
    if selected_book_idx.is_none() {
        return Ok(false);
    }
    let book_choice = &books[selected_book_idx.unwrap()];

    let book_id = if book_choice.0 == "new" {
        print!("  Enter name for the new Book: ");
        std::io::stdout().flush().ok();
        let mut name = String::new();
        std::io::stdin().read_line(&mut name)?;
        let trimmed = name.trim();
        if trimmed.is_empty() {
            println!("  Book creation cancelled.");
            return Ok(false);
        }
        let new_bid = format!("book-{}", uuid::Uuid::new_v4());
        library.upsert_node(&new_bid, NodeKind::Book, trimmed, &[], None).await?;
        println!("  ✓ Created Book: {}", trimmed);
        
        // Link book to shelf if we have a shelf
        if shelf_id != "none" {
            library.link(&new_bid, &shelf_id, EdgeRelation::SitsOn, 1.0).await?;
        }
        new_bid
    } else {
        book_choice.0.clone()
    };

    // 3. Create the Page node and link it to the selected Book
    // First ensure page is created in session store
    state.store.create_page(new_page_id).await?;
    
    // Upsert the Page node in the library
    let plabel = format!("Conversation on page {}", short_id(new_page_id));
    library.upsert_node(new_page_id, NodeKind::Page, &plabel, &[], None).await?;

    // Link page to book
    if book_id != "none" {
        library.link(new_page_id, &book_id, EdgeRelation::BelongsTo, 1.0).await?;
    }

    Ok(true)
}

/// Helper to run a simple arrow-key list selector on a slice of (id, label).
fn run_list_selector(items: &[(String, String)]) -> anyhow::Result<Option<usize>> {
    use crossterm::{
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind},
        queue,
        terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
        cursor::MoveUp,
    };
    use std::io::Write;

    let mut stdout = std::io::stdout();
    let mut selected = 0usize;
    let n = items.len() as u16;

    if enable_raw_mode().is_err() {
        return Ok(None);
    }

    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _guard = RawGuard;

    let render = |stdout: &mut std::io::Stdout, selected: usize| -> std::io::Result<()> {
        use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
        for (i, (_, label)) in items.iter().enumerate() {
            if i == selected {
                queue!(
                    stdout,
                    SetForegroundColor(Color::Cyan),
                    SetAttribute(Attribute::Bold),
                    Print(format!("    ▸ {}\r\n", label)),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                )?;
            } else {
                queue!(stdout, Print(format!("      {}\r\n", label)))?;
            }
        }
        Ok(())
    };

    let _ = render(&mut stdout, selected);
    let _ = stdout.flush();

    loop {
        let ev = match read() {
            Ok(ev) => ev,
            Err(_) => return Ok(None),
        };

        if let Event::Key(KeyEvent { code, kind, .. }) = ev {
            if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            let mut advance = false;
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if selected == 0 {
                        selected = items.len() - 1;
                    } else {
                        selected -= 1;
                    }
                    advance = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1) % items.len();
                    advance = true;
                }
                KeyCode::Enter => {
                    let _ = queue!(stdout, MoveUp(n), Clear(ClearType::FromCursorDown));
                    let _ = stdout.flush();
                    return Ok(Some(selected));
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    let _ = queue!(stdout, MoveUp(n), Clear(ClearType::FromCursorDown));
                    let _ = stdout.flush();
                    return Ok(None);
                }
                _ => {}
            }

            if advance {
                let _ = queue!(stdout, MoveUp(n), Clear(ClearType::FromCursorDown));
                let _ = render(&mut stdout, selected);
                let _ = stdout.flush();
            }
        }
    }
}

/// Manually or automatically run LLM compaction on conversation history.
async fn run_compaction_pass(state: &mut ReplState) -> anyhow::Result<()> {
    let dim = ANSI_DIM;
    let reset = ANSI_RESET;
    println!("  {}◆ running compaction pass…{}", dim, reset);

    // 1. Load all messages on the active page
    let messages = state.store.load_recent(&state.page_id, 1000).await?;
    if messages.is_empty() {
        println!("    {}no active messages to compact.{}", dim, reset);
        return Ok(());
    }

    // 2. Format as a conversation log
    let mut log = String::new();
    for msg in &messages {
        let role_str = match msg.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        };
        log.push_str(&format!("{}: {}\n\n", role_str, msg.content));
    }

    // 3. Prompt the LLM to summarize the log dense and structured
    let prompt = format!(
        "You are the Hydragent compaction system. Your job is to compress the following conversation history into a highly dense, comprehensive, and structured summary. Focus on extracting key facts, user preferences, decisions made, and technical details. Do not lose important context. Keep the summary under 1000 words.\n\nConversation History:\n{}",
        log
    );

    let chat_msg = hydragent_model::openrouter::ChatMessage {
        role: "user".to_string(),
        content: prompt,
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    let drain = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });
    let (summary, _) = state.model_router.chat_stream(vec![chat_msg], tx, None).await?;
    let _ = drain.await;

    // 4. Update the page summary in SQLite
    state.store.update_page_summary(&state.page_id, &summary).await?;

    // 5. Truncate the messages table to 0 to clear active history
    state.store.truncate_page_messages(&state.page_id, 0).await?;

    // 6. Reset token counters and context_pct based on the summary size
    let summary_tokens = estimate_tokens(&summary);
    state.input_tokens = summary_tokens + 1500;
    state.output_tokens = 0;
    const CONTEXT_LIMIT: u64 = 100_000;
    state.context_pct = (state.input_tokens * 100 / CONTEXT_LIMIT).min(100) as u8;

    println!("  {}✓ compaction complete. summary size: {} tokens. active history cleared.{}", dim, summary_tokens, reset);
    Ok(())
}

/// Simple proxy to estimate tokens: 1 token ≈ 4 characters for English text.
fn estimate_tokens(text: &str) -> u64 {
    (text.len() / 4).max(1) as u64
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
