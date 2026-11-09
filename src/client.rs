//! Terminal viewer: keeps an exact copy of the remote framebuffer and repaints
//! only damaged cells with sixel.

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::{cursor, execute, terminal};

use crate::capture::Share;
use crate::color::{Lut, Mode};
use crate::keys;
use crate::proto::{Frame, MOD_SUPER, SUPER_L, ToAgent, ToClient};
use crate::quant;
use crate::session::{Action, Control, Ev, Framebuffer, Options, Session};
use crate::sixel::Sixel;

/// What the terminal shows of the connection, as the window's modes: switched freely, on one
/// connection, without connecting again.
#[derive(Clone, Copy, PartialEq, Debug)]
enum View {
    Screen,
    Terminal,
    Files,
}

/// What covers the view for a moment.
#[derive(Clone, PartialEq, Debug)]
enum Overlay {
    None,
    Menu,
    /// The conversation with the person at the screen, and the line being typed.
    Chat(String),
    /// A question before something that cannot be undone: 'q' disconnect, 'x' delete.
    Confirm(char),
}

/// The remote folder shown in the files view.
#[derive(Default)]
struct RemoteFiles {
    path: String,
    entries: Vec<crate::proto::DirEntry>,
    error: String,
    sel: usize,
    top: usize,
    loading: bool,
    hidden: bool,
}

impl RemoteFiles {
    fn sep(&self) -> char {
        if self.path.contains('\\') && !self.path.contains('/') { '\\' } else { '/' }
    }

    fn child(&self, name: &str) -> String {
        if self.path == crate::files::DRIVES {
            return name.to_string();
        }
        let sep = self.sep();
        if self.path.ends_with(sep) { format!("{}{name}", self.path) } else { format!("{}{sep}{name}", self.path) }
    }

    fn parent(&self) -> String {
        let sep = self.sep();
        let trimmed = self.path.trim_end_matches(sep);
        if trimmed.len() == 2 && trimmed.ends_with(':') {
            return crate::files::DRIVES.into();
        }
        match trimmed.rfind(sep) {
            Some(0) => sep.to_string(),
            Some(i) if trimmed[..i].ends_with(':') => format!("{}{sep}", &trimmed[..i]),
            Some(i) => trimmed[..i].to_string(),
            None => self.path.clone(),
        }
    }

    /// The entries shown: hidden ones only when asked, folders first.
    fn shown(&self) -> Vec<&crate::proto::DirEntry> {
        let mut v: Vec<_> = self.entries.iter().filter(|e| self.hidden || !e.name.starts_with('.')).collect();
        v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }
}

/// The private modes the remote shell's program set (vim's own screen, a hidden cursor, mouse
/// reporting...), followed in its output so they can be given back after the menu or another
/// view used the terminal.
#[derive(Default)]
struct ShellModes {
    /// Its own (alternate) screen, as full-screen programs use.
    alt: bool,
    cursor_hidden: bool,
    /// Every other mode it turned on, by number (mouse 1000-1006, bracketed paste 2004...).
    on: std::collections::BTreeSet<u16>,
}

impl ShellModes {
    /// Follows `CSI ? n h` and `CSI ? n l` in what the shell wrote.
    fn follow(&mut self, data: &[u8]) {
        let mut i = 0;
        while i + 3 < data.len() {
            if data[i] == 0x1b && data[i + 1] == b'[' && data[i + 2] == b'?' {
                let mut j = i + 3;
                while j < data.len() && (data[j].is_ascii_digit() || data[j] == b';') {
                    j += 1;
                }
                if j < data.len() && (data[j] == b'h' || data[j] == b'l') {
                    let on = data[j] == b'h';
                    for n in String::from_utf8_lossy(&data[i + 3..j]).split(';').filter_map(|n| n.parse::<u16>().ok()) {
                        match n {
                            47 | 1047 | 1049 => self.alt = on,
                            25 => self.cursor_hidden = !on,
                            _ if on => {
                                self.on.insert(n);
                            }
                            _ => {
                                self.on.remove(&n);
                            }
                        }
                    }
                    i = j;
                }
            }
            i += 1;
        }
    }

    /// The sequences that set the modes again.
    fn restore(&self) -> String {
        let mut out = String::new();
        for n in &self.on {
            out.push_str(&format!("\x1b[?{n}h"));
        }
        out.push_str(if self.cursor_hidden { "\x1b[?25l" } else { "\x1b[?25h" });
        out
    }
}

/// A transfer under way: id, name, bytes done and in all, and whether it goes up.
struct Transfer {
    id: u32,
    name: String,
    done: u64,
    size: u64,
    upload: bool,
}

struct Viewer {
    out: Box<dyn Write>,
    /// Inside tmux: where the pane starts on the outer terminal (row, column), when images pass through
    /// tmux to it. tmux's own sixel support scales by the cell size it believes, which ssh clients that
    /// report a fixed 640x480 window get wrong.
    tmux: Option<(usize, usize)>,
    /// The terminal reports key releases (kitty keyboard protocol, or the Windows console), so keys go
    /// to the host as presses and releases and stay held as long as they are held here.
    key_states: bool,
    /// Keys and modifiers pressed on the host by this viewer.
    held: std::collections::BTreeSet<u32>,
    held_mods: u8,
    probe: Option<(u16, u16)>,
    paint_bytes: u64,
    sess: Session,
    ctl: Control,
    label: String,
    cols: usize,
    rows: usize,
    cw: usize,
    ch: usize,
    cell_override: Option<(usize, usize)>,
    cell_queried: Option<(usize, usize)>,
    fb: Framebuffer,
    lut_mode: Option<Mode>,
    lut: Lut,
    pal: Vec<[u8; 3]>,
    sixel: Sixel,
    idx: Vec<u8>,
    px: Vec<[u8; 3]>,
    qpal: Vec<[u8; 3]>,
    dirty: Vec<bool>,
    /// The mode shown, and what covers it.
    view: View,
    overlay: Overlay,
    /// What the shell wrote while it was not shown, written out when it is again.
    backlog: Vec<u8>,
    shell_modes: ShellModes,
    /// Uploads being sent a piece at a time: id, the file, its size and what went.
    uploads: Vec<(u32, std::fs::File, u64, u64)>,
    /// The full remote path a delete or rename was asked for (not looked up again later).
    target: Option<String>,
    files: RemoteFiles,
    transfers: Vec<Transfer>,
    /// Messages from the person at the screen not read yet.
    unread: usize,
    host: String,
    /// Who is at the screen, for the chat: "user (address)".
    peer: crate::chat::Peer,
    win_next: bool,
    player: Option<crate::audio::Player>,
    inbox: crate::files::Inbox,
    /// A message shown in the status line for a few seconds.
    note: Option<(String, Instant)>,
    /// A line being typed in the status line: c chat, g get a remote file, l send a local file.
    prompt: Option<(char, String)>,
    paused: bool,
    next_upload: u32,
    audio_bytes: usize,
    /// Number of remote monitors and the one shown.
    monitors: (usize, usize),
    /// Enlarge a reduced capture to the terminal; off where the terminal output itself crosses the network.
    stretch: bool,
    pointer: (u16, u16),
    quit: bool,
    paint_ms: f64,
    /// The shell on the remote host, kept while other modes are shown.
    shell: Option<u32>,
    next_shell: u32,
    /// What is shared: kind as in `Share` (0 the whole screen) and a description.
    sharing: (u8, String),
    /// The shareable windows, while the chooser is open (or why there is no list).
    share_list: Option<Result<Vec<crate::capture::WindowInfo>, String>>,
}

impl Viewer {
    fn measure(&mut self) {
        if let Some((w, h)) = self.probe {
            (self.cw, self.ch) = self.cell_override.unwrap_or((10, 20));
            (self.cols, self.rows) = (w as usize / self.cw, h as usize / self.ch + 1);
            (self.ctl.s.view_w, self.ctl.s.view_h) = (w, h);
            return;
        }
        if self.tmux.is_some() {
            self.tmux = tmux_pane();
        }
        let (c, r) = terminal::size().unwrap_or((80, 24));
        self.cols = c.max(10) as usize;
        self.rows = r.max(3) as usize;
        let (mut cw, mut ch) = (10, 20);
        if let Ok(ws) = terminal::window_size()
            && ws.columns > 0
            && ws.rows > 0
        {
            let (w, h) = (ws.width as usize / ws.columns as usize, ws.height as usize / ws.rows as usize);
            // Some ssh clients (Windows OpenSSH) report a fixed 640x480 whatever the window is.
            if (4..=64).contains(&w) && (8..=128).contains(&h) && (ws.width, ws.height) != (640, 480) {
                (cw, ch) = (w, h);
            }
        }
        (self.cw, self.ch) = self.cell_override.or(self.cell_queried).unwrap_or((cw, ch));
        self.ctl.s.view_w = (self.cols * self.cw).min(u16::MAX as usize) as u16;
        self.ctl.s.view_h = ((self.rows - 1) * self.ch).min(u16::MAX as usize) as u16;
    }

    fn display(&self) -> (usize, usize) {
        self.fb.fit((self.ctl.s.view_w as usize, self.ctl.s.view_h as usize), self.stretch)
    }

    fn apply(&mut self, f: Frame) -> io::Result<()> {
        if self.view != View::Screen || self.overlay != Overlay::None || self.prompt.is_some() {
            // Not in view (another mode, or something over it): kept, and drawn when it is again.
            if self.view == View::Screen {
                self.fb.apply(&f)?;
                self.dirty.iter_mut().for_each(|d| *d = true);
            }
            self.ctl.frame_done(false);
            return self.sess.send(ToAgent::Ack(f.id));
        }
        let started = Instant::now();
        let mut out = Vec::with_capacity(1 << 16);
        out.extend_from_slice(b"\x1b[?2026h");
        if self.fb.apply(&f)? {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
        if self.lut_mode != Some(f.mode) {
            // True colour gets a palette per painted rectangle instead (see `paint`).
            let shown = if f.mode == Mode::True { Mode::Color256 } else { f.mode };
            self.pal = shown.palette();
            self.lut = Lut::new(&self.pal);
            self.lut_mode = Some(f.mode);
        }
        let (w, h) = (self.fb.w, self.fb.h);
        let (dw, dh) = self.display();
        let img_rows = self.rows - 1;
        self.dirty.resize(self.cols * img_rows, false);
        for (tx, ty, tw, th) in f.damage() {
            let x0 = tx * dw / w;
            let x1 = ((tx + tw) * dw).div_ceil(w);
            let y0 = ty * dh / h;
            let y1 = ((ty + th) * dh).div_ceil(h);
            for r in y0 / self.ch..y1.div_ceil(self.ch).min(img_rows) {
                for c in x0 / self.cw..x1.div_ceil(self.cw).min(self.cols) {
                    self.dirty[r * self.cols + c] = true;
                }
            }
        }
        self.paint(&mut out, dw, dh);
        out.extend_from_slice(b"\x1b[?2026l");
        self.paint_bytes += out.len() as u64;
        self.out.write_all(&out)?;
        self.out.flush()?;
        self.paint_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.ctl.frame_done(f.covered() * 2 >= w * h);
        self.sess.send(ToAgent::Ack(f.id))
    }

    /// Repaints dirty cells, merged into rectangles of whole cells.
    fn paint(&mut self, out: &mut Vec<u8>, dw: usize, dh: usize) {
        let (cols, img_rows) = (self.cols, self.rows - 1);
        let (fw, fh) = (self.fb.w, self.fb.h);
        let adaptive = self.fb.mode == Some(Mode::True);
        for r in 0..img_rows {
            let mut c = 0;
            while c < cols {
                if !self.dirty[r * cols + c] {
                    c += 1;
                    continue;
                }
                let mut c1 = c;
                while c1 < cols && self.dirty[r * cols + c1] {
                    c1 += 1;
                }
                let mut r1 = r + 1;
                while r1 < img_rows && (c..c1).all(|k| self.dirty[r1 * cols + k]) {
                    r1 += 1;
                }
                for rr in r..r1 {
                    self.dirty[rr * cols + c..rr * cols + c1].fill(false);
                }
                let (x0, y0) = (c * self.cw, r * self.ch);
                let (w, h) = ((c1 - c) * self.cw, (r1 - r) * self.ch);
                self.px.clear();
                self.px.resize(w * h, [0; 3]);
                if dw > 0 {
                    for py in 0..h {
                        let dy = y0 + py;
                        if dy >= dh {
                            break;
                        }
                        let frow = (dy * fh / dh) * fw;
                        let line = &mut self.px[py * w..(py + 1) * w];
                        for (px, slot) in line.iter_mut().enumerate() {
                            let dx = x0 + px;
                            if dx >= dw {
                                break;
                            }
                            let o = (frow + dx * fw / dw) * 3;
                            slot.copy_from_slice(&self.fb.rgb[o..o + 3]);
                        }
                    }
                }
                let mut img = Vec::new();
                let (top, left) = self.tmux.unwrap_or((0, 0));
                let _ = write!(img, "\x1b[{};{}H", top + r + 1, left + c + 1);
                if adaptive {
                    quant::quantize(&self.px, &mut self.qpal, &mut self.idx);
                    self.sixel.encode(&mut img, w, h, &self.idx, &self.qpal);
                } else {
                    self.idx.clear();
                    self.idx.extend(self.px.iter().map(|p| self.lut.get(p[0], p[1], p[2])));
                    self.sixel.encode(&mut img, w, h, &self.idx, &self.pal);
                }
                if self.tmux.is_some() {
                    tmux_wrap(out, &img);
                } else {
                    out.extend_from_slice(&img);
                }
                c = c1;
            }
        }
    }

    /// The last row: the modes as tabs, the connection, and what is going on; or the line
    /// being typed.
    fn status(&mut self) -> io::Result<()> {
        if self.view == View::Terminal && self.overlay == Overlay::None {
            return Ok(());
        }
        let text = if let Some((kind, buf)) = &self.prompt {
            let what = match kind {
                'g' => "remote path to download",
                'v' => "share (number, A+number, a, p, or Enter for the whole screen)",
                'u' => "local file to send into this folder",
                'n' => "name of the new folder",
                'r' => "new name",
                _ => "local file to send to Downloads",
            };
            format!(" {what} (Enter: go, Esc: cancel)> {buf}_ ")
        } else {
            let tab = |v: View, name: &str| if self.view == v { format!("\x1b[1;4m{name}\x1b[22;24m") } else { name.to_string() };
            let tabs = format!(" {}  {}  {} ", tab(View::Screen, "Screen"), tab(View::Terminal, "Terminal"), tab(View::Files, "Files"));
            let what = if let Some((n, _)) = self.note.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(6)) {
                n.clone()
            } else if self.view == View::Files {
                self.files_line()
            } else if self.paused {
                format!("PAUSED (Ctrl+] u: one frame, p: resume) | {}", self.ctl.status(&self.label, self.fb.w, self.fb.h))
            } else if self.win_next {
                "Win+: press a key (Space: Win alone)".to_string()
            } else if self.sharing.0 != 0 {
                format!("SHARING {} (Ctrl+] v: whole screen) | {}", self.sharing.1, self.ctl.status(&self.label, self.fb.w, self.fb.h))
            } else {
                format!("{} | paint {:.1} ms", self.ctl.status(&self.label, self.fb.w, self.fb.h), self.paint_ms)
            };
            let chat = if self.unread > 0 { format!(" | chat: {} new (Ctrl+] c)", self.unread) } else { String::new() };
            format!("{tabs}| {what}{chat} | Ctrl+] menu ")
        };
        let shown = visible_len(&text);
        let text = cut_visible(&text, self.cols);
        let pad = self.cols.saturating_sub(shown.min(self.cols));
        write!(self.out, "\x1b[{};1H\x1b[0;7m{text}{}\x1b[0m", self.rows, " ".repeat(pad))?;
        self.out.flush()
    }

    fn push_settings(&mut self) -> io::Result<()> {
        self.sess.send(ToAgent::Settings(self.ctl.s))?;
        self.status()
    }

    fn full_redraw(&mut self) -> io::Result<()> {
        self.fb = Framebuffer::default();
        write!(self.out, "\x1b[0m\x1b[2J")?;
        self.out.flush()
    }

    fn run_action(&mut self, a: Action) -> io::Result<()> {
        match a {
            Action::None => self.status(),
            Action::Quit => {
                self.quit = true;
                Ok(())
            }
            Action::Refresh => {
                self.full_redraw()?;
                self.sess.send(ToAgent::Refresh)?;
                self.status()
            }
            Action::NextMonitor => {
                if self.monitors.0 > 1 {
                    let next = (self.monitors.1 + 1) % self.monitors.0;
                    self.sess.send(ToAgent::SelectMonitor(next as u8))?;
                }
                self.status()
            }
            Action::ArmWin => {
                self.win_next = true;
                self.status()
            }
            Action::Zoom => {
                self.stretch = !self.stretch;
                self.full_redraw()?;
                self.sess.send(ToAgent::Refresh)?;
                self.status()
            }
            Action::Settings => self.push_settings(),
            Action::Send(m) => {
                self.sess.send(m)?;
                self.status()
            }
        }
    }

    /// Shows a message in the status line (on stderr in a headless probe).
    fn notify(&mut self, text: &str) -> io::Result<()> {
        let text = &clean(text);
        if self.probe.is_some() {
            eprintln!("probe: {text}");
            return Ok(());
        }
        self.note = Some((text.to_string(), Instant::now()));
        self.status()
    }

    /// A key press or release, with the modifiers brought in line first.
    fn key_state(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        let Some((keysym, _)) = keys::keysym(k) else { return Ok(()) };
        let keysym = keys::base(keysym);
        let mods = keys::mod_bits(keys::altgr_free(k));
        if k.kind != KeyEventKind::Release {
            self.sync_mods(mods)?;
            self.held.insert(keysym);
            return self.sess.send(ToAgent::KeyState { keysym, down: true });
        }
        // A release reported as another symbol (Shift let go first, say) ends the key that is down.
        let keysym = if self.held.contains(&keysym) {
            keysym
        } else {
            match self.held.iter().copied().collect::<Vec<_>>()[..] {
                [only] => only,
                _ => return Ok(()),
            }
        };
        self.held.remove(&keysym);
        self.sess.send(ToAgent::KeyState { keysym, down: false })?;
        self.sync_mods(mods)
    }

    /// Presses or releases Shift, Ctrl, Alt and Super on the host to match `mods`.
    fn sync_mods(&mut self, mods: u8) -> io::Result<()> {
        for (bit, keysym) in keys::MODIFIERS {
            let want = mods & bit != 0;
            if want != (self.held_mods & bit != 0) {
                self.sess.send(ToAgent::KeyState { keysym, down: want })?;
                self.held_mods ^= bit;
            }
        }
        Ok(())
    }

    /// Modifiers still held at the host that are no longer down here: the Windows console reports
    /// no event for Shift, Ctrl or Alt alone, so their release is looked up.
    fn check_mods(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        if self.held_mods != 0 && self.probe.is_none() {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT};
            let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } < 0;
            let mut now = 0;
            for (bit, vk) in [(crate::proto::MOD_SHIFT, VK_SHIFT), (crate::proto::MOD_CTRL, VK_CONTROL), (crate::proto::MOD_ALT, VK_MENU), (MOD_SUPER, VK_LWIN)] {
                if down(vk) {
                    now |= bit;
                }
            }
            if now & self.held_mods != self.held_mods {
                self.sync_mods(now & self.held_mods)?;
            }
        }
        Ok(())
    }

    /// Lets go of everything held on the host (the menu, leaving).
    fn release_all(&mut self) -> io::Result<()> {
        for keysym in std::mem::take(&mut self.held) {
            self.sess.send(ToAgent::KeyState { keysym, down: false })?;
        }
        self.sync_mods(0)
    }

    /// Draws rows of text from the top, each cut or padded to the width; `style` per row.
    fn draw_rows(&mut self, rows: &[(String, &str)]) -> io::Result<()> {
        let mut out = String::from("\x1b[?2026h\x1b[0m");
        let height = self.rows.saturating_sub(1);
        for i in 0..height {
            let (text, style) = rows.get(i).map_or((String::new(), ""), |(t, s)| (t.clone(), *s));
            let shown = visible_len(&text);
            let text = cut_visible(&text, self.cols);
            let pad = self.cols.saturating_sub(shown.min(self.cols));
            out.push_str(&format!("\x1b[{};1H\x1b[0m{style}{text}{}\x1b[0m", i + 1, " ".repeat(pad)));
        }
        out.push_str("\x1b[?2026l");
        self.out.write_all(out.as_bytes())?;
        self.status()
    }

    /// The Ctrl+] menu: every action by its key, grouped as the window's toolbar and menus are.
    fn draw_menu(&mut self) -> io::Result<()> {
        let mark = |v: View| if self.view == v { "*" } else { " " };
        let quality = if self.ctl.auto.is_some() { "auto".to_string() } else { self.ctl.preset_label() };
        let chat = if self.unread > 0 { format!("  ({} new)", self.unread) } else { String::new() };
        let head = "\x1b[48;5;236;97m";
        let key = "\x1b[0m";
        let rows: Vec<(String, &str)> = vec![
            (format!(" sshremote  {}   (a key below, Esc: back)", self.label), head),
            (String::new(), key),
            (" Mode".into(), "\x1b[1m"),
            (format!("   s{} Screen sharing     t{} Terminal     f{} Files", mark(View::Screen), mark(View::Terminal), mark(View::Files)), key),
            (String::new(), key),
            (" Talk".into(), "\x1b[1m"),
            (format!("   c  Chat with the person at the screen{chat}"), key),
            (String::new(), key),
            (" Screen".into(), "\x1b[1m"),
            (format!("   0-4 quality, a automatic (now {quality})    m colours    + - scale    F frames per second"), key),
            ("   z  zoom to the terminal    n  next monitor    v  share a window or application".into(), key),
            ("   p  pause or resume    u  one frame    r  repaint    x  screenshot    o  sound".into(), key),
            ("   w  Win with the next key    k  Ctrl+Alt+Del    ]  send Ctrl+] to the host".into(), key),
            (String::new(), key),
            (" Files".into(), "\x1b[1m"),
            ("   g  get a remote file by its path    l  send a local file to Downloads    (f: browse)".into(), key),
            (String::new(), key),
            (" Leave".into(), "\x1b[1m"),
            ("   q  disconnect (asks first)".into(), key),
        ];
        self.draw_rows(&rows)
    }

    /// Covers the view with the menu, the chat or a question; in the terminal, on the alternate
    /// screen, so the shell's own screen comes back as it was.
    fn open_overlay(&mut self, o: Overlay) -> io::Result<()> {
        if self.view == View::Screen && self.overlay == Overlay::None {
            self.release_all()?;
        }
        if self.view == View::Terminal && self.overlay == Overlay::None && self.probe.is_none() {
            // A program on its own screen keeps it: the menu is drawn over, and it repaints after.
            if self.shell_modes.alt {
                let _ = execute!(self.out, cursor::Hide);
            } else {
                let _ = execute!(self.out, terminal::EnterAlternateScreen, cursor::Hide);
            }
        }
        self.overlay = o;
        self.redraw()
    }

    fn close_overlay(&mut self) -> io::Result<()> {
        self.overlay = Overlay::None;
        if self.view == View::Terminal {
            return self.show_shell();
        }
        self.redraw()
    }

    /// The shell's screen back as its program left it: the main screen (kept by the terminal) or,
    /// for a full-screen program, its own screen redrawn by it; its modes set again.
    fn show_shell(&mut self) -> io::Result<()> {
        if self.probe.is_some() {
            return Ok(());
        }
        if self.shell_modes.alt {
            write!(self.out, "\x1b[?1049h\x1b[0m\x1b[2J\x1b[H")?;
        } else {
            let _ = execute!(self.out, terminal::LeaveAlternateScreen);
        }
        let modes = self.shell_modes.restore();
        self.out.write_all(modes.as_bytes())?;
        self.out.flush()?;
        self.flush_backlog()?;
        if self.shell_modes.alt {
            self.nudge_shell()?;
        }
        Ok(())
    }

    /// A size change and back, so a full-screen program in the shell draws itself again.
    fn nudge_shell(&mut self) -> io::Result<()> {
        if let Some(id) = self.shell {
            let (cols, rows) = terminal::size().unwrap_or((80, 24));
            self.sess.send(ToAgent::TermResize { id, cols, rows: rows.saturating_sub(1).max(1) })?;
            self.sess.send(ToAgent::TermResize { id, cols, rows })?;
        }
        Ok(())
    }

    /// Draws what is in view again: the overlay, the files, or the picture from the copy kept.
    fn redraw(&mut self) -> io::Result<()> {
        if self.probe.is_some() {
            return Ok(());
        }
        match &self.overlay {
            Overlay::Menu => return self.draw_menu(),
            Overlay::Chat(_) => return self.draw_chat(),
            Overlay::Confirm(c) => return self.draw_confirm(*c),
            Overlay::None => {}
        }
        match self.view {
            View::Screen => {
                write!(self.out, "\x1b[0m\x1b[2J")?;
                if self.fb.w == 0 {
                    self.sess.send(ToAgent::Refresh)?;
                } else {
                    self.dirty = vec![true; self.cols * self.rows.saturating_sub(1)];
                    let (dw, dh) = self.display();
                    let mut out = Vec::new();
                    self.paint(&mut out, dw, dh);
                    self.out.write_all(&out)?;
                }
                self.status()
            }
            View::Files => self.draw_files(),
            View::Terminal => self.flush_backlog(),
        }
    }

    /// Goes to another mode on the same connection. The shell stays open behind the others and
    /// the picture pauses while it is not seen.
    fn set_view(&mut self, v: View) -> io::Result<()> {
        self.overlay = Overlay::None;
        let was = self.view;
        if v == was {
            return if v == View::Terminal { self.leave_to_shell() } else { self.redraw() };
        }
        self.view = v;
        if was == View::Screen {
            self.release_all()?;
            self.sess.send(ToAgent::Pause(true))?;
        }
        match v {
            View::Screen => {
                self.enter_drawing()?;
                self.sess.send(ToAgent::Pause(self.paused))?;
                self.sess.send(ToAgent::Refresh)?;
                self.redraw()
            }
            View::Files => {
                self.enter_drawing()?;
                if self.files.entries.is_empty() && !self.files.loading {
                    self.files.loading = true;
                    let path = self.files.path.clone();
                    self.sess.send(ToAgent::ListDir { path })?;
                }
                self.redraw()
            }
            View::Terminal => self.leave_to_shell(),
        }
    }

    /// The alternate screen with the mouse, for the picture, the files and the overlays.
    fn enter_drawing(&mut self) -> io::Result<()> {
        if self.probe.is_none() {
            let _ = execute!(self.out, terminal::EnterAlternateScreen, cursor::Hide, event::EnableMouseCapture);
        }
        Ok(())
    }

    /// The shell on the terminal's own screen, as typed into directly; one opens the first time.
    fn leave_to_shell(&mut self) -> io::Result<()> {
        if self.probe.is_some() {
            return Ok(());
        }
        let _ = execute!(self.out, event::DisableMouseCapture);
        self.show_shell()?;
        // What came before, then a new shell if the last one ended.
        if self.shell.is_none() {
            self.open_shell()?;
        }
        Ok(())
    }

    fn open_shell(&mut self) -> io::Result<()> {
        let id = self.next_shell;
        self.next_shell += 1;
        write!(self.out, "\x1b[0m\r\nsshremote: a shell on {}. Ctrl+] for the menu (screen, files, chat).\r\n", self.host)?;
        self.out.flush()?;
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        self.shell = Some(id);
        self.sess.send(ToAgent::TermOpen { id, cols, rows })
    }

    /// What the shell wrote while away, now that it shows.
    fn flush_backlog(&mut self) -> io::Result<()> {
        if self.view == View::Terminal && self.overlay == Overlay::None && !self.backlog.is_empty() {
            let b = std::mem::take(&mut self.backlog);
            self.out.write_all(&b)?;
            self.out.flush()?;
        }
        Ok(())
    }

    /// A key chosen in the menu.
    fn menu_key(&mut self, c: char) -> io::Result<()> {
        match c {
            's' => self.set_view(View::Screen),
            't' => self.set_view(View::Terminal),
            'f' => self.set_view(View::Files),
            'c' => {
                self.unread = 0;
                self.overlay = Overlay::Chat(String::new());
                self.redraw()
            }
            'q' => {
                self.overlay = Overlay::Confirm('q');
                self.redraw()
            }
            'k' => {
                self.close_overlay()?;
                self.sess.send(ToAgent::SecureAttention)?;
                self.notify("Sent Ctrl+Alt+Del")
            }
            'x' => {
                self.close_overlay()?;
                self.sess.send(ToAgent::Snapshot)?;
                self.notify("Taking a screenshot...")
            }
            'p' => {
                self.paused = !self.paused;
                self.close_overlay()?;
                if self.view == View::Screen {
                    self.sess.send(ToAgent::Pause(self.paused))?;
                }
                self.status()
            }
            'u' => {
                self.close_overlay()?;
                self.sess.send(ToAgent::Refresh)?;
                self.status()
            }
            'o' => {
                self.close_overlay()?;
                if self.player.take().is_some() {
                    self.sess.send(ToAgent::Audio(false))?;
                    return self.notify("Sound off");
                }
                match crate::audio::Player::new() {
                    Ok(p) => {
                        self.player = Some(p);
                        self.sess.send(ToAgent::Audio(true))?;
                        self.notify("Sound on")
                    }
                    Err(e) => self.notify(&format!("Sound: {e}")),
                }
            }
            'v' => {
                self.set_view(View::Screen)?;
                // Sharing part of the screen: the same key leads straight back to all of it.
                if self.sharing.0 != 0 {
                    self.sess.send(ToAgent::Share(Share::Screen))?;
                    return self.notify("Sharing the whole screen...");
                }
                self.share_list = None;
                self.sess.send(ToAgent::ListWindows)?;
                self.prompt = Some(('v', String::new()));
                self.draw_share_list()
            }
            'g' | 'l' => {
                self.close_overlay()?;
                if self.view == View::Terminal {
                    self.set_view(View::Files)?;
                }
                self.prompt = Some((c, String::new()));
                self.status()
            }
            // The screen's own keys: next monitor is n here (s is the screen mode), frames per
            // second F (f is the files).
            'n' => {
                self.close_overlay()?;
                let a = self.ctl.menu('s');
                self.run_action(a)
            }
            'F' => {
                self.close_overlay()?;
                let a = self.ctl.menu('f');
                self.run_action(a)
            }
            // In the terminal: Ctrl+] for the shell, and a repaint of its program.
            ']' if self.view == View::Terminal => {
                self.close_overlay()?;
                match self.shell {
                    Some(id) => self.sess.send(ToAgent::TermInput { id, data: vec![0x1d] }),
                    None => Ok(()),
                }
            }
            'r' if self.view == View::Terminal => {
                self.close_overlay()?;
                self.nudge_shell()
            }
            '0'..='4' | 'a' | 'm' | '+' | '=' | '-' | 'z' | 'r' | 'w' | ']' => {
                if matches!(c, 'z' | 'w') && self.view != View::Screen {
                    self.set_view(View::Screen)?;
                } else {
                    self.close_overlay()?;
                }
                let a = self.ctl.menu(c);
                self.run_action(a)
            }
            _ => self.close_overlay(),
        }
    }

    /// A question over the view: y says yes.
    fn draw_confirm(&mut self, what: char) -> io::Result<()> {
        let (title, body) = match what {
            'q' => (format!("Disconnect from {}?", self.host), "The screen, the shell and transfers end.".to_string()),
            _ => {
                let name = clean(self.target.as_deref().unwrap_or(""));
                (format!("Delete {name} on {}?", self.host), "It cannot be brought back; a folder goes with everything in it.".to_string())
            }
        };
        let rows: Vec<(String, &str)> = vec![
            (String::new(), ""),
            (format!("   {title}"), "\x1b[1m"),
            (format!("   {body}"), ""),
            (String::new(), ""),
            ("   y: yes      any other key: no".into(), "\x1b[33m"),
        ];
        self.draw_rows(&rows)
    }

    fn confirm_key(&mut self, what: char, k: &event::KeyEvent) -> io::Result<()> {
        let yes = matches!(k.code, event::KeyCode::Char('y' | 'Y'));
        self.overlay = Overlay::None;
        match (what, yes) {
            ('q', true) => {
                self.quit = true;
                Ok(())
            }
            ('x', true) => {
                let Some(path) = self.target.take() else { return self.redraw() };
                self.sess.send(ToAgent::FileOp { op: crate::proto::FileOp::Delete, path, arg: String::new() })?;
                self.redraw()
            }
            _ => {
                if self.view == View::Terminal {
                    return self.close_overlay();
                }
                self.redraw()
            }
        }
    }

    /// The conversation, newest at the bottom, as Slack shows it: who and when, then the text.
    fn draw_chat(&mut self) -> io::Result<()> {
        let Overlay::Chat(buf) = &self.overlay else { return Ok(()) };
        let buf = buf.clone();
        let height = self.rows.saturating_sub(1);
        let width = self.cols.saturating_sub(4).max(10);
        let mut body: Vec<(String, &str)> = Vec::new();
        for (theirs, text, at) in crate::chat::load_history(&self.host) {
            let peer = clean(&self.peer.get());
            let who = if theirs { peer.as_str() } else { "You" };
            let when = if at == 0 { String::new() } else { format!("  {}", crate::chat::when(at)) };
            body.push((format!("  {who}\x1b[22;2m{when}"), "\x1b[1m"));
            for line in wrap(&clean(&text), width) {
                body.push((format!("  {line}"), ""));
            }
            body.push((String::new(), ""));
        }
        let room = height.saturating_sub(3);
        let start = body.len().saturating_sub(room);
        let mut rows: Vec<(String, &str)> =
            vec![(format!(" Chat with the person at {}'s screen   (Enter: send, Esc: back)", self.host), "\x1b[48;5;236;97m")];
        rows.extend(body.drain(start..));
        while rows.len() < height - 1 {
            rows.push((String::new(), ""));
        }
        rows.push((format!(" > {buf}_"), "\x1b[48;5;238;97m"));
        self.draw_rows(&rows)
    }

    fn chat_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::KeyCode;
        let Overlay::Chat(buf) = &mut self.overlay else { return Ok(()) };
        match k.code {
            KeyCode::Esc => return self.close_overlay(),
            KeyCode::Enter => {
                let text = buf.trim().to_string();
                buf.clear();
                if !text.is_empty() {
                    self.sess.send(ToAgent::Chat(text.clone()))?;
                    crate::chat::append_history(&self.host, false, &text);
                }
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) if !k.modifiers.contains(event::KeyModifiers::CONTROL) => buf.push(c),
            _ => return Ok(()),
        }
        self.draw_chat()
    }

    /// The remote folder: a header, the entries with the selection, keys at the foot.
    fn draw_files(&mut self) -> io::Result<()> {
        let height = self.rows.saturating_sub(1);
        let list_h = height.saturating_sub(4).max(1);
        let shown: Vec<(String, bool, u64, u64)> =
            self.files.shown().iter().map(|e| (clean(&e.name), e.dir, e.size, e.mtime)).collect();
        if self.files.sel >= shown.len() {
            self.files.sel = shown.len().saturating_sub(1);
        }
        if self.files.sel < self.files.top {
            self.files.top = self.files.sel;
        } else if self.files.sel >= self.files.top + list_h {
            self.files.top = self.files.sel + 1 - list_h;
        }
        let place = if self.files.path == crate::files::DRIVES { "Drives".to_string() } else { clean(&self.files.path) };
        let mut rows: Vec<(String, &str)> = vec![(format!(" Files on {}:  {place}", self.host), "\x1b[48;5;236;97m")];
        let date_w = 17;
        let size_w = 10;
        let name_w = self.cols.saturating_sub(date_w + size_w + 6).max(10);
        rows.push((format!("   {:<name_w$} {:>size_w$}  {}", "Name", "Size", "Modified"), "\x1b[2m"));
        if self.files.loading {
            rows.push(("   Loading...".into(), "\x1b[2m"));
        } else if !self.files.error.is_empty() {
            rows.push((format!("   {}", clean(&self.files.error)), "\x1b[31m"));
        } else if shown.is_empty() {
            rows.push(("   (empty)".into(), "\x1b[2m"));
        }
        for (i, (name, dir, size, mtime)) in shown.iter().enumerate().skip(self.files.top).take(list_h) {
            let mark = if *dir { "/" } else { " " };
            let name: String = format!("{name}{mark}");
            let name = cut_visible(&name, name_w);
            let pad = name_w.saturating_sub(visible_len(&name));
            let size = if *dir { String::new() } else { crate::gui::human(*size) };
            let when = crate::files::local_time(*mtime);
            let style = if i == self.files.sel { "\x1b[7m" } else if *dir { "\x1b[34;1m" } else { "" };
            rows.push((format!("   {name}{} {size:>size_w$}  {when}", " ".repeat(pad)), style));
        }
        while rows.len() < height - 2 {
            rows.push((String::new(), ""));
        }
        rows.push((format!(" Downloads go to {}   (o: open that folder)", crate::files::downloads().display()), "\x1b[36m"));
        rows.push((
            " Enter open folder / get file  d get file or folder  Backspace up  ~ home  u send here  n new folder  F2 rename  Del delete  . hidden  F5 reload".into(),
            "\x1b[2m",
        ));
        self.draw_rows(&rows)
    }

    /// The status line in the files view: the selection, and the transfers going on.
    fn files_line(&self) -> String {
        let sel = self.files.shown().get(self.files.sel).map(|e| {
            let what = if e.dir { "folder".to_string() } else { crate::gui::human(e.size) };
            let when = crate::files::local_time(e.mtime);
            if when.is_empty() { format!("{}  {what}", e.name) } else { format!("{}  {what}  {when}", e.name) }
        });
        let moving = if self.transfers.is_empty() {
            String::new()
        } else {
            let done: u64 = self.transfers.iter().map(|t| t.done).sum();
            let size: u64 = self.transfers.iter().map(|t| t.size).sum::<u64>().max(1);
            let pct = (done * 100 / size).min(100) as usize;
            let bar = format!("[{}{}]", "#".repeat(pct / 10), ".".repeat(10 - pct / 10));
            let first = &self.transfers[0];
            let arrow = if first.upload { "up" } else { "down" };
            format!(" | {arrow} {} {bar} {pct}% ({} files)", first.name, self.transfers.len())
        };
        format!("{}{moving}", sel.unwrap_or_else(|| format!("{} items", self.files.shown().len())))
    }

    fn files_go(&mut self, path: String) -> io::Result<()> {
        self.files.loading = true;
        self.files.error.clear();
        self.sess.send(ToAgent::ListDir { path })?;
        if self.overlay == Overlay::None && self.prompt.is_none() {
            self.draw_files()?;
        }
        Ok(())
    }

    fn files_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::KeyCode;
        let n = self.files.shown().len();
        let page = self.rows.saturating_sub(6).max(1);
        let picked = self.files.shown().get(self.files.sel).map(|e| (e.name.clone(), e.dir));
        match k.code {
            KeyCode::Up => self.files.sel = self.files.sel.saturating_sub(1),
            KeyCode::Down => self.files.sel = (self.files.sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.files.sel = self.files.sel.saturating_sub(page),
            KeyCode::PageDown => self.files.sel = (self.files.sel + page).min(n.saturating_sub(1)),
            KeyCode::Home => self.files.sel = 0,
            KeyCode::End => self.files.sel = n.saturating_sub(1),
            KeyCode::Enter | KeyCode::Right => match picked {
                Some((name, true)) => {
                    let path = self.files.child(&name);
                    self.files.sel = 0;
                    return self.files_go(path);
                }
                Some((name, false)) if k.code == KeyCode::Enter => {
                    let path = self.files.child(&name);
                    self.sess.send(ToAgent::Fetch { tag: 0, path })?;
                    return self.notify(&format!("Downloading {name} to {}", crate::files::downloads().display()));
                }
                _ => {}
            },
            KeyCode::Backspace | KeyCode::Left => {
                let path = self.files.parent();
                self.files.sel = 0;
                return self.files_go(path);
            }
            KeyCode::Char('~') => {
                self.files.sel = 0;
                return self.files_go(String::new());
            }
            KeyCode::Char('d') => {
                if let Some((name, _)) = picked {
                    let path = self.files.child(&name);
                    self.sess.send(ToAgent::Fetch { tag: 0, path })?;
                    return self.notify(&format!("Downloading {name} to {}", crate::files::downloads().display()));
                }
            }
            KeyCode::Char('.') => self.files.hidden = !self.files.hidden,
            KeyCode::Char('o') => {
                let dir = crate::files::downloads();
                let _ = std::fs::create_dir_all(&dir);
                crate::tools::open_path(&dir);
                return self.notify(&format!("Opened {}", dir.display()));
            }
            KeyCode::F(5) => {
                let path = self.files.path.clone();
                return self.files_go(path);
            }
            KeyCode::Char('u') => {
                self.prompt = Some(('u', String::new()));
                return self.status();
            }
            KeyCode::Char('n') if self.files.path != crate::files::DRIVES => {
                self.prompt = Some(('n', String::new()));
                return self.status();
            }
            KeyCode::F(2) | KeyCode::Char('r') if self.files.path != crate::files::DRIVES => {
                if let Some((name, _)) = picked {
                    self.target = Some(self.files.child(&name));
                    self.prompt = Some(('r', name));
                    return self.status();
                }
            }
            KeyCode::Delete if self.files.path != crate::files::DRIVES => {
                if let Some((name, _)) = picked {
                    self.target = Some(self.files.child(&name));
                    self.overlay = Overlay::Confirm('x');
                    return self.redraw();
                }
            }
            _ => return Ok(()),
        }
        self.draw_files()
    }

    /// Keys typed while the shell is shown, as the bytes a terminal sends.
    fn shell_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::{KeyCode, KeyModifiers};
        if keys::is_menu(k) {
            return self.open_overlay(Overlay::Menu);
        }
        let Some(id) = self.shell else {
            if k.code == KeyCode::Enter {
                return self.open_shell();
            }
            return Ok(());
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let mut b: Vec<u8> = match k.code {
            KeyCode::Char(c) if ctrl && c.is_ascii_alphabetic() => vec![c.to_ascii_lowercase() as u8 - b'a' + 1],
            KeyCode::Char(' ') if ctrl => vec![0],
            KeyCode::Char(c) => c.to_string().into_bytes(),
            KeyCode::Enter => b"\r".to_vec(),
            KeyCode::Backspace => b"\x7f".to_vec(),
            KeyCode::Tab => b"\t".to_vec(),
            KeyCode::BackTab => b"\x1b[Z".to_vec(),
            KeyCode::Esc => b"\x1b".to_vec(),
            KeyCode::Up => b"\x1b[A".to_vec(),
            KeyCode::Down => b"\x1b[B".to_vec(),
            KeyCode::Right => b"\x1b[C".to_vec(),
            KeyCode::Left => b"\x1b[D".to_vec(),
            KeyCode::Home => b"\x1b[H".to_vec(),
            KeyCode::End => b"\x1b[F".to_vec(),
            KeyCode::Insert => b"\x1b[2~".to_vec(),
            KeyCode::Delete => b"\x1b[3~".to_vec(),
            KeyCode::PageUp => b"\x1b[5~".to_vec(),
            KeyCode::PageDown => b"\x1b[6~".to_vec(),
            KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
            KeyCode::F(n @ 5..=12) => {
                let code = [15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5];
                format!("\x1b[{code}~").into_bytes()
            }
            _ => return Ok(()),
        };
        if alt && !ctrl {
            b.insert(0, 0x1b);
        }
        self.sess.send(ToAgent::TermInput { id, data: b })
    }

    fn prompt_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::KeyCode;
        let Some((kind, buf)) = self.prompt.as_mut() else { return Ok(()) };
        match k.code {
            KeyCode::Char(c) if !k.modifiers.contains(event::KeyModifiers::CONTROL) => buf.push(c),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Esc => {
                if matches!(self.prompt, Some(('v', _))) {
                    self.prompt = None;
                    return self.close_share_list().and_then(|_| self.status());
                }
                self.prompt = None;
                return self.redraw();
            }
            KeyCode::Enter => {
                let (kind, text) = (*kind, buf.trim().to_string());
                self.prompt = None;
                if !text.is_empty() || kind == 'v' {
                    return self.run_prompt(kind, text);
                }
                return self.redraw();
            }
            _ => {}
        }
        self.status()
    }

    /// The share chooser drawn over the picture: what can be shared, by number.
    fn draw_share_list(&mut self) -> io::Result<()> {
        let mut lines = vec![
            " Share: type a number, a, p or nothing, then Enter (Esc cancels) ".to_string(),
            "   Enter  the whole screen".to_string(),
            "   a      the active window".to_string(),
            "   p      the active application (every window of it)".to_string(),
        ];
        match &self.share_list {
            None => lines.push("   ...asking the SSH host for its windows".into()),
            Some(Err(e)) => lines.push(format!("   ({e})")),
            Some(Ok(list)) => {
                for (i, w) in list.iter().enumerate() {
                    lines.push(format!("   {:<6} {}: {}  {}x{}", i + 1, clean(&w.app), clean(&w.title), w.w, w.h));
                }
                if !list.is_empty() {
                    lines.push(
                        "   A number shares that window; A and a number (A3) shares its whole application.".into(),
                    );
                }
            }
        }
        let width = self.cols.min(100);
        let mut out = String::new();
        for (i, l) in lines.iter().take(self.rows.saturating_sub(2)).enumerate() {
            let l: String = l.chars().take(width).collect();
            let pad = width.saturating_sub(l.chars().count());
            out.push_str(&format!("\x1b[{};1H\x1b[0;48;5;236;97m{l}{}\x1b[0m", i + 1, " ".repeat(pad)));
        }
        self.out.write_all(out.as_bytes())?;
        self.status()
    }

    /// Ends the chooser: the picture under the list is painted again.
    fn close_share_list(&mut self) -> io::Result<()> {
        self.share_list = None;
        self.dirty.clear();
        self.full_redraw()?;
        self.sess.send(ToAgent::Refresh)
    }

    fn run_prompt(&mut self, kind: char, text: String) -> io::Result<()> {
        match kind {
            'v' => {
                let t = text.trim().to_string();
                let list = match &self.share_list {
                    Some(Ok(l)) => l.clone(),
                    _ => Vec::new(),
                };
                let pick = |n: &str| n.parse::<usize>().ok().and_then(|i| list.get(i.wrapping_sub(1))).map(|w| w.id);
                let share = match t.as_str() {
                    "" => Share::Screen,
                    "a" => Share::ActiveWindow,
                    "p" => Share::ActiveApp,
                    n if n.starts_with(['A', 'P']) => match pick(&n[1..]) {
                        Some(id) => Share::App(id),
                        None => return self.close_share_list().and_then(|_| self.notify(&format!("No window {n}"))),
                    },
                    n => match pick(n) {
                        Some(id) => Share::Window(id),
                        None => return self.close_share_list().and_then(|_| self.notify(&format!("No window {n}"))),
                    },
                };
                self.close_share_list()?;
                self.sess.send(ToAgent::Share(share))
            }
            'u' => {
                let dir = self.files.path.clone();
                self.upload_to(&text, &dir)
            }
            'n' => {
                let path = self.files.child(&text);
                self.sess.send(ToAgent::FileOp { op: crate::proto::FileOp::Mkdir, path, arg: String::new() })?;
                self.redraw()
            }
            'r' => {
                let Some(path) = self.target.take() else { return self.redraw() };
                self.sess.send(ToAgent::FileOp { op: crate::proto::FileOp::Rename, path, arg: text })?;
                self.redraw()
            }
            'g' => {
                self.sess.send(ToAgent::Fetch { tag: 0, path: text.clone() })?;
                self.notify(&format!("Downloading {text}..."))
            }
            _ => self.upload(&text),
        }
    }

    /// Sends a local file to the remote Downloads folder; the link paces the writes.
    fn upload(&mut self, path: &str) -> io::Result<()> {
        self.upload_to(path, "")
    }

    /// Sends a local file into the remote folder `dir` (empty: Downloads), a piece at a time as
    /// the loop comes round, so the view stays live and the progress true.
    fn upload_to(&mut self, path: &str, dir: &str) -> io::Result<()> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => return self.notify(&format!("{path}: {e}")),
        };
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let name = std::path::Path::new(path).file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned());
        let id = self.next_upload;
        self.next_upload += 1;
        self.sess.send(ToAgent::FileStart { id, size, dir: dir.to_string(), name: name.clone() })?;
        self.uploads.push((id, file, size, 0));
        self.transfers.push(Transfer { id, name: name.clone(), done: 0, size, upload: true });
        self.notify(&format!("Sending {name}..."))
    }

    /// The next pieces of the uploads under way; true while some remain.
    fn pump_uploads(&mut self) -> io::Result<bool> {
        use std::io::Read;
        let mut budget: usize = 1 << 20;
        let mut finished = Vec::new();
        for (id, file, _, sent) in &mut self.uploads {
            let mut buf = vec![0u8; 64 << 10];
            while budget > 0 {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    finished.push(*id);
                    break;
                }
                self.sess.send(ToAgent::FileData { id: *id, data: buf[..n].to_vec() })?;
                *sent += n as u64;
                budget = budget.saturating_sub(n);
            }
            if let Some(t) = self.transfers.iter_mut().find(|t| t.upload && t.id == *id) {
                t.done = *sent;
            }
        }
        for id in finished {
            self.sess.send(ToAgent::FileEnd { id })?;
            self.uploads.retain(|u| u.0 != id);
        }
        if self.view == View::Files && self.overlay == Overlay::None && self.prompt.is_none() {
            self.status()?;
        }
        Ok(!self.uploads.is_empty())
    }

    /// Messages other than frames.
    fn on_message(&mut self, m: ToClient) -> io::Result<()> {
        match m {
            ToClient::Pong(t) => self.ctl.pong(t),
            ToClient::Audio(p) => {
                self.audio_bytes += p.len();
                if let Some(pl) = self.player.as_mut() {
                    pl.push(&p);
                }
            }
            ToClient::Notice(t) => self.notify(&t)?,
            ToClient::Chat(t) => {
                crate::chat::append_history(&self.host, true, &t);
                if matches!(self.overlay, Overlay::Chat(_)) {
                    self.draw_chat()?;
                } else {
                    // A bell, so a message is noticed while looking elsewhere.
                    self.unread += 1;
                    if self.probe.is_none() {
                        self.out.write_all(b"\x07")?;
                    }
                    if self.view != View::Terminal || self.overlay != Overlay::None {
                        self.notify(&format!("{}: {t}  (Ctrl+] c to answer)", self.peer.get()))?;
                    }
                }
            }
            ToClient::Snapshot(png) => {
                let path = crate::shot::save_path();
                match std::fs::write(&path, &png) {
                    Ok(()) => self.notify(&format!("Screenshot saved to {}", path.display()))?,
                    Err(e) => self.notify(&format!("Screenshot: {e}"))?,
                }
            }
            ToClient::Monitors { list, current } => {
                if self.probe.is_some() {
                    let names: Vec<String> = list.iter().map(|m| format!("{} {}x{}", m.name, m.w, m.h)).collect();
                    eprintln!("probe: monitors {names:?}, showing {current}");
                }
                self.monitors = (list.len(), current as usize);
            }
            ToClient::Sharing { kind, text } => {
                self.notify(&format!("Sharing {text}"))?;
                self.sharing = (kind, text);
            }
            ToClient::WindowList { list, error } => {
                self.share_list = Some(if error.is_empty() { Ok(list) } else { Err(error) });
                if matches!(self.prompt, Some(('v', _))) {
                    self.draw_share_list()?;
                }
            }
            ToClient::TermOutput { id, data } if self.shell == Some(id) => {
                self.shell_modes.follow(&data);
                if self.view == View::Terminal && self.overlay == Overlay::None {
                    self.out.write_all(&data)?;
                    self.out.flush()?;
                } else {
                    // Kept for when the shell is shown again; the newest few megabytes, cut at
                    // a line so no sequence or character is split.
                    self.backlog.extend_from_slice(&data);
                    if self.backlog.len() > 4 << 20 {
                        let from = self.backlog.len() - (2 << 20);
                        let cut = self.backlog[from..].iter().position(|&b| b == b'\n').map_or(from, |i| from + i + 1);
                        self.backlog.drain(..cut);
                    }
                }
            }
            ToClient::TermExit { id, text } if self.shell == Some(id) => {
                self.shell = None;
                let line = format!("\r\n\x1b[0msshremote: the shell ended ({text}). Enter: a new one, Ctrl+]: the menu.\r\n");
                if self.view == View::Terminal && self.overlay == Overlay::None {
                    self.out.write_all(line.as_bytes())?;
                    self.out.flush()?;
                } else {
                    self.backlog.extend_from_slice(line.as_bytes());
                }
            }
            ToClient::DirList { path, entries, error } => {
                self.files.loading = false;
                self.files.path = path;
                self.files.entries = entries;
                self.files.error = error;
                if self.view == View::Files && self.overlay == Overlay::None && self.prompt.is_none() {
                    self.draw_files()?;
                }
            }
            ToClient::OpDone { ok, text } => {
                self.notify(&if ok { text } else { format!("Failed: {text}") })?;
                if self.view == View::Files {
                    let path = self.files.path.clone();
                    self.files_go(path)?;
                }
            }
            ToClient::Clipboard(text) if self.probe.is_none() && text.len() < 100_000 => {
                // OSC 52 puts it on this computer's clipboard, also through ssh and tmux.
                write!(self.out, "\x1b]52;c;{}\x07", crate::shot::base64(text.as_bytes()))?;
                self.out.flush()?;
            }
            ToClient::FileDone { id, ok, text } => {
                self.transfers.retain(|t| !(t.upload && t.id == id));
                let t = if ok { format!("Sent: {text}") } else { format!("Send failed: {text}") };
                self.notify(&t)?;
                if ok && self.view == View::Files {
                    let path = self.files.path.clone();
                    self.files_go(path)?;
                }
            }
            ToClient::FetchStart { id, size, name, .. } => {
                if let Some(crate::files::Note::Done { text, .. }) = self.inbox.start(id, size, &name) {
                    self.notify(&format!("Download of {name} failed: {text}"))?;
                } else {
                    self.transfers.push(Transfer { id, name, done: 0, size, upload: false });
                }
            }
            ToClient::FetchData { id, data } => {
                if let Some(crate::files::Note::Progress { id, received }) = self.inbox.data(id, &data) {
                    if let Some(t) = self.transfers.iter_mut().find(|t| !t.upload && t.id == id) {
                        t.done = received;
                    }
                    self.sess.send(ToAgent::FetchAck { id, received })?;
                    if self.view == View::Files {
                        self.status()?;
                    }
                }
            }
            ToClient::FetchEnd { id, ok, text } => {
                self.transfers.retain(|t| t.upload || t.id != id);
                let note = self.inbox.end(id, ok, &text);
                match note {
                Some(crate::files::Note::Done { ok, text, .. }) => {
                    let t = if ok { format!("Saved {text}") } else { format!("Download failed: {text}") };
                    self.notify(&t)?
                }
                _ => self.notify(&format!("Download failed: {text}"))?,
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_term(&mut self, e: Event) -> io::Result<()> {
        let screen_input = self.view == View::Screen && self.overlay == Overlay::None && self.prompt.is_none();
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Release && !(screen_input && self.key_states) => {}
            // Shift, Ctrl or Alt alone only matter for keys held at the host.
            Event::Key(k) if matches!(k.code, event::KeyCode::Modifier(_)) && !(screen_input && self.key_states) => {}
            Event::Key(k) if self.overlay != Overlay::None => match self.overlay.clone() {
                Overlay::Confirm(c) => self.confirm_key(c, &k)?,
                Overlay::Chat(_) => self.chat_key(&k)?,
                _ => {
                    // The menu: a key, or Esc (or Ctrl+] again) back.
                    let c = match k.code {
                        event::KeyCode::Char(c) if !keys::is_menu(&k) => c,
                        _ => '\0',
                    };
                    self.menu_key(c)?
                }
            },
            Event::Key(k) if self.prompt.is_some() => self.prompt_key(&k)?,
            Event::Key(k) if keys::is_menu(&k) => self.open_overlay(Overlay::Menu)?,
            Event::Key(k) if self.view == View::Terminal => self.shell_key(&k)?,
            Event::Key(k) if self.view == View::Files => self.files_key(&k)?,
            Event::Paste(text) => {
                if let Some((_, buf)) = self.prompt.as_mut() {
                    buf.push_str(text.trim());
                    self.status()?;
                } else if let Overlay::Chat(buf) = &mut self.overlay {
                    buf.push_str(&text);
                    self.draw_chat()?;
                } else if self.view == View::Terminal {
                    if let Some(id) = self.shell {
                        self.sess.send(ToAgent::TermInput { id, data: text.into_bytes() })?;
                    }
                } else if self.view == View::Screen {
                    // As in RustDesk: the text goes to the host's clipboard and is pasted there.
                    self.sess.send(ToAgent::Clipboard(text))?;
                    self.sess.send(ToAgent::Key { keysym: 'v' as u32, mods: crate::proto::MOD_CTRL })?;
                    self.notify("Pasted into the host")?;
                }
            }
            Event::Resize(cols, rows) => {
                self.measure();
                if let Some(id) = self.shell {
                    self.sess.send(ToAgent::TermResize { id, cols, rows })?;
                }
                if self.view == View::Screen {
                    self.dirty.clear();
                    self.fb = Framebuffer::default();
                    self.push_settings()?;
                    self.sess.send(ToAgent::Refresh)?;
                }
                if self.prompt.is_some() && matches!(self.prompt, Some(('v', _))) {
                    self.draw_share_list()?;
                } else if self.view != View::Terminal || self.overlay != Overlay::None {
                    self.redraw()?;
                }
            }
            Event::Key(k)
                if self.key_states
                    && !self.win_next
                    && !keys::is_menu(&k)
                    && !matches!(k.code, event::KeyCode::Modifier(_)) =>
            {
                self.key_state(&k)?;
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if matches!(k.code, event::KeyCode::Modifier(_)) {
                    return Ok(());
                }
                if let Some((keysym, mods)) = keys::keysym(&k) {
                    let m = if std::mem::take(&mut self.win_next) {
                        self.status()?;
                        if keysym == ' ' as u32 {
                            return self.sess.send(ToAgent::Key { keysym: SUPER_L, mods: 0 });
                        }
                        mods | MOD_SUPER
                    } else {
                        mods
                    };
                    self.sess.send(ToAgent::Key { keysym, mods: m })?;
                }
            }
            Event::Mouse(m) if screen_input => {
                // Shift+click and Ctrl+drag: the modifiers held here are held on the host too.
                self.sync_mods(keys::mod_bits(m.modifiers))?;
                let (dw, dh) = self.display();
                let dx = m.column as usize * self.cw + self.cw / 2;
                let dy = m.row as usize * self.ch + self.ch / 2;
                if dw == 0 || dx >= dw || dy >= dh {
                    return Ok(());
                }
                let p = ((dx * self.fb.w / dw) as u16, (dy * self.fb.h / dh) as u16);
                if p != self.pointer || !matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
                    self.pointer = p;
                    self.sess.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                }
                let button = |b: MouseButton| match b {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                };
                match m.kind {
                    MouseEventKind::Down(b) => self.sess.send(ToAgent::Button { button: button(b), pressed: true })?,
                    MouseEventKind::Up(b) => self.sess.send(ToAgent::Button { button: button(b), pressed: false })?,
                    MouseEventKind::ScrollDown => self.sess.send(ToAgent::Scroll { steps: 1 })?,
                    MouseEventKind::ScrollUp => self.sess.send(ToAgent::Scroll { steps: -1 })?,
                    _ => {}
                }
            }
            Event::Mouse(m) if self.view == View::Files && self.overlay == Overlay::None => {
                // The wheel moves through the list.
                let k = match m.kind {
                    MouseEventKind::ScrollDown => event::KeyCode::Down,
                    MouseEventKind::ScrollUp => event::KeyCode::Up,
                    _ => return Ok(()),
                };
                self.files_key(&event::KeyEvent::new(k, event::KeyModifiers::NONE))?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Text from the other side (names, messages, errors) made safe to print: control characters,
/// escape sequences above all, become visible marks instead of commands to this terminal.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() && c != '\t' { '\u{fffd}' } else { c }).collect()
}

/// Text length on screen, without the escape sequences that style it.
fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c.is_ascii_alphabetic() {
                esc = false;
            }
        } else if c == '\x1b' {
            esc = true;
        } else {
            n += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    n
}

/// `s` cut to `width` columns on screen, its escape sequences kept.
fn cut_visible(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c);
            if c.is_ascii_alphabetic() {
                esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            esc = true;
            out.push(c);
            continue;
        }
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if n + w > width {
            break;
        }
        n += w;
        out.push(c);
    }
    out
}

/// A message broken into lines of at most `width` columns, at spaces where it can be.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let extra = if line.is_empty() { 0 } else { 1 };
            if !line.is_empty() && visible_len(&line) + extra + visible_len(word) > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            while visible_len(&line) > width {
                let head = cut_visible(&line, width);
                line = line[head.len()..].to_string();
                lines.push(head);
            }
        }
        lines.push(line);
    }
    lines
}

impl Viewer {
    fn new(opt: &Options, sess: Session, out: Box<dyn Write>) -> Viewer {
    let host = opt.host.clone().unwrap_or_else(|| "local".into());
    Viewer {
        tmux: None,
        key_states: false,
        held: Default::default(),
        held_mods: 0,
        out,
        probe: opt.probe.as_ref().map(|p| p.1),
        paint_bytes: 0,
        label: format!("{host} {}x{} {}", sess.desk.0, sess.desk.1, sess.backend),
        sess,
        ctl: Control::new(opt, Mode::True),
        cols: 0,
        rows: 0,
        cw: 10,
        ch: 20,
        cell_override: opt.cell,
        cell_queried: None,
        fb: Framebuffer::default(),
        lut_mode: None,
        lut: Lut::new(&[[0, 0, 0]]),
        pal: Vec::new(),
        sixel: Sixel::new(),
        idx: Vec::new(),
        px: Vec::new(),
        qpal: Vec::new(),
        dirty: Vec::new(),
        view: View::Screen,
        overlay: Overlay::None,
        backlog: Vec::new(),
        shell_modes: ShellModes::default(),
        uploads: Vec::new(),
        target: None,
        files: RemoteFiles::default(),
        transfers: Vec::new(),
        unread: 0,
        peer: crate::chat::Peer::new(&host),
        host: host.clone(),
        win_next: false,
        player: None,
        inbox: crate::files::Inbox::default(),
        note: None,
        prompt: None,
        paused: false,
        next_upload: 1,
        audio_bytes: 0,
        monitors: (1, 0),
        // A viewer run inside an ssh session sends its sixel output over that session.
        stretch: !(opt.local && std::env::var_os("SSH_CONNECTION").is_some()),
        pointer: (u16::MAX, u16::MAX),
        quit: false,
        paint_ms: 0.0,
        shell: None,
        next_shell: 1,
        sharing: (0, String::new()),
        share_list: None,
    }
    }
}

pub fn run(opt: Options) -> Result<(), String> {
    if opt.gui {
        return crate::gui::run(&opt);
    }
    let (sess, tx, rx) = Session::connect(&opt)?;
    let mut v = Viewer::new(&opt, sess, if opt.probe.is_some() { Box::new(io::sink()) } else { Box::new(io::stdout()) });
    let interactive = opt.probe.is_none();
    let deadline = opt.probe.as_ref().map(|p| Instant::now() + Duration::from_secs_f64(p.0));
    if interactive {
        #[cfg(windows)]
        if windows_ssh_session() {
            // Write past the pseudo console, which would drop the images.
            v.out = Box::new(crate::conpty::raw_output().map_err(|e| {
                format!(
                    "{WINDOWS_SSH}
({e})"
                )
            })?);
        }
        terminal::enable_raw_mode().map_err(|e| e.to_string())?;
        v.tmux = tmux_passthrough();
        if opt.cell.is_none() {
            v.cell_queried = query_cell();
        }
        let _ = execute!(
            v.out,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            event::EnableMouseCapture,
            event::EnableBracketedPaste
        );
        // Key releases: the Windows console always reports them; elsewhere the kitty keyboard protocol
        // (kitty, WezTerm, Ghostty, foot) is asked for.
        v.key_states = cfg!(windows);
        #[cfg(unix)]
        if terminal::supports_keyboard_enhancement().unwrap_or(false) {
            use event::KeyboardEnhancementFlags as F;
            let flags = F::DISAMBIGUATE_ESCAPE_CODES
                | F::REPORT_EVENT_TYPES
                | F::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | F::REPORT_ALTERNATE_KEYS;
            v.key_states = execute!(v.out, event::PushKeyboardEnhancementFlags(flags)).is_ok();
        }
        #[cfg(windows)]
        {
            use crossterm::Command;
            // The console itself must also deliver mouse records, not only the client terminal.
            let _ = event::EnableMouseCapture.execute_winapi();
        }
        thread::spawn(move || {
            while let Ok(e) = event::read() {
                if tx.send(Ev::Term(e)).is_err() {
                    break;
                }
            }
        });
    }
    v.measure();
    let mut failure = None;
    let mut typing: Vec<ToAgent> = opt
        .probe_type
        .chars()
        .map(|c| ToAgent::Key { keysym: c as u32, mods: 0 })
        .chain((0..opt.probe_scroll).map(|_| ToAgent::Scroll { steps: 1 }))
        .rev()
        .collect();
    let keys_total = typing.len();
    let mut next_key = Instant::now() + Duration::from_secs(2);
    let mut type_start = (0u64, 0u32);
    let r = (|| -> io::Result<()> {
        v.push_settings()?;
        if opt.sound {
            match crate::audio::Player::new() {
                Ok(p) => v.player = Some(p),
                Err(e) => eprintln!("sshremote: sound: {e}"),
            }
            v.sess.send(ToAgent::Audio(true))?;
        }
        if let Some(m) = opt.probe_monitor {
            v.sess.send(ToAgent::SelectMonitor(m))?;
        }
        if let Some(path) = &opt.probe_fetch {
            v.sess.send(ToAgent::Fetch { tag: 0, path: path.clone() })?;
        }
        if let Some(path) = &opt.probe_send {
            let data = std::fs::read(path)?;
            let name =
                std::path::Path::new(path).file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned());
            v.sess.send(ToAgent::FileStart { id: 1, size: data.len() as u64, dir: String::new(), name })?;
            for chunk in data.chunks(64 << 10) {
                v.sess.send(ToAgent::FileData { id: 1, data: chunk.to_vec() })?;
            }
            v.sess.send(ToAgent::FileEnd { id: 1 })?;
        }
        while !v.quit && deadline.is_none_or(|d| Instant::now() < d) {
            // Quick turns while files are being sent, so they go at the link's pace.
            let wait = if v.uploads.is_empty() { 250 } else { 2 };
            match rx.recv_timeout(Duration::from_millis(wait)) {
                Ok(Ev::Net(ToClient::Frame(f))) => v.apply(f)?,
                Ok(Ev::Net(ToClient::Error(e)) | Ev::Closed(e)) => {
                    failure = Some(e);
                    break;
                }
                Ok(Ev::Net(m)) => v.on_message(m)?,
                Ok(Ev::Term(e)) => v.on_term(e)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            if !typing.is_empty() && Instant::now() >= next_key {
                if typing.len() == keys_total {
                    type_start = (v.sess.rx(), v.ctl.frames);
                }
                v.sess.send(typing.pop().unwrap())?;
                next_key = Instant::now() + Duration::from_millis(500);
            }
            if !v.uploads.is_empty() {
                v.pump_uploads()?;
            }
            v.check_mods()?;
            if v.ctl.tick(&mut v.sess)? {
                v.status()?;
            }
        }
        if opt.sound && v.probe.is_some() {
            eprintln!("probe: sound {:.1} KB", v.audio_bytes as f64 / 1024.0);
        }
        if keys_total > 0 {
            let rx = v.sess.rx() - type_start.0;
            eprintln!(
                "probe: sent {keys_total} inputs, {} frames, {:.0} bytes per input (includes idle since the first)",
                v.ctl.frames - type_start.1,
                rx as f64 / keys_total as f64
            );
        }
        Ok(())
    })();
    if interactive {
        let _ = v.release_all();
        #[cfg(unix)]
        if v.key_states {
            let _ = execute!(v.out, event::PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            v.out,
            event::DisableBracketedPaste,
            event::DisableMouseCapture,
            cursor::Show,
            terminal::LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    } else {
        eprintln!(
            "probe: frame {}x{} {}, {}, rtt {:.0} ms, paint output {:.1} KB, last paint {:.1} ms",
            v.fb.w,
            v.fb.h,
            v.fb.mode.map_or("-", |m| m.name()),
            v.ctl.preset_label(),
            v.ctl.rtt,
            v.paint_bytes as f64 / 1024.0,
            v.paint_ms
        );
        if let Some(path) = opt.probe.as_ref().and_then(|p| p.2.as_ref()) {
            std::fs::write(path, v.fb.ppm()).map_err(|e| e.to_string())?;
        }
    }
    v.sess.close();
    eprintln!(
        "sshremote: {} frames, received {:.1} KB, sent {:.1} KB",
        v.ctl.frames,
        v.sess.rx() as f64 / 1024.0,
        v.sess.tx_bytes as f64 / 1024.0
    );
    if let Err(e) = r {
        failure.get_or_insert(e.to_string());
    }
    match failure {
        Some(e) => Err(format!("{e}\n{}", v.sess.log())),
        None => Ok(()),
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
const WINDOWS_SSH: &str = "this is a Windows ssh session, whose console layer drops sixel graphics,
and its output pipe could not be reached directly.
Run the viewer on the computer you are sitting at instead, for example:
  sshremote USER@THIS-PC --remote \"D:\\Projects\\sshremote.exe --agent\"
or add --gui to open a window there.";

/// The viewer runs inside a Windows OpenSSH session, where output through the pseudo console cannot carry images.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_ssh_session() -> bool {
    cfg!(windows) && (std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_CLIENT").is_some())
}

#[cfg_attr(not(unix), allow(dead_code))]
fn parse_cell_report(buf: &[u8]) -> Option<(usize, usize)> {
    let s = std::str::from_utf8(buf).ok()?;
    let body = &s[s.find("[6;")? + 4..];
    let (h, rest) = body.split_once(';')?;
    let w = &rest[..rest.find('t')?];
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    ((4..=64).contains(&w) && (8..=128).contains(&h)).then_some((w, h))
}

/// Lets this pane's images through tmux to the outer terminal; the pane's position, or None outside tmux.
fn tmux_passthrough() -> Option<(usize, usize)> {
    std::env::var_os("TMUX")?;
    let ok = std::process::Command::new("tmux")
        .args(["set", "-p", "allow-passthrough", "on"])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok { tmux_pane() } else { None }
}

/// Where the current tmux pane starts on the outer terminal, counting a status line at the top.
fn tmux_pane() -> Option<(usize, usize)> {
    let out = std::process::Command::new("tmux")
        .args(["display", "-p", "#{pane_top} #{pane_left} #{status} #{status-position}"])
        .output()
        .ok()?;
    parse_tmux_pane(&String::from_utf8_lossy(&out.stdout))
}

fn parse_tmux_pane(text: &str) -> Option<(usize, usize)> {
    let mut it = text.split_whitespace();
    let top: usize = it.next()?.parse().ok()?;
    let left: usize = it.next()?.parse().ok()?;
    let status = match it.next() {
        Some("on") => 1,
        Some(n) => n.parse().unwrap_or(0),
        None => 0,
    };
    let above = if it.next() == Some("top") { status } else { 0 };
    Some((top + above, left))
}

/// A DCS passthrough for tmux: the sequence inside reaches the outer terminal with its escapes doubled.
fn tmux_wrap(out: &mut Vec<u8>, seq: &[u8]) {
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in seq {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
}

/// Asks the terminal for its cell size in pixels (XTWINOPS 16); works through ssh,
/// unlike the pixel size in the window-size ioctl. Needs raw mode.
#[cfg(unix)]
fn query_cell() -> Option<(usize, usize)> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let mut tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    tty.write_all(b"[16t").ok()?;
    tty.flush().ok()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut buf = Vec::new();
    while !buf.ends_with(b"t") {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        let mut pfd = libc::pollfd { fd: tty.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        if left == 0 || unsafe { libc::poll(&mut pfd, 1, left) } <= 0 {
            break;
        }
        let mut b = [0u8; 64];
        match tty.read(&mut b) {
            Ok(n @ 1..) => buf.extend_from_slice(&b[..n]),
            _ => break,
        }
    }
    parse_cell_report(&buf)
}

/// The Windows console reports keys, not replies, so the default cell size is used there.
#[cfg(not(unix))]
fn query_cell() -> Option<(usize, usize)> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn tmux_pane_and_passthrough() {
        assert_eq!(super::parse_tmux_pane("3 40 on top\n"), Some((4, 40)));
        assert_eq!(super::parse_tmux_pane("0 0 2 bottom\n"), Some((0, 0)));
        let mut out = Vec::new();
        super::tmux_wrap(&mut out, b"\x1b[1;1H\x1bPq#0\x1b\\");
        assert_eq!(out, b"\x1bPtmux;\x1b\x1b[1;1H\x1b\x1bPq#0\x1b\x1b\\\x1b\\".to_vec());
    }

    /// What the viewer writes, kept for the test to read.
    #[derive(Clone, Default)]
    struct Screen(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Screen {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Screen {
        fn take(&self) -> String {
            String::from_utf8_lossy(&std::mem::take(&mut *self.0.lock().unwrap())).into_owned()
        }
    }

    /// The terminal viewer goes between the screen, a shell kept open, the files and the chat on
    /// one connection, as the window does, and asks before disconnecting.
    #[test]
    #[ignore]
    fn the_terminal_viewer_switches_modes_on_one_connection() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        use super::*;
        static SCRATCH: std::sync::Once = std::sync::Once::new();
        SCRATCH.call_once(|| {
            let dir = std::env::temp_dir().join(format!("sshremote-cli-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for var in ["APPDATA", "XDG_CONFIG_HOME"] {
                unsafe { std::env::set_var(var, &dir) };
            }
        });
        let opt = crate::parse_client(&["--local".into()]).unwrap();
        let (sess, _tx, rx) = Session::connect(&opt).expect("a local agent (SSHREMOTE_EXE)");
        let screen = Screen::default();
        let mut v = Viewer::new(&opt, sess, Box::new(screen.clone()));
        (v.cols, v.rows) = (100, 30);
        let key = |c: KeyCode| Event::Key(KeyEvent::new(c, KeyModifiers::NONE));
        let menu = Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
        let pump = |v: &mut Viewer, until: &dyn Fn(&Viewer) -> bool| {
            let end = Instant::now() + Duration::from_secs(10);
            while Instant::now() < end && !until(v) {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Ev::Net(ToClient::Frame(f))) => v.apply(f).unwrap(),
                    Ok(Ev::Net(m)) => v.on_message(m).unwrap(),
                    _ => {}
                }
            }
            assert!(until(v), "waited too long");
        };
        v.push_settings().unwrap();
        pump(&mut v, &|v| v.fb.w > 0);
        // The menu lists the modes; f goes to the files, listed from the host.
        v.on_term(menu.clone()).unwrap();
        assert_eq!(v.overlay, Overlay::Menu);
        assert!(screen.take().contains("Screen sharing"));
        v.on_term(key(KeyCode::Char('f'))).unwrap();
        assert_eq!(v.view, View::Files);
        pump(&mut v, &|v| !v.files.loading && !v.files.entries.is_empty());
        v.draw_files().unwrap();
        assert!(screen.take().contains("Files on"), "the folder is drawn");
        // The terminal: a shell opens and writes its prompt.
        v.on_term(menu.clone()).unwrap();
        v.on_term(key(KeyCode::Char('t'))).unwrap();
        assert_eq!(v.view, View::Terminal);
        let shell = v.shell.expect("a shell");
        pump(&mut v, &|_| !screen.0.lock().unwrap().is_empty());
        // Back to the screen: the shell stays, and what it writes waits for its return.
        v.on_term(menu.clone()).unwrap();
        v.on_term(key(KeyCode::Char('s'))).unwrap();
        assert_eq!(v.view, View::Screen);
        assert_eq!(v.shell, Some(shell), "the shell is kept");
        v.sess.send(ToAgent::TermInput { id: shell, data: b"echo kept-while-away\r".to_vec() }).unwrap();
        pump(&mut v, &|v| String::from_utf8_lossy(&v.backlog).contains("kept-while-away"));
        screen.take();
        v.on_term(menu.clone()).unwrap();
        v.on_term(key(KeyCode::Char('t'))).unwrap();
        assert!(screen.take().contains("kept-while-away"), "written out on return");
        assert!(v.backlog.is_empty());
        // The chat, with its history.
        v.on_term(menu.clone()).unwrap();
        v.on_term(key(KeyCode::Char('c'))).unwrap();
        for c in "hello".chars() {
            v.on_term(key(KeyCode::Char(c))).unwrap();
        }
        v.on_term(key(KeyCode::Enter)).unwrap();
        assert!(crate::chat::load_history(&v.host).iter().any(|l| !l.0 && l.1 == "hello"), "kept in the history");
        assert!(screen.take().contains("hello"), "and shown");
        v.on_term(key(KeyCode::Esc)).unwrap();
        assert_eq!(v.overlay, Overlay::None);
        // Disconnecting asks; no keeps the connection, y ends it.
        v.on_term(menu.clone()).unwrap();
        v.on_term(key(KeyCode::Char('q'))).unwrap();
        assert_eq!(v.overlay, Overlay::Confirm('q'));
        v.on_term(key(KeyCode::Char('n'))).unwrap();
        assert!(!v.quit);
        v.on_term(menu).unwrap();
        v.on_term(key(KeyCode::Char('q'))).unwrap();
        v.on_term(key(KeyCode::Char('y'))).unwrap();
        assert!(v.quit, "y disconnects");
        v.sess.close();
    }

    /// The shell's program's modes are followed, so they can be set again after the menu.
    #[test]
    fn shell_modes_are_followed() {
        let mut m = super::ShellModes::default();
        m.follow(b"[?1049h[?25l[?1000;1006h text");
        assert!(m.alt && m.cursor_hidden);
        assert_eq!(m.restore(), "[?1000h[?1006h[?25l");
        m.follow(b"[?1000l[?1049l[?25h");
        assert!(!m.alt);
        assert_eq!(m.restore(), "[?1006h[?25h");
    }

    /// Names and messages from the other side cannot drive this terminal.
    #[test]
    fn text_from_the_host_is_made_harmless() {
        let evil = "file]52;c;aGk=.txt";
        let shown = super::clean(evil);
        assert!(!shown.contains('') && !shown.contains(''), "{shown:?}");
        assert_eq!(super::visible_len(&shown), shown.chars().count());
    }

    #[test]
    fn cell_report() {
        assert_eq!(super::parse_cell_report(b"[6;20;10t"), Some((10, 20)));
        assert_eq!(super::parse_cell_report(b"junk[6;17;8t"), Some((8, 17)));
        assert_eq!(super::parse_cell_report(b"[4;600;1200t"), None);
    }
}
