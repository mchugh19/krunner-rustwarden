#!/usr/bin/env bash
# Remove krunner-rustwarden. The shared config file is left in place.
set -euo pipefail

systemctl --user disable --now krunner-rustwarden.service 2>/dev/null || true

rm -f "$HOME/.config/systemd/user/krunner-rustwarden.service"
rm -f "$HOME/.local/share/krunner/dbusplugins/bitwarden.desktop"
rm -rf "$HOME/.local/share/krunner-rustwarden"

systemctl --user daemon-reload

echo "Removed krunner-rustwarden. Config kept at ~/.config/krunner-bitwarden/config.ini"
