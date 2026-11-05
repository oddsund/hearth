//! Persistent app state: settings + the list of addons Hearth installed.
//! Stored as JSON in <data dir>/hearth/state.json: ~/.local/share on Linux and WSL, %APPDATA% on Windows.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrackedAddon {
    /// Unique key, e.g. "github:DeadlyBossMods/DeadlyBossMods" or "wowi:12345".
    pub id: String,
    pub name: String,
    /// "github" | "wowi"
    pub provider: String,
    /// "owner/repo" for GitHub, numeric id for WoWInterface.
    pub source: String,
    /// Human-facing page URL.
    pub url: String,
    pub version: String,
    /// Folders in Interface\AddOns that belong to this addon.
    pub folders: Vec<String>,
    /// Optional regex to pick a specific release asset (GitHub only).
    #[serde(default)]
    pub asset_pattern: Option<String>,
    #[serde(default)]
    pub installed_at: u64,
    /// Remembered for a future Curse provider.
    #[serde(default)]
    pub curse_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AppState {
    pub addons_dir: String,
    /// Where Hearth looks for manually downloaded addon zips.
    #[serde(default)]
    pub downloads_dir: String,
    /// Interface number the game uses (e.g. 16001 for _forever_); used to warn about incompatible zips.
    #[serde(default)]
    pub game_interface: String,
    #[serde(default)]
    pub github_token: Option<String>,
    #[serde(default)]
    pub addons: Vec<TrackedAddon>,
}

/// Windows Downloads folder; under WSL that's /mnt/c/Users/<you>/Downloads.
pub fn detect_downloads_dir() -> String {
    let skip = ["public", "default", "default user", "all users", "defaultuser0", "wsiaccount"];
    if let Ok(entries) = fs::read_dir("/mnt/c/Users") {
        let mut users: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| !skip.contains(&e.file_name().to_string_lossy().to_lowercase().as_str()))
            .map(|e| e.path().join("Downloads"))
            .filter(|p| p.is_dir())
            .collect();
        users.sort();
        if let Some(p) = users.first() {
            return p.to_string_lossy().into_owned();
        }
    }
    dirs::download_dir()
        .filter(|p| p.is_dir())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Known interface number per game folder. Only _forever_ for now (seen in its addons' tocs).
pub fn detect_game_interface(addons_dir: &str) -> String {
    if addons_dir.contains("_forever_") {
        "16001".into()
    } else {
        String::new()
    }
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hearth")
}

fn state_path() -> PathBuf {
    data_dir().join("state.json")
}

/// Best guess at the AddOns folder. `_forever_` first, the game Hearth was built for.
pub fn detect_addons_dir() -> String {
    // Native Windows paths, plus the same drives as seen from WSL.
    let roots = [
        r"C:\Program Files (x86)\World of Warcraft",
        r"C:\Program Files\World of Warcraft",
        "/mnt/c/Program Files (x86)/World of Warcraft",
        "/mnt/c/Program Files/World of Warcraft",
    ];
    let flavors = ["_forever_", "_retail_", "_classic_", "_classic_era_"];
    for root in roots {
        for flavor in flavors {
            let p = PathBuf::from(root).join(flavor).join("Interface").join("AddOns");
            if p.is_dir() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    String::new()
}

impl AppState {
    pub fn load() -> Self {
        let mut st = Self::load_raw();
        if st.downloads_dir.is_empty() {
            st.downloads_dir = detect_downloads_dir();
        }
        if st.game_interface.is_empty() {
            st.game_interface = detect_game_interface(&st.addons_dir);
        }
        st
    }

    fn load_raw() -> Self {
        let path = state_path();
        match fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(st) => st,
                Err(_) => {
                    // Don't silently overwrite a file we can't parse; keep it aside.
                    let _ = fs::rename(&path, path.with_extension("json.bad"));
                    Self::fresh()
                }
            },
            Err(_) => Self::fresh(),
        }
    }

    fn fresh() -> Self {
        AppState {
            addons_dir: detect_addons_dir(),
            ..Default::default()
        }
    }

    pub fn save(&self) -> Result<()> {
        let dir = data_dir();
        fs::create_dir_all(&dir).context("creating app data folder")?;
        let tmp = dir.join("state.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?).context("writing state")?;
        fs::rename(&tmp, state_path()).context("saving state")?;
        Ok(())
    }

    pub fn token(&self) -> Option<&str> {
        self.github_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
    }
}
