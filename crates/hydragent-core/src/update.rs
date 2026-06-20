use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const GITHUB_API_URL: &str = "https://api.github.com/repos/joker0210G/Hydragent/releases/latest";

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

/// Update Hydragent to the latest release from GitHub.
///
/// Checks the GitHub Releases API for a newer version, downloads the
/// correct prebuilt asset for the current platform, and replaces the
/// running binary safely.
pub async fn run() {
    let current_version = env!("CARGO_PKG_VERSION");

    println!("  Checking for updates… (current version: {})", current_version);

    match check_latest_version().await {
        Ok(Some((latest_version, asset))) => {
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
        Ok(None) => {
            println!(
                "  Hydragent is already up to date (v{}).",
                current_version
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("  ✗ Failed to check for updates: {}", e);
            std::process::exit(1);
        }
    }
}

/// Query GitHub Releases for the latest tag and see if it is newer than
/// the binary’s compile-time version.
///
/// Returns `Some((version_string, matching_asset))` when an update is
/// available, or `None` when the local build is already the latest.
async fn check_latest_version() -> Result<Option<(String, Asset)>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .user_agent("hydragent-updater")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let response = client.get(GITHUB_API_URL).send().await?;

    if !response.status().is_success() {
        return Err(format!(
            "GitHub API returned {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        )
        .into());
    }

    let release: Release = response.json().await?;
    let latest_version = release.tag_name.trim_start_matches('v').to_string();
    let current_version = env!("CARGO_PKG_VERSION");

    if !is_newer(&latest_version, current_version) {
        return Ok(None);
    }

    let target_triple = get_target_triple();
    let asset = find_asset(&release.assets, &latest_version, target_triple)
        .ok_or_else(|| format!("No release asset found for target {}", target_triple))?;

    Ok(Some((latest_version, asset)))
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

        // Move the new binary into place.
        std::fs::rename(new_binary, &current_exe)?;

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
            .creation_flags(0x08000008) // DETACHED_PROCESS | CREATE_NO_WINDOW
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
