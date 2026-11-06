//! Wire messages. Client to agent is plain framing; agent to client is the same
//! framing inside one long-lived zstd stream that is flushed after each message.

use std::io::{self, Read, Write};

use crate::color::Mode;
use crate::scroll::Move;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub view_w: u16,
    pub view_h: u16,
    pub scale: u8,
    pub mode: Mode,
    pub fps: u8,
}

pub struct Preset {
    pub name: &'static str,
    pub scale: u8,
    pub mode: Mode,
    pub fps: u8,
}

pub const PRESETS: [Preset; 5] = [
    Preset { name: "glance", scale: 50, mode: Mode::Mono, fps: 5 },
    Preset { name: "lean", scale: 75, mode: Mode::Gray16, fps: 10 },
    Preset { name: "balanced", scale: 100, mode: Mode::Color256, fps: 20 },
    Preset { name: "crisp", scale: 100, mode: Mode::True, fps: 30 },
    Preset { name: "exact", scale: 100, mode: Mode::True, fps: 60 },
];

pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_SUPER: u8 = 8;
/// X11 keysym of the left Win (Super) key.
pub const SUPER_L: u32 = 0xffeb;

#[derive(Debug, PartialEq)]
pub enum ToAgent {
    Settings(Settings),
    Ack(u32),
    Key {
        keysym: u32,
        mods: u8,
    },
    Pointer {
        x: u16,
        y: u16,
    },
    Button {
        button: u8,
        pressed: bool,
    },
    Scroll {
        steps: i8,
    },
    Refresh,
    /// Echoed back as `Pong` behind any queued frames, so its round trip shows queueing delay.
    Ping(u32),
    /// One key going down or up, for viewers that see both (held modifiers, games).
    KeyState {
        keysym: u32,
        down: bool,
    },
    /// Horizontal wheel steps, positive to the right.
    HScroll {
        steps: i8,
    },
    /// Text now on the viewer's clipboard.
    Clipboard(String),
    /// Ctrl+Alt+Del, which no viewer can capture locally.
    SecureAttention,
    /// A file upload begins; it lands in the remote user's Downloads folder.
    FileStart {
        id: u32,
        size: u64,
        name: String,
    },
    FileData {
        id: u32,
        data: Vec<u8>,
    },
    FileEnd {
        id: u32,
    },
    /// Lists a remote folder; an empty path is the remote user's home.
    ListDir {
        path: String,
    },
    /// Sends a remote file, or every file under a remote folder, to the viewer.
    Fetch {
        path: String,
    },
    /// Bytes of a download the viewer has written, so the agent can send more.
    FetchAck {
        id: u32,
        received: u64,
    },
    /// Shows another of the remote host's monitors (an index into `Monitors`).
    SelectMonitor(u8),
    /// Starts or stops sending the host's sound.
    Audio(bool),
    /// A chat message for the person at the remote screen.
    Chat(String),
    /// Stops or resumes the picture; while paused, `Refresh` sends exactly one frame.
    Pause(bool),
    /// Asks for the current screen as a full-colour PNG.
    Snapshot,
    /// An upload into a chosen remote folder; `name` may be relative ("folder/file.txt").
    FileStartIn {
        id: u32,
        size: u64,
        dir: String,
        name: String,
    },
    /// Changes the remote file system, answered with `OpDone`.
    FileOp {
        op: FileOp,
        path: String,
        arg: String,
    },
    /// Like `Fetch`, but each file is announced with `FetchBegin` carrying `tag`.
    FetchTagged {
        tag: u32,
        path: String,
    },
    /// Stops a download; the rest of its folder too, if `tag` is not 0.
    FetchCancel {
        id: u32,
        tag: u32,
    },
    /// Starts a shell on the remote host in a terminal of this size.
    TermOpen {
        id: u32,
        cols: u16,
        rows: u16,
    },
    /// Keystrokes and pasted text for a shell.
    TermInput {
        id: u32,
        data: Vec<u8>,
    },
    TermResize {
        id: u32,
        cols: u16,
        rows: u16,
    },
    /// Ends a shell.
    TermClose {
        id: u32,
    },
    /// Asks for the windows that can be shared on their own.
    ListWindows,
    /// Shares the screen, a window or a program (see `capture::Share`).
    Share {
        kind: u8,
        id: u64,
    },
    /// Installs what the window list needs on the host (the GNOME Shell helper).
    InstallHelper,
    /// Opens the desktop's own sharing dialog on the remote screen to choose there.
    PickOnRemote,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum FileOp {
    /// Creates the folder `path`.
    Mkdir = 1,
    /// Renames `path` to `arg`, a new name in the same folder.
    Rename = 2,
    /// Deletes `path`, with everything in it if it is a folder.
    Delete = 3,
}

#[derive(Debug, PartialEq, Clone)]
pub struct MonitorInfo {
    pub name: String,
    pub w: u16,
    pub h: u16,
    pub primary: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub struct DirEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Last modification, seconds since 1970; 0 when unknown.
    pub mtime: u64,
}

#[derive(Debug, PartialEq)]
pub struct Tile {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub struct Frame {
    pub id: u32,
    pub w: u16,
    pub h: u16,
    pub mode: Mode,
    /// Applied before the tiles: areas the client already has, shifted vertically.
    pub moves: Vec<Move>,
    pub tiles: Vec<Tile>,
    /// One H.264 access unit for the video region, which no tile covers.
    pub video: Option<Tile>,
}

impl Frame {
    /// Pixels carried by the frame's tiles.
    pub fn covered(&self) -> usize {
        self.tiles.iter().map(|t| t.w as usize * t.h as usize).sum()
    }

    /// Every rectangle whose pixels this frame changes, as (x, y, w, h).
    pub fn damage(&self) -> impl Iterator<Item = (usize, usize, usize, usize)> + '_ {
        let moved = self.moves.iter().map(|m| (m.x, m.y, m.w, m.h));
        let rect = |t: &Tile| (t.x as usize, t.y as usize, t.w as usize, t.h as usize);
        moved.chain(self.tiles.iter().map(rect)).chain(self.video.iter().map(rect))
    }
}

#[derive(Debug, PartialEq)]
pub enum ToClient {
    Hello {
        w: u16,
        h: u16,
        backend: String,
    },
    Frame(Frame),
    Error(String),
    Pong(u32),
    /// Text now on the remote clipboard.
    Clipboard(String),
    /// Bytes of an upload written so far.
    FileProgress {
        id: u32,
        received: u64,
    },
    /// An upload finished: where it was saved, or why it failed.
    FileDone {
        id: u32,
        ok: bool,
        text: String,
    },
    /// A remote folder: its full path and entries, or why it could not be read.
    DirList {
        path: String,
        entries: Vec<DirEntry>,
        error: String,
    },
    /// A download begins; `name` is relative ("folder/sub/file.txt" for a folder).
    FetchStart {
        id: u32,
        size: u64,
        name: String,
    },
    FetchData {
        id: u32,
        data: Vec<u8>,
    },
    /// A download finished, or failed with `text`.
    FetchEnd {
        id: u32,
        ok: bool,
        text: String,
    },
    /// The remote host's monitors and which one is shown.
    Monitors {
        list: Vec<MonitorInfo>,
        current: u8,
    },
    /// One 20 ms Opus packet of the host's sound.
    Audio(Vec<u8>),
    /// Something the viewer should show without ending the session.
    Notice(String),
    /// A chat message typed at the remote screen.
    Chat(String),
    /// The screen at the current capture size as a PNG.
    Snapshot(Vec<u8>),
    /// The result of a `FileOp`.
    OpDone {
        ok: bool,
        text: String,
    },
    /// A download of a `FetchTagged` request begins.
    FetchBegin {
        id: u32,
        size: u64,
        tag: u32,
        name: String,
    },
    /// What a shell wrote to its terminal.
    TermOutput {
        id: u32,
        data: Vec<u8>,
    },
    /// A shell ended, or could not start: why.
    TermExit {
        id: u32,
        text: String,
    },
    /// The shareable windows, or why there is no list.
    WindowList {
        list: Vec<crate::capture::WindowInfo>,
        error: String,
    },
    /// What is shared now; `kind` as in `Share`.
    Sharing {
        kind: u8,
        text: String,
    },
}

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed message")
}

struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let s = self.d.get(self.p..self.p + n).ok_or_else(bad)?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// A string prefixed by its u32 length.
    fn str(&mut self) -> io::Result<String> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn mode(&mut self) -> io::Result<Mode> {
        Mode::from_u8(self.u8()?).ok_or_else(bad)
    }
    fn rest(&mut self) -> &'a [u8] {
        let s = &self.d[self.p..];
        self.p = self.d.len();
        s
    }
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

pub fn write_msg(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = [kind, 0, 0, 0, 0];
    head[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&head)?;
    w.write_all(payload)
}

/// Returns `None` on a clean end of stream.
pub fn read_msg(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        r => r?,
    }
    let len = u32::from_le_bytes(head[1..].try_into().unwrap()) as usize;
    if len > 256 << 20 {
        return Err(bad());
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some((head[0], payload)))
}

impl ToAgent {
    pub fn encode(&self) -> (u8, Vec<u8>) {
        let mut b = Vec::new();
        let kind = match self {
            ToAgent::Settings(s) => {
                b.extend_from_slice(&s.view_w.to_le_bytes());
                b.extend_from_slice(&s.view_h.to_le_bytes());
                b.extend_from_slice(&[s.scale, s.mode.to_u8(), s.fps]);
                1
            }
            ToAgent::Ack(id) => {
                b.extend_from_slice(&id.to_le_bytes());
                2
            }
            ToAgent::Key { keysym, mods } => {
                b.extend_from_slice(&keysym.to_le_bytes());
                b.push(*mods);
                3
            }
            ToAgent::Pointer { x, y } => {
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
                4
            }
            ToAgent::Button { button, pressed } => {
                b.extend_from_slice(&[*button, *pressed as u8]);
                5
            }
            ToAgent::Scroll { steps } => {
                b.push(*steps as u8);
                6
            }
            ToAgent::Refresh => 7,
            ToAgent::Ping(t) => {
                b.extend_from_slice(&t.to_le_bytes());
                8
            }
            ToAgent::KeyState { keysym, down } => {
                b.extend_from_slice(&keysym.to_le_bytes());
                b.push(*down as u8);
                9
            }
            ToAgent::HScroll { steps } => {
                b.push(*steps as u8);
                10
            }
            ToAgent::Clipboard(t) => {
                b.extend_from_slice(t.as_bytes());
                11
            }
            ToAgent::SecureAttention => 12,
            ToAgent::FileStart { id, size, name } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                b.extend_from_slice(name.as_bytes());
                13
            }
            ToAgent::FileData { id, data } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(data);
                14
            }
            ToAgent::FileEnd { id } => {
                b.extend_from_slice(&id.to_le_bytes());
                15
            }
            ToAgent::ListDir { path } => {
                b.extend_from_slice(path.as_bytes());
                16
            }
            ToAgent::Fetch { path } => {
                b.extend_from_slice(path.as_bytes());
                17
            }
            ToAgent::FetchAck { id, received } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&received.to_le_bytes());
                18
            }
            ToAgent::SelectMonitor(i) => {
                b.push(*i);
                19
            }
            ToAgent::Audio(on) => {
                b.push(*on as u8);
                20
            }
            ToAgent::Chat(t) => {
                b.extend_from_slice(t.as_bytes());
                21
            }
            ToAgent::Pause(on) => {
                b.push(*on as u8);
                22
            }
            ToAgent::Snapshot => 23,
            ToAgent::FileStartIn { id, size, dir, name } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                put_str(&mut b, dir);
                put_str(&mut b, name);
                24
            }
            ToAgent::FileOp { op, path, arg } => {
                b.push(*op as u8);
                put_str(&mut b, path);
                put_str(&mut b, arg);
                25
            }
            ToAgent::FetchTagged { tag, path } => {
                b.extend_from_slice(&tag.to_le_bytes());
                b.extend_from_slice(path.as_bytes());
                26
            }
            ToAgent::FetchCancel { id, tag } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&tag.to_le_bytes());
                27
            }
            ToAgent::TermOpen { id, cols, rows } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&cols.to_le_bytes());
                b.extend_from_slice(&rows.to_le_bytes());
                28
            }
            ToAgent::TermInput { id, data } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(data);
                29
            }
            ToAgent::TermResize { id, cols, rows } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&cols.to_le_bytes());
                b.extend_from_slice(&rows.to_le_bytes());
                30
            }
            ToAgent::TermClose { id } => {
                b.extend_from_slice(&id.to_le_bytes());
                31
            }
            ToAgent::ListWindows => 32,
            ToAgent::InstallHelper => 34,
            ToAgent::PickOnRemote => 35,
            ToAgent::Share { kind, id } => {
                b.push(*kind);
                b.extend_from_slice(&id.to_le_bytes());
                33
            }
        };
        (kind, b)
    }

    pub fn decode(kind: u8, d: &[u8]) -> io::Result<ToAgent> {
        let mut c = Cur { d, p: 0 };
        Ok(match kind {
            1 => ToAgent::Settings(Settings {
                view_w: c.u16()?,
                view_h: c.u16()?,
                scale: c.u8()?,
                mode: c.mode()?,
                fps: c.u8()?,
            }),
            2 => ToAgent::Ack(c.u32()?),
            3 => ToAgent::Key { keysym: c.u32()?, mods: c.u8()? },
            4 => ToAgent::Pointer { x: c.u16()?, y: c.u16()? },
            5 => ToAgent::Button { button: c.u8()?, pressed: c.u8()? != 0 },
            6 => ToAgent::Scroll { steps: c.u8()? as i8 },
            7 => ToAgent::Refresh,
            8 => ToAgent::Ping(c.u32()?),
            9 => ToAgent::KeyState { keysym: c.u32()?, down: c.u8()? != 0 },
            10 => ToAgent::HScroll { steps: c.u8()? as i8 },
            11 => ToAgent::Clipboard(String::from_utf8_lossy(c.rest()).into_owned()),
            12 => ToAgent::SecureAttention,
            13 => ToAgent::FileStart {
                id: c.u32()?,
                size: u64::from_le_bytes(c.take(8)?.try_into().unwrap()),
                name: String::from_utf8_lossy(c.rest()).into_owned(),
            },
            14 => ToAgent::FileData { id: c.u32()?, data: c.rest().to_vec() },
            15 => ToAgent::FileEnd { id: c.u32()? },
            16 => ToAgent::ListDir { path: String::from_utf8_lossy(c.rest()).into_owned() },
            17 => ToAgent::Fetch { path: String::from_utf8_lossy(c.rest()).into_owned() },
            18 => ToAgent::FetchAck { id: c.u32()?, received: c.u64()? },
            19 => ToAgent::SelectMonitor(c.u8()?),
            20 => ToAgent::Audio(c.u8()? != 0),
            21 => ToAgent::Chat(String::from_utf8_lossy(c.rest()).into_owned()),
            22 => ToAgent::Pause(c.u8()? != 0),
            23 => ToAgent::Snapshot,
            24 => ToAgent::FileStartIn { id: c.u32()?, size: c.u64()?, dir: c.str()?, name: c.str()? },
            25 => ToAgent::FileOp {
                op: match c.u8()? {
                    1 => FileOp::Mkdir,
                    2 => FileOp::Rename,
                    3 => FileOp::Delete,
                    _ => return Err(bad()),
                },
                path: c.str()?,
                arg: c.str()?,
            },
            26 => ToAgent::FetchTagged { tag: c.u32()?, path: String::from_utf8_lossy(c.rest()).into_owned() },
            27 => ToAgent::FetchCancel { id: c.u32()?, tag: c.u32()? },
            28 => ToAgent::TermOpen { id: c.u32()?, cols: c.u16()?, rows: c.u16()? },
            29 => ToAgent::TermInput { id: c.u32()?, data: c.rest().to_vec() },
            30 => ToAgent::TermResize { id: c.u32()?, cols: c.u16()?, rows: c.u16()? },
            31 => ToAgent::TermClose { id: c.u32()? },
            32 => ToAgent::ListWindows,
            34 => ToAgent::InstallHelper,
            35 => ToAgent::PickOnRemote,
            33 => ToAgent::Share { kind: c.u8()?, id: c.u64()? },
            _ => return Err(bad()),
        })
    }
}

impl ToClient {
    pub fn encode(&self) -> (u8, Vec<u8>) {
        let mut b = Vec::new();
        let kind = match self {
            ToClient::Hello { w, h, backend } => {
                b.extend_from_slice(&w.to_le_bytes());
                b.extend_from_slice(&h.to_le_bytes());
                b.extend_from_slice(backend.as_bytes());
                101
            }
            ToClient::Frame(f) => {
                b.extend_from_slice(&f.id.to_le_bytes());
                b.extend_from_slice(&f.w.to_le_bytes());
                b.extend_from_slice(&f.h.to_le_bytes());
                b.push(f.mode.to_u8());
                b.extend_from_slice(&(f.moves.len() as u16).to_le_bytes());
                for m in &f.moves {
                    for v in [m.x as u16, m.y as u16, m.w as u16, m.h as u16, m.dy as i16 as u16] {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                }
                b.extend_from_slice(&(f.tiles.len() as u32).to_le_bytes());
                for t in &f.tiles {
                    for v in [t.x, t.y, t.w, t.h] {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                    b.extend_from_slice(&(t.data.len() as u32).to_le_bytes());
                    b.extend_from_slice(&t.data);
                }
                if let Some(t) = &f.video {
                    for v in [t.x, t.y, t.w, t.h] {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                    b.extend_from_slice(&(t.data.len() as u32).to_le_bytes());
                    b.extend_from_slice(&t.data);
                }
                102
            }
            ToClient::Error(e) => {
                b.extend_from_slice(e.as_bytes());
                103
            }
            ToClient::Pong(t) => {
                b.extend_from_slice(&t.to_le_bytes());
                104
            }
            ToClient::Clipboard(t) => {
                b.extend_from_slice(t.as_bytes());
                105
            }
            ToClient::FileProgress { id, received } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&received.to_le_bytes());
                106
            }
            ToClient::DirList { path, entries, error } => {
                put_str(&mut b, path);
                put_str(&mut b, error);
                b.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                for e in entries {
                    put_str(&mut b, &e.name);
                    b.push(e.dir as u8);
                    b.extend_from_slice(&e.size.to_le_bytes());
                }
                // Appended after the entries, so older viewers still read the list.
                for e in entries {
                    b.extend_from_slice(&e.mtime.to_le_bytes());
                }
                108
            }
            ToClient::FetchStart { id, size, name } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                b.extend_from_slice(name.as_bytes());
                109
            }
            ToClient::FetchData { id, data } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(data);
                110
            }
            ToClient::FetchEnd { id, ok, text } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.push(*ok as u8);
                b.extend_from_slice(text.as_bytes());
                111
            }
            ToClient::Audio(p) => {
                b.extend_from_slice(p);
                113
            }
            ToClient::Notice(t) => {
                b.extend_from_slice(t.as_bytes());
                114
            }
            ToClient::Chat(t) => {
                b.extend_from_slice(t.as_bytes());
                115
            }
            ToClient::Snapshot(p) => {
                b.extend_from_slice(p);
                116
            }
            ToClient::OpDone { ok, text } => {
                b.push(*ok as u8);
                b.extend_from_slice(text.as_bytes());
                117
            }
            ToClient::FetchBegin { id, size, tag, name } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                b.extend_from_slice(&tag.to_le_bytes());
                b.extend_from_slice(name.as_bytes());
                118
            }
            ToClient::TermOutput { id, data } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(data);
                119
            }
            ToClient::TermExit { id, text } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(text.as_bytes());
                120
            }
            ToClient::WindowList { list, error } => {
                put_str(&mut b, error);
                b.extend_from_slice(&(list.len() as u32).to_le_bytes());
                for w in list {
                    b.extend_from_slice(&w.id.to_le_bytes());
                    put_str(&mut b, &w.title);
                    put_str(&mut b, &w.app);
                    b.extend_from_slice(&w.w.to_le_bytes());
                    b.extend_from_slice(&w.h.to_le_bytes());
                }
                121
            }
            ToClient::Sharing { kind, text } => {
                b.push(*kind);
                b.extend_from_slice(text.as_bytes());
                122
            }
            ToClient::Monitors { list, current } => {
                b.push(*current);
                b.extend_from_slice(&(list.len() as u32).to_le_bytes());
                for m in list {
                    put_str(&mut b, &m.name);
                    b.extend_from_slice(&m.w.to_le_bytes());
                    b.extend_from_slice(&m.h.to_le_bytes());
                    b.push(m.primary as u8);
                }
                112
            }
            ToClient::FileDone { id, ok, text } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.push(*ok as u8);
                b.extend_from_slice(text.as_bytes());
                107
            }
        };
        (kind, b)
    }

    pub fn decode(kind: u8, d: &[u8]) -> io::Result<ToClient> {
        let mut c = Cur { d, p: 0 };
        Ok(match kind {
            101 => {
                ToClient::Hello { w: c.u16()?, h: c.u16()?, backend: String::from_utf8_lossy(c.rest()).into_owned() }
            }
            102 => {
                let (id, w, h, mode) = (c.u32()?, c.u16()?, c.u16()?, c.mode()?);
                let mut moves = Vec::new();
                for _ in 0..c.u16()? {
                    let (x, y, mw, mh) = (c.u16()? as usize, c.u16()? as usize, c.u16()? as usize, c.u16()? as usize);
                    let dy = c.u16()? as i16 as isize;
                    let src = y as isize - dy;
                    if x + mw > w as usize || y + mh > h as usize || src < 0 || src as usize + mh > h as usize {
                        return Err(bad());
                    }
                    moves.push(Move { x, y, w: mw, h: mh, dy });
                }
                let n = c.u32()? as usize;
                let mut tiles = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    let (x, y, tw, th) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
                    let len = c.u32()? as usize;
                    let data = c.take(len)?.to_vec();
                    if x as usize + tw as usize > w as usize || y as usize + th as usize > h as usize {
                        return Err(bad());
                    }
                    tiles.push(Tile { x, y, w: tw, h: th, data });
                }
                let mut video = None;
                if c.p < c.d.len() {
                    let (x, y, tw, th) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
                    let len = c.u32()? as usize;
                    let data = c.take(len)?.to_vec();
                    if x as usize + tw as usize > w as usize || y as usize + th as usize > h as usize {
                        return Err(bad());
                    }
                    video = Some(Tile { x, y, w: tw, h: th, data });
                }
                ToClient::Frame(Frame { id, w, h, mode, moves, tiles, video })
            }
            103 => ToClient::Error(String::from_utf8_lossy(c.rest()).into_owned()),
            104 => ToClient::Pong(c.u32()?),
            105 => ToClient::Clipboard(String::from_utf8_lossy(c.rest()).into_owned()),
            106 => {
                ToClient::FileProgress { id: c.u32()?, received: u64::from_le_bytes(c.take(8)?.try_into().unwrap()) }
            }
            108 => {
                let (path, error) = (c.str()?, c.str()?);
                let n = c.u32()? as usize;
                let mut entries = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    entries.push(DirEntry { name: c.str()?, dir: c.u8()? != 0, size: c.u64()?, mtime: 0 });
                }
                if c.d.len() - c.p >= entries.len() * 8 {
                    for e in entries.iter_mut() {
                        e.mtime = c.u64()?;
                    }
                }
                ToClient::DirList { path, entries, error }
            }
            109 => ToClient::FetchStart {
                id: c.u32()?,
                size: c.u64()?,
                name: String::from_utf8_lossy(c.rest()).into_owned(),
            },
            110 => ToClient::FetchData { id: c.u32()?, data: c.rest().to_vec() },
            111 => ToClient::FetchEnd {
                id: c.u32()?,
                ok: c.u8()? != 0,
                text: String::from_utf8_lossy(c.rest()).into_owned(),
            },
            113 => ToClient::Audio(c.rest().to_vec()),
            114 => ToClient::Notice(String::from_utf8_lossy(c.rest()).into_owned()),
            115 => ToClient::Chat(String::from_utf8_lossy(c.rest()).into_owned()),
            116 => ToClient::Snapshot(c.rest().to_vec()),
            117 => ToClient::OpDone { ok: c.u8()? != 0, text: String::from_utf8_lossy(c.rest()).into_owned() },
            119 => ToClient::TermOutput { id: c.u32()?, data: c.rest().to_vec() },
            121 => {
                let error = c.str()?;
                let n = c.u32()? as usize;
                let mut list = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    list.push(crate::capture::WindowInfo {
                        id: c.u64()?,
                        title: c.str()?,
                        app: c.str()?,
                        w: c.u16()?,
                        h: c.u16()?,
                    });
                }
                ToClient::WindowList { list, error }
            }
            122 => ToClient::Sharing { kind: c.u8()?, text: String::from_utf8_lossy(c.rest()).into_owned() },
            120 => ToClient::TermExit { id: c.u32()?, text: String::from_utf8_lossy(c.rest()).into_owned() },
            118 => ToClient::FetchBegin {
                id: c.u32()?,
                size: c.u64()?,
                tag: c.u32()?,
                name: String::from_utf8_lossy(c.rest()).into_owned(),
            },
            112 => {
                let current = c.u8()?;
                let n = c.u32()? as usize;
                let mut list = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    list.push(MonitorInfo { name: c.str()?, w: c.u16()?, h: c.u16()?, primary: c.u8()? != 0 });
                }
                ToClient::Monitors { list, current }
            }
            107 => ToClient::FileDone {
                id: c.u32()?,
                ok: c.u8()? != 0,
                text: String::from_utf8_lossy(c.rest()).into_owned(),
            },
            _ => return Err(bad()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let msgs = [
            ToAgent::Settings(Settings { view_w: 1900, view_h: 1000, scale: 75, mode: Mode::Gray16, fps: 10 }),
            ToAgent::Ack(7),
            ToAgent::Key { keysym: 0xff0d, mods: MOD_CTRL },
            ToAgent::Pointer { x: 3, y: 4 },
            ToAgent::Button { button: 1, pressed: true },
            ToAgent::Scroll { steps: -2 },
            ToAgent::Refresh,
            ToAgent::Ping(5),
            ToAgent::KeyState { keysym: 0xffe1, down: true },
            ToAgent::HScroll { steps: -3 },
            ToAgent::Clipboard("hello".into()),
            ToAgent::SecureAttention,
            ToAgent::FileStart { id: 1, size: 3, name: "a.txt".into() },
            ToAgent::FileData { id: 1, data: vec![1, 2, 3] },
            ToAgent::FileEnd { id: 1 },
            ToAgent::ListDir { path: "/home".into() },
            ToAgent::Fetch { path: "C:/x".into() },
            ToAgent::FetchAck { id: 3, received: 1 << 40 },
            ToAgent::FileStartIn { id: 4, size: 10, dir: "/tmp".into(), name: "a/b.txt".into() },
            ToAgent::FileOp { op: FileOp::Rename, path: "/tmp/a".into(), arg: "b".into() },
            ToAgent::FetchTagged { tag: 5, path: "/x".into() },
            ToAgent::FetchCancel { id: 6, tag: 5 },
            ToAgent::TermOpen { id: 1, cols: 80, rows: 24 },
            ToAgent::TermInput { id: 1, data: b"ls\r".to_vec() },
            ToAgent::TermResize { id: 1, cols: 120, rows: 40 },
            ToAgent::TermClose { id: 1 },
            ToAgent::ListWindows,
            ToAgent::Share { kind: 2, id: 0x1234 },
            ToAgent::InstallHelper,
            ToAgent::PickOnRemote,
        ];
        for m in [
            ToClient::DirList {
                path: "/h".into(),
                entries: vec![DirEntry { name: "a".into(), dir: true, size: 0, mtime: 1_700_000_000 }],
                error: String::new(),
            },
            ToClient::FetchStart { id: 2, size: 9, name: "d/x.txt".into() },
            ToClient::FetchData { id: 2, data: vec![1] },
            ToClient::FetchEnd { id: 2, ok: true, text: String::new() },
            ToClient::OpDone { ok: false, text: "denied".into() },
            ToClient::FetchBegin { id: 3, size: 4, tag: 5, name: "d/y".into() },
            ToClient::TermOutput { id: 1, data: b"\x1b[31mhi".to_vec() },
            ToClient::TermExit { id: 1, text: "exit 0".into() },
            ToClient::WindowList {
                list: vec![crate::capture::WindowInfo {
                    id: 9,
                    title: "Inbox".into(),
                    app: "chrome".into(),
                    w: 800,
                    h: 600,
                }],
                error: String::new(),
            },
            ToClient::Sharing { kind: 1, text: "Inbox".into() },
            ToClient::Monitors {
                list: vec![MonitorInfo { name: "DP-1".into(), w: 2560, h: 1440, primary: true }],
                current: 0,
            },
        ] {
            let (k, p) = m.encode();
            assert_eq!(ToClient::decode(k, &p).unwrap(), m);
        }
        for m in msgs {
            let (k, p) = m.encode();
            assert_eq!(ToAgent::decode(k, &p).unwrap(), m);
        }
        let f = ToClient::Frame(Frame {
            id: 9,
            w: 64,
            h: 32,
            mode: Mode::Mono,
            moves: vec![Move { x: 0, y: 10, w: 64, h: 15, dy: -3 }],
            tiles: vec![Tile { x: 1, y: 2, w: 3, h: 4, data: vec![1, 2] }],
            video: Some(Tile { x: 0, y: 0, w: 16, h: 16, data: vec![0, 0, 1] }),
        });
        let mut wire = Vec::new();
        let (k, p) = f.encode();
        write_msg(&mut wire, k, &p).unwrap();
        let (k, p) = read_msg(&mut wire.as_slice()).unwrap().unwrap();
        assert_eq!(ToClient::decode(k, &p).unwrap(), f);
    }
}
