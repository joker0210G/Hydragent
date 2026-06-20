// crates/hydragent-core/src/onboard.rs
//
// `hydragent onboard` — guided first-time setup.
//
// Walks a new user through the minimum-viable configuration:
//   1. Choose a provider (OpenAI / OpenRouter / Together / Groq / Ollama / custom).
//      This menu is **arrow-key navigable** on a TTY (↑/↓/Enter, or
//      type a digit to quick-select, or 'q' to cancel). On non-TTY
//      input (piped/CI) it falls back to the old "type a number" prompt.
//   2. Paste an API key (or skip for local providers)
//   3. Pick a model (sensible default per provider, overridable)
//   4. Write `.env` (preserves any existing keys the user wants to keep)
//   5. Optionally verify the brain speaks with `test-brain`
//   6. Print "what to do next"
//
// Non-interactive flags:
//   --provider <name>      skip the picker
//   --api-key <key>        skip the key prompt (don't commit secrets in CI!)
//   --model <name>         skip the model prompt
//   --non-interactive      same as supplying all of the above
//   --no-verify            skip the live `test-brain` call
//   --force                overwrite existing `.env` (default: update in place)

use std::io::{BufRead, Write};
use std::path::PathBuf;

/// True when the current stdin/stdout is a real terminal (i.e. the user
/// is sitting in front of a shell). False when stdin has been piped or
/// redirected, e.g. `echo 1 | hydragent onboard` or a CI runner.
fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Provider preset: a name + default base URL + a list of recommended
/// model choices. This is the catalogue the wizard offers.
#[derive(Debug, Clone)]
struct Provider {
    label: &'static str,
    base: &'static str,
    needs_key: bool,
    recommended: &'static [&'static str],
}

/// Built-in presets. Matches the comment block at the top of `.env.example`.
const PRESETS: &[Provider] = &[
    Provider {
        label: "OpenRouter (recommended for getting started — many free models)",
        base: "https://openrouter.ai/api/v1",
        needs_key: true,
        recommended: &[
            "openai/gpt-4o-mini",
            "anthropic/claude-3.5-sonnet",
            "meta-llama/llama-3.1-70b-instruct",
            "google/gemini-2.0-flash-exp:free",
        ],
    },
    Provider {
        label: "OpenAI (gpt-4o, gpt-4o-mini, o1-mini…)",
        base: "https://api.openai.com/v1",
        needs_key: true,
        recommended: &[
            "gpt-4o-mini",
            "gpt-4o",
            "o1-mini",
            "gpt-3.5-turbo",
        ],
    },
    Provider {
        label: "Together AI (Llama, Mixtral, Qwen — generous free tier)",
        base: "https://api.together.xyz/v1",
        needs_key: true,
        recommended: &[
            "meta-llama/Llama-3-70b-chat-hf",
            "mistralai/Mixtral-8x7B-Instruct-v0.1",
            "Qwen/Qwen2.5-72B-Instruct-Turbo",
        ],
    },
    Provider {
        label: "Groq (very fast inference, free tier)",
        base: "https://api.groq.com/openai/v1",
        needs_key: true,
        recommended: &[
            "llama-3.1-70b-versatile",
            "llama-3.1-8b-instant",
            "mixtral-8x7b-32768",
        ],
    },
    Provider {
        label: "Ollama (local, no key needed — must be running on the same machine)",
        base: "http://localhost:11434/v1",
        needs_key: false,
        recommended: &[
            "llama3.1",
            "qwen2.5",
            "mistral",
            "gemma2",
        ],
    },
    Provider {
        label: "LM Studio (local, OpenAI-compat server)",
        base: "http://localhost:1234/v1",
        needs_key: false,
        recommended: &["local-model"],
    },
    Provider {
        label: "Custom (paste your own OpenAI-compatible base URL)",
        base: "",
        needs_key: true,
        recommended: &[],
    },
];

/// Top-level entry. Returns the process exit code.
pub fn run(opts: OnboardOptions) -> i32 {
    // ── 1. Welcome ────────────────────────────────────────────────────
    if !opts.non_interactive {
        println!();
        println!("------------------------------------------------------------------------");
        println!("  🐉 Welcome to Hydragent — first-time setup");
        println!("------------------------------------------------------------------------");
        println!("  I'll help you create a `.env` file. You can use any");
        println!("  OpenAI-compatible endpoint (OpenRouter, vLLM, Ollama, LM Studio,");
        println!("  or a custom URL). Press Ctrl-C at any time to abort.");
        println!("------------------------------------------------------------------------");
        println!();
    }

    // ── 2. Pick provider ──────────────────────────────────────────────
    let provider = match &opts.provider {
        Some(name) => match find_preset(name) {
            Some(p) => p.clone(),
            None => {
                eprintln!(
                    "✗ Unknown provider: '{}'. Valid: openai, openrouter, together, groq, ollama, lmstudio, custom",
                    name
                );
                return 2;
            }
        },
        None => {
            // When --base-url is passed without --provider, auto-select
            // "custom" so the user doesn't have to type it.
            if opts.base_url.is_some() {
                PRESETS.last().unwrap().clone()
            } else {
                match pick_provider() {
                    Some(p) => p,
                    None => {
                        eprintln!("✗ Setup aborted.");
                        return 1;
                    }
                }
            }
        }
    };

    let provider_label = provider.label;
    let base = if let Some(url) = &opts.base_url {
        // Explicit --base-url overrides everything (preset or custom).
        url.trim().trim_end_matches('/').to_string()
    } else if let Some(name) = &opts.provider {
        // If the user passed a raw URL as --provider, use it directly.
        let n = name.trim().to_lowercase();
        if n.starts_with("http://") || n.starts_with("https://") {
            name.trim().trim_end_matches('/').to_string()
        } else if provider.base.is_empty() {
            // Custom without --base-url: must prompt interactively.
            if opts.non_interactive {
                eprintln!("✗ `--provider custom` requires `--base-url` in non-interactive mode.");
                return 2;
            }
            match prompt("Custom base URL (e.g. https://api.together.xyz/v1):") {
                Some(s) => s.trim().trim_end_matches('/').to_string(),
                None => {
                    eprintln!("✗ Setup aborted.");
                    return 1;
                }
            }
        } else {
            provider.base.to_string()
        }
    } else if provider.base.is_empty() {
        // Interactive custom path (user picked Custom from menu).
        match prompt("Custom base URL (e.g. https://api.together.xyz/v1):") {
            Some(s) => s.trim().trim_end_matches('/').to_string(),
            None => {
                eprintln!("✗ Setup aborted.");
                return 1;
            }
        }
    } else {
        provider.base.to_string()
    };

    // ── 3. API key ────────────────────────────────────────────────────
    let api_key = if !provider.needs_key {
        String::new()
    } else if let Some(k) = &opts.api_key {
        k.clone()
    } else {
        if opts.non_interactive {
            eprintln!(
                "✗ Provider '{}' needs an API key. Pass --api-key <KEY>.",
                provider_label
            );
            return 2;
        }
        match prompt_secret(&format!("API key for {}:", provider_label)) {
            Some(k) => k,
            None => {
                eprintln!("✗ Setup aborted.");
                return 1;
            }
        }
    };

    // ── 4. Model ──────────────────────────────────────────────────────
    let model = if let Some(m) = &opts.model {
        m.clone()
    } else if !provider.recommended.is_empty() {
        if opts.non_interactive {
            provider.recommended[0].to_string()
        } else {
            let m = pick_model(&provider);
            if m.is_empty() {
                eprintln!("✗ Setup aborted.");
                return 1;
            }
            m
        }
    } else if opts.non_interactive {
        eprintln!("✗ No model provided and no recommended default. Pass --model <NAME>.");
        return 2;
    } else {
        match prompt("Model name (e.g. gpt-4o-mini):") {
            Some(s) => s.trim().to_string(),
            None => {
                eprintln!("✗ Setup aborted.");
                return 1;
            }
        }
    };

    // ── 5. Write .env ─────────────────────────────────────────────────
    let env_path = std::env::current_dir()
        .map(|p| p.join(".env"))
        .unwrap_or_else(|_| PathBuf::from(".env"));
    if env_path.exists() && !opts.force {
        if opts.non_interactive {
            eprintln!(
                "✗ .env already exists at {}. Pass --force to overwrite, or move it aside.",
                env_path.display()
            );
            return 2;
        }
        match prompt_yes_no(
            &format!(".env already exists at {}. Update it in place? [y/N]", env_path.display()),
            false,
        ) {
            Some(true) => {}
            _ => {
                eprintln!("✗ Setup aborted; .env left untouched.");
                return 1;
            }
        }
    }

    let mut new_env = std::collections::BTreeMap::<String, String>::new();

    // Preserve every BRAIN_* and a few other chosen keys from the existing .env.
    if env_path.exists() {
        if let Ok(text) = std::fs::read_to_string(&env_path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let k = k.trim().to_string();
                    let v = v.trim().to_string();
                    // Only preserve these — everything else gets the new values.
                    if matches!(k.as_str(),
                        "BRAIN_FALLBACKS" | "DATA_DIR" | "LOG_LEVEL" | "LOG_FORMAT" |
                        "MAX_REACT_STEPS" | "BUS_PORT" |
                        "ENABLE_DREAMING" | "DREAMING_INTERVAL_SEC" |
                        "MAX_SEMANTIC_MEMORIES" | "WORKSPACE_DIR" |
                        "ENFORCE_SANDBOX" | "MEMORY_CONTEXT_TOKEN_LIMIT" |
                        "PAGE_COMPACTION_LIMIT" |
                        "TELEGRAM_BOT_TOKEN" | "TELEGRAM_ALLOWED_CHAT_IDS" | "TELEGRAM_WEBAPP_URL" |
                        "HYDRAGENT_VAULT_PASSPHRASE" |
                        "SEARXNG_BASE_URL" | "SEARXNG_MAX_RESULTS" | "SEARXNG_TIMEOUT_SECS" |
                        "SEARXNG_CATEGORIES" | "SEARXNG_LANGUAGE"
                    ) {
                        new_env.insert(k, v);
                    }
                }
            }
        }
    }

    // Now overlay the three values the wizard is responsible for.
    new_env.insert("BRAIN_BASE".to_string(), base.clone());
    new_env.insert("BRAIN_MODEL".to_string(), model.clone());
    if !api_key.is_empty() {
        new_env.insert("BRAIN_KEY".to_string(), api_key.clone());
    }
    // Suggest two fallbacks (empty fallback list is fine).
    if !new_env.contains_key("BRAIN_FALLBACKS") {
        new_env.insert("BRAIN_FALLBACKS".to_string(), String::new());
    }

    // Re-render the file.
    let rendered = render_env(&new_env);
    if let Err(e) = std::fs::write(&env_path, rendered) {
        eprintln!("✗ Failed to write .env: {}", e);
        return 1;
    }
    println!();
    println!("  ✓ Wrote .env at {}", env_path.display());

    // ── 6. Optionally verify ──────────────────────────────────────────
    if !opts.no_verify {
        if opts.non_interactive {
            println!("  → Run `hydragent test-brain \"hello\"` to verify the connection.");
        } else {
            match prompt_yes_no("Verify the connection now with `test-brain`? [Y/n]", true) {
                Some(true) => {
                    println!();
                    println!("  Spawning live brain test…");
                    println!();
                    let status = std::process::Command::new(std::env::current_exe().unwrap())
                        .arg("test-brain")
                        .arg("Reply with exactly the word PONG.")
                        .arg("--debug")
                        .status();
                    match status {
                        Ok(s) if s.success() => {
                            println!();
                            println!("  ✓ Brain is reachable. You're ready to chat.");
                        }
                        Ok(s) => {
                            println!();
                            println!("  ⚠ Brain test exited with code {:?}. The connection may be", s.code());
                            println!("    misconfigured. Try `hydragent doctor` to diagnose.");
                        }
                        Err(e) => {
                            println!();
                            println!("  ⚠ Could not spawn `test-brain`: {}", e);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ── 7. Next steps ─────────────────────────────────────────────────
    println!();
    println!("------------------------------------------------------------------------");
    println!("  Next steps");
    println!("------------------------------------------------------------------------");
    println!("    hydragent chat         Start an interactive chat (recommended)");
    println!("    hydragent test-brain   Send a single prompt and stream the reply");
    println!("    hydragent --list-sessions  Show past chat sessions");
    println!("    hydragent doctor       Re-run diagnostics if something breaks");
    println!("    hydragent security status  Inspect the Phase 6 security surface");
    println!("    hydragent vault init   Optional: encrypt your API keys in a vault");
    println!();
    println!("  Tip: append a model fallback list to .env as");
    println!("       BRAIN_FALLBACKS=model-a,model-b");
    println!("       and the router will try them in order if the primary errors out.");
    println!("------------------------------------------------------------------------");

    0
}

#[derive(Debug, Clone)]
pub struct OnboardOptions {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub non_interactive: bool,
    pub no_verify: bool,
    pub force: bool,
    pub base_url: Option<String>,
}

// ── helpers ────────────────────────────────────────────────────────────

fn find_preset(name: &str) -> Option<&'static Provider> {
    let n = name.trim().to_lowercase();
    // If the "name" is actually a full URL, treat it as Custom.
    if n.starts_with("http://") || n.starts_with("https://") {
        return PRESETS.iter().find(|p| p.label.starts_with("Custom"));
    }
    let key = match n.as_str() {
        "openai" | "oai" => "OpenAI",
        "openrouter" | "or" => "OpenRouter",
        "together" | "together-ai" | "together.ai" => "Together",
        "groq" => "Groq",
        "ollama" | "local-ollama" => "Ollama",
        "lmstudio" | "lm-studio" | "lm_studio" => "LMStudio",
        "custom" => "Custom",
        _ => return None,
    };
    PRESETS.iter().find(|p| p.label.starts_with(key))
}

fn render_env(values: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str("# Generated by `hydragent onboard` on ");
    out.push_str(&chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string());
    out.push('\n');
    out.push_str("# Edit BRAIN_* below to point at a different provider.\n\n");

    // Group keys in a stable order.
    let order: &[&str] = &[
        "BRAIN_BASE", "BRAIN_KEY", "BRAIN_MODEL", "BRAIN_FALLBACKS",
        "DATA_DIR", "LOG_LEVEL", "LOG_FORMAT", "MAX_REACT_STEPS", "BUS_PORT",
        "ENABLE_DREAMING", "DREAMING_INTERVAL_SEC", "MAX_SEMANTIC_MEMORIES",
        "WORKSPACE_DIR", "ENFORCE_SANDBOX", "MEMORY_CONTEXT_TOKEN_LIMIT",
        "PAGE_COMPACTION_LIMIT",
        "TELEGRAM_BOT_TOKEN", "TELEGRAM_ALLOWED_CHAT_IDS", "TELEGRAM_WEBAPP_URL",
        "HYDRAGENT_VAULT_PASSPHRASE",
        "SEARXNG_BASE_URL", "SEARXNG_MAX_RESULTS", "SEARXNG_TIMEOUT_SECS",
        "SEARXNG_CATEGORIES", "SEARXNG_LANGUAGE",
    ];
    for k in order {
        if let Some(v) = values.get(*k) {
            out.push_str(k);
            out.push('=');
            out.push_str(&quote_value(v));
            out.push('\n');
        }
    }
    // Anything else, alphabetically.
    for (k, v) in values {
        if !order.contains(&k.as_str()) {
            out.push_str(k);
            out.push('=');
            out.push_str(&quote_value(v));
            out.push('\n');
        }
    }
    out
}

fn quote_value(v: &str) -> String {
    if v.contains(' ') || v.contains('#') || v.contains('"') {
        let escaped = v.replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        v.to_string()
    }
}

fn pick_provider() -> Option<Provider> {
    println!("  Choose a provider (↑/↓ to move, Enter to select, q to quit):");
    let labels: Vec<&str> = PRESETS.iter().map(|p| p.label).collect();
    let idx = select(&labels, None)?;
    Some(PRESETS[idx].clone())
}

fn pick_model(provider: &Provider) -> String {
    println!();
    println!(
        "  Pick a primary model for {} (↑/↓ to move, Enter to select):",
        provider
            .label
            .split_whitespace()
            .next()
            .unwrap_or("provider")
    );
    let mut labels: Vec<&str> = provider.recommended.to_vec();
    let custom_idx = labels.len();
    labels.push("custom — type your own");
    let idx = match select(&labels, Some(("c", custom_idx))) {
        Some(i) => i,
        None => return String::new(), // user aborted; caller treats "" as "abort"
    };
    if idx == custom_idx {
        match prompt("  Model name:") {
            Some(s) => s.trim().to_string(),
            None => String::new(),
        }
    } else {
        provider.recommended[idx].to_string()
    }
}

// ── interactive arrow-key picker ──────────────────────────────────────
//
// Cross-platform (Windows + Unix) raw-mode menu that lets the user
// navigate a list with ↑/↓ (or j/k or number keys) and press Enter to
// select. Also accepts 'q' / Esc to cancel, and a one-letter shortcut
// for the "custom" slot if one is provided.
//
// Falls back to a plain line-based prompt on non-TTY stdin/stdout so
// piped input (`echo 1 | hydragent onboard`) keeps working, and so
// `cargo test` or CI doesn't hang waiting for a keypress.
//
// Returns the 0-based index, or `None` if the user cancelled.
fn select(labels: &[&str], custom: Option<(&str, usize)>) -> Option<usize> {
    if labels.is_empty() {
        return None;
    }

    // Non-TTY fallback: ask for a number (or the custom key).
    if !is_tty() {
        return numeric_fallback(labels, custom);
    }

    use crossterm::{
        cursor::MoveUp,
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        queue,
        terminal::{enable_raw_mode, Clear, ClearType},
    };

    // The caller has printed a header. We're about to print the
    // option list and a footer; the loop will redraw this block on
    // every keypress by moving up and clearing.
    let mut stdout = std::io::stdout();
    let n = labels.len() as u16;
    let mut selected: usize = 0;

    // Enable raw mode; restore on any early return via the guard below.
    if let Err(e) = enable_raw_mode() {
        eprintln!(
            "(arrow-key picker unavailable: {} — falling back to numbers)",
            e
        );
        return numeric_fallback(labels, custom);
    }

    // We need a guard so any panic or early return restores the
    // terminal to a sane state (raw mode disabled, cursor visible).
    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        }
    }
    let _guard = RawGuard;

    // First render: draw the options + footer once, then enter the
    // event loop. Cursor ends on the line *after* the footer.
    let _ = render_block(&mut stdout, labels, selected);
    let _ = stdout.flush();

    loop {
        // Read one key first so we only redraw when the user
        // actually changes the selection.
        let ev = match read() {
            Ok(ev) => ev,
            Err(_) => return None,
        };
        let Event::Key(KeyEvent {
            code, modifiers, kind, ..
        }) = ev
        else {
            continue;
        };
        if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        if modifiers.contains(KeyModifiers::CONTROL)
            || modifiers.contains(KeyModifiers::ALT)
        {
            continue;
        }
        // `advance` is true when the key changed the selection and we
        // need to redraw. The match is "action" oriented: every arm
        // either returns, mutates `selected` (sets advance=true), or
        // does nothing (advance=false).
        let mut advance = false;
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if selected == 0 {
                    selected = labels.len() - 1;
                } else {
                    selected -= 1;
                }
                advance = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1) % labels.len();
                advance = true;
            }
            KeyCode::Home => {
                selected = 0;
                advance = true;
            }
            KeyCode::End => {
                selected = labels.len() - 1;
                advance = true;
            }
            KeyCode::Enter => return Some(selected),
            KeyCode::Esc | KeyCode::Char('q') => return None,
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(d) = c.to_digit(10) {
                    if (1..=labels.len() as u32).contains(&d) {
                        return Some((d - 1) as usize);
                    }
                }
                // Unrecognised digit — ignore, don't redraw.
            }
            KeyCode::Char(c) => {
                if let Some((key, idx)) = custom {
                    if c.eq_ignore_ascii_case(&key.chars().next().unwrap_or('?')) {
                        return Some(idx);
                    }
                }
                // Unrecognised char — ignore, don't redraw.
            }
            _ => {}
        }
        if advance {
            // Redraw: move up `n + 1` lines (n options + 1 footer),
            // clear from the first option down, and re-draw.
            let _ = queue!(
                stdout,
                MoveUp(n + 1),
                Clear(ClearType::FromCursorDown),
            );
            let _ = render_block(&mut stdout, labels, selected);
            let _ = stdout.flush();
        }
    }
}

// Draw the option block (n options + footer) starting at the current
// cursor position. Each option is one line; the footer is one line.
fn render_block(
    stdout: &mut std::io::Stdout,
    labels: &[&str],
    selected: usize,
) -> std::io::Result<()> {
    use crossterm::{
        queue,
        style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    };
    for (i, label) in labels.iter().enumerate() {
        if i == selected {
            queue!(
                stdout,
                SetForegroundColor(Color::Cyan),
                SetAttribute(Attribute::Bold),
                Print(format!("  ▸ [{}] {}\r\n", i + 1, label)),
                ResetColor,
                SetAttribute(Attribute::Reset),
            )?;
        } else {
            queue!(stdout, Print(format!("    [{}] {}\r\n", i + 1, label)))?;
        }
    }
    queue!(
        stdout,
        Print("    ↑/↓ to move, Enter to select\r\n"),
    )?;
    Ok(())
}

// Fallback path: read a line from stdin and parse it as a number.
// Used when stdin/stdout is not a TTY (CI, scripts, redirected I/O).
fn numeric_fallback(labels: &[&str], custom: Option<(&str, usize)>) -> Option<usize> {
    loop {
        let q = match custom {
            Some((key, _)) => format!(
                "  Number (1-{}, or '{}' for custom, 'q' to quit):",
                labels.len(),
                key
            ),
            None => format!("  Number (1-{}, or 'q' to quit):", labels.len()),
        };
        let raw = prompt(&q)?;
        let trimmed = raw.trim().to_lowercase();
        if trimmed == "q" || trimmed == "quit" {
            return None;
        }
        if let Some((key, idx)) = custom {
            if trimmed == key.to_lowercase() {
                return Some(idx);
            }
        }
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= labels.len() {
                return Some(n - 1);
            }
        }
        eprintln!(
            "  Please enter a number between 1 and {}, or 'q'.",
            labels.len()
        );
    }
}

fn prompt(question: &str) -> Option<String> {
    let mut stdout = std::io::stdout();
    write!(stdout, "{} ", question).ok()?;
    stdout.flush().ok()?;
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok()?;
    let s = line.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn prompt_yes_no(question: &str, default_yes: bool) -> Option<bool> {
    // On a TTY we delegate to the arrow-key picker so the user can
    // press ←/→ to flip the answer and Enter to confirm. On non-TTY
    // input (CI, scripts) we fall back to a plain "y/N" line read.
    if is_tty() {
        return prompt_yes_no_tty(question, default_yes);
    }
    let hint = if default_yes { "Y/n" } else { "y/N" };
    let raw = prompt(&format!("{} [{}]", question, hint))?;
    let t = raw.trim().to_lowercase();
    if t.is_empty() {
        return Some(default_yes);
    }
    if t == "y" || t == "yes" {
        Some(true)
    } else if t == "n" || t == "no" {
        Some(false)
    } else {
        Some(default_yes)
    }
}

// TTY-only yes/no picker. Renders "<question> ▸ Yes / No" on a
// single line; the user can press ←/→ to switch, Enter to confirm,
// or y/n as a shortcut. The current selection is shown in bold
// cyan with a leading ▸ arrow.
fn prompt_yes_no_tty(question: &str, default_yes: bool) -> Option<bool> {
    use crossterm::{
        cursor::MoveToColumn,
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        queue,
        style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
        terminal::{enable_raw_mode, Clear, ClearType},
    };

    let mut stdout = std::io::stdout();
    if let Err(_) = enable_raw_mode() {
        // No raw mode? fall back to the typed prompt.
        let raw = prompt(&format!("{} [Y/n]", question))?;
        let t = raw.trim().to_lowercase();
        return Some(match t.as_str() {
            "" => default_yes,
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default_yes,
        });
    }
    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        }
    }
    let _guard = RawGuard;

    let mut yes_selected: bool = default_yes;
    loop {
        // Render the single line:  "<question>  [▸ Yes] / [  No]  (←/→, Enter)".
        let _ = queue!(stdout, Clear(ClearType::FromCursorDown));
        let (yes_str, no_str) = if yes_selected {
            (
                format!(
                    "{}{} ▸ Yes {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::Cyan),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
                format!(
                    "{}{}   No {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::DarkGrey),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
            )
        } else {
            (
                format!(
                    "{}{}   Yes {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::DarkGrey),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
                format!(
                    "{}{} ▸ No {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::Cyan),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
            )
        };
        let _ = queue!(
            stdout,
            Print(format!("  {}  {}{}   (←/→, Enter)  ", question, yes_str, no_str)),
            MoveToColumn(0),
        );
        let _ = stdout.flush();

        let ev = match read() {
            Ok(ev) => ev,
            Err(_) => return None,
        };
        let Event::Key(KeyEvent {
            code, modifiers, kind, ..
        }) = ev
        else {
            continue;
        };
        if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        if modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT) {
            continue;
        }
        match code {
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                yes_selected = true;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') | KeyCode::Char('N') => {
                yes_selected = false;
            }
            KeyCode::Char(' ') | KeyCode::Tab => {
                yes_selected = !yes_selected;
            }
            KeyCode::Enter => {
                // Move past the prompt line so subsequent `println!`
                // calls don't overwrite the rendered answer.
                let _ = queue!(stdout, Print("\r\n"));
                let _ = stdout.flush();
                return Some(yes_selected);
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                let _ = queue!(stdout, Print("\r\n"));
                let _ = stdout.flush();
                return None;
            }
            _ => {}
        }
    }
}

fn prompt_secret(question: &str) -> Option<String> {
    // Use rpassword for masked input. Falls back to plain stdin on
    // non-TTY contexts (e.g. CI or piped input).
    match rpassword::prompt_password(question) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(_) => prompt(question),
    }
}
