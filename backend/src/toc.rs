//! Reading `.toc` files and scanning the AddOns folder.

use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Serialize, Clone, Debug, Default)]
pub struct TocInfo {
    pub folder: String,
    pub title: String,
    pub version: Option<String>,
    pub interface: Option<String>,
    pub notes: Option<String>,
    pub curse_id: Option<String>,
    pub wowi_id: Option<String>,
    pub wago_id: Option<String>,
    /// "owner/repo" if any toc field links to GitHub.
    pub github: Option<String>,
    /// Required addons (## Dependencies / ## RequiredDeps / ## Dep*), not optional ones.
    pub deps: Vec<String>,
}

impl TocInfo {
    /// Does the toc's Interface list include this game's interface number?
    pub fn supports(&self, game_interface: &str) -> Option<bool> {
        let gi = game_interface.trim();
        if gi.is_empty() {
            return None;
        }
        let list = self.interface.as_deref()?;
        Some(list.split(',').any(|v| v.trim() == gi))
    }
}

fn flavor_suffix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)[-_](mainline|retail|classic|vanilla|tbc|bcc|wrath|wotlk|wotlkc|cata|mists|mop|standard|forever|wowf)$")
            .unwrap()
    })
}

/// "MyAddon_Mainline" -> "MyAddon"
pub fn toc_base(stem: &str) -> String {
    flavor_suffix().replace(stem, "").into_owned()
}

fn tocs_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|x| x.eq_ignore_ascii_case("toc"))
                    .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

pub fn has_toc(dir: &Path) -> bool {
    !tocs_in(dir).is_empty()
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The toc WoW would load for this folder (name must match the folder, optionally with a flavor suffix).
pub fn matching_toc(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let tocs = tocs_in(dir);
    tocs.iter()
        .find(|p| stem(p).eq_ignore_ascii_case(&name))
        .or_else(|| tocs.iter().find(|p| toc_base(&stem(p)).eq_ignore_ascii_case(&name)))
        .cloned()
}

/// Folder name this directory *should* have, based on its toc files.
pub fn intended_folder_name(dir: &Path) -> Option<String> {
    tocs_in(dir).first().map(|p| toc_base(&stem(p)))
}

fn strip_codes(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\|c[0-9a-fA-F]{8}|\|r|\|T[^|]*\|t|\|A[^|]*\|a").unwrap());
    re.replace_all(s, "").trim().to_string()
}

fn parse(path: &Path) -> HashMap<String, String> {
    let bytes = fs::read(path).unwrap_or_default();
    parse_str(&String::from_utf8_lossy(&bytes))
}

pub fn parse_str(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if let Some(rest) = line.strip_prefix("##") {
            if let Some((k, v)) = rest.split_once(':') {
                map.entry(k.trim().to_lowercase())
                    .or_insert_with(|| v.trim().to_string());
            }
        }
    }
    map
}

pub fn read_info(dir: &Path) -> Option<TocInfo> {
    let folder = dir.file_name()?.to_string_lossy().into_owned();
    let toc = matching_toc(dir).or_else(|| tocs_in(dir).into_iter().next())?;
    Some(info_from(folder, &parse(&toc)))
}

fn github_repo(m: &HashMap<String, String>) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)github\.com/([\w.-]+)/([\w-]+(?:\.[\w-]+)*?)(?:\.git)?(?:[/?#\s]|$)").unwrap());
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    keys.into_iter()
        .filter_map(|k| re.captures(&m[k]))
        .map(|c| format!("{}/{}", &c[1], &c[2]))
        .next()
}

/// WoW treats "Dependencies", "RequiredDeps" and any key starting with "Dep" as required.
fn required_deps(m: &HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<&String> = m.keys().filter(|k| k.starts_with("dep") || *k == "requireddeps").collect();
    keys.sort();
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        for d in m[k].split(',').map(str::trim).filter(|d| !d.is_empty()) {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(d)) {
                out.push(d.to_string());
            }
        }
    }
    out
}

pub fn info_from(folder: String, m: &HashMap<String, String>) -> TocInfo {
    let get = |k: &str| m.get(k).filter(|v| !v.is_empty()).cloned();
    TocInfo {
        deps: required_deps(m),
        github: github_repo(m),
        title: get("title").map(|t| strip_codes(&t)).unwrap_or_else(|| folder.clone()),
        version: get("version"),
        interface: get("interface"),
        notes: get("notes").map(|t| strip_codes(&t)),
        curse_id: get("x-curse-project-id").filter(|v| v.bytes().all(|b| b.is_ascii_digit())),
        wowi_id: get("x-wowi-id").filter(|v| v.bytes().all(|b| b.is_ascii_digit())),
        wago_id: get("x-wago-id"),
        folder,
    }
}

/// Every folder in AddOns that has a toc.
pub fn scan(addons_dir: &Path) -> Vec<TocInfo> {
    let mut out: Vec<TocInfo> = fs::read_dir(addons_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| read_info(&p))
        .collect();
    out.sort_by(|a, b| a.folder.to_lowercase().cmp(&b.folder.to_lowercase()));
    out
}
