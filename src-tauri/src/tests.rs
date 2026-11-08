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

    let sig = crate::downloads::signature(dl.path());
    assert_eq!(sig, crate::downloads::signature(dl.path()), "stable while nothing changes");
    let found = crate::downloads::scan(dl.path());
    assert_eq!(found.len(), 1, "duplicates collapsed, junk skipped");
    std::fs::remove_file(dl.path().join("notes.zip")).unwrap();
    assert_ne!(sig, crate::downloads::signature(dl.path()), "changes when a zip goes away");
    let c = &found[0];
    assert_eq!(c.duplicates.len(), 1, "the (1) copy is remembered for clean-up");
    assert_ne!(c.duplicates[0], c.path);
    assert_eq!(c.folders, vec!["Foo", "Foo_Options"]);
    assert_eq!(c.main.title, "Foo");
    assert_eq!(c.main.github.as_deref(), Some("someone/Foo"));
    assert_eq!(c.main.supports("16001"), Some(true));
    assert_eq!(c.main.supports("38000"), Some(false));

    let bytes = std::fs::read(&c.path).unwrap();
    let folders = installer::install_bytes(addons.path(), "test-local:Foo", &bytes, &[], None).unwrap();
    assert_eq!(folders.len(), 2);
    assert!(addons.path().join("Foo/Foo.lua").is_file());
}

/// Install v1, update to v2, roll back to v1, roll back again to v2.
#[test]
fn roll_back_swaps_versions() {
    use std::io::Write;
    let addons = tempfile::tempdir().unwrap();
    let id = format!("test-rollback:{}", std::process::id());
    let zip_with = |ver: &str| {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file("Bar/Bar.toc", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(format!("## Version: {ver}\n").as_bytes()).unwrap();
        z.finish().unwrap();
        buf.into_inner()
    };
    let ver_on_disk = || toc::read_info(&addons.path().join("Bar")).unwrap().version.unwrap();
    let f = installer::install_bytes(addons.path(), &id, &zip_with("1.0"), &[], None).unwrap();
    installer::install_bytes(addons.path(), &id, &zip_with("2.0"), &f, Some("1.0")).unwrap();
    assert_eq!(ver_on_disk(), "2.0");
    assert_eq!(installer::latest_backup(&id).unwrap().1.version.as_deref(), Some("1.0"));

    let m = installer::roll_back(addons.path(), &id, &f, "2.0").unwrap();
    assert_eq!((m.version.as_deref(), ver_on_disk().as_str()), (Some("1.0"), "1.0"));
    let m = installer::roll_back(addons.path(), &id, &f, "1.0").unwrap();
    assert_eq!((m.version.as_deref(), ver_on_disk().as_str()), (Some("2.0"), "2.0"));
    let _ = std::fs::remove_dir_all(crate::state::data_dir().join("backup").join(id.replace(':', "_")));
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

/// WoWInterface's API still reports versions, but (since Nov 2026) its download links answer
/// apps with a "log in" page. Hearth must report that clearly instead of a broken-zip error.
#[tokio::test]
#[ignore]
async fn wowinterface_addon() {
    let c = client();
    let rel = providers::latest(&c, None, "wowi", "5108", None, &forever()).await.unwrap();
    println!("5108: {} <- {}", rel.version, rel.download_url);
    match installer::download(&c, &rel.download_url).await {
        Ok(bytes) => assert!(bytes.starts_with(b"PK")),
        Err(e) => assert!(e.downcast_ref::<installer::NotAZip>().is_some(), "unexpected error: {e:#}"),
    }
}

/// The fallback for What's Training when WoWInterface refuses: CurseForge 324944 for _forever_.
#[tokio::test]
#[ignore]
async fn whats_training_on_curseforge() {
    install_into_temp("curse", "324944").await;
}

/// Real review, read-only: What's Training from CurseForge vs. the copy in the real AddOns folder.
#[tokio::test]
#[ignore]
async fn real_review_whats_training() {
    let c = client();
    let rel = providers::latest(&c, None, "curse", "324944", None, &forever()).await.unwrap();
    let stage = installer::stage(&installer::download(&c, &rel.download_url).await.unwrap()).unwrap();
    let addons = crate::state::detect_addons_dir();
    let r = crate::review::review(std::path::Path::new(&addons), &stage, &["WhatsTraining".into()]);
    println!("{} -> {}: {} changed, {} unchanged", addons, rel.version, r.files.len(), r.unchanged);
    for f in r.files.iter().take(12) {
        println!("  {} {} +{} -{}", &f.status[..1].to_uppercase(), f.path, f.added, f.removed);
    }
    for f in &r.findings {
        println!("  [{}] {}:{:?} {}", f.severity, f.path, f.line, f.message);
    }
    assert!(!r.has_danger());
}

/// Leatrix Maps: should pick the -forever build, not retail.
#[tokio::test]
#[ignore]
async fn curseforge_forever_build() {
    let rel = providers::latest(&client(), None, "curse", "298842", None, &forever()).await.unwrap();
    assert!(rel.version.contains("forever"), "got {}", rel.version);
    install_into_temp("curse", "298842").await;
}

#[cfg(not(windows))]
#[test]
fn wsl_paths_to_windows() {
    use std::path::Path;
    assert_eq!(crate::trash::windows_path(Path::new("/mnt/c/Users/x/Downloads/a b.zip")).as_deref(), Some(r"C:\Users\x\Downloads\a b.zip"));
    assert_eq!(crate::trash::windows_path(Path::new("/mnt/d")).as_deref(), Some("D:"));
    assert_eq!(crate::trash::windows_path(Path::new("/root/a.zip")), None);
    assert_eq!(crate::trash::windows_path(Path::new("/mnt/cc/a.zip")), None);
}

#[test]
fn review_lists_changes_and_flags_risky_code() {
    use std::io::Write;
    let addons = tempfile::tempdir().unwrap();
    let foo = addons.path().join("Foo");
    std::fs::create_dir_all(&foo).unwrap();
    std::fs::write(foo.join("Foo.toc"), "## Title: Foo\n## Version: 1\n").unwrap();
    std::fs::write(foo.join("Core.lua"), "local x = 1\nprint(x)\nSendChatMessage('old', 'SAY')\n").unwrap();
    std::fs::write(foo.join("Old.lua"), "-- going away\n").unwrap();

    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opt = zip::write::SimpleFileOptions::default();
    for (name, body) in [
        ("Foo/Foo.toc", "## Title: Foo\n## Version: 2\n"),
        // Unchanged SendChatMessage isn't re-flagged; the new one is.
        ("Foo/Core.lua", "local x = 2\nprint(x)\nSendChatMessage('old', 'SAY')\nSendChatMessage('buy gold at scam.example', 'GUILD')\n"),
        ("Foo/New.lua", "local f = loadstring(blob)\n"),
        ("Foo/helper.exe", "MZ fake program"),
    ] {
        z.start_file(name, opt).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap();
    let stage = installer::stage(&buf.into_inner()).unwrap();

    let r = crate::review::review(addons.path(), &stage, &["Foo".into()]);
    let files: Vec<(&str, &str, usize, usize)> = r.files.iter().map(|f| (f.path.as_str(), f.status, f.added, f.removed)).collect();
    assert_eq!(files, vec![
        ("Foo/Core.lua", "modified", 2, 1),
        ("Foo/Foo.toc", "modified", 1, 1),
        ("Foo/helper.exe", "added", 1, 0),
        ("Foo/New.lua", "added", 1, 0),
        ("Foo/Old.lua", "removed", 0, 1),
    ]);
    assert_eq!(r.unchanged, 0);
    let found: Vec<(&str, &str, Option<usize>)> = r.findings.iter().map(|f| (f.severity, f.path.as_str(), f.line)).collect();
    assert_eq!(found, vec![
        ("danger", "Foo/helper.exe", None),
        ("warn", "Foo/Core.lua", Some(4)),
        ("warn", "Foo/New.lua", Some(1)),
    ]);
    assert!(r.has_danger());
    assert_eq!(crate::review::program_files(&stage), vec!["Foo/helper.exe"]);
    assert!(crate::refuse_programs(&stage).is_err(), "plain Update refuses program files");

    let d = crate::review::file_diff(addons.path(), &stage, &["Foo".into()], "Foo/Core.lua");
    assert!(d.contains("--- a/Foo/Core.lua") && d.contains("+++ b/Foo/Core.lua"), "{d}");
    assert!(d.contains("-local x = 1") && d.contains("+local x = 2") && d.contains("+SendChatMessage('buy gold"), "{d}");
    let removed = crate::review::file_diff(addons.path(), &stage, &["Foo".into()], "Foo/Old.lua");
    assert!(removed.contains("+++ /dev/null") && removed.contains("--- going away"), "{removed}");
}

#[test]
fn addon_list_round_trip() {
    let json = r#"{"hearth":1,"exported_at":1,"addons":[
        {"name":"Leatrix Maps","provider":"curse","source":"298842","url":"https://www.curseforge.com/projects/298842","pinned":true},
        {"name":"Foo","provider":"local","source":"Foo","url":""}]}"#;
    let list: crate::AddonList = serde_json::from_str(json).unwrap();
    assert_eq!(list.addons.len(), 2);
    assert!(list.addons[0].pinned && !list.addons[1].pinned, "missing fields default");
    assert_eq!(list.addons[1].asset_pattern, None);
    let again: crate::AddonList = serde_json::from_slice(&serde_json::to_vec(&list).unwrap()).unwrap();
    assert_eq!(again.addons[0].source, "298842");
}

#[test]
fn changelog_html_to_text() {
    let t = providers::html_to_text("<p>\u{feff}1.2 - Today</p>\n<ul>\n<li>Fixed &amp; improved</li><li>New <b>thing</b></li></ul>");
    assert_eq!(t, "1.2 - Today\n\n• Fixed & improved\n• New thing");
}

/// Real CurseForge release notes for Leatrix Maps' -forever build.
#[tokio::test]
#[ignore]
async fn curseforge_changelog() {
    let rel = providers::latest(&client(), None, "curse", "298842", None, &forever()).await.unwrap();
    let text = providers::fetch_changelog(&client(), rel.changelog_url.as_deref().unwrap()).await.unwrap();
    println!("{}", &text[..text.len().min(300)]);
    assert!(!text.contains('<'), "no HTML left");
    assert!(text.len() > 20);
}

#[test]
fn finds_missing_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let addon = |folder: &str, body: &str| {
        std::fs::create_dir_all(dir.path().join(folder)).unwrap();
        std::fs::write(dir.path().join(folder).join(format!("{folder}.toc")), body).unwrap();
    };
    addon("Foo", "## Dependencies: Foo_Core, LibThing\n## OptionalDeps: Masque\n");
    addon("Foo_Core", "## RequiredDeps: Blizzard_UIPanels, HereLib\n");
    addon("HereLib", "## Title: HereLib\n");
    let tracked = |id: &str, folders: &[&str]| crate::state::TrackedAddon {
        id: id.into(), name: id.into(), provider: "local".into(), source: id.into(), url: String::new(),
        version: "1".into(), folders: folders.iter().map(|s| s.to_string()).collect(),
        asset_pattern: None, installed_at: 0, curse_id: None, pinned: false,
    };
    let st = crate::state::AppState {
        addons_dir: dir.path().to_string_lossy().into(),
        addons: vec![tracked("local:Foo", &["Foo", "Foo_Core"])],
        ..Default::default()
    };
    let zip = crate::downloads::Candidate {
        path: "/dl/LibThing.zip".into(), file_name: "LibThing.zip".into(), modified: 0,
        folders: vec!["LibThing".into()], main: Default::default(), duplicates: vec![],
    };
    let r = crate::missing_deps(&st, &[zip]);
    assert_eq!(r.len(), 1);
    let missing: Vec<(&str, Option<&str>)> = r[0].missing.iter().map(|m| (m.folder.as_str(), m.zip_file.as_deref())).collect();
    // Own folder, installed lib, Blizzard_* and optional deps don't count.
    assert_eq!(missing, vec![("LibThing", Some("LibThing.zip"))]);
}

#[test]
fn groups_hand_installed_folders() {
    let dir = tempfile::tempdir().unwrap();
    let addon = |folder: &str, body: &str| {
        std::fs::create_dir_all(dir.path().join(folder)).unwrap();
        std::fs::write(dir.path().join(folder).join(format!("{folder}.toc")), body).unwrap();
    };
    addon("BigWigs", "## Title: BigWigs\n## Version: v1\n## X-Curse-Project-ID: 2382\n");
    addon("BigWigs_Core", "## Title: BigWigs Core\n");
    addon("BigWigs_Options", "## Title: BigWigs Options\n");
    addon("Clique", "## Title: Clique\n## X-WoWI-ID: 5108\n");
    addon("CliqueExtras", "## Title: Clique extras\n## X-WoWI-ID: 5108\n"); // joins by shared id
    addon("Plain", "## Title: Plain\n");
    let st = crate::state::AppState { addons_dir: dir.path().to_string_lossy().into(), ..Default::default() };
    let mut groups: Vec<(String, Vec<String>, String)> = crate::adoptable(&st)
        .into_iter()
        .map(|g| (g.main.folder, g.folders, g.link_provider))
        .collect();
    groups.sort();
    assert_eq!(groups, vec![
        ("BigWigs".into(), vec!["BigWigs".into(), "BigWigs_Core".into(), "BigWigs_Options".into()], "curse".into()),
        ("Clique".into(), vec!["Clique".into(), "CliqueExtras".into()], "wowi".into()),
        ("Plain".into(), vec!["Plain".into()], "local".into()),
    ]);
}

#[test]
fn settings_snapshot_and_restore() {
    let root = tempfile::tempdir().unwrap();
    let flavor = root.path().join(format!("_hearthtest{}_", std::process::id()));
    let addons = flavor.join("Interface").join("AddOns");
    let sv = flavor.join("WTF").join("Account").join("ME").join("SavedVariables");
    std::fs::create_dir_all(&addons).unwrap();
    std::fs::create_dir_all(&sv).unwrap();
    std::fs::write(sv.join("Foo.lua"), "FooDB = { good = true }").unwrap();

    let snap = crate::wtf::snapshot(&addons, "manual").unwrap().expect("snapshot taken");
    assert_eq!(snap.reason, "manual");
    assert!(crate::wtf::snapshot_if_stale(&addons, std::time::Duration::from_secs(600)).unwrap().is_none(), "fresh one exists");

    std::fs::write(sv.join("Foo.lua"), "FooDB = { broken = true }").unwrap();
    crate::wtf::restore(&addons, std::path::Path::new(&snap.file)).unwrap();
    assert_eq!(std::fs::read_to_string(sv.join("Foo.lua")).unwrap(), "FooDB = { good = true }");
    let list = crate::wtf::list(&addons);
    assert!(list.iter().any(|s| s.reason == "before-restore"), "restore keeps the state it replaced");
    assert!(!flavor.join("WTF.hearth-old").exists() && !flavor.join("WTF.hearth-restore").exists());

    let store = std::path::Path::new(&snap.file).parent().unwrap().to_path_buf();
    let _ = std::fs::remove_dir_all(store);
}

/// Asks Windows whether WoW runs (read-only).
#[test]
#[ignore]
fn real_wow_running() {
    let r = crate::game::wow_running();
    println!("wow running: {r:?}");
    assert!(r.is_some(), "tasklist should be reachable");
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

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    for (name, body) in files {
        z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(body).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

#[test]
fn refuses_unsafe_folder_names() {
    // "...toc" names the folder "..": installing it would replace the Interface folder.
    for toc in ["Evil/...toc", "Evil/..toc"] {
        assert!(installer::stage(&zip_of(&[(toc, b"## Title: E\n")])).is_err(), "{toc}");
    }
    for bad in ["", ".", "..", "a/b", r"a\b", "C:", "Addon.", "Addon "] {
        assert!(!installer::is_folder_name(bad), "{bad:?}");
    }
    assert!(installer::is_folder_name("BigWigs_Core"));
    assert!(installer::is_folder_name("!BugGrabber"));
}

#[test]
fn review_reads_code_with_nul_bytes() {
    // A NUL in a comment must not turn a Lua file into "binary" that skips the checks.
    let addons = tempfile::tempdir().unwrap();
    let stage = installer::stage(&zip_of(&[
        ("Foo/Foo.toc", b"## Title: Foo\n"),
        ("Foo/Core.lua", b"-- \0\nSendChatMessage('hi', 'SAY')\n"),
    ]))
    .unwrap();
    let r = crate::review::review(addons.path(), &stage, &[]);
    assert!(r.files.iter().any(|f| f.path == "Foo/Core.lua" && !f.binary));
    assert!(r.findings.iter().any(|f| f.path == "Foo/Core.lua" && f.severity == "warn"));
}

#[test]
fn review_flags_unchanged_program_files() {
    let addons = tempfile::tempdir().unwrap();
    let foo = addons.path().join("Foo");
    std::fs::create_dir_all(&foo).unwrap();
    std::fs::write(foo.join("Foo.toc"), "## Title: Foo\n").unwrap();
    std::fs::write(foo.join("run.exe"), "MZ").unwrap();
    let stage = installer::stage(&zip_of(&[("Foo/Foo.toc", b"## Title: Foo\n"), ("Foo/run.exe", b"MZ")])).unwrap();
    let r = crate::review::review(addons.path(), &stage, &["Foo".into()]);
    assert!(r.has_danger());
}
