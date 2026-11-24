//! Backups of what sshremote knows about hosts: recent and favorite hosts, when each was last
//! reached, settings, ~/.ssh/config and known_hosts. With a passphrase, the saved passwords and
//! every file of ~/.ssh (private keys too) go in as well, encrypted. A copy without them is also
//! made automatically once a day.

use std::path::{Path, PathBuf};

use crate::sys::write_private;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const KEEP_AUTO: usize = 14;
/// Shorter passphrases are refused when private keys are to be sealed with them.
pub const MIN_PASSPHRASE: usize = 8;
/// Larger files in ~/.ssh (sockets, logs) are not keys or settings and are left out.
const MAX_SSH_FILE: u64 = 1 << 20;
/// PBKDF2 rounds for new backups (OWASP's figure for HMAC-SHA256); a backup names its own count.
const ROUNDS: u32 = 600_000;

/// Where backups made by hand go: Documents/sshremote.
pub fn folder() -> PathBuf {
    let home = crate::sys::home_dir();
    let docs = home.join("Documents");
    (if docs.is_dir() { docs } else { home }).join("sshremote")
}

fn auto_folder() -> Option<PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("backups"))
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    crate::sys::local_time(secs).replace([' ', ':'], "-")
}

/// Files of ~/.ssh a backup's sealed part leaves alone: config and known_hosts travel in the
/// plain part and are restored on their own terms, and earlier copies are not worth keeping.
fn ssh_file_kept_apart(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "config"
        || name.starts_with("known_hosts")
        || name.ends_with(".old")
        || name.rsplit_once(".bak").is_some_and(|(_, n)| n.chars().all(|c| c.is_ascii_digit()))
}

/// Makes a folder of ~/.ssh (and those above it) that only this user may enter.
fn private_dir(dir: &Path) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// `path` with ".bak" added, or ".bak2", ".bak3"... so an earlier copy is never overwritten.
fn free_bak(path: &Path) -> PathBuf {
    (1..)
        .map(|n| {
            let mut p = path.as_os_str().to_owned();
            p.push(if n == 1 { ".bak".to_string() } else { format!(".bak{n}") });
            PathBuf::from(p)
        })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("a free name")
}

/// A key from a passphrase by PBKDF2-HMAC-SHA256, stretched so guessing is slow.
fn pbkdf2(passphrase: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mac = <Hmac<Sha256> as Mac>::new_from_slice(passphrase.as_bytes()).expect("HMAC takes any key length");
    let mut u: [u8; 32] = mac.clone().chain_update(salt).chain_update(1u32.to_be_bytes()).finalize().into_bytes().into();
    let mut t = u;
    for _ in 1..rounds {
        u = mac.clone().chain_update(u).finalize().into_bytes().into();
        t.iter_mut().zip(u).for_each(|(a, b)| *a ^= b);
    }
    t
}

/// The key of backups made before PBKDF2: iterated SHA-256.
fn legacy_key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    let mut h: [u8; 32] = Sha256::new().chain_update(salt).chain_update(passphrase.as_bytes()).finalize().into();
    for _ in 0..200_000 {
        h = Sha256::new().chain_update(h).chain_update(salt).finalize().into();
    }
    h
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn seal(plain: &[u8], passphrase: &str) -> Value {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut salt).expect("system random source");
    getrandom::getrandom(&mut nonce).expect("system random source");
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&pbkdf2(passphrase, &salt, ROUNDS)));
    let data = cipher.encrypt(Nonce::from_slice(&nonce), plain).expect("encryption");
    json!({ "kdf": "pbkdf2-sha256", "rounds": ROUNDS, "salt": B64.encode(salt), "nonce": B64.encode(nonce), "data": B64.encode(data) })
}

fn open(sealed: &Value, passphrase: &str) -> Result<Vec<u8>, String> {
    let pbkdf2_sealed = sealed["kdf"] == "pbkdf2-sha256";
    let field = |k: &str| {
        let s = sealed[k].as_str();
        if pbkdf2_sealed { s.and_then(|s| B64.decode(s).ok()) } else { s.and_then(unhex) }.ok_or("damaged backup")
    };
    let salt = field("salt")?;
    let key = if pbkdf2_sealed {
        // A count far beyond any we write would only make opening hang.
        let rounds = sealed["rounds"].as_u64().filter(|r| (1..=10_000_000).contains(r)).ok_or("damaged backup")?;
        pbkdf2(passphrase, &salt, rounds as u32)
    } else {
        legacy_key(passphrase, &salt)
    };
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(Nonce::from_slice(&field("nonce")?), field("data")?.as_slice())
        .map_err(|_| "wrong passphrase".to_string())
}

/// Settings lines that hold a password (a proxy's), kept out of a backup's open part.
fn secret_setting(line: &str) -> bool {
    line.strip_prefix("proxy=").and_then(|u| crate::proxy::Proxy::parse(u).ok()).is_some_and(|p| p.login.is_some_and(|(_, pw)| !pw.is_empty()))
}

/// The backup as JSON; with a passphrase the saved passwords are included, encrypted.
pub fn create(passphrase: Option<&str>) -> String {
    let mut files = serde_json::Map::new();
    let mut secret_settings = Vec::new();
    if let Some(dir) = crate::hosts::config_dir()
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // The crash report is this computer's own, not a setting.
            if (name.ends_with(".txt") || name == "computers.json")
                && name != "crash.txt"
                && let Ok(mut text) = std::fs::read_to_string(e.path())
            {
                if name == "settings.txt" {
                    let (secret, open): (Vec<&str>, Vec<&str>) = text.lines().partition(|l| secret_setting(l));
                    secret_settings = secret.into_iter().map(Value::from).collect();
                    text = open.iter().map(|l| format!("{l}\n")).collect();
                }
                files.insert(name, Value::String(text));
            }
        }
    }
    let mut ssh = serde_json::Map::new();
    for name in ["config", "known_hosts"] {
        if let Ok(text) = std::fs::read_to_string(crate::sys::ssh_dir().join(name)) {
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
            ssh_files().into_iter().map(|(name, data)| (name, Value::String(B64.encode(data)))).collect();
        let secrets = json!({ "passwords": passwords, "ssh": files, "encoding": "base64", "settings": secret_settings });
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
                && !ssh_file_kept_apart(&name)
                && meta.len() <= MAX_SSH_FILE
                && let Ok(data) = std::fs::read(e.path())
            {
                out.push((name, data));
            }
        }
    }
    let mut out = Vec::new();
    walk(&crate::sys::ssh_dir(), "", 0, &mut out);
    out.sort();
    out
}

/// Writes a file of ~/.ssh from a backup: missing ones are created, changed ones replaced with the
/// current file kept beside it as NAME.bak (NAME.bak2 when that is taken). authorized_keys gains
/// the backup's lines rather than losing those added since. Returns whether anything was written.
fn put_ssh_file(ssh: &Path, rel: &str, data: &[u8]) -> Result<bool, String> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains(['\\', ':'])) {
        return Err(format!("unsafe name in the backup: {rel}"));
    }
    let path = parts.iter().fold(ssh.to_path_buf(), |p, part| p.join(part));
    let now = std::fs::read(&path).ok();
    let data = if rel == "authorized_keys" && let Some(now) = &now {
        union(&String::from_utf8_lossy(now), &String::from_utf8_lossy(data)).into_bytes()
    } else {
        data.to_vec()
    };
    if now.as_deref() == Some(data.as_slice()) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    if now.is_some() {
        std::fs::rename(&path, free_bak(&path)).map_err(|e| e.to_string())?;
    }
    // Readable by this user only from the start: ssh refuses private keys that others can read.
    write_private(&path, &data)?;
    Ok(true)
}

/// Writes a backup to the backup folder and returns its path.
pub fn save(passphrase: Option<&str>) -> Result<PathBuf, String> {
    let dir = folder();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = create(passphrase);
    // Two in the same minute are two files, not one written over the other.
    let stamp = stamp();
    for n in 1..100 {
        let name = if n == 1 { format!("sshremote-backup-{stamp}.json") } else { format!("sshremote-backup-{stamp} ({n}).json") };
        let path = dir.join(name);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        // Its user's alone, like the settings it holds.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        match o.open(&path) {
            Ok(mut f) => {
                std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err("too many backups this minute".into())
}

/// Keeps one automatic backup a day (without passwords), the last two weeks of them.
pub fn auto() {
    let Some(dir) = auto_folder() else { return };
    let day = stamp().get(..10).unwrap_or_default().to_string();
    let path = dir.join(format!("auto-{day}.json"));
    if path.exists() || std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = write_private(&path, create(None).as_bytes());
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

/// Deletes a backup: one in the backup folder or among the automatic ones, nothing else.
pub fn delete(path: &Path) -> Result<(), String> {
    let dir = path.parent().and_then(|d| std::fs::canonicalize(d).ok());
    let ours = [Some(folder()), auto_folder()].into_iter().flatten().filter_map(|d| std::fs::canonicalize(d).ok()).any(|d| Some(d) == dir);
    if !ours || path.extension().is_none_or(|x| x != "json") {
        return Err("only backups in the backup folders are deleted here".into());
    }
    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
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
    // A wrong passphrase stops the restore before anything has been changed.
    let pass = passphrase.filter(|p| !p.is_empty());
    // Each sealed part is opened once (the key takes a while to stretch), before anything changes.
    let mut opened = std::collections::HashMap::new();
    for key in ["passwords", "secrets"] {
        if let (Some(sealed), Some(p)) = (v.get(key), pass) {
            opened.insert(key, open(sealed, p)?);
        }
    }
    let dir = crate::hosts::config_dir().ok_or("no settings folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut done = Vec::new();
    if let Some(files) = v["files"].as_object() {
        // The computers by their address, the backup's winning; a backup older than the list
        // brings its recent hosts, favorites and when each was opened.
        let older = ["recent.txt", "favorites.txt", "seen.txt"];
        let whole = files.contains_key("computers.json");
        let computers = match files.get("computers.json").and_then(Value::as_str) {
            Some(text) => crate::computers::parse(text),
            None if older.iter().any(|n| files.contains_key(*n)) => {
                let from = std::env::temp_dir().join(format!("sshremote-restore-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&from);
                for n in older {
                    if let Some(t) = files.get(n).and_then(Value::as_str) {
                        let _ = std::fs::write(from.join(n), t);
                    }
                }
                let list = crate::computers::from_older_files(Some(&from));
                let _ = std::fs::remove_dir_all(&from);
                list
            }
            None => Vec::new(),
        };
        crate::computers::merge(computers, whole);
        for (name, text) in files {
            let older_list = older.contains(&name.as_str());
            let (Some(text), true) = (text.as_str(), name.ends_with(".txt") && !older_list && !name.contains(['/', '\\', ':'])) else {
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
            write_private(&dir.join(name), merged.as_bytes())?;
            if name == "settings.txt" {
                // At once: the next setting saved would otherwise write the cached old ones back.
                crate::hosts::reload_settings();
            }
        }
        done.push(format!("{} lists", files.len()));
    }
    let ssh = crate::sys::ssh_dir();
    private_dir(&ssh)?;
    if let Some(known) = v["ssh"]["known_hosts"].as_str() {
        let here = std::fs::read_to_string(ssh.join("known_hosts")).unwrap_or_default();
        write_private(&ssh.join("known_hosts"), union(&here, known).as_bytes())?;
        done.push("known_hosts".into());
    }
    if ssh_config && let Some(config) = v["ssh"]["config"].as_str() {
        let path = ssh.join("config");
        let kept = if path.exists() {
            let bak = free_bak(&path);
            std::fs::copy(&path, &bak).map_err(|e| e.to_string())?;
            format!(" (the old one is {})", bak.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()))
        } else {
            String::new()
        };
        write_private(&path, config.as_bytes())?;
        done.push(format!("~/.ssh/config{kept}"));
    }
    // The sealed part: ~/.ssh's files first (they need nothing else), then the passwords, whose
    // store may be missing on this machine.
    let mut passwords: Vec<(String, String)> = Vec::new();
    if v.get("passwords").is_some() {
        match opened.get("passwords") {
            Some(plain) => {
                let list: serde_json::Map<String, Value> = serde_json::from_slice(plain).map_err(|_| "damaged backup")?;
                passwords.extend(list.iter().filter_map(|(h, s)| Some((h.clone(), s.as_str()?.to_string()))));
            }
            None => done.push("passwords skipped (no passphrase)".into()),
        }
    }
    if v.get("secrets").is_some() {
        match opened.get("secrets") {
            Some(plain) => {
                let secrets: Value = serde_json::from_slice(plain).map_err(|_| "damaged backup")?;
                let list = secrets["passwords"].as_object().cloned().unwrap_or_default();
                passwords.extend(list.iter().filter_map(|(h, s)| Some((h.clone(), s.as_str()?.to_string()))));
                // Settings with a password in them (a proxy's), as the open ones: the backup's win.
                for line in secrets["settings"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    if let Some((k, v)) = line.split_once('=') {
                        crate::hosts::set_setting(k, v);
                    }
                }
                // Backups made before config and known_hosts were kept apart still hold them:
                // those are restored above on their own terms, never replaced from here.
                let files: Vec<(String, Value)> = secrets["ssh"]
                    .as_object()
                    .map(|m| m.iter().filter(|(rel, _)| !ssh_file_kept_apart(rel)).map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();
                if ssh_files {
                    let mut written = 0;
                    let base64 = secrets["encoding"] == "base64";
                    for (rel, data) in &files {
                        let data = data.as_str();
                        let data = if base64 { data.and_then(|d| B64.decode(d).ok()) } else { data.and_then(unhex) };
                        let data = data.ok_or("damaged backup")?;
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
    if !passwords.is_empty() {
        let mut failed = None;
        let mut saved = 0;
        for (host, s) in &passwords {
            match crate::vault::set(host, s) {
                Ok(()) => saved += 1,
                Err(e) => failed = Some(e),
            }
        }
        done.push(match failed {
            None => format!("{saved} passwords"),
            Some(e) => format!("{saved} of {} passwords ({e})", passwords.len()),
        });
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
    fn pbkdf2_matches_the_published_vectors() {
        let hex_of = |k: [u8; 32]| crate::sys::hex(&k);
        assert_eq!(hex_of(pbkdf2("password", b"salt", 1)), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
        assert_eq!(hex_of(pbkdf2("password", b"salt", 2)), "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43");
        assert_eq!(hex_of(pbkdf2("password", b"salt", 4096)), "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a");
    }

    /// Backups sealed before PBKDF2 still open.
    #[test]
    fn older_backups_still_open() {
        let salt = [7u8; 16];
        let nonce = [9u8; 12];
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&legacy_key("pass", &salt)));
        let data = cipher.encrypt(Nonce::from_slice(&nonce), b"old".as_slice()).unwrap();
        let sealed = json!({ "salt": crate::sys::hex(&salt), "nonce": crate::sys::hex(&nonce), "data": crate::sys::hex(&data) });
        assert_eq!(open(&sealed, "pass").unwrap(), b"old");
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

    /// What the sealed part leaves to the plain one, and earlier copies, are not restored from it;
    /// authorized_keys keeps the keys added since the backup.
    #[test]
    fn restoring_keys_leaves_config_and_known_hosts_and_keeps_new_authorized_keys() {
        for kept in ["config", "known_hosts", "known_hosts.old", "id_ed25519.bak", "id_rsa.bak2", "x.old"] {
            assert!(ssh_file_kept_apart(kept), "{kept}");
        }
        for taken in ["id_ed25519", "id_ed25519.pub", "authorized_keys", "keys/work"] {
            assert!(!ssh_file_kept_apart(taken), "{taken}");
        }
        let dir = std::env::temp_dir().join(format!("sshremote-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("authorized_keys"), "ssh-ed25519 AAAA monday\nssh-ed25519 BBBB colleague\n").unwrap();
        assert!(put_ssh_file(&dir, "authorized_keys", b"ssh-ed25519 AAAA monday\nssh-ed25519 CCCC old\n").unwrap());
        let now = std::fs::read_to_string(dir.join("authorized_keys")).unwrap();
        assert_eq!(now, "ssh-ed25519 AAAA monday\nssh-ed25519 BBBB colleague\nssh-ed25519 CCCC old\n");
        // A second replacement keeps the first copy too.
        assert!(put_ssh_file(&dir, "authorized_keys", b"ssh-ed25519 DDDD\n").unwrap());
        assert!(dir.join("authorized_keys.bak").exists() && dir.join("authorized_keys.bak2").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_backups_in_the_folders_are_deleted() {
        let elsewhere = std::env::temp_dir().join(format!("sshremote-not-a-backup-{}.json", std::process::id()));
        std::fs::write(&elsewhere, "{}").unwrap();
        assert!(delete(&elsewhere).is_err());
        assert!(elsewhere.exists());
        let _ = std::fs::remove_file(&elsewhere);
    }

    #[test]
    fn union_keeps_order_and_drops_repeats() {
        assert_eq!(union("a\nb\n", "b\nc"), "a\nb\nc\n");
    }

    /// A whole backup and restore in a scratch home folder. It changes the process environment
    /// while it runs, so it runs only on request: cargo test -- --ignored --test-threads=1
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
        // The variables are put back however this ends: the tests after it share the process.
        struct Back(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Back {
            fn drop(&mut self) {
                for (k, v) in &self.0 {
                    match v {
                        Some(v) => unsafe { std::env::set_var(k, v) },
                        None => unsafe { std::env::remove_var(k) },
                    }
                }
            }
        }
        let vars = ["USERPROFILE", "HOME", "APPDATA", "XDG_CONFIG_HOME"];
        let _back = Back(vars.iter().map(|&k| (k, std::env::var_os(k))).collect());
        for var in vars {
            unsafe { std::env::set_var(var, &home) };
        }
        let settings = crate::hosts::config_dir().unwrap().join("settings.txt");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, "theme=dark\nproxy=http://me:s3cret@10.0.0.1:3128\n").unwrap();
        let file = home.join("b.json");
        std::fs::write(&file, create(Some("correct horse"))).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("PRIVATE KEY"), "keys are only stored encrypted");
        assert!(!text.contains("s3cret") && text.contains("theme=dark"), "a proxy's password is only stored encrypted");
        let open = home.join("open.json");
        std::fs::write(&open, create(None)).unwrap();
        assert!(!std::fs::read_to_string(&open).unwrap().contains("s3cret"));
        // Without the passphrase the proxy here stays; with it the backup's comes back.
        std::fs::write(&settings, "proxy=socks5h://10.0.0.2:1080\n").unwrap();
        restore(&open, None, false, false).unwrap();
        let now = std::fs::read_to_string(&settings).unwrap();
        assert!(now.contains("proxy=socks5h://10.0.0.2:1080") && now.contains("theme=dark"), "{now}");
        restore(&file, Some("correct horse"), false, false).unwrap();
        let now = std::fs::read_to_string(&settings).unwrap();
        assert!(now.contains("proxy=http://me:s3cret@10.0.0.1:3128") && !now.contains("10.0.0.2"), "{now}");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&settings).unwrap().permissions()) & 0o777, 0o600);
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
