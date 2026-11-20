//! A newer version from sshremote.dev: its list of downloads is looked at (at most once a day by
//! the window), its signature checked with the registry's key under a namespace of its own, and
//! the file for this system, checked against the list, put in place of this program when asked.

use std::path::{Path, PathBuf};

const INDEX: &str = "https://sshremote.dev/download/index.json";
const NAMESPACE: &str = "sshremote-release";

/// A release newer than this program, and where its file for this system is.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// The list's address: the site's, or another a setting names (a company's own mirror, a test).
fn index_url() -> String {
    crate::hosts::setting("update_url").filter(|u| !u.trim().is_empty()).unwrap_or_else(|| INDEX.to_string())
}

/// Whether version `theirs` comes after `ours`, number by number ("0.25.10" after "0.25.9").
pub fn newer(theirs: &str, ours: &str) -> bool {
    let parts = |v: &str| v.trim().split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(theirs) > parts(ours)
}

/// The name of this system's file in the list.
fn file_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("sshremote-windows-x86_64.exe")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("sshremote-linux-x86_64")
    } else if cfg!(target_os = "macos") {
        // One program for Intel and Apple silicon.
        Some("sshremote-macos")
    } else {
        None
    }
}

/// The latest release, when it is newer than this program.
pub fn check() -> Result<Option<Release>, String> {
    let url = index_url();
    let data = crate::ext::fetch(&url)?;
    let sig = crate::ext::fetch(&format!("{url}.sig")).map_err(|_| "the list of downloads came without its signature".to_string())?;
    crate::ext::verify_with(&data, &sig, crate::ext::SIGNER, NAMESPACE).map_err(|e| format!("the list of downloads is not signed by sshremote.dev ({e})"))?;
    release_in(&data, &url, env!("CARGO_PKG_VERSION"))
}

/// The release a list offers this system over version `ours`; `url` is where the list came from.
fn release_in(list: &[u8], url: &str, ours: &str) -> Result<Option<Release>, String> {
    let v: serde_json::Value = serde_json::from_slice(list).map_err(|e| format!("the list of downloads: {e}"))?;
    let version = v["version"].as_str().ok_or("the list of downloads names no version")?.to_string();
    if !newer(&version, ours) {
        return Ok(None);
    }
    let name = file_name().ok_or("no build of it for this system")?;
    let file = v["files"].as_array().and_then(|f| f.iter().find(|f| f["name"] == name)).ok_or_else(|| format!("version {version} has no {name}"))?;
    let (Some(size), Some(sha256)) = (file["size"].as_u64(), file["sha256"].as_str()) else { return Err(format!("{name} is listed without its size and checksum")) };
    let dir = url.rsplit_once('/').map_or(url, |(d, _)| d);
    Ok(Some(Release { version, url: format!("{dir}/{name}"), size, sha256: sha256.to_lowercase() }))
}

/// Downloads the release, checks it against the list, and puts it in place of this program,
/// which is returned to be started again.
pub fn install(r: &Release) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let data = crate::ext::fetch(&r.url)?;
    use sha2::Digest;
    let sum: String = sha2::Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    if data.len() as u64 != r.size || sum != r.sha256 {
        return Err(format!("the download of {} is not the file the list names; nothing was changed", r.version));
    }
    place(&exe, &data)?;
    Ok(exe)
}

/// Writes `data` beside `exe` and swaps it in. Windows lets a running program be renamed but not
/// overwritten, so the one running moves aside (`tidy` removes it at the next start).
fn place(exe: &Path, data: &[u8]) -> Result<(), String> {
    let dir = exe.parent().ok_or("no folder for this program")?;
    let new = dir.join(".sshremote-update.new");
    std::fs::write(&new, data).map_err(|e| format!("this program's folder cannot be written ({}: {e}); get the new version from sshremote.dev", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(windows)]
    {
        let old = aside(exe);
        let _ = std::fs::remove_file(&old);
        if let Err(e) = std::fs::rename(exe, &old) {
            let _ = std::fs::remove_file(&new);
            return Err(format!("this program could not be moved aside ({e})"));
        }
        if let Err(e) = std::fs::rename(&new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(format!("the new version could not be put in place ({e})"));
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(&new, exe).map_err(|e| {
        let _ = std::fs::remove_file(&new);
        format!("the new version could not be put in place ({e})")
    })?;
    Ok(())
}

/// Where the program that ran before an update waits to be removed (Windows).
#[cfg(windows)]
fn aside(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Removes what an update left behind: the program it replaced, no longer running.
pub fn tidy() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(aside(&exe));
    }
}

/// `sshremote update [--check]`: says whether there is a newer version, and puts it in place.
pub fn cli(args: &[String]) -> Result<(), String> {
    let Some(r) = check()? else {
        println!("sshremote {} is the latest", env!("CARGO_PKG_VERSION"));
        return Ok(());
    };
    println!("sshremote {} is out (this is {})", r.version, env!("CARGO_PKG_VERSION"));
    if args.iter().any(|a| a == "--check") {
        return Ok(());
    }
    let exe = install(&r)?;
    println!("updated {} to {}: start it again to use it", exe.display(), r.version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_number_by_number() {
        assert!(newer("0.25.10", "0.25.9"));
        assert!(newer("0.26.0", "0.25.99"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.25.9", "0.25.10"));
        assert!(!newer("0.25.12", "0.25.12"));
    }

    #[test]
    fn a_list_offers_this_systems_file_only_when_newer() {
        let list = br#"{"version":"0.30.1","files":[{"name":"sshremote-windows-x86_64.exe","size":10,"sha256":"AB"},{"name":"sshremote-linux-x86_64","size":20,"sha256":"CD"},{"name":"sshremote-macos","size":30,"sha256":"EF"}]}"#;
        let r = release_in(list, "https://sshremote.dev/download/index.json", "0.25.12").unwrap().unwrap();
        assert_eq!(r.version, "0.30.1");
        assert!(r.url.starts_with("https://sshremote.dev/download/sshremote-"), "{}", r.url);
        let (size, sum) = if cfg!(windows) { (10, "ab") } else if cfg!(target_os = "macos") { (30, "ef") } else { (20, "cd") };
        assert_eq!((r.size, r.sha256.as_str()), (size, sum));
        assert_eq!(release_in(list, "x/index.json", "0.30.1").unwrap(), None);
        assert!(release_in(br#"{"version":"9.0.0","files":[]}"#, "x/index.json", "0.1.0").is_err());
    }

    #[test]
    fn a_new_version_takes_the_programs_place() {
        let dir = std::env::temp_dir().join(format!("sshremote-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(if cfg!(windows) { "sshremote.exe" } else { "sshremote" });
        std::fs::write(&exe, b"old").unwrap();
        place(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert!(!dir.join(".sshremote-update.new").exists());
        #[cfg(windows)]
        assert_eq!(std::fs::read(aside(&exe)).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
