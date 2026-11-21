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
    /// Shared with the thread that keeps a quiet session up (`keep_up`).
    writer: Arc<Mutex<Option<BufWriter<Box<dyn Write + Send>>>>>,
    /// When something was last sent (seconds since the session began).
    sent_at: Arc<AtomicU64>,
    started: Instant,
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


/// The agent commands for a Windows host, by the shell it runs: cmd with the service's copy, any
/// shell with one on the PATH, PowerShell with the service's copy.
const WINDOWS_AGENT: [&str; 3] = [
    r#""%ProgramFiles%\SSH Remote\sshremote.exe" --agent"#,
    "sshremote --agent",
    r#"& "$env:ProgramFiles\SSH Remote\sshremote.exe" --agent"#,
];

/// The agent command that worked for each SSH host, when not the usual one.
static WORKED: Mutex<Option<std::collections::HashMap<String, String>>> = Mutex::new(None);

/// Lines ssh writes that are no error: OpenSSH 10's warning about an older server's key
/// exchange (Windows' OpenSSH 9), a host key just added to the known ones.
fn ssh_chatter(line: &str) -> bool {
    let l = line.trim();
    l.is_empty() || l == "**" || l.starts_with("** ") || l.starts_with("Warning: Permanently added")
}

/// Whether the usual agent command failed as Windows' shells fail it: cmd takes `PATH=...` as its
/// own command and ends without a word or a byte (the stream ends before its first frame, and
/// with nothing on stderr the error is the reader's own); PowerShell and cmd say a name is not
/// recognised.
fn windows_shell(error: &str) -> bool {
    ["connection closed", "incomplete frame"].contains(&error) || error.contains("is not recognized as")
}

impl Session {
    /// Starts the agent and waits for its hello.
    #[cfg(test)]
    pub fn connect(opt: &Options) -> Result<(Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>), String> {
        Session::open(&opt.target()?, opt.gui)
    }

    /// Opens `target` and waits for the agent's hello; `hidden` keeps helper consoles out of sight.
    /// An SSH host given the usual agent command that turns out to run Windows' shell (which
    /// takes `PATH=...` as its own command and says nothing) is tried with Windows' commands, and
    /// the one that worked is kept for the next time.
    pub fn open(target: &Target, hidden: bool) -> Result<(Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>), String> {
        Session::open_noting(target, hidden, &|_| {})
    }

    /// `open`, telling `note` what the agent says before its hello (that it waits for the person
    /// at the screen to let the viewer in, say).
    pub fn open_noting(target: &Target, hidden: bool, note: &dyn Fn(&str)) -> Result<(Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>), String> {
        let Target::Ssh { host, remote, .. } = target else { return Session::open_once(target, hidden, note) };
        if remote != crate::options::REMOTE {
            return Session::open_once(target, hidden, note);
        }
        let with = |cmd: &str| {
            let mut t = target.clone();
            if let Target::Ssh { remote, .. } = &mut t {
                *remote = cmd.to_string();
            }
            t
        };
        let kept = WORKED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().get(host).cloned();
        if let Some(cmd) = kept
            && let Ok(s) = Session::open_once(&with(&cmd), hidden, note)
        {
            return Ok(s);
        }
        let first = match Session::open_once(target, hidden, note) {
            Ok(s) => return Ok(s),
            Err(e) => e,
        };
        if !windows_shell(&first) {
            return Err(first);
        }
        let mut last = first;
        for cmd in WINDOWS_AGENT {
            match Session::open_once(&with(cmd), hidden, note) {
                Ok(s) => {
                    WORKED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().insert(host.clone(), cmd.to_string());
                    return Ok(s);
                }
                Err(e) => last = e,
            }
        }
        Err(format!("{last}\nsshremote was not found on that Windows computer: install SSH Remote there (This computer, Start with the computer), or put sshremote.exe on its PATH."))
    }

    fn open_once(target: &Target, hidden: bool, note: &dyn Fn(&str)) -> Result<(Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>), String> {
        let mut link = transport::open(target, hidden)?;
        // Asked without a password: this side's own code goes with every word from there, so the
        // two can be compared (never one the other side sends, which someone between could copy).
        let code = link.code();
        let say = |t: &str| match &code {
            Some(c) => note(&format!("{t}\nYour code is {c}: the person there must see the same one.")),
            None => note(t),
        };
        if code.is_some() {
            say("Asking the person at that computer to let you in\u{2026}");
        }
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
            writer: Arc::new(Mutex::new(Some(BufWriter::new(writer)))),
            sent_at: Arc::new(AtomicU64::new(0)),
            started: Instant::now(),
            errlog,
            rx_bytes,
            tx_bytes: 0,
            desk: (0, 0),
            backend: String::new(),
            waker,
        };
        sess.keep_up();
        // What comes before the hello is said (the person at the screen is asked, which may take
        // them a minute) and waited past.
        let mut until = Instant::now() + Duration::from_secs(60);
        let first = loop {
            match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Ev::Net(ToClient::Notice(t))) => {
                    say(&t);
                    until = until.max(Instant::now() + Duration::from_secs(75));
                }
                other => break other,
            }
        };
        match first {
            Ok(Ev::Net(ToClient::Hello { w, h, backend })) => {
                sess.desk = (w, h);
                sess.backend = backend;
                Ok((sess, tx, rx))
            }
            Ok(Ev::Net(ToClient::Error(e))) => Err(e),
            Ok(Ev::Closed(e)) => {
                sess.link.wait();
                thread::sleep(Duration::from_millis(100));
                // What ssh said, but for its warnings that are no error (a newer ssh's word on
                // an older server's key exchange, a host key just learnt).
                let log = sess.log();
                let said: Vec<&str> = log.lines().filter(|l| !ssh_chatter(l)).collect();
                Err(if said.is_empty() { e } else { said.join("\n") })
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
        self.sent_at.store(self.link_age(), Ordering::Relaxed);
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let w = writer.as_mut().ok_or_else(|| io::Error::other("closed"))?;
        write_msg(w, k, &p)?;
        w.flush()
    }

    fn link_age(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Pings a session that has sent nothing for a while, from a thread of its own: a window
    /// not drawn (minimized) runs no frames to ping from, and the agent ends a session it hears
    /// nothing from for two minutes. Ends with the session.
    fn keep_up(&self) {
        let (writer, sent_at, started) = (Arc::downgrade(&self.writer), self.sent_at.clone(), self.started);
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(15));
                let Some(writer) = writer.upgrade() else { return };
                let mut writer = writer.lock().unwrap_or_else(|e| e.into_inner());
                let Some(w) = writer.as_mut() else { return };
                let now = started.elapsed().as_secs();
                if now.saturating_sub(sent_at.load(Ordering::Relaxed)) < 45 {
                    continue;
                }
                let (k, p) = ToAgent::Ping(0).encode();
                if write_msg(w, k, &p).and_then(|_| w.flush()).is_err() {
                    return;
                }
                sent_at.store(now, Ordering::Relaxed);
            }
        });
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
    /// For a session by ID: where its straight path goes, or None while it runs through the relay.
    pub fn straight(&self) -> Option<Option<std::net::SocketAddr>> {
        self.link.straight()
    }

    pub fn log(&self) -> String {
        String::from_utf8_lossy(&self.errlog.lock().unwrap()).trim().to_string()
    }

    pub fn close(&mut self) {
        *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
    colours: crate::color::Colours,
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
        if let Some((fresh, changed)) = &f.palette {
            self.colours.update(*fresh, changed);
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
            let px = match &f.palette {
                Some(_) => match self.colours.unpack(&t.data, tw, th) {
                    Some(px) => px,
                    // A colour this side was never told: ask for a whole new picture.
                    None => {
                        self.need_key = true;
                        continue;
                    }
                },
                None => unpack(&t.data, f.mode, tw, th).ok_or_else(|| io::Error::other("short tile"))?,
            };
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
/// Auto starts in true colour, and a slow link costs it frames (the agent waits for each to
/// arrive) before colours: it goes no lower than 256 colours unless a budget asks for it.
const AUTO_START: usize = 3;
const AUTO_FLOOR: usize = 2;

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
        let floor = if self.budget.is_some() { 0 } else { AUTO_FLOOR };
        if std::mem::take(&mut self.climbed) && slow && self.level > floor {
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
        let next = if self.over >= 2 && self.level > floor {
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

/// A picture unchanged this long is pinged this seldom.
const STILL: Duration = Duration::from_secs(3);
/// The most pictures a second a viewer may ask for: as many as a fast monitor shows.
pub const MOST_FPS: u8 = 120;
/// How often a still screen is pinged: the link keeps itself alive (UDP keepalives, a 120 s idle
/// limit), so this keeps only the round trip shown fresh, and wakes a window drawn for it rarely.
const STILL_PING: Duration = Duration::from_secs(10);

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
    /// When the last ping went, and when the picture last changed: a still screen is pinged
    /// less often, so it costs no more than a few bytes a second.
    pinged: Instant,
    busy: Instant,
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
        Control::with(opt.preset, opt.budget, (opt.mode, opt.scale, opt.fps), max_mode)
    }

    /// Auto, or preset `preset`, with nothing else asked (a browser's start).
    #[cfg(feature = "web")]
    pub fn for_preset(preset: Option<usize>, max_mode: Mode) -> Control {
        Control::with(preset, None, (None, None, None), max_mode)
    }

    /// Colours, resolution and frame rate of the user's own; auto ends.
    #[cfg(feature = "web")]
    pub fn set_custom(&mut self, mode: Mode, scale: u8, fps: u8) {
        self.s.mode = if mode.to_u8() > self.max_mode.to_u8() { self.max_mode } else { mode };
        self.s.scale = scale.clamp(10, 100);
        self.s.fps = fps.clamp(1, MOST_FPS);
        self.custom();
    }

    fn with(preset: Option<usize>, budget: Option<f64>, (mode, scale, fps): (Option<Mode>, Option<u8>, Option<u8>), max_mode: Mode) -> Control {
        let opt = (preset, budget, mode, scale, fps);
        let level = opt.0.unwrap_or(AUTO_START).min(PRESETS.len() - 1);
        let p = &PRESETS[level];
        let mode = if p.mode.to_u8() > max_mode.to_u8() { max_mode } else { p.mode };
        let mut s = Settings { view_w: 0, view_h: 0, scale: p.scale, mode, fps: p.fps, formats: crate::proto::FORMATS };
        let custom = opt.2.is_some() || opt.3.is_some() || opt.4.is_some();
        s.mode = opt.2.unwrap_or(s.mode);
        s.scale = opt.3.unwrap_or(s.scale).clamp(10, 100);
        s.fps = opt.4.unwrap_or(s.fps).clamp(1, MOST_FPS);
        let now = Instant::now();
        Control {
            s,
            preset: if custom { "custom".into() } else { p.name.into() },
            auto: (opt.0.is_none() && !custom).then(|| Auto::new(opt.1, level)),
            max_mode,
            budget: opt.1,
            epoch: now,
            ping: None,
            pinged: now,
            busy: now,
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

    /// The settings in effect, e.g. "Auto: true colour, 100%, 30 fps".
    pub fn describe(&self) -> String {
        let s = format!("{}, {}% size, {} fps", self.s.mode.label(), self.s.scale, self.s.fps);
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

    /// How soon `tick` has work: each second while pictures come or a ping is out; on a still
    /// screen not before the next ping.
    pub fn next_tick(&self) -> Duration {
        let mut at = self.stat_at + Duration::from_secs(1);
        if self.ping.is_none() && self.busy.elapsed() > STILL {
            at = at.max(self.pinged + STILL_PING);
        }
        at.saturating_duration_since(Instant::now()).max(Duration::from_millis(10))
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
        if self.stat_frames > 0 {
            self.busy = Instant::now();
        }
        (self.stat_rx, self.stat_frames, self.stat_at) = (rx, 0, Instant::now());
        let still = self.busy.elapsed() > STILL && self.pinged.elapsed() < STILL_PING;
        match self.ping {
            // An unanswered ping is at least as slow as its age.
            Some((_, at)) => self.rtt = self.rtt.max(at.elapsed().as_secs_f64() * 1000.0),
            None if still => {}
            None => {
                let t = self.epoch.elapsed().as_millis() as u32;
                self.ping = Some((t, Instant::now()));
                self.pinged = Instant::now();
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

    /// Pictures damaged on the way (or made up by a host) are refused or drawn wrong, never a crash.
    #[test]
    fn damaged_pictures_never_bring_the_viewer_down() {
        use crate::proto::{Frame, Tile};
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        let (w, h) = (40, 24);
        let img = crate::shared::Image { w, h, stride: w * 4, data: (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect() };
        let moves = vec![crate::scroll::Move { x: 0, y: 4, w: 40, h: 12, dy: -3, dx: 0 }, crate::scroll::Move { x: 6, y: 2, w: 20, h: 5, dy: 0, dx: 4 }];
        let rects = [crate::color::Rect { x: 0, y: 0, w: 40, h: 24 }, crate::color::Rect { x: 5, y: 3, w: 9, h: 7 }];
        let tile = |r: &crate::color::Rect, data| Tile { x: r.x as u16, y: r.y as u16, w: r.w as u16, h: r.h as u16, data };
        let mut wires = Vec::new();
        for mode in [Mode::True, Mode::Color256, Mode::Color16, Mode::Gray16, Mode::Gray4, Mode::Mono] {
            let p = crate::color::Reducer::new(mode).reduce_into(&img, Vec::new());
            let tiles = rects.iter().map(|r| tile(r, crate::color::pack(&p, r.x, r.y, r.w, r.h))).collect();
            wires.push(ToClient::Frame(Frame { id: 1, w: w as u16, h: h as u16, mode, moves: moves.clone(), palette: None, tiles, video: None }).encode());
        }
        let p = crate::color::Reducer::new(Mode::True).reduce_into(&img, Vec::new());
        for wide in [false, true] {
            let (fresh, changed, packed) = crate::color::Palette::default().pack(&p, &rects, true, wide);
            let tiles = rects.iter().zip(packed).map(|(r, d)| tile(r, d)).collect();
            wires.push(ToClient::Frame(Frame { id: 2, w: w as u16, h: h as u16, mode: Mode::True, moves: moves.clone(), palette: Some((fresh, changed)), tiles, video: None }).encode());
        }
        let rounds = std::env::var("SSHREMOTE_DAMAGE_ROUNDS").ok().and_then(|n| n.parse().ok()).unwrap_or(300);
        let mut kept = Framebuffer::default();
        for (k, p) in &wires {
            let Ok(ToClient::Frame(whole)) = ToClient::decode(*k, p) else { panic!("a picture") };
            assert!(Framebuffer::default().apply(&whole).is_ok());
            for _ in 0..rounds {
                let mut d = p.clone();
                for _ in 0..1 + next() % 3 {
                    let at = next() % (d.len() + 1);
                    match next() % 4 {
                        0 => d.truncate(at),
                        1 if at < d.len() => d[at] = next() as u8,
                        2 => d.insert(at, next() as u8),
                        _ if at + 2 <= d.len() => d[at..at + 2].copy_from_slice(&((next() as u16) >> (next() % 16)).to_le_bytes()),
                        _ => {}
                    }
                }
                if let Ok(ToClient::Frame(f)) = ToClient::decode(*k, &d) {
                    let _ = Framebuffer::default().apply(&f);
                    let _ = kept.apply(&f);
                }
            }
        }
    }

    /// A camera listed after the screen is picked like a monitor: the viewer hears its size and
    /// gets its pictures. SSHREMOTE_EXE=... SSHREMOTE_TEST_CAMERA=1 cargo test -- --ignored
    /// a_camera_is_picked_like_a_monitor (GStreamer's videotestsrc)
    #[test]
    #[ignore]
    fn a_camera_is_picked_like_a_monitor() {
        if std::env::var_os("SSHREMOTE_TEST_CAMERA").is_none() {
            return;
        }
        let target = Target::Local { capture: "synthetic".into() };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let wait = |want: &dyn Fn(&ToClient) -> bool| -> ToClient {
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                if let Ok(Ev::Net(m)) = rx.recv_timeout(Duration::from_millis(200))
                    && want(&m)
                {
                    return m;
                }
            }
            panic!("no answer in time");
        };
        let ToClient::Monitors { list, .. } = wait(&|m| matches!(m, ToClient::Monitors { .. })) else { unreachable!() };
        let at = list.iter().position(|m| m.name == "Camera: Test camera").expect("the camera listed");
        sess.send(ToAgent::SelectMonitor(at as u8)).unwrap();
        let until = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        let m = loop {
            assert!(Instant::now() < until, "no new size: {seen:?}\n{}", sess.log());
            if let Ok(Ev::Net(m)) = rx.recv_timeout(Duration::from_millis(200)) {
                if matches!(m, ToClient::Hello { w: 640, .. } | ToClient::Notice(_)) {
                    break m;
                }
                if !matches!(m, ToClient::Frame(_)) {
                    seen.push(format!("{m:?}").chars().take(80).collect::<String>());
                }
            }
        };
        assert!(matches!(m, ToClient::Hello { w: 640, h: 480, .. }), "{m:?}");
        // Pictures come once the viewer says how it shows them.
        let s = crate::proto::Settings { view_w: 640, view_h: 480, scale: 100, mode: crate::color::Mode::True, fps: 15, formats: crate::proto::FORMATS };
        sess.send(ToAgent::Settings(s)).unwrap();
        let ToClient::Frame(f) = wait(&|m| matches!(m, ToClient::Frame(_))) else { unreachable!() };
        assert!(f.w > 0, "a picture of it");
    }

    /// A Mac shown to a viewer whose size is no even fraction of its screen: pictures come, of
    /// the size asked for. As the app allowed to record the screen: SSHREMOTE_EXE=...
    /// SSHREMOTE_MAC_TEST=1 cargo test -- --ignored a_mac_is_shown_at_any_view_size
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn a_mac_is_shown_at_any_view_size() {
        if std::env::var_os("SSHREMOTE_MAC_TEST").is_none() {
            return;
        }
        let target = Target::Local { capture: "mac".into() };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        assert_eq!(sess.backend, "mac", "{}", sess.log());
        let s = crate::proto::Settings { view_w: 1001, view_h: 777, scale: 100, mode: crate::color::Mode::True, fps: 15, formats: crate::proto::FORMATS };
        sess.send(ToAgent::Settings(s)).unwrap();
        let until = Instant::now() + Duration::from_secs(20);
        let f = loop {
            assert!(Instant::now() < until, "no picture came");
            if let Ok(Ev::Net(ToClient::Frame(f))) = rx.recv_timeout(Duration::from_millis(200)) {
                break f;
            }
        };
        println!("screen {}x{}, picture {}x{}", sess.desk.0, sess.desk.1, f.w, f.h);
        sess.close();
    }

    /// A device that came in by ID with the two-step code and asked to be trusted is given a
    /// token, and comes in by it next time without the code; another token is a wrong code.
    /// Against remote access running by ID: SSHR_TRUST_ADDR=sshremote://ID@RELAY
    /// SSHR_TRUST_PW=... SSHR_TRUST_SECRET=... cargo test -- --ignored a_trusted_device
    #[test]
    #[ignore]
    fn a_trusted_device_comes_in_without_the_code() {
        use crate::net::{CODE_SEP, NOT_TRUSTED, TRUST_MARK};
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let addr = crate::net::Address::parse(&var("SSHR_TRUST_ADDR")).expect("an address");
        let (pw, secret) = (var("SSHR_TRUST_PW"), var("SSHR_TRUST_SECRET"));
        let open = |password: String| Session::open(&Target::Direct { addr: addr.clone(), password }, true).map(|(s, _, rx)| (s, rx));
        assert!(open(pw.clone()).err().is_some_and(|e| e.contains(crate::net::CODE_WANTED)), "the code is wanted");
        let (mut sess, rx) = open(format!("{pw}{CODE_SEP}{}", crate::totp::now(&secret))).expect("in with the code");
        sess.send(ToAgent::TrustMe).unwrap();
        let until = Instant::now() + Duration::from_secs(20);
        let token = loop {
            assert!(Instant::now() < until, "no token came");
            if let Ok(Ev::Net(ToClient::Trusted(t))) = rx.recv_timeout(Duration::from_millis(200)) {
                break t;
            }
        };
        assert_eq!(token.len(), 64);
        sess.close();
        drop((sess, rx));
        std::thread::sleep(Duration::from_secs(1));
        let (mut sess, _rx) = open(format!("{pw}{CODE_SEP}{TRUST_MARK}{token}")).expect("in by the token, without the code");
        sess.close();
        assert_eq!(open(format!("{pw}{CODE_SEP}{TRUST_MARK}{}", "0".repeat(64))).err(), Some(NOT_TRUSTED.to_string()), "another token is not taken, and said so");
        // Kept in the password store, it is given by itself; forgotten there while in by it
        // (SSHR_TRUST_CFG: the computer's settings folder), out; one no longer trusted there is
        // forgotten here, and the code asked for.
        if crate::vault::backend().is_some() {
            // The relay takes about five calls a minute from one address.
            std::thread::sleep(Duration::from_secs(61));
            let name = crate::vault::trust_name(&crate::access::address(&addr.id, &addr.relay));
            crate::vault::set(&name, &token).unwrap();
            let (mut sess, rx) = open(pw.clone()).expect("in with the password alone, by the token kept");
            if let Ok(cfg) = std::env::var("SSHR_TRUST_CFG") {
                let exe = std::env::var("SSHREMOTE_EXE").expect("set SSHREMOTE_EXE");
                let out = std::process::Command::new(exe).args(["remote-access", "trusted", "forget"]).env("XDG_CONFIG_HOME", cfg).output().unwrap();
                assert!(out.status.success());
                let until = Instant::now() + Duration::from_secs(15);
                loop {
                    assert!(Instant::now() < until, "still in after the device was forgotten");
                    if let Ok(Ev::Closed(_) | Ev::Net(ToClient::Error(_))) = rx.recv_timeout(Duration::from_millis(200)) {
                        break;
                    }
                }
            }
            sess.close();
            drop(rx);
            std::thread::sleep(Duration::from_secs(1));
            crate::vault::set(&name, &"1".repeat(64)).unwrap();
            assert_eq!(open(pw.clone()).err(), Some(NOT_TRUSTED.to_string()));
            assert_eq!(crate::vault::get(&name), None, "forgotten");
        }
    }

    /// A computer whose screen cannot be shared keeps the rest of the session: it says so, and
    /// its files and terminal work. SSHREMOTE_EXE=target/debug/sshremote cargo test -- --ignored
    /// no_screen_leaves_the_rest
    #[test]
    #[ignore]
    fn no_screen_leaves_the_rest() {
        let target = Target::Local { capture: "no-such-screen".into() };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        assert_eq!(sess.backend, "none");
        let wait = |want: &dyn Fn(&ToClient) -> bool| -> ToClient {
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                if let Ok(Ev::Net(m)) = rx.recv_timeout(Duration::from_millis(200))
                    && want(&m)
                {
                    return m;
                }
            }
            panic!("no answer in time");
        };
        let ToClient::Notice(n) = wait(&|m| matches!(m, ToClient::Notice(_))) else { unreachable!() };
        assert!(n.contains("screen cannot be shown"), "{n}");
        sess.send(ToAgent::ListDir { path: String::new() }).unwrap();
        let ToClient::DirList { error, .. } = wait(&|m| matches!(m, ToClient::DirList { .. })) else { unreachable!() };
        assert!(error.is_empty(), "{error}");
        sess.send(ToAgent::TermOpen { id: 1, cols: 80, rows: 24 }).unwrap();
        sess.send(ToAgent::TermInput { id: 1, data: b"echo no-screen-$((6*7))\r".to_vec() }).unwrap();
        let mut out = String::new();
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until && !out.contains("no-screen-42") {
            if let Ok(Ev::Net(ToClient::TermOutput { data, .. })) = rx.recv_timeout(Duration::from_millis(200)) {
                out.push_str(&String::from_utf8_lossy(&data));
            }
        }
        assert!(out.contains("no-screen-42"), "the shell answered: {out}");
    }

    /// A GNOME host's screen turned on its side during a session is followed: the viewer is told
    /// the new size, and frames come at it, scaled to fit as before. Needs SSHREMOTE_GNOME_HOST
    /// (an ssh alias of a host signed in to GNOME with one monitor, which is turned and turned
    /// back), and optionally SSHREMOTE_GNOME_ENV (variables pointing at that session, such as a
    /// A voice call on GNOME: it rings in the window there, is answered there (by the window's
    /// driver: SSHREMOTE_DRIVE names its command file on the host, SSHREMOTE_ANSWER_AT where the
    /// Answer button is), and the microphone there is heard here; then it is hung up.
    #[test]
    #[ignore]
    fn a_gnome_call_rings_is_answered_and_heard() {
        use crate::proto::ToAgent;
        let (Ok(host), Ok(drive)) = (std::env::var("SSHREMOTE_GNOME_HOST"), std::env::var("SSHREMOTE_DRIVE")) else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let at = std::env::var("SSHREMOTE_ANSWER_AT").unwrap_or_default();
        let target = Target::Ssh {
            host: host.clone(),
            port: None,
            args: Vec::new(),
            remote: format!("env {env} SSHREMOTE_DRIVE={drive} {agent} --agent"),
            capture: "gnome".into(),
            password: None,
            unattended: true,
        };
        let sh = |cmd: &str| drop(std::process::Command::new("ssh").arg(&host).arg(cmd).output());
        sh(&format!(": > {drive}"));
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let next = |want: &dyn Fn(&ToClient) -> bool, secs: u64| loop {
            match rx.recv_timeout(Duration::from_secs(secs)) {
                Ok(Ev::Net(m)) if want(&m) => return Some(m),
                Ok(Ev::Net(_)) => {}
                _ => return None,
            }
        };
        sess.send(ToAgent::Call(true)).unwrap();
        assert_eq!(next(&|m| matches!(m, ToClient::CallState(_)), 20), Some(ToClient::CallState(crate::chat::CALL_RINGING)));
        std::thread::sleep(Duration::from_secs(3));
        if at.is_empty() {
            sh(&format!("echo 'shot ringing' >> {drive}"));
            std::thread::sleep(Duration::from_secs(2));
            return;
        }
        sh(&format!("echo 'click {at}' >> {drive}"));
        assert_eq!(next(&|m| matches!(m, ToClient::CallState(_)), 20), Some(ToClient::CallState(crate::chat::CALL_TALKING)));
        let heard = next(&|m| matches!(m, ToClient::Voice(_) | ToClient::Notice(_)), 10);
        assert!(matches!(heard, Some(ToClient::Voice(_))), "the microphone there is heard: {heard:?}");
        let mut enc = opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip).unwrap();
        for _ in 0..25 {
            sess.send(ToAgent::Voice(enc.encode_vec_float(&[0.0; 960], 1500).unwrap())).unwrap();
        }
        sess.send(ToAgent::Call(false)).unwrap();
        assert_eq!(next(&|m| matches!(m, ToClient::CallState(_)), 20), Some(ToClient::CallState(crate::chat::CALL_ENDED)));
    }

    /// Files through the clipboard, on a host whose clipboard is an X display's (SSHREMOTE_CLIP_DISPLAY,
    /// say an Xvfb; with xclip there): files sent for a paste land on its clipboard as files, and
    /// files copied there are told to the viewer.
    #[test]
    #[ignore]
    fn a_gnome_files_cross_the_clipboard() {
        use crate::proto::ToAgent;
        let (Ok(host), Ok(display)) = (std::env::var("SSHREMOTE_GNOME_HOST"), std::env::var("SSHREMOTE_CLIP_DISPLAY")) else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let home = format!("/tmp/sshremote-clip-test-{}", std::process::id());
        let target = Target::Ssh {
            host: host.clone(),
            port: None,
            args: Vec::new(),
            remote: format!("env {env} DISPLAY={display} HOME={home} {agent} --agent"),
            capture: "gnome".into(),
            password: None,
            unattended: true,
        };
        let sh = |cmd: &str| {
            let out = std::process::Command::new("ssh").arg(&host).arg(cmd).output().expect("ssh");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let next = |want: &dyn Fn(&ToClient) -> bool| loop {
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ev::Net(m)) if want(&m) => return m,
                Ok(Ev::Net(_)) => {}
                other => panic!("{:?}", other.err()),
            }
        };
        let batch = format!("test-{}", std::process::id());
        sess.send(ToAgent::FileStart { id: 1, size: 5, dir: format!("{}{batch}", crate::files::CLIPBOARD), name: "hello.txt".into() }).unwrap();
        sess.send(ToAgent::FileData { id: 1, data: b"hello".to_vec() }).unwrap();
        sess.send(ToAgent::FileEnd { id: 1 }).unwrap();
        let ToClient::FileDone { ok, text, .. } = next(&|m| matches!(m, ToClient::FileDone { .. })) else { unreachable!() };
        assert!(ok, "{text}");
        assert!(text.ends_with(&format!("{batch}/hello.txt")), "{text}");
        sess.send(ToAgent::PasteFiles { batch, names: vec!["hello.txt".into()] }).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let on_board = sh(&format!("DISPLAY={display} xclip -o -selection clipboard -t text/uri-list"));
        assert!(on_board.contains("hello.txt") && on_board.starts_with("file://"), "{on_board}");
        // Copied there: the viewer is told, with the size.
        sh(&format!("printf hi! > {home}/copied.txt; printf 'file://{home}/copied.txt\\r\\n' | DISPLAY={display} xclip -selection clipboard -t text/uri-list -i > /dev/null 2>&1"));
        let ToClient::ClipFiles(list) = next(&|m| matches!(m, ToClient::ClipFiles(_))) else { unreachable!() };
        assert_eq!(list, vec![crate::proto::ClipFile { path: format!("{home}/copied.txt"), size: 3, dir: false }]);
        sh(&format!("rm -rf {home}"));
    }

    /// The computer itself on GNOME: it says what it can do, refuses to keep out its keyboard or
    /// hide its screen, and with SSHREMOTE_GNOME_LOCK=1 locks (the test session stays locked).
    #[test]
    #[ignore]
    fn a_gnome_computer_says_what_it_can_do() {
        use crate::hostctl::{BLOCK, LOCK, LOCK_ON_LEAVE, QUERY, RESTART, bit};
        use crate::proto::ToAgent;
        let Ok(host) = std::env::var("SSHREMOTE_GNOME_HOST") else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let target = Target::Ssh {
            host,
            port: None,
            args: Vec::new(),
            remote: format!("env {env} {agent} --agent"),
            capture: "gnome".into(),
            password: None,
            unattended: true,
        };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let next = |want: &dyn Fn(&ToClient) -> bool| loop {
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ev::Net(m)) if want(&m) => return m,
                Ok(Ev::Net(_)) => {}
                other => panic!("{:?}", other.err()),
            }
        };
        let state = |m: &ToClient| matches!(m, ToClient::HostState { .. });
        sess.send(ToAgent::Host { act: QUERY, on: false }).unwrap();
        assert_eq!(next(&state), ToClient::HostState { can: bit(RESTART) | bit(LOCK) | bit(LOCK_ON_LEAVE), on: 0 });
        sess.send(ToAgent::Host { act: BLOCK, on: true }).unwrap();
        let ToClient::Notice(why) = next(&|m| matches!(m, ToClient::Notice(_))) else { unreachable!() };
        assert!(why.contains("not on this computer"), "{why}");
        assert_eq!(next(&state), ToClient::HostState { can: bit(RESTART) | bit(LOCK) | bit(LOCK_ON_LEAVE), on: 0 });
        sess.send(ToAgent::Host { act: LOCK_ON_LEAVE, on: true }).unwrap();
        assert!(matches!(next(&state), ToClient::HostState { on, .. } if on == bit(LOCK_ON_LEAVE)));
        sess.send(ToAgent::Host { act: LOCK_ON_LEAVE, on: false }).unwrap();
        assert!(matches!(next(&state), ToClient::HostState { on: 0, .. }));
        if std::env::var("SSHREMOTE_GNOME_LOCK").is_ok() {
            sess.send(ToAgent::Host { act: LOCK, on: true }).unwrap();
            let ToClient::Notice(said) = next(&|m| matches!(m, ToClient::Notice(_))) else { unreachable!() };
            assert_eq!(said, "The computer is locked.");
        }
    }

    /// private test one) and SSHREMOTE_GNOME_AGENT (the program there).
    #[test]
    #[ignore]
    fn a_gnome_screen_turned_on_its_side_is_followed() {
        use crate::proto::{Settings, ToAgent};
        let Ok(host) = std::env::var("SSHREMOTE_GNOME_HOST") else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let target = Target::Ssh {
            host: host.clone(),
            port: None,
            args: Vec::new(),
            remote: format!("env {env} {agent} --agent"),
            capture: "gnome".into(),
            password: None,
            unattended: true,
        };
        // Mutter's own call, with the one monitor and its current mode; 1 applies it for now only.
        let turn = |transform: u32| {
            let script = r#"S=$(gdbus call --session -d org.gnome.Mutter.DisplayConfig -o /org/gnome/Mutter/DisplayConfig -m org.gnome.Mutter.DisplayConfig.GetCurrentState)
serial=$(echo "$S" | sed -E 's/^\(uint32 ([0-9]+).*/\1/')
con=$(echo "$S" | sed -E "s/^[^']*'([^']+)'.*/\1/")
mode=$(echo "$S" | grep -oE "\('[^']+', [0-9]+, [0-9]+, [0-9.]+, [0-9.]+, \[[^]]*\], \{[^}]*'is-current': <true>" | head -1 | sed -E "s/^\('([^']+)'.*/\1/")
gdbus call --session -d org.gnome.Mutter.DisplayConfig -o /org/gnome/Mutter/DisplayConfig -m org.gnome.Mutter.DisplayConfig.ApplyMonitorsConfig $serial 1 "[(0, 0, 1.0, uint32 TURN, true, [('$con', '$mode', @a{sv} {})])]" "@a{sv} {}""#
                .replace("TURN", &transform.to_string());
            let out = std::process::Command::new("ssh").arg(&host).arg(format!("env {env} sh -s")).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(script.as_bytes())?;
                c.wait_with_output()
            }).expect("ssh");
            assert!(out.status.success(), "turning the screen: {}", String::from_utf8_lossy(&out.stderr));
        };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let (w, h) = (sess.desk.0 as u16, sess.desk.1 as u16);
        assert!(w > h, "a screen wider than tall to start with: {w}x{h}");
        sess.send(ToAgent::Settings(Settings { view_w: 640, view_h: 640, scale: 100, mode: crate::color::Mode::Gray16, fps: 10, formats: crate::proto::FORMATS })).unwrap();
        // The next frame for which `want` holds, acknowledging every frame meanwhile; the sizes
        // announced on the way are returned with it.
        let mut frame = |want: &dyn Fn(u16, u16) -> bool| -> (Vec<(u16, u16)>, (u16, u16)) {
            let (mut hellos, mut seen) = (Vec::new(), Vec::new());
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ev::Net(ToClient::Hello { w, h, .. })) => hellos.push((w, h)),
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        seen.push((f.w, f.h));
                        if want(f.w, f.h) {
                            return (hellos, (f.w, f.h));
                        }
                    }
                    Ok(Ev::Closed(e)) => panic!("the agent ended: {e}"),
                    _ => {}
                }
            }
            seen.dedup();
            panic!("no such frame in time; sizes announced: {hellos:?}, frames: {seen:?}");
        };
        let (_, first) = frame(&|w, h| w > h);
        assert!(first.0 <= 640, "scaled to the view: {first:?}");
        turn(1);
        // Turned back however this ends.
        struct Back<F: Fn(u32)>(F);
        impl<F: Fn(u32)> Drop for Back<F> {
            fn drop(&mut self) {
                (self.0)(0);
            }
        }
        let back = Back(&turn);
        let (hellos, turned) = frame(&|w, h| h > w);
        drop(back);
        assert_eq!(hellos.last(), Some(&(h, w)), "the turned size is announced");
        assert!(turned.1 <= 640 && turned.1 > turned.0, "scaled to the view, standing: {turned:?}");
        let (hellos, back) = frame(&|w, h| w > h);
        assert_eq!(hellos.last(), Some(&(w, h)), "and the size it came back to");
        assert_eq!(back, first, "the same picture size as before");
        sess.close();
    }

    /// A picture with something in it (not one flat colour), kept as NAME.ppm in
    /// SSHREMOTE_TEST_SHOTS when that is set.
    fn a_picture(fb: &Framebuffer, name: &str) {
        if let Ok(dir) = std::env::var("SSHREMOTE_TEST_SHOTS") {
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}.ppm")), fb.ppm()).unwrap();
        }
        let shades: std::collections::HashSet<&[u8]> = fb.rgb.chunks(3).collect();
        assert!(shades.len() > 2, "the {name} has a picture: {} shades in {}x{}", shades.len(), fb.w, fb.h);
    }

    /// Sharing a GNOME host's active window sends that window at its own size, scaled to fit, and
    /// the whole screen comes back as it was. Needs SSHREMOTE_GNOME_HOST with a window focused
    /// there (and SSHREMOTE_GNOME_ENV, SSHREMOTE_GNOME_AGENT as above).
    #[test]
    #[ignore]
    fn a_gnome_window_shared_alone_comes_at_its_own_size() {
        use crate::proto::{Settings, ToAgent};
        use crate::shared::Share;
        let Ok(host) = std::env::var("SSHREMOTE_GNOME_HOST") else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let target = Target::Ssh {
            host,
            port: None,
            args: Vec::new(),
            remote: format!("env {env} {agent} --agent"),
            capture: "gnome".into(),
            password: None,
            unattended: true,
        };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let screen = (sess.desk.0 as u16, sess.desk.1 as u16);
        sess.send(ToAgent::Settings(Settings { view_w: 4000, view_h: 4000, scale: 100, mode: crate::color::Mode::Gray16, fps: 10, formats: crate::proto::FORMATS })).unwrap();
        // Sends `first`, then waits for a frame for which `want` holds.
        let mut fb = Framebuffer::default();
        let mut next = |first: Option<ToAgent>, want: &dyn Fn(u16, u16) -> bool| -> (Vec<(u16, u16)>, (u16, u16)) {
            if let Some(m) = first {
                sess.send(m).unwrap();
            }
            let (mut hellos, mut seen) = (Vec::new(), Vec::new());
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ev::Net(ToClient::Hello { w, h, .. })) => hellos.push((w, h)),
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        fb.apply(&f).unwrap();
                        seen.push((f.w, f.h));
                        if want(f.w, f.h) {
                            return (hellos, (f.w, f.h));
                        }
                    }
                    Ok(Ev::Closed(e)) => panic!("the agent ended: {e}"),
                    _ => {}
                }
            }
            seen.dedup();
            panic!("no such frame in time; sizes announced: {hellos:?}, frames: {seen:?}");
        };
        next(None, &|w, h| (w, h) == screen);
        let (hellos, window) = next(Some(ToAgent::Share(Share::ActiveWindow)), &|w, h| (w, h) != screen);
        assert_eq!(hellos.last(), Some(&window), "the window's size is announced and sent as it is");
        assert!(window.0 < screen.0 || window.1 < screen.1, "a window, not the screen: {window:?}");
        drop(next);
        a_picture(&fb, "window");
        let mut next = |first: Option<ToAgent>, want: &dyn Fn(u16, u16) -> bool| {
            sess.send(first.unwrap()).unwrap();
            let until = Instant::now() + Duration::from_secs(20);
            let mut hellos = Vec::new();
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ev::Net(ToClient::Hello { w, h, .. })) => hellos.push((w, h)),
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        if want(f.w, f.h) {
                            return (hellos, (f.w, f.h));
                        }
                    }
                    _ => {}
                }
            }
            panic!("no such frame in time");
        };
        let (hellos, back) = next(Some(ToAgent::Share(Share::Screen)), &|w, h| (w, h) == screen);
        assert_eq!((hellos.last(), back), (Some(&screen), screen));
        drop(next);
        sess.close();
    }

    /// GNOME's pointer comes apart to a viewer that draws it: its picture and where it is, and
    /// moving it alone sends no pictures; to one that does not, it is drawn into them. Needs
    /// SSHREMOTE_GNOME_HOST (and SSHREMOTE_GNOME_ENV, SSHREMOTE_GNOME_AGENT as above).
    #[test]
    #[ignore]
    fn a_gnome_pointer_comes_apart_to_the_viewer() {
        use crate::proto::{FORMAT_POINTER, FORMATS, Settings, ToAgent};
        let Ok(host) = std::env::var("SSHREMOTE_GNOME_HOST") else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let target = Target::Ssh {
            host,
            port: None,
            args: Vec::new(),
            remote: format!("env {env} {agent} --agent"),
            capture: "gnome".into(),
            password: None,
            unattended: true,
        };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let (w, h) = sess.desk;
        let settings = |formats| Settings { view_w: w, view_h: h, scale: 100, mode: crate::color::Mode::True, fps: 30, formats };
        // What comes within `ms`: the parts frames changed (acknowledged), the pointer's pictures
        // and places.
        let gather = |sess: &mut Session, ms: u64| {
            let (mut frames, mut shapes, mut places) = (Vec::new(), Vec::new(), Vec::new());
            let until = Instant::now() + Duration::from_millis(ms);
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        let parts = f.tiles.iter().chain(&f.video).map(|t| (t.x, t.y, t.x + t.w, t.y + t.h));
                        frames.push(parts.reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))));
                    }
                    Ok(Ev::Net(ToClient::PointerShape { w, h, .. })) => shapes.push((w, h)),
                    Ok(Ev::Net(ToClient::PointerAt(at))) => places.push(at),
                    Ok(Ev::Closed(e)) => panic!("the agent ended: {e}"),
                    _ => {}
                }
            }
            (frames, shapes, places)
        };
        // Somewhere first, so the pointer is on the screen and in sight.
        sess.send(ToAgent::Pointer { x: w / 3, y: h / 3 }).unwrap();
        sess.send(ToAgent::Settings(settings(FORMATS | FORMAT_POINTER))).unwrap();
        let (frames, shapes, places) = gather(&mut sess, 3000);
        assert!(!frames.is_empty(), "the screen comes");
        assert!(shapes.iter().any(|&(w, h)| w > 0 && h > 0), "the pointer's picture comes: {shapes:?}");
        assert!(places.iter().any(|p| p.is_some()), "and where it is: {places:?}");
        let wander = |sess: &mut Session| {
            for i in 0..20u16 {
                sess.send(ToAgent::Pointer { x: w / 3 + i * 9, y: h / 3 + i * 5 }).unwrap();
                std::thread::sleep(Duration::from_millis(30));
            }
            gather(sess, 800)
        };
        // Frames that changed where the pointer went (a text caret may blink elsewhere).
        let path = (w / 3 - 32, h / 3 - 32, w / 3 + 19 * 9 + 32, h / 3 + 19 * 5 + 32);
        let on_path = |frames: &[Option<(u16, u16, u16, u16)>]| {
            frames.iter().flatten().filter(|f| f.0 < path.2 && f.2 > path.0 && f.1 < path.3 && f.3 > path.1).count()
        };
        // (Its picture may come again where it changes, over a text field say.)
        let (apart, ..) = wander(&mut sess);
        assert_eq!(on_path(&apart), 0, "moving the pointer alone sends no pictures of it: {apart:?}");
        // A viewer that does not draw it gets it in the pictures, which follow its moves.
        sess.send(ToAgent::Settings(settings(FORMATS))).unwrap();
        gather(&mut sess, 1000);
        let (drawn, ..) = wander(&mut sess);
        assert!(on_path(&drawn) >= 5, "the pictures show the pointer moving: {drawn:?}");
        sess.close();
    }

    /// Text typed into the middle of a line (or taken out of it) comes as its rest moved along
    /// plus the new characters, and the viewer's picture is the host's: a whole new picture asked
    /// for at the end matches it. A GTK 3 text field (whole-pixel glyph places) is opened there
    /// for 40 s; needs SSHREMOTE_GNOME_HOST with python3-gi (and SSHREMOTE_GNOME_ENV,
    /// SSHREMOTE_GNOME_AGENT as above).
    #[test]
    #[ignore]
    fn a_gnome_line_typed_into_moves_along_and_stays_exact() {
        use crate::proto::{FORMATS, Settings, ToAgent};
        let Ok(host) = std::env::var("SSHREMOTE_GNOME_HOST") else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let field = "import gi; gi.require_version(\"Gtk\", \"3.0\"); from gi.repository import Gtk; \
            w = Gtk.Window(title=\"sideways\"); w.set_default_size(800, 300); v = Gtk.TextView(); b = v.get_buffer(); \
            b.set_text(\"the quick brown fox jumps over the lazy dog and on\"); b.place_cursor(b.get_start_iter()); \
            w.add(v); w.connect(\"destroy\", Gtk.main_quit); w.show_all(); w.present(); v.grab_focus(); Gtk.main()";
        let opened = std::process::Command::new("ssh")
            .arg(&host)
            .arg(format!("env {env} setsid timeout 40 python3 -c '{field}' > /dev/null 2>&1 < /dev/null &"))
            .status();
        assert!(opened.is_ok_and(|s| s.success()), "the text field opens");
        std::thread::sleep(Duration::from_secs(3));
        let target = Target::Ssh { host: host.clone(), port: None, args: Vec::new(), remote: format!("env {env} {agent} --agent"), capture: "gnome".into(), password: None, unattended: true };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let (w, h) = sess.desk;
        sess.send(ToAgent::Settings(Settings { view_w: w, view_h: h, scale: 100, mode: crate::color::Mode::True, fps: 30, formats: FORMATS })).unwrap();
        let mut fb = Framebuffer::default();
        // Frames for `ms`, applied; how many moved something sideways.
        let take = |sess: &mut Session, fb: &mut Framebuffer, ms: u64, last: bool| -> (usize, Option<Vec<u8>>) {
            let (mut sideways, mut before_whole) = (0, None);
            let until = Instant::now() + Duration::from_millis(ms);
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        sideways += f.moves.iter().filter(|m| m.dx != 0).count();
                        if last && f.covered() == fb.w * fb.h && before_whole.is_none() {
                            before_whole = Some(fb.rgb.clone());
                        }
                        fb.apply(&f).unwrap();
                    }
                    Ok(Ev::Closed(e)) => panic!("the agent ended: {e}"),
                    _ => {}
                }
            }
            (sideways, before_whole)
        };
        take(&mut sess, &mut fb, 2000, false);
        // A key, and the frames it brings.
        let mut sideways = 0;
        let mut key = |sess: &mut Session, fb: &mut Framebuffer, keysym: u32| {
            sess.send(ToAgent::Key { keysym, mods: 0 }).unwrap();
            sideways += take(sess, fb, 150, false).0;
        };
        for c in "and more ".chars() {
            key(&mut sess, &mut fb, crate::xk::of_char(c));
        }
        // And taken out again: the rest moves back.
        for _ in 0..3 {
            key(&mut sess, &mut fb, crate::xk::BACKSPACE);
        }
        drop(key);
        take(&mut sess, &mut fb, 1000, false);
        assert!(sideways >= 8, "the line's rest moved along: {sideways} sideways moves");
        // A whole new picture: the one the viewer has, but for a blinking caret.
        sess.send(ToAgent::Refresh).unwrap();
        let (_, before) = take(&mut sess, &mut fb, 3000, true);
        let before = before.expect("a whole new picture");
        let differ = before.as_chunks::<3>().0.iter().zip(fb.rgb.as_chunks::<3>().0).filter(|(a, b)| a != b).count();
        sess.close();
        // The text field closes (its timeout passes the signal on).
        let _ = std::process::Command::new("ssh").arg(&host).arg("pkill -f '^[t]imeout 40 python3 -c import gi'").status();
        assert!(differ < 200, "the viewer's picture was the host's: {differ} pixels differ");
    }

    /// A film that stops, and a photograph shown once, end on the viewer exactly as on the host:
    /// sent as a film from their first frame, then losslessly once still. In a GNOME session on
    /// SSHREMOTE_GNOME_HOST, programs started there through SSHREMOTE_GNOME_RUN (a command prefix
    /// that puts them in the session) and the agent SSHREMOTE_GNOME_AGENT.
    #[test]
    #[ignore]
    fn a_gnome_film_ends_exact() {
        use crate::proto::{FORMATS, Settings, ToAgent};
        let (Ok(host), Ok(run), Ok(agent)) = (std::env::var("SSHREMOTE_GNOME_HOST"), std::env::var("SSHREMOTE_GNOME_RUN"), std::env::var("SSHREMOTE_GNOME_AGENT")) else { return };
        // A photograph panned for three seconds after a wait, then still; then another, still.
        let film = [
            "import gi, glob, math",
            "gi.require_version(\"Gtk\", \"3.0\")",
            "from gi.repository import Gtk, GLib, GdkPixbuf",
            "p = GdkPixbuf.Pixbuf.new_from_file_at_scale(sorted(glob.glob(\"/usr/share/backgrounds/*.jpg\"))[0], 1600, 1000, True)",
            "w = Gtk.Window(title=\"film\")",
            "w.set_default_size(900, 560)",
            "a = Gtk.Image()",
            "w.add(a)",
            "f = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, False, 8, 840, 520)",
            "n = [0]",
            "def show(k):",
            "    s = 0.8 + 0.1 * math.sin(k / 20.0)",
            "    p.scale(f, 0, 0, 840, 520, -40 - 3 * k, -30 - 2 * k, s, s, GdkPixbuf.InterpType.BILINEAR)",
            "    a.set_from_pixbuf(f)",
            "def tick():",
            "    n[0] += 1",
            "    show(n[0])",
            "    return n[0] < 90",
            "show(0)",
            "GLib.timeout_add(3000, lambda: GLib.timeout_add(33, tick) and False)",
            "GLib.timeout_add(13000, lambda: show(20) or False)",
            "w.connect(\"destroy\", Gtk.main_quit)",
            "w.show_all()",
            "w.present()",
            "Gtk.main()",
        ]
        .join("\n");
        let opened = std::process::Command::new("ssh").arg(&host).arg(format!("{run} setsid timeout 40 python3 -c '{film}' > /dev/null 2>&1 < /dev/null &")).status();
        assert!(opened.is_ok_and(|s| s.success()), "the film opens");
        std::thread::sleep(Duration::from_secs(1));
        let target = Target::Ssh { host: host.clone(), port: None, args: Vec::new(), remote: format!("{run} {agent} --agent"), capture: "gnome".into(), password: None, unattended: true };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let (w, h) = sess.desk;
        sess.send(ToAgent::Settings(Settings { view_w: w, view_h: h, scale: 100, mode: crate::color::Mode::True, fps: 30, formats: FORMATS })).unwrap();
        let mut fb = Framebuffer::default();
        // Frames for `ms`, applied: how many carried film, and the picture before the first whole one.
        let take = |sess: &mut Session, fb: &mut Framebuffer, ms: u64, last: bool| -> (usize, Option<Vec<u8>>) {
            let (mut films, mut before_whole) = (0, None);
            let until = Instant::now() + Duration::from_millis(ms);
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        films += f.video.is_some() as usize;
                        if last && f.covered() == fb.w * fb.h && before_whole.is_none() {
                            before_whole = Some(fb.rgb.clone());
                        }
                        fb.apply(&f).unwrap();
                    }
                    Ok(Ev::Closed(e)) => panic!("the agent ended: {e}"),
                    _ => {}
                }
            }
            (films, before_whole)
        };
        let exact = |sess: &mut Session, fb: &mut Framebuffer| {
            sess.send(ToAgent::Refresh).unwrap();
            let (_, before) = take(sess, fb, 2500, true);
            let before = before.unwrap_or_else(|| panic!("a whole new picture ({}x{} viewer)\n{}", fb.w, fb.h, sess.log()));
            before.as_chunks::<3>().0.iter().zip(fb.rgb.as_chunks::<3>().0).filter(|(a, b)| a != b).count()
        };
        // The film plays from 2 s after the window opened for 3 s, then stays still.
        let (films, _) = take(&mut sess, &mut fb, 6500, false);
        let after_film = exact(&mut sess, &mut fb);
        // Then another photograph, once, and still.
        let (shown, _) = take(&mut sess, &mut fb, 5000, false);
        let after_photo = exact(&mut sess, &mut fb);
        // The window closes by itself (its timeout).
        sess.close();
        println!("film frames {films}, photograph frames {shown}; pixels differing after the film {after_film}, after the photograph {after_photo}");
        assert!(films > 10, "the film went as one: {films} frames");
        assert!(shown >= 1, "the photograph went first as a film's frame");
        assert!(after_film < 200 && after_photo < 200, "the viewer's picture was the host's: {after_film} and {after_photo} pixels differ");
    }

    /// A window shared alone shows the menus it opens: F10 in it opens its main menu, and the
    /// shared picture shows it. Needs SSHREMOTE_GNOME_HOST with the helper running and a
    /// window with a main menu (a text editor) focused there (and SSHREMOTE_GNOME_ENV,
    /// SSHREMOTE_GNOME_AGENT as above).
    #[test]
    #[ignore]
    fn a_gnome_window_shared_alone_shows_its_menus() {
        use crate::proto::{Settings, ToAgent};
        use crate::shared::Share;
        let Ok(host) = std::env::var("SSHREMOTE_GNOME_HOST") else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let target = Target::Ssh { host, port: None, args: Vec::new(), remote: format!("env {env} {agent} --agent"), capture: "gnome".into(), password: None, unattended: true };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let screen = (sess.desk.0 as u16, sess.desk.1 as u16);
        sess.send(ToAgent::Settings(Settings { view_w: 4000, view_h: 4000, scale: 100, mode: crate::color::Mode::True, fps: 10, formats: crate::proto::FORMATS })).unwrap();
        let mut fb = Framebuffer::default();
        // Frames for `secs` seconds (or until one for which `want` holds), applied to `fb`.
        let frames = |sess: &mut Session, fb: &mut Framebuffer, secs: u64, want: &dyn Fn(u16, u16) -> bool| -> bool {
            let until = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        fb.apply(&f).unwrap();
                        if want(f.w, f.h) {
                            return true;
                        }
                    }
                    Ok(Ev::Closed(e)) => panic!("the agent ended: {e}"),
                    _ => {}
                }
            }
            false
        };
        assert!(frames(&mut sess, &mut fb, 20, &|w, h| (w, h) == screen), "the screen");
        sess.send(ToAgent::Share(Share::ActiveWindow)).unwrap();
        assert!(frames(&mut sess, &mut fb, 20, &|w, h| (w, h) != screen), "the window alone");
        frames(&mut sess, &mut fb, 2, &|_, _| false);
        let before = (fb.w, fb.h, fb.rgb.clone());
        // Its main menu, as F10 opens it (GNOME 50's text editor opens no context menu on an
        // empty page, by Shift+F10 or a right click).
        sess.send(ToAgent::Key { keysym: crate::xk::F1 + 9, mods: 0 }).unwrap();
        frames(&mut sess, &mut fb, 3, &|_, _| false);
        a_picture(&fb, "window-menu");
        let grew = (fb.w, fb.h) != (before.0, before.1);
        let changed = if grew { usize::MAX } else { fb.rgb.chunks(3).zip(before.2.chunks(3)).filter(|(a, b)| a != b).count() };
        // A menu is thousands of pixels; a blinking caret a few hundred.
        assert!(changed > 5000, "the menu shows in the shared picture: {changed} pixels changed");
        sess.send(ToAgent::Key { keysym: crate::xk::ESCAPE, mods: 0 }).unwrap();
        sess.close();
    }

    /// Sharing the focused program on a GNOME host with the sshremote helper sends the part of the
    /// screen its windows take, and the whole screen comes back as it was. Needs
    /// SSHREMOTE_GNOME_HOST with the helper running and a program's window focused there (and
    /// SSHREMOTE_GNOME_ENV, SSHREMOTE_GNOME_AGENT as above).
    #[test]
    #[ignore]
    fn a_gnome_program_shared_alone_is_cut_out_of_the_screen() {
        use crate::proto::{Settings, ToAgent};
        use crate::shared::Share;
        let Ok(host) = std::env::var("SSHREMOTE_GNOME_HOST") else { return };
        let env = std::env::var("SSHREMOTE_GNOME_ENV").unwrap_or_default();
        let agent = std::env::var("SSHREMOTE_GNOME_AGENT").unwrap_or_else(|_| "sshremote".into());
        let target = Target::Ssh {
            host,
            port: None,
            args: Vec::new(),
            remote: format!("env {env} {agent} --agent"),
            capture: "gnome".into(),
            password: None,
            unattended: true,
        };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let screen = (sess.desk.0 as u16, sess.desk.1 as u16);
        sess.send(ToAgent::Settings(Settings { view_w: 4000, view_h: 4000, scale: 100, mode: crate::color::Mode::Gray16, fps: 10, formats: crate::proto::FORMATS })).unwrap();
        let mut fb = Framebuffer::default();
        let mut next = |first: Option<ToAgent>, want: &dyn Fn(u16, u16) -> bool| -> (Option<(u8, String)>, (u16, u16), Vec<u8>) {
            if let Some(m) = first {
                sess.send(m).unwrap();
            }
            let mut shared = None;
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ev::Net(ToClient::Sharing { kind, text })) => shared = Some((kind, text)),
                    Ok(Ev::Net(ToClient::Notice(n))) if n.starts_with("Sharing") => panic!("{n}"),
                    Ok(Ev::Net(ToClient::Frame(f))) => {
                        sess.send(ToAgent::Ack(f.id)).unwrap();
                        fb.apply(&f).unwrap();
                        if want(f.w, f.h) {
                            return (shared, (f.w, f.h), fb.rgb.clone());
                        }
                    }
                    Ok(Ev::Closed(e)) => panic!("the agent ended: {e}"),
                    _ => {}
                }
            }
            panic!("no such frame in time");
        };
        next(None, &|w, h| (w, h) == screen);
        let (shared, part, shot) = next(Some(ToAgent::Share(Share::ActiveApp)), &|w, h| (w, h) != screen);
        let (kind, text) = shared.expect("the share is confirmed");
        assert!(text.starts_with("every window of"), "{kind} {text}");
        assert!(part.0 < screen.0 && part.1 < screen.1, "the program's part: {part:?}");
        let (_, back, _) = next(Some(ToAgent::Share(Share::Screen)), &|w, h| (w, h) == screen);
        assert_eq!(back, screen);
        drop(next);
        let fb = Framebuffer { rgb: shot, w: part.0 as usize, h: part.1 as usize, ..Framebuffer::default() };
        a_picture(&fb, "program");
        sess.close();
    }

    /// ssh's warnings that are no error do not hide how a connection ended: a newer ssh warns
    /// of an older Windows server's key exchange, and the Windows commands must still be tried.
    #[test]
    fn ssh_warnings_are_not_taken_for_errors() {
        let log = "Warning: Permanently added '[192.168.40.1]:2222' (ED25519) to the list of known hosts.\n** WARNING: connection is not using a post-quantum key exchange algorithm.\n** This session may be vulnerable to \"store now, decrypt later\" attacks.\n** The server may need to be upgraded. See https://openssh.com/pq.html\n**";
        assert!(log.lines().all(ssh_chatter));
        assert!(!ssh_chatter("ssh: connect to host 10.0.0.1 port 22: Connection refused"));
        assert!(!ssh_chatter("'sshremote' is not recognized as an internal or external command,"));
    }

    /// Any SSH host with sshremote on it answers to the usual agent command, as it always did.
    /// Needs SSHREMOTE_SSH_HOST (an ssh alias).
    #[test]
    #[ignore]
    fn an_ssh_host_answers_the_usual_agent_command() {
        let Ok(host) = std::env::var("SSHREMOTE_SSH_HOST") else { return };
        let target = Target::Ssh {
            host: host.clone(),
            port: None,
            args: Vec::new(),
            remote: crate::options::REMOTE.into(),
            capture: "auto".into(),
            password: None,
            unattended: true,
        };
        let (mut sess, _tx, _rx) = Session::open(&target, true).expect("the agent answers");
        assert!(sess.desk.0 > 0);
        sess.close();
        assert!(WORKED.lock().unwrap().as_ref().is_none_or(|m| !m.contains_key(&host)), "no other command was needed");
    }

    /// A Windows host reached over ssh with the usual agent command (which its shell does not
    /// run) is tried with Windows' own, and the one that worked is used straight away next time.
    #[test]
    #[ignore]
    fn a_windows_ssh_host_needs_no_agent_command_of_its_own() {
        let Ok(host) = std::env::var("SSHREMOTE_WINDOWS_HOST") else { return };
        let target = Target::Ssh {
            host: host.clone(),
            port: None,
            args: Vec::new(),
            remote: crate::options::REMOTE.into(),
            capture: "auto".into(),
            password: None,
            unattended: true,
        };
        let (mut sess, _tx, _rx) = Session::open(&target, true).expect("reached with Windows' command");
        assert!(sess.desk.0 > 0, "a screen: {:?}", sess.desk);
        sess.close();
        let kept = WORKED.lock().unwrap().as_ref().and_then(|m| m.get(&host).cloned());
        assert!(kept.is_some(), "the command that worked is kept");
        let started = Instant::now();
        let (mut again, _, _) = Session::open(&target, true).expect("again");
        again.close();
        assert!(started.elapsed() < Duration::from_secs(8), "straight away the second time: {:?}", started.elapsed());
    }

    /// On a Windows host whose agent the service runs (an ssh session has no desktop, so the
    /// service's SYSTEM helper serves it): the terminal is the signed-in person's and so are the
    /// files, never SYSTEM's. Needs SSHREMOTE_WINDOWS_HOST (an ssh alias) and the service there.
    #[test]
    #[ignore]
    fn a_windows_agent_run_as_system_acts_as_the_signed_in_person() {
        let Ok(host) = std::env::var("SSHREMOTE_WINDOWS_HOST") else { return };
        let target = Target::Ssh {
            host,
            port: None,
            args: Vec::new(),
            remote: r#""C:\Program Files\SSH Remote\sshremote.exe" --agent"#.into(),
            capture: "auto".into(),
            password: None,
            unattended: true,
        };
        let (mut sess, _tx, rx) = Session::open(&target, true).expect("the agent answers");
        let wait = |want: &dyn Fn(&ToClient) -> bool| -> ToClient {
            let until = Instant::now() + Duration::from_secs(30);
            while Instant::now() < until {
                if let Ok(Ev::Net(m)) = rx.recv_timeout(Duration::from_millis(200))
                    && want(&m)
                {
                    return m;
                }
            }
            panic!("no answer in time");
        };
        sess.send(ToAgent::ListDir { path: String::new() }).unwrap();
        let ToClient::DirList { path, error, .. } = wait(&|m| matches!(m, ToClient::DirList { .. })) else { unreachable!() };
        assert!(error.is_empty(), "{error}");
        assert!(path.to_lowercase().contains(r"\users\") && !path.to_lowercase().contains("systemprofile"), "home: {path}");
        sess.send(ToAgent::TermOpen { id: 1, cols: 120, rows: 30 }).unwrap();
        sess.send(ToAgent::TermInput { id: 1, data: b"whoami\r\n".to_vec() }).unwrap();
        // The terminal's control sequences out: CSI (ESC [ ... letter) and OSC (ESC ] ... BEL).
        let plain = |out: &str| -> String {
            let mut s = String::new();
            let mut it = out.chars().peekable();
            while let Some(c) = it.next() {
                match (c, it.peek()) {
                    ('\x1b', Some('[')) => {
                        it.next();
                        while it.next().is_some_and(|c| !('@'..='~').contains(&c)) {}
                    }
                    ('\x1b', Some(']')) => while it.next().is_some_and(|c| c != '\x07') {},
                    _ => s.push(c),
                }
            }
            s
        };
        // "whoami" answers with DOMAIN\user alone on a line; the prompt has a backslash too.
        let answer = |out: &str| -> Option<String> {
            plain(out).split(['\r', '\n'])
                .map(|l| l.trim().trim_matches(|c: char| c.is_control()))
                .find(|l| l.matches('\\').count() == 1 && !l.contains([' ', ':', '>', '[', ']']))
                .map(str::to_lowercase)
        };
        let mut out = String::new();
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until && answer(&out).is_none() {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ev::Net(ToClient::TermOutput { data, .. })) => out.push_str(&String::from_utf8_lossy(&data)),
                Ok(Ev::Net(ToClient::TermExit { text, .. })) => panic!("no shell: {text}"),
                _ => {}
            }
        }
        let who = answer(&out).unwrap_or_default();
        assert!(!who.is_empty() && !who.contains("system"), "the shell is {who:?}: {out}");
        sess.close();
    }

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
