//! Premium licenses: a small JSON record signed by sshremote.dev's key and checked here with
//! OpenSSH's ssh-keygen, offline. It unlocks the registry's premium extensions while it lasts.
//!
//! A license file is the record, a line `-----SIGNATURE-----`, and the ssh signature of the
//! record's bytes in the "sshremote-license" namespace:
//!   {"id":"L-...","email":"...","plan":"premium","expires":"2027-11-10"}

use std::io::Write;
use std::path::PathBuf;

const MARK: &str = "\n-----SIGNATURE-----\n";
const NAMESPACE: &str = "sshremote-license";

#[derive(Clone, Debug, PartialEq)]
pub struct License {
    pub id: String,
    pub email: String,
    pub plan: String,
    /// YYYY-MM-DD, the last day it is good for.
    pub expires: String,
}

impl License {
    pub fn active(&self) -> bool {
        self.expires.as_str() >= today().as_str()
    }
}

fn file() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(p) = TEST_FILE.with(|f| f.borrow().clone()) {
        return Some(p);
    }
    crate::hosts::config_dir().map(|d| d.join("license"))
}

#[cfg(test)]
thread_local!(static TEST_FILE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) });

/// Today in UTC as YYYY-MM-DD.
pub fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86400) as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Reads and checks a license's text.
pub fn parse(text: &str) -> Result<License, String> {
    let text = text.replace("\r\n", "\n");
    let (record, sig) = text.split_once(MARK).ok_or("not a sshremote license")?;
    check_signature(record.as_bytes(), sig.as_bytes())?;
    let v: serde_json::Value = serde_json::from_str(record).map_err(|e| format!("license: {e}"))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let l = License { id: s("id"), email: s("email"), plan: s("plan"), expires: s("expires") };
    if l.expires.len() != 10 {
        return Err("license: no end date".into());
    }
    Ok(l)
}

fn check_signature(data: &[u8], sig: &[u8]) -> Result<(), String> {
    #[cfg(test)]
    if let Some(key) = TEST_KEY.with(|k| k.borrow().clone()) {
        return crate::ext::verify_with(data, sig, &key, NAMESPACE);
    }
    crate::ext::verify_with(data, sig, crate::ext::SIGNER, NAMESPACE)
}

#[cfg(test)]
thread_local!(static TEST_KEY: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) });

/// The license installed here, if it reads and its signature holds.
pub fn current() -> Option<License> {
    parse(&std::fs::read_to_string(file()?).ok()?).ok()
}

/// Whether premium extensions may be installed.
pub fn premium() -> bool {
    current().is_some_and(|l| l.active())
}

pub fn install(text: &str) -> Result<License, String> {
    let l = parse(text)?;
    let f = file().ok_or("no settings folder")?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::File::create(&f).and_then(|mut o| o.write_all(text.as_bytes())).map_err(|e| e.to_string())?;
    Ok(l)
}

pub fn remove() -> Result<(), String> {
    std::fs::remove_file(file().ok_or("no settings folder")?).map_err(|e| e.to_string())
}

pub fn describe(l: &License) -> String {
    let state = if l.active() { format!("good until {}", l.expires) } else { format!("ended {}", l.expires) };
    format!("{} for {}, {state} ({})", l.plan, l.email, l.id)
}

/// `sshremote license install FILE | show | remove`
pub fn cli(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("install") => {
            let f = args.get(1).ok_or("license install FILE")?;
            let l = install(&std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?)?;
            println!("installed: {}", describe(&l));
            Ok(())
        }
        Some("show") | None => {
            println!("{}", current().map_or("no license: the free edition".to_string(), |l| describe(&l)));
            Ok(())
        }
        Some("remove") => remove(),
        _ => Err("usage: sshremote license [show | install FILE | remove]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        let t = today();
        assert!(t.len() == 10 && t.as_str() > "2025-01-01" && t.as_str() < "2100-01-01", "{t}");
    }

    /// Signed with a key made here: good, an ended one, a changed record, and the gate.
    #[test]
    fn signed_licenses() {
        let dir = std::env::temp_dir().join(format!("sshremote-license-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("key");
        let mut cmd = std::process::Command::new("ssh-keygen");
        cmd.args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&key).stdin(std::process::Stdio::null());
        assert!(cmd.status().unwrap().success());
        // Windows' OpenSSH takes only a private key no one else may read.
        #[cfg(windows)]
        {
            let me = std::env::var("USERNAME").unwrap();
            let mut icacls = std::process::Command::new("icacls");
            icacls.arg(&key).args(["/inheritance:r", "/grant:r", &format!("{me}:F")]).stdout(std::process::Stdio::null());
            assert!(icacls.status().unwrap().success());
        }
        let public = std::fs::read_to_string(dir.join("key.pub")).unwrap();
        TEST_KEY.with(|k| *k.borrow_mut() = Some(public.split_whitespace().take(2).collect::<Vec<_>>().join(" ")));
        TEST_FILE.with(|f| *f.borrow_mut() = Some(dir.join("license")));
        let sign = |record: &str| {
            let r = dir.join("record");
            std::fs::write(&r, record).unwrap();
            let _ = std::fs::remove_file(dir.join("record.sig"));
            let mut cmd = std::process::Command::new("ssh-keygen");
            cmd.args(["-q", "-Y", "sign", "-n", NAMESPACE, "-f"]).arg(&key).arg(&r).stdin(std::process::Stdio::null());
            assert!(cmd.status().unwrap().success());
            format!("{record}{MARK}{}", std::fs::read_to_string(dir.join("record.sig")).unwrap())
        };
        assert!(!premium());
        let good = sign(r#"{"id":"L-1","email":"a@b.c","plan":"premium","expires":"2099-12-31"}"#);
        assert_eq!(install(&good).unwrap().email, "a@b.c");
        assert!(premium());
        let old = sign(r#"{"id":"L-2","email":"a@b.c","plan":"premium","expires":"2001-01-01"}"#);
        install(&old).unwrap();
        assert!(!premium(), "an ended license unlocks nothing");
        let forged = good.replace("2099-12-31", "2199-12-31");
        assert!(install(&forged).is_err());
        remove().unwrap();
        assert!(current().is_none());
    }
}
