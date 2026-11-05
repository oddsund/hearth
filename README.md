# Hearth

A small, tracker-free WoW addon manager. Tauri 2 (Rust) backend, plain HTML/JS UI.

- Install from a GitHub repo, a WoWInterface page, or a zip in your Downloads folder
- Updates from GitHub releases and the WoWInterface API (preference: GitHub > WoWInterface > manual)
- Swaps addon folders with a backup and rolls back on failure; Remove moves folders to the backup instead of deleting them
- Warns when a zip's `## Interface` doesn't list your game's version (16001 for `_forever_`)

## Running on Windows (via WSL)

Smart App Control blocks unsigned exes built locally, so Hearth is built and run in WSL2 (Fedora) and shown through WSLg.

One-time setup in Fedora: `dnf install webkit2gtk4.1-devel openssl-devel libappindicator-gtk3-devel librsvg2-devel libxdo-devel gcc gcc-c++` and rustup.

Build + install (re-run after changes):

```
wsl -d FedoraLinux-45 -u root -- bash /mnt/c/path/to/hearth/install.sh
```

Start: the desktop shortcut runs `wslg.exe -d FedoraLinux-45 -u root -- /usr/local/bin/hearth`.
A copy in the Windows Startup folder (`shell:startup`) adds `--hidden`, so Hearth runs in the
background from login. Only one Hearth runs at a time; launching it again shows the window.
Dev run without installing: `run.sh`.

## Tests

```
cargo test                      # offline
cargo test -- --ignored         # also hits GitHub / WoWInterface and reads your Downloads
```

State and backups live in `~/.local/share/hearth/` inside WSL.
