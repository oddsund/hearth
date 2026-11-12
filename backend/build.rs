//! Bakes a build identifier into the app: git commit, whether the working tree had
//! uncommitted changes, and when it was built. Shown in Hearth's header and Settings.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// git from inside WSL, on a checkout made by Windows git: trust the folder, and compare
/// line endings the way Windows does so CRLF files don't look modified.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(["-c", "safe.directory=*", "-c", "core.autocrlf=true"])
        .args(args)
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let commit = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let built = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    println!("cargo:rustc-env=HEARTH_COMMIT={commit}");
    println!("cargo:rustc-env=HEARTH_DIRTY={dirty}");
    println!("cargo:rustc-env=HEARTH_BUILT={built}");
    // Re-run (so the id stays true) when the commit or any source changes.
    for p in ["../.git/HEAD", "../.git/index", "../.git/refs/heads", "src", "../ui", "Cargo.toml"] {
        println!("cargo:rerun-if-changed={p}");
    }
}
