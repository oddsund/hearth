#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod downloads;
mod game;
mod installer;
mod providers;
mod state;
mod toc;
mod trash;
mod wtf;
#[cfg(test)]
mod tests;

/// Snapshot WTF before changing addons (at most every 10 minutes). Never blocks the change.
fn protect_settings(addons_dir: &Path) {
    if let Err(e) = wtf::snapshot_if_stale(addons_dir, std::time::Duration::from_secs(600)) {
        eprintln!("hearth: settings backup failed: {e:#}");
    }
}

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

/// Untracked folders that look like one addon (main folder + its modules).
#[derive(Serialize)]
struct AdoptGroup {
    main: toc::TocInfo,
    folders: Vec<String>,
    link_provider: String,
    link_source: String,
}

/// Group untracked folders: "BigWigs_Core" joins "BigWigs"; folders sharing a Curse/WoWI id join.
fn adoptable(st: &AppState) -> Vec<AdoptGroup> {
    let mut infos: Vec<toc::TocInfo> = toc::scan(Path::new(&st.addons_dir))
        .into_iter()
        .filter(|t| tracked_owner(st, &t.folder).is_none())
        .collect();
    infos.sort_by_key(|t| (t.folder.len(), t.folder.to_lowercase()));
    let mut groups: Vec<(toc::TocInfo, Vec<String>)> = Vec::new();
    for t in infos {
        let lower = t.folder.to_lowercase();
        let same_id = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
        let home = groups.iter_mut().find(|(m, _)| {
            let base = m.folder.to_lowercase();
            lower.starts_with(&format!("{base}_")) || lower.starts_with(&format!("{base}-"))
                || same_id(&m.curse_id, &t.curse_id) || same_id(&m.wowi_id, &t.wowi_id)
        });
        match home {
            Some((_, folders)) => folders.push(t.folder),
            None => groups.push((t.clone(), vec![t.folder])),
        }
    }
    groups
        .into_iter()
        .map(|(main, folders)| {
            let o = origin_for(st, &main);
            AdoptGroup { main, folders, link_provider: o.provider, link_source: o.source }
        })
        .collect()
}

#[derive(Serialize)]
struct MissingDep {
    folder: String,
    /// A zip in Downloads that provides it, if any.
    zip_path: Option<String>,
    zip_file: Option<String>,
}

#[derive(Serialize)]
struct DepReport {
    id: String,
    missing: Vec<MissingDep>,
}

/// Required dependencies of tracked addons that aren't in AddOns. Blizzard_* ship with the game.
fn missing_deps(st: &AppState, zips: &[downloads::Candidate]) -> Vec<DepReport> {
    let dir = Path::new(&st.addons_dir);
    let present = |f: &str| dir.join(f).is_dir();
    st.addons
        .iter()
        .filter_map(|a| {
            let mut missing: Vec<MissingDep> = Vec::new();
            for f in &a.folders {
                for d in toc::read_info(&dir.join(f)).map(|i| i.deps).unwrap_or_default() {
                    let own = a.folders.iter().any(|x| x.eq_ignore_ascii_case(&d));
                    let known = missing.iter().any(|m| m.folder.eq_ignore_ascii_case(&d));
                    if own || known || present(&d) || d.to_lowercase().starts_with("blizzard_") {
                        continue;
                    }
                    let zip = zips.iter().find(|z| z.folders.iter().any(|zf| zf.eq_ignore_ascii_case(&d)));
                    missing.push(MissingDep {
                        folder: d,
                        zip_path: zip.map(|z| z.path.clone()),
                        zip_file: zip.map(|z| z.file_name.clone()),
                    });
                }
            }
            (!missing.is_empty()).then(|| DepReport { id: a.id.clone(), missing })
        })
        .collect()
}

#[derive(Serialize)]
struct UpdateInfo {
    id: String,
    current: String,
    latest: Option<String>,
    has_update: bool,
    /// Installed from a zip with no known online source.
    manual: bool,
    /// Pinned: `latest` is shown, but it isn't offered as an update.
    pinned: bool,
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
    let pinned = st.addons.iter().any(|a| a.id == id && a.pinned);
    let addon = TrackedAddon {
        pinned,
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
    let old_version = tracked_version(&st, &id, &old);
    protect_settings(Path::new(&st.addons_dir));
    let folders = installer::install_bytes(Path::new(&st.addons_dir), &id, &bytes, &old, old_version.as_deref())?;
    record(&mut st, o, rel.version, folders)
}

/// Version being replaced: the tracked entry's, else whoever owns those folders.
fn tracked_version(st: &AppState, id: &str, folders: &[String]) -> Option<String> {
    st.addons
        .iter()
        .find(|a| a.id == id)
        .or_else(|| folders.first().and_then(|f| tracked_owner(st, f)))
        .map(|a| a.version.clone())
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
async fn missing_dependencies(ctx: tauri::State<'_, Ctx>) -> Res<Vec<DepReport>> {
    let st = ctx.state.lock().await.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let zips = if st.downloads_dir.is_empty() { vec![] } else { downloads::scan(Path::new(&st.downloads_dir)) };
        missing_deps(&st, &zips)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn adoptable_addons(ctx: tauri::State<'_, Ctx>) -> Res<Vec<AdoptGroup>> {
    Ok(adoptable(&*ctx.state.lock().await))
}

/// Start tracking folders that are already installed. Nothing is downloaded or moved.
#[tauri::command]
async fn adopt(ctx: tauri::State<'_, Ctx>, folders: Vec<String>) -> Res<TrackedAddon> {
    if let Some(f) = folders.iter().find(|f| !installer::is_folder_name(f)) {
        return Err(format!("{f:?} isn't an addon folder name"));
    }
    let mut st = ctx.state.lock().await;
    if let Some(f) = folders.iter().find(|f| tracked_owner(&st, f).is_some()) {
        return Err(format!("{f} is already tracked"));
    }
    let main = folders.iter().min_by_key(|f| f.len()).ok_or("No folders given")?;
    let info = toc::read_info(&Path::new(&st.addons_dir).join(main)).ok_or_else(|| format!("{main} has no .toc"))?;
    let o = origin_for(&st, &info);
    let version = info.version.clone().unwrap_or_else(|| "unknown".into());
    record(&mut st, o, version, folders).map_err(err)
}

/// Move zips from the Downloads folder to the Recycle Bin. Only .zip files directly in Downloads.
#[tauri::command]
async fn recycle_zips(ctx: tauri::State<'_, Ctx>, paths: Vec<String>) -> Res<usize> {
    let dl = PathBuf::from(&ctx.state.lock().await.downloads_dir);
    let mut ok: Vec<PathBuf> = Vec::new();
    for p in paths.iter().map(PathBuf::from) {
        let in_downloads = !dl.as_os_str().is_empty() && p.parent() == Some(dl.as_path());
        let is_zip = p.extension().map(|x| x.eq_ignore_ascii_case("zip")).unwrap_or(false);
        if !in_downloads || !is_zip || !p.is_file() {
            return Err(format!("Not touching {}", p.display()));
        }
        ok.push(p);
    }
    let n = ok.len();
    tauri::async_runtime::spawn_blocking(move || trash::recycle(&ok.iter().map(|p| p.as_path()).collect::<Vec<_>>()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;
    Ok(n)
}

/// Changes whenever a zip is added, removed or replaced in Downloads (as a string: u64 > JS number).
#[tauri::command]
async fn downloads_signature(ctx: tauri::State<'_, Ctx>) -> Res<String> {
    let dir = PathBuf::from(&ctx.state.lock().await.downloads_dir);
    tauri::async_runtime::spawn_blocking(move || downloads::signature(&dir).to_string())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn install_local(ctx: tauri::State<'_, Ctx>, path: String) -> Res<TrackedAddon> {
    let p = PathBuf::from(&path);
    let (_, info) = downloads::inspect(&p).ok_or_else(|| format!("{path} isn't an addon zip"))?;
    let bytes = std::fs::read(&p).map_err(|e| format!("Reading {path}: {e}"))?;
    let mut st = ctx.state.lock().await;
    let o = origin_for(&st, &info);
    let id = o.id();
    let old = old_folders(&st, &id);
    let old_version = tracked_version(&st, &id, &old);
    protect_settings(Path::new(&st.addons_dir));
    let folders = installer::install_bytes(Path::new(&st.addons_dir), &id, &bytes, &old, old_version.as_deref()).map_err(err)?;
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
        let base = UpdateInfo {
            id: a.id.clone(),
            current: a.version.clone(),
            latest: None,
            has_update: false,
            manual: a.provider == "local",
            pinned: a.pinned,
            error: None,
        };
        if base.manual {
            out.push(base);
            continue;
        }
        let r = providers::latest(&ctx.client, st.token(), &a.provider, &a.source, a.asset_pattern.as_deref(), &game).await;
        out.push(match r {
            Ok(rel) => UpdateInfo {
                has_update: !a.pinned && !downloads::same_version(&rel.version, &a.version),
                latest: Some(rel.version),
                ..base
            },
            Err(e) => UpdateInfo { error: Some(err(e)), ..base },
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
    protect_settings(Path::new(&st.addons_dir));
    installer::backup_folders(Path::new(&st.addons_dir), &a.id, &a.folders, Some(&a.version)).map_err(err)?;
    st.addons.retain(|x| x.id != id);
    st.save().map_err(err)?;
    Ok(())
}

#[tauri::command]
async fn settings_backups(ctx: tauri::State<'_, Ctx>) -> Res<Vec<wtf::Snapshot>> {
    let dir = PathBuf::from(&ctx.state.lock().await.addons_dir);
    Ok(wtf::list(&dir))
}

#[tauri::command]
async fn backup_settings_now(ctx: tauri::State<'_, Ctx>) -> Res<Option<wtf::Snapshot>> {
    let dir = PathBuf::from(&ctx.state.lock().await.addons_dir);
    tauri::async_runtime::spawn_blocking(move || wtf::snapshot(&dir, "manual"))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

#[tauri::command]
async fn restore_settings(ctx: tauri::State<'_, Ctx>, file: String) -> Res<()> {
    let dir = PathBuf::from(&ctx.state.lock().await.addons_dir);
    tauri::async_runtime::spawn_blocking(move || wtf::restore(&dir, Path::new(&file)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

#[tauri::command]
async fn set_pinned(ctx: tauri::State<'_, Ctx>, id: String, pinned: bool) -> Res<TrackedAddon> {
    let mut st = ctx.state.lock().await;
    let a = st.addons.iter_mut().find(|a| a.id == id).ok_or_else(|| format!("{id} isn't tracked"))?;
    a.pinned = pinned;
    let out = a.clone();
    st.save().map_err(err)?;
    Ok(out)
}

/// Release notes for the version an update would install.
#[tauri::command]
async fn changelog(ctx: tauri::State<'_, Ctx>, id: String) -> Res<String> {
    let (a, addons_dir, token) = {
        let st = ctx.state.lock().await;
        let a = st.addons.iter().find(|a| a.id == id).cloned().ok_or_else(|| format!("{id} isn't tracked"))?;
        (a, st.addons_dir.clone(), st.token().map(String::from))
    };
    let game = game::detect(Path::new(&addons_dir));
    let rel = providers::latest(&ctx.client, token.as_deref(), &a.provider, &a.source, a.asset_pattern.as_deref(), &game)
        .await
        .map_err(err)?;
    let text = match (rel.changelog, rel.changelog_url) {
        (Some(t), _) => t,
        (None, Some(url)) => providers::fetch_changelog(&ctx.client, &url).await.map_err(err)?,
        _ => String::new(),
    };
    let text = text.replace("\r\n", "\n");
    Ok(if text.trim().is_empty() { format!("{} {} has no release notes.", a.name, rel.version) } else { text.trim().to_string() })
}

/// Is WoW running? null when it can't be checked.
#[tauri::command]
async fn wow_running() -> Res<Option<bool>> {
    tauri::async_runtime::spawn_blocking(game::wow_running).await.map_err(|e| e.to_string())
}

/// For each tracked addon with a backup: the version it would roll back to ("" if unknown).
#[tauri::command]
async fn rollback_options(ctx: tauri::State<'_, Ctx>) -> Res<std::collections::HashMap<String, String>> {
    let st = ctx.state.lock().await;
    Ok(st
        .addons
        .iter()
        .filter_map(|a| installer::latest_backup(&a.id).map(|(_, m)| (a.id.clone(), m.version.unwrap_or_default())))
        .collect())
}

/// Swap an addon back to its backed-up version (and keep the current one as the new backup).
#[tauri::command]
async fn roll_back(ctx: tauri::State<'_, Ctx>, id: String) -> Res<TrackedAddon> {
    let mut st = ctx.state.lock().await;
    let addons_dir = PathBuf::from(&st.addons_dir);
    protect_settings(&addons_dir);
    let a = st.addons.iter_mut().find(|a| a.id == id).ok_or_else(|| format!("{id} isn't tracked"))?;
    let meta = installer::roll_back(&addons_dir, &a.id, &a.folders, &a.version).map_err(err)?;
    a.version = meta.version.unwrap_or_else(|| "previous".into());
    a.folders = meta.folders;
    a.installed_at = installer::now();
    let out = a.clone();
    st.save().map_err(err)?;
    Ok(out)
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
    // url.dll's FileProtocolHandler hands the URL to the default browser. (explorer.exe
    // misreads URLs with a query string and opens a File Explorer window instead.)
    let rundll = if cfg!(windows) {
        Some("rundll32.exe")
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        // Under WSL, hand the URL to Windows so it opens in the user's own browser.
        Some("/mnt/c/Windows/System32/rundll32.exe")
    } else {
        None
    };
    let spawn = match rundll {
        Some(exe) => std::process::Command::new(exe).args(["url.dll,FileProtocolHandler", &url]).spawn(),
        None => std::process::Command::new("xdg-open").arg(&url).spawn(),
    };
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
            downloads_signature,
            recycle_zips,
            adoptable_addons,
            adopt,
            missing_dependencies,
            install_local,
            install,
            link_addon,
            check_updates,
            update_addon,
            uninstall,
            rollback_options,
            roll_back,
            wow_running,
            set_pinned,
            changelog,
            settings_backups,
            backup_settings_now,
            restore_settings,
            open_url,
            backup_location
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hearth");
}
