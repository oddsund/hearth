#!/usr/bin/env bash
# Build (if needed) and start Hearth inside WSL. From Windows:
#   wsl -d FedoraLinux-45 -u root -- bash "$(wslpath 'C:/path/to/hearth/run.sh')"
# Pass --release for an optimised build.
set -euo pipefail
. "$HOME/.cargo/env"
cd "$(dirname "$(readlink -f "$0")")/src-tauri"
export CARGO_TARGET_DIR="$HOME/hearth-target"
# Draw on WSL's GPU through Mesa's d3d12 driver. Without one, WebKit's GPU renderer can show
# a blank window under WSLg, so draw on the CPU instead (works, but scrolling lags).
if [ -e /dev/dxg ]; then
  export GALLIUM_DRIVER=d3d12
else
  export WEBKIT_DISABLE_DMABUF_RENDERER=1
fi
# Under WSLg, run on X11 so WSLg draws the window frame (GTK's own Wayland frame is hard to resize).
if [ -d /mnt/wslg ]; then
  export GDK_BACKEND=x11
fi
exec cargo run "$@"
