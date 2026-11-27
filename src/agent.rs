//! Remote side: captures the desktop, reduces it to the requested colour mode,
//! sends only changed rectangles, and injects the viewer's input.

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::xk;
use crate::capture::{self, Image, Input};
use crate::color::Mode;
use crate::color::{Plane, Reducer, diff, pack};
use crate::proto::{Frame, Settings, Tile, ToAgent, ToClient, read_msg, write_msg};
use crate::{scroll, video};

/// Frames sent but not yet acknowledged; more would only queue stale screens in SSH. Small
/// frames (the pointer, a few characters) may run further ahead, so a long round trip does not
/// cap the frame rate of light updates.
const IN_FLIGHT: u32 = 2;
const IN_FLIGHT_SMALL: u32 = 6;
/// Bytes of frames in flight under which the small-frame allowance applies.
const SMALL_BYTES: usize = 256 << 10;

/// The stream to the viewer, zstd frames one after another: messages up to `BIG` bytes in long
/// frames at a strong level, where pictures of typing or scrolling compress against those before
/// them; a larger one (a whole new screen) in a frame of its own at a fast level, so it is not
/// held up compressing. Viewers read the frames as one stream.
struct Wire {
    enc: Option<zstd::stream::write::Encoder<'static, BufWriter<Box<dyn Write>>>>,
    raw: Option<BufWriter<Box<dyn Write>>>,
}

impl Wire {
    const BIG: usize = 1 << 20;

    fn new(output: Box<dyn Write>) -> Wire {
        Wire { enc: None, raw: Some(BufWriter::with_capacity(1 << 16, output)) }
    }

    fn message(&mut self, kind: u8, payload: &[u8]) -> io::Result<()> {
        if payload.len() + 5 > Self::BIG {
            let mut out = match (self.enc.take(), self.raw.take()) {
                (Some(enc), _) => enc.finish()?,
                (None, Some(raw)) => raw,
                (None, None) => return Err(io::Error::other("the stream to the viewer broke")),
            };
            let mut whole = Vec::with_capacity(payload.len() + 5);
            write_msg(&mut whole, kind, payload)?;
            out.write_all(&zstd::bulk::compress(&whole, 3)?)?;
            self.raw = Some(out);
            return Ok(());
        }
        if self.enc.is_none() {
            let raw = self.raw.take().ok_or_else(|| io::Error::other("the stream to the viewer broke"))?;
            let mut enc = zstd::stream::write::Encoder::new(raw, 7)?;
            // A long window lets earlier screens act as a dictionary for repeats.
            enc.window_log(24)?;
            enc.long_distance_matching(true)?;
            self.enc = Some(enc);
        }
        write_msg(self.enc.as_mut().expect("made above"), kind, payload)
    }

    fn flush(&mut self) -> io::Result<()> {
        match (&mut self.enc, &mut self.raw) {
            (Some(enc), _) => enc.flush(),
            (None, Some(raw)) => raw.flush(),
            (None, None) => Ok(()),
        }
    }
}

/// Queues a message; the loop flushes once per turn, before it waits, so several messages share
/// one compressed block.
fn send(w: &mut Wire, m: &ToClient) -> io::Result<()> {
    send_sized(w, m).map(|_| ())
}

/// [`send`], returning the message's size before compression.
fn send_sized(w: &mut Wire, m: &ToClient) -> io::Result<usize> {
    let (k, p) = m.encode();
    w.message(k, &p)?;
    Ok(p.len())
}

/// The picture's size for the viewer's area: the host's shape kept, never larger than the host,
/// then the preset's scale. The limiting side is exact and the other rounded, so a 1920x1080
/// desktop in a 1280x720 view is 1280x720, not a pixel short to be resampled.
fn target(desk: (usize, usize), s: &Settings, resizable: bool) -> (usize, usize) {
    let (dw, dh) = (desk.0.max(1) as f64, desk.1.max(1) as f64);
    let (vw, vh) = (s.view_w.max(16) as f64, s.view_h.max(16) as f64);
    let k = s.scale.clamp(1, 100) as f64 / 100.0;
    // A screen that takes any size (a web page) is laid out at the size it is shown at, within
    // what it takes (`Browser::set_target`), so its pictures come at the size asked for.
    if resizable {
        return (((vw * k).round() as usize).clamp(200, 3840), ((vh * k).round() as usize).clamp(150, 2160));
    }
    let (w, h) = if vw * dh <= vh * dw {
        let w = vw.min(dw);
        (w, (dh * w / dw).round())
    } else {
        let h = vh.min(dh);
        ((dw * h / dh).round(), h)
    };
    (((w * k).round() as usize).max(16), ((h * k).round() as usize).max(16))
}

/// The person signed in at the screen, acted as on this thread for a file operation when this
/// agent runs as SYSTEM; None when it runs as a person already; why not when nobody is signed in.
#[cfg(windows)]
fn files_as_user() -> Result<Option<crate::console_user::AsUser>, &'static str> {
    use crate::console_user::{Acting, NOBODY, acting};
    match acting() {
        Acting::Myself => Ok(None),
        Acting::Nobody => Err(NOBODY),
        Acting::User(u) => {
            crate::files::set_home(Some(u.profile.clone()));
            u.impersonate().map(Some).ok_or(n!("could not act as the person signed in at that computer"))
        }
    }
}

/// The folder an upload into `dir` goes to (empty: Downloads; a clipboard or drop batch) and its
/// name there.
fn upload_place(dir: &str, name: String) -> (std::path::PathBuf, String) {
    if dir.is_empty() {
        (crate::files::downloads(), name)
    } else if let Some(batch) = dir.strip_prefix(crate::files::CLIPBOARD) {
        (crate::files::clipboard_dir(batch), name)
    } else if let Some(batch) = dir.strip_prefix(crate::files::DROP) {
        // Named so that its path, typed at a prompt, is read as it is.
        let name: Vec<String> = name.split(['/', '\\']).map(crate::files::typable).collect();
        (crate::files::drop_dir(batch), name.join("/"))
    } else {
        (std::path::PathBuf::from(dir), name)
    }
}

/// A session's recording, written out whole however the session ends.
struct SessionRecording(Option<crate::record::Recording>);

impl Drop for SessionRecording {
    fn drop(&mut self) {
        if let Some(r) = self.0.take() {
            let _ = r.stop();
        }
    }
}

/// The token this agent may hand a viewer that asks to be trusted (`ToAgent::TrustToken`).
static TRUST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// What a file request refused for `why` is answered with.
fn refused(m: &ToAgent, why: &str) -> Vec<ToClient> {
    let why = why.to_string();
    match m {
        ToAgent::ListDir { path } => vec![ToClient::DirList { path: path.clone(), entries: Vec::new(), error: why }],
        ToAgent::Fetch { tag, .. } | ToAgent::FetchFrom { tag, .. } => {
            let mut out = vec![ToClient::FetchEnd { id: 0, ok: false, text: why }];
            if tag & crate::files::DONE_TAG != 0 {
                out.push(ToClient::FetchDone { tag: *tag });
            }
            out
        }
        ToAgent::FileStart { id, .. } | ToAgent::FileResume { id, .. } | ToAgent::FileEnd { id } => vec![ToClient::FileDone { id: *id, ok: false, text: why }],
        ToAgent::FileOp { .. } => vec![ToClient::OpDone { ok: false, text: why }],
        _ => Vec::new(),
    }
}

/// A Mac's login window: the agent a viewer by ID gets there runs as root with nobody signed in,
/// so no shells, files or programs are handed out (they would be root's).
#[cfg(target_os = "macos")]
fn mac_signed_out() -> bool {
    // SAFETY: getuid cannot fail.
    by_id() && unsafe { libc::getuid() } == 0
}

#[cfg(target_os = "macos")]
const MAC_NOBODY: &str = n!("Nobody is signed in at that Mac: its terminals, files, programs and chat open once someone signs in.");

/// The viewer came by this computer's remote access ID (rather than over ssh as its owner).
static BY_ID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// What such a viewer may do, and whether the person at the screen is asked first (`grant`).
static GRANT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(crate::grant::ALL);

pub fn mark_by_id(grant: u8, cameras: bool) {
    BY_ID.store(true, std::sync::atomic::Ordering::Relaxed);
    GRANT.store(grant, std::sync::atomic::Ordering::Relaxed);
    CAMERAS.store(cameras, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the viewer may look at this computer's cameras: one by ID where the settings let
/// viewers; one over SSH (the computer's own user) always.
static CAMERAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn cameras_allowed() -> bool {
    !by_id() || CAMERAS.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn by_id() -> bool {
    BY_ID.load(std::sync::atomic::Ordering::Relaxed)
}

/// The rights and whether to ask, as given to this agent.
pub fn grant() -> u8 {
    GRANT.load(std::sync::atomic::Ordering::Relaxed)
}

/// The viewer asked without a password: the code it was shown, for the person at the screen to
/// compare (six digits).
static CODE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_code(code: &str) {
    if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
        *CODE.lock().unwrap_or_else(|e| e.into_inner()) = Some(code.to_string());
    }
}

pub fn code() -> Option<String> {
    CODE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The code as given on to the helper or the window: ` --code 482913`, or nothing.
pub fn code_arg() -> String {
    code().map(|c| format!(" --code {c}")).unwrap_or_default()
}

/// The viewer by ID may see the cameras, as given on to the helper: ` --cameras`, or nothing.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn cameras_arg() -> &'static str {
    if by_id() && CAMERAS.load(std::sync::atomic::Ordering::Relaxed) { " --cameras" } else { "" }
}

/// What the agent's loop hears: a message from the viewer, something to send, the viewer gone.
enum Heard {
    Msg(ToAgent),
    /// Something came to send: a picture, a shell's output, a connection's data, sound.
    Wake,
    Gone,
}

/// Serves on stdin/stdout, or on the named pipes a Windows relay created.
pub fn run(backend: &str, pipe: Option<&str>) -> Result<(), String> {
    let backend = starting(backend);
    #[cfg(windows)]
    if pipe.is_none()
        && matches!(backend, "auto" | "windows")
        // SSHREMOTE_RELAY forces the relay path so it can be tested from a console session.
        && (crate::capture::windows::needs_relay() || std::env::var_os("SSHREMOTE_RELAY").is_some())
    {
        return crate::relay::run(backend);
    }
    let (input, output): (Box<dyn Read + Send>, Box<dyn Write>) = match pipe {
        #[cfg(windows)]
        Some(base) => {
            let (down, up) = crate::relay::pipe_names(base);
            let open = |p: &str, write: bool| {
                std::fs::OpenOptions::new().read(!write).write(write).open(p).map_err(|e| format!("{p}: {e}"))
            };
            (Box::new(open(&down, false)?), Box::new(open(&up, true)?))
        }
        #[cfg(not(windows))]
        Some(_) => return Err("--pipe is only used on Windows".into()),
        None => (Box::new(io::stdin()), Box::new(io::stdout())),
    };
    run_on(backend, Box::new(EndsWithViewer { inner: input, ended: false }), output)
}

/// How long an agent whose viewer has gone may take to let go of the screen before it ends anyway.
const LAST_WORDS: Duration = Duration::from_secs(5);

/// The viewer's stream for an agent process of its own: once it ends, the process ends within
/// `LAST_WORDS`, even if letting go of the screen hangs; else it would hold the screen cast (and
/// keep the next viewer from the screen) for as long as the machine runs.
struct EndsWithViewer {
    inner: Box<dyn Read + Send>,
    ended: bool,
}

impl Read for EndsWithViewer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let got = self.inner.read(buf);
        let gone = match &got {
            Ok(n) => *n == 0,
            Err(e) => e.kind() != io::ErrorKind::Interrupted,
        };
        if !self.ended && gone && !buf.is_empty() {
            self.ended = true;
            thread::spawn(|| {
                thread::sleep(LAST_WORDS);
                std::process::exit(0);
            });
        }
        got
    }
}

/// Serves one viewer over any pair of streams (stdio, relay pipes, a sealed network link).
pub fn run_on(backend: &str, input: Box<dyn Read + Send>, output: Box<dyn Write>) -> Result<(), String> {
    let backend = starting(backend);
    let mut enc = Wire::new(output);
    let result = serve(&mut enc, input, backend);
    // The viewer reads why in its own words: queued messages go out only when flushed.
    if let Err(e) = &result {
        let _ = send(&mut enc, &ToClient::Error(e.clone())).and_then(|()| enc.flush());
    }
    result
}

/// How long the person at the screen has to accept a viewer who came by this computer's ID.
const ASK_FOR: Duration = Duration::from_secs(60);

fn serve(enc: &mut Wire, input: Box<dyn Read + Send>, backend: &str) -> Result<(), String> {
    let mut chat = crate::chat::Window::default();
    let mut allowed = crate::grant::ALL;
    if by_id() {
        allowed = grant() & crate::grant::ALL;
        // A Mac's login window has nobody to ask or tell (and a window of this program would
        // show over it).
        #[cfg(target_os = "macos")]
        if mac_signed_out() && grant() & crate::grant::ASK != 0 {
            return Err(n!("Nobody is signed in at that Mac to let you in.").into());
        }
        #[cfg(target_os = "macos")]
        let tell = !mac_signed_out();
        // Root's at a Linux login screen has nobody signed in to tell.
        #[cfg(not(target_os = "macos"))]
        let tell = backend != "kms";
        if grant() & crate::grant::ASK != 0 {
            // Asked before the screen is opened: nothing of it is seen until then.
            send(enc, &ToClient::Notice(crate::chat::WAITING.into())).and_then(|()| enc.flush()).map_err(|e| e.to_string())?;
            allowed = match chat.ask(allowed, ASK_FOR) {
                Ok(Some(a)) => a,
                Ok(None) => return Err(n!("The person at that computer did not let you in.").into()),
                Err(e) => return Err(crate::lang::fill(n!("Nobody could let you in at that computer: {}"), &[&e])),
            };
        } else {
            // Someone who came by this computer's ID is made known to the person at the screen,
            // who can end it (nobody signed in: nobody to tell).
            if tell {
                let _ = chat.notice(allowed);
            }
        }
    }
    // A screen that cannot be shared leaves the rest of the session: terminals, files, clipboard.
    let mut cap = match capture::open(backend) {
        Ok(c) => c,
        // Locked, where root's remote access shows the lock screen (Linux): this connection
        // ends and the viewer, connecting again, comes to the lock screen there.
        #[cfg(target_os = "linux")]
        Err(_) if by_id() && crate::loginscreen::installed() && crate::console::now().is_some_and(|c| !matches!(c, crate::console::Console::Open(_))) => {
            return Err(n!("The screen is locked: on to the lock screen").into());
        }
        Err(e) => {
            eprintln!("sshremote: no screen to share: {e}");
            Box::new(capture::NoScreen(e))
        }
    };
    // The cameras after the monitors, where the viewer may look at them.
    let cams = if cameras_allowed() { capture::camera::list() } else { Vec::new() };
    if !cams.is_empty() {
        cap = Box::new(capture::camera::WithCameras::new(cap, cams));
    }
    let mut held = Held::default();
    // The resolutions this session sets, set back as they were when it ends.
    let mut screens = crate::display::Session::new(cap.name());
    let result = serve_with(enc, input, &mut cap, &mut held, &mut screens, chat, allowed);
    // What the viewer still held down at the host is let go, however the session ended.
    held.release(cap.as_mut());
    // The picture stops before the screens are set back (a stream may not take the change).
    drop(cap);
    drop(screens);
    result
}

/// Keys and buttons the viewer holds down at the host. Windows keeps injected keys down after
/// the program that pressed them is gone, so a dropped link would leave Ctrl or a drag held.
#[derive(Default)]
struct Held {
    keys: std::collections::BTreeSet<u32>,
    buttons: std::collections::BTreeSet<u8>,
}

impl Held {
    fn key(&mut self, keysym: u32, down: bool) {
        if down {
            self.keys.insert(keysym);
        } else {
            self.keys.remove(&keysym);
        }
    }

    fn button(&mut self, button: u8, pressed: bool) {
        if pressed {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
    }

    fn release(&mut self, cap: &mut dyn capture::Capture) {
        for keysym in std::mem::take(&mut self.keys) {
            let _ = cap.input(Input::KeyState { keysym, down: false });
        }
        for button in std::mem::take(&mut self.buttons) {
            let _ = cap.input(Input::Button { button, pressed: false });
        }
    }
}

fn serve_with(
    enc: &mut Wire,
    input: Box<dyn Read + Send>,
    cap: &mut Box<dyn capture::Capture>,
    held: &mut Held,
    screens: &mut crate::display::Session,
    mut chat: crate::chat::Window,
    mut allowed: u8,
) -> Result<(), String> {
    let mut desk = cap.desktop_size();
    let io_err = |e: io::Error| e.to_string();
    send(enc, &ToClient::Hello { w: desk.0 as u16, h: desk.1 as u16, backend: cap.name().into() }).map_err(io_err)?;
    let printer = crate::print::installed();
    let can = crate::proto::CAN_RESUME
        | if screens.adds() { crate::proto::CAN_SCREENS } else { 0 }
        | if screens.adds_with_helper() { crate::proto::CAN_SCREENS_WITH_HELPER } else { 0 }
        | if printer { crate::proto::CAN_PRINT } else { 0 }
        | if crate::hwenc::wanted() != crate::hwenc::Want::Never && crate::hwenc::has_h265() { crate::proto::CAN_H265 } else { 0 };
    send(enc, &ToClient::Can(can)).map_err(io_err)?;
    send(enc, &monitors(cap.as_ref(), screens)).map_err(io_err)?;
    send(enc, &modes(cap.as_ref(), screens)).map_err(io_err)?;
    if cap.name() == "none"
        && let Err(why) = cap.windows()
    {
        send(enc, &ToClient::Notice(crate::lang::fill(n!("This computer's screen cannot be shown: {}. Its terminal and files work."), &[&why]))).map_err(io_err)?;
    }
    // A session by ID recorded, as the computer is set to: its viewer is told.
    let mut rec = SessionRecording(None);
    if by_id() && crate::access::stored().ok().flatten().is_some_and(|c| c.record) {
        match crate::access::recordings_made().and_then(|d| crate::record::Recording::session(&d, cap.desktop_size())) {
            Ok(r) => {
                rec.0 = Some(r);
                send(enc, &ToClient::Notice(n!("This computer records this session.").into())).map_err(io_err)?;
            }
            Err(e) => eprintln!("sshremote agent: the session is not recorded: {e}"),
        }
    }
    if cap.name() == "portal-view" {
        send(enc, &ToClient::Notice(n!("This desktop lets its screen be seen, not controlled: it has no remote-desktop portal (as with Sway). Its terminal and files work.").into())).map_err(io_err)?;
    }

    let (tx, rx) = mpsc::channel::<Heard>();
    // A capture that says when a picture comes lets the loop wait for it instead of looking; so
    // do shells, forwarded connections and sound, or they would wait for the loop's next look.
    let woken = {
        let tx = tx.clone();
        cap.set_waker(Box::new(move || {
            let _ = tx.send(Heard::Wake);
        }))
    };
    let poke: std::sync::Arc<dyn Fn() + Send + Sync> = {
        let tx = tx.clone();
        std::sync::Arc::new(move || {
            let _ = tx.send(Heard::Wake);
        })
    };
    // Why the viewer's stream ended, when it ended badly, for the viewer to see.
    let (why_tx, why) = mpsc::channel::<String>();
    // Kinds of message this agent could not read, each reported once.
    let (skip_tx, skipped) = mpsc::channel::<u8>();
    thread::spawn(move || {
        let mut input = BufReader::new(input);
        let mut told = std::collections::HashSet::new();
        let mut first = true;
        loop {
            match read_msg(&mut input) {
                Ok(Some((k, p))) => match ToAgent::decode(k, &p) {
                    // Only as the remote access service's first word, never the viewer's.
                    Ok(ToAgent::TrustToken(t)) => {
                        if std::mem::take(&mut first) && by_id() && !t.is_empty() {
                            *TRUST.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
                        }
                    }
                    Ok(m) => {
                        first = false;
                        if tx.send(Heard::Msg(m)).is_err() {
                            break;
                        }
                    }
                    // A newer viewer's request: messages carry their length, so the stream stays in
                    // step and the rest of the session goes on.
                    Err(e) if crate::proto::is_unknown(&e) => {
                        if told.insert(k) {
                            let _ = skip_tx.send(k);
                        }
                    }
                    Err(_) => {
                        let _ = why_tx.send(crate::lang::fill(n!("the agent could not read message {} (different versions?)"), &[&k]));
                        break;
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    let _ = why_tx.send(crate::lang::fill(n!("the agent's input broke: {}"), &[&e]));
                    break;
                }
            }
        }
        // Said, as the capture's waker keeps the channel open.
        let _ = tx.send(Heard::Gone);
    });

    let mut settings: Option<Settings> = None;
    let mut pointer = PointerApart::default();
    // The last capture as it was, while few colours are sent (`video::changed_tiles`).
    let mut prev_img: Option<Image> = None;
    let mut size = (0, 0);
    let mut reducer: Option<Reducer> = None;
    let mut last: Option<Image> = None;
    let mut fresh = false;
    // The frame being made waited a moment for a second paint (see below).
    let mut waited = false;
    // Paused: no frames unless the viewer asks for one with Refresh.
    let (mut paused, mut one_shot) = (false, false);
    let mut prev: Option<Plane> = None;
    // Planes done with, filled again for the next frames: a fresh full-screen buffer each frame
    // cost Windows milliseconds of page faults.
    let mut spares: Vec<Vec<u8>> = Vec::new();
    let mut vid = video::Video::new();
    let mut colours = crate::color::Palette::default();
    // The picture's size changed: tell the viewer and start the stream over.
    macro_rules! resized {
        () => {
            desk = cap.desktop_size();
            announce(enc, cap.as_mut(), screens, desk, settings, &mut size)?;
            (prev, last) = (None, None);
            vid.reset();
        };
    }
    let mut clip = crate::clip::Clip::with_files();
    let mut inbox = crate::files::Inbox::default();
    let mut terms = crate::pty::Terms::waking(poke.clone());
    let mut ui = crate::ui::Ui::new();
    // When the viewer was last told its click fell outside the shared window (not before boot:
    // an `Instant` a minute back does not exist on a Windows host up for less than that).
    let mut told_outside: Option<Instant> = None;
    // Keys an accessibility action leaves to be typed, handled like the viewer's own.
    let mut queued: Vec<ToAgent> = Vec::new();
    let mut outbox = crate::files::Outbox::default();
    let mut sound: Option<crate::audio::Capture> = None;
    // A viewer kept from some things is told what it may do.
    if allowed != crate::grant::ALL {
        send(enc, &ToClient::Allowed(allowed)).map_err(io_err)?;
        let told = if cap.name() == "kms" {
            // Root's remote access on a Linux login or lock screen: nobody chose this.
            n!("This is the computer's login or lock screen: the keyboard and mouse work here, and the rest once someone signs in.").to_string()
        } else {
            crate::lang::fill(n!("The person at that computer lets you use: {}."), &[&crate::grant::words(allowed)])
        };
        send(enc, &ToClient::Notice(told)).map_err(io_err)?;
    }
    // Rights a request was turned away for since they last changed: said once each.
    let mut told_off = 0u8;
    // The computer itself: its own keyboard and mouse, its screen, locking as the viewer leaves.
    let mut host = crate::hostctl::HostCtl::default();
    // What is printed here, for a viewer that prints it (`ToAgent::Prints`), and when it was
    // last looked at.
    let mut spool: Option<(crate::print::Spool, Instant)> = None;
    // Connections for the viewer's forwarded ports.
    let mut ports = crate::porttunnel::Ports::waking(poke.clone());
    // A voice call: ringing (None) or answered, with the microphone here and a player for theirs.
    let mut call: Option<Option<(Option<crate::audio::Capture>, Option<crate::audio::Player>)>> = None;
    let mut pending = String::new();
    // A program started for the viewer, until its window is found and shown alone: one this
    // pane was opened for first (`app:` capture).
    let mut launching: Option<Launching> = None;
    if let Some((kind, text)) = START.lock().unwrap_or_else(|e| e.into_inner()).take() {
        match Launching::start(cap.as_ref(), kind, &text) {
            Ok(l) => launching = Some(l),
            Err(e) => send(enc, &ToClient::Launched { ok: false, text: e }).map_err(io_err)?,
        }
    }
    let mut in_flight = 0u32;
    // Sizes of the frames in flight, oldest first, matched to acknowledgements in order.
    let mut flight_sizes: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut next_id = 1u32;
    let mut due = Instant::now();
    let mut ending = false;
    loop {
        // Everything queued in the last turn goes out before waiting.
        enc.flush().map_err(io_err)?;
        // A download in progress keeps the loop turning; otherwise wait for input or the next frame.
        let longest = if outbox.busy() || ports.busy() { 1 } else { 8 };
        // A capture that wakes the loop is not looked at every millisecond for a new picture.
        let shortest = if woken && cap.wakes() && due <= Instant::now() { longest } else { 1 };
        let mut wait = due
            .saturating_duration_since(Instant::now())
            .clamp(Duration::from_millis(shortest), Duration::from_millis(longest));
        // A pointer the viewer draws is looked at on time however the loop is woken.
        if let Some(at) = pointer.next_look() {
            wait = wait.min(at.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
        }
        let mut msgs = Vec::new();
        let mut gone = false;
        match rx.recv_timeout(wait) {
            Ok(Heard::Msg(m)) => msgs.push(m),
            Ok(Heard::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Heard::Gone) | Err(_) => gone = true,
        }
        for h in rx.try_iter() {
            match h {
                Heard::Msg(m) => msgs.push(m),
                Heard::Wake => {}
                Heard::Gone => gone = true,
            }
        }
        // The viewer's last messages are seen to before the loop ends.
        ending |= gone;
        if ending && msgs.is_empty() {
            if let Ok(why) = why.try_recv() {
                let _ = send(enc, &ToClient::Error(why));
                let _ = enc.flush();
            }
            return Ok(());
        }
        msgs.splice(0..0, queued.drain(..));
        for k in skipped.try_iter() {
            let text = crate::lang::fill(n!("The SSH host's sshremote is older than this one and passed over a request it does not know (message {}); update it there."), &[&k]);
            send(enc, &ToClient::Notice(text)).map_err(io_err)?;
        }
        for m in msgs {
            // Text waiting to be pasted goes in before any later input, so the order is kept.
            let typed_text = matches!(m, ToAgent::Key { keysym, mods: 0 } if beyond_layout(keysym));
            if !typed_text && matches!(m, ToAgent::Key { .. } | ToAgent::KeyState { .. } | ToAgent::Button { .. }) {
                paste(&mut pending, &mut clip, cap.as_mut());
            }
            // What the person at the screen does not allow is turned away, and said once.
            if let Some(need) = crate::grant::needed(&m)
                && allowed & need == 0
            {
                let why = crate::lang::fill(n!("The person at that computer does not let you use {}."), &[&crate::grant::words(need)]);
                // A file's end follows its refused start: one answer is enough.
                if !matches!(m, ToAgent::FileEnd { .. }) {
                    for r in refused(&m, &why) {
                        send(enc, &r).map_err(io_err)?;
                    }
                }
                if let ToAgent::TermOpen { id, .. } = m {
                    send(enc, &ToClient::TermExit { id, text: why.clone() }).map_err(io_err)?;
                }
                if told_off & need == 0 {
                    told_off |= need;
                    send(enc, &ToClient::Notice(why)).map_err(io_err)?;
                }
                continue;
            }
            #[cfg(target_os = "macos")]
            if mac_signed_out()
                && matches!(
                    m,
                    ToAgent::ListDir { .. }
                        | ToAgent::Fetch { .. }
                        | ToAgent::FetchFrom { .. }
                        | ToAgent::FileStart { .. }
                        | ToAgent::FileData { .. }
                        | ToAgent::FileResume { .. }
                        | ToAgent::FileEnd { .. }
                        | ToAgent::FileOp { .. }
                        | ToAgent::PasteFiles { .. }
                        | ToAgent::TermOpen { .. }
                        | ToAgent::Launch { .. }
                        | ToAgent::Chat(_)
                        | ToAgent::Call(_)
                )
            {
                for r in refused(&m, MAC_NOBODY) {
                    send(enc, &r).map_err(io_err)?;
                }
                match m {
                    ToAgent::TermOpen { id, .. } => send(enc, &ToClient::TermExit { id, text: MAC_NOBODY.into() }).map_err(io_err)?,
                    ToAgent::Launch { .. } | ToAgent::PasteFiles { .. } | ToAgent::Chat(_) => send(enc, &ToClient::Notice(MAC_NOBODY.into())).map_err(io_err)?,
                    ToAgent::Call(_) => send(enc, &ToClient::CallState(crate::chat::CALL_ENDED)).map_err(io_err)?,
                    _ => {}
                }
                continue;
            }
            // Files are the signed-in person's business: an agent run as SYSTEM acts as them, and
            // with nobody signed in there are none to hand out.
            #[cfg(windows)]
            let _as_user = match &m {
                ToAgent::ListDir { .. }
                | ToAgent::Fetch { .. }
                | ToAgent::FetchFrom { .. }
                | ToAgent::FileStart { .. }
                | ToAgent::FileResume { .. }
                | ToAgent::FileEnd { .. }
                | ToAgent::FileOp { .. }
                | ToAgent::PasteFiles { .. } => {
                    match files_as_user() {
                        Ok(g) => g,
                        Err(why) => {
                            for r in refused(&m, why) {
                                send(enc, &r).map_err(io_err)?;
                            }
                            continue;
                        }
                    }
                }
                _ => None,
            };
            match m {
                // The computer's side ends the session (a session by ID left without input).
                ToAgent::End(why) => {
                    let _ = send(enc, &ToClient::Error(why));
                    let _ = enc.flush();
                    return Ok(());
                }
                ToAgent::Settings(s) => {
                    let t = target(desk, &s, cap.resizable());
                    if t != size {
                        cap.set_target(t.0, t.1)?;
                        size = t;
                        prev = None;
                        vid.reset();
                    }
                    if reducer.as_ref().is_none_or(|r| r.mode() != s.mode) {
                        reducer = Some(Reducer::new(s.mode));
                        prev = None;
                        vid.reset();
                    }
                    cap.set_fps(s.fps);
                    // Films in AV1 for a viewer that reads it, or in the codec it chose; one that
                    // switched gets a key frame.
                    vid.choose(s.formats & crate::proto::FORMAT_AV1 != 0, crate::proto::codec_of(s.formats), s.formats & crate::proto::FORMAT_H265 != 0);
                    settings = Some(s);
                    // A page laid out at the new size: the viewer hears its new size.
                    if cap.resizable() && cap.desktop_size() != desk {
                        resized!();
                    }
                }
                ToAgent::Ack(_) => {
                    in_flight = in_flight.saturating_sub(1);
                    flight_sizes.pop_front();
                }
                ToAgent::Call(true) => match chat.ring() {
                    Ok(()) => {
                        call = Some(None);
                        send(enc, &ToClient::CallState(crate::chat::CALL_RINGING)).map_err(io_err)?;
                    }
                    Err(e) => {
                        send(enc, &ToClient::Notice(crate::lang::fill(n!("Nobody can answer at that computer: {}"), &[&e]))).map_err(io_err)?;
                        send(enc, &ToClient::CallState(crate::chat::CALL_ENDED)).map_err(io_err)?;
                    }
                },
                ToAgent::Call(false) => {
                    if call.take().is_some() {
                        chat.hung_up();
                        send(enc, &ToClient::CallState(crate::chat::CALL_ENDED)).map_err(io_err)?;
                    }
                }
                ToAgent::Voice(p) => {
                    if let Some(Some((_, Some(player)))) = call.as_mut() {
                        player.push(&p);
                    }
                }
                ToAgent::PortOpen { id, host, port } => ports.open(id, host, port),
                ToAgent::PortData { id, data } => ports.data(id, data),
                ToAgent::PortClose { id } => ports.close(id),
                // Files copied at the viewer and sent over: on the clipboard here, then pasted.
                ToAgent::PasteFiles { batch, names } => {
                    let dir = crate::files::clipboard_dir(&batch);
                    let paths: Vec<_> = names.iter().map(|n| dir.join(n)).filter(|p| p.starts_with(&dir) && p.exists()).collect();
                    if allowed & crate::grant::CLIPBOARD == 0 {
                        send(enc, &ToClient::Notice(n!("The person at that computer does not let you use the clipboard.").into())).map_err(io_err)?;
                    } else if paths.is_empty() || !clip.set_files(paths) {
                        send(enc, &ToClient::Notice(n!("The files could not be put on that computer's clipboard.").into())).map_err(io_err)?;
                    } else {
                        shortcut(cap.as_mut(), 'v');
                    }
                }
                ToAgent::Host { act, on } => {
                    if act != crate::hostctl::QUERY {
                        let text = host.act(act, on).unwrap_or_else(|e| crate::lang::fill(n!("This computer: {}"), &[&e]));
                        send(enc, &ToClient::Notice(text)).map_err(io_err)?;
                    }
                    let can = if allowed & crate::grant::POWER != 0 { crate::hostctl::can() } else { 0 };
                    send(enc, &ToClient::HostState { can, on: host.on() }).map_err(io_err)?;
                }
                ToAgent::Refresh => {
                    prev = None;
                    one_shot = true;
                    vid.restart();
                }
                ToAgent::Pause(on) => {
                    paused = on;
                    if !on {
                        prev = None;
                    }
                }
                ToAgent::Snapshot => {
                    // Some backends only deliver a picture when the screen changes; wait a little for one.
                    for _ in 0..40 {
                        if let Some(img) = cap.grab()? {
                            if let Some(r) = rec.0.as_mut() {
                                r.picture(&img);
                            }
                            last = Some(img);
                            fresh = true;
                        }
                        // The shared window closed or changed size: fit the capture first, then grab again.
                        if cap.desktop_size() != desk {
                            resized!();
                            continue;
                        }
                        // Before the first settings no size is known, so any picture will do.
                        if last.as_ref().is_some_and(|i| settings.is_none() || (i.w, i.h) == size) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    let m = match last.as_ref().map(crate::shot::png_from_bgrx) {
                        Some(Ok(png)) => ToClient::Snapshot(png),
                        Some(Err(e)) => ToClient::Notice(crate::lang::fill(n!("Screenshot: {}"), &[&e])),
                        // No screen to share at all: why (what NoScreen holds).
                        None if cap.name() == "none" => ToClient::Notice(crate::lang::fill(n!("Screenshot: no screen to share: {}"), &[&cap.windows().err().unwrap_or_default()])),
                        None => ToClient::Notice(n!("Screenshot: no picture yet").into()),
                    };
                    send(enc, &m).map_err(io_err)?;
                }
                ToAgent::Ping(t) => send(enc, &ToClient::Pong(t)).map_err(io_err)?,
                // GNOME types only what its keyboard layout has; other text (Korean, accented
                // letters, emoji...) goes in as text, or is pasted.
                ToAgent::Key { keysym, mods: 0 } if beyond_layout(keysym) && cap.name() == "gnome" => {
                    if let Some(c) = char::from_u32(keysym & 0x00ff_ffff) {
                        pending.push(c);
                    }
                }
                ToAgent::Key { keysym, mods } => report(cap.input(Input::Key { keysym, mods })),
                ToAgent::Pointer { x, y } => {
                    let img = last.as_ref().map_or(desk, |i| (i.w, i.h));
                    let sx = (x as f64 + 0.5) * desk.0 as f64 / img.0.max(1) as f64;
                    let sy = (y as f64 + 0.5) * desk.1 as f64 / img.1.max(1) as f64;
                    report(cap.input(Input::Move { x: sx, y: sy }));
                    pointer.moved((sx as i32, sy as i32), Instant::now());
                }
                ToAgent::PointerBy { dx, dy } => {
                    if let Err(e) = cap.input(Input::MoveBy { dx: dx as i32, dy: dy as i32 }) {
                        if e == crate::capture::WHOLE_SCREEN_MOUSE && told_outside.is_none_or(|t| t.elapsed() > Duration::from_secs(4)) {
                            told_outside = Some(Instant::now());
                            send(enc, &ToClient::Notice(e)).map_err(io_err)?;
                        } else if e != crate::capture::WHOLE_SCREEN_MOUSE {
                            report(Err(e));
                        }
                    }
                }
                ToAgent::Button { button, pressed } => {
                    held.button(button, pressed);
                    // A click the shared window does not take: say once in a while how to get out.
                    if let Err(e) = cap.input(Input::Button { button, pressed }) {
                        if e == crate::capture::OUTSIDE && told_outside.is_none_or(|t| t.elapsed() > Duration::from_secs(4)) {
                            told_outside = Some(Instant::now());
                            send(enc, &ToClient::Notice(e)).map_err(io_err)?;
                        } else {
                            report(Err(e));
                        }
                    }
                }
                ToAgent::Scroll { steps } => report(cap.input(Input::Scroll { steps: steps as i32 })),
                ToAgent::HScroll { steps } => report(cap.input(Input::HScroll { steps: steps as i32 })),
                ToAgent::KeyState { keysym, down } => {
                    held.key(keysym, down);
                    report(cap.input(Input::KeyState { keysym, down }));
                }
                ToAgent::SecureAttention => report(cap.input(Input::SecureAttention)),
                ToAgent::Clipboard(text) => clip.set(text),
                // A picture for the paste the viewer sends next.
                ToAgent::Density(d) => cap.set_density(d as f64 / 100.0),
                ToAgent::ClipImage(png) if png.is_empty() => {}
                ToAgent::ClipImage(png) => {
                    if !clip.set_image(png) {
                        send(enc, &ToClient::Notice(n!("The picture could not be put on this computer's clipboard").into())).map_err(io_err)?;
                    }
                }
                ToAgent::FileStart { id, size, dir, name } => {
                    let (dir, name) = upload_place(&dir, name);
                    if let Some(m) = inbox.start_in(id, size, &dir, &name) {
                        send(enc, &m.into_client()).map_err(io_err)?;
                    }
                }
                ToAgent::Resumes => inbox.keep_broken = true,
                ToAgent::TrustMe => {
                    if let Some(t) = TRUST.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        send(enc, &ToClient::Trusted(t)).map_err(io_err)?;
                    }
                }
                ToAgent::TrustToken(_) => {}
                ToAgent::FileResume { id, size, from, tail, dir, name } => {
                    let (dir, name) = upload_place(&dir, name);
                    let m = match inbox.resume_in(id, size, &dir, &name, from, tail) {
                        Ok(from) => ToClient::FileFrom { id, from },
                        Err(note) => note.into_client(),
                    };
                    send(enc, &m).map_err(io_err)?;
                }
                ToAgent::FetchFrom { tag, path, name, from, tail } => {
                    for m in outbox.fetch_from(&path, tag, &name, from, tail) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::FileData { id, data } => {
                    if let Some(m) = inbox.data(id, &data) {
                        send(enc, &m.into_client()).map_err(io_err)?;
                    }
                }
                ToAgent::FileEnd { id } => {
                    if let Some(m) = inbox.end(id, true, "") {
                        send(enc, &m.into_client()).map_err(io_err)?;
                    }
                }
                ToAgent::ListDir { path } => send(enc, &crate::files::list(&path)).map_err(io_err)?,
                ToAgent::Fetch { tag, path } => {
                    for m in outbox.fetch(&path, tag) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::FetchAck { id, received } => outbox.ack(id, received),
                ToAgent::FileOp { op, path, arg } => send(enc, &crate::files::op(op, &path, &arg)).map_err(io_err)?,
                ToAgent::TermOpen { id, cols, rows } => {
                    if let Some(m) = terms.open(id, cols, rows) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::TermInput { id, data } => terms.input(id, &data),
                ToAgent::TermResize { id, cols, rows } => terms.resize(id, cols, rows),
                ToAgent::TermClose { id } => terms.close(id),
                ToAgent::FetchCancel { id, tag } => {
                    for m in outbox.cancel(id, tag) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::Chat(text) => {
                    if let Err(e) = chat.show(&crate::chat::trimmed(&text)) {
                        send(enc, &ToClient::Notice(crate::lang::fill(n!("Chat: {}"), &[&e]))).map_err(io_err)?;
                    }
                }
                ToAgent::Audio(on) => {
                    sound = None;
                    if on {
                        match crate::audio::capture() {
                            Ok(c) => sound = Some(c.waking(poke.clone())),
                            Err(e) => send(enc, &ToClient::Notice(crate::lang::fill(n!("Sound: {}"), &[&e]))).map_err(io_err)?,
                        }
                    }
                }
                ToAgent::SelectMonitor(i) => {
                    if let Err(e) = cap.select(i as usize) {
                        // The viewer hears why (a camera another program holds, one unplugged).
                        send(enc, &ToClient::Notice(e)).map_err(io_err)?;
                        continue;
                    }
                    // A camera looked at by someone who came by the ID is never a secret from
                    // the person in front of it.
                    if cap.name() == "camera"
                        && by_id()
                        && let Some(m) = cap.monitors().get(cap.current())
                    {
                        let _ = chat.show(&format!("(The viewer is looking at {}.)", m.name.replacen("Camera: ", "your camera ", 1)));
                    }
                    // A monitor shows all of it (not a window shared before); a camera is itself.
                    if cap.name() != "camera"
                        && let Ok(text) = cap.share(crate::capture::Share::Screen)
                    {
                        send(enc, &ToClient::Sharing { kind: 0, text }).map_err(io_err)?;
                    }
                    send(enc, &modes(cap.as_ref(), screens)).map_err(io_err)?;
                    resized!();
                }
                ToAgent::SetMode { w, h } => {
                    let id = if cap.name() == "camera" { None } else { screens.screen(cap.screen_id()) };
                    // The picture stops only for a change that is going to be made.
                    let done = match id.map(|id| screens.plan(&id, w, h).map(|p| (id, p))) {
                        Some(Ok((id, Some((w, h))))) => {
                            cap.screen_changing();
                            let done = screens.set(&id, w, h);
                            // The capture finds the screen at its new size; the viewer then hears it.
                            cap.screen_changed();
                            done
                        }
                        Some(Ok((_, None))) => Ok(()),
                        Some(Err(e)) => Err(e),
                        None => Err(n!("The resolution of that screen cannot be set from here.").to_string()),
                    };
                    if let Err(e) = done {
                        send(enc, &ToClient::Notice(e)).map_err(io_err)?;
                    }
                    send(enc, &modes(cap.as_ref(), screens)).map_err(io_err)?;
                    // Said at once, also to a viewer whose pictures are paused (one taking screenshots).
                    if cap.desktop_size() != desk {
                        resized!();
                    }
                }
                ToAgent::Prints(on) => {
                    spool = (on && printer).then(crate::print::Spool::new).flatten().map(|s| (s, Instant::now() - Duration::from_secs(1)));
                }
                ToAgent::Printed(id) => {
                    if let Some((s, _)) = spool.as_mut() {
                        s.done(id);
                    }
                }
                ToAgent::AddScreen { w, h } => {
                    match screens.add(w, h) {
                        // The viewer hears of it once the capture lists it.
                        Ok(id) => hear_of(cap.as_mut(), &id, true),
                        Err(e) => send(enc, &ToClient::Notice(e)).map_err(io_err)?,
                    }
                    send(enc, &monitors(cap.as_ref(), screens)).map_err(io_err)?;
                }
                ToAgent::RemoveScreen(name) => {
                    // Named as the viewer has it in `Monitors`; the system's name for it is its id.
                    let id = cap.monitors().iter().position(|m| m.name == name).and_then(|i| cap.screen_ids().get(i).cloned()).unwrap_or(name);
                    // Shown here: the first screen of the computer's own shows in its place.
                    if screens.added(&id)
                        && cap.screen_id().as_deref() == Some(id.as_str())
                        && let Some(i) = cap.screen_ids().iter().position(|s| !screens.added(s))
                        && cap.select(i).is_ok()
                    {
                        send(enc, &modes(cap.as_ref(), screens)).map_err(io_err)?;
                        resized!();
                    }
                    match screens.remove(&id) {
                        Ok(()) => hear_of(cap.as_mut(), &id, false),
                        Err(e) => send(enc, &ToClient::Notice(e)).map_err(io_err)?,
                    }
                    send(enc, &monitors(cap.as_ref(), screens)).map_err(io_err)?;
                    if cap.desktop_size() != desk {
                        resized!();
                    }
                }
                ToAgent::ListWindows => {
                    let m = match cap.windows() {
                        Ok(list) => ToClient::WindowList { list, error: String::new() },
                        Err(error) => ToClient::WindowList { list: Vec::new(), error },
                    };
                    send(enc, &m).map_err(io_err)?;
                }
                ToAgent::UiState { text, max } => {
                    let scope = crate::ui::Scope { window: cap.shared_window(), app: cap.shared_app() };
                    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
                    let mut tree = ui.state(cap.origin(), cap.desktop_size(), &scope, text, max.max(1) as usize);
                    // Accessibility lists panels, the desktop's background and a panel's own pop-ups
                    // as windows too: where the screen lists the windows it could bring forward, those
                    // are the ones offered (by their numbers as listed, which focusing goes by).
                    #[cfg(target_os = "linux")]
                    if let (ToClient::UiTree { windows, .. }, Ok(real)) = (&mut tree, cap.windows())
                        && !real.is_empty()
                    {
                        windows.retain(|(_, name)| real.iter().any(|w| w.title == *name));
                    }
                    send(enc, &tree).map_err(io_err)?;
                }
                ToAgent::UiAct { target, act, text } => {
                    let reply = match ui.act(target, act, &text) {
                        Ok(crate::ui::Outcome::Done(t)) => ToClient::UiDone { ok: true, text: t },
                        Ok(crate::ui::Outcome::ClickAt { x, y, button, double }) => {
                            report(cap.input(Input::Move { x, y }));
                            for _ in 0..if double { 2 } else { 1 } {
                                report(cap.input(Input::Button { button, pressed: true }));
                                report(cap.input(Input::Button { button, pressed: false }));
                            }
                            ToClient::UiDone { ok: true, text: "clicked".into() }
                        }
                        Ok(crate::ui::Outcome::Type { text, replace }) => {
                            if replace {
                                let m = cap.shortcut_key();
                                for (k, down) in [(m, true), ('a' as u32, true), ('a' as u32, false), (m, false)] {
                                    queued.push(ToAgent::KeyState { keysym: k, down });
                                }
                            }
                            for c in text.chars() {
                                let keysym = match c {
                                    '\n' => xk::RETURN,
                                    '\t' => xk::TAB,
                                    c if (c as u32) < 0x100 => c as u32,
                                    c => xk::unicode(c as u32),
                                };
                                queued.push(ToAgent::Key { keysym, mods: 0 });
                            }
                            ToClient::UiDone { ok: true, text: "typed".into() }
                        }
                        Ok(crate::ui::Outcome::Raise { title, focused }) => {
                            // The window manager brings it forward where the screen can ask it;
                            // the focus accessibility gave is all there is elsewhere.
                            let raised = cap.windows().ok().and_then(|list| list.into_iter().find(|w| w.title == title)).map(|w| cap.bring_forward(w.id));
                            // Under Wayland the compositor, where it can be asked.
                            #[cfg(target_os = "linux")]
                            let raised = raised.or_else(|| crate::ui::raise::by_title(&title));
                            match raised {
                                Some(Ok(())) => ToClient::UiDone { ok: true, text: format!("brought \"{title}\" to the front") },
                                _ if focused => ToClient::UiDone { ok: true, text: "asked the window to come forward".into() },
                                Some(Err(e)) => ToClient::UiDone { ok: false, text: e },
                                None => ToClient::UiDone { ok: false, text: "this desktop does not let programs bring windows to the front; click the window instead".into() },
                            }
                        }
                        Err(e) => ToClient::UiDone { ok: false, text: e },
                    };
                    send(enc, &reply).map_err(io_err)?;
                }
                ToAgent::PickOnRemote => match cap.pick() {
                    Ok(text) => {
                        send(enc, &ToClient::Sharing { kind: 0, text: n!("the whole screen").into() }).map_err(io_err)?;
                        resized!();
                        send(enc, &ToClient::Notice(text)).map_err(io_err)?;
                    }
                    Err(e) => send(enc, &ToClient::Notice(crate::lang::fill(n!("Sharing: {}"), &[&e]))).map_err(io_err)?,
                },
                ToAgent::InstallHelper => {
                    let reply = match cap.install_helper() {
                        Ok(text) => ToClient::HelperInstalled { ok: true, text },
                        Err(e) => ToClient::HelperInstalled { ok: false, text: crate::lang::fill(n!("Could not install the helper: {}"), &[&e]) },
                    };
                    send(enc, &reply).map_err(io_err)?;
                }
                ToAgent::Apps => {
                    let list = crate::apps::list();
                    let error = if list.is_empty() { n!("no programs were found on that computer").to_string() } else { String::new() };
                    let list = list.into_iter().map(|a| (a.id, a.name, a.detail)).collect();
                    send(enc, &ToClient::AppList { list, error }).map_err(io_err)?;
                }
                ToAgent::Launch { kind, text } => match Launching::start(cap.as_ref(), kind, &text) {
                    Ok(l) => launching = Some(l),
                    Err(e) => send(enc, &ToClient::Launched { ok: false, text: e }).map_err(io_err)?,
                },
                ToAgent::Share(scope) => {
                    let kind = scope.to_wire().0;
                    match cap.share(scope) {
                        Ok(text) => {
                            send(enc, &ToClient::Sharing { kind, text }).map_err(io_err)?;
                            ui.pin(kind);
                            resized!();
                        }
                        Err(e) => send(enc, &ToClient::Notice(crate::lang::fill(n!("Sharing: {}"), &[&e]))).map_err(io_err)?,
                    }
                }
            }
        }
        paste(&mut pending, &mut clip, cap.as_mut());
        if let Some(l) = launching.as_mut()
            && l.looked.elapsed() >= Duration::from_millis(300)
        {
            l.looked = Instant::now();
            let found = l.window(cap.as_ref());
            let late = l.since.elapsed() > Duration::from_secs(20);
            if found.is_some() || late {
                let l = launching.take().expect("checked");
                // Its windows; File Explorer's own window alone (its program is the shell, with the
                // person's other folders open).
                let share = found.map(|(w, app)| if app.eq_ignore_ascii_case("explorer") { crate::shared::Share::Window(w) } else { crate::shared::Share::App(w) });
                let reply = match share.map(|s| cap.share(s).map(|t| (s, t))) {
                    Some(Ok((s, text))) => {
                        let kind = s.to_wire().0;
                        send(enc, &ToClient::Sharing { kind, text }).map_err(io_err)?;
                        ui.pin(kind);
                        resized!();
                        ToClient::Launched { ok: true, text: crate::lang::fill(n!("{} opened, shown alone"), &[&l.what]) }
                    }
                    Some(Err(e)) => ToClient::Launched { ok: true, text: crate::lang::fill(n!("{} opened; it cannot be shown alone here ({})"), &[&l.what, &e]) },
                    None => ToClient::Launched { ok: true, text: crate::lang::fill(n!("{} opened, with no window of its own to show alone"), &[&l.what]) },
                };
                send(enc, &reply).map_err(io_err)?;
            }
        }
        if let Some((s, looked)) = spool.as_mut().filter(|(_, t)| t.elapsed() >= Duration::from_secs(1)) {
            *looked = Instant::now();
            for (id, name, size, path) in s.poll() {
                send(enc, &ToClient::PrintJob { id, size, name, path: path.to_string_lossy().into_owned() }).map_err(io_err)?;
            }
        }
        if host.taken_back() {
            send(enc, &ToClient::Notice(n!("The person at that computer took its keyboard and mouse back (Ctrl+P).").into())).map_err(io_err)?;
            send(enc, &ToClient::HostState { can: crate::hostctl::can(), on: host.on() }).map_err(io_err)?;
        }
        if let Some(Some((Some(mic), _))) = call.as_ref() {
            for p in mic.packets.try_iter() {
                send(enc, &ToClient::Voice(p)).map_err(io_err)?;
            }
        }
        for text in chat.replies() {
            // The call answered here: the microphone and the speakers here join it.
            if text == crate::chat::ANSWER && call.is_some() {
                let mic = crate::audio::microphone().map(|m| m.waking(poke.clone())).map_err(|e| send(enc, &ToClient::Notice(crate::lang::fill(n!("The microphone there: {}"), &[&e])))).ok();
                let player = crate::audio::Player::new().map_err(|e| send(enc, &ToClient::Notice(crate::lang::fill(n!("The speakers there: {}"), &[&e])))).ok();
                call = Some(Some((mic, player)));
                send(enc, &ToClient::CallState(crate::chat::CALL_TALKING)).map_err(io_err)?;
                continue;
            }
            if text == crate::chat::DECLINE || text == crate::chat::HANG_UP {
                let state = if text == crate::chat::DECLINE { crate::chat::CALL_DECLINED } else { crate::chat::CALL_ENDED };
                call = None;
                send(enc, &ToClient::CallState(state)).map_err(io_err)?;
                continue;
            }
            // The person at the screen ended it.
            if text == crate::chat::END || text == crate::chat::DENY {
                return Err(n!("The person at that computer's screen ended the connection.").into());
            }
            // The person at the screen changed what the viewer may do: what was taken away stops.
            if let Some(now) = text.strip_prefix(crate::chat::ALLOW).and_then(|b| b.parse::<u8>().ok()) {
                let now = now & crate::grant::ALL;
                let taken = allowed & !now;
                allowed = now;
                told_off = 0;
                if taken & crate::grant::CONTROL != 0 {
                    held.release(cap.as_mut());
                }
                if taken & crate::grant::SOUND != 0 {
                    sound = None;
                    if call.take().is_some() {
                        chat.hung_up();
                        send(enc, &ToClient::CallState(crate::chat::CALL_ENDED)).map_err(io_err)?;
                    }
                }
                if taken & crate::grant::TERMINAL != 0 {
                    terms = crate::pty::Terms::waking(poke.clone());
                }
                if taken & crate::grant::FILES != 0 {
                    // What was under way is called off, not kept to go on with.
                    let keep = std::mem::take(&mut inbox.keep_broken);
                    (inbox, outbox) = (Default::default(), Default::default());
                    inbox.keep_broken = keep;
                }
                if taken & crate::grant::POWER != 0 {
                    host = crate::hostctl::HostCtl::default();
                }
                if taken & crate::grant::FILES != 0 {
                    spool = None;
                }
                if taken & crate::grant::PORTS != 0 {
                    ports = crate::porttunnel::Ports::waking(poke.clone());
                }
                send(enc, &ToClient::Allowed(allowed)).map_err(io_err)?;
                send(enc, &ToClient::Notice(crate::lang::fill(n!("The person at that computer now lets you use: {}."), &[&crate::grant::words(allowed)]))).map_err(io_err)?;
                continue;
            }
            send(enc, &ToClient::Chat(text)).map_err(io_err)?;
        }
        if let Some(c) = &sound {
            for p in c.packets.try_iter() {
                send(enc, &ToClient::Audio(p)).map_err(io_err)?;
            }
        }
        if let Some(e) = sound.as_ref().and_then(|c| c.failed.try_recv().ok()) {
            sound = None;
            send(enc, &ToClient::Notice(crate::lang::fill(n!("Sound: {}"), &[&e]))).map_err(io_err)?;
        }
        for m in terms.poll() {
            send(enc, &m).map_err(io_err)?;
        }
        // Downloads share the stream with the screen, a slice per turn.
        // A file or folder is opened as the person signed in (an agent run as SYSTEM); one who
        // signed out ends what is left of the downloads.
        #[cfg(windows)]
        let _as_user = if outbox.opens_next() {
            match files_as_user() {
                Ok(g) => g,
                Err(why) => {
                    for m in outbox.cancel_all(why) {
                        send(enc, &m).map_err(io_err)?;
                    }
                    None
                }
            }
        } else {
            None
        };
        for m in outbox.pump(256 << 10) {
            send(enc, &m).map_err(io_err)?;
        }
        for m in ports.pump(256 << 10) {
            send(enc, &m).map_err(io_err)?;
        }
        match clip.poll() {
            Some(crate::clip::Copied::Text(text)) if allowed & crate::grant::CLIPBOARD != 0 => {
                send(enc, &ToClient::Clipboard(text)).map_err(io_err)?;
            }
            // A picture copied here (a screenshot): onto the viewer's clipboard.
            Some(crate::clip::Copied::Image(png)) if allowed & crate::grant::CLIPBOARD != 0 => {
                send(enc, &ToClient::ClipImage(png)).map_err(io_err)?;
            }
            // Files copied here: the viewer may fetch them onto its own clipboard.
            Some(crate::clip::Copied::Files(paths)) if allowed & (crate::grant::CLIPBOARD | crate::grant::FILES) == crate::grant::CLIPBOARD | crate::grant::FILES => {
                send(enc, &ToClient::ClipFiles(paths.iter().map(|p| crate::files::clip_file(p)).collect())).map_err(io_err)?;
            }
            _ => {}
        }
        if let Some(text) = cap.notice() {
            send(enc, &ToClient::Notice(text)).map_err(io_err)?;
        }
        // The choice made in the desktop's sharing dialog, even while the picture is paused.
        match cap.poll_share() {
            Some(Ok((kind, text))) => {
                send(enc, &ToClient::Sharing { kind, text }).map_err(io_err)?;
                ui.pin(kind);
            }
            Some(Err(e)) => send(enc, &ToClient::Notice(crate::lang::fill(n!("Sharing: {}"), &[&e]))).map_err(io_err)?,
            None => {}
        }
        let (Some(s), Some(red)) = (settings, reducer.as_ref()) else {
            continue;
        };
        let now = Instant::now();
        pointer.follow(enc, cap.as_mut(), s.formats & crate::proto::FORMAT_POINTER != 0, now).map_err(io_err)?;
        // Grab only when a frame may go out: some backends copy the whole screen per grab.
        let light = in_flight < IN_FLIGHT_SMALL && flight_sizes.iter().sum::<usize>() < SMALL_BYTES;
        if paused && !one_shot {
            // Recorded all the same: a viewer that paused the picture (a program driving the
            // computer by snapshots) still acts on it.
            if let Some(r) = rec.0.as_mut().filter(|r| r.wants())
                && let Some(img) = cap.grab()?
            {
                r.picture(&img);
                if let Some(old) = last.replace(img) {
                    cap.recycle(old);
                }
                fresh = true;
            }
            continue;
        }
        if (in_flight >= IN_FLIGHT && !light) || now < due {
            continue;
        }
        if let Some(img) = cap.grab()? {
            if let Some(r) = rec.0.as_mut() {
                r.picture(&img);
            }
            if let Some(old) = last.replace(img) {
                cap.recycle(old);
            }
            fresh = true;
        }
        // A shared window was resized or moved to where its size changed.
        if cap.desktop_size() != desk {
            resized!();
            continue;
        }
        let Some(img) = last.as_ref().filter(|i| (i.w, i.h) == size) else {
            continue;
        };
        // A film gone still is looked at once more, to send its pixels losslessly.
        if !fresh && prev.is_some() && !vid.quiet() {
            continue;
        }
        fresh = false;
        let plane = red.reduce_into(img, spares.pop().unwrap_or_default());
        // A whole new picture: the viewer starts its palette over too.
        let whole = prev.is_none();
        let mut rects = diff(prev.as_ref(), &plane);
        let found = prev.as_ref().and_then(|p| scroll::find(p, &plane, &rects));
        // Most of the screen changing often comes as two paints a moment apart (a window, then
        // what is in it): one frame's wait sends them as one. Not a scroll, a film or a frame asked for.
        let most = rects.iter().map(|r| r.w * r.h).sum::<usize>() * 2 > plane.w * plane.h;
        if most && !whole && !one_shot && !waited && found.is_none() && vid.region.is_none() {
            waited = true;
            fresh = true;
            due = now + Duration::from_millis(34);
            spares.push(plane.data);
            continue;
        }
        waited = false;
        one_shot = false;
        let mut moves = Vec::new();
        let mut scrolled = false;
        if let Some(m) = found
            && let Some(mut moved) = prev.take()
        {
            // The previous screen is replaced below anyway, so it is moved in place, not copied.
            scroll::apply(&mut moved.data, moved.w, moved.mode.bpp(), &m);
            let after = diff(Some(&moved), &plane);
            // A move that leaves most of the change unexplained still saves what it explains, but
            // is a likeness rather than a scroll to the film finder: a film panning in few colours
            // matches itself shifted here and there.
            let area = |rs: &[crate::color::Rect]| rs.iter().map(|r| r.w * r.h).sum::<usize>();
            scrolled = area(&after) * 2 <= area(&rects);
            rects = after;
            moves.push(m);
            spares.push(moved.data);
        } else if s.formats & crate::proto::FORMAT_SIDEWAYS != 0
            && let Some(p) = prev.as_ref()
        {
            // Text moving along its line as a character is typed into it (not in a film).
            let text: Vec<_> = rects.iter().filter(|r| vid.region.is_none_or(|v| !video::overlaps(r, &v))).copied().collect();
            let found = scroll::sideways(p, &plane, &text);
            if !found.is_empty() {
                let mut data = spares.pop().unwrap_or_default();
                data.clear();
                data.extend_from_slice(&p.data);
                let mut moved = Plane { data, ..*p };
                for m in &found {
                    scroll::apply(&mut moved.data, moved.w, moved.mode.bpp(), m);
                }
                let after = diff(Some(&moved), &plane);
                let area = |rs: &[crate::color::Rect]| rs.iter().map(|r| r.w * r.h).sum::<usize>();
                if area(&after) * 4 <= area(&rects) * 3 {
                    rects = after;
                    moves = found;
                }
                spares.push(moved.data);
            }
        }
        let mut video_part = None;
        // A film goes as one at every number of colours: frame after frame of it as tiles would
        // cost a black and white picture more than a colour one. Fewer colours, a grey film.
        let grey = !matches!(plane.mode, Mode::Color256 | Mode::True);
        // In few colours most of a film's pixels keep their shade from one frame to the next, so
        // it is told by what the screen itself did.
        let watched = match (&prev_img, grey) {
            (Some(p), true) => video::changed_tiles(p, img),
            _ => rects.clone(),
        };
        // A whole new picture (a start, a refresh) is no change of the screen: it makes no film,
        // and shows a film playing as one.
        let watched = if whole { vid.region.map(|r| vec![r]).unwrap_or_default() } else { watched };
        prev_img = grey.then(|| Image { w: img.w, h: img.h, stride: img.stride, data: img.data.clone() });
        if let video::Event::Ended(r) = vid.observe(plane.w, plane.h, &watched, scrolled, &|r| crate::color::rich(img, r), &|r| crate::color::photo_cell(img, r)) {
            // Its pixels went as a film: they go again losslessly, tile by tile, but for the
            // tiles this frame sends whole anyway.
            let inside = |c: &crate::color::Rect, t: &crate::color::Rect| c.x <= t.x && c.y <= t.y && c.x + c.w >= t.x + t.w && c.y + c.h >= t.y + t.h;
            for ty in (r.y..r.y + r.h).step_by(32) {
                for tx in (r.x..r.x + r.w).step_by(32) {
                    let t = crate::color::Rect { x: tx, y: ty, w: 32.min(r.x + r.w - tx), h: 32.min(r.y + r.h - ty) };
                    if !rects.iter().any(|c| inside(c, &t)) {
                        rects.push(t);
                    }
                }
            }
        }
        if let Some(reg) = vid.region {
            if watched.iter().any(|r| video::overlaps(r, &reg)) {
                let rate =
                    (reg.w * reg.h) as f64 * s.fps as f64 * if plane.mode == Mode::True { 0.08 } else { 0.04 };
                let data = vid.encode(img, s.fps, rate.clamp(150e3, 6e6) as u32, grey)?;
                let (x, y, w, h) = (reg.x as u16, reg.y as u16, reg.w as u16, reg.h as u16);
                video_part = Some(Tile { x, y, w, h, data });
            }
            rects = rects.iter().flat_map(|r| video::subtract(r, &reg)).collect();
        }
        if let Some(old) = prev.replace(plane) {
            spares.push(old.data);
        }
        spares.truncate(2);
        if rects.is_empty() && moves.is_empty() && video_part.is_none() {
            due = now + Duration::from_millis(1000 / s.fps.max(1) as u64);
            continue;
        }
        let plane = prev.as_ref().unwrap();
        let palette = plane.mode == Mode::True && s.formats & crate::proto::FORMAT_PALETTE != 0;
        let wide = s.formats & crate::proto::FORMAT_WIDE != 0;
        let rects = crate::color::join_rows(plane, &rects, palette, wide);
        let (palette, data) = if palette {
            let (fresh, changed, data) = colours.pack(plane, &rects, whole, wide);
            (Some((fresh, changed)), data)
        } else {
            (None, rects.iter().map(|r| pack(plane, r.x, r.y, r.w, r.h)).collect())
        };
        let tiles = rects
            .iter()
            .zip(data)
            .map(|(r, data)| Tile { x: r.x as u16, y: r.y as u16, w: r.w as u16, h: r.h as u16, data })
            .collect();
        let frame = Frame {
            id: next_id,
            w: plane.w as u16,
            h: plane.h as u16,
            mode: plane.mode,
            moves,
            palette,
            tiles,
            video: video_part,
        };
        next_id = next_id.wrapping_add(1);
        let bytes = send_sized(enc, &ToClient::Frame(frame)).map_err(io_err)?;
        in_flight += 1;
        flight_sizes.push_back(bytes);
        due = now + Duration::from_millis(1000 / s.fps.max(1) as u64);
    }
}

/// The pointer as a viewer that draws it itself sees it: apart from the pictures, its shape sent
/// as it changes and its place as the host moves it (not where the viewer just put it).
#[derive(Default)]
struct PointerApart {
    asked: bool,
    apart: bool,
    /// What the viewer was told last.
    at: Option<Option<(i16, i16)>>,
    /// Where the viewer moved it lately, in the host's pixels: a host tells where its pointer is
    /// a moment late, while the viewer has moved it on.
    moved_to: std::collections::VecDeque<((i32, i32), Instant)>,
    due: Option<Instant>,
}

impl PointerApart {
    fn follow(&mut self, enc: &mut Wire, cap: &mut dyn capture::Capture, asked: bool, now: Instant) -> io::Result<()> {
        if asked != self.asked {
            (self.asked, self.at) = (asked, None);
            self.apart = cap.pointer_apart(asked) && asked;
        }
        if !self.apart || self.due.is_some_and(|d| now < d) {
            return Ok(());
        }
        // As often as a screen refreshes.
        self.due = Some(now + Duration::from_millis(16));
        let (shape, at) = cap.pointer();
        if let Some(p) = shape {
            let (w, h, hot_x, hot_y) = (p.w as u16, p.h as u16, p.hot.0 as u16, p.hot.1 as u16);
            send(enc, &ToClient::PointerShape { w, h, hot_x, hot_y, rgba: p.rgba })?;
        }
        let at = at.map(|(x, y)| (x.clamp(-32768, 32767) as i16, y.clamp(-32768, 32767) as i16));
        if let Some(at) = self.news(at) {
            send(enc, &ToClient::PointerAt(at))?;
        }
        Ok(())
    }

    /// When the pointer is next looked at, while the viewer draws it.
    fn next_look(&self) -> Option<Instant> {
        self.apart.then(|| self.due.unwrap_or_else(Instant::now))
    }

    fn moved(&mut self, to: (i32, i32), now: Instant) {
        while self.moved_to.front().is_some_and(|m| now.duration_since(m.1) > Duration::from_millis(300)) || self.moved_to.len() >= 32 {
            self.moved_to.pop_front();
        }
        self.moved_to.push_back((to, now));
    }

    /// What the viewer is to hear of where the pointer is now, if anything: not where the viewer
    /// itself put it lately, unless it last heard the pointer was hidden.
    fn news(&mut self, at: Option<(i16, i16)>) -> Option<Option<(i16, i16)>> {
        if self.at == Some(at) {
            return None;
        }
        let told = self.at.replace(at);
        let echo = at.is_some_and(|(x, y)| self.moved_to.iter().any(|((mx, my), _)| (x as i32 - mx).abs() <= 1 && (y as i32 - my).abs() <= 1))
            && matches!(told, Some(Some(_)));
        (!echo).then_some(at)
    }
}

/// A program started for the viewer, whose window is looked for to show it alone.
struct Launching {
    /// The windows there before it started.
    before: std::collections::HashSet<u64>,
    /// Its program's name, to know it by when it opened no new window (one already running).
    name: String,
    /// What the viewer is told it is.
    what: String,
    since: Instant,
    looked: Instant,
}

/// What a pane opened for a program asks to start (`app:KIND:TEXT` as its capture).
static START: std::sync::Mutex<Option<(u8, String)>> = std::sync::Mutex::new(None);

/// The capture to open, keeping aside a program to start (`app:KIND:TEXT`, on the desktop's own).
fn starting(backend: &str) -> &str {
    let Some(rest) = backend.strip_prefix("app:") else { return backend };
    let (kind, text) = rest.split_once(':').unwrap_or(("1", rest));
    *START.lock().unwrap_or_else(|e| e.into_inner()) = Some((kind.parse().unwrap_or(crate::apps::COMMAND), text.to_string()));
    "auto"
}

impl Launching {
    /// Starts it, with the windows there before it so its own is the one that is new.
    fn start(cap: &dyn capture::Capture, kind: u8, text: &str) -> Result<Launching, String> {
        let before = cap.windows().map(|l| l.iter().map(|w| w.id).collect()).unwrap_or_default();
        let name = crate::apps::launch(kind, text)?;
        let what = if name.is_empty() { text.trim().to_string() } else { name.clone() };
        Ok(Launching { before, name, what, since: Instant::now(), looked: Instant::now() })
    }

    /// Its window and program: the first window that is new, or (after a few seconds) the
    /// topmost of the program started (one already running brings its window forward instead).
    fn window(&self, cap: &dyn capture::Capture) -> Option<(u64, String)> {
        let list = cap.windows().ok()?;
        if let Some(w) = list.iter().find(|w| !self.before.contains(&w.id)) {
            return Some((w.id, w.app.clone()));
        }
        let name = self.name.to_lowercase();
        if self.since.elapsed() < Duration::from_secs(3) || name.is_empty() {
            return None;
        }
        list.iter()
            .find(|w| {
                let app = w.app.to_lowercase();
                !app.is_empty() && (app.contains(&name) || name.contains(&app))
            })
            .map(|w| (w.id, w.app.clone()))
    }
}

/// A character a keyboard layout may lack: beyond ASCII (Latin-1's own keysyms, or Unicode's).
fn beyond_layout(keysym: u32) -> bool {
    xk::is_unicode(keysym) || (0xa0..=0xff).contains(&keysym)
}

/// Puts in text the backend cannot type: as text where it can (GNOME with the helper: terminals
/// too), else through the clipboard and Ctrl+V.
fn paste(pending: &mut String, clip: &mut crate::clip::Clip, cap: &mut dyn capture::Capture) {
    if pending.is_empty() {
        return;
    }
    if cap.commit_text(pending) {
        pending.clear();
        return;
    }
    if !clip.stage_paste(std::mem::take(pending)) {
        report(Err("text outside the keyboard layout needs the clipboard, which is unavailable".into()));
        return;
    }
    shortcut(cap, 'v');
    // Applications fetch pasted text asynchronously; let it land before the next keys.
    thread::sleep(Duration::from_millis(150));
}

/// `key` with the key shortcuts take here (Ctrl, or the command key on a Mac), as pressed.
fn shortcut(cap: &mut dyn capture::Capture, key: char) {
    let m = cap.shortcut_key();
    for (k, down) in [(m, true), (key as u32, true), (key as u32, false), (m, false)] {
        report(cap.input(Input::KeyState { keysym: k, down }));
    }
}

/// The shown screen's resolutions, for the viewer to set it to one (none where it cannot).
fn modes(cap: &dyn capture::Capture, screens: &crate::display::Session) -> ToClient {
    let id = if cap.name() == "camera" { None } else { screens.screen(cap.screen_id()) };
    match id.and_then(|id| screens.modes(&id)) {
        // One resolution only (a virtual screen): nothing to choose.
        Some((m, own)) if m.list.len() > 1 => ToClient::Modes { list: m.list, current: m.current, own },
        _ => ToClient::Modes { list: Vec::new(), current: (0, 0), own: (0, 0) },
    }
}

fn monitors(cap: &dyn capture::Capture, screens: &crate::display::Session) -> ToClient {
    let ids = cap.screen_ids();
    let list = cap
        .monitors()
        .into_iter()
        .enumerate()
        .map(|(i, m)| crate::proto::MonitorInfo { name: m.name, w: m.w as u16, h: m.h as u16, primary: m.primary, at: m.at, added: ids.get(i).is_some_and(|id| screens.added(id)) })
        .collect();
    ToClient::Monitors { list, current: cap.current() as u8 }
}

/// Waits (up to three seconds) for the capture to list screen `id`, or (`there` false) no longer.
fn hear_of(cap: &mut dyn capture::Capture, id: &str, there: bool) {
    for i in 0..30 {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        cap.screens_changed();
        if cap.screen_ids().iter().any(|s| s == id) == there {
            return;
        }
    }
}

/// Tells the viewer the new picture size and fits the capture to it.
fn announce(
    enc: &mut Wire,
    cap: &mut dyn capture::Capture,
    screens: &crate::display::Session,
    desk: (usize, usize),
    settings: Option<Settings>,
    size: &mut (usize, usize),
) -> Result<(), String> {
    let io_err = |e: io::Error| e.to_string();
    let hello = ToClient::Hello { w: desk.0 as u16, h: desk.1 as u16, backend: cap.name().into() };
    send(enc, &hello).map_err(io_err)?;
    send(enc, &monitors(cap, screens)).map_err(io_err)?;
    if let Some(s) = settings {
        *size = target(desk, &s, cap.resizable());
        cap.set_target(size.0, size.1)?;
    }
    Ok(())
}

fn report(r: Result<(), String>) {
    if let Err(e) = r {
        eprintln!("sshremote agent: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(w: u16, h: u16, scale: u8) -> Settings {
        Settings { view_w: w, view_h: h, scale, mode: Mode::True, fps: 30, formats: 0 }
    }

    /// The picture keeps the host's shape at the viewer's size exactly on the limiting side, is
    /// never larger than the host, and the scale rounds rather than drops a pixel.
    #[test]
    fn the_picture_size_is_exact_on_the_limiting_side() {
        assert_eq!(target((1920, 1080), &view(1280, 720, 100), false), (1280, 720));
        assert_eq!(target((1920, 1080), &view(1366, 768, 100), false), (1365, 768));
        assert_eq!(target((1920, 1080), &view(1920, 1200, 100), false), (1920, 1080));
        assert_eq!(target((1920, 1080), &view(4000, 3000, 100), false), (1920, 1080), "never enlarged");
        assert_eq!(target((1920, 1080), &view(1280, 720, 50), false), (640, 360));
        assert_eq!(target((1920, 1080), &view(1280, 720, 255), false), (1280, 720), "a scale above 100 is 100");
        // A web page takes the size it is shown at, whatever its shape was.
        assert_eq!(target((1280, 800), &view(1178, 696, 100), true), (1178, 696));
        assert_eq!(target((1280, 800), &view(1178, 696, 50), true), (589, 348));
    }

    /// Small and large messages go in frames of their own levels, and a viewer reads them back
    /// in order as one stream.
    #[test]
    fn messages_read_back_across_the_streams_frames() {
        struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let buf = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut w = Wire::new(Box::new(Shared(buf.clone())));
        let big: Vec<u8> = (0..Wire::BIG + 10).map(|i| (i * 7 % 251) as u8).collect();
        let sent: Vec<(u8, Vec<u8>)> = vec![(1, b"one".to_vec()), (2, b"two".to_vec()), (3, big.clone()), (4, b"four".to_vec()), (5, big), (6, Vec::new())];
        for (i, (k, p)) in sent.iter().enumerate() {
            w.message(*k, p).unwrap();
            if i % 2 == 1 {
                w.flush().unwrap();
            }
        }
        w.flush().unwrap();
        let bytes = buf.borrow().clone();
        assert!(bytes.len() < 2 * Wire::BIG / 4, "compressed: {}", bytes.len());
        let mut dec = zstd::stream::read::Decoder::new(&bytes[..]).unwrap();
        // The last frame stays open, as the stream goes on.
        let got: Vec<(u8, Vec<u8>)> = sent.iter().map(|_| read_msg(&mut dec).unwrap().expect("a message")).collect();
        assert_eq!(got, sent);
    }

    /// The viewer hears where the host's pointer is, but not of its own moves, unless it last
    /// heard the pointer was hidden.
    #[test]
    fn a_pointer_the_viewer_moved_is_told_only_after_it_was_hidden() {
        let mut p = PointerApart::default();
        let t = Instant::now();
        assert_eq!(p.news(None), Some(None));
        p.moved((10, 10), t);
        assert_eq!(p.news(Some((10, 10))), Some(Some((10, 10))), "shown again, where the viewer put it");
        p.moved((20, 20), t);
        p.moved((30, 30), t);
        assert_eq!(p.news(Some((20, 21))), None, "the viewer's own move, told late");
        assert_eq!(p.news(Some((30, 30))), None, "and its last");
        assert_eq!(p.news(Some((50, 60))), Some(Some((50, 60))), "moved there on the host");
        assert_eq!(p.news(Some((50, 60))), None, "told once");
        assert_eq!(p.news(None), Some(None), "hidden");
        // Moves forgotten after a moment: the host moving it back there later is told.
        p.moved((70, 70), t + Duration::from_secs(1));
        assert_eq!(p.news(Some((50, 60))), Some(Some((50, 60))));
        assert_eq!(p.news(Some((20, 20))), Some(Some((20, 20))));
    }
}
