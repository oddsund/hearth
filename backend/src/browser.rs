//! Opens Hearth's window. Under WSL that's Microsoft Edge in app mode: a plain Windows window
//! with no browser toolbar. Edge is signed by Microsoft, so Smart App Control lets it run, and it
//! draws with the GPU, resizes and scales like any Windows app.
//!
//! HEARTH_BROWSER overrides this: a program to run with the link, or "none" to print the link.

use std::path::Path;
use std::process::Command;

const EDGE: [&str; 2] = [
    "/mnt/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
    "/mnt/c/Program Files/Microsoft/Edge/Application/msedge.exe",
];

pub fn open(url: &str) {
    let cmd = match std::env::var("HEARTH_BROWSER").ok().filter(|b| !b.is_empty()).as_deref() {
        Some("none") => {
            println!("Open Hearth at {url}");
            return;
        }
        Some(program) => {
            let mut c = Command::new(program);
            c.arg(url);
            c
        }
        None if std::env::var_os("WSL_DISTRO_NAME").is_some() => match edge(url) {
            Some(c) => c,
            None => {
                eprintln!("hearth: Microsoft Edge not found. Open this in a browser: {url}");
                return;
            }
        },
        None if cfg!(windows) => {
            let mut c = Command::new("rundll32.exe");
            c.args(["url.dll,FileProtocolHandler", url]);
            c
        }
        None => {
            let mut c = Command::new("xdg-open");
            c.arg(url);
            c
        }
    };
    spawn(cmd, url);
}

fn edge(url: &str) -> Option<Command> {
    let exe = EDGE.into_iter().find(|p| Path::new(p).is_file())?;
    let mut c = Command::new(exe);
    // Windows programs started from a Linux folder warn about it; start them from C:\.
    c.current_dir("/mnt/c");
    c.arg(format!("--app={url}"));
    c.args(["--no-first-run", "--no-default-browser-check", "--window-size=1000,720"]);
    // A profile of its own: your extensions don't run in it, and closing Hearth's window
    // doesn't touch your browsing.
    if let Some(dir) = local_app_data() {
        c.arg(format!("--user-data-dir={dir}\\Hearth\\Edge"));
    }
    Some(c)
}

/// %LOCALAPPDATA% of the Windows user, e.g. C:\Users\you\AppData\Local.
fn local_app_data() -> Option<String> {
    let out = Command::new("/mnt/c/Windows/System32/cmd.exe")
        .current_dir("/mnt/c")
        .args(["/d", "/c", "echo %LOCALAPPDATA%"])
        .output()
        .ok()?;
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (dir.len() > 3 && !dir.contains('%') && !dir.contains('"')).then_some(dir)
}

fn spawn(mut cmd: Command, url: &str) {
    match cmd.spawn() {
        // Reap it when it exits (Edge hands the window to its main process and quits).
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => eprintln!("hearth: couldn't open a window ({e}). Open this in a browser: {url}"),
    }
}
