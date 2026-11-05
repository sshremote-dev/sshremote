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
    /// Enlarge a reduced capture to the terminal; off where the terminal output itself crosses the network.
    stretch: bool,
    pointer: (u16, u16),
    quit: bool,
    paint_ms: f64,
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
        let text = if self.menu {
            format!(" menu: {} ", Control::MENU_HELP)
        } else if self.win_next {
            " Win+: press a key (Space: Win alone) ".to_string()
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

    fn on_term(&mut self, e: Event) -> io::Result<()> {
        match e {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if self.menu {
                    self.menu = false;
                    let c = match k.code {
                        event::KeyCode::Char(c) => c,
                        _ => '\0',
                    };
                    let a = self.ctl.menu(c);
                    return self.run_action(a);
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
        // A viewer run inside an ssh session sends its sixel output over that session.
        stretch: !(opt.local && std::env::var_os("SSH_CONNECTION").is_some()),
        pointer: (u16::MAX, u16::MAX),
        quit: false,
        paint_ms: 0.0,
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
        while !v.quit && deadline.is_none_or(|d| Instant::now() < d) {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Ev::Net(ToClient::Frame(f))) => v.apply(f)?,
                Ok(Ev::Net(ToClient::Pong(t))) => v.ctl.pong(t),
                Ok(Ev::Net(ToClient::Error(e)) | Ev::Closed(e)) => {
                    failure = Some(e);
                    break;
                }
                Ok(Ev::Net(_)) => {}
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
        "desk: {} frames, received {:.1} KB, sent {:.1} KB",
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
  desk USER@THIS-PC --remote \"D:\\Projects\\desk.exe --agent\"
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
