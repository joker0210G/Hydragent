// crates/hydragent-core/src/doctor.rs
//
// `hydragent doctor` — first-line diagnostic command.
//
// Runs a battery of file-based / config-based checks against the user's
// working directory and prints a colour-coded report. Goal: answer
// "why isn't this working?" in 5 seconds without forcing the user to
// read the codebase.
//
// Design notes:
//   * All checks are local & offline (no network). The brain reachability
//     check is offered as a *suggestion* ("run `hydragent test-brain`"),
//     not a built-in check, so we don't need reqwest here.
//   * Output uses three levels:
//       ✓ OK       — green check
//       ⚠ WARN     — yellow triangle (works, but suboptimal)
//       ✗ FAIL     — red X (something is broken)
//     Each line is followed by a one-line fix hint, not a paragraph.
//   * Exit code is non-zero only if any check FAILs. WARN is exit 0.

use std::path::PathBuf;

/// Severity of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Fail,
}

/// A single diagnostic result.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub severity: Severity,
    pub detail: String,
    /// One-line hint shown only for WARN/FAIL.
    pub hint: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self { name: name.to_string(), severity: Severity::Ok, detail: detail.into(), hint: None }
    }
    fn warn(name: &str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { name: name.to_string(), severity: Severity::Warn, detail: detail.into(), hint: Some(hint.into()) }
    }
    fn fail(name: &str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { name: name.to_string(), severity: Severity::Fail, detail: detail.into(), hint: Some(hint.into()) }
    }
}

/// All checks together.
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn has_failures(&self) -> bool {
        self.checks.iter().any(|c| c.severity == Severity::Fail)
    }
    pub fn warn_count(&self) -> usize {
        self.checks.iter().filter(|c| c.severity == Severity::Warn).count()
    }
    pub fn fail_count(&self) -> usize {
        self.checks.iter().filter(|c| c.severity == Severity::Fail).count()
    }
    pub fn ok_count(&self) -> usize {
        self.checks.iter().filter(|c| c.severity == Severity::Ok).count()
    }

    /// Print a colour-coded report to stdout. Uses ANSI escapes — these
    /// are no-ops on Windows legacy console, but modern Windows Terminal
    /// (the default since 2019) renders them correctly.
    pub fn print(&self) {
        println!("------------------------------------------------------------------------");
        println!("  🩺 Hydragent doctor — {} checks run", self.checks.len());
        println!("------------------------------------------------------------------------");
        for c in &self.checks {
            let (icon, colour) = match c.severity {
                Severity::Ok   => ("✓ ", "\x1b[32m"),  // green
                Severity::Warn => ("⚠ ", "\x1b[33m"),  // yellow
                Severity::Fail => ("✗ ", "\x1b[31m"),  // red
            };
            let reset = "\x1b[0m";
            println!("  {}{}{}  {:<24}  {}", colour, icon, reset, c.name, c.detail);
            if let Some(h) = &c.hint {
                println!("       {}└─ fix: {}{}", "\x1b[90m", h, reset);
            }
        }
        println!("------------------------------------------------------------------------");
        let ok = self.ok_count();
        let warn = self.warn_count();
        let fail = self.fail_count();
        let summary = if fail > 0 {
            format!("\x1b[31m{} failed\x1b[0m, \x1b[33m{} warnings\x1b[0m, \x1b[32m{} ok\x1b[0m",
                fail, warn, ok)
        } else if warn > 0 {
            format!("\x1b[32m{} ok\x1b[0m, \x1b[33m{} warnings\x1b[0m", ok, warn)
        } else {
            format!("\x1b[32mall {} checks passed\x1b[0m", ok)
        };
        println!("  Summary: {}", summary);
        println!("------------------------------------------------------------------------");
        if fail > 0 {
            println!();
            println!("  Next step: `hydragent onboard` will fix the most common setup issues.");
            println!("             `hydragent test-brain \"hello\"` will verify your brain is wired up.");
        } else if warn > 0 {
            println!();
            println!("  Hydragent will run, but you may want to address the warnings above.");
        } else {
            println!();
            println!("  Ready to chat!  `hydragent chat`         — interactive REPL");
            println!("                  `hydragent test-brain`  — single-prompt smoke test");
        }
    }
}

/// Run every check. The `app_config` is the same `AppConfig` the rest
/// of the binary uses, so the doctor is consistent with the live runtime.
pub fn run(app_config: &crate::config::AppConfig) -> Report {
    let mut checks = Vec::new();

    // ── [1] .env file ─────────────────────────────────────────────────
    let env_path = std::env::current_dir()
        .map(|p| p.join(".env"))
        .unwrap_or_else(|_| PathBuf::from(".env"));
    match std::fs::metadata(&env_path) {
        Ok(md) => {
            checks.push(Check::ok(
                ".env file",
                format!("present ({} bytes, {})", md.len(), env_path.display()),
            ));
        }
        Err(_) => {
            checks.push(Check::fail(
                ".env file",
                format!("missing at {}", env_path.display()),
                "run `hydragent onboard` to create one, or `cp .env.example .env`",
            ));
        }
    }

    // ── [2] brain config ──────────────────────────────────────────────
    let brain_base = app_config.effective_brain_base();
    if brain_base.is_empty() {
        checks.push(Check::fail(
            "BRAIN_BASE",
            "unset (or OPENROUTER_API_KEYS not set)",
            "set BRAIN_BASE in `.env` — try `hydragent onboard`",
        ));
    } else {
        checks.push(Check::ok(
            "BRAIN_BASE",
            format!("{}", redact_url(&brain_base)),
        ));
    }

    let brain_key = app_config.effective_brain_key();
    if brain_key.is_empty() {
        // Local providers (Ollama, LM Studio) intentionally have no key.
        let is_local = brain_base.contains("localhost") || brain_base.contains("127.0.0.1");
        if is_local {
            checks.push(Check::ok(
                "BRAIN_KEY",
                "<empty> (local provider — OK)".to_string(),
            ));
        } else {
            checks.push(Check::fail(
                "BRAIN_KEY",
                "unset",
                "set BRAIN_KEY in `.env` (or use OPENROUTER_API_KEYS for back-compat)",
            ));
        }
    } else if brain_key.contains("your-key-here") || brain_key.contains("9b9c8f09436e") {
        checks.push(Check::fail(
            "BRAIN_KEY",
            "looks like the placeholder from .env.example",
            "paste your real key into `.env` (BRAIN_KEY=...)",
        ));
    } else {
        let head: String = brain_key.chars().take(4).collect();
        let n = brain_key.chars().count();
        checks.push(Check::ok(
            "BRAIN_KEY",
            format!("set ({}…  {} chars)", head, n),
        ));
    }

    let brain_model = app_config.effective_brain_model();
    if brain_model.is_empty() {
        checks.push(Check::fail(
            "BRAIN_MODEL",
            "unset",
            "set BRAIN_MODEL in `.env` (e.g. `openai/gpt-4o-mini` or `gpt-4o-mini`)",
        ));
    } else {
        checks.push(Check::ok("BRAIN_MODEL", brain_model.clone()));
    }

    // ── [3] data directory ────────────────────────────────────────────
    let data_dir = PathBuf::from(&app_config.data_dir);
    match std::fs::metadata(&data_dir) {
        Ok(md) if md.is_dir() => {
            // Try to create a probe file to check write access.
            let probe = data_dir.join(".doctor_probe");
            let writable = std::fs::write(&probe, b"ok").is_ok()
                && std::fs::remove_file(&probe).is_ok();
            if writable {
                checks.push(Check::ok(
                    "data_dir",
                    format!("writable at {}", data_dir.display()),
                ));
            } else {
                checks.push(Check::warn(
                    "data_dir",
                    format!("exists but not writable at {}", data_dir.display()),
                    "chmod +w the directory, or change DATA_DIR in `.env`",
                ));
            }
        }
        Ok(_) => {
            checks.push(Check::fail(
                "data_dir",
                format!("path exists but is not a directory: {}", data_dir.display()),
                "DATA_DIR in `.env` should point at a directory, not a file",
            ));
        }
        Err(_) => {
            // The agent creates this on first run, so missing is a WARN not a FAIL.
            checks.push(Check::warn(
                "data_dir",
                format!("does not exist yet ({} — will be created on first run)", data_dir.display()),
                "if you've never run hydragent, this is expected",
            ));
        }
    }

    // ── [4] vault ─────────────────────────────────────────────────────
    let vault_path = data_dir.join("vault/.hydravault");
    if vault_path.exists() {
        checks.push(Check::ok(
            "vault",
            format!("initialised at {}", vault_path.display()),
        ));
    } else {
        checks.push(Check::warn(
            "vault",
            "not initialised yet (optional but recommended for storing API keys)",
            "run `hydragent vault init` to create one (you'll be prompted for a passphrase)",
        ));
    }

    // ── [5] audit chain ───────────────────────────────────────────────
    let audit_path = data_dir.join("audit");
    if audit_path.exists() {
        checks.push(Check::ok(
            "audit_dir",
            format!("present at {}", audit_path.display()),
        ));
    } else {
        checks.push(Check::warn(
            "audit_dir",
            "not present yet (created on first audit event)",
            "no action needed — appears when hydragent starts logging",
        ));
    }

    // ── [6] sandbox WASM tools ────────────────────────────────────────
    let sandbox_dir = PathBuf::from("./sandbox/tools");
    let echo_wasm = sandbox_dir.join("echo.wasm");
    let file_read_wasm = sandbox_dir.join("file_read.wasm");
    if echo_wasm.exists() {
        checks.push(Check::ok("sandbox/echo.wasm", "present"));
    } else {
        let enforce = std::env::var("ENFORCE_SANDBOX")
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if enforce {
            checks.push(Check::fail(
                "sandbox/echo.wasm",
                "missing but ENFORCE_SANDBOX=true",
                "build the WASM toolchain: see sandbox/build.ps1 / sandbox/echo/",
            ));
        } else {
            checks.push(Check::warn(
                "sandbox/echo.wasm",
                "missing (native fallback in use)",
                "set ENFORCE_SANDBOX=true to require WASM, or build the WASM toolchain",
            ));
        }
    }
    if file_read_wasm.exists() {
        checks.push(Check::ok("sandbox/file_read.wasm", "present"));
    } else {
        let enforce = std::env::var("ENFORCE_SANDBOX")
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if enforce {
            checks.push(Check::fail(
                "sandbox/file_read.wasm",
                "missing but ENFORCE_SANDBOX=true",
                "build the WASM toolchain: see sandbox/build.ps1 / sandbox/file_read/",
            ));
        } else {
            checks.push(Check::warn(
                "sandbox/file_read.wasm",
                "missing (native fallback in use)",
                "set ENFORCE_SANDBOX=true to require WASM, or build the WASM toolchain",
            ));
        }
    }

    // ── [7] log level ─────────────────────────────────────────────────
    let log_level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| app_config.log_level.clone());
    if log_level == "debug" {
        checks.push(Check::warn(
            "LOG_LEVEL",
            "debug (very chatty — useful for diagnosis only)",
            "set LOG_LEVEL=info (or warn) once you've finished debugging",
        ));
    } else {
        checks.push(Check::ok("LOG_LEVEL", log_level));
    }

    // ── [8] RUST_LOG presence ─────────────────────────────────────────
    match std::env::var("RUST_LOG") {
        Ok(v) => checks.push(Check::ok("RUST_LOG", v)),
        Err(_) => checks.push(Check::ok("RUST_LOG", "<unset — using LOG_LEVEL>")),
    }

    // ── [9] optional: Telegram adapter ────────────────────────────────
    let tg_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    let tg_chats = std::env::var("TELEGRAM_ALLOWED_CHAT_IDS").ok();
    match (tg_token, tg_chats) {
        (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => {
            checks.push(Check::ok("telegram", format!("enabled (chats: {})", c)));
        }
        (Some(_), _) | (_, Some(_)) => {
            checks.push(Check::warn(
                "telegram",
                "partially configured (need both TELEGRAM_BOT_TOKEN and TELEGRAM_ALLOWED_CHAT_IDS)",
                "see adapters/telegram_adapter.py",
            ));
        }
        _ => {
            checks.push(Check::ok(
                "telegram",
                "<disabled> (no TELEGRAM_BOT_TOKEN — that's fine)",
            ));
        }
    }

    // ── [10] bus port free? ───────────────────────────────────────────
    let port = app_config.bus_port;
    // Try a TCP connect — non-blocking hint.
    match std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(200),
    ) {
        Ok(_) => {
            checks.push(Check::warn(
                "bus_port",
                format!("port {} is already in use (another hydragent is running)", port),
                "stop the other instance, or set BUS_PORT=… in `.env`",
            ));
        }
        Err(_) => {
            checks.push(Check::ok(
                "bus_port",
                format!("{} is free", port),
            ));
        }
    }

    Report { checks }
}

/// Redact a URL to show its origin + first path segment only.
/// E.g. `https://api.openai.com/v1` → `https://api.openai.com/...`
fn redact_url(url: &str) -> String {
    if let Some(idx) = url.find("://") {
        let after = &url[idx + 3..];
        if let Some(slash) = after.find('/') {
            return format!("{}://{}/…", &url[..idx], &after[..slash]);
        }
    }
    url.to_string()
}
