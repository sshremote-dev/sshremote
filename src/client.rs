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
use crate::proto::{Frame, ToAgent, ToClient};
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
    fb: Framebuffer,
    lut_mode: Option<Mode>,
    lut: Lut,
    pal: Vec<[u8; 3]>,
    sixel: Sixel,
    idx: Vec<u8>,
    dirty: Vec<bool>,
    menu: bool,
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
            && ws.width > 0
            && ws.height > 0
            && ws.columns > 0
            && ws.rows > 0
        {
            cw = ws.width as usize / ws.columns as usize;
            ch = ws.height as usize / ws.rows as usize;
        }
        (self.cw, self.ch) = self.cell_override.unwrap_or((cw, ch));
        self.ctl.s.view_w = (self.cols * self.cw).min(u16::MAX as usize) as u16;
        self.ctl.s.view_h = ((self.rows - 1) * self.ch).min(u16::MAX as usize) as u16;
    }

    fn display(&self) -> (usize, usize) {
        self.fb.fit((self.ctl.s.view_w as usize, self.ctl.s.view_h as usize))
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
            // Sixel registers hold at most 256 colours, so true colour is shown through the 256 palette.
            let shown = if f.mode == Mode::True { Mode::Color256 } else { f.mode };
            self.pal = shown.palette();
            self.lut = Lut::new(&self.pal);
            self.lut_mode = Some(f.mode);
        }
        let (w, h) = (self.fb.w, self.fb.h);
        let (dw, dh) = self.display();
        let img_rows = self.rows - 1;
        self.dirty.resize(self.cols * img_rows, false);
        for t in &f.tiles {
            let (tx, ty, tw, th) = (t.x as usize, t.y as usize, t.w as usize, t.h as usize);
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
        self.ctl.frame_done();
        self.sess.send(ToAgent::Ack(f.id))
    }

    /// Repaints dirty cells, merged into rectangles of whole cells.
    fn paint(&mut self, out: &mut Vec<u8>, dw: usize, dh: usize) {
        let (cols, img_rows) = (self.cols, self.rows - 1);
        let (fw, fh) = (self.fb.w, self.fb.h);
        let black = self.lut.get(0, 0, 0);
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
                self.idx.clear();
                self.idx.resize(w * h, black);
                if dw > 0 {
                    for py in 0..h {
                        let dy = y0 + py;
                        if dy >= dh {
                            break;
                        }
                        let frow = (dy * fh / dh) * fw;
                        let line = &mut self.idx[py * w..(py + 1) * w];
                        for (px, slot) in line.iter_mut().enumerate() {
                            let dx = x0 + px;
                            if dx >= dw {
                                break;
                            }
                            let o = (frow + dx * fw / dw) * 3;
                            let rgb = &self.fb.rgb[o..o + 3];
                            *slot = self.lut.get(rgb[0], rgb[1], rgb[2]);
                        }
                    }
                }
                let _ = write!(out, "\x1b[{};{}H", r + 1, c + 1);
                self.sixel.encode(out, w, h, &self.idx, &self.pal);
                c = c1;
            }
        }
    }

    fn status(&mut self) -> io::Result<()> {
        let text = if self.menu {
            format!(" menu: {} ", Control::MENU_HELP)
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
                    self.sess.send(ToAgent::Key { keysym, mods })?;
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
    let (sess, tx, rx) = Session::connect(&opt)?;
    if opt.gui {
        return crate::gui::run(&opt, sess, rx);
    }
    let host = opt.host.clone().unwrap_or_else(|| "local".into());
    let mut v = Viewer {
        out: if opt.probe.is_some() { Box::new(io::sink()) } else { Box::new(io::stdout()) },
        probe: opt.probe.as_ref().map(|p| p.1),
        paint_bytes: 0,
        label: format!("{host} {}x{} {}", sess.desk.0, sess.desk.1, sess.backend),
        sess,
        ctl: Control::new(&opt, Mode::Color256),
        cols: 0,
        rows: 0,
        cw: 10,
        ch: 20,
        cell_override: opt.cell,
        fb: Framebuffer::default(),
        lut_mode: None,
        lut: Lut::new(&[[0, 0, 0]]),
        pal: Vec::new(),
        sixel: Sixel::new(),
        idx: Vec::new(),
        dirty: Vec::new(),
        menu: false,
        pointer: (u16::MAX, u16::MAX),
        quit: false,
        paint_ms: 0.0,
    };

    let interactive = opt.probe.is_none();
    let deadline = opt.probe.as_ref().map(|p| Instant::now() + Duration::from_secs_f64(p.0));
    if interactive {
        terminal::enable_raw_mode().map_err(|e| e.to_string())?;
        let _ = execute!(v.out, terminal::EnterAlternateScreen, cursor::Hide, event::EnableMouseCapture);
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
    let mut typing: Vec<char> = opt.probe_type.chars().rev().collect();
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
                let c = typing.pop().unwrap();
                v.sess.send(ToAgent::Key { keysym: c as u32, mods: 0 })?;
                next_key = Instant::now() + Duration::from_millis(500);
            }
            if v.ctl.tick(&mut v.sess)? {
                v.status()?;
            }
        }
        if keys_total > 0 {
            let rx = v.sess.rx() - type_start.0;
            eprintln!(
                "probe: typed {keys_total} keys, {} frames, {:.0} bytes per key (includes idle since the first key)",
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
