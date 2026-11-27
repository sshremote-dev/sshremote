//! A newer version from sshremote.dev: its list of downloads is looked at (at most once a day by
//! the window), its signature checked with the release key under a namespace of its own, and the
//! file for this system (by its system and processor in the list), checked against the list, put
//! in place of this program when asked.

use std::path::Path;
use std::path::PathBuf;

const INDEX: &str = "https://sshremote.dev/download/latest.json";
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

/// The name of this system's file in a list of the first kind, which names its files only (a
/// mirror of `index.json`).
fn file_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("sshremote-windows-x86_64.exe")
    } else if cfg!(all(windows, target_arch = "x86")) {
        Some("sshremote-windows-x86.exe")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("sshremote-linux-x86_64")
    } else if cfg!(target_os = "macos") {
        // The app, zipped: one program for Intel and Apple silicon, signed as a whole.
        Some("sshremote-macos.zip")
    } else {
        None
    }
}

/// The latest release, when it is newer than this program.
pub fn check() -> Result<Option<Release>, String> {
    let url = index_url();
    let data = crate::ext::fetch(&url)?;
    let sig = crate::ext::fetch(&format!("{url}.sig")).map_err(|_| n!("the list of downloads came without its signature").to_string())?;
    crate::ext::verify_with(&data, &sig, crate::ext::SIGNERS, NAMESPACE).map_err(|e| crate::lang::fill(n!("the list of downloads is not signed by sshremote.dev ({})"), &[&e]))?;
    release_in(&data, &url, env!("CARGO_PKG_VERSION"))
}

/// The release a list offers this system over version `ours`; `url` is where the list came from.
fn release_in(list: &[u8], url: &str, ours: &str) -> Result<Option<Release>, String> {
    let v: serde_json::Value = serde_json::from_slice(list).map_err(|e| crate::lang::fill(n!("the list of downloads: {}"), &[&e]))?;
    let version = v["version"].as_str().ok_or(n!("the list of downloads names no version"))?.to_string();
    if !newer(&version, ours) {
        return Ok(None);
    }
    let file = v["files"].as_array().and_then(|f| f.iter().find(|f| for_here(f))).ok_or_else(|| crate::lang::fill(n!("version {} has no build for this system ({} on {})"), &[&version, &OS, &ARCH]))?;
    let name = file["name"].as_str().ok_or(n!("a file is listed without its name"))?;
    let (Some(size), Some(sha256)) = (file["size"].as_u64(), file["sha256"].as_str()) else { return Err(crate::lang::fill(n!("{} is listed without its size and checksum"), &[&name])) };
    // Where the list says it is (its release on GitHub), or beside the list.
    let at = match file["url"].as_str() {
        Some(u) if u.starts_with("https://") => u.to_string(),
        Some(u) => return Err(crate::lang::fill(n!("{} is listed at {}, which is not https"), &[&name, &u])),
        None => format!("{}/{name}", url.rsplit_once('/').map_or(url, |(d, _)| d)),
    };
    Ok(Some(Release { version, url: at, size, sha256: sha256.to_lowercase() }))
}

/// This system and processor as the list names them ("linux", "windows", "macos"; "x86_64",
/// "aarch64").
const OS: &str = std::env::consts::OS;
const ARCH: &str = std::env::consts::ARCH;

/// Whether a listed file is the one for this system: by its system and processor ("universal":
/// any), or, in a list that names its files only, by name.
fn for_here(file: &serde_json::Value) -> bool {
    match (file["os"].as_str(), file["arch"].as_str()) {
        (Some(os), Some(arch)) => os == OS && (arch == ARCH || arch == "universal"),
        _ => file_name().is_some_and(|n| file["name"] == n),
    }
}

/// The package manager that put this program where it is, if one did: it updates it, and a
/// copy changed behind its back would confuse it. The command that updates it is returned.
pub fn managed_by(exe: &Path) -> Option<&'static str> {
    let p = exe.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    if p.contains("/cellar/") || p.contains("/caskroom/") || p.contains("/homebrew/") || p.contains("/linuxbrew/") {
        Some("brew upgrade sshremote")
    } else if p.contains("/scoop/apps/") {
        Some("scoop update sshremote")
    } else if p.contains("/winget/packages/") || p.contains("/microsoft/winget/") {
        Some("winget upgrade SSHRemote.SSHRemote")
    } else if p.starts_with("/usr/bin/") {
        // The .deb or .rpm, which the system's package manager keeps.
        Some("the newer .deb or .rpm from https://sshremote.dev")
    } else if cfg!(windows) && p.ends_with("/ssh remote/sshremote.exe") && from_msi() && !crate::admin::elevated() {
        // The MSI's copy in Program Files, which only an administrator changes (the service
        // updates it by itself).
        Some("the newer .msi from https://sshremote.dev")
    } else {
        None
    }
}

/// Whether the MSI package installed this computer's copy (its value in the registry).
fn from_msi() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
        let (key, value) = (crate::sys::wide(r"Software\SSH Remote"), crate::sys::wide("msi"));
        let mut len = 0u32;
        // SAFETY: the size alone is asked for.
        return unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut len) } == 0;
    }
    #[allow(unreachable_code)]
    false
}

/// Remote access run as a service, with nobody at the computer to click: a newer release put in
/// place by itself (unless turned off: `access::Config::auto_update`, or a package manager keeps
/// this copy), once `idle` says nobody is connected, and the service started again on it.
#[cfg(feature = "remote")]
pub fn unattended(idle: impl Fn() -> bool) {
    use std::time::Duration;
    std::thread::sleep(Duration::from_secs(600));
    loop {
        let on = crate::access::stored().ok().flatten().is_some_and(|c| c.auto_update);
        let exe = std::env::current_exe().ok().map(|e| e.canonicalize().unwrap_or(e));
        if on
            && exe.as_deref().is_some_and(|e| managed_by(e).is_none())
            && let Ok(Some(r)) = check()
        {
            // Not under someone working there: after they leave.
            while !idle() {
                std::thread::sleep(Duration::from_secs(60));
            }
            match install(&r) {
                Ok(_) => {
                    eprintln!("remote access: updated to {} by itself; starting again", r.version);
                    // The service manager starts it again (systemd's Restart=always, launchd's
                    // KeepAlive); the Windows service starts itself again through a helper.
                    #[cfg(windows)]
                    crate::service::start_again();
                    std::process::exit(0);
                }
                Err(e) => eprintln!("remote access: the update to {} did not go in: {e}", r.version),
            }
        }
        std::thread::sleep(Duration::from_secs(6 * 3600));
    }
}

/// Downloads the release, checks it against the list, and puts it in place of this program,
/// which is returned to be started again.
pub fn install(r: &Release) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(how) = managed_by(&exe.canonicalize().unwrap_or_else(|_| exe.clone())) {
        return Err(crate::lang::fill(n!("a package manager installed this copy: update it with {}"), &[&how]));
    }
    let data = crate::ext::fetch(&r.url)?;
    use sha2::Digest;
    let sum: String = sha2::Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    if data.len() as u64 != r.size || sum != r.sha256 {
        return Err(crate::lang::fill(n!("the download of {} is not the file the list names; nothing was changed"), &[&r.version]));
    }
    #[cfg(target_os = "macos")]
    return mac::place(&exe, &data).map(|_| exe);
    #[cfg(not(target_os = "macos"))]
    {
        place(&exe, &data)?;
        Ok(exe)
    }
}

/// macOS: the release is the app zipped. The app this program runs from is swapped for the new
/// one whole (its signature covers all of it); a program run on its own takes the new one's.
#[cfg(target_os = "macos")]
mod mac {
    use std::io::Read;
    use std::path::{Path, PathBuf};

    /// The app `exe` is in (`X.app/Contents/MacOS/exe`), if it is in one.
    fn bundle(exe: &Path) -> Option<PathBuf> {
        let app = exe.parent()?.parent()?.parent()?;
        (app.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS")).then(|| app.to_path_buf())
    }

    /// Writes the zip's entries under `to`, with their permissions; none may reach outside it.
    fn unzip(data: &[u8], to: &Path) -> Result<(), String> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| crate::lang::fill(n!("the download is no zip ({})"), &[&e]))?;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| e.to_string())?;
            let Some(rel) = f.enclosed_name() else { return Err(crate::lang::fill(n!("{} would land outside the app"), &[&f.name().unwrap_or_default()])) };
            // macOS's own archiver keeps file metadata apart; nothing of the app is in it.
            if rel.starts_with("__MACOSX") {
                continue;
            }
            let path = to.join(rel);
            if f.is_dir() {
                std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            let mut bytes = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(mode) = f.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o777));
            }
        }
        Ok(())
    }

    pub fn place(exe: &Path, data: &[u8]) -> Result<(), String> {
        let home = bundle(exe);
        let dir = home.as_deref().and_then(Path::parent).or_else(|| exe.parent()).ok_or(n!("no folder for this program"))?;
        let unpacked = dir.join(".sshremote-update");
        let _ = std::fs::remove_dir_all(&unpacked);
        let written = |e: String| crate::lang::fill(n!("this program's folder cannot be written ({}: {}); get the new version from sshremote.dev"), &[&dir.display(), &e]);
        std::fs::create_dir_all(&unpacked).map_err(|e| written(e.to_string()))?;
        let done = (|| {
            unzip(data, &unpacked).map_err(written)?;
            let new_app = std::fs::read_dir(&unpacked)
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "app"))
                .ok_or(n!("the download holds no app"))?;
            match &home {
                // The app as a whole: the one running moves aside, keeps running, and goes.
                Some(app) => {
                    let old = unpacked.join("old.app");
                    std::fs::rename(app, &old).map_err(|e| crate::lang::fill(n!("this app could not be moved aside ({})"), &[&e]))?;
                    if let Err(e) = std::fs::rename(&new_app, app) {
                        let _ = std::fs::rename(&old, app);
                        return Err(crate::lang::fill(n!("the new version could not be put in place ({})"), &[&e]));
                    }
                    Ok(())
                }
                None => {
                    let new = new_app.join("Contents/MacOS/sshremote");
                    std::fs::rename(&new, exe).map_err(|e| crate::lang::fill(n!("the new version could not be put in place ({})"), &[&e]))
                }
            }
        })();
        let _ = std::fs::remove_dir_all(&unpacked);
        done
    }
}

/// Writes `data` beside `exe` and swaps it in. Windows lets a running program be renamed but not
/// overwritten, so the one running moves aside (`tidy` removes it at the next start).
#[cfg(not(target_os = "macos"))]
fn place(exe: &Path, data: &[u8]) -> Result<(), String> {
    let dir = exe.parent().ok_or(n!("no folder for this program"))?;
    let new = dir.join(".sshremote-update.new");
    std::fs::write(&new, data).map_err(|e| crate::lang::fill(n!("this program's folder cannot be written ({}: {}); get the new version from sshremote.dev"), &[&dir.display(), &e]))?;
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
            return Err(crate::lang::fill(n!("this program could not be moved aside ({})"), &[&e]));
        }
        if let Err(e) = std::fs::rename(&new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(crate::lang::fill(n!("the new version could not be put in place ({})"), &[&e]));
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(&new, exe).map_err(|e| {
        let _ = std::fs::remove_file(&new);
        crate::lang::fill(n!("the new version could not be put in place ({})"), &[&e])
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
    fn copies_package_managers_keep_are_left_to_them() {
        let m = |p: &str| managed_by(Path::new(p));
        assert_eq!(m("/opt/homebrew/Caskroom/sshremote/0.27.0/SSH Remote.app/Contents/MacOS/sshremote"), Some("brew upgrade sshremote"));
        assert_eq!(m("/home/linuxbrew/.linuxbrew/Cellar/sshremote/0.27.0/bin/sshremote"), Some("brew upgrade sshremote"));
        assert_eq!(m("C:\\Users\\ana\\scoop\\apps\\sshremote\\current\\sshremote.exe"), Some("scoop update sshremote"));
        assert_eq!(m("/home/ana/.local/bin/sshremote"), None);
        assert_eq!(m("/usr/bin/sshremote"), Some("the newer .deb or .rpm from https://sshremote.dev"));
        assert_eq!(m("C:\\Users\\ana\\AppData\\Local\\Programs\\SSH Remote\\sshremote.exe"), None);
    }

    #[test]
    fn a_list_offers_this_systems_file_only_when_newer() {
        let list = br#"{"version":"0.30.1","files":[{"name":"sshremote-windows-x86_64.exe","size":10,"sha256":"AB"},{"name":"sshremote-linux-x86_64","size":20,"sha256":"CD"},{"name":"sshremote-macos.zip","size":30,"sha256":"EF"}]}"#;
        let r = release_in(list, "https://sshremote.dev/download/index.json", "0.25.12").unwrap().unwrap();
        assert_eq!(r.version, "0.30.1");
        assert!(r.url.starts_with("https://sshremote.dev/download/sshremote-"), "{}", r.url);
        let (size, sum) = if cfg!(windows) { (10, "ab") } else if cfg!(target_os = "macos") { (30, "ef") } else { (20, "cd") };
        assert_eq!((r.size, r.sha256.as_str()), (size, sum));
        assert_eq!(release_in(list, "x/index.json", "0.30.1").unwrap(), None);
        assert!(release_in(br#"{"version":"9.0.0","files":[]}"#, "x/index.json", "0.1.0").is_err());
    }

    /// The list since 0.25.36 names each file's system and processor and where it is (its
    /// release on GitHub): this system's is taken, whatever its name, and only over https.
    #[test]
    fn a_list_offers_the_file_for_this_system_and_processor() {
        let entry = |os: &str, arch: &str, n: u64| format!(r#"{{"name":"f-{os}-{arch}","os":"{os}","arch":"{arch}","size":{n},"sha256":"S{n}","url":"https://github.com/o/r/releases/download/v9/f-{os}-{arch}"}}"#);
        let other = if ARCH == "x86_64" { "aarch64" } else { "x86_64" };
        let mut files = vec![entry("linux", other, 1), entry("windows", other, 2), entry("plan9", ARCH, 3), entry(OS, ARCH, 4)];
        if OS == "macos" {
            files.truncate(3);
            files.push(entry("macos", "universal", 4));
        }
        let list = format!(r#"{{"version":"9.0.0","files":[{}]}}"#, files.join(","));
        let r = release_in(list.as_bytes(), "https://sshremote.dev/download/latest.json", "0.25.36").unwrap().unwrap();
        assert_eq!((r.size, r.sha256.as_str()), (4, "s4"));
        assert!(r.url.starts_with("https://github.com/o/r/releases/download/v9/f-"), "{}", r.url);
        let plain = list.replace("https://github.com", "http://github.com");
        assert!(release_in(plain.as_bytes(), "https://sshremote.dev/download/latest.json", "0.25.36").is_err(), "not over http");
        let none = format!(r#"{{"version":"9.0.0","files":[{}]}}"#, entry("plan9", ARCH, 3));
        assert!(release_in(none.as_bytes(), "x/latest.json", "0.25.36").is_err(), "nothing for this system");
    }

    /// Every file release/platforms.json ships is named by the system and processor this program
    /// compares with its own, once; the three names versions up to 0.25.35 look for are among them.
    #[test]
    fn the_platforms_shipped_are_named_as_the_update_reads_them() {
        let list: serde_json::Value = serde_json::from_str(include_str!("../release/platforms.json")).unwrap();
        let platforms = list["platforms"].as_array().unwrap();
        let mut names = std::collections::HashSet::new();
        for p in platforms {
            let (file, os, arch) = (p["file"].as_str().unwrap(), p["os"].as_str().unwrap(), p["arch"].as_str().unwrap());
            assert!(["linux", "windows", "macos"].contains(&os), "{file}: {os}");
            assert!(["x86_64", "aarch64"].contains(&arch) || (os == "macos" && arch == "universal") || (os == "windows" && arch == "x86"), "{file}: {arch}");
            assert!(["ci", "mac"].contains(&p["build"]["by"].as_str().unwrap()), "{file}");
            assert!(names.insert(file), "{file} twice");
        }
        for first in ["sshremote-windows-x86_64.exe", "sshremote-linux-x86_64", "sshremote-macos.zip"] {
            assert!(names.contains(first), "{first}");
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
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

    /// macOS: the zipped app takes the place of the app this program runs from, whole and
    /// still runnable; a program on its own takes the new one's program.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_new_app_takes_the_apps_place() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let exe_opts = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        zip.start_file("SSH Remote.app/Contents/MacOS/sshremote", exe_opts).unwrap();
        zip.write_all(b"new").unwrap();
        zip.start_file("SSH Remote.app/Contents/Info.plist", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"<plist/>").unwrap();
        let data = zip.finish().unwrap().into_inner();
        let dir = std::env::temp_dir().join(format!("sshremote-update-mac-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe = dir.join("SSH Remote.app/Contents/MacOS/sshremote");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(dir.join("SSH Remote.app/Contents/stale"), b"x").unwrap();
        mac::place(&exe, &data).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(!dir.join("SSH Remote.app/Contents/stale").exists(), "the app replaced whole");
        assert!(!dir.join(".sshremote-update").exists());
        let alone = dir.join("sshremote");
        std::fs::write(&alone, b"old").unwrap();
        mac::place(&alone, &data).unwrap();
        assert_eq!(std::fs::read(&alone).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real release's zip (SSHREMOTE_UPDATE_ZIP) put in place of the app at SSHREMOTE_UPDATE_APP:
    /// cargo test -- --ignored a_released_app_takes_the_apps_place
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn a_released_app_takes_the_apps_place() {
        let (Some(zip), Some(app)) = (std::env::var_os("SSHREMOTE_UPDATE_ZIP"), std::env::var_os("SSHREMOTE_UPDATE_APP")) else { return };
        let exe = std::path::Path::new(&app).join("Contents/MacOS/sshremote");
        mac::place(&exe, &std::fs::read(zip).unwrap()).unwrap();
        let out = std::process::Command::new(&exe).arg("--version").output().unwrap();
        println!("now {}", String::from_utf8_lossy(&out.stdout).trim());
        let check = std::process::Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&app).status().unwrap();
        assert!(check.success(), "the app's signature holds");
    }
}
