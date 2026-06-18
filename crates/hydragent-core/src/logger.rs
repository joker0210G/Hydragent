use std::fs;
use std::io;
use std::path::Path;

use tracing_subscriber::{fmt, prelude::*, Registry, EnvFilter};

/// Initialize the logging system based on format + level.
///
/// - `format`   = "json" or "terminal"
/// - `level`    = one of "off", "error", "warn", "info", "debug", "trace"
/// - `log_file` = optional path; if set, ALL levels go there in JSON.
///                Otherwise everything goes to stderr at the requested level.
///
/// In `hydragent chat` we pass `log_file = data_dir/logs/chat.jsonl` so
/// the interactive terminal stays clean — only `error` and above are
/// mirrored to stderr, full detail is in the file. The default
/// `HYDRAGENT_CHAT_LOG` is `error`; set it to `warn`/`info`/`debug`
/// to bring that level back to the terminal.
pub fn init_logger(format: &str, level: &str, log_file: Option<&Path>) {
    // ── Always install a file-side subscriber if requested ────────────
    if let Some(path) = log_file {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // The file captures everything (debug+) regardless of screen level.
        let file_filter = EnvFilter::new("debug");
        let file_path = path.to_path_buf();
        let file_layer = fmt::layer()
            .json()
            .with_writer(move || match fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
            {
                Ok(f) => Box::new(f) as Box<dyn io::Write + Send>,
                Err(_) => Box::new(io::sink()),
            })
            .with_filter(file_filter);

        let registry = Registry::default().with(file_layer);

        // The terminal layer is optional — callers can suppress it by
        // passing level = "off" (used in REPL mode to silence stderr).
        if !level.eq_ignore_ascii_case("off") {
            let stderr_filter = EnvFilter::new(level);
            let stderr_layer = fmt::layer()
                .with_writer(io::stderr)
                .with_ansi(true)
                .with_filter(stderr_filter);
            let registry = registry.with(stderr_layer);
            tracing::subscriber::set_global_default(registry)
                .expect("Failed to set logging subscriber");
        } else {
            tracing::subscriber::set_global_default(registry)
                .expect("Failed to set logging subscriber");
        }
        return;
    }

    // ── Plain stderr-only mode (default) ──────────────────────────────
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    if format.eq_ignore_ascii_case("json") {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt::layer().json());
        tracing::subscriber::set_global_default(subscriber)
            .expect("Failed to set logging subscriber");
    } else {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt::layer().with_ansi(true));
        tracing::subscriber::set_global_default(subscriber)
            .expect("Failed to set logging subscriber");
    }
}
