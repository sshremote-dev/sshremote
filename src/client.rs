//! Terminal viewer: runs the agent over `ssh -T`, keeps an exact copy of the
//! remote framebuffer and repaints only damaged cells with sixel.

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::{cursor, execute, terminal};

use crate::color::{Lut, Mode, unpack};
use crate::keys;
use crate::proto::{Frame, PRESETS, Settings, ToAgent, ToClient, read_msg, write_msg};
use crate::sixel::Sixel;

pub struct Options {
    pub host: Option<String>,
    pub local: bool,
    pub remote: String,
    pub capture: String,
    pub ssh_args: Vec<String>,
    pub preset: usize,
    pub mode: Option<Mode>,
    pub scale: Option<u8>,
    pub fps: Option<u8>,
    pub cell: Option<(usize, usize)>,
    /// Headless run: (seconds, view size, optional PPM dump of the final framebuffer).
    pub probe: Option<(f64, (u16, u16), Option<String>)>,
    /// Probe only: text typed one key per 500 ms after a 2 s settle, to measure per-key traffic.
    pub probe_type: String,
}

enum Ev {
    Net(ToClient),
    Closed(String),
    Term(Event),
}

struct Counting<R> {
    inner: R,
    n: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
}

const FPS_STEPS: [u8; 7] = [1, 2, 5, 10, 20, 30, 60];

struct Viewer {
    out: Box<dyn Write>,
    probe: Option<(u16, u16)>,
    paint_bytes: u64,
    link: BufWriter<ChildStdin>,
    label: String,
    preset: String,
    s: Settings,
    cols: usize,
    rows: usize,
    cw: usize,
    ch: usize,
    cell_override: Option<(usize, usize)>,
    fb: Vec<u8>,
    fw: usize,
    fh: usize,
    mode: Option<Mode>,
    lut: Lut,
    pal: Vec<[u8; 3]>,
    sixel: Sixel,
    idx: Vec<u8>,
    dirty: Vec<bool>,
    menu: bool,
    pointer: (u16, u16),
    quit: bool,
    rx_bytes: Arc<AtomicU64>,
    tx_bytes: u64,
    stat_at: Instant,
    stat_rx: u64,
    stat_frames: u32,
    frames: u32,
    kbps: f64,
    fps_now: f64,
    paint_ms: f64,
}

impl Viewer {
    fn send(&mut self, m: ToAgent) -> io::Result<()> {
        let (k, p) = m.encode();
        self.tx_bytes += 5 + p.len() as u64;
        write_msg(&mut self.link, k, &p)?;
        self.link.flush()
    }

    fn measure(&mut self) {
        if let Some((w, h)) = self.probe {
            (self.cw, self.ch) = self.cell_override.unwrap_or((10, 20));
            (self.cols, self.rows) = (w as usize / self.cw, h as usize / self.ch + 1);
            (self.s.view_w, self.s.view_h) = (w, h);
            return;
        }
        let (c, r) = terminal::size().unwrap_or((80, 24));
        self.cols = c.max(10) as usize;
        self.rows = r.max(3) as usize;
        let (mut cw, mut ch) = (10, 20);
        if let Ok(ws) = terminal::window_size() {
            if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 {
                cw = ws.width as usize / ws.columns as usize;
                ch = ws.height as usize / ws.rows as usize;
            }
        }
        (self.cw, self.ch) = self.cell_override.unwrap_or((cw, ch));
        self.s.view_w = (self.cols * self.cw).min(u16::MAX as usize) as u16;
        self.s.view_h = ((self.rows - 1) * self.ch).min(u16::MAX as usize) as u16;
    }

    /// The frame is shown top-left, scaled locally to fit the image area.
    fn display(&self) -> (usize, usize) {
        if self.fw == 0 || self.fh == 0 {
            return (0, 0);
        }
        let (vw, vh) = (self.s.view_w as f64, self.s.view_h as f64);
        let k = (vw / self.fw as f64).min(vh / self.fh as f64);
        (((self.fw as f64 * k) as usize).max(1), ((self.fh as f64 * k) as usize).max(1))
    }

    fn apply(&mut self, f: Frame) -> io::Result<()> {
        let started = Instant::now();
        let (w, h) = (f.w as usize, f.h as usize);
        let mut out = Vec::with_capacity(1 << 16);
        out.extend_from_slice(b"\x1b[?2026h");
        if (w, h) != (self.fw, self.fh) {
            self.fb = vec![0; w * h * 3];
            (self.fw, self.fh) = (w, h);
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
        if self.mode != Some(f.mode) {
            let shown = if f.mode == Mode::True { Mode::Color256 } else { f.mode };
            self.pal = f.mode.palette();
            self.lut = Lut::new(&shown.palette());
            self.mode = Some(f.mode);
        }
        let (dw, dh) = self.display();
        let img_rows = self.rows - 1;
        self.dirty.resize(self.cols * img_rows, false);
        for t in &f.tiles {
            let (tx, ty, tw, th) = (t.x as usize, t.y as usize, t.w as usize, t.h as usize);
            let px = unpack(&t.data, f.mode, tw * th).ok_or_else(|| io::Error::other("short tile"))?;
            for row in 0..th {
                for col in 0..tw {
                    let i = row * tw + col;
                    let rgb = if f.mode == Mode::True {
                        [px[i * 3], px[i * 3 + 1], px[i * 3 + 2]]
                    } else {
                        *self.pal.get(px[i] as usize).unwrap_or(&[0, 0, 0])
                    };
                    let o = ((ty + row) * w + tx + col) * 3;
                    self.fb[o..o + 3].copy_from_slice(&rgb);
                }
            }
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
        self.frames += 1;
        self.stat_frames += 1;
        self.send(ToAgent::Ack(f.id))
    }

    /// Repaints dirty cells, merged into rectangles of whole cells.
    fn paint(&mut self, out: &mut Vec<u8>, dw: usize, dh: usize) {
        let (cols, img_rows) = (self.cols, self.rows - 1);
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
                        let frow = (dy * self.fh / dh) * self.fw;
                        let line = &mut self.idx[py * w..(py + 1) * w];
                        for (px, slot) in line.iter_mut().enumerate() {
                            let dx = x0 + px;
                            if dx >= dw {
                                break;
                            }
                            let o = (frow + dx * self.fw / dw) * 3;
                            *slot = self.lut.get(self.fb[o], self.fb[o + 1], self.fb[o + 2]);
                        }
                    }
                }
                let _ = write!(out, "\x1b[{};{}H", r + 1, c + 1);
                let pal = if self.mode == Some(Mode::True) { Mode::Color256.palette() } else { self.pal.clone() };
                self.sixel.encode(out, w, h, &self.idx, &pal);
                c = c1;
            }
        }
    }

    fn status(&mut self) -> io::Result<()> {
        let text = if self.menu {
            " menu: q quit | 0-4 preset | m colour | +/- scale | f fps | r refresh | ] send Ctrl+] ".to_string()
        } else {
            format!(
                " {} | {} {} {}% {}fps | {}x{} | rx {:.1} KB/s tx {:.1} KB | {:.0} fps paint {:.1} ms | Ctrl+] menu ",
                self.label,
                self.preset,
                self.s.mode.name(),
                self.s.scale,
                self.s.fps,
                self.fw,
                self.fh,
                self.kbps,
                self.tx_bytes as f64 / 1024.0,
                self.fps_now,
                self.paint_ms,
            )
        };
        let text: String = text.chars().take(self.cols).collect();
        let pad = self.cols.saturating_sub(text.chars().count());
        write!(self.out, "\x1b[{};1H\x1b[0;7m{text}{}\x1b[0m", self.rows, " ".repeat(pad))?;
        self.out.flush()
    }

    fn tick(&mut self) -> io::Result<()> {
        let dt = self.stat_at.elapsed().as_secs_f64();
        if dt < 1.0 {
            return Ok(());
        }
        let rx = self.rx_bytes.load(Ordering::Relaxed);
        self.kbps = (rx - self.stat_rx) as f64 / 1024.0 / dt;
        self.fps_now = self.stat_frames as f64 / dt;
        (self.stat_rx, self.stat_frames, self.stat_at) = (rx, 0, Instant::now());
        self.status()
    }

    fn push_settings(&mut self) -> io::Result<()> {
        self.send(ToAgent::Settings(self.s))?;
        self.status()
    }

    fn menu_key(&mut self, ev: &event::KeyEvent) -> io::Result<()> {
        use crossterm::event::KeyCode::Char;
        self.menu = false;
        match ev.code {
            Char('q') => self.quit = true,
            Char(d @ '0'..='4') => {
                let p = &PRESETS[d as usize - '0' as usize];
                self.preset = p.name.into();
                (self.s.scale, self.s.mode, self.s.fps) = (p.scale, p.mode, p.fps);
            }
            Char('m') => {
                self.s.mode = self.s.mode.next();
                self.preset = "custom".into();
            }
            Char('+') | Char('=') => {
                self.s.scale = (self.s.scale + 10).min(100);
                self.preset = "custom".into();
            }
            Char('-') => {
                self.s.scale = self.s.scale.saturating_sub(10).max(20);
                self.preset = "custom".into();
            }
            Char('f') => {
                let i = FPS_STEPS.iter().position(|&f| f >= self.s.fps).unwrap_or(0);
                self.s.fps = FPS_STEPS[(i + 1) % FPS_STEPS.len()];
                self.preset = "custom".into();
            }
            Char('r') => {
                self.full_redraw()?;
                return self.send(ToAgent::Refresh);
            }
            Char(']') => return self.send(ToAgent::Key { keysym: ']' as u32, mods: crate::proto::MOD_CTRL }),
            _ => return self.status(),
        }
        self.push_settings()
    }

    fn full_redraw(&mut self) -> io::Result<()> {
        self.fw = 0;
        self.fh = 0;
        write!(self.out, "\x1b[0m\x1b[2J")?;
        self.out.flush()
    }

    fn on_term(&mut self, e: Event) -> io::Result<()> {
        match e {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if self.menu {
                    return self.menu_key(&k);
                }
                if keys::is_menu(&k) {
                    self.menu = true;
                    return self.status();
                }
                if let Some((keysym, mods)) = keys::keysym(&k) {
                    self.send(ToAgent::Key { keysym, mods })?;
                }
            }
            Event::Mouse(m) => {
                let (dw, dh) = self.display();
                let dx = m.column as usize * self.cw + self.cw / 2;
                let dy = m.row as usize * self.ch + self.ch / 2;
                if dw == 0 || dx >= dw || dy >= dh {
                    return Ok(());
                }
                let p = ((dx * self.fw / dw) as u16, (dy * self.fh / dh) as u16);
                if p != self.pointer || !matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
                    self.pointer = p;
                    self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                }
                let button = |b: MouseButton| match b {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                };
                match m.kind {
                    MouseEventKind::Down(b) => self.send(ToAgent::Button { button: button(b), pressed: true })?,
                    MouseEventKind::Up(b) => self.send(ToAgent::Button { button: button(b), pressed: false })?,
                    MouseEventKind::ScrollDown => self.send(ToAgent::Scroll { steps: 1 })?,
                    MouseEventKind::ScrollUp => self.send(ToAgent::Scroll { steps: -1 })?,
                    _ => {}
                }
            }
            Event::Resize(..) => {
                self.measure();
                self.dirty.clear();
                self.full_redraw()?;
                self.push_settings()?;
                self.send(ToAgent::Refresh)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn spawn(opt: &Options) -> Result<Child, String> {
    let mut cmd = if opt.local {
        let mut c = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        c.args(["--agent", "--capture", &opt.capture]);
        c
    } else {
        let host = opt.host.as_deref().ok_or("no host given")?;
        let mut c = Command::new("ssh");
        c.arg("-T").args(&opt.ssh_args).arg(host);
        c.arg(format!("{} --capture {}", opt.remote, opt.capture));
        c
    };
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.spawn().map_err(|e| format!("could not start the agent: {e}"))
}

pub fn run(opt: Options) -> Result<(), String> {
    let mut child = spawn(&opt)?;
    let link = BufWriter::new(child.stdin.take().unwrap());
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let errlog = Arc::new(Mutex::new(Vec::new()));
    {
        let errlog = errlog.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = stderr.read(&mut buf) {
                errlog.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }
    let rx_bytes = Arc::new(AtomicU64::new(0));
    let (tx, rx) = mpsc::channel();
    {
        let (tx, n) = (tx.clone(), rx_bytes.clone());
        thread::spawn(move || {
            let reader = BufReader::with_capacity(1 << 16, Counting { inner: stdout, n });
            let mut dec = match zstd::stream::read::Decoder::with_buffer(reader) {
                Ok(d) => d,
                Err(e) => return drop(tx.send(Ev::Closed(e.to_string()))),
            };
            let _ = dec.window_log_max(27);
            loop {
                let msg = match read_msg(&mut dec) {
                    Ok(Some((k, p))) => ToClient::decode(k, &p).map_err(|e| e.to_string()),
                    Ok(None) => Err("connection closed".into()),
                    Err(e) => Err(e.to_string()),
                };
                let stop = msg.is_err();
                if tx.send(msg.map_or_else(Ev::Closed, Ev::Net)).is_err() || stop {
                    break;
                }
            }
        });
    }
    let agent_log = |errlog: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&errlog.lock().unwrap()).trim().to_string();

    let (desk, backend) = match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ev::Net(ToClient::Hello { w, h, backend })) => ((w, h), backend),
        Ok(Ev::Net(ToClient::Error(e))) => return Err(e),
        Ok(Ev::Closed(e)) => {
            let _ = child.wait();
            thread::sleep(Duration::from_millis(100));
            return Err(format!("{e}\n{}", agent_log(&errlog)));
        }
        _ => {
            let _ = child.kill();
            return Err("the agent did not answer within 60 s".into());
        }
    };

    let p = &PRESETS[opt.preset.min(PRESETS.len() - 1)];
    let mut s = Settings { view_w: 0, view_h: 0, scale: p.scale, mode: p.mode, fps: p.fps };
    let custom = opt.mode.is_some() || opt.scale.is_some() || opt.fps.is_some();
    s.mode = opt.mode.unwrap_or(s.mode);
    s.scale = opt.scale.unwrap_or(s.scale).clamp(10, 100);
    s.fps = opt.fps.unwrap_or(s.fps).clamp(1, 60);
    let host = opt.host.clone().unwrap_or_else(|| "local".into());
    let mut v = Viewer {
        out: if opt.probe.is_some() { Box::new(io::sink()) } else { Box::new(io::stdout()) },
        probe: opt.probe.as_ref().map(|p| p.1),
        paint_bytes: 0,
        link,
        label: format!("{host} {}x{} {backend}", desk.0, desk.1),
        preset: if custom { "custom".into() } else { p.name.into() },
        s,
        cols: 0,
        rows: 0,
        cw: 10,
        ch: 20,
        cell_override: opt.cell,
        fb: Vec::new(),
        fw: 0,
        fh: 0,
        mode: None,
        lut: Lut::new(&[[0, 0, 0]]),
        pal: Vec::new(),
        sixel: Sixel::new(),
        idx: Vec::new(),
        dirty: Vec::new(),
        menu: false,
        pointer: (u16::MAX, u16::MAX),
        quit: false,
        rx_bytes,
        tx_bytes: 0,
        stat_at: Instant::now(),
        stat_rx: 0,
        stat_frames: 0,
        frames: 0,
        kbps: 0.0,
        fps_now: 0.0,
        paint_ms: 0.0,
    };

    let interactive = opt.probe.is_none();
    let deadline = opt.probe.as_ref().map(|p| Instant::now() + Duration::from_secs_f64(p.0));
    if interactive {
        terminal::enable_raw_mode().map_err(|e| e.to_string())?;
        let _ = execute!(v.out, terminal::EnterAlternateScreen, cursor::Hide, event::EnableMouseCapture);
        let tx = tx.clone();
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
                Ok(Ev::Net(ToClient::Error(e))) => {
                    failure = Some(e);
                    break;
                }
                Ok(Ev::Net(_)) => {}
                Ok(Ev::Closed(e)) => {
                    failure = Some(e);
                    break;
                }
                Ok(Ev::Term(e)) => v.on_term(e)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            if !typing.is_empty() && Instant::now() >= next_key {
                if typing.len() == keys_total {
                    type_start = (v.rx_bytes.load(Ordering::Relaxed), v.frames);
                }
                let c = typing.pop().unwrap();
                v.send(ToAgent::Key { keysym: c as u32, mods: 0 })?;
                next_key = Instant::now() + Duration::from_millis(500);
            }
            v.tick()?;
        }
        if keys_total > 0 {
            let rx = v.rx_bytes.load(Ordering::Relaxed) - type_start.0;
            eprintln!(
                "probe: typed {keys_total} keys, {} frames, {:.0} bytes per key (includes idle since the first key)",
                v.frames - type_start.1,
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
            "probe: frame {}x{} {}, paint output {:.1} KB, last paint {:.1} ms",
            v.fw,
            v.fh,
            v.mode.map_or("-", |m| m.name()),
            v.paint_bytes as f64 / 1024.0,
            v.paint_ms
        );
        if let Some(path) = opt.probe.as_ref().and_then(|p| p.2.as_ref()) {
            let mut ppm = format!("P6\n{} {}\n255\n", v.fw, v.fh).into_bytes();
            ppm.extend_from_slice(&v.fb);
            std::fs::write(path, ppm).map_err(|e| e.to_string())?;
        }
    }
    drop(v.link);
    let _ = child.kill();
    let _ = child.wait();
    let total = v.rx_bytes.load(Ordering::Relaxed);
    eprintln!("desk: {} frames, received {:.1} KB, sent {:.1} KB", v.frames, total as f64 / 1024.0, v.tx_bytes as f64 / 1024.0);
    if let Err(e) = r {
        failure.get_or_insert(e.to_string());
    }
    match failure {
        Some(e) => Err(format!("{e}\n{}", agent_log(&errlog))),
        None => Ok(()),
    }
}
