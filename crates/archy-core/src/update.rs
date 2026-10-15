//! Checking GitHub for a newer Archy, and fetching its installer.
//!
//! The only time Archy goes online after installing, and only while "Check for
//! updates" is on. It asks GitHub's public release list for this project (no
//! account, nothing about the PC is sent beyond a normal web request).
//! Installers are only ever downloaded from github.com and must match the
//! SHA-256 fingerprint GitHub lists for the file; one without a fingerprint is
//! never offered. Pre-releases (betas) are never offered.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const RELEASES: &str = "https://api.github.com/repos/hasthij/Archy/releases?per_page=10";
const ASSET: &str = "Archy-Setup.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Update {
    /// e.g. "0.2.1"
    pub version: String,
    /// The release page, for "what's new".
    pub page: String,
    /// What's new (the release notes, Markdown, shortened).
    pub notes: String,
    #[serde(skip)]
    download: String,
    #[serde(skip)]
    sha256: String,
}

/// (major, minor, patch) from "v0.2.1" / "0.2.1".
fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().trim_start_matches(['v', 'V']).split('.').map(|p| {
        p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok()
    });
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

/// Windows' own curl (System32), not whatever is first on PATH.
fn system_tool(name: &str) -> std::path::PathBuf {
    let path = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| r"C:\Windows".into())
        .join("System32")
        .join(name);
    if path.exists() { path } else { name.into() }
}

fn curl() -> Command {
    let mut c = Command::new(system_tool("curl.exe"));
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// The newest release if it is newer than `current`. `Ok(None)`: up to date.
pub fn check(current: &str) -> Result<Option<Update>, String> {
    let out = curl()
        .args(["-fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: Archy", RELEASES])
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        return Err("couldn't reach GitHub".into());
    }
    let releases: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    newest(&releases, current)
}

/// The update to offer from GitHub's release list, if any: the newest release
/// above `current` that is published (not a draft), not a pre-release, has the
/// installer, from github.com, with a SHA-256 fingerprint.
fn newest(releases: &serde_json::Value, current: &str) -> Result<Option<Update>, String> {
    let current = parse(current).ok_or("bad version")?;
    let newest = releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["draft"].as_bool().unwrap_or(true))
        // A beta must never reach everyone as an update.
        .filter(|r| !r["prerelease"].as_bool().unwrap_or(true))
        .filter_map(|r| Some((parse(r["tag_name"].as_str()?)?, r)))
        .max_by_key(|(v, _)| *v);
    let Some((version, release)) = newest else { return Ok(None) };
    if version <= current {
        return Ok(None);
    }
    let Some(asset) = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str() == Some(ASSET))
    else {
        return Ok(None); // a release without an installer: nothing to offer
    };
    let download = asset["browser_download_url"].as_str().unwrap_or_default().to_string();
    if !download.starts_with("https://github.com/") {
        return Err("unexpected download address".into());
    }
    // No fingerprint, no update: the installer would run unchecked.
    let Some(sha256) = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .map(str::to_lowercase)
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
    else {
        return Err("the new version has no fingerprint to check it against; download it from the Archy website instead".into());
    };
    Ok(Some(Update {
        version: format!("{}.{}.{}", version.0, version.1, version.2),
        page: release["html_url"].as_str().unwrap_or_default().to_string(),
        notes: release["body"].as_str().unwrap_or_default().chars().take(3000).collect(),
        download,
        sha256,
    }))
}

/// Downloads the update's installer to the temp folder and verifies it.
pub fn download(update: &Update) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("Archy-Setup-{}.exe", update.version));
    let _ = std::fs::remove_file(&path);
    let status = curl()
        .args(["-fsSL", "--retry", "2", "--max-time", "300", "-o"])
        .arg(&path)
        .arg(&update.download)
        .status()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !status.success() {
        return Err("the download failed".into());
    }
    {
        let expected = &update.sha256;
        let out = Command::new(system_tool("certutil.exe"))
            .creation_flags(CREATE_NO_WINDOW)
            .arg("-hashfile")
            .arg(&path)
            .arg("SHA256")
            .output()
            .map_err(|e| e.to_string())?;
        let actual = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().replace(' ', "").to_lowercase())
            .find(|l| l.len() == 64 && l.chars().all(|c| c.is_ascii_hexdigit()));
        if expected.is_empty() || actual.as_deref() != Some(expected.as_str()) {
            let _ = std::fs::remove_file(&path);
            return Err("the download didn't match GitHub's fingerprint, so it wasn't run".into());
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert_eq!(super::parse("v0.2.1"), Some((0, 2, 1)));
        assert_eq!(super::parse("0.10"), Some((0, 10, 0)));
        assert!(super::parse("v0.2.0") > super::parse("v0.1.9"));
        assert_eq!(super::parse("beta"), None);
    }

    fn release(tag: &str, prerelease: bool, digest: Option<&str>) -> serde_json::Value {
        let mut asset = serde_json::json!({
            "name": "Archy-Setup.exe",
            "browser_download_url": format!("https://github.com/hasthij/Archy/releases/download/{tag}/Archy-Setup.exe"),
        });
        if let Some(d) = digest {
            asset["digest"] = serde_json::json!(d);
        }
        serde_json::json!({ "tag_name": tag, "draft": false, "prerelease": prerelease, "assets": [asset] })
    }

    const HASH: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn offers_the_newest_stable_release() {
        let list = serde_json::json!([release("v0.5.0", false, Some(HASH)), release("v0.4.0", false, Some(HASH))]);
        assert_eq!(super::newest(&list, "0.4.0").unwrap().unwrap().version, "0.5.0");
        assert!(super::newest(&list, "0.5.0").unwrap().is_none());
    }

    #[test]
    fn never_offers_a_beta() {
        let list = serde_json::json!([release("v0.6.0", true, Some(HASH)), release("v0.5.0", false, Some(HASH))]);
        assert_eq!(super::newest(&list, "0.4.0").unwrap().unwrap().version, "0.5.0");
    }

    #[test]
    fn never_offers_an_installer_without_a_fingerprint() {
        let list = serde_json::json!([release("v0.5.0", false, None)]);
        assert!(super::newest(&list, "0.4.0").is_err());
    }
}

#[cfg(test)]
mod live {
    /// Real GitHub check + download: `cargo test -p archy-core live -- --ignored`.
    #[test]
    #[ignore]
    fn offers_newer_and_verifies_download() {
        assert!(super::check("99.0.0").unwrap().is_none());
        let u = super::check("0.1.0").unwrap().expect("a newer release");
        println!("offered {} from {}", u.version, u.download);
        assert_eq!(u.sha256.len(), 64);
        let path = super::download(&u).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
        let _ = std::fs::remove_file(path);
    }
}
