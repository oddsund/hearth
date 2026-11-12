#!/usr/bin/env bash
# Build (if needed) and start Hearth inside WSL. From Windows:
#   wsl -d FedoraLinux-45 -u root -- bash "$(wslpath 'C:/path/to/hearth/run.sh')"
# Pass --release for an optimised build.
set -euo pipefail
. "$HOME/.cargo/env"
cd "$(dirname "$(readlink -f "$0")")/backend"
export CARGO_TARGET_DIR="$HOME/hearth-target"
exec cargo run "$@"
