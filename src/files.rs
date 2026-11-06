//! File transfer, used the same way in both directions: the receiving side
//! writes into its user's Downloads folder (never overwriting a file), the
//! sending side streams files with a bounded amount unacknowledged, and the
//! agent lists folders for the viewer's file panel.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::proto::{DirEntry, ToClient};

/// Progress is reported about this often.
pub const REPORT: u64 = 256 << 10;
/// Unacknowledged bytes per transfer; more would only queue in ssh and delay the screen.
pub const WINDOW: u64 = 1 << 20;
pub const CHUNK: usize = 64 << 10;

pub fn downloads() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    home.map(|h| h.join("Downloads")).unwrap_or_else(std::env::temp_dir)
}

fn home() -> PathBuf {
    downloads().parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir)
}

/// One path component made safe: no separators, drive letters or reserved characters.
fn clean(name: &str) -> String {
    let s: String = name.chars().filter(|c| !c.is_control() && !":*?\"<>|/\\".contains(*c)).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "file".into() } else { s }
}

/// A relative name such as "folder/sub/file.txt" as components that cannot climb out of the target folder.
fn clean_relative(name: &str) -> Vec<String> {
    let parts: Vec<String> =
        name.split(['/', '\\']).filter(|p| !p.is_empty() && *p != "." && *p != "..").map(clean).collect();
    if parts.is_empty() { vec!["file".into()] } else { parts }
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

/// What the receiving side has to tell the sender.
pub enum Note {
    Progress { id: u32, received: u64 },
    Done { id: u32, ok: bool, text: String },
}

impl Note {
    /// The agent's answer to an upload.
    pub fn to_client(self) -> ToClient {
        match self {
            Note::Progress { id, received } => ToClient::FileProgress { id, received },
            Note::Done { id, ok, text } => ToClient::FileDone { id, ok, text },
        }
    }
}

struct Incoming {
    file: File,
    path: PathBuf,
    size: u64,
    received: u64,
    reported: u64,
}

/// Receiving side, writing under the local Downloads folder.
#[derive(Default)]
pub struct Inbox {
    open: HashMap<u32, Incoming>,
}

impl Inbox {
    pub fn start(&mut self, id: u32, size: u64, name: &str) -> Option<Note> {
        let parts = clean_relative(name);
        let mut dir = downloads();
        for p in &parts[..parts.len() - 1] {
            dir.push(p);
        }
        let _ = std::fs::create_dir_all(&dir);
        let path = free_path(&dir, &parts[parts.len() - 1]);
        match File::create(&path) {
            Ok(file) => {
                self.open.insert(id, Incoming { file, path, size, received: 0, reported: 0 });
                None
            }
            Err(e) => Some(Note::Done { id, ok: false, text: format!("{}: {e}", path.display()) }),
        }
    }

    pub fn data(&mut self, id: u32, data: &[u8]) -> Option<Note> {
        let u = self.open.get_mut(&id)?;
        if let Err(e) = u.file.write_all(data) {
            let u = self.open.remove(&id)?;
            let _ = std::fs::remove_file(&u.path);
            return Some(Note::Done { id, ok: false, text: format!("{}: {e}", u.path.display()) });
        }
        u.received += data.len() as u64;
        (u.received - u.reported >= REPORT).then(|| {
            u.reported = u.received;
            Note::Progress { id, received: u.received }
        })
    }

    /// Closes a transfer; `sent_ok` is false when the sender gave up.
    pub fn end(&mut self, id: u32, sent_ok: bool, why: &str) -> Option<Note> {
        let mut u = self.open.remove(&id)?;
        let ok = sent_ok && u.file.flush().is_ok() && u.received == u.size;
        if !ok {
            let _ = std::fs::remove_file(&u.path);
        }
        let text = if ok {
            u.path.display().to_string()
        } else if !sent_ok {
            why.to_string()
        } else {
            format!("incomplete: {} of {} bytes", u.received, u.size)
        };
        Some(Note::Done { id, ok, text })
    }

    pub fn received(&self, id: u32) -> Option<(u64, u64)> {
        self.open.get(&id).map(|u| (u.received, u.size))
    }
}

struct Outgoing {
    id: u32,
    path: PathBuf,
    name: String,
    file: Option<File>,
    size: u64,
    sent: u64,
    acked: u64,
}

/// Agent side of downloads: queued files sent one after another.
#[derive(Default)]
pub struct Outbox {
    queue: VecDeque<Outgoing>,
    next_id: u32,
}

/// Every file under `dir`, with names relative to `base`; links are not followed.
pub fn walk(dir: &Path, base: &str, out: &mut Vec<(PathBuf, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        let name = format!("{base}/{}", e.file_name().to_string_lossy());
        if t.is_dir() {
            walk(&e.path(), &name, out);
        } else if t.is_file() {
            out.push((e.path(), name));
        }
    }
}

impl Outbox {
    /// Queues a file or a whole folder; returns an error message to send at once, if any.
    pub fn fetch(&mut self, path: &str) -> Option<ToClient> {
        let p = PathBuf::from(path);
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let mut files = Vec::new();
        if p.is_dir() {
            walk(&p, &name, &mut files);
        } else if p.is_file() {
            files.push((p.clone(), name));
        } else {
            return Some(ToClient::FetchEnd { id: 0, ok: false, text: format!("{path}: not found") });
        }
        for (path, name) in files {
            self.next_id += 1;
            self.queue.push_back(Outgoing { id: self.next_id, path, name, file: None, size: 0, sent: 0, acked: 0 });
        }
        None
    }

    pub fn ack(&mut self, id: u32, received: u64) {
        if let Some(o) = self.queue.iter_mut().find(|o| o.id == id) {
            o.acked = received;
        }
    }

    pub fn busy(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Up to `budget` bytes of the current download, as messages to send.
    pub fn pump(&mut self, budget: usize) -> Vec<ToClient> {
        let mut out = Vec::new();
        let mut spent = 0;
        while spent < budget {
            let Some(o) = self.queue.front_mut() else { break };
            if o.file.is_none() {
                match File::open(&o.path) {
                    Ok(f) => {
                        o.size = f.metadata().map(|m| m.len()).unwrap_or(0);
                        o.file = Some(f);
                        out.push(ToClient::FetchStart { id: o.id, size: o.size, name: o.name.clone() });
                    }
                    Err(e) => {
                        out.push(ToClient::FetchEnd {
                            id: o.id,
                            ok: false,
                            text: format!("{}: {e}", o.path.display()),
                        });
                        self.queue.pop_front();
                        continue;
                    }
                }
            }
            if o.sent - o.acked.min(o.sent) >= WINDOW {
                break;
            }
            let mut buf = vec![0u8; CHUNK];
            match o.file.as_mut().unwrap().read(&mut buf) {
                Ok(0) => {
                    out.push(ToClient::FetchEnd { id: o.id, ok: o.sent == o.size, text: String::new() });
                    self.queue.pop_front();
                }
                Ok(n) => {
                    buf.truncate(n);
                    o.sent += n as u64;
                    spent += n;
                    out.push(ToClient::FetchData { id: o.id, data: buf });
                }
                Err(e) => {
                    out.push(ToClient::FetchEnd { id: o.id, ok: false, text: e.to_string() });
                    self.queue.pop_front();
                }
            }
        }
        out
    }
}

/// A folder listing, folders first; an empty path is the user's home.
pub fn list(path: &str) -> ToClient {
    let dir = if path.is_empty() { home() } else { PathBuf::from(path) };
    let dir = dir.canonicalize().unwrap_or(dir);
    let shown = dir.display().to_string();
    // Windows' verbatim prefix only gets in the way of typing paths.
    let shown = shown.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(shown);
    match std::fs::read_dir(&dir) {
        Ok(rd) => {
            let mut entries: Vec<DirEntry> = rd
                .flatten()
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    Some(DirEntry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        dir: m.is_dir(),
                        size: m.len(),
                    })
                })
                .collect();
            entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            ToClient::DirList { path: shown, entries, error: String::new() }
        }
        Err(e) => ToClient::DirList { path: shown, entries: Vec::new(), error: e.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(clean_relative("../../etc/passwd"), ["etc", "passwd"]);
        assert_eq!(clean_relative("C:\\Windows\\evil.exe"), ["C", "Windows", "evil.exe"]);
        assert_eq!(clean_relative("a:b?.txt"), ["ab.txt"]);
        assert_eq!(clean_relative(".."), ["file"]);
        assert_eq!(clean_relative("photos/2026/a.jpg"), ["photos", "2026", "a.jpg"]);
    }

    #[test]
    fn existing_files_are_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("desk-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("r.txt"), b"x").unwrap();
        assert_eq!(free_path(&dir, "r.txt"), dir.join("r (2).txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_streams_every_file() {
        let dir = std::env::temp_dir().join(format!("desk-out-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), vec![1u8; 100_000]).unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), b"bb").unwrap();
        let mut out = Outbox::default();
        assert!(out.fetch(dir.to_str().unwrap()).is_none());
        let mut names = Vec::new();
        let mut bytes = 0;
        for _ in 0..10 {
            for m in out.pump(1 << 20) {
                match m {
                    ToClient::FetchStart { name, .. } => names.push(name),
                    ToClient::FetchData { data, .. } => bytes += data.len(),
                    _ => {}
                }
            }
        }
        names.sort();
        let base = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(names, [format!("{base}/a.txt"), format!("{base}/sub/b.txt")]);
        assert_eq!(bytes, 100_002);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
