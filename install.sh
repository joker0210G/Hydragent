#!/usr/bin/env bash
# ============================================================================
# Hydragent installer (macOS / Linux)
#
# One-command installer mirroring the Ollama / OpenClaw one-liner pattern:
#
#     curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
#
# Behaviour:
#   1. Resolve the latest GitHub release for the detected OS / arch.
#   2. Download the prebuilt hydragent binary (or build from source if no
#      release is available).
#   3. Drop it in $HYDRAGENT_HOME/bin (default ~/.hydragent/bin).
#   4. Drop a `hydragent` shell shim on $BinDir so it is on PATH.
#   5. Create a data directory and run `hydragent onboard`.
#
# The script is idempotent: re-running updates an existing install in place.
# It uses only POSIX shell + curl + tar; no admin elevation required.
# ============================================================================

set -euo pipefail

# ----------------------------------------------------------------------------
# Config
# ----------------------------------------------------------------------------
REPO="${HYDRAGENT_REPO:-joker0210G/Hydragent}"
VERSION="${HYDRAGENT_VERSION:-latest}"
INSTALL_ROOT="${HYDRAGENT_INSTALL_ROOT:-$HOME/.hydragent}"
BIN_NAME="hydragent"
LAUNCHER_NAME="hydragent"
BIN_DIR="$INSTALL_ROOT/bin"
DATA_DIR="$INSTALL_ROOT/data"
SRC_DIR="$INSTALL_ROOT/src"

# Split $REPO into Org + Name so we can derive all the URLs from it.
ORG="${REPO%%/*}"
NAME="${REPO##*/}"

# Where the hosted install script lives (GitHub Pages).
# When a custom domain is configured (docs/CNAME) the same URL works
# at e.g. https://hydragent.dev/install.sh.
PAGES_BASE="https://$ORG.github.io/$NAME"
INSTALLER_URL="$PAGES_BASE/install.sh"

# Where prebuilt release tarballs live.
RELEASE_BASE="https://github.com/$REPO/releases"
GITHUB_API="https://api.github.com/repos/$REPO/releases"

# Pretty output (respect NO_COLOR).
if [[ -t 1 ]] && [[ -z "${NO_COLOR:-}" ]]; then
    C_RESET=$'\033[0m'
    C_BOLD=$'\033[1m'
    C_DIM=$'\033[2m'
    C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m'
    C_RED=$'\033[31m'
    C_CYAN=$'\033[36m'
    C_MAGENTA=$'\033[35m'
else
    C_RESET=""; C_BOLD=""; C_DIM=""
    C_GREEN=""; C_YELLOW=""; C_RED=""; C_CYAN=""; C_MAGENTA=""
fi

ok()    { printf '%b  ok%b  %s\n' "$C_GREEN" "$C_RESET" "$*"; }
info()  { printf '%b  ..%b  %s\n' "$C_CYAN"  "$C_RESET" "$*"; }
warn()  { printf '%b  !!%b  %s\n' "$C_YELLOW" "$C_RESET" "$*"; }
err()   { printf '%b  ERR%b %s\n' "$C_RED"    "$C_RESET" "$*" >&2; exit 1; }
step()  {
    printf '\n%b%s%b %b%s%b\n' "$C_BOLD$C_MAGENTA" "[$1]" "$C_RESET" "$C_BOLD" "$2" "$C_RESET"
}

banner() {
    cat <<'EOF' | sed "s/^/$C_CYAN/"
██╗  ██╗██╗   ██╗██████╗ ██████╗  █████╗  ██████╗ ███████╗███╗   ██╗████████╗
██║  ██║╚██╗ ██╔╝██╔══██╗██╔══██╗██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝
███████║ ╚████╔╝ ██║  ██║██████╔╝███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║
██╔══██║  ╚██╔╝  ██║  ██║██╔══██╗██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║
██║  ██║   ██║   ██████╔╝██║  ██║██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║
╚═╝  ╚═╝   ╚═╝   ╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝
EOF
    printf '%bone-command installer%b\n' "$C_DIM" "$C_RESET"
    printf '\n'
}

# ----------------------------------------------------------------------------
# Detection helpers
# ----------------------------------------------------------------------------
detect_target_triple() {
    local os arch triple
    case "$(uname -s)" in
        Linux)  os="unknown-linux" ;;
        Darwin) os="apple-darwin" ;;
        *)      err "Unsupported OS: $(uname -s). For Windows, use install.ps1." ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)              err "Unsupported architecture: $(uname -m)" ;;
    esac

    triple="$arch-$os"
    # Append the libc suffix for Linux (we ship musl + gnu variants).
    if [[ "$os" == "unknown-linux" ]]; then
        if ldd --version 2>&1 | grep -qi musl; then
            triple="$triple-musl"
        else
            triple="$triple-gnu"
        fi
    fi
    printf '%s\n' "$triple"
}

have() { command -v "$1" >/dev/null 2>&1; }

mkdir_p() { mkdir -p "$1"; }

# ----------------------------------------------------------------------------
# Install steps
# ----------------------------------------------------------------------------
install_from_release() {
    local triple="$1" version="$2"
    step 1 "Resolving latest release from GitHub"
    info "Repo:    $REPO"
    info "Version: $version"
    info "Target:  $triple"

    if [[ "$version" == "latest" ]]; then
        version="$(curl -fsSL "$GITHUB_API/latest" \
            | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
            | head -n1)"
        [[ -n "$version" ]] || err "Could not resolve latest tag from $GITHUB_API/latest"
        info "Resolved latest: $version"
    fi

    local asset="hydragent-$version-$triple.tar.gz"
    local url="$RELEASE_BASE/download/$version/$asset"
    local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

    step 2 "Downloading prebuilt binary"
    info "URL: $url"
    if ! curl -fSL --retry 3 -o "$tmp/$asset" "$url"; then
        err "Download failed (release $version may not be published for $triple)"
    fi

    step 3 "Extracting to $BIN_DIR"
    [[ -f "$BIN_DIR/$BIN_NAME" ]] && rm -f "$BIN_DIR/$BIN_NAME"
    tar -xzf "$tmp/$asset" -C "$BIN_DIR"
    [[ -x "$BIN_DIR/$BIN_NAME" ]] || err "Extraction succeeded but $BIN_DIR/$BIN_NAME is not executable"
    chmod +x "$BIN_DIR/$BIN_NAME"
    ok "Installed $BIN_DIR/$BIN_NAME"
}

install_from_source() {
    install_rust_if_missing
    checkout_source
    build_source
}

install_rust_if_missing() {
    if have cargo; then
        ok "Rust already installed: $(cargo --version)"
        return
    fi
    step A1 "Installing Rust toolchain via rustup"
    if have curl; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --default-toolchain stable --profile minimal --no-modify-path
    else
        err "curl is required to install Rust. Install curl or Rust manually."
    fi
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
    have cargo || err "Rust install reported success but cargo is not on PATH"
    ok "Rust installed: $(cargo --version)"
}

checkout_source() {
    if [[ -f "$SRC_DIR/Cargo.toml" ]]; then
        info "Source checkout already present at $SRC_DIR"
        if have git; then
            ( cd "$SRC_DIR" && git pull --ff-only ) >/dev/null 2>&1 || true
        fi
        return
    fi
    step A2 "Cloning $REPO into $SRC_DIR"
    have git || err "git is required for a from-source install."
    git clone "https://github.com/$REPO.git" "$SRC_DIR"
}

build_source() {
    step A3 "Building hydragent-core (release)"
    ( cd "$SRC_DIR"
      cargo build --release -p hydragent-core )
    local built="$SRC_DIR/target/release/$BIN_NAME"
    [[ -x "$built" ]] || err "Build reported success but $built not found"
    cp -f "$built" "$BIN_DIR/$BIN_NAME"
    chmod +x "$BIN_DIR/$BIN_NAME"
    ok "Built and installed $BIN_DIR/$BIN_NAME"
}

install_launcher() {
    step 4 "Installing $LAUNCHER_NAME shell shim"
    cat > "$BIN_DIR/$LAUNCHER_NAME" <<EOF
#!/usr/bin/env bash
# Hydragent shell shim (auto-generated by install.sh)
set -e
export HYDRAGENT_HOME="\${HYDRAGENT_HOME:-$INSTALL_ROOT}"
export HYDRAGENT_DATA_DIR="\${HYDRAGENT_DATA_DIR:-$DATA_DIR}"

if [[ "\${1:-}" == "install" ]] || [[ ! -x "\$HYDRAGENT_HOME/bin/hydragent" ]]; then
    exec bash "\$(dirname "\${BASH_SOURCE[0]}")/install.sh" "\$@"
fi

# Pass through update/uninstall directly to the binary.
if [[ "\${1:-}" == "update" ]] || [[ "\${1:-}" == "uninstall" ]]; then
    exec "\$HYDRAGENT_HOME/bin/hydragent" "\$@"
fi

if [[ \$# -eq 0 ]]; then
    # The canonical .env lives at $HYDRAGENT_HOME/.env (top-level), not
    # in the data dir. See crates/hydragent-core/src/paths.rs.
    if [[ -f "\$HYDRAGENT_HOME/.env" ]]; then
        set -- serve
    else
        set -- onboard
    fi
fi
exec "\$HYDRAGENT_HOME/bin/hydragent" "\$@"
EOF
    chmod +x "$BIN_DIR/$LAUNCHER_NAME"
    ok "Launcher written: $BIN_DIR/$LAUNCHER_NAME"
}

install_path_entry() {
    case ":$PATH:" in
        *":$BIN_DIR:"*) info "PATH already contains $BIN_DIR"; return ;;
    esac

    # Persist onto the user's shell rc. Pick the first that exists.
    local rc=""
    for cand in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.profile"; do
        [[ -f "$cand" ]] && { rc="$cand"; break; }
    done
    if [[ -z "$rc" ]]; then
        rc="$HOME/.profile"
        touch "$rc"
    fi
    if ! grep -qF "$BIN_DIR" "$rc"; then
        printf '\n# Added by hydragent-installer\nexport PATH="%s:$PATH"\n' "$BIN_DIR" >> "$rc"
        ok "Added $BIN_DIR to PATH (in $rc)"
    fi
    export PATH="$BIN_DIR:$PATH"
}

write_next_steps() {
    cat <<EOF

${C_BOLD}Next steps:${C_RESET}

    hydragent status          # one-shot status dashboard
    hydragent serve           # start the gateway (foreground)
    hydragent ps              # list running gateways
    hydragent stop [pid]      # stop a running gateway
    hydragent chat            # interactive REPL

${C_DIM}Open a NEW terminal so the updated PATH is picked up.${C_RESET}

EOF
}

run_onboarding() {
    step 5 "Launching first-run onboarding wizard"
    "$BIN_DIR/$BIN_NAME" onboard
}

# ----------------------------------------------------------------------------
# Driver
# ----------------------------------------------------------------------------
banner
info "Install root: $INSTALL_ROOT"
info "Repo:         $REPO"
info "Version:      $VERSION"
info "OS/Arch:      $(uname -s) $(uname -m)"

mkdir_p "$BIN_DIR" "$DATA_DIR"

already_installed=0
[[ -x "$BIN_DIR/$BIN_NAME" ]] && already_installed=1

if (( already_installed == 1 )) && [[ "${HYDRAGENT_FORCE:-0}" != "1" ]]; then
    existing="$("$BIN_DIR/$BIN_NAME" --version 2>/dev/null || true)"
    if [[ -n "$existing" ]]; then
        ok "Hydragent already installed: $existing"
    else
        ok "Hydragent already installed at $BIN_DIR/$BIN_NAME"
    fi
    info "Run 'hydragent update' to update, 'hydragent uninstall' to remove, or HYDRAGENT_FORCE=1 to reinstall."
    install_launcher
    install_path_entry
    [[ "${HYDRAGENT_SKIP_ONBOARD:-0}" != "1" ]] && run_onboarding
    write_next_steps
    exit 0
fi

force_source=0
[[ "${HYDRAGENT_SOURCE:-0}" == "1" ]] && force_source=1

if (( force_source == 1 )); then
    install_from_source
else
    triple="$(detect_target_triple)"
    if ! install_from_release "$triple" "$VERSION"; then
        warn "Prebuilt release unavailable; falling back to source build."
        install_from_source
    fi
fi

install_launcher
install_path_entry

ok "Hydragent installed to $BIN_DIR"
ok "Data directory: $DATA_DIR"
ok "Launcher:       $BIN_DIR/$LAUNCHER_NAME"

[[ "${HYDRAGENT_SKIP_ONBOARD:-0}" != "1" ]] && run_onboarding
write_next_steps
