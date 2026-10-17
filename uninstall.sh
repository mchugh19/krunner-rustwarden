#!/usr/bin/env bash
# Remove krunner-rbw. The shared config file is left in place.
set -euo pipefail

systemctl --user disable --now krunner-rbw.service 2>/dev/null || true

rm -f "$HOME/.config/systemd/user/krunner-rbw.service"
rm -f "$HOME/.local/share/krunner/dbusplugins/bitwarden.desktop"
rm -rf "$HOME/.local/share/krunner-rbw"

systemctl --user daemon-reload

echo "Removed krunner-rbw. Config kept at ~/.config/krunner-bitwarden/config.ini"
