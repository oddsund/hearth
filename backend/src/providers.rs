//! Where addons come from. Preference order: GitHub > WoWInterface > CurseForge (Wago isn't supported).

use crate::game::GameInfo;
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

#[derive(Default)]
pub struct Release {
    pub version: String,
    pub download_url: String,
    /// Release notes, when the source includes them with the release.
    pub changelog: Option<String>,
    /// Where to fetch the notes separately (CurseForge), when they aren't included.
    pub changelog_url: Option<String>,
}

/// CurseForge changelogs are HTML. Turn them into readable plain text.
pub fn html_to_text(html: &str) -> String {
    let breaks = Regex::new(r"(?i)<br\s*/?>|</p>|</li>|</h\d>|</div>").unwrap();
    let bullets = Regex::new(r"(?i)<li[^>]*>").unwrap();
    let tags = Regex::new(r"<[^>]+>").unwrap();
    let blank = Regex::new(r"\n{3,}").unwrap();
    let s = breaks.replace_all(html, "\n");
    let s = bullets.replace_all(&s, "• ");
    let s = tags.replace_all(&s, "");
    let s = s
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace('\u{feff}', "");
    blank.replace_all(s.trim(), "\n\n").into_owned()
}

/// Fetch notes that weren't included with the release (CurseForge's change-log endpoint).
pub async fn fetch_changelog(client: &reqwest::Client, url: &str) -> Result<String> {
    let v: Value = client.get(url).send().await?.error_for_status()?.json().await?;
    Ok(html_to_text(v["changelogBody"].as_str().unwrap_or("")))
}

pub struct Parsed {
    pub provider: &'static str,
    pub source: String,
    pub url: String,
}

/// Turn whatever the user pasted into a provider + source id.
pub fn parse_url(input: &str) -> Result<Parsed> {
    let s = input.trim();
    let lower = s.to_lowercase();

    if lower.contains("wago.io") {
        bail!("Wago isn't supported. Try the addon's GitHub, WoWInterface or CurseForge project ID.");
    }

    // CurseForge: pages are behind bot protection, so we need the numeric project ID.
    let curse_id = Regex::new(r"(?i)(?:curseforge\.com/projects/|projectid=)(\d+)").unwrap();
    let digits = Regex::new(r"^\d{3,}$").unwrap();
    let id = curse_id
        .captures(s)
        .map(|c| c[1].to_string())
        .or_else(|| digits.is_match(s).then(|| s.to_string()));
    if let Some(id) = id {
        return Ok(Parsed {
            provider: "curse",
            url: format!("https://www.curseforge.com/projects/{id}"),
            source: id,
        });
    }
    if lower.contains("curseforge.com") {
        bail!("CurseForge pages can't be read automatically. Paste the Project ID instead (in the \"About Project\" box on the addon's page).");
    }

    if lower.contains("wowinterface.com") {
        let re = Regex::new(r"(?i)(?:info|id=)(\d+)").unwrap();
        let id = re
            .captures(s)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| anyhow!("Couldn't find an addon id in that WoWInterface URL"))?;
        return Ok(Parsed {
            provider: "wowi",
            url: format!("https://www.wowinterface.com/downloads/info{id}"),
            source: id,
        });
    }

    let gh = Regex::new(r"(?i)^(?:https?://)?(?:www\.)?github\.com/([\w.-]+)/([\w.-]+?)(?:\.git)?(?:[/?#].*)?$").unwrap();
    let bare = Regex::new(r"^([\w-]+)/([\w.-]+?)(?:\.git)?$").unwrap();
    if let Some(c) = gh.captures(s).or_else(|| bare.captures(s)) {
        let source = format!("{}/{}", &c[1], &c[2]);
        return Ok(Parsed {
            provider: "github",
            url: format!("https://github.com/{source}"),
            source,
        });
    }

    bail!("Didn't recognise that. Paste a GitHub repo URL (or owner/repo), a WoWInterface addon URL, or a CurseForge project ID.")
}

pub async fn latest(
    client: &reqwest::Client,
    token: Option<&str>,
    provider: &str,
    source: &str,
    asset_pattern: Option<&str>,
    game: &GameInfo,
) -> Result<Release> {
    match provider {
        "github" => github_latest(client, token, source, asset_pattern).await,
        "wowi" => wowi_latest(client, source).await,
        "curse" => curse_latest(client, source, asset_pattern, game).await,
        p => bail!("Unknown provider '{p}'"),
    }
}

// ------------------------------------------------------------ CurseForge
// The JSON endpoint curseforge.com's own pages use. Not an official API: may change.

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfFile {
    pub id: u64,
    pub display_name: String,
    pub file_name: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    /// 1 = release, 2 = beta, 3 = alpha
    #[serde(default)]
    pub release_type: u8,
}

#[derive(Deserialize)]
struct CfFiles {
    data: Vec<CfFile>,
}

/// Newest file for this game: stable before beta; game-version tag first, flavor in file name second.
pub fn pick_curse<'a>(files: &'a [CfFile], pattern: Option<&str>, game: &GameInfo) -> Result<&'a CfFile> {
    let mm = game.major_minor();
    let for_game = |f: &CfFile| {
        mm.as_ref()
            .map(|m| f.game_versions.iter().any(|v| v == m || v.starts_with(&format!("{m}."))))
            .unwrap_or(false)
    };
    let for_flavor = |f: &CfFile| {
        game.flavor
            .as_ref()
            .map(|fl| f.file_name.to_lowercase().contains(fl.as_str()))
            .unwrap_or(false)
    };
    let pat = match pattern.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(Regex::new(&format!("(?i){p}")).context("Invalid asset pattern")?),
        None => None,
    };
    for stable_only in [true, false] {
        let pool: Vec<&CfFile> = files
            .iter()
            .filter(|f| !stable_only || f.release_type == 1)
            .collect();
        if let Some(re) = &pat {
            if let Some(f) = pool.iter().find(|f| re.is_match(&f.file_name)) {
                return Ok(f);
            }
            continue;
        }
        if let Some(f) = pool.iter().find(|f| for_game(f)).or_else(|| pool.iter().find(|f| for_flavor(f))) {
            return Ok(f);
        }
        if mm.is_none() && game.flavor.is_none() {
            if let Some(f) = pool.first() {
                return Ok(f);
            }
        }
    }
    match (&game.version, pattern) {
        (_, Some(_)) => bail!("No CurseForge file matches the asset pattern"),
        (Some(v), _) => bail!("No CurseForge file is tagged for game version {v}"),
        _ => bail!("CurseForge has no files for this project"),
    }
}

async fn curse_latest(client: &reqwest::Client, id: &str, pattern: Option<&str>, game: &GameInfo) -> Result<Release> {
    let url = format!(
        "https://www.curseforge.com/api/v1/mods/{id}/files?pageIndex=0&pageSize=50&sort=dateCreated&sortDescending=true"
    );
    let resp = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
        .context("Couldn't reach CurseForge")?;
    match resp.status().as_u16() {
        200 => {}
        404 => bail!("CurseForge project {id} not found"),
        403 => bail!("CurseForge refused the request (its bot protection may have changed)"),
        s => bail!("CurseForge returned HTTP {s}"),
    }
    let files: CfFiles = resp
        .json()
        .await
        .context("CurseForge's response format changed")?;
    let f = pick_curse(&files.data, pattern, game)?;
    Ok(Release {
        version: f.display_name.clone(),
        download_url: format!("https://www.curseforge.com/api/v1/mods/{id}/files/{}/download", f.id),
        changelog_url: Some(format!("https://www.curseforge.com/api/v1/mods/{id}/files/{}/change-log", f.id)),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- GitHub

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
    zipball_url: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

async fn gh_get(client: &reqwest::Client, token: Option<&str>, url: &str) -> Result<reqwest::Response> {
    let mut req = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.context("Couldn't reach GitHub")?;
    let status = resp.status();
    if status.as_u16() == 404 {
        bail!("Not found on GitHub");
    }
    if status.as_u16() == 403 || status.as_u16() == 429 {
        bail!("GitHub rate limit hit (60 requests/hour without a token). Add a token in Settings.");
    }
    if !status.is_success() {
        bail!("GitHub returned {status}");
    }
    Ok(resp)
}

/// Pick the right zip from a release. Packager releases often ship one zip per game flavor.
fn pick_asset<'a>(assets: &'a [GhAsset], pattern: Option<&str>) -> Result<Option<&'a GhAsset>> {
    let zips: Vec<&GhAsset> = assets
        .iter()
        .filter(|a| a.name.to_lowercase().ends_with(".zip"))
        .collect();
    if let Some(p) = pattern.map(str::trim).filter(|p| !p.is_empty()) {
        let re = Regex::new(&format!("(?i){p}")).context("Invalid asset pattern")?;
        return Ok(zips.into_iter().find(|a| re.is_match(&a.name)));
    }
    let other_flavor = Regex::new(
        r"(?i)[-_.](classic|bcc|tbc|wrath|wotlk|wotlkc|cata|mists|mop|vanilla|era|nolib)(?:[-_.]|$)",
    )
    .unwrap();
    Ok(zips
        .iter()
        .find(|a| !other_flavor.is_match(a.name.trim_end_matches(".zip")))
        .or_else(|| zips.first())
        .copied())
}

async fn github_latest(
    client: &reqwest::Client,
    token: Option<&str>,
    source: &str,
    asset_pattern: Option<&str>,
) -> Result<Release> {
    let url = format!("https://api.github.com/repos/{source}/releases?per_page=10");
    let releases: Vec<GhRelease> = gh_get(client, token, &url)
        .await
        .with_context(|| format!("Fetching releases for {source}"))?
        .json()
        .await?;

    let rel = releases
        .iter()
        .find(|r| !r.draft && !r.prerelease)
        .or_else(|| releases.iter().find(|r| !r.draft));

    if let Some(rel) = rel {
        if let Some(asset) = pick_asset(&rel.assets, asset_pattern)? {
            return Ok(Release {
                version: rel.tag_name.clone(),
                download_url: asset.browser_download_url.clone(),
                changelog: rel.body.clone(),
                ..Default::default()
            });
        }
        if asset_pattern.is_some() {
            bail!("No release asset of {} matches the asset pattern", rel.tag_name);
        }
        if let Some(zb) = &rel.zipball_url {
            // No packaged zip: fall back to the source archive.
            return Ok(Release {
                version: rel.tag_name.clone(),
                download_url: zb.clone(),
                changelog: rel.body.clone(),
                ..Default::default()
            });
        }
    }

    // No releases at all: track the default branch by commit.
    let commits: Value = gh_get(client, token, &format!("https://api.github.com/repos/{source}/commits?per_page=1"))
        .await?
        .json()
        .await?;
    let sha = commits[0]["sha"]
        .as_str()
        .ok_or_else(|| anyhow!("{source} has no releases or commits"))?;
    Ok(Release {
        version: format!("commit {}", &sha[..7.min(sha.len())]),
        download_url: format!("https://api.github.com/repos/{source}/zipball/{sha}"),
        changelog: commits[0]["commit"]["message"].as_str().map(String::from),
        ..Default::default()
    })
}

// ---------------------------------------------------------- WoWInterface

async fn wowi_latest(client: &reqwest::Client, id: &str) -> Result<Release> {
    let url = format!("https://api.mmoui.com/v4/game/WOW/filedetails/{id}.json");
    let v: Value = client
        .get(&url)
        .send()
        .await
        .context("Couldn't reach WoWInterface")?
        .error_for_status()?
        .json()
        .await?;
    if let Some(e) = v["ERROR"].as_str() {
        bail!("WoWInterface addon {id}: {e}");
    }
    let item = v
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("WoWInterface addon {id} not found"))?;
    let version = item["version"].as_str().unwrap_or("unknown").to_string();
    let download_url = item["downloadUri"]
        .as_str()
        .ok_or_else(|| anyhow!("WoWInterface gave no download link for {id}"))?
        .to_string();
    let changelog = item["changeLog"].as_str().map(|s| s.replace("\r\n", "\n"));
    Ok(Release { version, download_url, changelog, ..Default::default() })
}
