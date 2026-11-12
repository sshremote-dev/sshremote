//! File transfer, used the same way in both directions: the receiving side
//! writes into its user's Downloads folder (never overwriting a file), the
//! sending side streams files with a bounded amount unacknowledged, and the
//! agent lists folders for the viewer's file panel.

pub use crate::sys::{human, local_time};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::proto::{DirEntry, FileOp, ToClient};

/// Progress is reported about this often.
pub const REPORT: u64 = 256 << 10;
/// Unacknowledged bytes per transfer; more would only queue in ssh and delay the screen.
pub const WINDOW: u64 = 1 << 20;
pub const CHUNK: usize = 64 << 10;

pub fn downloads() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    home.map(|h| h.join("Downloads")).unwrap_or_else(std::env::temp_dir)
}

pub fn home() -> PathBuf {
    downloads().parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir)
}

/// Names Windows keeps for devices, whatever their extension: "aux.c" opens the AUX device.
const RESERVED: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$", "CONOUT$",
];

/// One path component made safe: no separators, drive letters, reserved characters or device
/// names. A leading dot stays (".gitignore"); trailing dots and spaces go, as Windows drops them.
fn clean(name: &str) -> String {
    let s: String = name.chars().filter(|c| !c.is_control() && !":*?\"<>|/\\".contains(*c)).collect();
    let s = s.trim().trim_end_matches(['.', ' ']);
    if s.is_empty() || s.chars().all(|c| c == '.') {
        return "file".into();
    }
    let stem = s.split('.').next().unwrap_or(s).trim_end();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return format!("{stem}_{}", &s[stem.len()..]);
    }
    s.to_string()
}

/// Longest relative name taken, in bytes and in folders; a longer one is refused.
const MAX_NAME: usize = 4096;
const MAX_DEPTH: usize = 64;

/// A relative name such as "folder/sub/file.txt" as components that cannot climb out of the target folder.
fn clean_relative(name: &str) -> Vec<String> {
    let parts: Vec<String> =
        name.split(['/', '\\']).filter(|p| !p.is_empty() && *p != "." && *p != "..").map(clean).collect();
    if parts.is_empty() { vec!["file".into()] } else { parts }
}

/// `name` split before its extension: "a.tar" is ("a", ".tar"), ".bashrc" has none.
fn stem_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// `name`, or `name (2)`, `name (3)`... whichever is not taken yet in `dir` (a dangling link
/// takes its name too).
#[cfg(test)]
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = stem_ext(name);
    let mut p = dir.join(name);
    let mut n = 2;
    while std::fs::symlink_metadata(&p).is_ok() {
        p = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}

/// Creates a new file named `name` in `dir`, or "name (2)" and so on: the name is claimed when
/// the file is created, so two transfers at once cannot take the same one.
fn create_free(dir: &Path, name: &str) -> std::io::Result<(File, PathBuf)> {
    let (stem, ext) = stem_ext(name);
    for n in 1..10_000 {
        let path = if n == 1 { dir.join(name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(std::io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{}: no free name left", dir.join(name).display())))
}

/// Transfers open at once; more are refused rather than holding ever more files open.
const MAX_OPEN: usize = 64;

/// What a file being received is called until it is whole.
const PART: &str = ".part";

/// What the receiving side has to tell the sender.
pub enum Note {
    Progress { id: u32, received: u64 },
    Done { id: u32, ok: bool, text: String },
}

impl Note {
    /// The agent's answer to an upload.
    pub fn into_client(self) -> ToClient {
        match self {
            Note::Progress { id, received } => ToClient::FileProgress { id, received },
            Note::Done { id, ok, text } => ToClient::FileDone { id, ok, text },
        }
    }
}

struct Incoming {
    file: File,
    /// Where it is written: "name.part" beside where it goes.
    part: PathBuf,
    /// The folder and name it takes once whole.
    dir: PathBuf,
    name: String,
    size: u64,
    received: u64,
    reported: u64,
}

impl Incoming {
    /// Lets go of the file and deletes what arrived of it.
    fn discard(self) {
        drop(self.file);
        let _ = std::fs::remove_file(&self.part);
    }
}

/// Receiving side, writing under the local Downloads folder. A file is written as "name.part"
/// and takes its name once whole, so a broken transfer never looks finished; one still open when
/// the inbox goes (the connection ended) is deleted.
#[derive(Default)]
pub struct Inbox {
    open: HashMap<u32, Incoming>,
}

impl Drop for Inbox {
    fn drop(&mut self) {
        for (_, u) in self.open.drain() {
            u.discard();
        }
    }
}

impl Inbox {
    pub fn start(&mut self, id: u32, size: u64, name: &str) -> Option<Note> {
        self.start_in(id, size, &downloads(), name)
    }

    /// Like `start`, into `dir` instead of Downloads.
    pub fn start_in(&mut self, id: u32, size: u64, dir: &Path, name: &str) -> Option<Note> {
        let refuse = |text: String| Some(Note::Done { id, ok: false, text });
        if self.open.len() >= MAX_OPEN {
            return refuse("too many transfers at once".into());
        }
        if name.len() > MAX_NAME || name.split(['/', '\\']).count() > MAX_DEPTH {
            return refuse("the name is too long".into());
        }
        let mut parts = clean_relative(name);
        let leaf = parts.pop().unwrap_or_else(|| "file".into());
        let mut dir = dir.to_path_buf();
        dir.extend(&parts);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return refuse(format!("{}: {e}", dir.display()));
        }
        match create_free(&dir, &format!("{leaf}{PART}")) {
            Ok((file, part)) => {
                self.open.insert(id, Incoming { file, part, dir, name: leaf, size, received: 0, reported: 0 });
                None
            }
            Err(e) => refuse(e.to_string()),
        }
    }

    pub fn data(&mut self, id: u32, data: &[u8]) -> Option<Note> {
        let u = self.open.get_mut(&id)?;
        if u.received + data.len() as u64 > u.size {
            self.open.remove(&id)?.discard();
            return Some(Note::Done { id, ok: false, text: "more data than the file's size".into() });
        }
        if let Err(e) = u.file.write_all(data) {
            let text = format!("{}: {e}", u.dir.join(&u.name).display());
            self.open.remove(&id)?.discard();
            return Some(Note::Done { id, ok: false, text });
        }
        u.received += data.len() as u64;
        (u.received - u.reported >= REPORT).then(|| {
            u.reported = u.received;
            Note::Progress { id, received: u.received }
        })
    }

    /// Closes a transfer; `sent_ok` is false when the sender gave up. A whole file takes its
    /// name, or "name (2)" and so on when that is taken.
    pub fn end(&mut self, id: u32, sent_ok: bool, why: &str) -> Option<Note> {
        let mut u = self.open.remove(&id)?;
        let whole = sent_ok && u.file.flush().is_ok() && u.received == u.size;
        if !whole {
            let text = if !sent_ok { why.to_string() } else { format!("incomplete: {} of {} bytes", u.received, u.size) };
            u.discard();
            return Some(Note::Done { id, ok: false, text });
        }
        let Incoming { file, part, dir, name, .. } = u;
        drop(file);
        // The name is claimed by creating it, then the whole file replaces that empty one.
        let done = create_free(&dir, &name).and_then(|(claim, path)| {
            drop(claim);
            match std::fs::rename(&part, &path) {
                Ok(()) => Ok(path),
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    Err(e)
                }
            }
        });
        Some(match done {
            Ok(path) => Note::Done { id, ok: true, text: path.display().to_string() },
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                Note::Done { id, ok: false, text: e.to_string() }
            }
        })
    }

    pub fn received(&self, id: u32) -> Option<(u64, u64)> {
        self.open.get(&id).map(|u| (u.received, u.size))
    }
}

struct Outgoing {
    id: u32,
    tag: u32,
    path: PathBuf,
    name: String,
    file: Option<File>,
    size: u64,
    sent: u64,
    acked: u64,
    /// Since when the window has been full with no acknowledgement coming.
    stalled: Option<std::time::Instant>,
}

/// What waits to be sent: a file, or a folder whose files are found when it comes up.
enum Queued {
    File(Outgoing),
    Folder { path: PathBuf, name: String, tag: u32 },
}

impl Queued {
    fn tag(&self) -> u32 {
        match self {
            Queued::File(o) => o.tag,
            Queued::Folder { tag, .. } => *tag,
        }
    }
}

/// A `Fetch` tag with this bit set is answered with `FetchDone` once all its files are sent.
pub const DONE_TAG: u32 = 0x8000_0000;

/// A receiver that has taken nothing for this long is gone; its file is given up.
const STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// Agent side of downloads: queued files sent one after another.
#[derive(Default)]
pub struct Outbox {
    queue: VecDeque<Queued>,
    next_id: u32,
    buf: Vec<u8>,
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
    /// Queues a file or a whole folder; returns messages to send at once (why it cannot be).
    /// Each file is announced with `FetchStart` carrying `tag`.
    pub fn fetch(&mut self, path: &str, tag: u32) -> Vec<ToClient> {
        let p = PathBuf::from(path);
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        if p.is_dir() {
            self.queue.push_back(Queued::Folder { path: p, name, tag });
        } else if p.is_file() {
            self.push_file(p, name, tag, false);
        } else {
            let mut out = vec![ToClient::FetchEnd { id: 0, ok: false, text: format!("{path}: not found") }];
            self.done_if_last(tag, &mut out);
            return out;
        }
        Vec::new()
    }

    /// Says a request is complete once nothing of it is queued, if it asked to be told.
    fn done_if_last(&self, tag: u32, out: &mut Vec<ToClient>) {
        if tag & DONE_TAG != 0 && !self.queue.iter().any(|q| q.tag() == tag) {
            out.push(ToClient::FetchDone { tag });
        }
    }

    /// Takes the front item off the queue, saying its request is complete if it was the last.
    fn pop(&mut self, out: &mut Vec<ToClient>) {
        if let Some(q) = self.queue.pop_front() {
            self.done_if_last(q.tag(), out);
        }
    }

    fn push_file(&mut self, path: PathBuf, name: String, tag: u32, front: bool) {
        self.next_id += 1;
        let o = Outgoing { id: self.next_id, tag, path, name, file: None, size: 0, sent: 0, acked: 0, stalled: None };
        if front { self.queue.push_front(Queued::File(o)) } else { self.queue.push_back(Queued::File(o)) }
    }

    /// Puts the folder's files and folders at the front of the queue, in their order.
    fn open_folder(&mut self, path: &Path, name: &str, tag: u32) {
        let Ok(rd) = std::fs::read_dir(path) else { return };
        let mut items: Vec<(bool, PathBuf, String)> = rd
            .flatten()
            .filter_map(|e| {
                let t = e.file_type().ok()?;
                let sub = format!("{name}/{}", e.file_name().to_string_lossy());
                (t.is_dir() || t.is_file()).then(|| (t.is_dir(), e.path(), sub))
            })
            .collect();
        items.sort_by(|a, b| a.2.cmp(&b.2));
        for (dir, path, sub) in items.into_iter().rev() {
            if dir {
                self.queue.push_front(Queued::Folder { path, name: sub, tag });
            } else {
                self.push_file(path, sub, tag, true);
            }
        }
    }

    pub fn ack(&mut self, id: u32, received: u64) {
        for q in &mut self.queue {
            if let Queued::File(o) = q
                && o.id == id
            {
                (o.acked, o.stalled) = (received, None);
            }
        }
    }

    /// Drops a download, or every queued file of its request when `tag` is not 0.
    pub fn cancel(&mut self, id: u32, tag: u32) -> Vec<ToClient> {
        let mut out = Vec::new();
        self.queue.retain(|q| {
            let hit = match q {
                Queued::File(o) => o.id == id || (tag != 0 && o.tag == tag),
                Queued::Folder { .. } => tag != 0 && q.tag() == tag,
            };
            if hit && let Queued::File(o) = q && o.file.is_some() {
                out.push(ToClient::FetchEnd { id: o.id, ok: false, text: "cancelled".into() });
            }
            !hit
        });
        out
    }

    /// Whether there is something to send now (not only acknowledgements to wait for).
    pub fn busy(&self) -> bool {
        match self.queue.front() {
            Some(Queued::File(o)) => o.file.is_none() || o.sent - o.acked.min(o.sent) < WINDOW,
            Some(Queued::Folder { .. }) => true,
            None => false,
        }
    }

    /// Up to `budget` bytes of the current download, as messages to send. A file is sent at the
    /// size it had when it started, so one that grows meanwhile still arrives whole.
    pub fn pump(&mut self, budget: usize) -> Vec<ToClient> {
        let mut out = Vec::new();
        let mut spent = 0;
        while spent < budget {
            let o = match self.queue.front_mut() {
                None => break,
                Some(Queued::Folder { .. }) => {
                    let Some(Queued::Folder { path, name, tag }) = self.queue.pop_front() else { unreachable!() };
                    self.open_folder(&path, &name, tag);
                    // An empty folder completes its request at once.
                    self.done_if_last(tag, &mut out);
                    continue;
                }
                Some(Queued::File(o)) => o,
            };
            if o.file.is_none() {
                match File::open(&o.path) {
                    Ok(f) => {
                        o.size = f.metadata().map(|m| m.len()).unwrap_or(0);
                        o.file = Some(f);
                        out.push(ToClient::FetchStart { id: o.id, size: o.size, tag: o.tag, name: o.name.clone() });
                    }
                    Err(e) => {
                        out.push(ToClient::FetchEnd { id: o.id, ok: false, text: format!("{}: {e}", o.path.display()) });
                        self.pop(&mut out);
                        continue;
                    }
                }
            }
            if o.sent - o.acked.min(o.sent) >= WINDOW {
                let since = *o.stalled.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() > STALL {
                    out.push(ToClient::FetchEnd { id: o.id, ok: false, text: "the other side stopped taking the file".into() });
                    self.pop(&mut out);
                    continue;
                }
                break;
            }
            let want = (o.size - o.sent).min(CHUNK as u64) as usize;
            if want == 0 {
                out.push(ToClient::FetchEnd { id: o.id, ok: true, text: String::new() });
                self.pop(&mut out);
                continue;
            }
            self.buf.resize(want, 0);
            match o.file.as_mut().map(|f| f.read(&mut self.buf[..want])) {
                Some(Ok(0)) => {
                    let text = format!("{}: it got shorter while being sent", o.path.display());
                    out.push(ToClient::FetchEnd { id: o.id, ok: false, text });
                    self.pop(&mut out);
                }
                Some(Ok(n)) => {
                    o.sent += n as u64;
                    spent += n;
                    out.push(ToClient::FetchData { id: o.id, data: self.buf[..n].to_vec() });
                }
                Some(Err(e)) => {
                    out.push(ToClient::FetchEnd { id: o.id, ok: false, text: format!("{}: {e}", o.path.display()) });
                    self.pop(&mut out);
                }
                None => self.pop(&mut out),
            }
        }
        out
    }
}

/// The pseudo-folder listing the drives (Windows); elsewhere it is the root.
pub const DRIVES: &str = "::";

fn mtime(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

#[cfg(windows)]
fn drives() -> ToClient {
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    let entries = (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| DirEntry { name: format!("{}:\\", (b'A' + i) as char), dir: true, size: 0, mtime: 0 })
        .collect();
    ToClient::DirList { path: DRIVES.into(), entries, error: String::new() }
}

/// A folder listing, folders first; an empty path is the user's home, `DRIVES` the drives.
pub fn list(path: &str) -> ToClient {
    #[cfg(windows)]
    if path == DRIVES {
        return drives();
    }
    let path = if path == DRIVES { "/" } else { path };
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
                    // A link to a folder is shown and opened as the folder; a broken link as itself.
                    let m = std::fs::metadata(e.path()).or_else(|_| e.metadata()).ok()?;
                    Some(DirEntry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        dir: m.is_dir(),
                        size: m.len(),
                        mtime: mtime(&m),
                    })
                })
                .collect();
            entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
            ToClient::DirList { path: shown, entries, error: String::new() }
        }
        Err(e) => ToClient::DirList { path: shown, entries: Vec::new(), error: e.to_string() },
    }
}

/// Carries out a `FileOp` on this machine.
pub fn op(op: FileOp, path: &str, arg: &str) -> ToClient {
    let p = Path::new(path);
    let r = match op {
        FileOp::Mkdir => std::fs::create_dir(p).map(|_| format!("Created {path}")),
        FileOp::Rename => {
            let name = clean(arg);
            let to = p.with_file_name(&name);
            // Taken by another file (a dangling link included); the same file under another case
            // ("report" to "Report" where case does not count) is a rename all the same.
            let same = matches!((std::fs::canonicalize(p), std::fs::canonicalize(&to)), (Ok(a), Ok(b)) if a == b);
            if std::fs::symlink_metadata(&to).is_ok() && !same {
                Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{name} already exists")))
            } else {
                std::fs::rename(p, &to).map(|_| format!("Renamed to {name}"))
            }
        }
        FileOp::Delete => {
            let r = if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
                std::fs::remove_dir_all(p)
            } else {
                std::fs::remove_file(p)
            };
            r.map(|_| format!("Deleted {path}"))
        }
    };
    match r {
        Ok(text) => ToClient::OpDone { ok: true, text },
        Err(e) => ToClient::OpDone { ok: false, text: format!("{path}: {e}") },
    }
}




#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_ops_work_and_report() {
        let dir = std::env::temp_dir().join(format!("sshremote-ops-{}", std::process::id()));
        let d = dir.to_str().unwrap();
        assert!(matches!(op(FileOp::Mkdir, d, ""), ToClient::OpDone { ok: true, .. }));
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let a = dir.join("a.txt");
        assert!(matches!(op(FileOp::Rename, a.to_str().unwrap(), "b.txt"), ToClient::OpDone { ok: true, .. }));
        assert!(dir.join("b.txt").exists());
        // Another file's name is refused, a dangling link's too; the file's own under another
        // case is a rename.
        std::fs::write(dir.join("c.txt"), b"c").unwrap();
        assert!(matches!(op(FileOp::Rename, dir.join("b.txt").to_str().unwrap(), "c.txt"), ToClient::OpDone { ok: false, .. }));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("gone"), dir.join("link")).unwrap();
            assert!(matches!(op(FileOp::Rename, dir.join("c.txt").to_str().unwrap(), "link"), ToClient::OpDone { ok: false, .. }));
        }
        assert!(matches!(op(FileOp::Rename, dir.join("c.txt").to_str().unwrap(), "C.txt"), ToClient::OpDone { ok: true, .. }));
        assert!(matches!(op(FileOp::Mkdir, d, ""), ToClient::OpDone { ok: false, .. }));
        assert!(matches!(op(FileOp::Delete, d, ""), ToClient::OpDone { ok: true, .. }));
        assert!(!dir.exists());
        assert_eq!(local_time(0), "");
        assert_eq!(local_time(1_700_000_000).len(), 16);
    }

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(clean_relative("../../etc/passwd"), ["etc", "passwd"]);
        assert_eq!(clean_relative("C:\\Windows\\evil.exe"), ["C", "Windows", "evil.exe"]);
        assert_eq!(clean_relative("a:b?.txt"), ["ab.txt"]);
        assert_eq!(clean_relative(".."), ["file"]);
        assert_eq!(clean_relative("photos/2026/a.jpg"), ["photos", "2026", "a.jpg"]);
        // A repository's dot files keep their names.
        assert_eq!(clean_relative("repo/.gitignore"), ["repo", ".gitignore"]);
        assert_eq!(clean_relative(".git/config"), [".git", "config"]);
        // Windows drops trailing dots and spaces itself; all dots is no name.
        assert_eq!(clean("notes. . "), "notes");
        assert_eq!(clean("..."), "file");
        // Device names, which Windows opens whatever the extension, get an underscore.
        assert_eq!(clean("aux.c"), "aux_.c");
        assert_eq!(clean("NUL"), "NUL_");
        assert_eq!(clean("com1.tar.gz"), "com1_.tar.gz");
        assert_eq!(clean("console.log"), "console.log");
    }

    /// A file is written as "name.part" and takes its name only once whole; a transfer cut off
    /// leaves nothing behind, and a name already taken gets " (2)".
    #[test]
    fn a_file_takes_its_name_only_when_whole() {
        let dir = std::env::temp_dir().join(format!("sshremote-inbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let listing = || {
            let mut v: Vec<String> =
                std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        let mut inbox = Inbox::default();
        assert!(inbox.start_in(1, 4, &dir, "a.txt").is_none());
        assert!(inbox.data(1, b"ab").is_none());
        assert_eq!(listing(), ["a.txt.part"], "under way");
        assert!(inbox.data(1, b"cd").is_none());
        assert!(matches!(inbox.end(1, true, ""), Some(Note::Done { ok: true, .. })));
        assert_eq!(listing(), ["a.txt"], "whole");
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"abcd");

        // The same name again does not overwrite.
        assert!(inbox.start_in(2, 1, &dir, "a.txt").is_none());
        inbox.data(2, b"x");
        assert!(matches!(inbox.end(2, true, ""), Some(Note::Done { ok: true, .. })));
        assert_eq!(listing(), ["a (2).txt", "a.txt"]);

        // Cut off: short, or the connection gone with it open.
        assert!(inbox.start_in(3, 10, &dir, "b.bin").is_none());
        inbox.data(3, b"123");
        assert!(matches!(inbox.end(3, true, ""), Some(Note::Done { ok: false, .. })));
        assert!(inbox.start_in(4, 10, &dir, "c.bin").is_none());
        inbox.data(4, b"123");
        drop(inbox);
        assert_eq!(listing(), ["a (2).txt", "a.txt"], "nothing left of the broken ones");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A dangling link with the name wanted is passed over, not tried forever.
    #[cfg(unix)]
    #[test]
    fn a_dangling_link_does_not_hold_up_a_download() {
        let dir = std::env::temp_dir().join(format!("sshremote-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("r.pdf")).unwrap();
        assert_eq!(free_path(&dir, "r.pdf"), dir.join("r (2).pdf"));
        let (_, path) = create_free(&dir, "r.pdf").unwrap();
        assert_eq!(path, dir.join("r (2).pdf"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file sent is the size it was announced at, even if it grows meanwhile; and the sender
    /// waits for acknowledgements rather than spin while the window is full.
    #[test]
    fn a_growing_file_is_sent_at_its_starting_size() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("sshremote-grow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        std::fs::write(&path, vec![b'a'; 3 * CHUNK]).unwrap();
        let mut out = Outbox::default();
        assert!(out.fetch(path.to_str().unwrap(), 0).is_empty());
        let first = out.pump(CHUNK);
        assert!(matches!(first[0], ToClient::FetchStart { size, .. } if size == 3 * CHUNK as u64));
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[b'b'; 1000]).unwrap();
        let (mut sent, mut ended) = (0, None);
        for m in first.into_iter().chain(out.pump(1 << 20)) {
            match m {
                ToClient::FetchData { data, .. } => sent += data.len(),
                ToClient::FetchEnd { ok, .. } => ended = Some(ok),
                _ => {}
            }
        }
        assert_eq!((sent, ended), (3 * CHUNK, Some(true)));
        assert!(!out.busy());

        // A large one stops at the window until acknowledged.
        std::fs::write(&path, vec![0u8; 2 * WINDOW as usize]).unwrap();
        out.fetch(path.to_str().unwrap(), 0);
        let sent: usize = out.pump(4 << 20).iter().map(|m| if let ToClient::FetchData { data, .. } = m { data.len() } else { 0 }).sum();
        assert_eq!(sent as u64, WINDOW);
        assert!(!out.busy(), "nothing to do until the other side takes some");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_files_are_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("sshremote-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("r.txt"), b"x").unwrap();
        assert_eq!(free_path(&dir, "r.txt"), dir.join("r (2).txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_streams_every_file() {
        let dir = std::env::temp_dir().join(format!("sshremote-out-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), vec![1u8; 100_000]).unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), b"bb").unwrap();
        let mut out = Outbox::default();
        assert!(out.fetch(dir.to_str().unwrap(), DONE_TAG | 7).is_empty());
        let mut names = Vec::new();
        let mut done = 0;
        let mut bytes = 0;
        for _ in 0..10 {
            for m in out.pump(1 << 20) {
                match m {
                    ToClient::FetchStart { name, .. } => names.push(name),
                    ToClient::FetchData { data, .. } => bytes += data.len(),
                    ToClient::FetchDone { tag } => {
                        assert_eq!((tag, names.len()), (DONE_TAG | 7, 2), "told once, after the last file");
                        done += 1;
                    }
                    _ => {}
                }
            }
        }
        names.sort();
        let base = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(names, [format!("{base}/a.txt"), format!("{base}/sub/b.txt")]);
        assert_eq!((bytes, done), (100_002, 1));
        // A path that is not there is told at once, and so is its end.
        assert!(matches!(&out.fetch("/no/such/path", DONE_TAG | 8)[..], [ToClient::FetchEnd { ok: false, .. }, ToClient::FetchDone { tag }] if *tag == DONE_TAG | 8));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
