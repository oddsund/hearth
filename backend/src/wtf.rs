//! Snapshots of the WTF folder (SavedVariables = every addon's settings), taken before
//! Hearth changes addons, so a bad update or removal can't cost you your setup.
//! Stored as zips in <data>/wtf-backups/<flavor>/WTF-<ms>-<reason>.zip.

use crate::state::data_dir;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KEEP: usize = 10;
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub file: String,
    pub taken_at_ms: u64,
    pub reason: String,
    pub size: u64,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// <flavor>/WTF for an <flavor>/Interface/AddOns folder.
pub fn wtf_dir(addons_dir: &Path) -> Option<PathBuf> {
    Some(addons_dir.parent()?.parent()?.join("WTF"))
}

fn store(addons_dir: &Path) -> PathBuf {
    let flavor = addons_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().trim_matches('_').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "game".into());
    data_dir().join("wtf-backups").join(flavor)
}

pub fn list(addons_dir: &Path) -> Vec<Snapshot> {
    let mut v: Vec<Snapshot> = fs::read_dir(store(addons_dir))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let rest = name.strip_prefix("WTF-")?.strip_suffix(".zip")?;
            let (ms, reason) = rest.split_once('-').unwrap_or((rest, "auto"));
            Some(Snapshot {
                taken_at_ms: ms.parse().ok()?,
                reason: reason.to_string(),
                size: e.metadata().map(|m| m.len()).unwrap_or(0),
                file: e.path().to_string_lossy().into_owned(),
            })
        })
        .collect();
    v.sort_by(|a, b| b.taken_at_ms.cmp(&a.taken_at_ms)); // newest first
    v
}

fn add_dir(z: &mut zip::ZipWriter<fs::File>, root: &Path, dir: &Path) -> Result<()> {
    let opt = zip::write::SimpleFileOptions::default();
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        let rel = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        if p.is_dir() {
            z.add_directory(format!("{rel}/"), opt)?;
            add_dir(z, root, &p)?;
        } else {
            z.start_file(rel, opt)?;
            io::copy(&mut fs::File::open(&p)?, z)?;
        }
    }
    Ok(())
}

/// Zip the WTF folder now. `reason` ends up in the file name ("auto", "manual", ...).
pub fn snapshot(addons_dir: &Path, reason: &str) -> Result<Option<Snapshot>> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    take(addons_dir, reason, None)
}

/// `keep`: a backup that must survive the pruning to the last KEEP (the one being restored).
fn take(addons_dir: &Path, reason: &str, keep: Option<&Path>) -> Result<Option<Snapshot>> {
    let Some(wtf) = wtf_dir(addons_dir).filter(|d| d.is_dir()) else {
        return Ok(None); // fresh install: nothing to protect yet
    };
    let dir = store(addons_dir);
    fs::create_dir_all(&dir)?;
    let ms = now_ms();
    let file = dir.join(format!("WTF-{ms}-{reason}.zip"));
    let tmp = file.with_extension("zip.tmp");
    let mut z = zip::ZipWriter::new(fs::File::create(&tmp)?);
    add_dir(&mut z, &wtf, &wtf).context("Backing up WTF")?;
    z.finish()?.flush()?;
    fs::rename(&tmp, &file)?;
    for old in list(addons_dir).into_iter().skip(KEEP).filter(|s| keep != Some(Path::new(&s.file))) {
        let _ = fs::remove_file(old.file);
    }
    Ok(list(addons_dir).into_iter().find(|s| s.taken_at_ms == ms))
}

/// Snapshot unless one was taken in the last `max_age` (one per round of updates, not per addon).
pub fn snapshot_if_stale(addons_dir: &Path, max_age: Duration) -> Result<Option<Snapshot>> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = list(addons_dir).first() {
        if now_ms().saturating_sub(last.taken_at_ms) < max_age.as_millis() as u64 {
            return Ok(None);
        }
    }
    take(addons_dir, "auto", None)
}

/// Replace WTF with a snapshot. The current WTF is snapshotted first ("before-restore").
pub fn restore(addons_dir: &Path, file: &Path) -> Result<()> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Exactly a WTF-*.zip in the store: no "store/../elsewhere.zip".
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ours = file.parent() == Some(store(addons_dir).as_path()) && name.starts_with("WTF-") && name.ends_with(".zip");
    if !ours || !file.is_file() {
        bail!("Not one of Hearth's settings backups");
    }
    let wtf = wtf_dir(addons_dir).context("Can't find the WTF folder")?;
    take(addons_dir, "before-restore", Some(file))?;
    // Unpack next to WTF (same drive, so the swap is a rename), then swap.
    let staging = wtf.with_file_name("WTF.hearth-restore");
    let old = wtf.with_file_name("WTF.hearth-old");
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_dir_all(&old);
    let mut zip = zip::ZipArchive::new(fs::File::open(file)?)?;
    zip.extract(&staging).context("Unpacking the backup")?;
    if wtf.exists() {
        fs::rename(&wtf, &old).context("Couldn't move the current WTF aside (is WoW running?)")?;
    }
    if let Err(e) = fs::rename(&staging, &wtf) {
        let _ = fs::rename(&old, &wtf);
        return Err(e).context("Couldn't put the backup in place; nothing changed");
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}
