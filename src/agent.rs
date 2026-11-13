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

type Wire = zstd::stream::write::Encoder<'static, BufWriter<Box<dyn Write>>>;

/// Queues a message; the loop flushes once per turn, before it waits, so several messages share
/// one compressed block.
fn send(w: &mut Wire, m: &ToClient) -> io::Result<()> {
    send_sized(w, m).map(|_| ())
}

/// [`send`], returning the message's size before compression.
fn send_sized(w: &mut Wire, m: &ToClient) -> io::Result<usize> {
    let (k, p) = m.encode();
    write_msg(w, k, &p)?;
    Ok(p.len())
}

/// The picture's size for the viewer's area: the host's shape kept, never larger than the host,
/// then the preset's scale. The limiting side is exact and the other rounded, so a 1920x1080
/// desktop in a 1280x720 view is 1280x720, not a pixel short to be resampled.
fn target(desk: (usize, usize), s: &Settings) -> (usize, usize) {
    let (dw, dh) = (desk.0.max(1) as f64, desk.1.max(1) as f64);
    let (vw, vh) = (s.view_w.max(16) as f64, s.view_h.max(16) as f64);
    let (w, h) = if vw * dh <= vh * dw {
        let w = vw.min(dw);
        (w, (dh * w / dw).round())
    } else {
        let h = vh.min(dh);
        ((dw * h / dh).round(), h)
    };
    let k = s.scale.clamp(1, 100) as f64 / 100.0;
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
            u.impersonate().map(Some).ok_or("could not act as the person signed in at that computer")
        }
    }
}

/// What a file request refused for `why` is answered with.
fn refused(m: &ToAgent, why: &str) -> Vec<ToClient> {
    let why = why.to_string();
    match m {
        ToAgent::ListDir { path } => vec![ToClient::DirList { path: path.clone(), entries: Vec::new(), error: why }],
        ToAgent::Fetch { tag, .. } => {
            let mut out = vec![ToClient::FetchEnd { id: 0, ok: false, text: why }];
            if tag & crate::files::DONE_TAG != 0 {
                out.push(ToClient::FetchDone { tag: *tag });
            }
            out
        }
        ToAgent::FileStart { id, .. } | ToAgent::FileEnd { id } => vec![ToClient::FileDone { id: *id, ok: false, text: why }],
        ToAgent::FileOp { .. } => vec![ToClient::OpDone { ok: false, text: why }],
        _ => Vec::new(),
    }
}

/// The viewer came by this computer's remote access ID (rather than over ssh as its owner).
static BY_ID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// What such a viewer may do, and whether the person at the screen is asked first (`grant`).
static GRANT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(crate::grant::ALL);

pub fn mark_by_id(grant: u8) {
    BY_ID.store(true, std::sync::atomic::Ordering::Relaxed);
    GRANT.store(grant, std::sync::atomic::Ordering::Relaxed);
}

pub fn by_id() -> bool {
    BY_ID.load(std::sync::atomic::Ordering::Relaxed)
}

/// The rights and whether to ask, as given to this agent.
pub fn grant() -> u8 {
    GRANT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Serves on stdin/stdout, or on the named pipes a Windows relay created.
pub fn run(backend: &str, pipe: Option<&str>) -> Result<(), String> {
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
    let mut enc =
        zstd::stream::write::Encoder::new(BufWriter::with_capacity(1 << 16, output), 3).map_err(|e| e.to_string())?;
    // A long window lets earlier screens act as a dictionary for repeats.
    enc.window_log(24).map_err(|e| e.to_string())?;
    enc.long_distance_matching(true).map_err(|e| e.to_string())?;
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
        if grant() & crate::grant::ASK != 0 {
            // Asked before the screen is opened: nothing of it is seen until then.
            send(enc, &ToClient::Notice(crate::chat::WAITING.into())).and_then(|()| enc.flush()).map_err(|e| e.to_string())?;
            allowed = match chat.ask(allowed, ASK_FOR) {
                Ok(Some(a)) => a,
                Ok(None) => return Err("The person at that computer did not let you in.".into()),
                Err(e) => return Err(format!("Nobody could let you in at that computer: {e}")),
            };
        } else {
            // Someone who came by this computer's ID is made known to the person at the screen,
            // who can end it (nobody signed in: nobody to tell).
            let _ = chat.notice(allowed);
        }
    }
    let mut cap = capture::open(backend)?;
    let mut held = Held::default();
    let result = serve_with(enc, input, &mut cap, &mut held, chat, allowed);
    // What the viewer still held down at the host is let go, however the session ended.
    held.release(cap.as_mut());
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
    mut chat: crate::chat::Window,
    mut allowed: u8,
) -> Result<(), String> {
    let mut desk = cap.desktop_size();
    let io_err = |e: io::Error| e.to_string();
    send(enc, &ToClient::Hello { w: desk.0 as u16, h: desk.1 as u16, backend: cap.name().into() }).map_err(io_err)?;
    send(enc, &monitors(cap.as_ref())).map_err(io_err)?;

    let (tx, rx) = mpsc::channel();
    // Why the viewer's stream ended, when it ended badly, for the viewer to see.
    let (why_tx, why) = mpsc::channel::<String>();
    // Kinds of message this agent could not read, each reported once.
    let (skip_tx, skipped) = mpsc::channel::<u8>();
    thread::spawn(move || {
        let mut input = BufReader::new(input);
        let mut told = std::collections::HashSet::new();
        loop {
            match read_msg(&mut input) {
                Ok(Some((k, p))) => match ToAgent::decode(k, &p) {
                    Ok(m) => {
                        if tx.send(m).is_err() {
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
                        let _ = why_tx.send(format!("the agent could not read message {k} (different versions?)"));
                        break;
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    let _ = why_tx.send(format!("the agent's input broke: {e}"));
                    break;
                }
            }
        }
    });

    let mut settings: Option<Settings> = None;
    let mut size = (0, 0);
    let mut reducer: Option<Reducer> = None;
    let mut last: Option<Image> = None;
    let mut fresh = false;
    // The frame being made waited a moment for a second paint (see below).
    let mut waited = false;
    // Paused: no frames unless the viewer asks for one with Refresh.
    let (mut paused, mut one_shot) = (false, false);
    let mut prev: Option<Plane> = None;
    let mut vid = video::Video::new();
    let mut colours = crate::color::Palette::default();
    // The picture's size changed: tell the viewer and start the stream over.
    macro_rules! resized {
        () => {
            desk = cap.desktop_size();
            announce(enc, cap.as_mut(), desk, settings, &mut size)?;
            (prev, last) = (None, None);
            vid.reset();
        };
    }
    let mut clip = crate::clip::Clip::new();
    let mut inbox = crate::files::Inbox::default();
    let mut terms = crate::pty::Terms::default();
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
        send(enc, &ToClient::Notice(format!("The person at that computer lets you use: {}.", crate::grant::words(allowed)))).map_err(io_err)?;
    }
    // Rights a request was turned away for since they last changed: said once each.
    let mut told_off = 0u8;
    let mut pending = String::new();
    let mut in_flight = 0u32;
    // Sizes of the frames in flight, oldest first, matched to acknowledgements in order.
    let mut flight_sizes: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut next_id = 1u32;
    let mut due = Instant::now();
    loop {
        // Everything queued in the last turn goes out before waiting.
        enc.flush().map_err(io_err)?;
        // A download in progress keeps the loop turning; otherwise wait for input or the next frame.
        let longest = if outbox.busy() { 1 } else { 8 };
        let wait = due
            .saturating_duration_since(Instant::now())
            .clamp(Duration::from_millis(1), Duration::from_millis(longest));
        let mut msgs = Vec::new();
        match rx.recv_timeout(wait) {
            Ok(m) => msgs.push(m),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => {
                if let Ok(why) = why.try_recv() {
                    let _ = send(enc, &ToClient::Error(why));
                    let _ = enc.flush();
                }
                return Ok(());
            }
        }
        msgs.extend(rx.try_iter());
        msgs.splice(0..0, queued.drain(..));
        for k in skipped.try_iter() {
            let text = format!("The SSH host's sshremote is older than this one and passed over a request it does not know (message {k}); update it there.");
            send(enc, &ToClient::Notice(text)).map_err(io_err)?;
        }
        for m in msgs {
            // Text waiting to be pasted goes in before any later input, so the order is kept.
            let typed_text = matches!(m, ToAgent::Key { keysym, mods: 0 } if xk::is_unicode(keysym));
            if !typed_text && matches!(m, ToAgent::Key { .. } | ToAgent::KeyState { .. } | ToAgent::Button { .. }) {
                paste(&mut pending, &mut clip, cap.as_mut());
            }
            // What the person at the screen does not allow is turned away, and said once.
            if let Some(need) = crate::grant::needed(&m)
                && allowed & need == 0
            {
                let why = format!("The person at that computer does not let you use {}.", crate::grant::words(need));
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
            // Files are the signed-in person's business: an agent run as SYSTEM acts as them, and
            // with nobody signed in there are none to hand out.
            #[cfg(windows)]
            let _as_user = match &m {
                ToAgent::ListDir { .. } | ToAgent::Fetch { .. } | ToAgent::FileStart { .. } | ToAgent::FileEnd { .. } | ToAgent::FileOp { .. } => {
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
                ToAgent::Settings(s) => {
                    let t = target(desk, &s);
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
                    settings = Some(s);
                }
                ToAgent::Ack(_) => {
                    in_flight = in_flight.saturating_sub(1);
                    flight_sizes.pop_front();
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
                        Some(Err(e)) => ToClient::Notice(format!("Screenshot: {e}")),
                        None => ToClient::Notice("Screenshot: no picture yet".into()),
                    };
                    send(enc, &m).map_err(io_err)?;
                }
                ToAgent::Ping(t) => send(enc, &ToClient::Pong(t)).map_err(io_err)?,
                // GNOME types only what its keyboard layout has; other text (Korean, emoji...) is pasted.
                ToAgent::Key { keysym, mods: 0 } if xk::is_unicode(keysym) && cap.name() == "gnome" => {
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
                ToAgent::FileStart { id, size, dir, name } => {
                    let note = if dir.is_empty() {
                        inbox.start(id, size, &name)
                    } else {
                        inbox.start_in(id, size, std::path::Path::new(&dir), &name)
                    };
                    if let Some(m) = note {
                        send(enc, &m.into_client()).map_err(io_err)?;
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
                        send(enc, &ToClient::Notice(format!("Chat: {e}"))).map_err(io_err)?;
                    }
                }
                ToAgent::Audio(on) => {
                    sound = None;
                    if on {
                        match crate::audio::capture() {
                            Ok(c) => sound = Some(c),
                            Err(e) => send(enc, &ToClient::Notice(format!("Sound: {e}"))).map_err(io_err)?,
                        }
                    }
                }
                ToAgent::SelectMonitor(i) => {
                    if let Err(e) = cap.select(i as usize) {
                        report(Err(e));
                        continue;
                    }
                    if let Ok(text) = cap.share(crate::capture::Share::Screen) {
                        send(enc, &ToClient::Sharing { kind: 0, text }).map_err(io_err)?;
                    }
                    resized!();
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
                    let tree = ui.state(cap.origin(), cap.desktop_size(), &scope, text, max.max(1) as usize);
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
                                for (k, down) in
                                    [(xk::CONTROL_L, true), ('a' as u32, true), ('a' as u32, false), (xk::CONTROL_L, false)]
                                {
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
                        Err(e) => ToClient::UiDone { ok: false, text: e },
                    };
                    send(enc, &reply).map_err(io_err)?;
                }
                ToAgent::PickOnRemote => match cap.pick() {
                    Ok(text) => {
                        send(enc, &ToClient::Sharing { kind: 0, text: "the whole screen".into() }).map_err(io_err)?;
                        resized!();
                        send(enc, &ToClient::Notice(text)).map_err(io_err)?;
                    }
                    Err(e) => send(enc, &ToClient::Notice(format!("Sharing: {e}"))).map_err(io_err)?,
                },
                ToAgent::InstallHelper => {
                    let reply = match cap.install_helper() {
                        Ok(text) => ToClient::HelperInstalled { ok: true, text },
                        Err(e) => ToClient::HelperInstalled { ok: false, text: format!("Could not install the helper: {e}") },
                    };
                    send(enc, &reply).map_err(io_err)?;
                }
                ToAgent::Share(scope) => {
                    let kind = scope.to_wire().0;
                    match cap.share(scope) {
                        Ok(text) => {
                            send(enc, &ToClient::Sharing { kind, text }).map_err(io_err)?;
                            ui.pin(kind);
                            resized!();
                        }
                        Err(e) => send(enc, &ToClient::Notice(format!("Sharing: {e}"))).map_err(io_err)?,
                    }
                }
            }
        }
        paste(&mut pending, &mut clip, cap.as_mut());
        for text in chat.replies() {
            // The person at the screen ended it.
            if text == crate::chat::END || text == crate::chat::DENY {
                return Err("The person at that computer's screen ended the connection.".into());
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
                }
                if taken & crate::grant::TERMINAL != 0 {
                    terms = crate::pty::Terms::default();
                }
                if taken & crate::grant::FILES != 0 {
                    (inbox, outbox) = (Default::default(), Default::default());
                }
                send(enc, &ToClient::Allowed(allowed)).map_err(io_err)?;
                send(enc, &ToClient::Notice(format!("The person at that computer now lets you use: {}.", crate::grant::words(allowed)))).map_err(io_err)?;
                continue;
            }
            send(enc, &ToClient::Chat(text)).map_err(io_err)?;
        }
        if let Some(c) = &sound {
            for p in c.packets.try_iter() {
                send(enc, &ToClient::Audio(p)).map_err(io_err)?;
            }
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
        if let Some(text) = clip.poll()
            && allowed & crate::grant::CLIPBOARD != 0
        {
            send(enc, &ToClient::Clipboard(text)).map_err(io_err)?;
        }
        // The choice made in the desktop's sharing dialog, even while the picture is paused.
        match cap.poll_share() {
            Some(Ok((kind, text))) => {
                send(enc, &ToClient::Sharing { kind, text }).map_err(io_err)?;
                ui.pin(kind);
            }
            Some(Err(e)) => send(enc, &ToClient::Notice(format!("Sharing: {e}"))).map_err(io_err)?,
            None => {}
        }
        let (Some(s), Some(red)) = (settings, reducer.as_ref()) else {
            continue;
        };
        let now = Instant::now();
        // Grab only when a frame may go out: some backends copy the whole screen per grab.
        let light = in_flight < IN_FLIGHT_SMALL && flight_sizes.iter().sum::<usize>() < SMALL_BYTES;
        if (in_flight >= IN_FLIGHT && !light) || now < due || (paused && !one_shot) {
            continue;
        }
        if let Some(img) = cap.grab()? {
            last = Some(img);
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
        if !fresh && prev.is_some() {
            continue;
        }
        fresh = false;
        let plane = red.reduce(img);
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
            continue;
        }
        waited = false;
        one_shot = false;
        let mut moves = Vec::new();
        if let Some(m) = found
            && let Some(mut moved) = prev.take()
        {
            // The previous screen is replaced below anyway, so it is moved in place, not copied.
            scroll::apply(&mut moved.data, moved.w, moved.mode.bpp(), &m);
            rects = diff(Some(&moved), &plane);
            moves.push(m);
        }
        let mut video_part = None;
        if matches!(plane.mode, Mode::Color256 | Mode::True) {
            if let video::Event::Ended(r) = vid.observe(plane.w, plane.h, &rects, !moves.is_empty(), &|r| crate::color::rich(img, r)) {
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
                if rects.iter().any(|r| video::overlaps(r, &reg)) {
                    let rate =
                        (reg.w * reg.h) as f64 * s.fps as f64 * if plane.mode == Mode::True { 0.08 } else { 0.04 };
                    let data = vid.encode(img, s.fps, rate.clamp(150e3, 6e6) as u32)?;
                    let (x, y, w, h) = (reg.x as u16, reg.y as u16, reg.w as u16, reg.h as u16);
                    video_part = Some(Tile { x, y, w, h, data });
                }
                rects = rects.iter().flat_map(|r| video::subtract(r, &reg)).collect();
            }
        }
        prev = Some(plane);
        if rects.is_empty() && moves.is_empty() && video_part.is_none() {
            due = now + Duration::from_millis(1000 / s.fps.max(1) as u64);
            continue;
        }
        let plane = prev.as_ref().unwrap();
        let (palette, data) = if plane.mode == Mode::True && s.formats & crate::proto::FORMAT_PALETTE != 0 {
            let (fresh, changed, data) = colours.pack(plane, &rects, whole);
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

/// Pastes text the backend cannot type, through the clipboard and Ctrl+V.
fn paste(pending: &mut String, clip: &mut crate::clip::Clip, cap: &mut dyn capture::Capture) {
    if pending.is_empty() {
        return;
    }
    if !clip.stage_paste(std::mem::take(pending)) {
        report(Err("text outside the keyboard layout needs the clipboard, which is unavailable".into()));
        return;
    }
    for (k, down) in [(xk::CONTROL_L, true), ('v' as u32, true), ('v' as u32, false), (xk::CONTROL_L, false)] {
        report(cap.input(Input::KeyState { keysym: k, down }));
    }
    // Applications fetch pasted text asynchronously; let it land before the next keys.
    thread::sleep(Duration::from_millis(150));
}

fn monitors(cap: &dyn capture::Capture) -> ToClient {
    let list = cap
        .monitors()
        .into_iter()
        .map(|m| crate::proto::MonitorInfo { name: m.name, w: m.w as u16, h: m.h as u16, primary: m.primary })
        .collect();
    ToClient::Monitors { list, current: cap.current() as u8 }
}

/// Tells the viewer the new picture size and fits the capture to it.
fn announce(
    enc: &mut Wire,
    cap: &mut dyn capture::Capture,
    desk: (usize, usize),
    settings: Option<Settings>,
    size: &mut (usize, usize),
) -> Result<(), String> {
    let io_err = |e: io::Error| e.to_string();
    let hello = ToClient::Hello { w: desk.0 as u16, h: desk.1 as u16, backend: cap.name().into() };
    send(enc, &hello).map_err(io_err)?;
    send(enc, &monitors(cap)).map_err(io_err)?;
    if let Some(s) = settings {
        *size = target(desk, &s);
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
        assert_eq!(target((1920, 1080), &view(1280, 720, 100)), (1280, 720));
        assert_eq!(target((1920, 1080), &view(1366, 768, 100)), (1365, 768));
        assert_eq!(target((1920, 1080), &view(1920, 1200, 100)), (1920, 1080));
        assert_eq!(target((1920, 1080), &view(4000, 3000, 100)), (1920, 1080), "never enlarged");
        assert_eq!(target((1920, 1080), &view(1280, 720, 50)), (640, 360));
        assert_eq!(target((1920, 1080), &view(1280, 720, 255)), (1280, 720), "a scale above 100 is 100");
    }
}
