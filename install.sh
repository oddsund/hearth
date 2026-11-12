#!/usr/bin/env bash
# Build an optimised Hearth and install it inside WSL, so it starts with just `hearth`.
# Run again after code changes to update the installed copy.
#   wsl -d FedoraLinux-45 -u root -- bash /path/to/hearth/install.sh
set -euo pipefail
. "$HOME/.cargo/env"
here="$(dirname "$(readlink -f "$0")")"
cd "$here/backend"
export CARGO_TARGET_DIR="$HOME/hearth-target"
cargo build --release

install -Dm755 "$CARGO_TARGET_DIR/release/hearth" /usr/local/bin/hearth
install -Dm644 icons/icon.png /usr/share/icons/hicolor/256x256/apps/hearth.png
# Left over from the WebKitGTK version, which needed a launcher script.
rm -rf /usr/local/lib/hearth

# WSLg turns .desktop files into Windows Start menu entries.
cat > /usr/share/applications/hearth.desktop <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Hearth
Comment=WoW addon manager
Exec=/usr/local/bin/hearth
Icon=hearth
Categories=Game;Utility;
DESKTOP

echo "Installed: /usr/local/bin/hearth"
