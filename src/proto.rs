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
    /// Picture forms the viewer reads beyond the first ones (`FORMAT_*`); an older viewer sends
    /// none, so its agent keeps to those.
    pub formats: u8,
}

/// True colour frames whose text and drawings index a palette kept for the session (`color::Palette`).
pub const FORMAT_PALETTE: u8 = 1;
/// Every form this program reads.
pub const FORMATS: u8 = FORMAT_PALETTE;

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
pub use crate::xk::SUPER_L;

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
    /// A picture on the viewer's clipboard (a screenshot), as PNG, put on the host's for the
    /// paste that follows.
    ClipImage(Vec<u8>),
    /// The viewer's pixels to a point, in hundredths (200 on a HiDPI screen), for a screen that
    /// lays itself out by it (a web page).
    Density(u16),
    /// Ctrl+Alt+Del, which no viewer can capture locally.
    SecureAttention,
    /// A file upload begins, into `dir` (empty: the remote user's Downloads folder); `name` may
    /// be relative ("folder/file.txt").
    FileStart {
        id: u32,
        size: u64,
        dir: String,
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
    /// Sends a remote file, or every file under a remote folder, to the viewer; each file is
    /// announced with `FetchStart` carrying `tag`.
    Fetch {
        tag: u32,
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
    /// Changes the remote file system, answered with `OpDone`.
    FileOp {
        op: FileOp,
        path: String,
        arg: String,
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
    /// Shares the screen, a window or a program.
    Share(crate::shared::Share),
    /// Installs what the window list needs on the host (the GNOME Shell helper).
    InstallHelper,
    /// Opens the desktop's own sharing dialog on the remote screen to choose there.
    PickOnRemote,
    /// Asks for the interactive elements of the window in front (accessibility), for AI agents.
    UiState {
        /// Also list plain text (labels, headings), not only what can be operated.
        text: bool,
        max: u16,
    },
    /// Operates an element from the last `UiState`, or brings a window (`target` = window id) forward.
    /// Files uploaded into the clipboard folder of `batch` (`files::CLIPBOARD`), to put on the
    /// clipboard by their names there and paste (Ctrl+V).
    PasteFiles {
        batch: String,
        names: Vec<String>,
    },
    /// A voice call started (true) or hung up; the person at the screen answers it.
    Call(bool),
    /// A piece of the viewer's voice: 20 ms of Opus.
    Voice(Vec<u8>),
    /// A connection to `host`:`port` as the computer sees it, for the viewer's forwarded port.
    PortOpen {
        id: u32,
        host: String,
        port: u16,
    },
    PortData {
        id: u32,
        data: Vec<u8>,
    },
    PortClose {
        id: u32,
    },
    /// Something done to the computer itself (`hostctl`): restart, lock, keep its own keyboard
    /// and mouse out, hide its screen, lock it on leaving; `QUERY` asks what can be done.
    Host {
        act: u8,
        on: bool,
    },
    UiAct {
        target: u64,
        act: UiAction,
        text: String,
    },
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum UiAction {
    Click = 1,
    DoubleClick = 2,
    RightClick = 3,
    /// Puts `text` in (after focusing it), replacing what is there.
    SetText = 4,
    /// Types `text` at the element's caret, keeping what is there.
    Type = 5,
    Focus = 6,
    /// Brings the window `target` to the front.
    FocusWindow = 7,
}

/// One element of a window's accessibility tree.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct UiElem {
    pub id: u32,
    pub role: String,
    pub name: String,
    pub value: String,
    /// Centre in captured pixels, or -1 when the desktop does not tell (GNOME on Wayland).
    pub x: i32,
    pub y: i32,
    /// UI_FOCUSED, UI_DISABLED, UI_CHECKED, UI_SELECTED, UI_EXPANDED, UI_EDITABLE.
    pub flags: u8,
}

pub const UI_FOCUSED: u8 = 1;
pub const UI_DISABLED: u8 = 2;
pub const UI_CHECKED: u8 = 4;
pub const UI_SELECTED: u8 = 8;
pub const UI_EXPANDED: u8 = 16;
pub const UI_EDITABLE: u8 = 32;

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

impl MonitorInfo {
    /// A camera listed after the monitors (`capture::camera`): to look at, not a screen.
    pub fn camera(&self) -> bool {
        self.name.starts_with("Camera: ")
    }
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
    /// With a palette (whether the viewer forgets its own first, and the slots that changed),
    /// every tile's data starts with its form (`color::Palette`).
    pub palette: Option<(bool, Vec<(u8, [u8; 3])>)>,
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
    /// A picture copied on the remote computer (a screenshot), as PNG.
    ClipImage(Vec<u8>),
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
    /// A download of the `Fetch` with `tag` begins; `name` is relative ("folder/sub/file.txt" for
    /// a folder).
    FetchStart {
        id: u32,
        size: u64,
        tag: u32,
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
    /// Every file of the `Fetch` with `tag` has been sent (or failed). Sent only for tags with
    /// [`crate::files::DONE_TAG`] set, so viewers that do not ask never see it.
    FetchDone {
        tag: u32,
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
        list: Vec<crate::shared::WindowInfo>,
        error: String,
    },
    /// What InstallHelper did: whether the helper is in place, and what to tell the user.
    HelperInstalled {
        ok: bool,
        text: String,
    },
    /// What is shared now; `kind` is `Share`'s number on the wire (0: the whole screen).
    Sharing {
        kind: u8,
        text: String,
    },
    /// The window in front: its title, the other windows (id, title), its elements, how many more
    /// there were, and any problem.
    UiTree {
        title: String,
        windows: Vec<(u64, String)>,
        elems: Vec<UiElem>,
        more: u32,
        note: String,
    },
    /// The outcome of a `UiAct`.
    UiDone {
        ok: bool,
        text: String,
    },
    /// What the person at the screen lets this viewer, who came by the computer's ID, do (`grant`).
    Allowed(u8),
    /// What can be done to the computer itself and what is on (`hostctl` bits).
    HostState {
        can: u8,
        on: u8,
    },
    /// Files copied at the host, for the viewer to fetch onto its own clipboard.
    ClipFiles(Vec<ClipFile>),
    /// A piece of the voice at the computer, 20 ms of Opus.
    Voice(Vec<u8>),
    /// Where the call is (`chat::CALL_*`).
    CallState(u8),
    /// Whether the connection for a forwarded port was made, and why not.
    PortOpened {
        id: u32,
        ok: bool,
        text: String,
    },
    PortData {
        id: u32,
        data: Vec<u8>,
    },
    PortClosed {
        id: u32,
    },
}

/// A file or folder copied at the host: its path there, and its size (a folder's, all of it).
#[derive(Debug, PartialEq, Clone)]
pub struct ClipFile {
    pub path: String,
    pub size: u64,
    pub dir: bool,
}

/// The largest picture a viewer takes: 16384 a side (four 4K screens in a row) and 64 megapixels.
pub const MAX_SIDE: usize = 16384;
pub const MAX_PIXELS: usize = 64 << 20;

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed message")
}

/// A message, or a kind of something in one, from a newer version: the reader may pass it over.
fn unknown() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "unknown message")
}

/// Whether a decoding error is only a newer version's message, which can be passed over.
pub fn is_unknown(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported
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

/// Returns `None` on a clean end of stream; a cut inside a header is an error.
pub fn read_msg(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(head[1..].try_into().unwrap()) as usize;
    // Frames, snapshots and file chunks stay far below this; anything bigger is a broken peer.
    if len > 64 << 20 {
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
                b.extend_from_slice(&[s.scale, s.mode.to_u8(), s.fps, s.formats]);
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
            ToAgent::ClipImage(png) => {
                b.extend_from_slice(png);
                45
            }
            ToAgent::Density(d) => {
                b.extend_from_slice(&d.to_le_bytes());
                46
            }
            ToAgent::SecureAttention => 12,
            // The short forms (Downloads, tag 0) keep their first codes, which every agent knows.
            ToAgent::FileStart { id, size, dir, name } if dir.is_empty() => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                b.extend_from_slice(name.as_bytes());
                13
            }
            ToAgent::FileStart { id, size, dir, name } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                put_str(&mut b, dir);
                put_str(&mut b, name);
                24
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
            ToAgent::Fetch { tag: 0, path } => {
                b.extend_from_slice(path.as_bytes());
                17
            }
            ToAgent::Fetch { tag, path } => {
                b.extend_from_slice(&tag.to_le_bytes());
                b.extend_from_slice(path.as_bytes());
                26
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
            ToAgent::FileOp { op, path, arg } => {
                b.push(*op as u8);
                put_str(&mut b, path);
                put_str(&mut b, arg);
                25
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
            ToAgent::UiState { text, max } => {
                b.push(*text as u8);
                b.extend_from_slice(&max.to_le_bytes());
                36
            }
            ToAgent::UiAct { target, act, text } => {
                b.extend_from_slice(&target.to_le_bytes());
                b.push(*act as u8);
                b.extend_from_slice(text.as_bytes());
                37
            }
            ToAgent::Host { act, on } => {
                b.extend_from_slice(&[*act, *on as u8]);
                38
            }
            ToAgent::Call(on) => {
                b.push(*on as u8);
                43
            }
            ToAgent::Voice(p) => {
                b.extend_from_slice(p);
                44
            }
            ToAgent::PortOpen { id, host, port } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&port.to_le_bytes());
                b.extend_from_slice(host.as_bytes());
                40
            }
            ToAgent::PortData { id, data } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(data);
                41
            }
            ToAgent::PortClose { id } => {
                b.extend_from_slice(&id.to_le_bytes());
                42
            }
            ToAgent::PasteFiles { batch, names } => {
                put_str(&mut b, batch);
                b.extend_from_slice(&(names.len() as u16).to_le_bytes());
                for n in names {
                    put_str(&mut b, n);
                }
                39
            }
            ToAgent::Share(share) => {
                let (kind, id) = share.to_wire();
                b.push(kind);
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
                formats: c.u8().unwrap_or(0),
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
            45 => ToAgent::ClipImage(c.rest().to_vec()),
            46 => ToAgent::Density(c.u16()?),
            12 => ToAgent::SecureAttention,
            13 => ToAgent::FileStart {
                id: c.u32()?,
                size: c.u64()?,
                dir: String::new(),
                name: String::from_utf8_lossy(c.rest()).into_owned(),
            },
            14 => ToAgent::FileData { id: c.u32()?, data: c.rest().to_vec() },
            15 => ToAgent::FileEnd { id: c.u32()? },
            16 => ToAgent::ListDir { path: String::from_utf8_lossy(c.rest()).into_owned() },
            17 => ToAgent::Fetch { tag: 0, path: String::from_utf8_lossy(c.rest()).into_owned() },
            18 => ToAgent::FetchAck { id: c.u32()?, received: c.u64()? },
            19 => ToAgent::SelectMonitor(c.u8()?),
            20 => ToAgent::Audio(c.u8()? != 0),
            21 => ToAgent::Chat(String::from_utf8_lossy(c.rest()).into_owned()),
            22 => ToAgent::Pause(c.u8()? != 0),
            23 => ToAgent::Snapshot,
            24 => ToAgent::FileStart { id: c.u32()?, size: c.u64()?, dir: c.str()?, name: c.str()? },
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
            26 => ToAgent::Fetch { tag: c.u32()?, path: String::from_utf8_lossy(c.rest()).into_owned() },
            27 => ToAgent::FetchCancel { id: c.u32()?, tag: c.u32()? },
            28 => ToAgent::TermOpen { id: c.u32()?, cols: c.u16()?, rows: c.u16()? },
            29 => ToAgent::TermInput { id: c.u32()?, data: c.rest().to_vec() },
            30 => ToAgent::TermResize { id: c.u32()?, cols: c.u16()?, rows: c.u16()? },
            31 => ToAgent::TermClose { id: c.u32()? },
            32 => ToAgent::ListWindows,
            34 => ToAgent::InstallHelper,
            35 => ToAgent::PickOnRemote,
            36 => ToAgent::UiState { text: c.u8()? != 0, max: c.u16()? },
            38 => ToAgent::Host { act: c.u8()?, on: c.u8()? != 0 },
            43 => ToAgent::Call(c.u8()? != 0),
            44 => ToAgent::Voice(c.rest().to_vec()),
            40 => ToAgent::PortOpen { id: c.u32()?, port: c.u16()?, host: String::from_utf8_lossy(c.rest()).into_owned() },
            41 => ToAgent::PortData { id: c.u32()?, data: c.rest().to_vec() },
            42 => ToAgent::PortClose { id: c.u32()? },
            39 => {
                let batch = c.str()?;
                let n = c.u16()? as usize;
                let mut names = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    names.push(c.str()?);
                }
                ToAgent::PasteFiles { batch, names }
            }
            37 => ToAgent::UiAct {
                target: c.u64()?,
                act: match c.u8()? {
                    1 => UiAction::Click,
                    2 => UiAction::DoubleClick,
                    3 => UiAction::RightClick,
                    4 => UiAction::SetText,
                    5 => UiAction::Type,
                    6 => UiAction::Focus,
                    7 => UiAction::FocusWindow,
                    _ => return Err(bad()),
                },
                text: String::from_utf8_lossy(c.rest()).into_owned(),
            },
            33 => ToAgent::Share(crate::shared::Share::from_wire(c.u8()?, c.u64()?).ok_or_else(unknown)?),
            _ => return Err(unknown()),
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
                b.push(f.mode.to_u8() | if f.palette.is_some() { 0x80 } else { 0 });
                b.extend_from_slice(&(f.moves.len() as u16).to_le_bytes());
                for m in &f.moves {
                    for v in [m.x as u16, m.y as u16, m.w as u16, m.h as u16, m.dy as i16 as u16] {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                }
                if let Some((fresh, changed)) = &f.palette {
                    b.extend_from_slice(&(changed.len() as u16 | if *fresh { 0x8000 } else { 0 }).to_le_bytes());
                    for (n, c) in changed {
                        b.push(*n);
                        b.extend_from_slice(c);
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
            ToClient::ClipImage(png) => {
                b.extend_from_slice(png);
                135
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
            ToClient::FetchStart { id, size, tag: 0, name } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                b.extend_from_slice(name.as_bytes());
                109
            }
            ToClient::FetchStart { id, size, tag, name } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(&size.to_le_bytes());
                b.extend_from_slice(&tag.to_le_bytes());
                b.extend_from_slice(name.as_bytes());
                118
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
            ToClient::FetchDone { tag } => {
                b.extend_from_slice(&tag.to_le_bytes());
                126
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
            ToClient::HelperInstalled { ok, text } => {
                b.push(*ok as u8);
                b.extend_from_slice(text.as_bytes());
                125
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
            ToClient::UiTree { title, windows, elems, more, note } => {
                put_str(&mut b, title);
                put_str(&mut b, note);
                b.extend_from_slice(&more.to_le_bytes());
                b.extend_from_slice(&(windows.len() as u32).to_le_bytes());
                for (id, t) in windows {
                    b.extend_from_slice(&id.to_le_bytes());
                    put_str(&mut b, t);
                }
                b.extend_from_slice(&(elems.len() as u32).to_le_bytes());
                for e in elems {
                    b.extend_from_slice(&e.id.to_le_bytes());
                    put_str(&mut b, &e.role);
                    put_str(&mut b, &e.name);
                    put_str(&mut b, &e.value);
                    b.extend_from_slice(&e.x.to_le_bytes());
                    b.extend_from_slice(&e.y.to_le_bytes());
                    b.push(e.flags);
                }
                123
            }
            ToClient::UiDone { ok, text } => {
                b.push(*ok as u8);
                b.extend_from_slice(text.as_bytes());
                124
            }
            ToClient::Allowed(bits) => {
                b.push(*bits);
                127
            }
            ToClient::HostState { can, on } => {
                b.extend_from_slice(&[*can, *on]);
                128
            }
            ToClient::Voice(p) => {
                b.extend_from_slice(p);
                133
            }
            ToClient::CallState(c) => {
                b.push(*c);
                134
            }
            ToClient::PortOpened { id, ok, text } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.push(*ok as u8);
                b.extend_from_slice(text.as_bytes());
                130
            }
            ToClient::PortData { id, data } => {
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(data);
                131
            }
            ToClient::PortClosed { id } => {
                b.extend_from_slice(&id.to_le_bytes());
                132
            }
            ToClient::ClipFiles(list) => {
                b.extend_from_slice(&(list.len() as u16).to_le_bytes());
                for f in list {
                    put_str(&mut b, &f.path);
                    b.extend_from_slice(&f.size.to_le_bytes());
                    b.push(f.dir as u8);
                }
                129
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
                let (id, w, h, form) = (c.u32()?, c.u16()?, c.u16()?, c.u8()?);
                let mode = Mode::from_u8(form & 0x7f).ok_or_else(bad)?;
                // The viewer keeps a picture this big: one no screen sends is refused, not allocated.
                if w as usize > MAX_SIDE || h as usize > MAX_SIDE || w as usize * h as usize > MAX_PIXELS {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a {w}x{h} picture is too big")));
                }
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
                let palette = if form & 0x80 != 0 {
                    let n = c.u16()?;
                    let (fresh, n) = (n & 0x8000 != 0, (n & 0x7fff) as usize);
                    if n > 256 {
                        return Err(bad());
                    }
                    Some((fresh, c.take(n * 4)?.chunks_exact(4).map(|p| (p[0], [p[1], p[2], p[3]])).collect()))
                } else {
                    None
                };
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
                ToClient::Frame(Frame { id, w, h, mode, moves, palette, tiles, video })
            }
            103 => ToClient::Error(String::from_utf8_lossy(c.rest()).into_owned()),
            104 => ToClient::Pong(c.u32()?),
            105 => ToClient::Clipboard(String::from_utf8_lossy(c.rest()).into_owned()),
            135 => ToClient::ClipImage(c.rest().to_vec()),
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
                tag: 0,
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
            126 => ToClient::FetchDone { tag: c.u32()? },
            125 => ToClient::HelperInstalled { ok: c.u8()? != 0, text: String::from_utf8_lossy(c.rest()).into_owned() },
            119 => ToClient::TermOutput { id: c.u32()?, data: c.rest().to_vec() },
            121 => {
                let error = c.str()?;
                let n = c.u32()? as usize;
                let mut list = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    list.push(crate::shared::WindowInfo {
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
            123 => {
                let (title, note, more) = (c.str()?, c.str()?, c.u32()?);
                let n = c.u32()? as usize;
                let mut windows = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    windows.push((c.u64()?, c.str()?));
                }
                let n = c.u32()? as usize;
                let mut elems = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    elems.push(UiElem {
                        id: c.u32()?,
                        role: c.str()?,
                        name: c.str()?,
                        value: c.str()?,
                        x: c.u32()? as i32,
                        y: c.u32()? as i32,
                        flags: c.u8()?,
                    });
                }
                ToClient::UiTree { title, windows, elems, more, note }
            }
            124 => ToClient::UiDone { ok: c.u8()? != 0, text: String::from_utf8_lossy(c.rest()).into_owned() },
            127 => ToClient::Allowed(c.u8()?),
            128 => ToClient::HostState { can: c.u8()?, on: c.u8()? },
            133 => ToClient::Voice(c.rest().to_vec()),
            134 => ToClient::CallState(c.u8()?),
            130 => ToClient::PortOpened { id: c.u32()?, ok: c.u8()? != 0, text: String::from_utf8_lossy(c.rest()).into_owned() },
            131 => ToClient::PortData { id: c.u32()?, data: c.rest().to_vec() },
            132 => ToClient::PortClosed { id: c.u32()? },
            129 => {
                let n = c.u16()? as usize;
                let mut list = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    list.push(ClipFile { path: c.str()?, size: c.u64()?, dir: c.u8()? != 0 });
                }
                ToClient::ClipFiles(list)
            }
            120 => ToClient::TermExit { id: c.u32()?, text: String::from_utf8_lossy(c.rest()).into_owned() },
            118 => ToClient::FetchStart {
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
            _ => return Err(unknown()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xk;

    /// A newer version's message can be passed over; a damaged one cannot.
    #[test]
    fn unknown_messages_differ_from_damaged_ones() {
        assert!(is_unknown(&ToAgent::decode(250, &[]).unwrap_err()));
        assert!(is_unknown(&ToClient::decode(250, &[]).unwrap_err()));
        let mut share = vec![9u8];
        share.extend_from_slice(&0u64.to_le_bytes());
        assert!(is_unknown(&ToAgent::decode(33, &share).unwrap_err()));
        let (k, p) = ToAgent::FileStart { id: 1, size: 3, dir: "/d".into(), name: "a".into() }.encode();
        assert!(!is_unknown(&ToAgent::decode(k, &p[..p.len() - 3]).unwrap_err()));
    }

    #[test]
    fn roundtrip() {
        let msgs = [
            ToAgent::Settings(Settings { view_w: 1900, view_h: 1000, scale: 75, mode: Mode::Gray16, fps: 10, formats: FORMATS }),
            ToAgent::Ack(7),
            ToAgent::Key { keysym: xk::RETURN, mods: MOD_CTRL },
            ToAgent::Pointer { x: 3, y: 4 },
            ToAgent::Button { button: 1, pressed: true },
            ToAgent::Scroll { steps: -2 },
            ToAgent::Refresh,
            ToAgent::Ping(5),
            ToAgent::KeyState { keysym: xk::SHIFT_L, down: true },
            ToAgent::HScroll { steps: -3 },
            ToAgent::Clipboard("hello".into()),
            ToAgent::Density(200),
            ToAgent::SecureAttention,
            ToAgent::FileStart { id: 1, size: 3, dir: String::new(), name: "a.txt".into() },
            ToAgent::FileData { id: 1, data: vec![1, 2, 3] },
            ToAgent::FileEnd { id: 1 },
            ToAgent::ListDir { path: "/home".into() },
            ToAgent::Fetch { tag: 0, path: "C:/x".into() },
            ToAgent::FetchAck { id: 3, received: 1 << 40 },
            ToAgent::FileStart { id: 4, size: 10, dir: "/tmp".into(), name: "a/b.txt".into() },
            ToAgent::FileOp { op: FileOp::Rename, path: "/tmp/a".into(), arg: "b".into() },
            ToAgent::Fetch { tag: 5, path: "/x".into() },
            ToAgent::FetchCancel { id: 6, tag: 5 },
            ToAgent::TermOpen { id: 1, cols: 80, rows: 24 },
            ToAgent::TermInput { id: 1, data: b"ls\r".to_vec() },
            ToAgent::TermResize { id: 1, cols: 120, rows: 40 },
            ToAgent::TermClose { id: 1 },
            ToAgent::ListWindows,
            ToAgent::Share(crate::shared::Share::App(0x1234)),
            ToAgent::InstallHelper,
            ToAgent::PickOnRemote,
            ToAgent::UiState { text: true, max: 150 },
            ToAgent::UiAct { target: 3, act: UiAction::SetText, text: "hello".into() },
            ToAgent::Host { act: 4, on: true },
            ToAgent::Call(true),
            ToAgent::Voice(vec![7, 8]),
            ToAgent::PortOpen { id: 2, host: "localhost".into(), port: 3389 },
            ToAgent::PortData { id: 2, data: vec![1, 2, 3] },
            ToAgent::PortClose { id: 2 },
            ToAgent::PasteFiles { batch: "3".into(), names: vec!["a.txt".into(), "photos".into()] },
        ];
        for m in [
            ToClient::DirList {
                path: "/h".into(),
                entries: vec![DirEntry { name: "a".into(), dir: true, size: 0, mtime: 1_700_000_000 }],
                error: String::new(),
            },
            ToClient::FetchStart { id: 2, size: 9, tag: 0, name: "d/x.txt".into() },
            ToClient::FetchData { id: 2, data: vec![1] },
            ToClient::FetchEnd { id: 2, ok: true, text: String::new() },
            ToClient::OpDone { ok: false, text: "denied".into() },
            ToClient::HelperInstalled { ok: true, text: "installed; sign out and in".into() },
            ToClient::FetchStart { id: 3, size: 4, tag: 5, name: "d/y".into() },
            ToClient::FetchDone { tag: 0x8000_0005 },
            ToClient::TermOutput { id: 1, data: b"\x1b[31mhi".to_vec() },
            ToClient::TermExit { id: 1, text: "exit 0".into() },
            ToClient::WindowList {
                list: vec![crate::shared::WindowInfo {
                    id: 9,
                    title: "Inbox".into(),
                    app: "chrome".into(),
                    w: 800,
                    h: 600,
                }],
                error: String::new(),
            },
            ToClient::Sharing { kind: 1, text: "Inbox".into() },
            ToClient::UiTree {
                title: "Notepad".into(),
                windows: vec![(42, "Chrome".into())],
                elems: vec![UiElem {
                    id: 1,
                    role: "button".into(),
                    name: "Save".into(),
                    x: -1,
                    y: 20,
                    flags: UI_FOCUSED,
                    ..Default::default()
                }],
                more: 3,
                note: String::new(),
            },
            ToClient::UiDone { ok: true, text: "clicked".into() },
            ToClient::Allowed(5),
            ToClient::HostState { can: 0b111110, on: 0b1000 },
            ToClient::Voice(vec![1, 2, 3]),
            ToClient::CallState(2),
            ToClient::PortOpened { id: 2, ok: false, text: "refused".into() },
            ToClient::PortData { id: 2, data: vec![9; 5] },
            ToClient::PortClosed { id: 2 },
            ToClient::ClipFiles(vec![ClipFile { path: "C:/a.txt".into(), size: 5, dir: false }, ClipFile { path: "/home/x/photos".into(), size: 1 << 33, dir: true }]),
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
        // A picture no screen sends is refused before anything is allocated for it.
        let huge = ToClient::Frame(Frame { id: 1, w: 65535, h: 65535, mode: Mode::Mono, moves: vec![], palette: None, tiles: vec![], video: None });
        let (k, p) = huge.encode();
        assert!(ToClient::decode(k, &p).is_err());
        let wide = ToClient::Frame(Frame { id: 1, w: 15360, h: 2160, mode: Mode::Mono, moves: vec![], palette: None, tiles: vec![], video: None });
        let (k, p) = wide.encode();
        assert!(ToClient::decode(k, &p).is_ok(), "four 4K screens side by side");
        let f = ToClient::Frame(Frame {
            id: 9,
            w: 64,
            h: 32,
            mode: Mode::Mono,
            moves: vec![Move { x: 0, y: 10, w: 64, h: 15, dy: -3 }],
            palette: None,
            tiles: vec![Tile { x: 1, y: 2, w: 3, h: 4, data: vec![1, 2] }],
            video: Some(Tile { x: 0, y: 0, w: 16, h: 16, data: vec![0, 0, 1] }),
        });
        let mut wire = Vec::new();
        let (k, p) = f.encode();
        write_msg(&mut wire, k, &p).unwrap();
        let (k, p) = read_msg(&mut wire.as_slice()).unwrap().unwrap();
        assert_eq!(ToClient::decode(k, &p).unwrap(), f);
        let f = ToClient::Frame(Frame {
            id: 10,
            w: 64,
            h: 32,
            mode: Mode::True,
            moves: vec![],
            palette: Some((true, vec![(0, [255, 255, 255]), (7, [0, 0, 0])])),
            tiles: vec![Tile { x: 0, y: 0, w: 2, h: 1, data: vec![1, 0, 1] }],
            video: None,
        });
        let (k, p) = f.encode();
        assert_eq!(ToClient::decode(k, &p).unwrap(), f);
        // An older viewer's settings, without the forms it reads.
        let (k, mut p) = ToAgent::Settings(Settings { view_w: 9, view_h: 9, scale: 100, mode: Mode::True, fps: 5, formats: 0 }).encode();
        p.pop();
        assert!(matches!(ToAgent::decode(k, &p).unwrap(), ToAgent::Settings(Settings { formats: 0, .. })));
    }

    #[test]
    fn cut_header_is_an_error() {
        assert!(read_msg(&mut [].as_slice()).unwrap().is_none());
        let e = read_msg(&mut [7u8, 1].as_slice()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }
}
