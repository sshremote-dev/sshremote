//! Agent side of file uploads: files dropped on the viewer are written to the
//! remote user's Downloads folder, never overwriting an existing file.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::proto::ToClient;

/// Progress is reported about this often; the viewer keeps at most a few reports' worth in flight.
pub const REPORT: u64 = 256 << 10;

struct Upload {
    file: File,
    path: PathBuf,
    size: u64,
    received: u64,
    reported: u64,
}

#[derive(Default)]
pub struct Inbox {
    open: HashMap<u32, Upload>,
}

fn downloads() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    home.map(|h| h.join("Downloads")).unwrap_or_else(std::env::temp_dir)
}

/// The base name only, so a crafted name cannot climb out of the folder.
fn clean(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let s: String = base.chars().filter(|c| !c.is_control() && !":*?\"<>|".contains(*c)).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "file".into() } else { s }
}

/// `name`, or `name (2)`, `name (3)`... whichever does not exist yet in `dir`.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut p = dir.join(name);
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}

impl Inbox {
    pub fn start(&mut self, id: u32, size: u64, name: &str) -> Option<ToClient> {
        let dir = downloads();
        let _ = std::fs::create_dir_all(&dir);
        let path = free_path(&dir, &clean(name));
        match File::create(&path) {
            Ok(file) => {
                self.open.insert(id, Upload { file, path, size, received: 0, reported: 0 });
                None
            }
            Err(e) => Some(ToClient::FileDone { id, ok: false, text: format!("{}: {e}", path.display()) }),
        }
    }

    pub fn data(&mut self, id: u32, data: &[u8]) -> Option<ToClient> {
        let u = self.open.get_mut(&id)?;
        if let Err(e) = u.file.write_all(data) {
            let u = self.open.remove(&id)?;
            let _ = std::fs::remove_file(&u.path);
            return Some(ToClient::FileDone { id, ok: false, text: format!("{}: {e}", u.path.display()) });
        }
        u.received += data.len() as u64;
        (u.received - u.reported >= REPORT).then(|| {
            u.reported = u.received;
            ToClient::FileProgress { id, received: u.received }
        })
    }

    pub fn end(&mut self, id: u32) -> Option<ToClient> {
        let mut u = self.open.remove(&id)?;
        let ok = u.file.flush().is_ok() && u.received == u.size;
        if !ok {
            let _ = std::fs::remove_file(&u.path);
        }
        let text =
            if ok { u.path.display().to_string() } else { format!("incomplete: {} of {} bytes", u.received, u.size) };
        Some(ToClient::FileDone { id, ok, text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(clean("../../etc/passwd"), "passwd");
        assert_eq!(clean("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(clean("a:b?.txt"), "ab.txt");
        assert_eq!(clean(".."), "file");
    }

    #[test]
    fn existing_files_are_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("desk-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("r.txt"), b"x").unwrap();
        assert_eq!(free_path(&dir, "r.txt"), dir.join("r (2).txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
