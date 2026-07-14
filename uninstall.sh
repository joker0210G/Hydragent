#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Uninstaller for Hydragent (macOS/Linux)
# ---------------------------------------------------------------------------
set -euo pipefail

echo "Stopping any running Hydragent processes..."
pkill -f hydragent || true

INSTALL_DIR="$HOME/.hydragent"
if [[ -d "$INSTALL_DIR" ]]; then
    echo "Removing installation directory: $INSTALL_DIR"
    rm -rf "$INSTALL_DIR"
    echo "Successfully removed $INSTALL_DIR"
else
    echo "Installation directory not found: $INSTALL_DIR"
fi

# Clean up cargo bin directory
CARGO_BIN="$HOME/.cargo/bin/hydragent"
if [[ -f "$CARGO_BIN" ]]; then
    echo "Removing binary from cargo directory: $CARGO_BIN"
    rm -f "$CARGO_BIN"
fi

# PATH cleanup check
echo ""
echo "Please verify and remove any references to ~/.hydragent/bin from your shell profile (~/.bashrc, ~/.zshrc, etc.)."
echo "Hydragent has been successfully uninstalled!"
