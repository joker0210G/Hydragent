use std::io::{self, Write};

/// Uninstall Hydragent.
///
/// Removes the binary directory, data directory, and reverses PATH
/// additions. Prompts for confirmation unless `yes` is true.
pub fn run(yes: bool) {
    if !yes {
        print!("Are you sure you want to uninstall Hydragent? [y/N]: ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            eprintln!("  Failed to read confirmation. Pass --yes to skip the prompt.");
            std::process::exit(1);
        }
        let trimmed = input.trim().to_lowercase();
        if trimmed != "y" && trimmed != "yes" {
            println!("  Uninstall cancelled.");
            std::process::exit(0);
        }
    }

    println!("  Uninstalling Hydragent…");

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  Could not locate current binary: {}", e);
            std::process::exit(1);
        }
    };

    let bin_dir = match current_exe.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            eprintln!("  Could not determine binary directory.");
            std::process::exit(1);
        }
    };

    let install_root = match bin_dir.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            eprintln!("  Could not determine install root.");
            std::process::exit(1);
        }
    };

    let data_dir = install_root.join("data");
    let src_dir = install_root.join("src");

    // Remove directories.
    if let Err(e) = remove_dir_if_exists(&bin_dir) {
        eprintln!("  Warning: could not remove binary directory: {}", e);
    }
    if let Err(e) = remove_dir_if_exists(&data_dir) {
        eprintln!("  Warning: could not remove data directory: {}", e);
    }
    if let Err(e) = remove_dir_if_exists(&src_dir) {
        eprintln!("  Warning: could not remove source directory: {}", e);
    }
    if src_dir.join(".git").exists() {
        println!("  ℹ Source directory contained a git repo. Any uncommitted work was removed.");
    }

    // Remove PATH / launcher entries.
    #[cfg(target_os = "windows")]
    if let Err(e) = windows_remove_path_entry(&bin_dir) {
        eprintln!("  Warning: could not remove PATH entry: {}", e);
    }

    #[cfg(not(target_os = "windows"))]
    if let Err(e) = unix_remove_path_entry(&bin_dir) {
        eprintln!("  Warning: could not remove PATH entry: {}", e);
    }

    println!("  Hydragent has been uninstalled.");
    println!();
    println!("  Remaining manual steps (if any):");
    println!("    - If you opened a new terminal after installing, close it.");
    println!("    - Remove any custom HYDRAGENT_* environment variables.");
}

fn remove_dir_if_exists(path: &std::path::Path) -> std::io::Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
        println!("  Removed: {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unix PATH cleanup
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "windows"))]
fn unix_remove_path_entry(bin_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let bin_str = bin_dir.to_string_lossy();
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .ok();
    let rc_files = [
        home.as_ref().map(|h| h.join(".zshrc")),
        home.as_ref().map(|h| h.join(".bashrc")),
        home.as_ref().map(|h| h.join(".profile")),
    ];

    for rc in rc_files.iter().flatten() {
        if !rc.exists() {
            continue;
        }
        let content = std::fs::read_to_string(rc)?;
        let original_len = content.lines().count();

        let mut filtered = Vec::new();
        let mut in_added_block = false;
        for line in content.lines() {
            // Remove the "# Added by hydragent-installer" block.
            if line.trim() == "# Added by hydragent-installer" {
                in_added_block = true;
                continue;
            }
            if in_added_block {
                if line.trim().starts_with("export PATH=") && line.contains(&*bin_str) {
                    continue;
                }
                if !line.trim().is_empty() {
                    in_added_block = false;
                }
            }
            // Remove exact PATH export lines that mention this bin dir.
            let trimmed = line.trim();
            if trimmed.starts_with("export PATH=") && trimmed.contains(&*bin_str) {
                continue;
            }
            filtered.push(line);
        }

        if filtered.len() != original_len {
            let new_content = filtered.join("\n") + "\n";
            std::fs::write(rc, new_content)?;
            println!("  Removed PATH entry from {}", rc.display());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Windows PATH cleanup
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn windows_remove_path_entry(bin_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let bin_str = bin_dir.to_string_lossy().to_string();

    // Read current user PATH via PowerShell (avoids adding a winreg crate).
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path', 'User')",
        ])
        .output()?;

    if !output.status.success() {
        return Err("Failed to read user PATH".into());
    }

    let current = String::from_utf8_lossy(&output.stdout);
    let current = current.trim();

    let entries: Vec<&str> = current.split(';').collect();
    let filtered: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|e| {
            let e_norm = e.trim().trim_end_matches('\\');
            let bin_norm = bin_str.trim().trim_end_matches('\\');
            e_norm != bin_norm
        })
        .collect();

    if filtered.len() == entries.len() {
        // Nothing to remove.
        return Ok(());
    }

    let new_path = filtered.join(";");
    let ps_cmd = format!(
        "[Environment]::SetEnvironmentVariable('Path', '{}', 'User')",
        new_path.replace('\'', "''")
    );

    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_cmd])
        .status()?;

    if !status.success() {
        return Err("Failed to update user PATH".into());
    }

    println!("  Removed PATH entry for {}", bin_dir.display());
    Ok(())
}
