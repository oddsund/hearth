//! Send files to the Windows Recycle Bin (recoverable, unlike a delete).
//! From WSL this goes through powershell.exe and VisualBasic's FileSystem.DeleteFile.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// "/mnt/c/Users/x/a.zip" -> "C:\Users\x\a.zip". On Windows the path is used as is.
pub fn windows_path(p: &Path) -> Option<String> {
    let s = p.to_str()?;
    if cfg!(windows) {
        return Some(s.to_string());
    }
    let rest = s.strip_prefix("/mnt/")?;
    let mut chars = rest.chars();
    let drive = chars.next().filter(|c| c.is_ascii_alphabetic())?;
    let tail = chars.as_str();
    if !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    Some(format!("{}:{}", drive.to_ascii_uppercase(), tail.replace('/', "\\")))
}

pub fn recycle(paths: &[&Path]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let ps = if cfg!(windows) {
        "powershell.exe"
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"
    } else {
        bail!("The Recycle Bin is only available on Windows");
    };
    let mut list = Vec::new();
    for p in paths {
        let w = windows_path(p).with_context(|| format!("{} isn't on a Windows drive", p.display()))?;
        list.push(format!("'{}'", w.replace('\'', "''"))); // PowerShell single-quoted literal
    }
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n\
         Add-Type -AssemblyName Microsoft.VisualBasic\n\
         foreach ($p in @({})) {{ [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($p, 'OnlyErrorDialogs', 'SendToRecycleBin') }}\n",
        list.join(", ")
    );
    let mut child = Command::new(ps)
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("Couldn't start PowerShell")?;
    child.stdin.take().context("PowerShell stdin")?.write_all(script.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("PowerShell: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
