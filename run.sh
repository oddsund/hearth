#!/usr/bin/env bash
# Build (if needed) and start Hearth inside WSL. From Windows:
#   wsl -d FedoraLinux-45 -u root -- bash "$(wslpath 'C:/path/to/hearth/run.sh')"
# Pass --release for an optimised build.
set -euo pipefail
. "$HOME/.cargo/env"
cd "$(dirname "$(readlink -f "$0")")/src-tauri"
export CARGO_TARGET_DIR="$HOME/hearth-target"
# WebKitGTK under WSLg can render a blank window with the DMA-BUF renderer.
export WEBKIT_DISABLE_DMABUF_RENDERER=1
exec cargo run "$@"
