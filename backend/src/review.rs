//! Look at an update before it's installed: which files change (git-style), a unified diff per
//! file, and static checks on the code that is new or changed.
//!
//! WoW runs addon Lua in a sandbox (no files, network or keyboard outside the game), so the
//! checks target what an addon *can* abuse in-game, plus Windows program files hidden in a zip.

use crate::installer::Stage;
use regex::Regex;
use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Files Windows can run. WoW never does, and an addon has no reason to ship one.
const PROGRAM_EXT: &[&str] = &[
    "exe", "dll", "bat", "cmd", "com", "scr", "pif", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh",
    "msi", "msp", "jar", "hta", "lnk", "reg", "cpl", "sys", "ocx", "py", "sh",
];
/// What addons normally contain; anything else that's binary gets a note.
const NORMAL_EXT: &[&str] = &[
    "lua", "xml", "toc", "txt", "md", "tga", "blp", "png", "jpg", "jpeg", "gif", "bmp", "ogg", "mp3", "wav",
    "ttf", "otf", "json", "yml", "yaml", "html", "css", "m2", "skin", "pkgmeta", "editorconfig", "gitignore",
];
const CODE_EXT: &[&str] = &["lua", "xml"];
const MAX_FINDINGS_PER_FILE: usize = 20;

#[derive(Serialize, Clone, Debug)]
pub struct FileChange {
    /// "Folder/sub/file.lua"
    pub path: String,
    /// "added" | "removed" | "modified"
    pub status: &'static str,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Finding {
    /// "danger" (blocks plain Update) | "warn" | "info"
    pub severity: &'static str,
    pub path: String,
    pub line: Option<usize>,
    pub message: String,
    pub snippet: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Review {
    pub files: Vec<FileChange>,
    pub findings: Vec<Finding>,
    pub unchanged: usize,
}

impl Review {
    pub fn has_danger(&self) -> bool {
        self.findings.iter().any(|f| f.severity == "danger")
    }
}

fn ext(path: &str) -> String {
    path.rsplit('/').next().and_then(|n| n.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

pub fn is_program(path: &str) -> bool {
    PROGRAM_EXT.contains(&ext(path).as_str())
}

/// Program files anywhere in an unpacked update (the quick check used by plain Update).
pub fn program_files(stage: &Stage) -> Vec<String> {
    let mut files = BTreeMap::new();
    for (name, dir) in &stage.folders {
        walk(dir, name, &mut files);
    }
    files.into_values().map(|(p, _)| p).filter(|p| is_program(p)).collect()
}

/// lowercased "Folder/rel" -> (display path, file on disk)
fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, (String, PathBuf)>) {
    for e in fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
        let p = e.path();
        let rel = format!("{prefix}/{}", e.file_name().to_string_lossy());
        if p.is_dir() {
            walk(&p, &rel, out);
        } else {
            out.insert(rel.to_lowercase(), (rel, p));
        }
    }
}

type Side = BTreeMap<String, (String, PathBuf)>;

/// Installed files (the folders being replaced) and the update's files.
fn sides(addons_dir: &Path, stage: &Stage, old_folders: &[String]) -> (Side, Side) {
    let mut new = BTreeMap::new();
    for (name, dir) in &stage.folders {
        walk(dir, name, &mut new);
    }
    let mut old = BTreeMap::new();
    let mut folders: Vec<&str> = old_folders.iter().map(String::as_str).collect();
    for (name, _) in &stage.folders {
        if !folders.iter().any(|f| f.eq_ignore_ascii_case(name)) {
            folders.push(name);
        }
    }
    for f in folders {
        let d = addons_dir.join(f);
        if d.is_dir() {
            walk(&d, f, &mut old);
        }
    }
    (old, new)
}

/// Text content, or None for binary files (NUL byte near the start). Code is always text:
/// a NUL in a Lua comment must not hide the file from the checks and the diff.
fn text(path: &Path) -> Option<String> {
    let b = fs::read(path).ok()?;
    let code = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| CODE_EXT.contains(&e.to_ascii_lowercase().as_str()) || e.eq_ignore_ascii_case("toc"));
    if !code && b.iter().take(8000).any(|&c| c == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&b).into_owned())
}

struct Pattern {
    re: Regex,
    severity: &'static str,
    message: &'static str,
}

fn patterns() -> &'static [Pattern] {
    static P: OnceLock<Vec<Pattern>> = OnceLock::new();
    P.get_or_init(|| {
        let p = |re: &str, severity, message| Pattern { re: Regex::new(re).unwrap(), severity, message };
        vec![
            p(r"\bloadstring\s*\(|\bload\s*\(\s*[A-Za-z_]", "warn", "Runs code built from text (loadstring/load). Check where the text comes from."),
            p(r"\bRunScript\s*\(", "warn", "Runs code built from text (RunScript)."),
            p(r"\b(setfenv|getfenv)\s*\(", "info", "Changes or reads a function's environment (setfenv/getfenv); sometimes used to hide what code does."),
            p(r"\bSendChatMessage\s*\(", "warn", "Sends chat messages as you (SendChatMessage)."),
            p(r"\bBNSendWhisper\s*\(|\bBNSendGameData\s*\(", "warn", "Sends Battle.net whispers or data as you."),
            p(r"\b(C_ChatInfo\.)?SendAddonMessage(Logged)?\s*\(", "info", "Sends hidden addon messages to other players (common for sync features)."),
            p(r"\b(SendMail|SetSendMailMoney|SetSendMailCOD|PickupPlayerMoney|SetTradeMoney|AcceptTrade|InitiateTrade)\b", "warn", "Moves gold or items (mail or trade)."),
            p(r"\b(DeleteCursorItem|C_Container\.PickupContainerItem.*Delete)\b", "warn", "Deletes items."),
            p(r"\b(GuildUninvite|GuildDemote|GuildSetLeader|GuildDisband|C_GuildInfo\.Remove)\b", "warn", "Changes guild membership or ranks."),
            p(r"\b(AcceptGroup|InviteUnit|C_PartyInfo\.InviteUnit)\b", "info", "Accepts or sends group invites."),
            p(r"[A-Za-z0-9+/]{200,}={0,2}", "warn", "Long encoded blob (base64-like). Hidden code or data?"),
            p(r"(\\\d{1,3}){24,}|(\\x[0-9a-fA-F]{2}){24,}", "warn", "Long run of escaped bytes. A common way to hide code."),
            p(r"string\.char\s*\(\s*\d+(\s*,\s*\d+){15,}", "warn", "Builds a string from many character codes. A common way to hide code."),
        ]
    })
}

fn scan_lines<'a>(path: &str, lines: impl Iterator<Item = (usize, &'a str)>, out: &mut Vec<Finding>) {
    if !CODE_EXT.contains(&ext(path).as_str()) {
        return;
    }
    let mut n = 0;
    let mut more = 0;
    for (no, line) in lines {
        for p in patterns() {
            if p.re.is_match(line) {
                if n < MAX_FINDINGS_PER_FILE {
                    let t = line.trim();
                    let snippet: String = t.chars().take(160).collect();
                    out.push(Finding { severity: p.severity, path: path.to_string(), line: Some(no), message: p.message.to_string(), snippet });
                    n += 1;
                } else {
                    more += 1;
                }
                break; // one finding per line
            }
        }
    }
    if more > 0 {
        out.push(Finding { severity: "info", path: path.to_string(), line: None, message: format!("…and {more} more matching lines in this file"), snippet: String::new() });
    }
}

fn file_flags(path: &str, binary: bool, out: &mut Vec<Finding>) {
    if is_program(path) {
        out.push(Finding {
            severity: "danger",
            path: path.to_string(),
            line: None,
            message: format!("Windows program file (.{}). WoW never runs these, and an addon has no reason to ship one.", ext(path)),
            snippet: String::new(),
        });
    } else if binary && !NORMAL_EXT.contains(&ext(path).as_str()) {
        out.push(Finding { severity: "info", path: path.to_string(), line: None, message: "Unusual binary file for an addon.".into(), snippet: String::new() });
    }
}

/// Compare the update with what's installed and run the checks on new/changed content.
pub fn review(addons_dir: &Path, stage: &Stage, old_folders: &[String]) -> Review {
    let (old, new) = sides(addons_dir, stage, old_folders);
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut r = Review::default();
    for k in keys {
        match (old.get(k), new.get(k)) {
            (None, Some((path, p))) => {
                let t = text(p);
                file_flags(path, t.is_none(), &mut r.findings);
                if let Some(t) = &t {
                    scan_lines(path, t.lines().enumerate().map(|(i, l)| (i + 1, l)), &mut r.findings);
                }
                let added = t.as_deref().map(|t| t.lines().count()).unwrap_or(0);
                r.files.push(FileChange { path: path.clone(), status: "added", added, removed: 0, binary: t.is_none() });
            }
            (Some((path, p)), None) => {
                let t = text(p);
                let removed = t.as_deref().map(|t| t.lines().count()).unwrap_or(0);
                r.files.push(FileChange { path: path.clone(), status: "removed", added: 0, removed, binary: t.is_none() });
            }
            (Some((_, op)), Some((path, np))) => {
                if fs::read(op).ok() == fs::read(np).ok() {
                    r.unchanged += 1;
                    // A program file is flagged even when this update didn't change it,
                    // so Show diff never lets through what plain Update refuses.
                    file_flags(path, false, &mut r.findings);
                    continue;
                }
                let (ot, nt) = (text(op), text(np));
                file_flags(path, nt.is_none(), &mut r.findings);
                let (mut added, mut removed) = (0, 0);
                if let (Some(a), Some(b)) = (&ot, &nt) {
                    let d = TextDiff::from_lines(a.as_str(), b.as_str());
                    let mut new_lines = Vec::new();
                    for c in d.iter_all_changes() {
                        match c.tag() {
                            ChangeTag::Insert => {
                                added += 1;
                                if let Some(i) = c.new_index() {
                                    new_lines.push((i + 1, c.value().trim_end_matches(['\r', '\n'])));
                                }
                            }
                            ChangeTag::Delete => removed += 1,
                            ChangeTag::Equal => {}
                        }
                    }
                    scan_lines(path, new_lines.into_iter(), &mut r.findings);
                }
                r.files.push(FileChange { path: path.clone(), status: "modified", added, removed, binary: nt.is_none() || ot.is_none() });
            }
            (None, None) => {}
        }
    }
    let rank = |s: &str| match s { "danger" => 0, "warn" => 1, _ => 2 };
    r.findings.sort_by(|a, b| rank(a.severity).cmp(&rank(b.severity)).then(a.path.cmp(&b.path)).then(a.line.cmp(&b.line)));
    r
}

/// Unified diff (git-style) for one file of the update.
pub fn file_diff(addons_dir: &Path, stage: &Stage, old_folders: &[String], path: &str) -> String {
    let (old, new) = sides(addons_dir, stage, old_folders);
    let k = path.to_lowercase();
    let (o, n) = (old.get(&k).map(|(_, p)| p), new.get(&k).map(|(_, p)| p));
    if o.is_none() && n.is_none() {
        return format!("{path}: not part of this update");
    }
    let size = |p: Option<&PathBuf>| p.and_then(|p| fs::metadata(p).ok()).map(|m| m.len());
    let (ot, nt) = (o.map(|p| text(p)), n.map(|p| text(p)));
    if matches!(ot, Some(None)) || matches!(nt, Some(None)) {
        let s = |x: Option<u64>| x.map(|b| format!("{b} bytes")).unwrap_or_else(|| "none".into());
        return format!("Binary file {path}\n{} -> {}", s(size(o)), s(size(n)));
    }
    let a = ot.flatten().unwrap_or_default();
    let b = nt.flatten().unwrap_or_default();
    let d = TextDiff::from_lines(a.as_str(), b.as_str());
    let (ah, bh) = (
        if o.is_some() { format!("a/{path}") } else { "/dev/null".into() },
        if n.is_some() { format!("b/{path}") } else { "/dev/null".into() },
    );
    let out = d.unified_diff().context_radius(3).header(&ah, &bh).to_string();
    if out.trim().is_empty() { format!("{path}: no changes") } else { out }
}
