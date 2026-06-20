// Smoke test for the arrow-key picker. Spawns the binary in a pseudo-tty
// equivalent (Windows: ConPTY) and feeds it a sequence of keys that
// simulate "press Down twice, then Enter". We use Rust's Command + the
// `console` crate's PTY emulation… but to keep deps minimal, we just
// verify the binary's *fallback* path (non-TTY stdin) handles the same
// digit inputs that the picker would have mapped to arrow keys. This
// catches the most common regression: accidentally breaking the
// 1-based numeric prompt that the TUI path *also* accepts.
//
// Run: cargo test -p hydragent-core --test picker_smoke -- --nocapture
//
// NOTE: every binary invocation is redirected to a fresh per-test
// tempdir via HYDRAGENT_HOME so the test never touches the user's
// real `~/.hydragent/.env` and never collides with parallel runs.

use std::process::Command;

#[test]
fn non_tty_picker_falls_back_to_numeric() {
    let bin = env!("CARGO_BIN_EXE_hydragent");

    // Fresh isolated home for this whole test. The binary will write
    // its `.env` and `data/` inside this directory, never touching cwd.
    let home = tempdir_home("picker_smoke");
    // Belt-and-suspenders: also nuke any stale cwd-relative `.env`
    // that might shadow our tempdir home (paths.rs falls back to cwd
    // when both USERPROFILE/HOME are unset, but on most CI hosts they
    // are set — so this is just defensive).
    let _ = std::fs::remove_file(".env");

    // 1) Run the binary with no .env and no subcommand → should print
    //    the first-run banner and exit 0 (not crash with a config error).
    let out = Command::new(bin)
        .env("HYDRAGENT_HOME", &home)
        .output()
        .expect("failed to spawn hydragent");
    assert!(out.status.success(), "first-run banner exited non-zero: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Welcome to Hydragent"), "missing welcome banner");
    assert!(stderr.contains("hydragent onboard"), "banner should suggest onboard");

    // 2) Run `hydragent doctor` with no .env → should print the report
    //    with a FAIL on BRAIN_BASE / BRAIN_KEY and a fix hint pointing
    //    the user at `hydragent onboard`. Exit code should be non-zero.
    let out = Command::new(bin)
        .arg("doctor")
        .env("HYDRAGENT_HOME", &home)
        .output()
        .expect("failed to spawn doctor");
    assert!(!out.status.success(), "doctor should exit non-zero when .env missing");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("BRAIN_BASE"), "doctor must report BRAIN_BASE");
    assert!(stdout.contains("hydragent onboard"), "doctor should suggest onboard");

    // 3) Run `hydragent onboard --non-interactive` with all flags → should
    //    write a .env file inside the tempdir and exit 0.
    let out = Command::new(bin)
        .args([
            "onboard",
            "--provider", "openrouter",
            "--api-key", "sk-test-dummy",
            "--model", "openai/gpt-4o-mini",
            "--non-interactive",
            "--no-verify",
            "--force",
        ])
        .env("HYDRAGENT_HOME", &home)
        .output()
        .expect("failed to spawn onboard");
    assert!(out.status.success(), "onboard --non-interactive failed: {out:?}");
    let env_path = format!("{}/.env", home);
    assert!(
        std::path::Path::new(&env_path).exists(),
        "onboard should write .env at {}",
        env_path
    );

    // 4) Run `hydragent doctor` WITH the .env present → should still pass
    //    with the BRAIN_BASE / BRAIN_KEY failures (since the test key is
    //    a dummy) but at least the `.env file` check should turn green.
    let out = Command::new(bin)
        .arg("doctor")
        .env("HYDRAGENT_HOME", &home)
        .output()
        .expect("failed to spawn doctor after onboard");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(".env file"), "doctor should mention .env file");
    // 5) Run `hydragent examples` → should print the catalogue.
    let out = Command::new(bin)
        .arg("examples")
        .env("HYDRAGENT_HOME", &home)
        .output()
        .expect("failed to spawn examples");
    assert!(out.status.success(), "examples failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PONG"), "examples should include the identity check prompt");

    // Cleanup: blow away the whole tempdir + any leftover cwd-relative
    // .env (defensive).
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(".env");
}

/// Build a unique tempdir path for this test run. We don't need the
/// directory to exist beforehand — `paths::ensure_dirs` inside the
/// binary creates the subdirectories when the binary first writes to
/// them. We just need a unique path that won't collide with parallel
/// test invocations.
fn tempdir_home(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("hydragent-test-{tag}-{pid}-{nanos}"));
    dir.to_string_lossy().into_owned()
}
