//! Where addons come from. Preference order: GitHub > WoWInterface > (later) Curse/Wago.

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

pub struct Release {
    pub version: String,
    pub download_url: String,
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

    if lower.contains("curseforge.com") || lower.contains("wago.io") {
        bail!("CurseForge/Wago aren't supported yet. Try the addon's GitHub or WoWInterface page.");
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

    bail!("Didn't recognise that. Paste a GitHub repo URL (or owner/repo) or a WoWInterface addon URL.")
}

pub async fn latest(
    client: &reqwest::Client,
    token: Option<&str>,
    provider: &str,
    source: &str,
    asset_pattern: Option<&str>,
) -> Result<Release> {
    match provider {
        "github" => github_latest(client, token, source, asset_pattern).await,
        "wowi" => wowi_latest(client, source).await,
        p => bail!("Unknown provider '{p}'"),
    }
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
    Ok(Release { version, download_url })
}
