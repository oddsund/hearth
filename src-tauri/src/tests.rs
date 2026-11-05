//! `cargo test` runs the offline tests; `cargo test -- --ignored` also hits GitHub/WoWInterface.

use crate::{installer, providers, toc};

#[test]
fn parses_urls() {
    let p = providers::parse_url("https://github.com/BigWigsMods/BigWigs/releases").unwrap();
    assert_eq!((p.provider, p.source.as_str()), ("github", "BigWigsMods/BigWigs"));
    let p = providers::parse_url("BigWigsMods/BigWigs.git").unwrap();
    assert_eq!(p.source, "BigWigsMods/BigWigs");
    let p = providers::parse_url("https://www.wowinterface.com/downloads/info5108-Clique.html").unwrap();
    assert_eq!((p.provider, p.source.as_str()), ("wowi", "5108"));
    assert!(providers::parse_url("https://www.curseforge.com/wow/addons/details").is_err());
    let p = providers::parse_url(" 298842 ").unwrap();
    assert_eq!((p.provider, p.source.as_str()), ("curse", "298842"));
    let p = providers::parse_url("https://www.curseforge.com/projects/298842").unwrap();
    assert_eq!(p.source, "298842");
    assert!(providers::parse_url("hello").is_err());
}

fn cf(id: u64, name: &str, versions: &[&str], release_type: u8) -> providers::CfFile {
    providers::CfFile {
        id,
        display_name: name.trim_end_matches(".zip").into(),
        file_name: name.into(),
        game_versions: versions.iter().map(|s| s.to_string()).collect(),
        release_type,
    }
}

#[test]
fn curse_picks_file_for_this_game() {
    use crate::game::GameInfo;
    // Newest first, like the API returns them.
    let files = vec![
        cf(5, "Addon-2.0-beta.zip", &["1.60.1"], 2),
        cf(4, "Addon-2.0-titan.zip", &["3.80.0"], 1),
        cf(3, "Addon-1.9-forever.zip", &["1.60.1"], 1),
        cf(2, "Addon-12.1.zip", &["12.1.5"], 1),
    ];
    let forever = GameInfo { version: Some("1.60.2".into()), flavor: Some("forever".into()) };
    assert_eq!(providers::pick_curse(&files, None, &forever).unwrap().id, 3, "stable, tagged 1.60.x");
    let retail = GameInfo { version: Some("12.1.5".into()), flavor: Some("retail".into()) };
    assert_eq!(providers::pick_curse(&files, None, &retail).unwrap().id, 2);
    let untagged = GameInfo { version: None, flavor: Some("forever".into()) };
    assert_eq!(providers::pick_curse(&files, None, &untagged).unwrap().id, 3, "flavor in file name");
    let unknown = GameInfo { version: Some("9.9.9".into()), flavor: None };
    assert!(providers::pick_curse(&files, None, &unknown).is_err(), "never guess another flavor");
    assert_eq!(providers::pick_curse(&files, Some("titan"), &forever).unwrap().id, 4);
}

#[test]
fn toc_base_strips_flavor() {
    assert_eq!(toc::toc_base("Details_Mainline"), "Details");
    assert_eq!(toc::toc_base("BigWigs-Classic"), "BigWigs");
    assert_eq!(toc::toc_base("Plain"), "Plain");
}

#[test]
fn version_compare() {
    use std::cmp::Ordering::*;
    use crate::downloads::{cmp_versions, same_version};
    assert_eq!(cmp_versions("1.2.10", "v1.2.9"), Some(Greater));
    assert_eq!(cmp_versions("12.1.16", "12.1.16.0"), Some(Equal));
    assert_eq!(cmp_versions("340", "341"), Some(Less));
    assert_eq!(cmp_versions("abc", "1"), None);
    assert!(same_version("v1.0.7", "1.0.7"));
}

/// Builds a zip like a real download and checks discovery + local install.
#[test]
fn local_zip_discovery_and_install() {
    use std::io::Write;
    let dl = tempfile::tempdir().unwrap();
    let addons = tempfile::tempdir().unwrap();
    let make = |name: &str, toc_body: &str| {
        let f = std::fs::File::create(dl.path().join(name)).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opt = zip::write::SimpleFileOptions::default();
        z.start_file("Foo/Foo.toc", opt).unwrap();
        z.write_all(toc_body.as_bytes()).unwrap();
        z.start_file("Foo/Foo.lua", opt).unwrap();
        z.write_all(b"-- hi").unwrap();
        z.start_file("Foo_Options/Foo_Options.toc", opt).unwrap();
        z.write_all(b"## Title: Foo Options\n").unwrap();
        z.finish().unwrap();
    };
    let body = "## Interface: 120105, 16001\n## Title: |cff00ff00Foo|r\n## Version: 1.2.0\n## X-WoWI-ID: 999\n## X-Website: https://github.com/someone/Foo\n";
    make("Foo-1.2.0.zip", body);
    make("Foo-1.2.0(1).zip", body); // browser duplicate
    std::fs::write(dl.path().join("notes.zip"), b"not a zip").unwrap();

    let found = crate::downloads::scan(dl.path());
    assert_eq!(found.len(), 1, "duplicates collapsed, junk skipped");
    let c = &found[0];
    assert_eq!(c.folders, vec!["Foo", "Foo_Options"]);
    assert_eq!(c.main.title, "Foo");
    assert_eq!(c.main.github.as_deref(), Some("someone/Foo"));
    assert_eq!(c.main.supports("16001"), Some(true));
    assert_eq!(c.main.supports("38000"), Some(false));

    let bytes = std::fs::read(&c.path).unwrap();
    let folders = installer::install_bytes(addons.path(), "test-local:Foo", &bytes, &[]).unwrap();
    assert_eq!(folders.len(), 2);
    assert!(addons.path().join("Foo/Foo.lua").is_file());
}

/// Read-only look at the real Downloads folder.
#[test]
#[ignore]
fn real_downloads_scan() {
    let dir = crate::state::detect_downloads_dir();
    println!("downloads: {dir}");
    for c in crate::downloads::scan(std::path::Path::new(&dir)) {
        println!(
            "{:<32} {:<22} v{:<10} 16001={:?} wowi={:?} gh={:?} folders={:?}",
            c.file_name, c.main.title, c.main.version.as_deref().unwrap_or("?"),
            c.main.supports("16001"), c.main.wowi_id, c.main.github, c.folders
        );
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().user_agent("hearth-test").build().unwrap()
}

fn forever() -> crate::game::GameInfo {
    crate::game::GameInfo { version: Some("1.60.1".into()), flavor: Some("forever".into()) }
}

async fn install_into_temp(provider: &str, source: &str) {
    let c = client();
    let rel = providers::latest(&c, None, provider, source, None, &forever()).await.unwrap();
    println!("{source}: {} <- {}", rel.version, rel.download_url);
    let dir = tempfile::tempdir().unwrap();
    let id = format!("test-{provider}:{source}");
    let folders = installer::install(&c, dir.path(), &id, &rel.download_url, &[]).await.unwrap();
    println!("installed folders: {folders:?}");
    assert!(!folders.is_empty());
    for f in &folders {
        assert!(toc::matching_toc(&dir.path().join(f)).is_some(), "{f} has no matching toc");
    }
    // Re-install over itself = the update path (backup + swap).
    let again = installer::install(&c, dir.path(), &id, &rel.download_url, &folders).await.unwrap();
    assert_eq!(again.len(), folders.len());
    println!("scan: {:#?}", toc::scan(dir.path()).first());
}

#[tokio::test]
#[ignore]
async fn github_packaged_release() {
    install_into_temp("github", "BigWigsMods/BigWigs").await;
}

#[tokio::test]
#[ignore]
async fn wowinterface_addon() {
    install_into_temp("wowi", "5108").await;
}

/// Leatrix Maps: should pick the -forever build, not retail.
#[tokio::test]
#[ignore]
async fn curseforge_forever_build() {
    let rel = providers::latest(&client(), None, "curse", "298842", None, &forever()).await.unwrap();
    assert!(rel.version.contains("forever"), "got {}", rel.version);
    install_into_temp("curse", "298842").await;
}

/// Reads the real WoW install (read-only).
#[test]
#[ignore]
fn real_game_detection() {
    let dir = crate::state::detect_addons_dir();
    let g = crate::game::detect(std::path::Path::new(&dir));
    println!("{dir}: {g:?}");
    assert!(g.version.is_some());
}
