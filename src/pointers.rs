//! Where the viewers of this computer point, for one another: a session whose viewer shares its
//! pointer (`ToAgent::SharePointer`) writes where it is on the desktop to a file of its own, in a
//! folder only this account reads, and every session passes the others' to its own viewer
//! (`ToClient::Pointers`). Sessions of one desktop run as one account, so they meet there.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Another viewer's pointer: an id for it, its name, and where it is on the host's desktop.
pub type Other = (u32, String, i32, i32);

/// A place not written again for this long is a viewer gone (its session ended at once).
const STALE: Duration = Duration::from_secs(3);

fn dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    // SAFETY: getuid cannot fail; the folder is the account's own.
    #[cfg(unix)]
    let d = base.join(format!("sshremote-pointers-{}", unsafe { libc::getuid() }));
    #[cfg(not(unix))]
    let d = base.join("sshremote-pointers");
    std::fs::create_dir_all(&d).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        // Someone else's folder of that name is not used.
        if std::fs::metadata(&d).ok()?.uid() != unsafe { libc::getuid() } {
            return None;
        }
    }
    Some(d)
}

/// A name as another viewer is shown it: one line, not too long.
pub fn clean_name(n: &str) -> String {
    n.chars().filter(|c| !c.is_control()).take(40).collect::<String>().trim().to_string()
}

#[derive(Default)]
pub struct Shared {
    /// This session's file, while its viewer shares its pointer, and the name it goes by.
    mine: Option<(PathBuf, String)>,
    written: Option<Instant>,
    at: Option<(i32, i32)>,
    looked: Option<Instant>,
    told: Vec<Other>,
}

impl Shared {
    /// The viewer shares its pointer under `name` from now on, or (None) no longer.
    pub fn set(&mut self, name: Option<String>) {
        if let Some((f, _)) = self.mine.take() {
            let _ = std::fs::remove_file(f);
        }
        let name = name.map(|n| clean_name(&n)).filter(|n| !n.is_empty());
        self.mine = name.and_then(|n| Some((dir()?.join(format!("{}.json", std::process::id())), n)));
        self.written = None;
        if let Some(at) = self.at {
            self.moved(at);
        }
    }

    /// The viewer's pointer is at `at` on the desktop: written, a few dozen times a second at
    /// most (and again every second, so others know it is still there).
    pub fn moved(&mut self, at: (i32, i32)) {
        let fresh = self.at != Some(at);
        self.at = Some(at);
        let Some((f, name)) = &self.mine else { return };
        let due = self.written.is_none_or(|t| t.elapsed() >= if fresh { Duration::from_millis(33) } else { Duration::from_secs(1) });
        if !due {
            return;
        }
        let text = serde_json::json!({ "name": name, "x": at.0, "y": at.1 }).to_string();
        if crate::sys::write_private(&f.with_extension("new"), text.as_bytes()).and_then(|()| std::fs::rename(f.with_extension("new"), f).map_err(|e| e.to_string())).is_ok() {
            self.written = Some(Instant::now());
        }
    }

    /// Kept up while the viewer shares: written again when it has not been for a second.
    pub fn tick(&mut self) {
        if let Some(at) = self.at.filter(|_| self.written.is_some_and(|t| t.elapsed() >= Duration::from_secs(1))) {
            self.moved(at);
        }
    }

    /// The other viewers' pointers, when they changed since last told (looked at ten times a
    /// second at most).
    pub fn others(&mut self) -> Option<Vec<Other>> {
        if self.looked.is_some_and(|t| t.elapsed() < Duration::from_millis(100)) {
            return None;
        }
        self.looked = Some(Instant::now());
        let d = dir()?;
        let me = format!("{}.json", std::process::id());
        let mut now = Vec::new();
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(pid) = name.strip_suffix(".json").and_then(|p| p.parse::<u32>().ok()) else { continue };
            if name == me {
                continue;
            }
            let fresh = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age < STALE);
            if !fresh {
                // Gone for a while: its session ended without taking it away.
                if e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE * 20) {
                    let _ = std::fs::remove_file(e.path());
                }
                continue;
            }
            let Some(v) = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else { continue };
            let (Some(x), Some(y)) = (v["x"].as_i64(), v["y"].as_i64()) else { continue };
            now.push((pid, clean_name(v["name"].as_str().unwrap_or("")), x as i32, y as i32));
            if now.len() >= 16 {
                break;
            }
        }
        now.sort();
        (now != self.told).then(|| {
            self.told = now.clone();
            now
        })
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some((f, _)) = self.mine.take() {
            let _ = std::fs::remove_file(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One session's viewer sharing its pointer is seen by another's, with its name and place,
    /// and gone when it stops; a session never hears of its own.
    #[test]
    fn one_viewer_s_pointer_reaches_the_others() {
        let mut a = Shared::default();
        // Another session's file, as its process would write it (an id no process has).
        let d = dir().unwrap();
        let other = d.join("4000000001.json");
        std::fs::write(&other, r#"{"name":"bo@laptop","x":1910,"y":40}"#).unwrap();
        let seen = a.others().unwrap();
        assert!(seen.contains(&(4000000001, "bo@laptop".into(), 1910, 40)), "{seen:?}");
        a.set(Some("ana@desk".into()));
        a.moved((100, 200));
        let mine = d.join(format!("{}.json", std::process::id()));
        assert!(std::fs::read_to_string(&mine).unwrap().contains("ana@desk"));
        a.looked = None;
        assert!(a.others().is_none_or(|l| l.iter().all(|o| o.0 != std::process::id())), "not its own");
        std::fs::remove_file(&other).unwrap();
        a.looked = None;
        assert!(a.others().unwrap().iter().all(|o| o.0 != 4000000001), "gone");
        a.set(None);
        assert!(!mine.exists());
    }
}
