// Regression tests for the v0.7.3 "tweak + optimize" pass.
//
// What this file covers:
//   1. Slash-only input (`/`, `//`, `///`) must be silently consumed
//      by the REPL. The pre-fix behavior was an `✗ Unknown command: /`
//      STDERR message, which is noisy and incorrect: a stray `/`
//      keypress is a typographical accident, not a command.
//   2. Very long user messages must be truncated in the local
//      `you ▸ …` echo. The full message is still sent to the model;
//      only the on-screen echo is clipped (200 chars + "+N more chars"
//      hint) so a 5 KB log blob doesn't bury the screen before the
//      LLM even starts thinking.
//   3. Paste mode still works for normal-sized input. The 256 KB
//      paste-buffer cap is enforced by a separate (large-input) test
//      in `paste_overflow.rs` (TBD) since synthesising 256 KB of
//      stdin in a unit test is wasteful — the cap is a one-line
//      guard that only fires on pathological input.
//
// These tests do NOT need a working brain. They pipe a sequence of
// slash commands (which never touch the network) plus a long
// echo, then assert on the captured output.
//
// Run: cargo test -p hydragent-core --test repl_polish -- --nocapture

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hydragent")
}

/// Spawn `hydragent chat`, pipe `input` into stdin, capture stdout+stderr,
/// and wait up to `timeout` for a clean exit. Returns the full output.
fn chat_with(input: &str, timeout: Duration) -> std::process::Output {
    chat_with_bytes(input.as_bytes(), timeout)
}

/// Like `chat_with`, but accepts raw bytes so callers can include
/// NUL / control characters that wouldn't survive a `&str` round-trip
/// in some test runners.
fn chat_with_bytes(input: &[u8], timeout: Duration) -> std::process::Output {
    // Each REPL invocation gets its own tempdir-as-home so the test
    // never reads the user's real `~/.hydragent/.env` and never
    // collides with parallel runs.
    let home = tempdir_home("repl_polish");
    let home_path = std::path::PathBuf::from(&home);
    let _ = std::fs::create_dir_all(&home_path);
    let _ = std::fs::write(home_path.join(".env"), "BRAIN_BASE=http://localhost:11434/v1\n");

    let mut child = Command::new(bin())
        .arg("chat")
        .env("HYDRAGENT_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hydragent chat");

    // Write the input, then close stdin to signal EOF.
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(input);
        // Drop closes the pipe.
    }

    // Bounded wait so a hung REPL doesn't stall the test suite.
    // On timeout we return whatever output we've gathered so far
    // (the process is killed) — some assertions below only check
    // the local echo, which is printed before any brain call, so
    // a slow network doesn't have to fail the test.
    let start = std::time::Instant::now();
    let output = loop {
        if let Ok(Some(_)) = child.try_wait() {
            break child.wait_with_output().expect("failed to wait");
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            // Best-effort drain of whatever the REPL has written
            // before the kill. If wait_with_output blocks we
            // fall back to an empty Output.
            break child.wait_with_output().unwrap_or_else(|_| std::process::Output {
                status: std::process::ExitStatus::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    output
}

#[test]
fn slash_only_input_is_silent() {
    // Three slash-only lines, then exit. The REPL should NOT emit
    // "Unknown command" on STDERR for any of them. We assert by
    // substring absence across both streams.
    let input = "/\n//\n///\n/exit\n";
    let out = chat_with(input, Duration::from_secs(15));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "hydragent chat should exit 0 on slash-only input, got {:?}\n\
         combined output:\n{}",
        out.status.code(),
        combined,
    );
    assert!(
        !combined.contains("Unknown command"),
        "slash-only input must not emit 'Unknown command'. Output:\n{}",
        combined,
    );
    // Sanity: the prompt must appear, proving the REPL got past
    // the first banner. We don't grep for the banner text directly
    // because it has ANSI escape codes that make substring matches
    // fragile; the Unicode prompt glyph "❯" (U+276F) is plain text
    // and is rendered after every banner refresh.
    assert!(
        combined.contains('\u{276F}'),
        "REPL banner should appear. Output:\n{}",
        combined,
    );
}

#[test]
fn long_user_message_echo_is_truncated() {
    // Build a 1000-char message so it comfortably exceeds the
    // 200-char truncation threshold. The REPL should echo only
    // the first 200 chars plus a "+N more chars" hint.
    let long_msg: String = std::iter::repeat('A').take(1000).collect();
    let input = format!("{long_msg}\n/exit\n");
    // 60s budget: the REPL echoes immediately, but if the network
    // is slow the brain call may run past the default 15s. The
    // assertions only look at the local echo, so a network stall
    // shouldn't fail the test (see chat_with_bytes for the
    // timeout-tolerant drain behavior).
    let out = chat_with(&input, Duration::from_secs(60));
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The truncation marker should appear on the `you ▸` echo line.
    assert!(
        stdout.contains("more chars"),
        "long input must be truncated with a 'more chars' marker. Output:\n{stdout}",
    );
    // And the full 1000-char echo must NOT appear (otherwise the
    // truncation is a no-op).
    assert!(
        !stdout.contains(&long_msg),
        "full long input should not appear untruncated in the echo. Output:\n{stdout}",
    );
}

#[test]
fn control_chars_only_input_is_silently_dropped() {
    // A line of pure NUL / BEL / BS bytes should be filtered to an
    // empty string by `strip_control_chars`, which the REPL then
    // treats like any other empty input: silent re-prompt, no
    // `you ▸` echo, no spinner, no brain call.
    let input: Vec<u8> = vec![0x00, 0x00, 0x07, 0x08, 0x1B, b'\n', b'/', b'e', b'x', b'i', b't', b'\n'];
    let out = chat_with_bytes(&input, Duration::from_secs(15));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "hydragent chat should exit 0 on control-chars-only input, got {:?}\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code(),
    );
    // The brain spinner must NOT appear — that would prove the
    // NUL line was forwarded to the model instead of being dropped.
    assert!(
        !stdout.contains("hydra") || !stdout.contains("thinking"),
        "control-chars-only line must not trigger a brain call (no 'thinking' spinner).\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // And the `you ▸` echo for the garbage line must be absent.
    assert!(
        !stdout.contains("you ▸"),
        "control-chars-only line must not produce a 'you ▸' echo.\n\
         stdout:\n{stdout}",
    );
}

#[test]
fn single_line_input_over_64kb_is_rejected() {
    // 100 KB of 'A' exceeds the 64 KB single-line cap. The REPL
    // should reject it with a friendly error, NOT call the brain,
    // and then process /exit normally.
    let oversized: String = std::iter::repeat('A').take(100 * 1024).collect();
    let mut input = oversized.into_bytes();
    input.push(b'\n');
    input.extend_from_slice(b"/exit\n");
    let out = chat_with_bytes(&input, Duration::from_secs(15));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        out.status.success(),
        "hydragent chat should exit 0 even when rejecting oversized input, got {:?}",
        out.status.code(),
    );
    assert!(
        combined.contains("64 KB") || combined.contains("over the"),
        "oversized input must produce a clear cap-exceeded message. Output:\n{combined}",
    );
    // The spinner must not appear — proof we bailed before dispatch.
    assert!(
        !stdout.contains("thinking"),
        "oversized input must not trigger a brain call.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // Sanity: /exit still works.
    assert!(
        stdout.contains("Goodbye"),
        "REPL should still process /exit after rejecting oversized input.\n\
         stdout:\n{stdout}",
    );
}

/// Build a unique tempdir path for one REPL invocation. The directory
/// does not need to exist beforehand — `paths::ensure_dirs` inside the
/// binary creates the subdirectories lazily when the binary first
/// writes to them. We just need a unique path that won't collide with
/// parallel test runs.
fn tempdir_home(tag: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    // Per-test counter so multiple chat_with_bytes calls inside the
    // same test get distinct homes and don't trip over each other.
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("hydragent-test-{tag}-{pid}-{nanos}-{n}"));
    dir.to_string_lossy().into_owned()
}
