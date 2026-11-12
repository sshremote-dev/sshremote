//! Viewer side shared by the terminal and window front ends: the agent process,
//! the decoded framebuffer, settings, statistics and the auto preset.

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::color::{Mode, unpack};
use crate::proto::{Frame, MOD_CTRL, PRESETS, Settings, ToAgent, ToClient, read_msg, write_msg};
use crate::transport::{self, Link, Target};
pub use crate::options::Options;


/// Saves a screenshot sent by the agent; the text tells where, or why not.
pub fn save_snapshot(png: &[u8]) -> String {
    let path = crate::shot::save_path();
    match std::fs::write(&path, png) {
        Ok(()) => format!("Screenshot saved to {}", path.display()),
        Err(e) => format!("Screenshot: {e}"),
    }
}

pub enum Ev {
    Net(ToClient),
    Closed(String),
    Term(crossterm::event::Event),
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

pub struct Session {
    link: Link,
    writer: Option<BufWriter<Box<dyn Write + Send>>>,
    errlog: Arc<Mutex<Vec<u8>>>,
    rx_bytes: Arc<AtomicU64>,
    pub tx_bytes: u64,
    pub desk: (u16, u16),
    pub backend: String,
    waker: Waker,
}

type Waker = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

impl Options {
    pub fn target(&self) -> Result<Target, String> {
        if self.local {
            return Ok(Target::Local { capture: self.capture.clone() });
        }
        if let Some(addr) = self.host.as_deref().and_then(crate::net::Address::parse) {
            let password = self.password.clone().ok_or("a sshremote:// address needs --password")?;
            return Ok(Target::Direct { addr, password });
        }
        Ok(Target::Ssh {
            host: self.host.clone().ok_or("no host given")?,
            port: None,
            args: self.ssh_args.clone(),
            remote: self.remote.clone(),
            capture: self.capture.clone(),
            password: None,
            unattended: false,
        })
    }
}


impl Session {
    /// Starts the agent and waits for its hello.
    pub fn connect(opt: &Options) -> Result<(Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>), String> {
        Session::open(&opt.target()?, opt.gui)
    }

    /// Opens `target` and waits for the agent's hello; `hidden` keeps helper consoles out of sight.
    pub fn open(target: &Target, hidden: bool) -> Result<(Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>), String> {
        let mut link = transport::open(target, hidden)?;
        let reader = std::mem::replace(&mut link.reader, Box::new(io::empty()));
        let writer = std::mem::replace(&mut link.writer, Box::new(io::sink()));
        let mut errors = std::mem::replace(&mut link.errors, Box::new(io::empty()));
        let errlog = Arc::new(Mutex::new(Vec::new()));
        {
            let errlog = errlog.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = errors.read(&mut buf) {
                    errlog.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
        }
        let rx_bytes = Arc::new(AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        let waker: Waker = Arc::new(Mutex::new(None));
        {
            let (tx, n, waker) = (tx.clone(), rx_bytes.clone(), waker.clone());
            thread::spawn(move || {
                let reader = BufReader::with_capacity(1 << 16, Counting { inner: reader, n });
                let mut dec = match zstd::stream::read::Decoder::with_buffer(reader) {
                    Ok(d) => d,
                    Err(e) => return drop(tx.send(Ev::Closed(e.to_string()))),
                };
                let _ = dec.window_log_max(27);
                let mut told = std::collections::HashSet::new();
                loop {
                    let msg = match read_msg(&mut dec) {
                        Ok(Some((k, p))) => match ToClient::decode(k, &p) {
                            Ok(m) => Ok(m),
                            // A newer agent's message: it carries its length, so the rest still
                            // reads; said once per kind.
                            Err(e) if crate::proto::is_unknown(&e) => {
                                if !told.insert(k) {
                                    continue;
                                }
                                Ok(ToClient::Notice(format!(
                                    "The SSH host's sshremote is newer than this one and sent something it does not know (message {k}); update it here."
                                )))
                            }
                            Err(e) => Err(e.to_string()),
                        },
                        Ok(None) => Err("connection closed".into()),
                        Err(e) => Err(e.to_string()),
                    };
                    let stop = msg.is_err();
                    if tx.send(msg.map_or_else(Ev::Closed, Ev::Net)).is_err() || stop {
                        break;
                    }
                    if let Some(w) = waker.lock().unwrap().as_ref() {
                        w();
                    }
                }
            });
        }
        let mut sess = Session {
            link,
            writer: Some(BufWriter::new(writer)),
            errlog,
            rx_bytes,
            tx_bytes: 0,
            desk: (0, 0),
            backend: String::new(),
            waker,
        };
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ev::Net(ToClient::Hello { w, h, backend })) => {
                sess.desk = (w, h);
                sess.backend = backend;
                Ok((sess, tx, rx))
            }
            Ok(Ev::Net(ToClient::Error(e))) => Err(e),
            Ok(Ev::Closed(e)) => {
                sess.link.wait();
                thread::sleep(Duration::from_millis(100));
                let log = sess.log();
                Err(if log.is_empty() { e } else { log })
            }
            _ => {
                sess.link.close();
                Err("the agent did not answer within 60 s".into())
            }
        }
    }

    pub fn send(&mut self, m: ToAgent) -> io::Result<()> {
        let (k, p) = m.encode();
        self.tx_bytes += 5 + p.len() as u64;
        let w = self.writer.as_mut().ok_or_else(|| io::Error::other("closed"))?;
        write_msg(w, k, &p)?;
        w.flush()
    }

    /// A re-announced desktop (another monitor or shared area): the new size and backend.
    pub fn hello(&mut self, w: u16, h: u16, backend: String) {
        self.desk = (w, h);
        self.backend = backend;
    }

    /// Starts writing a download; one that cannot be written is called off at the agent too, so
    /// its queue moves on.
    pub fn fetch_start(&mut self, inbox: &mut crate::files::Inbox, id: u32, size: u64, dir: &std::path::Path, name: &str) -> io::Result<Option<crate::files::Note>> {
        let note = inbox.start_in(id, size, dir, name);
        if note.is_some() {
            self.send(ToAgent::FetchCancel { id, tag: 0 })?;
        }
        Ok(note)
    }

    /// Writes a chunk of a download and acknowledges the progress to the agent; a download that
    /// failed here (a full disk) is called off there.
    pub fn fetch_data(&mut self, inbox: &mut crate::files::Inbox, id: u32, data: &[u8]) -> io::Result<Option<crate::files::Note>> {
        let note = inbox.data(id, data);
        match note {
            Some(crate::files::Note::Progress { id, received }) => self.send(ToAgent::FetchAck { id, received })?,
            Some(crate::files::Note::Done { .. }) => self.send(ToAgent::FetchCancel { id, tag: 0 })?,
            None => {}
        }
        Ok(note)
    }

    /// Called from the network thread after each message, so a window can repaint at once.
    pub fn on_message(&self, f: impl Fn() + Send + 'static) {
        *self.waker.lock().unwrap() = Some(Box::new(f));
    }

    pub fn rx(&self) -> u64 {
        self.rx_bytes.load(Ordering::Relaxed)
    }

    /// The agent's stderr so far.
    pub fn log(&self) -> String {
        String::from_utf8_lossy(&self.errlog.lock().unwrap()).trim().to_string()
    }

    pub fn close(&mut self) {
        self.writer = None;
        self.link.close();
    }
}

/// Exact copy of the remote screen in RGB.
#[derive(Default)]
pub struct Framebuffer {
    pub rgb: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub mode: Option<Mode>,
    pal: Vec<[u8; 3]>,
    player: crate::video::Player,
    /// The video could not be decoded (a decoder started over met frames of the stream before
    /// it): the viewer asks the agent to start the picture over, with a fresh stream.
    pub need_key: bool,
}

impl Framebuffer {
    /// Returns true when the frame size changed and everything must be repainted.
    pub fn apply(&mut self, f: &Frame) -> io::Result<bool> {
        let (w, h) = (f.w as usize, f.h as usize);
        let resized = (w, h) != (self.w, self.h);
        if resized {
            self.rgb = vec![0; w * h * 3];
            (self.w, self.h) = (w, h);
        }
        if self.mode != Some(f.mode) {
            self.pal = f.mode.palette();
            self.mode = Some(f.mode);
        }
        if !resized {
            for m in &f.moves {
                crate::scroll::apply(&mut self.rgb, w, 3, m);
            }
        }
        if let Some(v) = &f.video {
            let r = (v.x as usize, v.y as usize, v.w as usize, v.h as usize);
            if r.0 + r.2 > w || r.1 + r.3 > h {
                return Err(io::Error::other("a video region outside the picture"));
            }
            match self.player.decode(r, &v.data) {
                Ok(Some(px)) => {
                    for row in 0..r.3 {
                        let o = ((r.1 + row) * w + r.0) * 3;
                        self.rgb[o..o + r.2 * 3].copy_from_slice(&px[row * r.2 * 3..(row + 1) * r.2 * 3]);
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    self.player = crate::video::Player::default();
                    self.need_key = true;
                }
            }
        }
        for t in &f.tiles {
            let (tx, ty, tw, th) = (t.x as usize, t.y as usize, t.w as usize, t.h as usize);
            if tx + tw > w || ty + th > h {
                return Err(io::Error::other("a tile outside the picture"));
            }
            let px = unpack(&t.data, f.mode, tw, th).ok_or_else(|| io::Error::other("short tile"))?;
            for row in 0..th {
                let o = ((ty + row) * w + tx) * 3;
                let line = &mut self.rgb[o..o + tw * 3];
                if f.mode == Mode::True {
                    line.copy_from_slice(&px[row * tw * 3..(row + 1) * tw * 3]);
                } else {
                    for (col, rgb) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                        rgb.copy_from_slice(self.pal.get(px[row * tw + col] as usize).unwrap_or(&[0, 0, 0]));
                    }
                }
            }
        }
        Ok(resized)
    }

    pub fn ppm(&self) -> Vec<u8> {
        let mut ppm = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        ppm.extend_from_slice(&self.rgb);
        ppm
    }

    /// The frame scaled to fit `view` without distortion, shown top-left; never enlarged unless `upscale`.
    pub fn fit(&self, view: (usize, usize), upscale: bool) -> (usize, usize) {
        if self.w == 0 || self.h == 0 {
            return (0, 0);
        }
        let k = (view.0 as f64 / self.w as f64).min(view.1 as f64 / self.h as f64);
        let k = if upscale { k } else { k.min(1.0) };
        (((self.w as f64 * k) as usize).max(1), ((self.h as f64 * k) as usize).max(1))
    }
}

const FPS_STEPS: [u8; 7] = [1, 2, 5, 10, 20, 30, 60];
const SETTLE: Duration = Duration::from_secs(3);

/// Steps between presets from the link's queueing delay, and from traffic when a budget is set.
/// A link that answers fast and evenly gets the richest picture; one whose delay grows gets less.
pub struct Auto {
    /// Traffic to stay under, in KB/s; without one only the delay counts.
    budget: Option<f64>,
    pub level: usize,
    changed: Instant,
    over: u32,
    under: u32,
    hold: [u32; PRESETS.len()],
    calm_since: Instant,
    climbed: bool,
    ceiling: Option<(usize, Instant)>,
}

/// A round trip this short is a local network: auto climbs at every chance.
const LAN_RTT: f64 = 30.0;

impl Auto {
    fn new(budget: Option<f64>, level: usize) -> Auto {
        let now = Instant::now();
        Auto {
            budget,
            level,
            changed: now,
            over: 0,
            under: 0,
            hold: [5; PRESETS.len()],
            calm_since: now,
            climbed: false,
            ceiling: None,
        }
    }

    /// Called once a second with the traffic and round trips; returns the new level when it should
    /// change.
    fn step(&mut self, kbps: f64, rtt: f64, min_rtt: f64) -> Option<usize> {
        if self.changed.elapsed() < SETTLE {
            return None;
        }
        if self.ceiling.is_some_and(|(_, until)| Instant::now() > until) {
            self.ceiling = None;
        }
        let slow = rtt > (3.0 * min_rtt).max(250.0);
        let quiet = rtt < (1.5 * min_rtt).max(100.0);
        if std::mem::take(&mut self.climbed) && slow && self.level > 0 {
            // A climb that changes the colour mode resends the whole screen; if the link queued it
            // up, the level stays out of reach for a while.
            self.ceiling = Some((self.level - 1, Instant::now() + Duration::from_secs(600)));
            return self.change(self.level - 1);
        }
        let over_budget = self.budget.is_some_and(|b| kbps > b);
        let under_budget = self.budget.is_none_or(|b| kbps < 0.3 * b);
        if over_budget || slow {
            self.over += 1;
            self.under = 0;
        } else if under_budget && quiet {
            self.under += 1;
            self.over = 0;
        } else {
            self.over = 0;
            self.under = 0;
        }
        if self.calm_since.elapsed() > Duration::from_secs(60) {
            self.hold = [5; PRESETS.len()];
            self.calm_since = Instant::now();
        }
        let next = if self.over >= 2 && self.level > 0 {
            // Climbing back to the level that just failed waits longer each time.
            self.hold[self.level - 1] = (self.hold[self.level - 1] * 2).min(60);
            self.calm_since = Instant::now();
            self.level - 1
        } else if (self.under >= self.hold[self.level] || self.under > 0 && min_rtt < LAN_RTT)
            && self.level + 1 < PRESETS.len()
            && self.ceiling.is_none_or(|(top, _)| self.level < top)
        {
            self.level + 1
        } else {
            return None;
        };
        self.change(next)
    }

    fn change(&mut self, next: usize) -> Option<usize> {
        self.climbed = next > self.level;
        (self.level, self.over, self.under, self.changed) = (next, 0, 0, Instant::now());
        Some(next)
    }
}

pub enum Action {
    None,
    Quit,
    Refresh,
    Zoom,
    /// The next key goes out with the Win (Super) key held; Space sends Win alone.
    ArmWin,
    /// Shows the remote host's next monitor.
    NextMonitor,
    Settings,
    Send(ToAgent),
}

/// Settings, the menu that edits them, and live statistics.
pub struct Control {
    pub s: Settings,
    pub preset: String,
    pub auto: Option<Auto>,
    /// The richest colour mode the front end can show.
    max_mode: Mode,
    budget: Option<f64>,
    epoch: Instant,
    ping: Option<(u32, Instant)>,
    stat_at: Instant,
    stat_rx: u64,
    stat_frames: u32,
    pub frames: u32,
    pub kbps: f64,
    pub fps_now: f64,
    pub rtt: f64,
    min_rtt: f64,
}

impl Control {
    pub fn new(opt: &Options, max_mode: Mode) -> Control {
        let level = opt.preset.unwrap_or(2).min(PRESETS.len() - 1);
        let p = &PRESETS[level];
        let mode = if p.mode.to_u8() > max_mode.to_u8() { max_mode } else { p.mode };
        let mut s = Settings { view_w: 0, view_h: 0, scale: p.scale, mode, fps: p.fps };
        let custom = opt.mode.is_some() || opt.scale.is_some() || opt.fps.is_some();
        s.mode = opt.mode.unwrap_or(s.mode);
        s.scale = opt.scale.unwrap_or(s.scale).clamp(10, 100);
        s.fps = opt.fps.unwrap_or(s.fps).clamp(1, 60);
        let now = Instant::now();
        Control {
            s,
            preset: if custom { "custom".into() } else { p.name.into() },
            auto: (opt.preset.is_none() && !custom).then(|| Auto::new(opt.budget, level)),
            max_mode,
            budget: opt.budget,
            epoch: now,
            ping: None,
            stat_at: now,
            stat_rx: 0,
            stat_frames: 0,
            frames: 0,
            kbps: 0.0,
            fps_now: 0.0,
            rtt: 0.0,
            min_rtt: f64::MAX,
        }
    }

    fn use_preset(&mut self, i: usize) {
        let p = &PRESETS[i];
        self.preset = p.name.into();
        let mode = if p.mode.to_u8() > self.max_mode.to_u8() { self.max_mode } else { p.mode };
        (self.s.scale, self.s.mode, self.s.fps) = (p.scale, mode, p.fps);
    }

    fn custom(&mut self) {
        self.preset = "custom".into();
        self.auto = None;
    }

    /// The settings in effect, e.g. "Auto: 256 colours, 100%, 20 fps".
    pub fn describe(&self) -> String {
        let s = format!("{}, {}%, {} fps", self.s.mode.label(), self.s.scale, self.s.fps);
        if self.auto.is_some() { format!("Auto: {s}") } else { s }
    }

    pub fn preset_label(&self) -> String {
        match &self.auto {
            Some(_) => format!("auto {}", self.preset),
            None => self.preset.clone(),
        }
    }

    /// A key pressed after the menu prefix.
    pub fn menu(&mut self, c: char) -> Action {
        match c {
            'q' => return Action::Quit,
            d @ '0'..='4' => {
                self.auto = None;
                self.use_preset(d as usize - '0' as usize);
            }
            'a' => {
                let level = PRESETS.iter().position(|p| p.name == self.preset).unwrap_or(2);
                self.auto = Some(Auto::new(self.budget, level));
                self.use_preset(level);
            }
            'm' => {
                self.s.mode = self.s.mode.next();
                self.custom();
            }
            '+' | '=' => {
                self.s.scale = (self.s.scale + 10).min(100);
                self.custom();
            }
            '-' => {
                self.s.scale = self.s.scale.saturating_sub(10).max(20);
                self.custom();
            }
            'f' => {
                let i = FPS_STEPS.iter().position(|&f| f >= self.s.fps).unwrap_or(0);
                self.s.fps = FPS_STEPS[(i + 1) % FPS_STEPS.len()];
                self.custom();
            }
            'r' => return Action::Refresh,
            'z' => return Action::Zoom,
            'w' => return Action::ArmWin,
            's' => return Action::NextMonitor,
            ']' => {
                return Action::Send(ToAgent::Key { keysym: ']' as u32, mods: MOD_CTRL });
            }
            _ => return Action::None,
        }
        Action::Settings
    }

    /// `full` marks a frame that resent most of the screen; such bursts come from
    /// starting, resizing or a mode change, not from what the screen is doing, so auto waits them out.
    pub fn frame_done(&mut self, full: bool) {
        self.frames += 1;
        self.stat_frames += 1;
        if full && let Some(a) = self.auto.as_mut() {
            a.changed = Instant::now();
            a.over = 0;
        }
    }

    pub fn pong(&mut self, t: u32) {
        if let Some((sent, at)) = self.ping
            && sent == t
        {
            self.rtt = at.elapsed().as_secs_f64() * 1000.0;
            self.min_rtt = self.min_rtt.min(self.rtt);
            self.ping = None;
        }
    }

    /// Once a second: refresh the statistics, ping, and let auto pick a preset.
    /// Returns true when the statistics changed; settings changes are sent here.
    pub fn tick(&mut self, sess: &mut Session) -> io::Result<bool> {
        let dt = self.stat_at.elapsed().as_secs_f64();
        if dt < 1.0 {
            return Ok(false);
        }
        let rx = sess.rx();
        self.kbps = (rx - self.stat_rx) as f64 / 1024.0 / dt;
        self.fps_now = self.stat_frames as f64 / dt;
        (self.stat_rx, self.stat_frames, self.stat_at) = (rx, 0, Instant::now());
        match self.ping {
            // An unanswered ping is at least as slow as its age.
            Some((_, at)) => self.rtt = self.rtt.max(at.elapsed().as_secs_f64() * 1000.0),
            None => {
                let t = self.epoch.elapsed().as_millis() as u32;
                self.ping = Some((t, Instant::now()));
                sess.send(ToAgent::Ping(t))?;
            }
        }
        let min_rtt = if self.min_rtt == f64::MAX { self.rtt } else { self.min_rtt };
        if let Some(level) = self.auto.as_mut().and_then(|a| a.step(self.kbps, self.rtt, min_rtt)) {
            self.use_preset(level);
            sess.send(ToAgent::Settings(self.s))?;
        }
        Ok(true)
    }

    pub fn status(&self, label: &str, fw: usize, fh: usize) -> String {
        format!(
            "{label} | {} {} {}% {}fps | {fw}x{fh} | rx {:.1} KB/s | rtt {:.0} ms | {:.0} fps",
            self.preset_label(),
            self.s.mode.name(),
            self.s.scale,
            self.s.fps,
            self.kbps,
            self.rtt,
            self.fps_now,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged(budget: Option<f64>, level: usize) -> Auto {
        let mut a = Auto::new(budget, level);
        a.changed -= SETTLE;
        a
    }

    #[test]
    fn auto_steps_down_on_delay_and_on_traffic_over_a_budget() {
        let mut a = aged(Some(100.0), 3);
        assert_eq!(a.step(500.0, 40.0, 40.0), None);
        assert_eq!(a.step(500.0, 40.0, 40.0), Some(2));
        let mut a = aged(None, 3);
        assert_eq!(a.step(1.0, 900.0, 40.0), None);
        assert_eq!(a.step(1.0, 900.0, 40.0), Some(2));
        // Without a budget, heavy traffic on a link that keeps up does not lower the picture.
        let mut a = aged(None, PRESETS.len() - 1);
        for _ in 0..10 {
            assert_eq!(a.step(5000.0, 40.0, 40.0), None);
        }
    }

    #[test]
    fn auto_caps_a_level_whose_climb_floods_the_link() {
        let mut a = aged(None, 2);
        for _ in 0..4 {
            a.step(1.0, 40.0, 40.0);
        }
        assert_eq!(a.step(1.0, 40.0, 40.0), Some(3));
        a.changed -= SETTLE;
        // The whole screen resent queued up on the link.
        assert_eq!(a.step(1.0, 600.0, 40.0), Some(2));
        a.changed -= SETTLE;
        for _ in 0..30 {
            assert_eq!(a.step(1.0, 40.0, 40.0), None);
        }
    }

    #[test]
    fn auto_climbs_slower_after_a_failure() {
        let mut a = aged(Some(100.0), 2);
        for _ in 0..4 {
            assert_eq!(a.step(1.0, 40.0, 40.0), None);
        }
        assert_eq!(a.step(1.0, 40.0, 40.0), Some(3));
        a.changed -= SETTLE;
        a.step(500.0, 40.0, 40.0);
        assert_eq!(a.step(500.0, 40.0, 40.0), Some(2));
        a.changed -= SETTLE;
        for _ in 0..9 {
            assert_eq!(a.step(1.0, 40.0, 40.0), None);
        }
        assert_eq!(a.step(1.0, 40.0, 40.0), Some(3));
    }

    #[test]
    fn auto_climbs_at_once_on_a_local_network() {
        let mut a = aged(None, 2);
        assert_eq!(a.step(800.0, 2.0, 1.0), Some(3), "full colour on the first quiet second");
        a.changed -= SETTLE;
        assert_eq!(a.step(800.0, 2.0, 1.0), Some(4));
    }
}
