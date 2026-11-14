//! Terminal viewer: keeps an exact copy of the remote framebuffer and repaints
//! only damaged cells with sixel, or kitty images where the terminal shows them.

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

mod browse;
mod overlay;
mod picture;
mod pressed;

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
    /// Kitty images in place of sixel (kitty, Ghostty, WezTerm): true colour.
    kitty: Option<crate::kitty::Kitty>,
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

    /// The shell's screen back as its program left it: the main screen (kept by the terminal) or,
    /// for a full-screen program, its own screen redrawn by it; its modes set again.
    fn show_shell(&mut self) -> io::Result<()> {
        if self.probe.is_some() {
            return Ok(());
        }
        self.unpicture()?;
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
                self.unpicture()?;
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
                let t = crate::chat::trimmed(&t);
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
            ToClient::Snapshot(png) => self.notify(&crate::session::save_snapshot(&png))?,
            ToClient::Hello { w, h, backend } => {
                self.sess.hello(w, h, backend);
                self.label = format!("{} {w}x{h} {}", self.host, self.sess.backend);
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
            ToClient::HelperInstalled { ok, text } => {
                self.notify(&text)?;
                if ok {
                    self.sess.send(ToAgent::ListWindows)?;
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
                // A refused upload (the host's disk full) stops being sent too.
                self.uploads.retain(|u| u.0 != id);
                self.transfers.retain(|t| !(t.upload && t.id == id));
                let t = if ok { format!("Sent: {text}") } else { format!("Send failed: {text}") };
                self.notify(&t)?;
                if ok && self.view == View::Files {
                    let path = self.files.path.clone();
                    self.files_go(path)?;
                }
            }
            ToClient::FetchStart { id, size, name, .. } => {
                if let Some(crate::files::Note::Done { text, .. }) = self.sess.fetch_start(&mut self.inbox, id, size, &crate::files::downloads(), &name)? {
                    self.notify(&format!("Download of {name} failed: {text}"))?;
                } else {
                    self.transfers.push(Transfer { id, name, done: 0, size, upload: false });
                }
            }
            ToClient::FetchData { id, data } => match self.sess.fetch_data(&mut self.inbox, id, &data)? {
                Some(crate::files::Note::Progress { id, received }) => {
                    if let Some(t) = self.transfers.iter_mut().find(|t| !t.upload && t.id == id) {
                        t.done = received;
                    }
                    if self.view == View::Files {
                        self.status()?;
                    }
                }
                Some(crate::files::Note::Done { text, .. }) => {
                    self.transfers.retain(|t| t.upload || t.id != id);
                    self.notify(&format!("Download failed: {text}"))?;
                }
                None => {}
            },
            ToClient::FetchEnd { id, ok, text } => {
                self.transfers.retain(|t| t.upload || t.id != id);
                match self.inbox.end(id, ok, &text) {
                    Some(crate::files::Note::Done { ok, text, .. }) => {
                        self.notify(&if ok { format!("Saved {text}") } else { format!("Download failed: {text}") })?
                    }
                    // One that could not even be opened there says why here.
                    _ if !ok && text != "cancelled" => self.notify(&format!("Download failed: {text}"))?,
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_term(&mut self, e: Event) -> io::Result<()> {
        let screen_input = self.view == View::Screen && self.overlay == Overlay::None && self.prompt.is_none();
        let modifier = match &e {
            Event::Key(k) => keys::modifier_bit(&k.code),
            _ => None,
        };
        match e {
            // Keys held at the host are let go when this terminal goes to the back: their releases
            // would reach whatever is in front.
            Event::FocusLost => self.release_all()?,
            // A modifier the host holds is let go there as here, whatever is shown now (the menu
            // was opened with Ctrl held, say).
            Event::Key(k) if k.kind == KeyEventKind::Release && modifier.is_some_and(|b| self.held_mods & b != 0) => {
                self.sync_mods(self.held_mods & !modifier.unwrap_or(0))?;
            }
            // With presses and releases, a modifier goes down at the host as it does here.
            Event::Key(k) if k.kind != KeyEventKind::Release && screen_input && self.key_states && modifier.is_some() => {
                self.sync_mods(self.held_mods | modifier.unwrap_or(0))?;
            }
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
                let (ox, oy) = self.origin();
                let (Some(dx), Some(dy)) = ((m.column as usize * self.cw + self.cw / 2).checked_sub(ox), (m.row as usize * self.ch + self.ch / 2).checked_sub(oy)) else {
                    return Ok(());
                };
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
        kitty: None,
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
    // What is said before the screen comes (asking the person there, and the code to compare
    // when asking without a password) is shown on the terminal while it waits.
    let (sess, tx, rx) = Session::open_noting(&opt.target()?, opt.gui, &|t: &str| eprintln!("sshremote: {}", t.replace('\n', "\n           ")))?;
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
        v.kitty = kitty_images(v.tmux.is_some()).then(crate::kitty::Kitty::new);
        // Kitty images placed by text: sent on while the pane is out of sight too (they show
        // only where tmux draws their text), so a pane brought back is whole.
        if v.kitty.is_some() && v.tmux.is_some() {
            let _ = std::process::Command::new("tmux").args(["set", "-p", "allow-passthrough", "all"]).stderr(std::process::Stdio::null()).status();
        }
        let _ = execute!(
            v.out,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            event::EnableMouseCapture,
            event::EnableBracketedPaste,
            event::EnableFocusChange
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
                Ok(p) => {
                    v.player = Some(p);
                    v.sess.send(ToAgent::Audio(true))?;
                }
                // A probe measures the sound's bytes with nothing to play them.
                Err(_) if opt.probe.is_some() => v.sess.send(ToAgent::Audio(true))?,
                Err(e) => eprintln!("sshremote: sound: {e}"),
            }
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
        // The kitty images go with the viewer (the terminal would keep them otherwise).
        let _ = v.unpicture();
        #[cfg(unix)]
        if v.key_states {
            let _ = execute!(v.out, event::PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            v.out,
            event::DisableFocusChange,
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
        match v.sess.straight() {
            Some(Some(a)) => eprintln!("probe: straight to {a}, past the relay"),
            Some(None) => eprintln!("probe: through the relay"),
            None => {}
        }
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

/// Whether to draw with kitty images: `SSHREMOTE_GRAPHICS=kitty` or `sixel` says so; otherwise
/// the terminal is asked. Through tmux (which answers for itself) only when tmux runs in kitty,
/// which takes the images placed by text there.
fn kitty_images(tmux: bool) -> bool {
    match std::env::var("SSHREMOTE_GRAPHICS").as_deref() {
        Ok("kitty") => true,
        Ok("sixel") => false,
        _ if tmux => std::env::var_os("KITTY_WINDOW_ID").is_some(),
        _ => query_kitty(),
    }
}

#[cfg(unix)]
fn query_kitty() -> bool {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let Ok(mut tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") else { return false };
    if tty.write_all(crate::kitty::QUERY).and_then(|_| tty.flush()).is_err() {
        return false;
    }
    // Until the attributes' answer (ESC [ ? ... c), which comes after any answer to the first.
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut buf = Vec::new();
    let done = |b: &[u8]| b.windows(3).position(|w| w == b"\x1b[?").is_some_and(|at| b[at..].contains(&b'c'));
    while !done(&buf) {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        let mut pfd = libc::pollfd { fd: tty.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        if left == 0 || unsafe { libc::poll(&mut pfd, 1, left) } <= 0 {
            break;
        }
        let mut b = [0u8; 256];
        match tty.read(&mut b) {
            Ok(n @ 1..) => buf.extend_from_slice(&b[..n]),
            _ => break,
        }
    }
    crate::kitty::answered(&buf)
}

/// Windows Terminal and the console show sixel, not kitty images.
#[cfg(not(unix))]
fn query_kitty() -> bool {
    false
}

#[cfg(test)]
mod tests;
