#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod downloads;
mod game;
mod installer;
mod providers;
mod state;
mod toc;
#[cfg(test)]
mod tests;

use serde::Serialize;
use state::{AppState, TrackedAddon};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

struct Ctx {
    state: Mutex<AppState>,
    client: reqwest::Client,
}

type Res<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
struct ScanRow {
    #[serde(flatten)]
    toc: toc::TocInfo,
    tracked_by: Option<String>,
}

#[derive(Serialize)]
struct UpdateInfo {
    id: String,
    current: String,
    latest: Option<String>,
    has_update: bool,
    /// Installed from a zip with no known online source.
    manual: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct DownloadRow {
    #[serde(flatten)]
    cand: downloads::Candidate,
    /// "new" | "update" | "installed" | "older" | "different" | "other_game"
    status: &'static str,
    installed_version: Option<String>,
    /// Where future updates will come from: "github" | "wowi" | "local"
    link_provider: String,
    link_source: String,
    /// false = toc doesn't list the game's interface number.
    compatible: Option<bool>,
}

/// Where an addon comes from and how to update it.
struct Origin {
    provider: String,
    source: String,
    url: String,
    asset_pattern: Option<String>,
    curse_id: Option<String>,
}

impl Origin {
    fn id(&self) -> String {
        format!("{}:{}", self.provider, self.source)
    }
}

/// Display name: toc Title of the "main" folder (matching repo name, else shortest name).
fn addon_name(addons_dir: &Path, folders: &[String], source: &str) -> String {
    let repo = source.rsplit('/').next().unwrap_or(source);
    let main = folders
        .iter()
        .find(|f| f.eq_ignore_ascii_case(repo))
        .or_else(|| folders.iter().min_by_key(|f| f.len()));
    main.and_then(|f| toc::read_info(&addons_dir.join(f)))
        .map(|i| i.title)
        .unwrap_or_else(|| repo.to_string())
}

/// Save a finished install into state. Folders move ownership to this addon.
fn record(st: &mut AppState, o: Origin, version: String, folders: Vec<String>) -> anyhow::Result<TrackedAddon> {
    let id = o.id();
    let addons_dir = PathBuf::from(&st.addons_dir);
    let curse_id = o.curse_id.clone().or_else(|| (o.provider == "curse").then(|| o.source.clone()));
    let addon = TrackedAddon {
        name: addon_name(&addons_dir, &folders, &o.source),
        id: id.clone(),
        provider: o.provider,
        source: o.source,
        url: o.url,
        version,
        folders: folders.clone(),
        asset_pattern: o.asset_pattern,
        installed_at: installer::now(),
        curse_id,
    };
    for other in st.addons.iter_mut().filter(|a| a.id != id) {
        other.folders.retain(|f| !folders.iter().any(|n| n.eq_ignore_ascii_case(f)));
    }
    st.addons.retain(|a| a.id != id && !a.folders.is_empty());
    st.addons.push(addon.clone());
    st.addons.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    st.save()?;
    Ok(addon)
}

fn old_folders(st: &AppState, id: &str) -> Vec<String> {
    st.addons.iter().find(|a| a.id == id).map(|a| a.folders.clone()).unwrap_or_default()
}

/// Install/update from an online source. `extra_old` = folders of another entry being replaced.
/// The state lock is NOT held while talking to the network, so several installs download in
/// parallel; only the folder swap + state write is serialized.
async fn do_install(ctx: &Ctx, o: Origin, extra_old: &[String]) -> anyhow::Result<TrackedAddon> {
    if o.provider == "local" {
        anyhow::bail!("Installed from a zip with no online source. Use Link to connect it to GitHub, WoWInterface or CurseForge.");
    }
    let id = o.id();
    let (addons_dir, token) = {
        let st = ctx.state.lock().await;
        (st.addons_dir.clone(), st.token().map(String::from))
    };
    if !Path::new(&addons_dir).is_dir() {
        anyhow::bail!("AddOns folder not found: {addons_dir}. Check Settings.");
    }
    let game = game::detect(Path::new(&addons_dir));
    let rel = providers::latest(&ctx.client, token.as_deref(), &o.provider, &o.source, o.asset_pattern.as_deref(), &game).await?;
    let bytes = installer::download(&ctx.client, &rel.download_url).await?;

    let mut st = ctx.state.lock().await;
    let mut old = old_folders(&st, &id);
    for f in extra_old {
        if !old.iter().any(|o| o.eq_ignore_ascii_case(f)) {
            old.push(f.clone());
        }
    }
    let folders = installer::install_bytes(Path::new(&st.addons_dir), &id, &bytes, &old)?;
    record(&mut st, o, rel.version, folders)
}

fn tracked_owner<'a>(st: &'a AppState, folder: &str) -> Option<&'a TrackedAddon> {
    st.addons.iter().find(|a| a.folders.iter().any(|f| f.eq_ignore_ascii_case(folder)))
}

/// How a downloaded zip should be tracked: keep existing online tracking,
/// else GitHub > WoWInterface > CurseForge > manual.
fn origin_for(st: &AppState, info: &toc::TocInfo) -> Origin {
    if let Some(a) = tracked_owner(st, &info.folder).filter(|a| a.provider != "local") {
        return Origin {
            provider: a.provider.clone(),
            source: a.source.clone(),
            url: a.url.clone(),
            asset_pattern: a.asset_pattern.clone(),
            curse_id: a.curse_id.clone().or_else(|| info.curse_id.clone()),
        };
    }
    let curse_id = info.curse_id.clone();
    if let Some(repo) = &info.github {
        return Origin { provider: "github".into(), url: format!("https://github.com/{repo}"), source: repo.clone(), asset_pattern: None, curse_id };
    }
    if let Some(id) = &info.wowi_id {
        return Origin { provider: "wowi".into(), url: format!("https://www.wowinterface.com/downloads/info{id}"), source: id.clone(), asset_pattern: None, curse_id };
    }
    if let Some(id) = &info.curse_id {
        return Origin { provider: "curse".into(), url: format!("https://www.curseforge.com/projects/{id}"), source: id.clone(), asset_pattern: None, curse_id };
    }
    Origin { provider: "local".into(), source: info.folder.clone(), url: String::new(), asset_pattern: None, curse_id }
}

#[tauri::command]
async fn get_state(ctx: tauri::State<'_, Ctx>) -> Res<AppState> {
    Ok(ctx.state.lock().await.clone())
}

#[tauri::command]
async fn save_settings(
    ctx: tauri::State<'_, Ctx>,
    addons_dir: String,
    downloads_dir: String,
    game_interface: String,
    github_token: Option<String>,
) -> Res<AppState> {
    let mut st = ctx.state.lock().await;
    st.addons_dir = addons_dir.trim().to_string();
    st.downloads_dir = downloads_dir.trim().to_string();
    st.game_interface = game_interface.trim().to_string();
    st.github_token = github_token.filter(|t| !t.trim().is_empty());
    st.save().map_err(err)?;
    Ok(st.clone())
}

#[tauri::command]
async fn scan(ctx: tauri::State<'_, Ctx>) -> Res<Vec<ScanRow>> {
    let st = ctx.state.lock().await;
    let rows = toc::scan(Path::new(&st.addons_dir))
        .into_iter()
        .map(|t| {
            let tracked_by = tracked_owner(&st, &t.folder).map(|a| a.id.clone());
            ScanRow { toc: t, tracked_by }
        })
        .collect();
    Ok(rows)
}

#[tauri::command]
async fn scan_downloads(ctx: tauri::State<'_, Ctx>) -> Res<Vec<DownloadRow>> {
    let st = ctx.state.lock().await.clone();
    if st.downloads_dir.is_empty() {
        return Ok(vec![]);
    }
    let addons_dir = PathBuf::from(&st.addons_dir);
    let dl_dir = PathBuf::from(&st.downloads_dir);
    let rows = tauri::async_runtime::spawn_blocking(move || {
        downloads::scan(&dl_dir)
            .into_iter()
            .map(|c| {
                // Compare with what's actually on disk.
                let installed = toc::read_info(&addons_dir.join(&c.main.folder));
                let installed_version = installed.as_ref().map(|i| i.version.clone().unwrap_or_else(|| "?".into()));
                let zip_fits = c.main.supports(&st.game_interface);
                let tracked_online = tracked_owner(&st, &c.main.folder).is_some_and(|a| a.provider != "local");
                // A zip for another game version is never an upgrade over a build that fits,
                // nor over one Hearth keeps updated from an online source.
                let other_game = zip_fits == Some(false)
                    && installed.is_some()
                    && (installed.as_ref().and_then(|i| i.supports(&st.game_interface)) == Some(true) || tracked_online);
                let status = match (&installed_version, &c.main.version) {
                    _ if other_game => "other_game",
                    (None, _) => "new",
                    (Some(iv), Some(cv)) if downloads::same_version(iv, cv) => "installed",
                    (Some(iv), Some(cv)) => match downloads::cmp_versions(cv, iv) {
                        Some(Ordering::Greater) => "update",
                        Some(Ordering::Equal) => "installed",
                        Some(Ordering::Less) => "older",
                        None => "different",
                    },
                    (Some(_), None) => "different",
                };
                let o = origin_for(&st, &c.main);
                DownloadRow {
                    compatible: zip_fits,
                    link_provider: o.provider,
                    link_source: o.source,
                    installed_version,
                    status,
                    cand: c,
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
async fn install_local(ctx: tauri::State<'_, Ctx>, path: String) -> Res<TrackedAddon> {
    let p = PathBuf::from(&path);
    let (_, info) = downloads::inspect(&p).ok_or_else(|| format!("{path} isn't an addon zip"))?;
    let bytes = std::fs::read(&p).map_err(|e| format!("Reading {path}: {e}"))?;
    let mut st = ctx.state.lock().await;
    let o = origin_for(&st, &info);
    let id = o.id();
    let folders = installer::install_bytes(Path::new(&st.addons_dir), &id, &bytes, &old_folders(&st, &id)).map_err(err)?;
    let version = info.version.clone().unwrap_or_else(|| "unknown".into());
    record(&mut st, o, version, folders).map_err(err)
}

#[tauri::command]
async fn install(ctx: tauri::State<'_, Ctx>, url: String, asset_pattern: Option<String>) -> Res<TrackedAddon> {
    let p = providers::parse_url(&url).map_err(err)?;
    let o = Origin {
        provider: p.provider.into(),
        source: p.source,
        url: p.url,
        asset_pattern: asset_pattern.filter(|s| !s.trim().is_empty()),
        curse_id: None,
    };
    do_install(ctx.inner(), o, &[]).await.map_err(err)
}

/// Connect an addon (usually a manual one) to an online source and install the latest from there.
#[tauri::command]
async fn link_addon(ctx: tauri::State<'_, Ctx>, id: String, source: String) -> Res<TrackedAddon> {
    let p = providers::parse_url(&source).map_err(err)?;
    let old = {
        let st = ctx.state.lock().await;
        st.addons.iter().find(|a| a.id == id).cloned().ok_or_else(|| format!("{id} isn't tracked"))?
    };
    let curse_id = if p.provider == "curse" { Some(p.source.clone()) } else { old.curse_id.clone() };
    let o = Origin { provider: p.provider.into(), source: p.source, url: p.url, asset_pattern: None, curse_id };
    let new = do_install(ctx.inner(), o, &old.folders).await.map_err(err)?;
    if new.id != old.id {
        // Its folders were backed up/replaced by the install above.
        let mut st = ctx.state.lock().await;
        st.addons.retain(|a| a.id != old.id);
        st.save().map_err(err)?;
    }
    Ok(new)
}

#[tauri::command]
async fn check_updates(ctx: tauri::State<'_, Ctx>) -> Res<Vec<UpdateInfo>> {
    let st = ctx.state.lock().await.clone();
    let game = game::detect(Path::new(&st.addons_dir));
    let mut out = Vec::new();
    for a in &st.addons {
        if a.provider == "local" {
            out.push(UpdateInfo { id: a.id.clone(), current: a.version.clone(), latest: None, has_update: false, manual: true, error: None });
            continue;
        }
        let r = providers::latest(&ctx.client, st.token(), &a.provider, &a.source, a.asset_pattern.as_deref(), &game).await;
        out.push(match r {
            Ok(rel) => UpdateInfo {
                id: a.id.clone(),
                current: a.version.clone(),
                has_update: !downloads::same_version(&rel.version, &a.version),
                latest: Some(rel.version),
                manual: false,
                error: None,
            },
            Err(e) => UpdateInfo { id: a.id.clone(), current: a.version.clone(), latest: None, has_update: false, manual: false, error: Some(err(e)) },
        });
    }
    Ok(out)
}

#[tauri::command]
async fn update_addon(ctx: tauri::State<'_, Ctx>, id: String) -> Res<TrackedAddon> {
    let a = {
        let st = ctx.state.lock().await;
        st.addons.iter().find(|a| a.id == id).cloned().ok_or_else(|| format!("{id} isn't tracked"))?
    };
    let o = Origin { provider: a.provider, source: a.source, url: a.url, asset_pattern: a.asset_pattern, curse_id: a.curse_id };
    do_install(ctx.inner(), o, &[]).await.map_err(err)
}

/// Moves the addon's folders to the backup area (not a hard delete) and stops tracking it.
#[tauri::command]
async fn uninstall(ctx: tauri::State<'_, Ctx>, id: String) -> Res<()> {
    let mut st = ctx.state.lock().await;
    let a = st.addons.iter().find(|a| a.id == id).cloned().ok_or_else(|| format!("{id} isn't tracked"))?;
    installer::backup_folders(Path::new(&st.addons_dir), &a.id, &a.folders).map_err(err)?;
    st.addons.retain(|x| x.id != id);
    st.save().map_err(err)?;
    Ok(())
}

/// Open a web page in the user's normal (Windows) browser. Only for the addon sites Hearth links to.
#[tauri::command]
fn open_url(url: String) -> Res<()> {
    const ALLOWED: [&str; 4] = [
        "https://www.curseforge.com/",
        "https://www.wowinterface.com/",
        "https://github.com/",
        "https://duckduckgo.com/",
    ];
    if !ALLOWED.iter().any(|p| url.starts_with(p)) || url.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err(format!("Not opening {url}"));
    }
    let spawn = if cfg!(windows) {
        std::process::Command::new("explorer.exe").arg(&url).spawn()
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        // Under WSL, hand the URL to Windows so it opens in the user's own browser.
        std::process::Command::new("/mnt/c/Windows/explorer.exe").arg(&url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&url).spawn()
    };
    // explorer.exe exits non-zero even on success, so only a failure to start counts.
    spawn.map(|_| ()).map_err(|e| format!("Couldn't open the browser: {e}"))
}

#[tauri::command]
fn backup_location() -> String {
    state::data_dir().join("backup").to_string_lossy().into_owned()
}

fn main() {
    let client = reqwest::Client::builder()
        .user_agent(concat!("hearth-addon-manager/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client");

    tauri::Builder::default()
        .manage(Ctx { state: Mutex::new(AppState::load()), client })
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_settings,
            scan,
            scan_downloads,
            install_local,
            install,
            link_addon,
            check_updates,
            update_addon,
            uninstall,
            open_url,
            backup_location
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hearth");
}
