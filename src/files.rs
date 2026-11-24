//! File transfer, used the same way in both directions: the receiving side
//! writes into its user's Downloads folder (never overwriting a file), the
//! sending side streams files with a bounded amount unacknowledged, and the
//! agent lists folders for the viewer's file panel.

pub use crate::sys::{human, local_time};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::proto::{DirEntry, FileOp, ToClient};

/// Progress is reported about this often.
pub const REPORT: u64 = 256 << 10;
/// Unacknowledged bytes per transfer; more would only queue in ssh and delay the screen.
pub const WINDOW: u64 = 1 << 20;
pub const CHUNK: usize = 64 << 10;

/// The home folder of the person transfers are for, when not this process's own: an agent run as
/// SYSTEM acts for the person signed in at the screen.
static HOME: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg_attr(not(windows), allow(dead_code))]
pub fn set_home(home: Option<PathBuf>) {
    *HOME.lock().unwrap_or_else(|e| e.into_inner()) = home;
}

pub fn home() -> PathBuf {
    if let Some(h) = HOME.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return h;
    }
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

pub fn downloads() -> PathBuf {
    home().join("Downloads")
}

/// A copied file or folder as the viewer is told of it, with the size of all of it (counted up
/// to a limit, past which it is only "large").
pub fn clip_file(path: &Path) -> crate::proto::ClipFile {
    let dir = path.is_dir();
    let size = if dir {
        let mut files = Vec::new();
        walk(path, "", &mut files);
        files.iter().take(20_000).filter_map(|(p, _)| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
    } else {
        std::fs::metadata(path).map_or(0, |m| m.len())
    };
    crate::proto::ClipFile { path: path.to_string_lossy().into_owned(), size, dir }
}

/// Where an upload goes that is to be pasted from the clipboard: `CLIPBOARD` and a batch number
/// in place of the folder.
pub const CLIPBOARD: &str = "\u{0}clipboard/";
/// A file's name as a shell takes it whole once quoted, on either kind of computer: no character
/// that Windows' shells read inside double quotes (`$` and a backquote in PowerShell, `%` and `!`
/// in cmd), no double quote and no control character (a paste's end among them); not one of
/// Windows' device names, and not ending in a dot or a space.
pub fn typable(name: &str) -> String {
    let s: String = name.chars().take(200).map(|c| if c.is_control() || "\"$`%!".contains(c) { '_' } else { c }).collect();
    let mut s = s.trim_end_matches([' ', '.']).trim_start().to_string();
    if s.is_empty() || s == "." || s == ".." {
        s = "file".into();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    if ["con", "prn", "aux", "nul"].contains(&stem.as_str()) || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

/// `name` made free of `taken`: "shot.png", then "shot-2.png" and so on; and taken.
pub fn unique(name: String, taken: &mut std::collections::HashSet<String>) -> String {
    let mut n = 1;
    let mut out = name.clone();
    while !taken.insert(out.to_lowercase()) {
        n += 1;
        out = match name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) {
            Some((stem, ext)) => format!("{stem}-{n}.{ext}"),
            None => format!("{name}-{n}"),
        };
    }
    out
}

/// A batch name no other copy from this program has: the time, the process and a count.
pub fn batch_name() -> String {
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!("{}-{}-{}", crate::sys::now_secs(), std::process::id(), COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Where an upload goes for a terminal: `DROP` and a batch name, into `drop_dir`.
pub const DROP: &str = "\u{0}drop/";

/// The folder of files dropped or pasted on a terminal for one copy, in the user's own cache (as
/// `sshremote`'s ssh copies use too); those over a day old are cleared as a new one is made.
pub fn drop_dir(batch: &str) -> PathBuf {
    let base = if cfg!(windows) { home().join(r"AppData\Local\Temp\sshremote-drop") } else { home().join(".cache/sshremote/drop") };
    // What is dropped (screenshots) is the user's alone.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&base);
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
    }
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 24 * 3600);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    base.join(clean(batch))
}

/// The folder of files copied over for one paste, out of the way in the user's own temporary
/// folder; those of other days are cleared as a new one is made.
pub fn clipboard_dir(batch: &str) -> PathBuf {
    let base = if cfg!(windows) { home().join(r"AppData\Local\Temp\sshremote-clipboard") } else { home().join(".cache/sshremote/clipboard") };
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 24 * 3600);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    base.join(clean(batch))
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
    for n in 1..10_000 {
        let path = nth_name(dir, name, n);
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

/// What arrived of a file whose transfer the connection broke off, kept to go on with.
#[derive(Clone, Debug)]
pub struct Kept {
    pub part: PathBuf,
    pub dir: PathBuf,
    pub name: String,
    pub size: u64,
    pub received: u64,
}

/// The bytes before the point a transfer goes on from that both sides compare first.
const TAIL: u64 = 64 << 10;

/// The checksum of the up to 64 KB of `file` just before `at`: a part kept from a broken transfer
/// is taken up again only where it ends as the file it is the start of does there.
pub fn tail_sum(file: &mut File, at: u64) -> std::io::Result<[u8; 32]> {
    use sha2::Digest;
    let from = at.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0; (at - from) as usize];
    file.read_exact(&mut buf)?;
    Ok(sha2::Sha256::digest(&buf).into())
}

fn tail_of(path: &Path, at: u64) -> Option<[u8; 32]> {
    tail_sum(&mut File::open(path).ok()?, at).ok()
}

/// The `n`th name `create_free` tries for `name`: "name", then "name (2)" and so on.
fn nth_name(dir: &Path, name: &str, n: u32) -> PathBuf {
    let (stem, ext) = stem_ext(name);
    if n == 1 { dir.join(name) } else { dir.join(format!("{stem} ({n}){ext}")) }
}

/// The files in `dir` that `create_free` could have named after `name`, up to a run of names
/// not taken (past which it would not have gone).
fn names_like(dir: &Path, name: &str) -> impl Iterator<Item = PathBuf> {
    let (dir, name) = (dir.to_path_buf(), name.to_string());
    let mut free = 0;
    (1..10_000)
        .map(move |n| nth_name(&dir, &name, n))
        .take_while(move |p| {
            free = if std::fs::symlink_metadata(p).is_ok() { 0 } else { free + 1 };
            free < 50
        })
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
}

/// A list of the parts kept for transfers that may yet go on, so that those that never do are
/// not left for ever.
fn kept_list() -> Option<PathBuf> {
    // Tests keep out of the user's own settings.
    if cfg!(test) {
        return None;
    }
    crate::hosts::config_dir().map(|d| d.join("kept-parts"))
}

/// How long a kept part waits for its transfer to go on.
const KEEP_FOR: std::time::Duration = std::time::Duration::from_secs(3 * 24 * 3600);

fn note_kept(parts: &[PathBuf]) {
    let Some(list) = kept_list().filter(|_| !parts.is_empty()) else { return };
    let text: String = parts.iter().map(|p| format!("{}\n", p.display())).collect();
    // Added to, never written over: sessions ending at once each keep theirs.
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&list).and_then(|mut f| f.write_all(text.as_bytes()));
}

/// Deletes the kept parts left alone for longer than `KEEP_FOR`, once a run.
fn prune_kept() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(list) = kept_list() {
            prune(&list);
        }
    });
}

fn prune(list: &Path) {
    let Ok(text) = std::fs::read_to_string(list) else { return };
    let mut left = String::new();
    for line in text.lines().filter(|l| l.ends_with(PART)) {
        let Ok(m) = std::fs::metadata(line) else { continue };
        if m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > KEEP_FOR) {
            let _ = std::fs::remove_file(line);
        } else if !left.lines().any(|l| l == line) {
            left.push_str(line);
            left.push('\n');
        }
    }
    let _ = std::fs::write(list, left);
}

/// Receiving side, writing under the local Downloads folder. A file is written as "name.part"
/// and takes its name once whole, so a broken transfer never looks finished; one still open when
/// the inbox goes (the connection ended) is deleted, unless `keep_broken` (the viewer goes on
/// with it when it connects again).
#[derive(Default)]
pub struct Inbox {
    open: HashMap<u32, Incoming>,
    pub keep_broken: bool,
}

impl Drop for Inbox {
    fn drop(&mut self) {
        if self.keep_broken {
            let (kept, empty): (Vec<Kept>, Vec<Kept>) = self.keep_all().into_iter().map(|(_, k)| k).partition(|k| k.received > 0);
            empty.iter().for_each(Kept::discard);
            note_kept(&kept.into_iter().map(|k| k.part).collect::<Vec<_>>());
        }
        for (_, u) in self.open.drain() {
            u.discard();
        }
    }
}

impl Kept {
    /// Deletes what arrived.
    pub fn discard(&self) {
        let _ = std::fs::remove_file(&self.part);
    }
}

impl Inbox {
    /// The folder a file `name` (perhaps "sub/file") goes into under `dir`, made, and its name.
    fn place(&self, dir: &Path, name: &str) -> Result<(PathBuf, String), String> {
        if self.open.len() >= MAX_OPEN {
            return Err("too many transfers at once".into());
        }
        if name.len() > MAX_NAME || name.split(['/', '\\']).count() > MAX_DEPTH {
            return Err("the name is too long".into());
        }
        let mut parts = clean_relative(name);
        let leaf = parts.pop().unwrap_or_else(|| "file".into());
        let mut dir = dir.to_path_buf();
        dir.extend(&parts);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok((dir, leaf))
    }

    /// Receives file `id` of `size` bytes named `name` (perhaps "sub/file") under `dir`.
    pub fn start_in(&mut self, id: u32, size: u64, dir: &Path, name: &str) -> Option<Note> {
        let (dir, leaf) = match self.place(dir, name) {
            Ok(p) => p,
            Err(text) => return Some(Note::Done { id, ok: false, text }),
        };
        if self.keep_broken {
            prune_kept();
        }
        self.open_new(id, size, dir, leaf).err()
    }

    /// Receives `id` into a new "leaf.part" in `dir`.
    fn open_new(&mut self, id: u32, size: u64, dir: PathBuf, leaf: String) -> Result<(), Note> {
        let (file, part) = create_free(&dir, &format!("{leaf}{PART}")).map_err(|e| Note::Done { id, ok: false, text: e.to_string() })?;
        self.open.insert(id, Incoming { file, part, dir, name: leaf, size, received: 0, reported: 0 });
        Ok(())
    }

    /// An upload the connection broke off, sent again as `id` from `from` bytes: goes on in the
    /// part kept of it under `dir` (one as long at least, ending there as `tail` says), and
    /// answers where it goes on from; 0 when no part fits, in a new one. One that the connection
    /// broke as it ended, already in place whole, is done.
    pub fn resume_in(&mut self, id: u32, size: u64, dir: &Path, name: &str, from: u64, tail: [u8; 32]) -> Result<u64, Note> {
        let (dir, leaf) = self.place(dir, name).map_err(|text| Note::Done { id, ok: false, text })?;
        prune_kept();
        let fits = |p: &Path, len_ok: &dyn Fn(u64) -> bool| {
            std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && len_ok(m.len())) && tail_of(p, from) == Some(tail)
        };
        if from > 0 && from <= size {
            let busy: Vec<&PathBuf> = self.open.values().map(|u| &u.part).collect();
            let part = names_like(&dir, &format!("{leaf}{PART}")).find(|p| !busy.contains(&p) && fits(p, &|len| len >= from));
            if let Some(part) = part
                && let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&part)
                && file.set_len(from).is_ok()
                && file.seek(SeekFrom::Start(from)).is_ok()
            {
                self.open.insert(id, Incoming { file, part, dir, name: leaf, size, received: from, reported: from });
                return Ok(from);
            }
            // All of it had been sent: the connection broke before the answer.
            if from == size
                && let Some(whole) = names_like(&dir, &leaf).find(|p| fits(p, &|len| len == size))
            {
                return Err(Note::Done { id, ok: true, text: whole.display().to_string() });
            }
        }
        self.open_new(id, size, dir, leaf).map(|_| 0)
    }

    /// Takes up a part this side kept as download `id`, going on at `from` (no further than it
    /// got); false when it cannot be.
    pub fn take_up(&mut self, id: u32, k: &Kept, from: u64) -> bool {
        if from > k.received || from > k.size || self.open.len() >= MAX_OPEN {
            return false;
        }
        let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&k.part) else { return false };
        if file.set_len(from).is_err() || file.seek(SeekFrom::Start(from)).is_err() {
            return false;
        }
        let u = Incoming { file, part: k.part.clone(), dir: k.dir.clone(), name: k.name.clone(), size: k.size, received: from, reported: from };
        self.open.insert(id, u);
        true
    }

    /// Every file being received, let go of with what arrived of it kept.
    pub fn keep_all(&mut self) -> Vec<(u32, Kept)> {
        self.open
            .drain()
            .map(|(id, u)| {
                let Incoming { part, dir, name, size, received, .. } = u;
                (id, Kept { part, dir, name, size, received })
            })
            .collect()
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
    /// Requests fetched again after a broken connection (`fetch_from`), by tag.
    again: HashMap<u32, Again>,
}

/// Where a request fetched again goes on: the file it had come to, in parts ("folder/sub/file"),
/// what of it arrived (`u64::MAX`: all), and the checksum of what arrived last.
struct Again {
    name: Vec<String>,
    from: u64,
    tail: [u8; 32],
}

fn parts(name: &str) -> Vec<String> {
    name.split('/').map(str::to_string).collect()
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

    /// Fetches a request again that a broken connection cut off at file `name`, `from` bytes of
    /// which arrived (`u64::MAX`: all of it): the files sent before it are left out, as is it
    /// when whole; else it goes on from there if it still ends there as `tail` says.
    pub fn fetch_from(&mut self, path: &str, tag: u32, name: &str, from: u64, tail: [u8; 32]) -> Vec<ToClient> {
        self.again.insert(tag, Again { name: parts(name), from, tail });
        self.fetch(path, tag)
    }

    /// Whether the file or folder `name` of request `tag` comes before where it is fetched again
    /// from: files go in the order of their names' parts, a folder's before what follows it.
    fn sent_before(&self, tag: u32, name: &str, folder: bool) -> bool {
        let Some(a) = self.again.get(&tag) else { return false };
        let p = parts(name);
        if folder {
            // A folder holding the file is opened; one wholly before it is passed over.
            return !a.name.starts_with(&p) && p < a.name;
        }
        p < a.name || (p == a.name && a.from == u64::MAX)
    }

    /// Says a request is complete once nothing of it is queued, if it asked to be told.
    fn done_if_last(&mut self, tag: u32, out: &mut Vec<ToClient>) {
        if !self.queue.iter().any(|q| q.tag() == tag) {
            self.again.remove(&tag);
            if tag & DONE_TAG != 0 {
                out.push(ToClient::FetchDone { tag });
            }
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
        if tag != 0 {
            self.again.remove(&tag);
        }
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

    /// Whether the next step opens a file or a folder (rather than reading one already open).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn opens_next(&self) -> bool {
        match self.queue.front() {
            Some(Queued::File(o)) => o.file.is_none(),
            Some(Queued::Folder { .. }) => true,
            None => false,
        }
    }

    /// Drops every download, saying so for those under way and for requests that asked to be told.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn cancel_all(&mut self, why: &str) -> Vec<ToClient> {
        let mut out = Vec::new();
        self.again.clear();
        let mut tags = Vec::new();
        for q in self.queue.drain(..) {
            if let Queued::File(o) = &q
                && o.file.is_some()
            {
                out.push(ToClient::FetchEnd { id: o.id, ok: false, text: why.to_string() });
            }
            let tag = q.tag();
            if tag & DONE_TAG != 0 && !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        out.extend(tags.into_iter().map(|tag| ToClient::FetchDone { tag }));
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
                    if !self.sent_before(tag, &name, true) {
                        self.open_folder(&path, &name, tag);
                    }
                    // An empty folder completes its request at once.
                    self.done_if_last(tag, &mut out);
                    continue;
                }
                // Fetched again: what was sent before is left out, and the first file sent ends
                // what is left out.
                Some(Queued::File(o)) if o.file.is_none() && self.again.contains_key(&o.tag) => {
                    let (tag, name) = (o.tag, o.name.clone());
                    if self.sent_before(tag, &name, false) {
                        self.pop(&mut out);
                        continue;
                    }
                    let a = self.again.remove(&tag).filter(|a| a.name == parts(&name));
                    let Some(Queued::File(o)) = self.queue.front_mut() else { unreachable!() };
                    (o, a)
                }
                Some(Queued::File(o)) => (o, None),
            };
            let (o, again) = o;
            if o.file.is_none() {
                match File::open(&o.path) {
                    Ok(mut f) => {
                        o.size = f.metadata().map(|m| m.len()).unwrap_or(0);
                        // The file it stopped at goes on from there, if it still is as it was.
                        let from = again.as_ref().filter(|a| a.from <= o.size && tail_sum(&mut f, a.from).is_ok_and(|t| t == a.tail)).map_or(0, |a| a.from);
                        let at = f.seek(SeekFrom::Start(from));
                        (o.sent, o.acked, o.file) = (from, from, Some(f));
                        let (id, size, tag, name) = (o.id, o.size, o.tag, o.name.clone());
                        out.push(if again.is_some() { ToClient::FetchAt { id, size, tag, from, name } } else { ToClient::FetchStart { id, size, tag, name } });
                        if at.is_err() {
                            out.push(ToClient::FetchEnd { id, ok: false, text: format!("{}: cannot be read", o.path.display()) });
                            self.pop(&mut out);
                            continue;
                        }
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

/// The separator of a remote folder's path: `\` on Windows, `/` elsewhere.
pub fn path_sep(path: &str) -> char {
    if path.contains('\\') && !path.contains('/') { '\\' } else { '/' }
}

/// Entry `name` in the remote folder `path` (a drive, in the list of drives).
pub fn path_child(path: &str, name: &str) -> String {
    if path == DRIVES {
        return name.to_string();
    }
    let sep = path_sep(path);
    if path.ends_with(sep) { format!("{path}{name}") } else { format!("{path}{sep}{name}") }
}

/// The folder above the remote folder `path`: above a drive's root, the list of drives.
pub fn path_parent(path: &str) -> String {
    let sep = path_sep(path);
    let trimmed = path.trim_end_matches(sep);
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return DRIVES.into();
    }
    match trimmed.rfind(sep) {
        Some(0) => sep.to_string(),
        Some(i) if trimmed[..i].ends_with(':') => format!("{}{sep}", &trimmed[..i]),
        Some(i) => trimmed[..i].to_string(),
        None => path.to_string(),
    }
}

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

    fn keeping() -> Inbox {
        let mut inbox = Inbox::default();
        inbox.keep_broken = true;
        inbox
    }

    fn bytes(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3] ^ seed).collect()
    }

    fn sum(data: &[u8], at: usize) -> [u8; 32] {
        use sha2::Digest;
        sha2::Sha256::digest(&data[at.saturating_sub(TAIL as usize)..at]).into()
    }

    #[test]
    fn a_broken_upload_goes_on_from_what_was_kept() {
        let dir = std::env::temp_dir().join(format!("sshremote-resume-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = bytes(700_000, 1);
        let mut inbox = keeping();
        assert!(inbox.start_in(1, data.len() as u64, &dir, "sub/big.bin").is_none());
        for c in data[..300_000].chunks(CHUNK) {
            inbox.data(1, c);
        }
        // The connection breaks: what arrived stays, under its part's name.
        drop(inbox);
        let part = dir.join("sub").join("big.bin.part");
        assert_eq!(std::fs::metadata(&part).unwrap().len(), 300_000);
        // A part that does not end as the file does is not taken up: it starts again, anew.
        let mut inbox = keeping();
        assert_eq!(inbox.resume_in(2, data.len() as u64, &dir, "sub/big.bin", 200_000, sum(&bytes(700_000, 2), 200_000)).ok(), Some(0));
        inbox.end(2, false, "called off");
        // The viewer had word of 200 000 bytes: on from there, the rest after them.
        assert_eq!(inbox.resume_in(3, data.len() as u64, &dir, "sub/big.bin", 200_000, sum(&data, 200_000)).ok(), Some(200_000));
        assert_eq!(inbox.received(3), Some((200_000, 700_000)));
        for c in data[200_000..].chunks(CHUNK) {
            inbox.data(3, c);
        }
        let Some(Note::Done { ok: true, text, .. }) = inbox.end(3, true, "") else { panic!("not whole") };
        assert_eq!(std::fs::read(&text).unwrap(), data);
        assert!(!part.exists());
        // Broken as it ended: the file whole in place already is the answer.
        assert!(matches!(inbox.resume_in(4, data.len() as u64, &dir, "sub/big.bin", 700_000, sum(&data, 700_000)), Err(Note::Done { ok: true, text: t, .. }) if t == text));
        // Not before the end, though: a file of that size there could be an older one.
        assert_eq!(inbox.resume_in(7, data.len() as u64, &dir, "sub/big.bin", 200_000, sum(&data, 200_000)).ok(), Some(0));
        inbox.end(7, false, "called off");
        // Not kept unless the viewer goes on with it, nor when nothing arrived.
        let mut inbox = Inbox::default();
        inbox.start_in(5, 10, &dir, "gone.bin");
        inbox.data(5, b"abc");
        drop(inbox);
        let mut inbox = keeping();
        inbox.start_in(6, 10, &dir, "empty.bin");
        drop(inbox);
        assert!(!dir.join("gone.bin.part").exists() && !dir.join("empty.bin.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_kept_here_is_taken_up_again() {
        let dir = std::env::temp_dir().join(format!("sshremote-resume-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = bytes(500_000, 3);
        let mut inbox = Inbox::default();
        inbox.start_in(1, data.len() as u64, &dir, "d.bin");
        inbox.data(1, &data[..123_456]);
        let kept = inbox.keep_all();
        let [(1, k)] = &kept[..] else { panic!("{kept:?}") };
        assert_eq!((k.received, k.size), (123_456, 500_000));
        assert_eq!(tail_sum(&mut File::open(&k.part).unwrap(), k.received).unwrap(), sum(&data, 123_456));
        // Further than what arrived it cannot go on from.
        assert!(!inbox.take_up(2, k, 200_000));
        assert!(inbox.take_up(2, k, 100_000));
        inbox.data(2, &data[100_000..]);
        let Some(Note::Done { ok: true, text, .. }) = inbox.end(2, true, "") else { panic!("not whole") };
        assert_eq!(std::fs::read(text).unwrap(), data);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_fetched_again_leaves_out_what_arrived() {
        let dir = std::env::temp_dir().join(format!("sshremote-resume-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("c")).unwrap();
        std::fs::write(dir.join("a").join("1.bin"), b"one").unwrap();
        let big = bytes(400_000, 4);
        std::fs::write(dir.join("b.bin"), &big).unwrap();
        std::fs::write(dir.join("c").join("2.bin"), b"two").unwrap();
        let base = dir.file_name().unwrap().to_string_lossy().into_owned();
        let run = |out: &mut Outbox| {
            let mut got: Vec<(String, u64, usize)> = Vec::new();
            for _ in 0..20 {
                for m in out.pump(1 << 20) {
                    match m {
                        ToClient::FetchStart { name, .. } => got.push((name, 0, 0)),
                        ToClient::FetchAt { name, from, .. } => got.push((name, from, 0)),
                        ToClient::FetchData { data, .. } => got.last_mut().unwrap().2 += data.len(),
                        ToClient::FetchDone { .. } => return got,
                        _ => {}
                    }
                }
            }
            panic!("never done: {got:?}");
        };
        // Cut off 150 000 bytes into b.bin: a/1.bin is left out, b.bin goes on, c/2.bin follows.
        let mut out = Outbox::default();
        out.fetch_from(dir.to_str().unwrap(), DONE_TAG | 1, &format!("{base}/b.bin"), 150_000, sum(&big, 150_000));
        let b = format!("{base}/b.bin");
        let c = format!("{base}/c/2.bin");
        assert_eq!(run(&mut out), [(b.clone(), 150_000, 250_000), (c.clone(), 0, 3)]);
        // Changed since: b.bin comes whole.
        out.fetch_from(dir.to_str().unwrap(), DONE_TAG | 2, &b, 150_000, sum(&bytes(400_000, 5), 150_000));
        assert_eq!(run(&mut out), [(b.clone(), 0, 400_000), (c.clone(), 0, 3)]);
        // Cut off right after b.bin arrived whole.
        out.fetch_from(dir.to_str().unwrap(), DONE_TAG | 3, &b, u64::MAX, [0; 32]);
        assert_eq!(run(&mut out), [(c.clone(), 0, 3)]);
        // Before anything came, all of it.
        out.fetch_from(dir.to_str().unwrap(), DONE_TAG | 4, "", 0, [0; 32]);
        assert_eq!(run(&mut out).len(), 3);
        // A single file, once more from where it was.
        out.fetch_from(dir.join("b.bin").to_str().unwrap(), DONE_TAG | 5, "b.bin", 399_999, sum(&big, 399_999));
        assert_eq!(run(&mut out), [("b.bin".to_string(), 399_999, 1)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn kept_parts_left_alone_go_after_a_while() {
        let dir = std::env::temp_dir().join(format!("sshremote-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (old, new, list) = (dir.join("old.bin.part"), dir.join("new.bin.part"), dir.join("kept-parts"));
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&new, b"y").unwrap();
        File::options().write(true).open(&old).unwrap().set_modified(std::time::SystemTime::now() - KEEP_FOR * 2).unwrap();
        std::fs::write(&list, format!("{}\n{}\n{}\n/not/there.part\n", old.display(), new.display(), new.display())).unwrap();
        prune(&list);
        assert!(!old.exists() && new.exists());
        assert_eq!(std::fs::read_to_string(&list).unwrap(), format!("{}\n", new.display()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
