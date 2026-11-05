//! Which game is the AddOns folder for? Read from Blizzard's own files, no network.
//!   <root>/.build.info              pipe-separated table: Product, Version, ...
//!   <root>/_forever_/.flavor.info   "Product Flavor!STRING:0\nwowf"

use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct GameInfo {
    /// e.g. "1.60.1" (build number stripped)
    pub version: Option<String>,
    /// e.g. "forever", from the "_forever_" folder name
    pub flavor: Option<String>,
}

impl GameInfo {
    /// "1.60.1" -> "1.60"; files are tagged per patch, so match on major.minor.
    pub fn major_minor(&self) -> Option<String> {
        let v = self.version.as_deref()?;
        let parts: Vec<&str> = v.split('.').collect();
        (parts.len() >= 2).then(|| format!("{}.{}", parts[0], parts[1]))
    }
}

pub fn detect(addons_dir: &Path) -> GameInfo {
    let Some(flavor_dir) = addons_dir.parent().and_then(|p| p.parent()) else {
        return GameInfo::default();
    };
    let flavor = flavor_dir
        .file_name()
        .map(|n| n.to_string_lossy().trim_matches('_').to_lowercase())
        .filter(|s| !s.is_empty());
    let product = fs::read_to_string(flavor_dir.join(".flavor.info"))
        .ok()
        .and_then(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).last().map(String::from));
    let version = match (&product, flavor_dir.parent()) {
        (Some(prod), Some(root)) => build_version(&root.join(".build.info"), prod),
        _ => None,
    };
    GameInfo { version, flavor }
}

fn build_version(path: &Path, product: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()?
        .split('|')
        .map(|h| h.split('!').next().unwrap_or("").to_lowercase())
        .collect();
    let col = |name: &str| header.iter().position(|h| h == name);
    let (pc, vc) = (col("product")?, col("version")?);
    for line in lines {
        let cells: Vec<&str> = line.split('|').collect();
        if cells.get(pc).map(|p| p.trim()) == Some(product) {
            let full = cells.get(vc)?.trim();
            let parts: Vec<&str> = full.split('.').collect();
            // "1.60.1.70790" -> "1.60.1"
            return Some(if parts.len() > 3 { parts[..3].join(".") } else { full.to_string() });
        }
    }
    None
}
