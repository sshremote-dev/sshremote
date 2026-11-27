//! `sshremote://` links opening the app, and on Linux the app in the desktop's list of
//! applications: a desktop entry and the icon in the person's own folders (Linux), or the link's
//! class among the person's own (Windows); written by the window when it opens and kept pointing
//! at the program wherever it was put.

#[cfg(not(windows))]
use std::path::PathBuf;
use std::path::Path;

#[cfg(not(windows))]
const ICON: &[u8] = include_bytes!("../assets/sshremote.png");

/// The entry for the program at `exe` (the Desktop Entry Specification's quoting).
#[cfg(not(windows))]
pub fn entry(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    let quoted = format!("\"{}\"", exe.replace('\\', "\\\\\\\\").replace('"', "\\\\\"").replace('`', "\\\\`").replace('$', "\\\\$"));
    format!(
        "[Desktop Entry]\nType=Application\nName=SSH Remote\nGenericName=Remote desktop and SSH\n\
         Comment=Split terminals, screens and files of your servers and computers\nExec={quoted} %u\nIcon=sshremote\n\
         Terminal=false\nCategories=Network;RemoteAccess;System;\nKeywords=ssh;remote;desktop;terminal;support;\n\
         StartupWMClass=sshremote\nMimeType=x-scheme-handler/sshremote;\n"
    )
}

/// Where a program kept elsewhere has its own entry already (a package's), or is not one to
/// list (a build, a copy in the temporary folder).
fn listed_elsewhere(exe: &Path) -> bool {
    let p = exe.to_string_lossy().replace('\\', "/");
    p.starts_with("/usr/") || p.starts_with("/opt/") || p.contains("/target/") || exe.starts_with(std::env::temp_dir())
}

#[cfg(not(windows))]
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
}

/// Windows: `sshremote:` links open this program, by the class of the person's own
/// (HKCU\Software\Classes\sshremote), set when it is not there or names another program.
#[cfg(windows)]
pub fn ensure() {
    use crate::sys::wide;
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegGetValueW, RRF_RT_REG_SZ, RegSetValueExW};
    let Ok(exe) = std::env::current_exe() else { return };
    if listed_elsewhere(&exe) {
        return;
    }
    let exe = exe.to_string_lossy().to_string();
    let command = format!("\"{exe}\" \"%1\"");
    let key = |path: &str| -> Option<HKEY> {
        let mut h: HKEY = std::ptr::null_mut();
        let p = wide(path);
        (unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, p.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE, std::ptr::null(), &mut h, std::ptr::null_mut()) } == 0).then_some(h)
    };
    let set = |h: HKEY, name: Option<&str>, value: &str| {
        let (n, v) = (name.map(wide), wide(value));
        unsafe { RegSetValueExW(h, n.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()), 0, REG_SZ, v.as_ptr() as _, (v.len() * 2) as u32) };
    };
    // Already this program's: nothing to write.
    let open = r"Software\Classes\sshremote\shell\open\command";
    let mut buf = vec![0u16; 2048];
    let mut len = (buf.len() * 2) as u32;
    let p = wide(open);
    let now = (unsafe { RegGetValueW(HKEY_CURRENT_USER, p.as_ptr(), std::ptr::null(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut len) } == 0)
        .then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]));
    if now.as_deref() == Some(command.as_str()) {
        return;
    }
    for (path, values) in [
        (r"Software\Classes\sshremote", vec![(None, "URL:SSH Remote"), (Some("URL Protocol"), "")]),
        (r"Software\Classes\sshremote\DefaultIcon", vec![(None, &*format!("\"{exe}\",0"))]),
        (open, vec![(None, command.as_str())]),
    ] {
        if let Some(h) = key(path) {
            for (n, v) in values {
                set(h, n, v);
            }
            unsafe { RegCloseKey(h) };
        }
    }
}

/// Writes what is not there, or not as it should be; quietly, as nothing depends on it.
#[cfg(not(windows))]
pub fn ensure() {
    let Ok(exe) = std::env::current_exe().and_then(|e| e.canonicalize()) else { return };
    let Some(data) = data_home() else { return };
    if listed_elsewhere(&exe) {
        return;
    }
    let icon = data.join("icons/hicolor/256x256/apps/sshremote.png");
    if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
        let _ = icon.parent().map(std::fs::create_dir_all);
        let _ = crate::sys::write_whole(&icon, ICON, false, false);
    }
    let apps = data.join("applications");
    let file = apps.join("sshremote.desktop");
    let want = entry(&exe);
    if std::fs::read_to_string(&file).ok().as_deref() == Some(want.as_str()) {
        return;
    }
    let _ = std::fs::create_dir_all(&apps);
    if crate::sys::write_whole(&file, want.as_bytes(), false, false).is_ok() {
        // So that links open it at once (desktops read the folder again by themselves too).
        let mut cmd = std::process::Command::new("update-desktop-database");
        cmd.arg(&apps).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let _ = cmd.status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn the_entry_starts_this_program_quoted_as_the_spec_says() {
        let e = entry(Path::new("/home/ana/My $apps/ssh\"remote"));
        assert!(e.contains("Exec=\"/home/ana/My \\\\$apps/ssh\\\\\"remote\" %u\n"), "{e}");
        assert!(e.contains("MimeType=x-scheme-handler/sshremote;\n") && e.contains("StartupWMClass=sshremote\n"));
        assert!(listed_elsewhere(Path::new("/usr/bin/sshremote")) && listed_elsewhere(Path::new("/home/a/src/target/debug/sshremote")));
        assert!(!listed_elsewhere(Path::new("/home/ana/.local/bin/sshremote")));
    }
}
