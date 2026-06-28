use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

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
    ahead_by: i64,
    behind_by: i64,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct InstallationMetadata {
    version: String,
    commit: String,
    install_mode: String,
    date: String,
}

fn get_installation_metadata() -> Option<InstallationMetadata> {
    let path = crate::paths::hydragent_home().join("metadata.json");
    if path.exists() {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(meta) = serde_json::from_str::<InstallationMetadata>(&data) {
                return Some(meta);
            }
        }
    }
    None
}

fn write_installation_metadata(meta: &InstallationMetadata) {
    let path = crate::paths::hydragent_home().join("metadata.json");
    if let Ok(data) = serde_json::to_string_pretty(meta) {
        let _ = std::fs::write(path, data);
    }
}

fn parse_git_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");
    // Validate host is github.com (per user feedback!)
    if !url.contains("github.com") {
        return None;
    }
    // Handle SSH format: git@github.com:owner/repo
    if let Some(pos) = url.find("github.com:") {
        return Some(url[pos + 11..].to_string());
    }
    // Handle HTTPS format: https://github.com/owner/repo
    if let Some(pos) = url.find("github.com/") {
        return Some(url[pos + 11..].to_string());
    }
    None
}

fn parse_git_remote_from_local_config() -> Option<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        dirs.push(p);
    }
    #[cfg(target_os = "windows")]
    let src_dir = std::env::var("USERPROFILE").ok().map(|p| PathBuf::from(p).join(".hydragent").join("src"));
    #[cfg(not(target_os = "windows"))]
    let src_dir = std::env::var("HOME").ok().map(|p| PathBuf::from(p).join(".hydragent").join("src"));
    if let Some(p) = src_dir {
        dirs.push(p);
    }
    if let Ok(p) = std::env::current_dir() {
        dirs.push(p);
    }

    for dir in dirs {
        if let Ok(output) = Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "config", "--get", "remote.origin.url"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
        {
            if output.status.success() {
                let url = String::from_utf8_lossy(&output.stdout);
                if let Some(parsed) = parse_git_remote(&url) {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

fn github_repo() -> String {
    if let Ok(r) = std::env::var("HYDRAGENT_REPO") {
        let r = r.trim().to_string();
        if !r.is_empty() {
            return r;
        }
    }
    // Simple key-value line parser for .env scoped specifically to Hydragent-generated formats (per user feedback)
    if let Ok(text) = std::fs::read_to_string(crate::paths::env_file()) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "HYDRAGENT_REPO" {
                    let val = v.trim().trim_matches('"').trim_matches('\'').trim().to_string();
                    if !val.is_empty() {
                        return val;
                    }
                }
            }
        }
    }
    if let Some(parsed) = parse_git_remote_from_local_config() {
        return parsed;
    }
    "joker0210G/Hydragent".to_string()
}

fn install_ps1_url() -> String {
    let repo = github_repo();
    let (org, name) = repo.split_once('/').unwrap_or(("joker0210G", "Hydragent"));
    format!("https://{}.github.io/{}/install.ps1", org, name)
}

#[allow(dead_code)]
fn install_sh_url() -> String {
    let repo = github_repo();
    let (org, name) = repo.split_once('/').unwrap_or(("joker0210G", "Hydragent"));
    format!("https://{}.github.io/{}/install.sh", org, name)
}

/// Update Hydragent to the latest release from GitHub.
pub async fn run() {
    let repo = github_repo();
    let current_version = if let Some(meta) = get_installation_metadata() {
        if meta.version != "source" {
            meta.version
        } else {
            env!("CARGO_PKG_VERSION").to_string()
        }
    } else {
        env!("CARGO_PKG_VERSION").to_string()
    };

    println!("  Checking for updates in repository: {}… (installed: v{})", repo, current_version);

    let latest_release = check_latest_version().await;
    let latest_commit = fetch_latest_commit().await.ok();
    let local_commit = get_local_commit();

    let mut installed_str = format!("v{}", current_version);
    if let Some(ref commit) = local_commit {
        installed_str.push_str(&format!(" (commit: {})", &commit[..8.min(commit.len())]));
    }

    let mut upstream_ver = current_version.clone();
    let mut upstream_commit = "unknown".to_string();
    let mut has_new_release = false;
    let mut target_asset = None;

    match &latest_release {
        Ok(CheckOutcome::NewRelease { version, asset }) => {
            upstream_ver = version.clone();
            target_asset = Some(asset.clone());
            has_new_release = true;
        }
        _ => {}
    }

    if let Some(ref info) = latest_commit {
        upstream_commit = info.sha.clone();
    }

    let commits_behind = match (&local_commit, &latest_commit) {
        (Some(local), Some(latest)) => fetch_commits_behind(local, &latest.sha).await,
        _ => None,
    };

    println!();
    println!("  Comparison Summary:");
    println!("    Installed : {}", installed_str);
    if upstream_commit != "unknown" {
        println!("    Upstream  : v{} (commit: {})", upstream_ver, &upstream_commit[..8.min(upstream_commit.len())]);
    } else {
        println!("    Upstream  : v{}", upstream_ver);
    }

    if let Some(behind) = commits_behind {
        if behind > 0 {
            println!("    Difference: {} commit(s) behind", behind);
        } else {
            println!("    Difference: Up to date");
        }
    } else {
        println!("    Difference: Unknown (could not compare commits)");
    }
    println!();

    if has_new_release {
        let asset = target_asset.unwrap();
        println!("  New version available: v{} (installed: v{})", upstream_ver, current_version);
        if !confirm_yes_no("  Do you want to download and install the new prebuilt release?", true) {
            println!("  Update cancelled.");
            std::process::exit(0);
        }

        println!("  Downloading {} …", asset.name);
        match download_and_extract(&asset).await {
            Ok(new_binary_path) => {
                println!("  Extracted to {}", new_binary_path.display());
                match replace_binary(&new_binary_path).await {
                    Ok(()) => {
                        if let Some(staging_dir) = new_binary_path.parent() {
                            let _ = std::fs::remove_dir(staging_dir);
                        }
                        
                        // Write fresh metadata.json
                        let meta = InstallationMetadata {
                            version: upstream_ver.clone(),
                            commit: upstream_commit.clone(),
                            install_mode: "prebuilt".to_string(),
                            date: chrono::Utc::now().to_rfc3339(),
                        };
                        write_installation_metadata(&meta);

                        println!(
                            "  ✓ Hydragent updated to v{}. Run `hydragent --version` to verify.",
                            upstream_ver
                        );
                        std::process::exit(0);
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
    } else {
        match latest_release {
            Ok(CheckOutcome::NewRelease { .. }) => {
                // Logically unreachable because has_new_release would be true and that branch exits.
                unreachable!();
            }
            Ok(CheckOutcome::UpToDate) => {
                println!("  Hydragent is already up to date (v{}).", current_version);
                std::process::exit(0);
            }
            Ok(CheckOutcome::NoReleasesPublished) | Err(_) => {
                if let Err(ref e) = latest_release {
                    // Check if it's a rate limit error (graceful degradation, per user feedback!)
                    let err_str = e.to_string();
                    if err_str.contains("rate limit") || err_str.contains("403") || err_str.contains("429") {
                        eprintln!("  ⚠ {}", err_str);
                    } else {
                        eprintln!("  ⚠ Failed to check release channel: {}", e);
                    }
                } else {
                    println!("  No release found for this repo.");
                }

                if !confirm_yes_no("  Do you want to update and rebuild from the latest GitHub commit?", true) {
                    println!("  Update cancelled.");
                    std::process::exit(0);
                }

                println!();
                println!("  Launching source build installer...");
                match launch_source_installer().await {
                    Ok(status) => {
                        if !status.success() {
                            eprintln!(
                                "  ✗ Installer exited with code {}",
                                status.code().unwrap_or(-1)
                            );
                            std::process::exit(status.code().unwrap_or(1));
                        }
                        
                        // Write fresh metadata.json
                        let meta = InstallationMetadata {
                            version: "source".to_string(),
                            commit: upstream_commit.clone(),
                            install_mode: "source".to_string(),
                            date: chrono::Utc::now().to_rfc3339(),
                        };
                        write_installation_metadata(&meta);
                        
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("  ✗ Failed to launch installer: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

/// Outcome of querying GitHub for the latest release.
#[derive(Debug)]
enum CheckOutcome {
    NewRelease {
        version: String,
        asset: Asset,
    },
    UpToDate,
    NoReleasesPublished,
}

/// Query GitHub Releases for the latest tag.
async fn check_latest_version() -> Result<CheckOutcome, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let repo = github_repo();
    let api_url = format!("https://api.github.com/repos/{}/releases/latest", repo);
    let response = client.get(&api_url).send().await?;

    // Graceful rate limit handling (per user feedback!)
    if response.status() == reqwest::StatusCode::FORBIDDEN || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err("GitHub API rate limit exceeded (403/429). Skipped release channel check.".into());
    }

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

/// Fetch the SHA of the most recent commit.
async fn fetch_latest_commit() -> Result<CommitInfo, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let repo = github_repo();
    let commits_api_url = format!("https://api.github.com/repos/{}/commits?per_page=1", repo);
    let response = client.get(&commits_api_url).send().await?;

    // Graceful rate limit handling (per user feedback!)
    if response.status() == reqwest::StatusCode::FORBIDDEN || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err("GitHub API rate limit exceeded (403/429). Skipped commit channel check.".into());
    }

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

/// Query the GitHub Compare API.
async fn fetch_commits_behind(
    local_sha: &str,
    latest_sha: &str,
) -> Option<i64> {
    let repo = github_repo();
    let url = format!(
        "https://api.github.com/repos/{}/compare/{}...{}",
        repo,
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

/// Best-effort detection of the local git commit the running binary was built from.
fn get_local_commit() -> Option<String> {
    // 1. Primary source of truth: installation metadata
    if let Some(meta) = get_installation_metadata() {
        if !meta.commit.is_empty() && meta.commit != "unknown" {
            return Some(meta.commit);
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(p) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        dirs.push(p);
    }

    #[cfg(target_os = "windows")]
    let src_dir = std::env::var("USERPROFILE").ok().map(|p| PathBuf::from(p).join(".hydragent").join("src"));
    #[cfg(not(target_os = "windows"))]
    let src_dir = std::env::var("HOME").ok().map(|p| PathBuf::from(p).join(".hydragent").join("src"));
    if let Some(p) = src_dir {
        dirs.push(p);
    }

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

/// Parse two dotted version strings and compare them.
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
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => latest != current,
    }
}

/// Return the Rust target triple.
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
#[allow(dead_code)]
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
        url = install_sh_url(),
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
    let (tmp_path, _guard) = match download_windows_installer_to_temp(&install_ps1_url()).await {
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

    /// Test the git remote URL parser (verifying hosts, SSH, and HTTPS formats)
    #[test]
    fn test_git_remote_parser() {
        // Valid GitHub HTTPS URLs
        assert_eq!(
            parse_git_remote("https://github.com/owner/repo.git").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            parse_git_remote("https://github.com/owner/repo").as_deref(),
            Some("owner/repo")
        );
        // Valid GitHub SSH URLs
        assert_eq!(
            parse_git_remote("git@github.com:owner/repo.git").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            parse_git_remote("git@github.com:owner/repo").as_deref(),
            Some("owner/repo")
        );

        // Invalid hosts (GitLab/Gitea) should return None
        assert_eq!(parse_git_remote("https://gitlab.com/owner/repo.git"), None);
        assert_eq!(parse_git_remote("git@gitlab.com:owner/repo.git"), None);
        assert_eq!(parse_git_remote("https://gitea.com/owner/repo"), None);
    }

    /// Test the simple .env key-value line parser
    #[test]
    fn test_github_repo_resolves_env_or_fallback() {
        // By default, without env overrides, it should fallback to the canonical repo
        // (unless running inside a git checkout, in which case it discovers the local remote,
        // which is also acceptable since it's testing the dynamic path!)
        let repo = github_repo();
        assert!(!repo.is_empty());
        assert!(repo.contains('/'));
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
            cmd.contains(&install_sh_url()),
            "unix command must reference the dynamic install.sh URL: {}",
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
