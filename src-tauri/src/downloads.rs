//! Discover addon zips the user downloaded by hand. Zips are only read, never extracted, here.

use crate::toc::{self, TocInfo};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Skip anything huge; addon zips are a few MB at most.
const MAX_ZIP: u64 = 200 * 1024 * 1024;

#[derive(Serialize, Clone, Debug)]
pub struct Candidate {
    pub path: String,
    pub file_name: String,
    pub modified: u64,
    /// Folders the zip would install into AddOns.
    pub folders: Vec<String>,
    /// Toc info of the main folder.
    pub main: TocInfo,
}

/// Look inside a zip: which addon folders does it contain, and what does the main toc say?
pub fn inspect(path: &Path) -> Option<(Vec<String>, TocInfo)> {
    let mut zip = zip::ZipArchive::new(fs::File::open(path).ok()?).ok()?;
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index_raw(i).ok()?.enclosed_name())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();

    // folder path inside zip -> toc entries directly in it
    let mut by_depth: [BTreeMap<String, Vec<String>>; 2] = Default::default();
    for n in &names {
        if !n.to_lowercase().ends_with(".toc") {
            continue;
        }
        let parts: Vec<&str> = n.split('/').collect();
        match parts.len() {
            2 => by_depth[0].entry(parts[0].to_string()).or_default().push(n.clone()),
            3 => by_depth[1].entry(format!("{}/{}", parts[0], parts[1])).or_default().push(n.clone()),
            _ => {}
        }
    }
    let dirs = if !by_depth[0].is_empty() {
        std::mem::take(&mut by_depth[0])
    } else {
        // Wrapped zip (e.g. "repo-main/Addon/Addon.toc"); only if there's a single wrapper.
        let wrappers: std::collections::BTreeSet<&str> =
            by_depth[1].keys().filter_map(|k| k.split('/').next()).collect();
        if wrappers.len() != 1 {
            return None;
        }
        std::mem::take(&mut by_depth[1])
    };
    if dirs.is_empty() {
        return None;
    }

    // Same naming rule as the installer: folder name must match a toc (minus flavor suffix).
    let stem = |e: &str| e.rsplit('/').next().unwrap_or(e).rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_default();
    let mut folders: Vec<(String, String)> = Vec::new(); // (install name, toc entry to read)
    for (dir, tocs) in &dirs {
        let dir_name = dir.rsplit('/').next().unwrap_or(dir).to_string();
        let exact = tocs.iter().find(|t| stem(t).eq_ignore_ascii_case(&dir_name))
            .or_else(|| tocs.iter().find(|t| toc::toc_base(&stem(t)).eq_ignore_ascii_case(&dir_name)));
        let (name, entry) = match exact {
            Some(t) => (dir_name, t.clone()),
            None => (toc::toc_base(&stem(&tocs[0])), tocs[0].clone()),
        };
        if !folders.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            folders.push((name, entry));
        }
    }

    let (main_name, main_entry) = folders.iter().min_by_key(|(n, _)| n.len())?.clone();
    let mut text = Vec::new();
    zip.by_name(&main_entry).ok()?.read_to_end(&mut text).ok()?;
    let info = toc::info_from(main_name, &toc::parse_str(&String::from_utf8_lossy(&text)));
    Some((folders.into_iter().map(|(n, _)| n).collect(), info))
}

/// All addon zips in `dir`, newest first, with duplicates (same addon + version) collapsed.
pub fn scan(dir: &Path) -> Vec<Candidate> {
    let mut seen: HashMap<(String, String), Candidate> = HashMap::new();
    for e in fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
        let path = e.path();
        let is_zip = path.extension().map(|x| x.eq_ignore_ascii_case("zip")).unwrap_or(false);
        let Ok(meta) = e.metadata() else { continue };
        if !is_zip || !meta.is_file() || meta.len() > MAX_ZIP {
            continue;
        }
        let Some((folders, main)) = inspect(&path) else { continue };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let c = Candidate {
            file_name: e.file_name().to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            modified,
            folders,
            main,
        };
        let key = (c.main.folder.to_lowercase(), c.main.version.clone().unwrap_or_default());
        match seen.get(&key) {
            Some(old) if old.modified >= c.modified => {}
            _ => {
                seen.insert(key, c);
            }
        }
    }
    let mut v: Vec<Candidate> = seen.into_values().collect();
    v.sort_by(|a, b| b.modified.cmp(&a.modified));
    v
}

/// Cheap fingerprint of the zips in `dir` (names, sizes, mtimes), for polling.
/// inotify doesn't see changes Windows makes under /mnt/c, so the UI polls this.
pub fn signature(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut entries: Vec<(String, u64, u64)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x.eq_ignore_ascii_case("zip")).unwrap_or(false))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some((e.file_name().to_string_lossy().into_owned(), m.len(), t))
        })
        .collect();
    entries.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut h);
    h.finish()
}

/// "v1.2.10" vs "1.2.9" -> Greater. None if either has no numbers.
pub fn cmp_versions(a: &str, b: &str) -> Option<Ordering> {
    fn nums(s: &str) -> Vec<u64> {
        let mut v: Vec<u64> = s
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse().ok())
            .collect();
        while v.last() == Some(&0) && v.len() > 1 {
            v.pop();
        }
        v
    }
    let (x, y) = (nums(a), nums(b));
    if x.is_empty() || y.is_empty() {
        return None;
    }
    Some(x.cmp(&y))
}

/// Loose equality for version strings: "v1.0.7" == "1.0.7".
pub fn same_version(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim().trim_start_matches(['v', 'V']).to_lowercase();
    n(a) == n(b)
}
