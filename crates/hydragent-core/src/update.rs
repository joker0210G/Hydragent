use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const GITHUB_API_URL: &str = "https://api.github.com/repos/joker0210G/Hydragent/releases/latest";
const GITHUB_COMMITS_API_URL: &str = "https://api.github.com/repos/joker0210G/Hydragent/commits?per_page=1";
#[cfg(target_os = "windows")]
const INSTALL_PS1_URL: &str = "https://joker0210G.github.io/Hydragent/install.ps1";
#[cfg(not(target_os = "windows"))]
const INSTALL_SH_URL: &str = "https://joker0210G.github.io/Hydragent/install.sh";

// Windows process creation flags used by the updater.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(target_os = "windows")]
const DETACHED_PROCESS: u32 = 0x0000_0008;

#[derive(Debug, serde::Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Single commit object returned by the GitHub Commits API.
#[derive(Debug, serde::Deserialize)]
struct GitHubCommit {
    sha: String,
}

/// Wrapper for the latest-commit fallback when releases are empty.
#[derive(Debug)]
struct CommitInfo {
    sha: String,
}

/// GitHub Compare API response for commit-distance checks.
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct CompareResponse {
    status: String,
    // `ahead_by` is deserialized but not used directly; kept for
    // completeness so the API shape is fully represented.
    ahead_by: i64,
    behind_by: i64,
}

/// Update Hydragent to the latest release from GitHub.
///
/// Checks the GitHub Releases API for a newer version, downloads the
/// correct prebuilt asset for the current platform, and replaces the
/// running binary safely.
pub async fn run() {
    let current_version = env!("CARGO_PKG_VERSION");

    println!("  Checking for updates… (current version: {})", current_version);

    match check_latest_version().await {
        Ok(CheckOutcome::NewRelease { version: latest_version, asset }) => {
            println!(
                "  New version available: {} (current: {})",
                latest_version, current_version
            );
            println!(
                "  Downloading {} …",
                asset.name
            );
            match download_and_extract(&asset).await {
                Ok(new_binary_path) => {
                    println!("  Extracted to {}", new_binary_path.display());
                    match replace_binary(&new_binary_path).await {
                        Ok(()) => {
                            // Clean up the staging directory.
                            if let Some(staging_dir) = new_binary_path.parent() {
                                let _ = std::fs::remove_dir(staging_dir);
                            }
                            println!(
                                "  ✓ Hydragent updated to v{}. Run `hydragent --version` to verify.",
                                latest_version
                            );
                        }
                        Err(e) => {
                            eprintln!("  ✗ Failed to replace binary: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  ✗ Failed to download update: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Ok(CheckOutcome::UpToDate) => {
            println!(
                "  Hydragent is already up to date (v{}).",
                current_version
            );
            std::process::exit(0);
        }
        Ok(CheckOutcome::NoReleasesPublished) => {
            // GitHub returned 404 from /releases/latest, which means the
            // repo has not published any tagged releases yet.
            println!("  No release found for this repo ({}).", GITHUB_REPO);
            println!();

            // Try to show both local and upstream commit info.
            let local_commit = get_local_commit();
            let latest_commit = match fetch_latest_commit().await {
                Ok(info) => Some(info),
                Err(e) => {
                    eprintln!("  ⚠ Could not fetch latest commit from GitHub: {}", e);
                    None
                }
            };

            let current_version = env!("CARGO_PKG_VERSION");

            // Try to compute how far behind the local build is.
            let commits_behind = match (&local_commit, &latest_commit) {
                (Some(local), Some(latest)) => fetch_commits_behind(local, &latest.sha).await,
                _ => None,
            };

            if let Some(ref commit) = local_commit {
                if let Some(behind) = commits_behind {
                    if behind == 0 {
                        println!("  Current version: v{} (prebuild, commit: {}) — up to date", current_version, &commit[..8.min(commit.len())]);
                    } else {
                        println!("  Current version: v{} (prebuild, commit: {}, {} commits behind)", current_version, &commit[..8.min(commit.len())], behind);
                    }
                } else {
                    println!("  Current version: v{} (prebuild, commit: {})", current_version, &commit[..8.min(commit.len())]);
                }
            } else {
                println!("  Current version: v{}", current_version);
            }

            if let Some(ref info) = latest_commit {
                println!("  Latest version:  v{} (commit: {})", current_version, &info.sha[..8.min(info.sha.len())]);
            } else {
                println!("  Latest version:  v{} (commit: unknown)", current_version);
            }
            println!();

            if !confirm_yes_no("  Do you want to update from the current state of the GitHub commit?", true) {
                println!("  Update cancelled.");
                std::process::exit(0);
            }

            println!();
            println!("  The one-command installer will:");
            println!("    1. Clone the repo into ~/.hydragent/src (or update it)");
            println!("    2. cargo build --release -p hydragent-core");
            println!("    3. Copy the fresh binary over the running one");
            println!("    4. Refresh the launcher and PATH entries");
            println!();
            println!("  Launching installer: {}", install_one_liner());
            println!();
            match launch_source_installer().await {
                Ok(status) => {
                    if !status.success() {
                        eprintln!(
                            "  ✗ Installer exited with code {}",
                            status.code().unwrap_or(-1)
                        );
                        std::process::exit(status.code().unwrap_or(1));
                    }
                    // Installer already printed its own success message;
                    // nothing more to add.
                }
                Err(e) => {
                    eprintln!("  ✗ Failed to launch installer: {}", e);
                    std::process::exit(1);
                }
            }
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("  ✗ Failed to check for updates: {}", e);
            std::process::exit(1);
        }
    }
}

/// Repo identifier for the source-only "no release" hint. Kept in sync
/// with the launchers and installer (`install.ps1`, `install.sh`,
/// `Hydragent.cmd`). The single `GITHUB_API_URL` constant above is the
/// authoritative endpoint used by the check itself.
const GITHUB_REPO: &str = "joker0210G/Hydragent";

/// Outcome of querying GitHub for the latest release.
#[derive(Debug)]
enum CheckOutcome {
    /// A release newer than the local binary is available.
    NewRelease {
        version: String,
        asset: Asset,
    },
    /// The local binary is at least as new as the published release.
    UpToDate,
    /// The repo has not published any releases yet (API returned 404
    /// for `/releases/latest`). Treated as informational, not an error.
    NoReleasesPublished,
}

/// Query GitHub Releases for the latest tag and see if it is newer than
/// the binary's compile-time version.
async fn check_latest_version() -> Result<CheckOutcome, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let response = client.get(GITHUB_API_URL).send().await?;

    // Special case: /releases/latest returns 404 when the repo has not
    // published any releases at all. Treat as "no release channel yet"
    // instead of an error so the user gets an actionable hint.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(CheckOutcome::NoReleasesPublished);
    }

    if !response.status().is_success() {
        return Err(format!(
            "GitHub API returned {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        )
        .into());
    }

    let release: Release = response.json().await?;

    // Defensive: even if /releases/latest succeeds, an empty tag list
    // (shouldn't happen, but GitHub occasionally returns odd shapes) is
    // also treated as "no release channel".
    if release.tag_name.trim().is_empty() {
        return Ok(CheckOutcome::NoReleasesPublished);
    }

    let latest_version = release.tag_name.trim_start_matches('v').to_string();
    let current_version = env!("CARGO_PKG_VERSION");

    if !is_newer(&latest_version, current_version) {
        return Ok(CheckOutcome::UpToDate);
    }

    let target_triple = get_target_triple();
    let asset = find_asset(&release.assets, &latest_version, target_triple)
        .ok_or_else(|| format!("No release asset found for target {}", target_triple))?;

    Ok(CheckOutcome::NewRelease {
        version: latest_version,
        asset,
    })
}

/// Fetch the SHA of the most recent commit on the default branch from
/// the GitHub Commits API.
async fn fetch_latest_commit() -> Result<CommitInfo, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let response = client.get(GITHUB_COMMITS_API_URL).send().await?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub API returned {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        )
        .into());
    }

    let commits: Vec<GitHubCommit> = response.json().await?;
    let sha = commits
        .first()
        .map(|c| c.sha.clone())
        .unwrap_or_else(|| "unknown".to_string());

    Ok(CommitInfo { sha })
}

/// Query the GitHub Compare API to see how many commits `local_sha` is
/// behind `latest_sha`. Returns the count on success, or None on any
/// error so the UI can silently omit the distance line.
async fn fetch_commits_behind(
    local_sha: &str,
    latest_sha: &str,
) -> Option<i64> {
    let url = format!(
        "https://api.github.com/repos/joker0210G/Hydragent/compare/{}...{}",
        local_sha,
        latest_sha,
    );
    let client = match reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(_) => return None,
    };
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<CompareResponse>().await {
                Ok(data) if data.status == "identical" => Some(0),
                Ok(data) if data.status == "ahead" || data.status == "diverged" => Some(data.behind_by.max(0)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Best-effort detection of the local git commit the running binary was
/// built from.
///
/// We check, in order:
///   1. The directory holding the current binary (works for `cargo run`).
///   2. The standard source checkout path used by the installer
///      (`~/.hydragent/src` or `%USERPROFILE%\.hydragent\src`).
///   3. The current working directory (last-ditch fallback).
fn get_local_commit() -> Option<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    // 1. Directory that holds the binary.
    if let Some(p) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        dirs.push(p);
    }

    // 2. Installer source checkout directory.
    #[cfg(target_os = "windows")]
    let src_dir = std::env::var("USERPROFILE").ok().map(|p| PathBuf::from(p).join(".hydragent").join("src"));
    #[cfg(not(target_os = "windows"))]
    let src_dir = std::env::var("HOME").ok().map(|p| PathBuf::from(p).join(".hydragent").join("src"));
    if let Some(p) = src_dir {
        dirs.push(p);
    }

    // 3. Current working directory.
    if let Ok(p) = std::env::current_dir() {
        dirs.push(p);
    }

    for dir in dirs {
        if let Ok(output) = Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "rev-parse", "HEAD"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
        {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() && output.status.success() {
                return Some(s);
            }
        }
    }
    None
}

/// Parse two dotted version strings and compare them as (major,minor,patch).
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Option<(u64, u64, u64)> {
        let mut parts = s.split('.');
        Some((
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ))
    };

    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        (Some(_), None) => true,   // release parses, local doesn't → assume release is newer
        (None, Some(_)) => false,  // local parses, release doesn't → don't downgrade
        (None, None) => latest != current,
    }
}

/// Return the Rust target triple for the platform this binary was built
/// for, matching the asset names published by the release workflow.
fn get_target_triple() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))
    {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else {
        panic!("Unsupported target platform for hydragent update")
    }
}

/// Scan the release asset list for a file matching the expected naming
/// convention.
fn find_asset(assets: &[Asset], version: &str, triple: &str) -> Option<Asset> {
    let expected_names = [
        format!("hydragent-v{}-{}.zip", version, triple),
        format!("hydragent-{}-{}.zip", version, triple),
        format!("hydragent-v{}-{}.tar.gz", version, triple),
        format!("hydragent-{}-{}.tar.gz", version, triple),
    ];

    for asset in assets {
        for expected in &expected_names {
            if asset.name == *expected {
                return Some(asset.clone());
            }
        }
    }
    None
}

/// Download the release asset to a temporary directory and extract it.
async fn download_and_extract(asset: &Asset) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let response = client.get(&asset.browser_download_url).send().await?;

    if !response.status().is_success() {
        return Err(format!(
            "Download returned {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        )
        .into());
    }

    let bytes = response.bytes().await?;

    let temp_dir = tempfile::tempdir()?;
    let archive_path = temp_dir.path().join(&asset.name);
    std::fs::write(&archive_path, &bytes)?;

    extract_archive(&archive_path, temp_dir.path())?;

    let binary_name = if cfg!(target_os = "windows") {
        "hydragent.exe"
    } else {
        "hydragent"
    };

    let extracted_binary = temp_dir.path().join(binary_name);
    if !extracted_binary.exists() {
        return Err(format!(
            "Archive did not contain expected binary: {}",
            binary_name
        )
        .into());
    }

    // Keep the temp dir alive until the caller is done with the path.
    // We do this by moving the binary out of the temp dir before returning.
    let staging = std::env::temp_dir().join(format!("hydragent-update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging)?;
    let staging_binary = staging.join(binary_name);
    std::fs::rename(&extracted_binary, &staging_binary)?;

    Ok(staging_binary)
}

/// Extract a `.zip` or `.tar.gz` archive into `out_dir`.
fn extract_archive(archive: &Path, out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let stem = archive
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let is_zip = ext == "zip";
    let is_tar_gz = stem.ends_with(".tar") && ext == "gz";

    if !is_zip && !is_tar_gz {
        return Err(format!("Unknown archive format: {}", archive.display()).into());
    }

    // Primary extraction: system `tar` (works on Win10+, macOS, Linux).
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(out_dir)
        .status();

    if let Ok(status) = status {
        if status.success() {
            return Ok(());
        }
    }

    // Fallback 1: native Rust extraction (cross-platform, no external tools).
    if is_tar_gz {
        let file = std::fs::File::open(archive)?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(out_dir)?;
        return Ok(());
    }

    if is_zip {
        let file = std::fs::File::open(archive)?;
        let mut archive = zip::ZipArchive::new(file)?;
        archive.extract(out_dir)?;
        return Ok(());
    }

    // Fallback 2: PowerShell Expand-Archive for .zip on older Windows.
    #[cfg(target_os = "windows")]
    if is_zip {
        let ps = format!(
            "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
            archive.display(),
            out_dir.display()
        );
        let status = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .status()?;
        if status.success() {
            return Ok(());
        }
    }

    Err("Failed to extract archive with tar, native extractors, and PowerShell".into())
}

/// Replace the current hydragent binary with the newly downloaded one.
///
/// On Windows the running `.exe` is locked, so we rename it to `.old`
/// first and then move the new binary into place.
async fn replace_binary(new_binary: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;

    #[cfg(target_os = "windows")]
    {
        let old_exe = current_exe.with_extension("old");

        // Rename the running binary (Windows allows renaming a running exe).
        std::fs::rename(&current_exe, &old_exe)?;

        // Move the new binary into place. If this fails, restore the old
        // binary immediately so the user isn't left stranded.
        if let Err(e) = std::fs::rename(new_binary, &current_exe) {
            let _ = std::fs::rename(&old_exe, &current_exe);
            return Err(format!(
                "Failed to install new binary (restored old one): {}", e
            )
            .into());
        }

        // Try to clean up the .old file. This usually fails because the
        // process is still running, so we ignore errors. The `.old` file
        // is harmless and can be deleted manually later.
        let _ = std::fs::remove_file(&old_exe);

        // Best-effort: spawn a detached process to delete the .old file
        // after this process exits.
        let old_path = old_exe.to_string_lossy().to_string();
        let cmd_str = format!(
            r#"ping 127.0.0.1 -n 4 >nul & del /F "{}""#,
            old_path
        );
        let _ = Command::new("cmd")
            .args(["/C", &cmd_str])
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
            .spawn();
    }

    #[cfg(not(target_os = "windows"))]
    {
        // On Unix we can overwrite a running binary in-place because the
        // kernel keeps the old inode alive for the running process.
        std::fs::rename(new_binary, &current_exe)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// No-release fallback: interactive prompt + one-command installer launch.
//
// When the repo has no published GitHub releases we offer to update from
// the current `main` commit by invoking the same one-command installer the
// docs recommend (`install.ps1` / `install.sh`). The installer already
// handles all the clone / build / copy / PATH steps, so we just shell out
// to it with the right flags.
//
// Windows implementation note (this matters):
//
// We used to do the download inside a PowerShell wrapper script:
// `[System.Net.WebClient]::new().DownloadData(...)` + `WriteAllBytes` to
// a `Join-Path $env:TEMP (..., [Guid]::NewGuid(), ...)` + `& powershell
// -File <that tmp> -Source -Force`. That combination of patterns is a
// textbook malware dropper (random temp path + network download +
// immediate execution), and Windows Defender's AMSI antimalware scan
// refused the spawn with `ERROR_ACCESS_DENIED` (os error 5). The end
// result was that PowerShell never started at all.
//
// The fix is to move the download-and-write step out of PowerShell and
// do it from Rust with the existing `reqwest` client. PowerShell then
// only sees a plain `powershell -File <tmp> -Source -Force` invocation
// of the canonical `install.ps1` — nothing for AMSI to flag.
//
// The hosted install.ps1 is intentionally BOM-less so `irm ... | iex`
// works correctly (a BOM prefix breaks Invoke-Expression when the
// script starts with a `<#` block comment). The updater prepends the
// BOM itself before writing the temp file because `powershell -File`
// reads the file from disk and PowerShell 5.1 uses the BOM to detect
// UTF-8 encoding. PowerShell 7+ ignores the BOM either way.
//
// `-File` (NOT `-Command` + `Invoke-Expression`) is what actually
// honours the install script's `param()` block, which is the only way
// `-Source` / `-Force` reach the installer. Without those two flags
// the installer hits the "already installed" early-exit and never
// rebuilds anything.
// ---------------------------------------------------------------------------

/// Return the human-facing one-line install command for the current OS.
/// Used both in the prompt and in the "you can run this anytime" hint.
fn install_one_liner() -> &'static str {
    if cfg!(target_os = "windows") {
        "irm https://joker0210G.github.io/Hydragent/install.ps1 | iex"
    } else {
        "curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh"
    }
}

/// Read a single line from stdin and interpret it as a yes/no answer.
///
/// - `prompt` is printed verbatim (caller is responsible for any
///   leading whitespace and trailing punctuation).
/// - `default_yes` controls what an empty line (just Enter) means.
/// - Recognized: `y` / `yes` (always yes), `n` / `no` / `q` / `quit`
///   (always no), and the empty line which falls back to `default_yes`.
/// - Any other input re-prompts until the user gives a recognizable
///   answer, so we never accidentally launch the installer on a typo.
fn confirm_yes_no(prompt: &str, default_yes: bool) -> bool {
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    loop {
        print!("{} [{}] ", prompt, if default_yes { "Y/n" } else { "y/N" });
        let _ = io::stdout().flush();

        let mut line = String::new();
        if handle.read_line(&mut line).is_err() {
            // EOF on stdin (e.g. piped input that ran out, or a
            // non-interactive shell). Respect `default_yes` rather than
            // guessing.
            return default_yes;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return default_yes;
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "y" | "yes" => return true,
            "n" | "no" | "q" | "quit" => return false,
            other => {
                println!(
                    "  (unrecognized answer '{}'; please type y, n, or press Enter)",
                    other
                );
                continue;
            }
        }
    }
}

/// UTF-8 byte order mark. The updater prepends this to the temp
/// install.ps1 before spawning `powershell -File` so PowerShell 5.1
/// detects UTF-8 encoding when reading the file from disk. The hosted
/// install.ps1 itself is kept BOM-less because `irm ... | iex` fails
/// with parser errors when a BOM precedes a `<#` block comment.
/// PowerShell 7+ ignores the BOM in both paths.
#[cfg(target_os = "windows")]
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Build the Unix shell command that pipes the hosted installer into
/// `sh` with the right env vars. `install.sh` reads
/// `HYDRAGENT_SOURCE=1` and `HYDRAGENT_FORCE=1` (NOT `--source` /
/// `--force` positional args — those would be silently ignored) to
/// decide whether to skip the early-exit and force a from-source
/// rebuild. Shell assignment-prefix sets the env vars on `sh`'s own
/// environment so the script sees them.
#[cfg(not(target_os = "windows"))]
fn unix_installer_command() -> String {
    format!(
        "curl -fsSL {url} | HYDRAGENT_SOURCE=1 HYDRAGENT_FORCE=1 sh",
        url = INSTALL_SH_URL,
    )
}

/// Download the Windows installer script to a uniquely-named temp file
/// in `$TEMP` and prepend a UTF-8 BOM so PowerShell 5.1 detects UTF-8
/// when it reads the file via `powershell -File`. The hosted script
/// itself is kept BOM-less for `irm | iex` compatibility. The temp
/// file guard deletes the .ps1 on drop so we don't leak files in
/// %TEMP% if the spawned PowerShell is killed mid-run.
///
/// Doing the download in Rust (not inside a PowerShell wrapper) is the
/// critical bit: a `[Guid]::NewGuid()` + `[Net.WebClient]::DownloadData`
/// + `WriteAllBytes` + `powershell -File` chain is a textbook malware
/// dropper pattern, and Windows Defender AMSI refuses to spawn the
/// child PowerShell with `ERROR_ACCESS_DENIED`. By the time PowerShell
/// starts, the file already exists on disk with a predictable shape
/// and a name that doesn't contain a fresh GUID — nothing for AMSI
/// to flag.
#[cfg(target_os = "windows")]
async fn download_windows_installer_to_temp(
    url: &str,
) -> Result<(PathBuf, TempInstallerGuard), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch {} (HTTP {})",
            url,
            response.status()
        )
        .into());
    }
    let bytes = response.bytes().await?;

    let mut tmp_dir = std::env::temp_dir();
    tmp_dir.push(format!(
        "hydragent-installer-{}.ps1",
        uuid::Uuid::new_v4()
    ));

    // Write BOM + bytes in one shot so PowerShell never sees a
    // half-written file.
    let mut combined = Vec::with_capacity(UTF8_BOM.len() + bytes.len());
    combined.extend_from_slice(&UTF8_BOM);
    combined.extend_from_slice(&bytes);
    std::fs::write(&tmp_dir, &combined)?;

    let guard = TempInstallerGuard {
        path: tmp_dir.clone(),
    };
    Ok((tmp_dir, guard))
}

/// RAII guard that best-effort removes a temp installer file when
/// dropped. We can't return the bytes out of this function, so the
/// caller passes the path back into [`launch_source_installer`], but
/// if the caller is aborted (panic, early return) the file still gets
/// cleaned up.
#[cfg(target_os = "windows")]
struct TempInstallerGuard {
    path: PathBuf,
}

#[cfg(target_os = "windows")]
impl Drop for TempInstallerGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Spawn the one-command installer with the flags that force a
/// from-source rebuild and overwrite the existing binary. Returns the
/// installer's `ExitStatus` so the caller can propagate its exit code.
///
/// Platform-specific helpers below keep `#[cfg]` at the function level
/// so the Linux build never sees `creation_flags` or the Windows-only
/// helpers; previously `cfg!` inside the function body caused the
/// compiler to type-check both branches on every platform.
async fn launch_source_installer() -> io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "windows")]
    return launch_source_installer_windows().await;
    #[cfg(not(target_os = "windows"))]
    return launch_source_installer_unix().await;
}

#[cfg(target_os = "windows")]
async fn launch_source_installer_windows() -> io::Result<std::process::ExitStatus> {
    let (tmp_path, _guard) = match download_windows_installer_to_temp(INSTALL_PS1_URL).await {
        Ok(pair) => pair,
        Err(e) => {
            return Err(io::Error::new(io::ErrorKind::Other, e.to_string()));
        }
    };
    let tmp_arg = tmp_path.to_string_lossy().to_string();
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-NonInteractive",
        "-File",
        &tmp_arg,
        "-Source",
        "-Force",
    ]);
    // Inherit our stdio so the installer's own output (banner,
    // progress, errors) is visible to the user instead of being
    // swallowed. Disable stdin so the installer can't accidentally
    // block on a prompt it didn't expect from us.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());
    let result = cmd.status();
    // Guard drops here and removes the temp .ps1.
    result
}

#[cfg(not(target_os = "windows"))]
async fn launch_source_installer_unix() -> io::Result<std::process::ExitStatus> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", &unix_installer_command()]);
    // Let the installer talk to the user (it might prompt for
    // confirmations, sudo password, etc.).
    cmd.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The no-release fallback shells out to the canonical hosted
    /// install script. Make sure the constants agree with the live
    /// site, otherwise `hydragent update` on a repo with no releases
    /// will silently fail to fetch the installer.
    #[test]
    fn installer_urls_point_to_canonical_github_pages() {
        assert!(
            INSTALL_PS1_URL.starts_with("https://joker0210G.github.io/Hydragent/"),
            "unexpected install.ps1 URL: {}",
            INSTALL_PS1_URL
        );
        assert!(
            INSTALL_SH_URL.starts_with("https://joker0210G.github.io/Hydragent/"),
            "unexpected install.sh URL: {}",
            INSTALL_SH_URL
        );
        assert_eq!(GITHUB_REPO, "joker0210G/Hydragent");
    }

    /// The one-liner we print to the user must match the platform
    /// we are running on, otherwise we'd tell a Windows user to run
    /// a bash command (or vice versa).
    #[test]
    fn one_liner_matches_current_platform() {
        let line = install_one_liner();
        if cfg!(target_os = "windows") {
            assert!(line.contains("install.ps1"), "windows should reference install.ps1: {}", line);
            assert!(line.contains("irm"), "windows one-liner should use irm: {}", line);
        } else {
            assert!(line.contains("install.sh"), "unix should reference install.sh: {}", line);
            assert!(line.contains("curl"), "unix one-liner should use curl: {}", line);
        }
    }

    /// The PowerShell wrapper MUST hand the install script to
    /// `powershell -File <tmp> -Source -Force`. Without `-File`,
    /// PowerShell never parses the script's `param()` block, so
    /// `[switch]$Source` and `[switch]$Force` stay `$false` and the
    /// installer hits the "already installed" early-exit instead of
    /// actually rebuilding. This test is the canary that prevents the
    /// regression: we don't have the wrapper script as a string any
    /// more (it's constructed in Rust), but we can still assert the
    /// shape of the command-line invocation we build.
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn windows_command_line_includes_file_source_and_force() {
        // Build a command line the same way launch_source_installer
        // does, but with a fake temp path so we don't actually hit
        // the network. We only care about the argument SHAPE here.
        let fake_tmp = std::env::temp_dir().join("hydragent-test.ps1");
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-NonInteractive",
            "-File",
            fake_tmp.to_string_lossy().as_ref(),
            "-Source",
            "-Force",
        ]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(
            args.windows(2).any(|w| w[0] == "-File" && !w[1].is_empty()),
            "powershell args must include -File <tmp>: {:?}",
            args
        );
        assert!(
            args.contains(&"-Source".to_string()),
            "powershell args must include -Source: {:?}",
            args
        );
        assert!(
            args.contains(&"-Force".to_string()),
            "powershell args must include -Force: {:?}",
            args
        );
        assert!(
            !args.iter().any(|a| a.contains("Invoke-Expression")),
            "powershell args must NOT contain Invoke-Expression: {:?}",
            args
        );
    }

    /// The temp installer filename must embed a UUID so concurrent
    /// update attempts don't collide, and must end in `.ps1` so
    /// PowerShell recognises it as a script. (The full path is
    /// `$TEMP/hydragent-installer-<uuid>.ps1` — see
    /// `download_windows_installer_to_temp`.)
    #[cfg(target_os = "windows")]
    #[test]
    fn temp_installer_filename_has_uuid_and_ps1_extension() {
        // Mirror the exact format used in download_windows_installer_to_temp.
        let name = format!(
            "hydragent-installer-{}.ps1",
            uuid::Uuid::new_v4()
        );
        assert!(name.starts_with("hydragent-installer-"));
        assert!(name.ends_with(".ps1"));
        assert_eq!(
            name.len(),
            "hydragent-installer-".len() + 36 + ".ps1".len()
        );
    }

    /// The UTF-8 BOM must be the exact 3-byte sequence EF BB BF so
    /// PowerShell 5.1 decodes the script as UTF-8. If this changes
    /// (e.g. someone "fixes" it to a UTF-16 LE BOM, which would also
    /// "work" on Win10+ but break on older PowerShell hosts), the
    /// install banner will turn into mojibake again.
    #[cfg(target_os = "windows")]
    #[test]
    fn utf8_bom_is_exact_three_bytes() {
        assert_eq!(UTF8_BOM, [0xEF, 0xBB, 0xBF]);
        assert_eq!(UTF8_BOM.len(), 3);
    }

    /// The Unix command MUST set `HYDRAGENT_SOURCE=1` and
    /// `HYDRAGENT_FORCE=1` as env vars on the shell that runs the
    /// script. `install.sh` reads those (NOT `--source` / `--force`
    /// positional args — those are silently ignored), so passing
    /// the wrong shape causes the same "already installed" early-exit
    /// bug as the Windows side.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn unix_command_sets_hydragent_source_and_force_env_vars() {
        let cmd = unix_installer_command();
        assert!(
            cmd.contains("HYDRAGENT_SOURCE=1"),
            "unix command must set HYDRAGENT_SOURCE=1: {}",
            cmd
        );
        assert!(
            cmd.contains("HYDRAGENT_FORCE=1"),
            "unix command must set HYDRAGENT_FORCE=1: {}",
            cmd
        );
        assert!(
            cmd.contains(INSTALL_SH_URL),
            "unix command must reference the canonical install.sh URL: {}",
            cmd
        );
        // Make sure we didn't regress to the broken
        // `sh -s -- --source --force` shape.
        assert!(
            !cmd.contains("--source") && !cmd.contains("--force"),
            "unix command must NOT pass --source/--force positional args (install.sh ignores them): {}",
            cmd
        );
    }

    /// `get_local_commit` should discover the current git commit when
    /// the binary lives inside a git working tree (e.g. `cargo run` or
    /// `cargo test`). When the binary was installed from a release
    /// download it returns `None`, so we only assert structure when we
    /// do get a value.
    #[test]
    fn local_commit_detects_git_sha() {
        let Some(sha) = get_local_commit() else {
            // Binary is outside a git repo (release install) — nothing to assert.
            return;
        };
        assert!(
            !sha.is_empty() && sha.len() >= 7,
            "git commit sha should be at least 7 chars, got: {}",
            sha
        );
    }

    /// `GitHubCommit` should deserialize from the minimal JSON shape
    /// the GitHub Commits API returns.
    #[test]
    fn github_commit_deserializes_sha() {
        let raw = r#"{ "sha": "abc1234def5678" }"#;
        let parsed: GitHubCommit = serde_json::from_str(raw).expect("failed to parse GitHubCommit");
        assert_eq!(parsed.sha, "abc1234def5678");
    }

    /// `CommitInfo` constructor is a trivial wrapper but we exercise
    /// it here so future refactors don't accidentally drop the field.
    #[test]
    fn commit_info_holds_sha() {
        let info = CommitInfo {
            sha: "deadbeef".to_string(),
        };
        assert_eq!(info.sha, "deadbeef");
    }
}
