//! Download -> unzip to temp -> find addon folders -> swap into AddOns (with backup + rollback).

use crate::state::data_dir;
use crate::toc;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .context("Download failed")?
        .error_for_status()
        .context("Download failed")?;
    Ok(resp.bytes().await?.to_vec())
}

/// Addons unpack to a few MB, the biggest to a few hundred. Far beyond that is a zip bomb.
const MAX_UNPACKED: u64 = 2 << 30;

fn extract(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("Not a valid zip file")?;
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        // enclosed_name rejects absolute paths and `..` (zip-slip).
        let Some(rel) = f.enclosed_name() else { continue };
        let out = dest.join(rel);
        if f.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut w = fs::File::create(&out)?;
            total += io::copy(&mut (&mut f).take(MAX_UNPACKED - total + 1), &mut w)?;
            if total > MAX_UNPACKED {
                bail!("The zip unpacks to more than 2 GB; refusing it");
            }
        }
    }
    Ok(())
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

/// Find the directories inside the extracted zip that are addons.
/// Handles plain packager zips (Addon/, Addon_Options/) and wrapped ones (repo-sha/Addon/...).
fn locate(root: &Path, depth: u32) -> Vec<PathBuf> {
    let dirs = subdirs(root);
    let found: Vec<PathBuf> = dirs.iter().filter(|d| toc::has_toc(d)).cloned().collect();
    if !found.is_empty() || depth >= 2 {
        return found;
    }
    if dirs.len() == 1 {
        return locate(&dirs[0], depth + 1);
    }
    found
}

/// Usable as one folder directly inside AddOns: not empty, not `.` or `..`, no path
/// separators, and nothing Windows would refuse or silently rename.
pub fn is_folder_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

/// Returns (folder name to install as, path in temp dir).
fn staged_folders(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for dir in locate(root, 0) {
        let name = if toc::matching_toc(&dir).is_some() {
            dir.file_name().unwrap().to_string_lossy().into_owned()
        } else {
            // e.g. GitHub source zip "Owner-Repo-abc123" containing MyAddon.toc
            toc::intended_folder_name(&dir).unwrap_or_else(|| dir.file_name().unwrap().to_string_lossy().into_owned())
        };
        // A toc named "...toc" would otherwise install as AddOns/.. (the Interface folder).
        if !is_folder_name(&name) {
            bail!("The zip names an addon folder {name:?}, which isn't a safe folder name");
        }
        if out.iter().any(|(n, _): &(String, PathBuf)| n.eq_ignore_ascii_case(&name)) {
            continue;
        }
        out.push((name, dir));
    }
    if out.is_empty() {
        bail!("Couldn't find any addon (.toc) inside the zip");
    }
    Ok(out)
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

fn move_dir(src: &Path, dst: &Path) -> io::Result<()> {
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    // Different drive, or rename refused: copy then delete.
    copy_dir(src, dst)?;
    fs::remove_dir_all(src)
}

fn safe(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect()
}

fn backup_root(id: &str) -> PathBuf {
    data_dir().join("backup").join(safe(id))
}

/// Keep only the newest backup per addon.
fn prune_backups(id: &str) {
    let mut v = subdirs(&backup_root(id));
    v.sort();
    if v.len() > 1 {
        for old in &v[..v.len() - 1] {
            let _ = fs::remove_dir_all(old);
        }
    }
}

/// Move folders out of AddOns into a backup dir. Used for uninstall and before upgrades.
pub fn backup_folders(addons_dir: &Path, id: &str, folders: &[String]) -> Result<Vec<(PathBuf, PathBuf)>> {
    let bdir = backup_root(id).join(now().to_string());
    let mut moved = Vec::new();
    for f in folders {
        let live = addons_dir.join(f);
        if !live.exists() {
            continue;
        }
        fs::create_dir_all(&bdir)?;
        let to = bdir.join(f);
        if let Err(e) = move_dir(&live, &to) {
            restore(&moved);
            return Err(e).with_context(|| format!("Couldn't move {f} (is WoW running?)"));
        }
        moved.push((live, to));
    }
    Ok(moved)
}

fn restore(moved: &[(PathBuf, PathBuf)]) {
    for (live, backup) in moved.iter().rev() {
        let _ = fs::remove_dir_all(live);
        let _ = move_dir(backup, live);
    }
}

/// Download and install. Returns the folder names now owned by this addon.
#[allow(dead_code)] // the app downloads first and swaps under the state lock; tests use this
pub async fn install(
    client: &reqwest::Client,
    addons_dir: &Path,
    id: &str,
    download_url: &str,
    old_folders: &[String],
) -> Result<Vec<String>> {
    if !addons_dir.is_dir() {
        bail!("AddOns folder not found: {}. Check Settings.", addons_dir.display());
    }
    let bytes = download(client, download_url).await?;
    install_bytes(addons_dir, id, &bytes, old_folders)
}

/// Install from zip bytes already in memory (downloaded or a local file).
pub fn install_bytes(addons_dir: &Path, id: &str, bytes: &[u8], old_folders: &[String]) -> Result<Vec<String>> {
    if !addons_dir.is_dir() {
        bail!("AddOns folder not found: {}. Check Settings.", addons_dir.display());
    }
    let tmp = tempfile::tempdir().context("Creating temp folder")?;
    extract(bytes, tmp.path())?;
    let staged = staged_folders(tmp.path())?;

    let mut replace: Vec<String> = old_folders.to_vec();
    for (name, _) in &staged {
        if !replace.iter().any(|f| f.eq_ignore_ascii_case(name)) {
            replace.push(name.clone());
        }
    }

    let moved = backup_folders(addons_dir, id, &replace)?;

    let mut placed: Vec<PathBuf> = Vec::new();
    for (name, src) in &staged {
        let dst = addons_dir.join(name);
        if let Err(e) = move_dir(src, &dst) {
            for p in &placed {
                let _ = fs::remove_dir_all(p);
            }
            let _ = fs::remove_dir_all(&dst);
            restore(&moved);
            return Err(e).with_context(|| format!("Couldn't install {name}; rolled back"));
        }
        placed.push(dst);
    }

    prune_backups(id);
    Ok(staged.into_iter().map(|(n, _)| n).collect())
}
