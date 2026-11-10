//! Saved SSH passwords, kept by the system's own store: Windows Credential Manager, the Secret
//! Service through `secret-tool` on Linux, or the macOS keychain. Nothing is written to sshremote's
//! own files.

/// Environment variable through which `sshremote` answers ssh's askpass call.
pub const ASKPASS_SECRET: &str = "SSHREMOTE_SSH_SECRET";

/// The store's name, or None when this system has none sshremote can use.
pub fn backend() -> Option<&'static str> {
    static BACKEND: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *BACKEND.get_or_init(find_backend)
}

fn find_backend() -> Option<&'static str> {
    if cfg!(windows) {
        Some("Windows Credential Manager")
    } else if cfg!(target_os = "macos") {
        Some("macOS keychain")
    } else if which("secret-tool") {
        Some("Secret Service (secret-tool)")
    } else {
        None
    }
}

#[cfg(unix)]
fn which(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

#[cfg(windows)]
fn which(_: &str) -> bool {
    false
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredEnumerateW, CredFree, CredReadW,
        CredWriteW,
    };

    const PREFIX: &str = "sshremote:";

    use crate::sys::wide;

    unsafe fn text(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let n = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
    }

    pub fn get(host: &str) -> Option<String> {
        let target = wide(&format!("{PREFIX}{host}"));
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) } == 0 {
            return None;
        }
        let c = unsafe { &*cred };
        let blob: &[u8] = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize) }
        };
        let units: Vec<u16> = blob.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        unsafe { CredFree(cred as _) };
        Some(String::from_utf16_lossy(&units))
    }

    pub fn set(host: &str, secret: &str) -> Result<(), String> {
        let mut target = wide(&format!("{PREFIX}{host}"));
        let mut user = wide(host);
        let mut blob: Vec<u8> = secret.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: std::ptr::null_mut(),
            LastWritten: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        if unsafe { CredWriteW(&cred, 0) } == 0 {
            return Err(format!("Credential Manager: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn delete(host: &str) -> Result<(), String> {
        let target = wide(&format!("{PREFIX}{host}"));
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            return Err(format!("Credential Manager: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn list() -> Vec<String> {
        let filter = wide(&format!("{PREFIX}*"));
        let mut count = 0u32;
        let mut creds: *mut *mut CREDENTIALW = std::ptr::null_mut();
        if unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut creds) } == 0 {
            return Vec::new();
        }
        let mut out: Vec<String> = (0..count as usize)
            .map(|i| unsafe { text((**creds.add(i)).TargetName) })
            .filter_map(|t| t.strip_prefix(PREFIX).map(str::to_string))
            .collect();
        unsafe { CredFree(creds as _) };
        out.sort();
        out
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::process::{Command, Stdio};

    pub fn get(host: &str) -> Option<String> {
        let out = Command::new("security")
            .args(["find-generic-password", "-s", "sshremote", "-a", host, "-w"])
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
    }

    pub fn set(host: &str, secret: &str) -> Result<(), String> {
        let out = Command::new("security")
            .args(["add-generic-password", "-U", "-s", "sshremote", "-a", host, "-w", secret])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    }

    pub fn delete(host: &str) -> Result<(), String> {
        let out = Command::new("security")
            .args(["delete-generic-password", "-s", "sshremote", "-a", host])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    }

    pub fn list() -> Vec<String> {
        let Ok(out) = Command::new("security").args(["dump-keychain"]).output() else { return Vec::new() };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut hosts = Vec::new();
        let mut account = None;
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with("keychain:") {
                account = None;
            }
            if let Some(v) = l.strip_prefix("\"acct\"<blob>=\"") {
                account = v.strip_suffix('"').map(str::to_string);
            }
            if l == "\"svce\"<blob>=\"sshremote\""
                && let Some(a) = account.take()
            {
                hosts.push(a);
            }
        }
        hosts.sort();
        hosts.dedup();
        hosts
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod sys {
    use std::io::Write;
    use std::process::{Command, Stdio};

    pub fn get(host: &str) -> Option<String> {
        let out = Command::new("secret-tool").args(["lookup", "service", "sshremote", "host", host]).output().ok()?;
        (out.status.success() && !out.stdout.is_empty()).then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub fn set(host: &str, secret: &str) -> Result<(), String> {
        let mut child = Command::new("secret-tool")
            .args(["store", &format!("--label=sshremote {host}"), "service", "sshremote", "host", host])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("secret-tool: {e}"))?;
        child.stdin.take().expect("piped").write_all(secret.as_bytes()).map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    }

    pub fn delete(host: &str) -> Result<(), String> {
        let out = Command::new("secret-tool")
            .args(["clear", "service", "sshremote", "host", host])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    }

    pub fn list() -> Vec<String> {
        let Ok(out) = Command::new("secret-tool").args(["search", "--all", "service", "sshremote"]).output() else {
            return Vec::new();
        };
        let mut hosts: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&out.stderr).lines())
            .filter_map(|l| l.trim().strip_prefix("attribute.host = ").map(str::to_string))
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }
}

/// The saved password for `host`.
pub fn get(host: &str) -> Option<String> {
    backend()?;
    sys::get(host)
}

pub fn set(host: &str, secret: &str) -> Result<(), String> {
    backend().ok_or("no password store on this system (install secret-tool)")?;
    forget_list();
    sys::set(host, secret)
}

pub fn delete(host: &str) -> Result<(), String> {
    forget_list();
    sys::delete(host)
}

/// Hosts with a saved password, asked of the store once and kept until a password is saved or
/// removed (the store can be slow, and terminal tabs ask on every ssh tab).
pub fn list() -> Vec<String> {
    if backend().is_none() {
        return Vec::new();
    }
    let mut cache = LIST.lock().unwrap_or_else(|e| e.into_inner());
    cache.get_or_insert_with(sys::list).clone()
}

static LIST: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

fn forget_list() {
    *LIST.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    /// Writes to the real password store, so it runs only on request (cargo test -- --ignored).
    #[test]
    #[ignore]
    fn passwords_round_trip() {
        let host = format!("test-{}@example.invalid", std::process::id());
        super::set(&host, "pa ss \u{D55C}").unwrap();
        assert_eq!(super::get(&host).as_deref(), Some("pa ss \u{D55C}"));
        assert!(super::list().contains(&host));
        super::delete(&host).unwrap();
        assert_eq!(super::get(&host), None);
    }
}
