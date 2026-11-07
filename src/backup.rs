//! Backups of what sshremote knows about hosts: recent and favorite hosts, when each was last
//! reached, settings, ~/.ssh/config and known_hosts. With a passphrase, the saved passwords and
//! every file of ~/.ssh (private keys too) go in as well, encrypted. A copy without them is also
//! made automatically once a day.

use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const KEEP_AUTO: usize = 14;
/// Shorter passphrases are refused when private keys are to be sealed with them.
pub const MIN_PASSPHRASE: usize = 8;
/// Larger files in ~/.ssh (sockets, logs) are not keys or settings and are left out.
const MAX_SSH_FILE: u64 = 1 << 20;

fn ssh_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| Path::new(&h).join(".ssh"))
}

/// Where backups made by hand go: Documents/sshremote.
pub fn folder() -> PathBuf {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
    let docs = home.join("Documents");
    (if docs.is_dir() { docs } else { home }).join("sshremote")
}

fn auto_folder() -> Option<PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("backups"))
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    crate::files::local_time(secs).replace([' ', ':'], "-")
}

/// A key from a passphrase, stretched so guessing is slow.
fn key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    let mut h: [u8; 32] = Sha256::new().chain_update(salt).chain_update(passphrase.as_bytes()).finalize().into();
    for _ in 0..200_000 {
        h = Sha256::new().chain_update(h).chain_update(salt).finalize().into();
    }
    h
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn seal(plain: &[u8], passphrase: &str) -> Value {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut salt).expect("system random source");
    getrandom::getrandom(&mut nonce).expect("system random source");
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key(passphrase, &salt)));
    let data = cipher.encrypt(Nonce::from_slice(&nonce), plain).expect("encryption");
    json!({ "salt": hex(&salt), "nonce": hex(&nonce), "data": hex(&data) })
}

fn open(sealed: &Value, passphrase: &str) -> Result<Vec<u8>, String> {
    let field = |k: &str| sealed[k].as_str().and_then(unhex).ok_or("damaged backup");
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key(passphrase, &field("salt")?)));
    cipher
        .decrypt(Nonce::from_slice(&field("nonce")?), field("data")?.as_slice())
        .map_err(|_| "wrong passphrase".to_string())
}

/// The backup as JSON; with a passphrase the saved passwords are included, encrypted.
pub fn create(passphrase: Option<&str>) -> String {
    let mut files = serde_json::Map::new();
    if let Some(dir) = crate::hosts::config_dir()
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".txt")
                && let Ok(text) = std::fs::read_to_string(e.path())
            {
                files.insert(name, Value::String(text));
            }
        }
    }
    let mut ssh = serde_json::Map::new();
    for name in ["config", "known_hosts"] {
        if let Some(text) = ssh_dir().and_then(|d| std::fs::read_to_string(d.join(name)).ok()) {
            ssh.insert(name.into(), Value::String(text));
        }
    }
    let mut out = json!({ "sshremote_backup": 1, "created": stamp(), "files": files, "ssh": ssh });
    if let Some(p) = passphrase.filter(|p| !p.is_empty()) {
        let passwords: serde_json::Map<String, Value> = crate::vault::list()
            .into_iter()
            .filter_map(|h| crate::vault::get(&h).map(|s| (h, Value::String(s))))
            .collect();
        let files: serde_json::Map<String, Value> =
            ssh_files().into_iter().map(|(name, data)| (name, Value::String(hex(&data)))).collect();
        let secrets = json!({ "passwords": passwords, "ssh": files });
        out["secrets"] = seal(secrets.to_string().as_bytes(), p);
    }
    serde_json::to_string_pretty(&out).expect("json")
}

/// Every file of ~/.ssh and its folders, by path relative to it with / between names.
fn ssh_files() -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, depth: u32, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = format!("{prefix}{}", e.file_name().to_string_lossy());
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() && depth < 3 {
                walk(&e.path(), &format!("{name}/"), depth + 1, out);
            } else if meta.is_file()
                && meta.len() <= MAX_SSH_FILE
                && let Ok(data) = std::fs::read(e.path())
            {
                out.push((name, data));
            }
        }
    }
    let mut out = Vec::new();
    if let Some(d) = ssh_dir() {
        walk(&d, "", 0, &mut out);
    }
    out.sort();
    out
}

/// Writes a file of ~/.ssh from a backup: missing ones are created, changed ones replaced with the
/// current file kept beside it as NAME.bak. Returns whether anything was written.
fn put_ssh_file(ssh: &Path, rel: &str, data: &[u8]) -> Result<bool, String> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains(['\\', ':'])) {
        return Err(format!("unsafe name in the backup: {rel}"));
    }
    let path = parts.iter().fold(ssh.to_path_buf(), |p, part| p.join(part));
    if std::fs::read(&path).is_ok_and(|now| now == data) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if path.exists() {
        let mut bak = path.clone().into_os_string();
        bak.push(".bak");
        std::fs::rename(&path, bak).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, data).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // ssh refuses private keys that others can read.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(true)
}

/// Writes a backup to the backup folder and returns its path.
pub fn save(passphrase: Option<&str>) -> Result<PathBuf, String> {
    let dir = folder();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("sshremote-backup-{}.json", stamp()));
    std::fs::write(&path, create(passphrase)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Keeps one automatic backup a day (without passwords), the last two weeks of them.
pub fn auto() {
    let Some(dir) = auto_folder() else { return };
    let day = stamp().get(..10).unwrap_or_default().to_string();
    let path = dir.join(format!("auto-{day}.json"));
    if path.exists() || std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(&path, create(None));
    let mut old: Vec<PathBuf> = list_in(&dir);
    if old.len() > KEEP_AUTO {
        old.sort();
        for p in &old[..old.len() - KEEP_AUTO] {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn list_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
        .unwrap_or_default()
}

/// Backups found in the backup folder and the automatic ones, newest first.
pub fn list() -> Vec<PathBuf> {
    let mut all = list_in(&folder());
    if let Some(d) = auto_folder() {
        all.extend(list_in(&d));
    }
    all.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok()));
    all
}

/// What a backup holds, for the restore list: its date and whether passwords are inside.
pub fn describe(path: &Path) -> Option<(String, bool)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("sshremote_backup")?;
    Some((v["created"].as_str().unwrap_or("").to_string(), v.get("passwords").is_some() || v.get("secrets").is_some()))
}

/// Lines of `a` followed by those of `b` not in `a`.
fn union(a: &str, b: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for l in a.lines().chain(b.lines()) {
        if !l.trim().is_empty() && seen.insert(l.to_string()) {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// Restores a backup into what is here: hosts and favorites are merged, the backup's settings
/// win, known_hosts gains missing lines, and ~/.ssh/config is replaced only when asked (the old
/// one is kept as config.bak). Returns what was done.
pub fn restore(path: &Path, passphrase: Option<&str>, ssh_config: bool, ssh_files: bool) -> Result<String, String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
        .map_err(|_| "not a sshremote backup")?;
    if v.get("sshremote_backup").is_none() {
        return Err("not a sshremote backup".into());
    }
    let dir = crate::hosts::config_dir().ok_or("no settings folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut done = Vec::new();
    if let Some(files) = v["files"].as_object() {
        for (name, text) in files {
            let (Some(text), true) = (text.as_str(), name.ends_with(".txt") && !name.contains(['/', '\\'])) else {
                continue;
            };
            let here = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
            let merged = if name == "settings.txt" {
                // The backup's value wins for each key.
                let keys: Vec<&str> = text.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
                let kept: String = here
                    .lines()
                    .filter(|l| !l.split_once('=').is_some_and(|(k, _)| keys.contains(&k)))
                    .map(|l| format!("{l}\n"))
                    .collect();
                union(text, &kept)
            } else {
                union(text, &here)
            };
            std::fs::write(dir.join(name), merged).map_err(|e| e.to_string())?;
        }
        done.push(format!("{} lists", files.len()));
    }
    if let Some(ssh) = ssh_dir() {
        let _ = std::fs::create_dir_all(&ssh);
        if let Some(known) = v["ssh"]["known_hosts"].as_str() {
            let here = std::fs::read_to_string(ssh.join("known_hosts")).unwrap_or_default();
            std::fs::write(ssh.join("known_hosts"), union(&here, known)).map_err(|e| e.to_string())?;
            done.push("known_hosts".into());
        }
        if ssh_config && let Some(config) = v["ssh"]["config"].as_str() {
            let path = ssh.join("config");
            if path.exists() {
                std::fs::copy(&path, ssh.join("config.bak")).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, config).map_err(|e| e.to_string())?;
            done.push("~/.ssh/config (the old one is config.bak)".into());
        }
    }
    if let Some(sealed) = v.get("passwords") {
        match passphrase.filter(|p| !p.is_empty()) {
            Some(p) => {
                let plain = open(sealed, p)?;
                let secrets: serde_json::Map<String, Value> =
                    serde_json::from_slice(&plain).map_err(|_| "damaged backup")?;
                for (host, s) in &secrets {
                    if let Some(s) = s.as_str() {
                        crate::vault::set(host, s)?;
                    }
                }
                done.push(format!("{} passwords", secrets.len()));
            }
            None => done.push("passwords skipped (no passphrase)".into()),
        }
    }
    if let Some(sealed) = v.get("secrets") {
        match passphrase.filter(|p| !p.is_empty()) {
            Some(p) => {
                let plain = open(sealed, p)?;
                let secrets: Value = serde_json::from_slice(&plain).map_err(|_| "damaged backup")?;
                let passwords = secrets["passwords"].as_object().cloned().unwrap_or_default();
                for (host, s) in &passwords {
                    if let Some(s) = s.as_str() {
                        crate::vault::set(host, s)?;
                    }
                }
                done.push(format!("{} passwords", passwords.len()));
                let files = secrets["ssh"].as_object().cloned().unwrap_or_default();
                if ssh_files {
                    let ssh = ssh_dir().ok_or("no home folder")?;
                    let mut written = 0;
                    for (rel, data) in &files {
                        let data = data.as_str().and_then(unhex).ok_or("damaged backup")?;
                        written += put_ssh_file(&ssh, rel, &data)? as usize;
                    }
                    done.push(format!(
                        "{written} of {} files of ~/.ssh (the others were the same; replaced ones are kept as .bak)",
                        files.len()
                    ));
                } else if !files.is_empty() {
                    done.push(format!("{} files of ~/.ssh left out (not asked for)", files.len()));
                }
            }
            None => done.push("passwords and ~/.ssh files skipped (no passphrase)".into()),
        }
    }
    Ok(format!("Restored {}", done.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_data_needs_the_passphrase() {
        let sealed = seal(b"secret", "pass");
        assert_eq!(open(&sealed, "pass").unwrap(), b"secret");
        assert!(open(&sealed, "wrong").is_err());
    }

    #[test]
    fn ssh_files_are_restored_beside_a_backup_of_the_old_one() {
        let dir = std::env::temp_dir().join(format!("sshremote-ssh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(put_ssh_file(&dir, "keys/id_test", b"new").unwrap());
        assert!(!put_ssh_file(&dir, "keys/id_test", b"new").unwrap(), "the same content is left alone");
        assert!(put_ssh_file(&dir, "keys/id_test", b"newer").unwrap());
        assert_eq!(std::fs::read(dir.join("keys").join("id_test")).unwrap(), b"newer");
        assert_eq!(std::fs::read(dir.join("keys").join("id_test.bak")).unwrap(), b"new");
        assert!(put_ssh_file(&dir, "../escape", b"x").is_err());
        assert!(put_ssh_file(&dir, "a/../../escape", b"x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn union_keeps_order_and_drops_repeats() {
        assert_eq!(union("a\nb\n", "b\nc"), "a\nb\nc\n");
    }

    /// A whole backup and restore in a scratch home folder. It changes the process environment,
    /// so it runs only on request: cargo test -- --ignored --test-threads=1
    #[test]
    #[ignore]
    fn keys_survive_a_round_trip_only_with_the_passphrase() {
        let home = std::env::temp_dir().join(format!("sshremote-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(ssh.join("sub")).unwrap();
        std::fs::write(ssh.join("id_ed25519"), b"PRIVATE KEY").unwrap();
        std::fs::write(ssh.join("sub").join("extra"), b"x").unwrap();
        std::fs::write(ssh.join("config"), "Host a\n  HostName 1.2.3.4\n").unwrap();
        for var in ["USERPROFILE", "HOME", "APPDATA", "XDG_CONFIG_HOME"] {
            unsafe { std::env::set_var(var, &home) };
        }
        let file = home.join("b.json");
        std::fs::write(&file, create(Some("correct horse"))).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("PRIVATE KEY"), "keys are only stored encrypted");
        std::fs::remove_file(ssh.join("id_ed25519")).unwrap();
        std::fs::remove_dir_all(ssh.join("sub")).unwrap();
        assert!(restore(&file, Some("wrong pass"), false, true).is_err());
        let left_out = restore(&file, Some("correct horse"), false, false).unwrap();
        assert!(!ssh.join("id_ed25519").exists(), "{left_out}");
        let done = restore(&file, Some("correct horse"), false, true).unwrap();
        assert_eq!(std::fs::read(ssh.join("id_ed25519")).unwrap(), b"PRIVATE KEY", "{done}");
        assert_eq!(std::fs::read(ssh.join("sub").join("extra")).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&home);
    }
}
