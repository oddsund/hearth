<div align="center">

<img src="src-tauri/icons/icon.png" width="96" alt="">

# Hearth

**A small WoW addon manager with no trackers, no ads, no account and no telemetry.**

It installs and updates your addons, keeps backups, and shows you what an update changes before it lands.

![Rust](https://img.shields.io/badge/Rust-Tauri_2-dea584?logo=rust&logoColor=white)
![UI](https://img.shields.io/badge/UI-plain_HTML%2FJS-f0a030)
![Version](https://img.shields.io/badge/version-1.0.0-2f8f4e)
![License](https://img.shields.io/badge/license-MIT-6b7385)

</div>

---

## Why

The usual addon managers ask you to accept tracking from a long list of ad vendors before they'll update a single addon. Hearth doesn't. It talks directly to where addons are published, and nothing else:

| Source | How Hearth uses it |
|---|---|
| **GitHub** | Official REST API: releases, release assets, or the latest commit |
| **WoWInterface** | The public `api.mmoui.com` file-details API |
| **CurseForge** | The JSON endpoints curseforge.com's own pages use, by project ID |
| **Your Downloads folder** | Zips you downloaded by hand in your browser |

There's no search and no catalogue. You bring the addon, and Hearth keeps it up to date.

## Features

### Installing
- **Paste and install:** a GitHub URL or `owner/repo`, a WoWInterface link, or a CurseForge project ID.
- **Downloads pickup:** zips you save to Downloads appear within seconds. A zip built for another game version is flagged so you can fetch the right build.
- **Several at once:** installs and updates run in a queue, four at a time.
- **Adopts what you already have:** addons you installed by hand are detected from their `.toc` files and can be tracked with one click.
- **Missing dependency warnings,** read from `## Dependencies` / `## RequiredDeps`.

### Updating
- **Checks on startup and every 6 hours.** Choose Update one or Update all.
- **Release notes** shown before you update.
- **Show diff:** a git-style review of every changed file, plus static checks on changed lines. These flag bundled executables and suspicious Lua, such as obfuscated code and calls that send data or post to chat. If the zip contains programs (`.exe`, `.dll`, scripts), Hearth won't install it unless you explicitly allow it.
- **Pin** an addon to keep it on its current version.
- **Automatic fallback:** when WoWInterface hands out a login page instead of a zip, Hearth uses the same addon's CurseForge release.

### Safety net
- **Every update is backed up.** **Roll back to _version_** restores the previous version in one click.
- **Swaps are atomic:** if anything fails partway, the old folders are put back.
- **Remove** moves the addon's folders to backup instead of deleting them.
- **Saved settings (WTF) snapshots** are taken before changes (at most every 10 minutes, last 10 kept) and can be restored from Settings.
- **Warns while WoW is running,** since the game locks addon files.
- **Recycle, not delete:** installed zips in Downloads go to the Recycle Bin.

### Everything else
- **Export / import** your addon list as `hearth-addons.json` to move to another PC or share a setup.
- **Starts with Windows,** hidden in the background. Launching it again brings the window up.
- **Build identifier** in the header (`v1.0.0 · bc73de9`), so you always know which build you're running.
- **Light and dark** follow your system theme.

## Game versions

Hearth reads your game's version from `.build.info` / `.flavor.info` and uses it to choose files and check zips. It was built for **`_forever_`** (interface `16001`) but isn't tied to it. Set your AddOns folder and interface version in **Settings**.

## Running it

### On Windows, through WSL2

Windows' Smart App Control blocks unsigned executables you build yourself. Hearth sidesteps that by building and running inside WSL2, and WSLg shows the window like any other app. It's developed on **Fedora**, but any distro with WebKitGTK works.

**1. One-time setup** (in the distro, as the user that will run Hearth):

```bash
sudo dnf install webkit2gtk4.1-devel openssl-devel libappindicator-gtk3-devel librsvg2-devel libxdo-devel gcc gcc-c++
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

On Ubuntu/Debian the packages are `libwebkit2gtk-4.1-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev build-essential`.

**2. Build and install.** Re-run this after pulling changes:

```bash
wsl -d <distro> -u root -- bash /mnt/c/path/to/hearth/install.sh
```

This puts the binary in `/usr/local/lib/hearth/`, adds a `hearth` launcher, and creates a `.desktop` entry, which WSLg turns into a Start menu item.

**3. Start it** from the Start menu, or from a shortcut that runs:

```
wslg.exe -d <distro> -u root -- /usr/local/bin/hearth
```

Put a copy of that shortcut in `shell:startup` with `--hidden` added, and Hearth runs in the background from login.

> [!TIP]
> If links won't open in your Windows browser ("Exec format error"), systemd in your distro has removed WSL's interop handler. Restore it with
> `echo ':WSLInterop:M::MZ::/init:PF' | sudo tee /etc/binfmt.d/WSLInterop.conf` and restart WSL.

### On Linux

Install the same dependencies, then use `./run.sh` (add `--release` for an optimized build) or `install.sh`.

## Development

```
hearth/
├─ ui/                 plain HTML/CSS/JS: no Node, no bundler
└─ src-tauri/src/
   ├─ main.rs          Tauri commands and app state
   ├─ providers.rs     GitHub, WoWInterface and CurseForge lookups
   ├─ installer.rs     download → unzip → stage → swap, backups, roll back
   ├─ review.rs        diffs and static checks for updates
   ├─ toc.rs           .toc parsing (versions, dependencies, source hints)
   ├─ downloads.rs     Downloads folder scanning
   ├─ game.rs          game version detection, "is WoW running?"
   ├─ wtf.rs           saved-settings snapshots
   ├─ trash.rs         Recycle Bin
   └─ state.rs         settings and tracked addons (JSON)
```

```bash
cd src-tauri
cargo test                # offline tests
cargo test -- --ignored   # also hits GitHub, WoWInterface and CurseForge, and reads your Downloads
```

**Where things live:** settings, the addon list and backups are in `~/.local/share/hearth/` (inside WSL on Windows). Your GitHub token, if you add one, is stored there in plain text. A fine-grained token with no permissions is enough; it only raises the API rate limit.

## Caveats

- **CurseForge** support uses undocumented endpoints and could break whenever CurseForge changes its site. Its web pages sit behind bot protection, so Hearth needs the numeric **Project ID** (shown in the "About Project" box) instead of a URL.
- **WoWInterface** now asks for a login for some downloads. Hearth won't work around that; it falls back to CurseForge where it can.
- **The static checks are a tripwire, not antivirus.** They catch the obvious things. The diff is there so you can judge for yourself.
- This is a personal project, built for one setup and shared as is. It's not affiliated with Blizzard, CurseForge, Overwolf or WoWInterface.

## License

[MIT](LICENSE)
