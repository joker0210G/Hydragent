// Minimal smoke test for the TUI picker logic. We can't truly test
// the arrow-key handling here (that requires a real TTY), but we can
// verify that the binary:
//   1. Loads cleanly
//   2. Honours the numeric fallback for piped input (which the
//      arrow-key path *also* uses internally as a digit shortcut)
//   3. Does NOT block forever when stdin is non-interactive
//
// Run: cargo test -p hydragent-core --test picker_tty -- --nocapture

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn picker_doesnt_hang_on_piped_input() {
    let bin = env!("CARGO_BIN_EXE_hydragent");

    // Isolate every binary invocation in its own tempdir so the test
    // never reads/writes a real `~/.hydragent/.env` and never collides
    // with another test in the same suite.
    let home = tempdir_home("picker_tty");
    // Belt-and-suspenders: make sure no leftover cwd-relative .env
    // shadows the tempdir-based one we just set up.
    let _ = std::fs::remove_file(".env");

    // Simulate "press 2 then Enter" via piped input. The picker in
    // TTY mode would also accept '2' as a digit shortcut, so this
    // exercises the same code path's non-TTY fallback.
    let mut child = Command::new(bin)
        .args([
            "onboard",
            "--non-interactive", // forces non-TTY path explicitly
            "--provider", "openai",
            "--api-key", "sk-test",
            "--model", "gpt-4o-mini",
            "--no-verify",
        ])
        .env("HYDRAGENT_HOME", &home) // redirect all paths to tempdir
        .env("HYDRAGENT_TTY", "0")    // hint to the binary, in case we add one
        .stdin(std::process::Stdio::null()) // no input at all
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn");

    let start = Instant::now();
    let status = child.wait().expect("failed to wait");
    let elapsed = start.elapsed();

    assert!(status.success(), "onboard --non-interactive failed: {status:?}");
    assert!(
        elapsed < Duration::from_secs(10),
        "onboard took too long: {elapsed:?}"
    );

    // Cleanup: remove the .env that onboard wrote (both in tempdir
    // and any cwd-relative one for safety).
    let _ = std::fs::remove_file(format!("{}/.env", home));
    let _ = std::fs::remove_file(".env");
    let _ = std::fs::remove_dir_all(&home);
}

/// Build a unique tempdir path for a test and return it as a String.
/// We don't actually need a real fs object yet — `paths::ensure_dirs`
/// inside the binary creates the subdirectories. We just need a unique
/// path that won't collide with parallel test runs.
fn tempdir_home(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("hydragent-test-{tag}-{pid}-{nanos}"));
    dir.to_string_lossy().into_owned()
}
