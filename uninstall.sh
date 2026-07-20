#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Uninstaller for Hydragent (macOS/Linux)
# ---------------------------------------------------------------------------
set -euo pipefail

echo "Stopping any running Hydragent processes..."
pkill -f hydragent || true

INSTALL_DIR="$HOME/.hydragent"
if [[ -d "$INSTALL_DIR" ]]; then
    completely=0
    if [[ " $* " == *" --completely "* || " $* " == *" -c "* ]]; then
        completely=1
    elif [[ " $* " != *" --yes "* && " $* " != *" -y "* ]]; then
        echo "How would you like to uninstall Hydragent?"
        echo "  1. Delete ONLY the build/binaries (preserves your memory database, graphs, config .env, and vault)"
        echo "  2. Delete ENTIRELY (deletes all config, databases, memory, and vault)"
        read -p "Select option [1 or 2, default: 1]: " -r choice
        if [[ "$choice" == "2" ]]; then
            completely=1
        elif [[ "$choice" != "1" && -n "$choice" ]]; then
            echo "Uninstall cancelled."
            exit 0
        fi
    fi

    if (( completely == 1 )); then
        echo "Removing installation directory entirely: $INSTALL_DIR"
        rm -rf "$INSTALL_DIR"
        echo "Successfully removed $INSTALL_DIR"
    else
        echo "Removing ONLY binaries and source directories (preserving data, config, and vault)..."
        rm -rf "$INSTALL_DIR/bin" "$INSTALL_DIR/src"
        echo "Successfully removed binaries and source. Data and config preserved."
    fi
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
