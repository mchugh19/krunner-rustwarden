#!/usr/bin/env bash
# Install krunner-rbw (Rust runner) for the current user.
#
# Installs:
#   ~/.local/share/krunner-rbw/krunner-rbw                 (binary)
#   ~/.local/share/krunner/dbusplugins/bitwarden.desktop   (runner registration)
#   ~/.config/systemd/user/krunner-rbw.service
#   ~/.config/krunner-bitwarden/config.ini                 (only if missing)
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_SRC="$SRC_DIR/target/release/krunner-rbw"

BIN_DIR="$HOME/.local/share/krunner-rbw"
PLUGIN_DIR="$HOME/.local/share/krunner/dbusplugins"
UNIT_DIR="$HOME/.config/systemd/user"
CONFIG_DIR="$HOME/.config/krunner-bitwarden"

if [[ ! -x "$BIN_SRC" ]]; then
    echo "Release binary not found at $BIN_SRC" >&2
    echo "Build it first:  cargo build --release" >&2
    exit 1
fi

# rbw presence check (the runner talks to rbw for everything)
if ! command -v rbw >/dev/null 2>&1; then
    echo "WARNING: rbw not found in PATH. Install rbw before using this runner." >&2
    echo "         https://github.com/doy/rbw" >&2
fi

mkdir -p "$BIN_DIR" "$PLUGIN_DIR" "$UNIT_DIR" "$CONFIG_DIR"

install -m 0755 "$BIN_SRC" "$BIN_DIR/krunner-rbw"
install -m 0644 "$SRC_DIR/krunner-rbw.desktop" "$PLUGIN_DIR/bitwarden.desktop"
install -m 0644 "$SRC_DIR/krunner-rbw.service" "$UNIT_DIR/krunner-rbw.service"

# The Python version used the same bus name. If its service is still
# enabled, it will fight this one for org.kde.krunner.bitwarden.
if systemctl --user is-enabled bitwarden-runner.service >/dev/null 2>&1; then
    echo "NOTE: the Python runner's bitwarden-runner.service is enabled."
    echo "      Both runners own the same bus name; disable one of them:"
    echo "        systemctl --user disable --now bitwarden-runner.service"
fi

if [[ ! -f "$CONFIG_DIR/config.ini" ]]; then
    install -m 0644 "$SRC_DIR/config.example.ini" "$CONFIG_DIR/config.ini"
    echo "Wrote default config to $CONFIG_DIR/config.ini"
else
    echo "Keeping existing $CONFIG_DIR/config.ini (shared with the Python version)"
fi

systemctl --user daemon-reload
systemctl --user enable --now krunner-rbw.service

echo
echo "Installed. KRunner picks up D-Bus runners automatically; if Bitwarden"
echo "results do not appear, restart KRunner:  kquitapp6 krunner; krunner &"
echo
echo "Tip: for GUI unlock prompts, point rbw at a Qt pinentry:"
echo "       rbw config set pinentry pinentry-qt"
echo "     then restart the agent once:  rbw stop-agent"
