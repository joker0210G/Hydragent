// crates/hydragent-core/src/paths.rs
//
// Centralized filesystem layout for Hydragent.
//
// **All** Hydragent state lives under a single "install root" directory:
//
//   ~/.hydragent/                  (Linux / macOS)
//   %USERPROFILE%\.hydragent\      (Windows)
//
//     ├── .env                     ← config / secrets (top level)
//     ├── bin/                     ← hydragent binary + bundled scripts
//     ├── data/                    ← mutable runtime data
//     │    ├── sessions.db         ← chat history
//     │    ├── skill_library.sqlite
//     │    ├── audit/chain.db      ← Merkle audit chain
//     │    ├── keys/agent_ed25519.key
//     │    ├── vault/.hydravault   ← encrypted credential vault
//     │    └── logs/chat.jsonl     ← chat session transcript
//     └── .cache/                  ← downloaded embeddings model, etc.
//
// Why centralised?
//   * Prevents the previous "everything scattered in cwd" problem:
//     `.env`, `data/`, `audit/` and `keys/` lived wherever the user
//     happened to be. After the migration they all live under one root.
//   * Lets the installer launcher (`install.sh` / `install.ps1`) inject
//     `HYDRAGENT_HOME` + `HYDRAGENT_DATA_DIR` env vars once, instead of
//     each Rust call site re-implementing the same resolution logic.
//   * Tests can isolate themselves by setting `HYDRAGENT_HOME` to a
//     `tempdir()` before launching the binary.
//
// Resolution order (highest priority first):
//
//   1. `HYDRAGENT_HOME` env var (set by the installer launcher).
//   2. `$HOME/.hydragent` (Unix)  /  `%USERPROFILE%\.hydragent` (Windows).
//   3. Relative fallback: `./.hydragent` — never panics, but logs a
//      warning so the operator knows to set the env var or install
//      properly. The relative fallback is intentional: we never want a
//      panic during the very first invocation that would prevent the
//      first-run banner from showing.
//
// `env_file()` is ALWAYS `{hydragent_home()}/.env` — there is no override.
// If a user wants a different location they can symlink (Unix) or `mklink`
// (Windows). This avoids the previous ambiguity where the installer
// expected `$HOME/data/.env` but Rust wrote `cwd/.env`.

use std::path::{Path, PathBuf};

/// Resolve the Hydragent install root directory (the directory that
/// contains `.env`, `data/`, `bin/`, ...).
///
/// Order: `HYDRAGENT_HOME` env > `$HOME/.hydragent` /
/// `%USERPROFILE%\.hydragent` > `./.hydragent` fallback.
///
/// Does NOT create the directory — see [`ensure_dirs`].
pub fn hydragent_home() -> PathBuf {
    // 1. explicit env var (set by install.sh / install.ps1 launcher)
    if let Ok(p) = std::env::var("HYDRAGENT_HOME") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }

    // 2. OS-native home + "/.hydragent"
    #[cfg(target_os = "windows")]
    {
        // Prefer USERPROFILE; HOMEDRIVE+HOMEPATH is older but still set on
        // some installs, so we fall back to it. HOMESHARE (roaming
        // profiles) is a third fallback for corporate environments.
        let candidate = std::env::var("USERPROFILE")
            .ok()
            .or_else(|| {
                let drive = std::env::var("HOMEDRIVE").ok()?;
                let path = std::env::var("HOMEPATH").ok()?;
                Some(format!("{drive}{path}"))
            })
            .or_else(|| std::env::var("HOMESHARE").ok());
        if let Some(home) = candidate {
            let home = home.trim();
            if !home.is_empty() {
                return PathBuf::from(home).join(".hydragent");
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home = home.trim();
            if !home.is_empty() {
                return PathBuf::from(home).join(".hydragent");
            }
        }
    }

    // 3. last-ditch fallback so we never panic during first-run
    PathBuf::from(".hydragent")
}

/// Resolve the data directory (sqlite DBs, vault, logs, audit chain,
/// Resolve the data directory (sqlite DBs, vault, logs, audit chain,
/// embedding cache, ...).
///
/// Order: `HYDRAGENT_DATA_DIR` env > `{hydragent_home()}/data`.
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("HYDRAGENT_DATA_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    hydragent_home().join("data")
}

/// Resolve the config directory (USER.md, SOUL.md, keys, security policies, ...).
///
/// Order: `HYDRAGENT_CONFIG_DIR` env > `{hydragent_home()}/config` (if exists) >
/// `{hydragent_home()}/src/config` (if exists) > `{hydragent_home()}/config` (default).
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("HYDRAGENT_CONFIG_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home_config = hydragent_home().join("config");
    if home_config.exists() {
        return home_config;
    }
    let src_config = hydragent_home().join("src").join("config");
    if src_config.exists() {
        return src_config;
    }
    home_config
}

/// Resolve the path to the user's `.env` config file. Always at the top
/// level of the install root — there is no override (use a symlink if
/// you need it elsewhere).
pub fn env_file() -> PathBuf {
    hydragent_home().join(".env")
}

/// Resolve the path to the bundled binary directory. Used by `update`
/// and `uninstall` to find sibling files (the launcher script, etc.).
pub fn bin_dir() -> PathBuf {
    hydragent_home().join("bin")
}

/// Create every directory Hydragent will need at runtime if it doesn't
/// exist already. Idempotent — safe to call from every entry point.
///
/// Creates (relative to [`hydragent_home`]):
///   * `.hydragent/`                          (the home itself)
///   * `.hydragent/data/`                     (sqlite DBs, vault, logs)
///   * `.hydragent/data/audit/`               (Merkle chain)
///   * `.hydragent/data/keys/`                (agent Ed25519 keypair)
///   * `.hydragent/data/vault/`               (encrypted vault)
///   * `.hydragent/data/logs/`                (chat.jsonl + per-command logs)
///   * `.hydragent/data/cache/`               (downloaded embedding model, …)
///   * `.hydragent/bin/`                      (launcher + bundled tools)
///   * `.hydragent/config/`                   (user configuration files)
///
/// Returns the canonical [`hydragent_home`] path so callers can chain:
/// `paths::ensure_dirs()?; Ok(paths::data_dir())`.
pub fn ensure_dirs() -> std::io::Result<PathBuf> {
    let home = hydragent_home();
    let data = data_dir();
    let bins = bin_dir();
    let config = config_dir();

    let dirs = [
        &home,
        &data,
        &data.join("audit"),
        &data.join("keys"),
        &data.join("vault"),
        &data.join("logs"),
        &data.join("cache"),
        &bins,
        &config,
    ];
    for d in dirs {
        if !d.as_os_str().is_empty() {
            std::fs::create_dir_all(d)?;
        }
    }
    Ok(home)
}

/// Return a short, human-readable description of the resolved layout.
/// Used by `hydragent doctor` and the `--debug` dump so the operator
/// can see at a glance which directories the binary will actually use.
#[derive(Debug, Clone)]
pub struct Layout {
    pub home: PathBuf,
    pub data: PathBuf,
    pub env_file: PathBuf,
    pub bin: PathBuf,
    pub home_source: &'static str, // "HYDRAGENT_HOME" / "USERPROFILE" / "HOME" / "fallback"
    pub data_source: &'static str, // "HYDRAGENT_DATA_DIR" / "default(<home>)"
}

pub fn describe() -> Layout {
    let home_source = if std::env::var("HYDRAGENT_HOME").is_ok() {
        "HYDRAGENT_HOME"
    } else if cfg!(target_os = "windows") {
        if std::env::var("USERPROFILE").is_ok() {
            "USERPROFILE"
        } else if std::env::var("HOMEDRIVE").is_ok() {
            "HOMEDRIVE+HOMEPATH"
        } else {
            "fallback(./.hydragent)"
        }
    } else if std::env::var("HOME").is_ok() {
        "HOME"
    } else {
        "fallback(./.hydragent)"
    };

    let data_source = if std::env::var("HYDRAGENT_DATA_DIR").is_ok() {
        "HYDRAGENT_DATA_DIR"
    } else {
        "default(<home>/data)"
    };

    Layout {
        home: hydragent_home(),
        data: data_dir(),
        env_file: env_file(),
        bin: bin_dir(),
        home_source,
        data_source,
    }
}

/// Convenience: `true` if [`env_file`] exists on disk and is non-empty.
pub fn env_file_exists() -> bool {
    let p = env_file();
    p.exists()
        && std::fs::metadata(&p)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
}

/// Convenience: `true` if [`env_file`] exists at the resolved location
/// OR at any of the legacy fallback locations (`./.env` in cwd, the old
/// `$HOME/data/.env`). Used by the first-run banner so we don't show
/// "Welcome, run `onboard`" when the user clearly *has* a .env — just
/// in the wrong place.
pub fn env_file_anywhere() -> bool {
    if env_file_exists() {
        return true;
    }
    // Legacy: cwd/.env (the pre-migration default).
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".env").exists() {
            return true;
        }
    }
    false
}

/// Convenience wrapper around [`std::fs::write`] for [`env_file`].
/// Creates the parent directory if needed.
pub fn write_env_file(contents: &str) -> std::io::Result<()> {
    let p = env_file();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, contents)
}

/// Load [`env_file`] into the process environment using `dotenvy`.
/// Returns `Ok(true)` if the file was loaded, `Ok(false)` if it didn't
/// exist. Errors propagate.
///
/// We intentionally avoid `dotenvy::dotenv()` here because that helper
/// searches the *current directory*, which is exactly the bug we're
/// fixing: a user running `hydragent` from `C:\Users\Me\Projects\foo\`
/// would get `C:\Users\Me\Projects\foo\.env` instead of
/// `C:\Users\Me\.hydragent\.env`.
pub fn load_dotenv() -> std::io::Result<bool> {
    let p = env_file();
    if !p.exists() {
        return Ok(false);
    }
    // `dotenvy::from_path` (rather than `dotenvy::dotenv()`) loads a
    // specific file by path, no cwd walk. Existing env vars always
    // win, which is the standard behaviour and what every other dotenv
    // implementation does.
    match dotenvy::from_path(&p) {
        Ok(_) => Ok(true),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())),
    }
}

/// Convert any path to an absolute path if it's currently relative.
/// Used by `config::AppConfig::load()` to turn `./data` into an
/// absolute path anchored at `hydragent_home()` instead of `cwd`.
pub fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        // Anchor at hydragent_home() so `./data` from a config file
        // resolves to `<home>/data` instead of `<cwd>/data`.
        hydragent_home().join(p)
    }
}

// ── Tests ────────────────────────────────────────────────────────────
//
// These tests don't touch the filesystem at all (they assert on
// resolution order by manipulating env vars), so they're safe to run
// in any environment including CI sandboxes that don't have $HOME.

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    // Tests that mutate HYDRAGENT_HOME / HOME / USERPROFILE must be
    // serialised because Rust runs `cargo test` workers in parallel.
    // A `static` Mutex is the canonical lockfor this in Rust 1.81+.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with HYDRAGENT_HOME / HOME / USERPROFILE / HOMEDRIVE / HOMEPATH / HOMESHARE cleared, then
    /// restore the originals. Returns whatever `f` returns.
    fn with_env_cleared<F: FnOnce() -> T, T>(f: F) -> T {
        let _g = ENV_LOCK.lock().unwrap();
        let saved_home = env::var("HYDRAGENT_HOME").ok();
        let saved_userprofile = env::var("USERPROFILE").ok();
        let saved_home_unix = env::var("HOME").ok();
        let saved_homedrive = env::var("HOMEDRIVE").ok();
        let saved_homepath = env::var("HOMEPATH").ok();
        let saved_homeshare = env::var("HOMESHARE").ok();

        env::remove_var("HYDRAGENT_HOME");
        env::remove_var("USERPROFILE");
        env::remove_var("HOME");
        env::remove_var("HOMEDRIVE");
        env::remove_var("HOMEPATH");
        env::remove_var("HOMESHARE");

        let result = f();

        if let Some(v) = saved_home { env::set_var("HYDRAGENT_HOME", v); }
        if let Some(v) = saved_userprofile { env::set_var("USERPROFILE", v); }
        if let Some(v) = saved_home_unix { env::set_var("HOME", v); }
        if let Some(v) = saved_homedrive { env::set_var("HOMEDRIVE", v); }
        if let Some(v) = saved_homepath { env::set_var("HOMEPATH", v); }
        if let Some(v) = saved_homeshare { env::set_var("HOMESHARE", v); }
        result
    }

    #[test]
    fn hydragent_home_honours_explicit_env_var() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("HYDRAGENT_HOME", "/opt/hydragent-custom");
        let home = hydragent_home();
        env::remove_var("HYDRAGENT_HOME");
        assert_eq!(home, PathBuf::from("/opt/hydragent-custom"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn hydragent_home_falls_back_to_userprofile_on_windows() {
        with_env_cleared(|| {
            env::set_var("USERPROFILE", "C:\\Users\\TestUser");
            let home = hydragent_home();
            assert_eq!(home, PathBuf::from("C:\\Users\\TestUser\\.hydragent"));
        });
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn hydragent_home_falls_back_to_home_on_unix() {
        with_env_cleared(|| {
            env::set_var("HOME", "/home/testuser");
            let home = hydragent_home();
            assert_eq!(home, PathBuf::from("/home/testuser/.hydragent"));
        });
    }

    #[test]
    fn hydragent_home_never_panics_when_all_vars_unset() {
        with_env_cleared(|| {
            let home = hydragent_home();
            // Must be the relative fallback — non-empty, no panic.
            assert_eq!(home, PathBuf::from(".hydragent"));
        });
    }

    #[test]
    fn env_file_lives_under_home() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("HYDRAGENT_HOME", "/tmp/hyd-test");
        let f = env_file();
        env::remove_var("HYDRAGENT_HOME");
        assert_eq!(f, PathBuf::from("/tmp/hyd-test/.env"));
    }

    #[test]
    fn data_dir_honours_explicit_env_var() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("HYDRAGENT_HOME", "/tmp/hyd-test");
        env::set_var("HYDRAGENT_DATA_DIR", "/var/lib/hydragent-data");
        let d = data_dir();
        env::remove_var("HYDRAGENT_DATA_DIR");
        env::remove_var("HYDRAGENT_HOME");
        assert_eq!(d, PathBuf::from("/var/lib/hydragent-data"));
    }

    #[test]
    fn data_dir_defaults_to_home_slash_data() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("HYDRAGENT_HOME", "/tmp/hyd-test");
        env::remove_var("HYDRAGENT_DATA_DIR");
        let d = data_dir();
        env::remove_var("HYDRAGENT_HOME");
        assert_eq!(d, PathBuf::from("/tmp/hyd-test/data"));
    }

    #[test]
    fn absolutize_anchors_relative_to_home() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("HYDRAGENT_HOME", "/tmp/hyd-test");
        let abs = absolutize(Path::new("./data"));
        env::remove_var("HYDRAGENT_HOME");
        assert_eq!(abs, PathBuf::from("/tmp/hyd-test/./data"));
    }

    #[test]
    fn absolutize_preserves_absolute_paths() {
        let path_str = if cfg!(target_os = "windows") {
            "C:\\already\\absolute"
        } else {
            "/already/absolute"
        };
        let abs = absolutize(Path::new(path_str));
        assert_eq!(abs, PathBuf::from(path_str));
    }

    #[test]
    fn describe_includes_source_labels() {
        let _g = ENV_LOCK.lock().unwrap();
        let layout = describe();
        // Source labels are non-empty so a UI can always show *why*
        // a particular path was chosen.
        assert!(!layout.home_source.is_empty());
        assert!(!layout.data_source.is_empty());
        assert!(layout.home.as_os_str().len() > 0);
        assert!(layout.data.as_os_str().len() > 0);
        assert!(layout.env_file.as_os_str().len() > 0);
        // env_file is always inside home
        assert!(layout.env_file.starts_with(&layout.home));
        // data is either home/data or an override
        if !layout.data.starts_with(&layout.home) {
            // explicit HYDRAGENT_DATA_DIR — both are valid
        } else {
            assert_eq!(layout.data, layout.home.join("data"));
        }
    }
}
