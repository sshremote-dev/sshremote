//! Terminal viewer: keeps an exact copy of the remote framebuffer and repaints
//! only damaged cells with sixel.

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::{cursor, execute, terminal};

use crate::color::{Lut, Mode};
use crate::keys;
use crate::proto::{Frame, MOD_SUPER, SUPER_L, ToAgent, ToClient};
use crate::quant;
use crate::session::{Action, Control, Ev, Framebuffer, Options, Session};
use crate::sixel::Sixel;

const MENU: &str = "q quit | 0-4 a quality | m colour | +/- scale | f fps | z zoom | s screen | x screenshot | \
p pause | u one frame | o sound | c chat | g get file | l send file | t shell | v share window/app/screen | w Win | r refresh";

struct Viewer {
    out: Box<dyn Write>,
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
    menu: bool,
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
    /// The remote shell shown instead of the picture, while open.
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
        if self.shell.is_some() {
            // Frames still in flight when the shell opened; the screen is redrawn after it.
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
                let _ = write!(out, "\x1b[{};{}H", r + 1, c + 1);
                if adaptive {
                    quant::quantize(&self.px, &mut self.qpal, &mut self.idx);
                    self.sixel.encode(out, w, h, &self.idx, &self.qpal);
                } else {
                    self.idx.clear();
                    self.idx.extend(self.px.iter().map(|p| self.lut.get(p[0], p[1], p[2])));
                    self.sixel.encode(out, w, h, &self.idx, &self.pal);
                }
                c = c1;
            }
        }
    }

    fn status(&mut self) -> io::Result<()> {
        if self.shell.is_some() {
            return Ok(());
        }
        let text = if let Some((kind, buf)) = &self.prompt {
            let what = match kind {
                'c' => "message",
                'g' => "remote path to download",
                'v' => "share (number, A+number, a, p, or Enter for the whole screen)",
                _ => "local file to send",
            };
            format!(" {what} (Enter sends, Esc cancels)> {buf}_ ")
        } else if self.menu {
            format!(" menu: {MENU} ")
        } else if let Some((n, _)) = self.note.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(6)) {
            format!(" {n} ")
        } else if self.paused {
            format!(
                " PAUSED (Ctrl+] u: one frame, p: resume) | {} ",
                self.ctl.status(&self.label, self.fb.w, self.fb.h)
            )
        } else if self.win_next {
            " Win+: press a key (Space: Win alone) ".to_string()
        } else if self.sharing.0 != 0 {
            format!(
                " SHARING {} (Ctrl+] v: whole screen) | {} ",
                self.sharing.1,
                self.ctl.status(&self.label, self.fb.w, self.fb.h)
            )
        } else {
            format!(
                " {} | paint {:.1} ms | Ctrl+] menu ",
                self.ctl.status(&self.label, self.fb.w, self.fb.h),
                self.paint_ms
            )
        };
        let text: String = text.chars().take(self.cols).collect();
        let pad = self.cols.saturating_sub(text.chars().count());
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
        if self.probe.is_some() {
            eprintln!("probe: {text}");
            return Ok(());
        }
        self.note = Some((text.to_string(), Instant::now()));
        self.status()
    }

    /// Menu keys beyond the shared ones: screenshot, pause, sound, chat and files.
    fn menu_extra(&mut self, c: char) -> io::Result<()> {
        match c {
            'x' => {
                self.sess.send(ToAgent::Snapshot)?;
                self.notify("Taking a screenshot...")
            }
            'p' => {
                self.paused = !self.paused;
                self.sess.send(ToAgent::Pause(self.paused))?;
                self.status()
            }
            'u' => {
                self.sess.send(ToAgent::Refresh)?;
                self.status()
            }
            'o' => {
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
            't' => self.open_shell(),
            'v' => {
                // Sharing part of the screen: the same key leads straight back to all of it.
                if self.sharing.0 != 0 {
                    self.sess.send(ToAgent::Share { kind: 0, id: 0 })?;
                    return self.notify("Sharing the whole screen...");
                }
                self.share_list = None;
                self.sess.send(ToAgent::ListWindows)?;
                self.prompt = Some(('v', String::new()));
                self.draw_share_list()
            }
            'c' | 'g' | 'l' => {
                self.prompt = Some((c, String::new()));
                self.status()
            }
            _ => {
                let a = self.ctl.menu(c);
                self.run_action(a)
            }
        }
    }

    /// Leaves the picture for a shell on the remote host; this terminal emulates it directly.
    fn open_shell(&mut self) -> io::Result<()> {
        if self.probe.is_some() {
            return Ok(());
        }
        let id = self.next_shell;
        self.next_shell += 1;
        self.sess.send(ToAgent::Pause(true))?;
        let _ = execute!(self.out, event::DisableMouseCapture, terminal::LeaveAlternateScreen, cursor::Show);
        write!(
            self.out,
            "\x1b[0m\r\nsshremote: shell on the SSH host. Type exit, or press Ctrl+] to return to the screen.\r\n"
        )?;
        self.out.flush()?;
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        self.shell = Some(id);
        self.sess.send(ToAgent::TermOpen { id, cols, rows })
    }

    /// Back from the shell to the picture.
    fn close_shell(&mut self, why: &str) -> io::Result<()> {
        let Some(id) = self.shell.take() else { return Ok(()) };
        self.sess.send(ToAgent::TermClose { id })?;
        let _ = execute!(self.out, terminal::EnterAlternateScreen, cursor::Hide, event::EnableMouseCapture);
        self.dirty.clear();
        self.full_redraw()?;
        self.sess.send(ToAgent::Pause(self.paused))?;
        self.sess.send(ToAgent::Refresh)?;
        self.notify(why)
    }

    /// Keys typed while the shell is shown, as the bytes a terminal sends.
    fn shell_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::{KeyCode, KeyModifiers};
        let Some(id) = self.shell else { return Ok(()) };
        if keys::is_menu(k) {
            return self.close_shell("Shell closed");
        }
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
                self.prompt = None
            }
            KeyCode::Enter => {
                let (kind, text) = (*kind, buf.trim().to_string());
                self.prompt = None;
                if !text.is_empty() {
                    return self.run_prompt(kind, text);
                }
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
                    lines.push(format!("   {:<6} {}: {}  {}x{}", i + 1, w.app, w.title, w.w, w.h));
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
                let (kind, id) = match t.as_str() {
                    "" => (0, 0),
                    "a" => (3, 0),
                    "p" => (4, 0),
                    n if n.starts_with(['A', 'P']) => match pick(&n[1..]) {
                        Some(id) => (2, id),
                        None => return self.close_share_list().and_then(|_| self.notify(&format!("No window {n}"))),
                    },
                    n => match pick(n) {
                        Some(id) => (1, id),
                        None => return self.close_share_list().and_then(|_| self.notify(&format!("No window {n}"))),
                    },
                };
                self.close_share_list()?;
                self.sess.send(ToAgent::Share { kind, id })
            }
            'c' => {
                self.sess.send(ToAgent::Chat(text.clone()))?;
                self.notify(&format!("You: {text}"))
            }
            'g' => {
                self.sess.send(ToAgent::Fetch { path: text.clone() })?;
                self.notify(&format!("Downloading {text}..."))
            }
            _ => self.upload(&text),
        }
    }

    /// Sends a local file to the remote Downloads folder; the link paces the writes.
    fn upload(&mut self, path: &str) -> io::Result<()> {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => return self.notify(&format!("{path}: {e}")),
        };
        let name = std::path::Path::new(path).file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned());
        let id = self.next_upload;
        self.next_upload += 1;
        self.sess.send(ToAgent::FileStart { id, size: data.len() as u64, name: name.clone() })?;
        for chunk in data.chunks(64 << 10) {
            self.sess.send(ToAgent::FileData { id, data: chunk.to_vec() })?;
        }
        self.sess.send(ToAgent::FileEnd { id })?;
        self.notify(&format!("Sending {name}..."))
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
                // A bell, so a message is noticed while looking elsewhere.
                if self.probe.is_none() {
                    self.out.write_all(b"\x07")?;
                }
                self.notify(&format!("Remote: {t}"))?
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
                self.out.write_all(&data)?;
                self.out.flush()?;
            }
            ToClient::TermExit { id, text } if self.shell == Some(id) => {
                self.close_shell(&format!("Shell: {text}"))?;
            }
            ToClient::FileDone { ok, text, .. } => {
                let t = if ok { format!("Sent: {text}") } else { format!("Send failed: {text}") };
                self.notify(&t)?
            }
            ToClient::FetchStart { id, size, name } => {
                if let Some(crate::files::Note::Done { text, .. }) = self.inbox.start(id, size, &name) {
                    self.notify(&format!("download {name}: {text}"))?;
                }
            }
            ToClient::FetchData { id, data } => {
                if let Some(crate::files::Note::Progress { id, received }) = self.inbox.data(id, &data) {
                    self.sess.send(ToAgent::FetchAck { id, received })?;
                }
            }
            ToClient::FetchEnd { id, ok, text } => match self.inbox.end(id, ok, &text) {
                Some(crate::files::Note::Done { ok, text, .. }) => {
                    let t = if ok { format!("download ok={ok} {text}") } else { format!("download failed: {text}") };
                    self.notify(&t)?
                }
                _ => self.notify(&format!("download failed: {text}"))?,
            },
            _ => {}
        }
        Ok(())
    }

    fn on_term(&mut self, e: Event) -> io::Result<()> {
        match e {
            Event::Key(k) if k.kind != KeyEventKind::Release && self.shell.is_some() => self.shell_key(&k)?,
            Event::Paste(text) if self.shell.is_some() => {
                if let Some(id) = self.shell {
                    self.sess.send(ToAgent::TermInput { id, data: text.into_bytes() })?;
                }
            }
            Event::Resize(cols, rows) if self.shell.is_some() => {
                if let Some(id) = self.shell {
                    self.sess.send(ToAgent::TermResize { id, cols, rows })?;
                }
                self.measure();
            }
            Event::Mouse(_) if self.shell.is_some() => {}
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if self.prompt.is_some() {
                    return self.prompt_key(&k);
                }
                if self.menu {
                    self.menu = false;
                    let c = match k.code {
                        event::KeyCode::Char(c) => c,
                        _ => '\0',
                    };
                    return self.menu_extra(c);
                }
                if keys::is_menu(&k) {
                    self.menu = true;
                    return self.status();
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
            Event::Mouse(m) => {
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
            Event::Resize(..) => {
                self.measure();
                self.dirty.clear();
                self.full_redraw()?;
                self.push_settings()?;
                self.sess.send(ToAgent::Refresh)?;
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn run(opt: Options) -> Result<(), String> {
    if opt.gui {
        return crate::gui::run(&opt);
    }
    let (sess, tx, rx) = Session::connect(&opt)?;
    let host = opt.host.clone().unwrap_or_else(|| "local".into());
    let mut v = Viewer {
        out: if opt.probe.is_some() { Box::new(io::sink()) } else { Box::new(io::stdout()) },
        probe: opt.probe.as_ref().map(|p| p.1),
        paint_bytes: 0,
        label: format!("{host} {}x{} {}", sess.desk.0, sess.desk.1, sess.backend),
        sess,
        ctl: Control::new(&opt, Mode::True),
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
        menu: false,
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
    };

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
        if opt.cell.is_none() {
            v.cell_queried = query_cell();
        }
        let _ = execute!(v.out, terminal::EnterAlternateScreen, cursor::Hide, event::EnableMouseCapture);
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
            v.sess.send(ToAgent::Fetch { path: path.clone() })?;
        }
        if let Some(path) = &opt.probe_send {
            let data = std::fs::read(path)?;
            let name =
                std::path::Path::new(path).file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned());
            v.sess.send(ToAgent::FileStart { id: 1, size: data.len() as u64, name })?;
            for chunk in data.chunks(64 << 10) {
                v.sess.send(ToAgent::FileData { id: 1, data: chunk.to_vec() })?;
            }
            v.sess.send(ToAgent::FileEnd { id: 1 })?;
        }
        while !v.quit && deadline.is_none_or(|d| Instant::now() < d) {
            match rx.recv_timeout(Duration::from_millis(250)) {
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
        let _ = execute!(v.out, event::DisableMouseCapture, cursor::Show, terminal::LeaveAlternateScreen);
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
    fn cell_report() {
        assert_eq!(super::parse_cell_report(b"[6;20;10t"), Some((10, 20)));
        assert_eq!(super::parse_cell_report(b"junk[6;17;8t"), Some((8, 17)));
        assert_eq!(super::parse_cell_report(b"[4;600;1200t"), None);
    }
}
