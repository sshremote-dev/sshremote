//! Text chat between the viewer and the person at the remote screen. The agent
//! has no window of its own, so it starts `sshremote --chat-window` on the remote
//! desktop and talks to it over stdin/stdout, one escaped line per message.
//! Nothing is stored.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[cfg(feature = "gui")]
use eframe::egui::{self, Color32, RichText};

/// One message per line: backslashes and newlines escaped.
pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (c, c == '\\') {
            (_, true) => match it.next() {
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            },
            (c, false) => out.push(c),
        }
    }
    out
}

/// What the chat window writes when the person at the screen ends the connection (a typed
/// message cannot hold a NUL).
pub const END: &str = "\u{0}end";
/// What it writes, with the rights as a number, when the person lets the viewer in or changes
/// what it may do (`grant`).
pub const ALLOW: &str = "\u{0}allow ";
/// What it writes when the person turns the viewer away.
pub const DENY: &str = "\u{0}deny";
/// A voice call, as the viewer is told where it is (`ToClient::CallState`).
pub const CALL_ENDED: u8 = 0;
pub const CALL_RINGING: u8 = 1;
pub const CALL_TALKING: u8 = 2;
pub const CALL_DECLINED: u8 = 3;
/// What the agent tells the window of a call, and what the window answers.
pub const RING: &str = "\u{0}ring";
pub const HUNG_UP: &str = "\u{0}hungup";
pub const ANSWER: &str = "\u{0}answer";
pub const DECLINE: &str = "\u{0}decline";
pub const HANG_UP: &str = "\u{0}hangup";
/// What a viewer is told while the person at the screen is asked.
pub const WAITING: &str = n!("Waiting for the person at that computer to let you in\u{2026}");

/// The chat window's program: one started here, or one started as the person signed in at the
/// screen (an agent run as SYSTEM).
enum Proc {
    Here(Child),
    #[cfg(windows)]
    AsUser(crate::console_user::Process),
}

impl Proc {
    fn alive(&mut self) -> bool {
        match self {
            Proc::Here(c) => matches!(c.try_wait(), Ok(None)),
            #[cfg(windows)]
            Proc::AsUser(p) => p.alive(),
        }
    }

    /// Ends it once it has had a moment to go by itself: its input ending is its cue, and a
    /// notification it shows goes with it then (one left behind would stay on the screen, ahead of
    /// the next connection's).
    fn end(&mut self, input: Box<dyn Write + Send>) {
        drop(input);
        match self {
            Proc::Here(c) => {
                let until = std::time::Instant::now() + Duration::from_millis(500);
                while std::time::Instant::now() < until && matches!(c.try_wait(), Ok(None)) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = c.kill();
                let _ = c.wait();
            }
            #[cfg(windows)]
            Proc::AsUser(p) => p.kill(),
        }
    }
}

/// Agent side: the chat window on the remote desktop, started on the first message, or at once
/// for a viewer who came by the computer's ID, to say so.
#[derive(Default)]
pub struct Window {
    child: Option<(Proc, Box<dyn Write + Send>)>,
    replies: Option<Receiver<String>>,
    /// It says that someone is connected by this computer's ID, with a button to end that and
    /// what they may do (`grant`), and asks first when `grant::ASK` is in it.
    by_id: Option<u8>,
}

impl Window {
    fn alive(&mut self) -> bool {
        self.child.as_mut().is_some_and(|(c, _)| c.alive())
    }

    /// Tells the person at the screen that someone connected by this computer's ID: the window
    /// opens at once, with a button to end the connection and what they may do (`grant`).
    pub fn notice(&mut self, allowed: u8) -> Result<(), String> {
        self.by_id = Some(allowed & crate::grant::ALL);
        self.open()
    }

    /// Asks the person at the screen to let in someone who came by this computer's ID, offering
    /// `allowed`: what they let the viewer do, or None when they turned it away, closed the
    /// window or did not answer within `limit`.
    pub fn ask(&mut self, allowed: u8, limit: Duration) -> Result<Option<u8>, String> {
        self.by_id = Some((allowed & crate::grant::ALL) | crate::grant::ASK);
        self.open()?;
        let until = std::time::Instant::now() + limit;
        let rx = self.replies.as_ref().ok_or("no window")?;
        let answer = loop {
            match rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
                Ok(m) => {
                    if let Some(a) = m.strip_prefix(ALLOW).and_then(|b| b.parse::<u8>().ok()) {
                        break Some(a & crate::grant::ALL);
                    }
                    if m == DENY || m == END {
                        break None;
                    }
                }
                Err(_) => break None,
            }
        };
        if answer.is_none()
            && let Some((mut c, input)) = self.child.take()
        {
            c.end(input);
        }
        self.by_id = answer;
        Ok(answer)
    }

    /// The viewer calls: the window opens if it was closed and rings, for the person at the
    /// screen to answer or decline.
    pub fn ring(&mut self) -> Result<(), String> {
        if !self.alive() {
            self.open()?;
        }
        self.say(RING)
    }

    /// The viewer hung up.
    pub fn hung_up(&mut self) {
        if self.alive() {
            let _ = self.say(HUNG_UP);
        }
    }

    fn say(&mut self, line: &str) -> Result<(), String> {
        let (_, stdin) = self.child.as_mut().ok_or("no chat window")?;
        writeln!(stdin, "{line}").and_then(|_| stdin.flush()).map_err(|e| e.to_string())
    }

    /// Shows a message from the viewer, opening the window if the user closed it.
    pub fn show(&mut self, text: &str) -> Result<(), String> {
        if !self.alive() {
            self.open()?;
        }
        let (_, stdin) = self.child.as_mut().ok_or("no chat window")?;
        writeln!(stdin, "{}", escape(text)).and_then(|_| stdin.flush()).map_err(|e| e.to_string())
    }

    fn open(&mut self) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let name = if self.by_id.is_some() { n!("Someone connected by this computer's ID").to_string() } else { viewer_name() };
        let by_id = self.by_id.map(|g| format!(" --by-id --grant {g}{}", crate::agent::code_arg())).unwrap_or_default();
        // An agent run as SYSTEM shows it as the person signed in at the screen, never as SYSTEM.
        #[cfg(windows)]
        match crate::console_user::acting() {
            crate::console_user::Acting::User(u) => {
                let line = format!("\"{}\" --chat-window \"{}\"{by_id}", exe.display(), name);
                let (proc, stdin, stdout) = crate::console_user::spawn_piped(&u, &line)?;
                self.listen(Box::new(stdout));
                self.child = Some((Proc::AsUser(proc), Box::new(stdin)));
                return Ok(());
            }
            crate::console_user::Acting::Nobody => return Err(n!("nobody is signed in at that computer's screen").into()),
            crate::console_user::Acting::Myself => {}
        }
        let mut cmd = Command::new(exe);
        cmd.arg("--chat-window").arg(&name).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        cmd.args(by_id.split_whitespace());
        #[cfg(target_os = "linux")]
        {
            // An agent started over ssh has no display variables; the desktop session uses the
            // defaults: Wayland's only where it is there (an X11 session has none).
            crate::capture::ensure_session_env();
            if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
                cmd.env("WAYLAND_DISPLAY", "wayland-0");
            }
            if std::env::var_os("DISPLAY").is_none() {
                cmd.env("DISPLAY", ":0");
            }
        }
        // The chat window itself, without a console behind it.
        crate::sys::quiet(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("chat window: {e}"))?;
        crate::sys::tie(&child);
        let stdin = child.stdin.take().ok_or("chat window stdin")?;
        let stdout = child.stdout.take().ok_or("chat window stdout")?;
        self.listen(Box::new(stdout));
        self.child = Some((Proc::Here(child), Box::new(stdin)));
        Ok(())
    }

    /// What the window says comes in on a thread of its own.
    fn listen(&mut self, stdout: Box<dyn std::io::Read + Send>) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // What the window says for itself (it starts with a NUL) comes as is; a message is unescaped.
                let m = if line.starts_with('\u{0}') { line } else { unescape(&line) };
                if tx.send(m).is_err() {
                    break;
                }
            }
        });
        self.replies = Some(rx);
    }

    /// Replies typed at the remote screen since the last call.
    pub fn replies(&mut self) -> Vec<String> {
        self.replies.as_ref().map(|r| r.try_iter().collect()).unwrap_or_default()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if let Some((mut c, input)) = self.child.take() {
            c.end(input);
        }
    }
}

/// Who is viewing, as the agent sees it: the address ssh came from.
fn viewer_name() -> String {
    std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|c| c.split_whitespace().next().map(|ip| t!("Viewer ({})", ip)))
        .unwrap_or_else(|| n!("Viewer").into())
}

/// `sshremote --chat-window NAME`: the small window shown on the remote desktop.
#[cfg(feature = "gui")]
/// `code`: the viewer asked without a password, and was shown this code.
pub fn run_window(name: String, by_id: bool, grant: Option<u8>, code: Option<String>) -> Result<(), String> {
    crate::lang::start();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(unescape(&line)).is_err() {
                break;
            }
        }
        // The agent went away with the session.
        let _ = tx.send(String::from("\u{0}"));
    });
    let grant = grant.unwrap_or(crate::grant::ALL);
    let asking = by_id && grant & crate::grant::ASK != 0;
    // Only telling that someone came by the ID: it must not take the keyboard from what the
    // viewer is typing into. Asking takes it: nobody types there before the answer.
    let quiet = by_id && !asking;
    // Lines that came while no window was open, for the window to show first.
    #[cfg_attr(not(all(target_os = "linux", feature = "remote")), allow(unused_mut))]
    let mut early = Vec::new();
    #[cfg(all(target_os = "linux", feature = "remote"))]
    if quiet && wayland_here() {
        // A Wayland window cannot open without the keyboard: a notification says it instead, and
        // the window opens when something comes for it or the person asks for it.
        if let Some((note, acts)) = note::Note::show(crate::lang::tr(CONNECTED_TITLE), t!("They see this screen and can use it, with this computer's ID and password.")) {
            loop {
                match acts.try_recv() {
                    Ok(note::Act::End) => {
                        tell(END);
                        note.close();
                        return Ok(());
                    }
                    Ok(note::Act::Open) => break,
                    _ => {}
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(m) if m != "\u{0}" => {
                        early.push(m);
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    // The viewer went away before anything needed the window.
                    _ => {
                        note.close();
                        return Ok(());
                    }
                }
            }
            note.close();
        }
    }
    let title = if asking { t!("SSH Remote: someone wants to connect") } else if by_id { crate::lang::tr(CONNECTED_TITLE) } else { t!("SSH Remote chat") };
    // Only telling that someone is connected, with nothing said yet: the header and the line to
    // write in, out of the way; it grows when there is something to read. Under Wayland it opens
    // only when there is (a notification says the rest).
    #[cfg(target_os = "linux")]
    let compact = quiet && early.is_empty() && !wayland_here();
    #[cfg(not(target_os = "linux"))]
    let compact = quiet;
    let size = [400.0, if asking { 600.0 } else if compact { COMPACT_HEIGHT } else { FULL_HEIGHT }];
    let mut native = crate::gui::native_options(title, size);
    native.viewport = native.viewport.with_min_inner_size([320.0, COMPACT_HEIGHT.min(240.0)]);
    native.viewport = native.viewport.with_always_on_top();
    // A window manager that tiles would give it a share of the screen, rearranging the person's
    // windows; a dialog it floats over them.
    #[cfg(target_os = "linux")]
    if !wayland_here() && x11_tiles() {
        native.viewport = native.viewport.with_window_type(egui::X11WindowType::Dialog);
    }
    // Windows and macOS open it without the keyboard when asked; X11 is told below.
    if quiet {
        native.viewport = native.viewport.with_active(false);
        // A Mac's program comes in front of the others as it starts, its keyboard with it.
        #[cfg(target_os = "macos")]
        {
            native.event_loop_builder = Some(Box::new(|b| {
                use winit::platform::macos::EventLoopBuilderExtMacOS;
                b.with_activate_ignoring_other_apps(false);
            }));
        }
    }
    #[cfg(feature = "drive")]
    crate::gui::drive::start();
    eframe::run_native(
        "sshremote chat",
        native,
        Box::new(move |cc| {
            crate::gui::style(&cc.egui_ctx);
            // What gives the keyboard back if the window takes it, started with its first frame.
            #[cfg(target_os = "linux")]
            let give_back = if quiet { x11_no_focus(cc).or_else(wayland_give_back) } else { None };
            // Sway and Hyprland would tile it among the person's windows: it floats there instead.
            #[cfg(target_os = "linux")]
            let give_back = match (wayland_float(size, asking), give_back) {
                (Some(float), Some(back)) => Some(Box::new(move || {
                    float();
                    back();
                }) as Box<dyn FnOnce() + Send>),
                (float, back) => float.or(back),
            };
            #[cfg(not(target_os = "linux"))]
            let give_back = None;
            let ctx = cc.egui_ctx.clone();
            let (wake_tx, wake_rx) = mpsc::channel::<String>();
            for m in early {
                let _ = wake_tx.send(m);
            }
            std::thread::spawn(move || {
                while let Ok(m) = rx.recv() {
                    if wake_tx.send(m).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            Ok(Box::new(ChatApp {
                incoming: wake_rx,
                lines: Vec::new(),
                input: String::new(),
                ended: None,
                name,
                by_id,
                allowed: grant & crate::grant::ALL,
                asking: asking.then(std::time::Instant::now),
                rights_open: false,
                call: None,
                minimized_once: false,
                code,
                give_back,
                placed: false,
                compact,
                cornered: false,
            }))
        }),
    )
    .map_err(|e| e.to_string())
    .or_else(|e| if crate::gui::gpu::retry(&e) { Ok(()) } else { Err(e) })
}

#[cfg(feature = "gui")]
struct ChatApp {
    incoming: Receiver<String>,
    /// (from the viewer, text)
    lines: Vec<Line>,
    input: String,
    /// When the viewer went away; the window closes itself a little later.
    ended: Option<std::time::Instant>,
    /// Who is viewing: "Viewer (address)".
    name: String,
    /// Someone came by this computer's ID: the window says so and can end it.
    by_id: bool,
    /// What they may do (`grant`), changed here for this session.
    allowed: u8,
    /// Since when the window asks whether to let them in at all.
    asking: Option<std::time::Instant>,
    /// The list of what they may do is unfolded.
    rights_open: bool,
    /// A call: ringing (false) or answered (true).
    call: Option<bool>,
    /// Closed once while they were connected: it went to the taskbar, and says so.
    minimized_once: bool,
    /// They asked without a password: the code their screen shows, to compare before letting them in.
    code: Option<String>,
    /// Started when the window first draws (it shows right after): gives the keyboard back to
    /// the window that had it, where the desktop gives it to this one anyway.
    give_back: Option<Box<dyn FnOnce() + Send>>,
    /// Moved to the screen's corner (once its size is known).
    placed: bool,
    /// Only the header and the line to write in, until there is more to show.
    compact: bool,
    /// It was put in the bottom right corner, so it grows upwards.
    cornered: bool,
}

/// The window's height with the chat, and with nothing said yet.
const FULL_HEIGHT: f32 = 460.0;
const COMPACT_HEIGHT: f32 = 260.0;

/// Tests: the window shown on the remote screen, as one connected by ID sees it (asking first,
/// or with what they may do unfolded), drawn into `ui`.
#[cfg(all(test, feature = "gui"))]
pub fn host_window_for_test(asking: bool, rights_open: bool, compact: bool) -> impl FnMut(&mut egui::Ui) {
    let (_tx, rx) = mpsc::channel();
    let mut app = ChatApp {
        incoming: rx,
        lines: if compact { Vec::new() } else { vec![(true, "Hello, I am looking at the printer settings.".into(), now())] },
        input: String::new(),
        ended: None,
        name: "Someone connected by this computer's ID".into(),
        by_id: true,
        allowed: crate::grant::ALL & !crate::grant::POWER,
        asking: asking.then(std::time::Instant::now),
        rights_open,
        call: None,
        minimized_once: false,
        code: asking.then(|| "482913".to_string()),
        give_back: None,
        placed: true,
        compact,
        cornered: false,
    };
    move |ui| app.draw(ui)
}

/// This window would open on Wayland: its display is named and there (a name alone, left by
/// someone else, may point at nothing on an X11 desktop).
#[cfg(all(feature = "gui", target_os = "linux"))]
fn wayland_here() -> bool {
    let Some(name) = std::env::var_os("WAYLAND_DISPLAY").filter(|d| !d.is_empty()) else { return false };
    let path = std::path::PathBuf::from(&name);
    let path = if path.is_absolute() { path } else { std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_default().join(path) };
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Whether the X11 window manager is one that tiles windows (i3, bspwm, awesome...), by the name
/// it gives itself (`_NET_SUPPORTING_WM_CHECK`).
#[cfg(all(feature = "gui", target_os = "linux"))]
fn x11_tiles() -> bool {
    use std::os::raw::{c_int, c_uchar, c_ulong};
    use x11_dl::xlib::{AnyPropertyType, Xlib};
    const TILING: [&str; 9] = ["i3", "bspwm", "awesome", "xmonad", "qtile", "herbstluftwm", "spectrwm", "leftwm", "dwm"];
    let Ok(xlib) = Xlib::open() else { return false };
    // SAFETY: a connection of its own, closed here; properties read as X lays them out (32-bit
    // ones as longs).
    unsafe {
        let d = (xlib.XOpenDisplay)(std::ptr::null());
        if d.is_null() {
            return false;
        }
        let property = |window: c_ulong, name: &std::ffi::CStr| -> Option<Vec<u8>> {
            let atom = (xlib.XInternAtom)(d, name.as_ptr(), 0);
            let (mut kind, mut format, mut n, mut after, mut data): (c_ulong, c_int, c_ulong, c_ulong, *mut c_uchar) = (0, 0, 0, 0, std::ptr::null_mut());
            let ok = (xlib.XGetWindowProperty)(d, window, atom, 0, 64, 0, AnyPropertyType as c_ulong, &mut kind, &mut format, &mut n, &mut after, &mut data) == 0;
            if !ok || data.is_null() {
                return None;
            }
            let size = match format {
                8 => n as usize,
                32 => n as usize * std::mem::size_of::<c_ulong>(),
                _ => 0,
            };
            let bytes = std::slice::from_raw_parts(data, size).to_vec();
            (xlib.XFree)(data.cast());
            Some(bytes)
        };
        let root = (xlib.XDefaultRootWindow)(d);
        let name = property(root, c"_NET_SUPPORTING_WM_CHECK")
            .and_then(|w| Some(c_ulong::from_ne_bytes(w.get(..std::mem::size_of::<c_ulong>())?.try_into().ok()?)))
            .and_then(|wm| property(wm, c"_NET_WM_NAME"))
            .map(|n| String::from_utf8_lossy(&n).to_lowercase())
            .unwrap_or_default();
        (xlib.XCloseDisplay)(d);
        TILING.iter().any(|t| name.starts_with(t))
    }
}

/// Asks the X11 window manager not to give the keyboard to the window when it shows
/// (`_NET_WM_USER_TIME` 0, before eframe maps it after its first frame), and gives it back to the
/// window that had it where the window manager gives it anyway (i3): what the viewer types into
/// goes on getting it.
#[cfg(all(feature = "gui", target_os = "linux"))]
fn x11_no_focus(cc: &eframe::CreationContext<'_>) -> Option<Box<dyn FnOnce() + Send>> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::os::raw::{c_int, c_long, c_uchar, c_ulong};
    use x11_dl::xlib::{ClientMessage, PropModeReplace, SubstructureNotifyMask, SubstructureRedirectMask, XA_CARDINAL, XA_WINDOW, XClientMessageEvent, XEvent, Xlib};
    let window = match cc.window_handle().map(|w| w.as_raw()) {
        Ok(RawWindowHandle::Xlib(w)) => w.window,
        Ok(RawWindowHandle::Xcb(w)) => u32::from(w.window) as c_ulong,
        _ => return None,
    };
    let Ok(xlib) = Xlib::open() else { return None };
    // SAFETY: connections of its own, closed here; properties read and written as X lays them out.
    unsafe {
        let d = (xlib.XOpenDisplay)(std::ptr::null());
        if d.is_null() {
            return None;
        }
        let user_time = (xlib.XInternAtom)(d, c"_NET_WM_USER_TIME".as_ptr(), 0);
        let zero: c_long = 0;
        (xlib.XChangeProperty)(d, window, user_time, XA_CARDINAL, 32, PropModeReplace, (&zero as *const c_long).cast(), 1);
        (xlib.XFlush)(d);
        let active_atom = (xlib.XInternAtom)(d, c"_NET_ACTIVE_WINDOW".as_ptr(), 0);
        let root = (xlib.XDefaultRootWindow)(d);
        // The window with the keyboard now.
        unsafe fn active(xlib: &Xlib, d: *mut x11_dl::xlib::Display, root: c_ulong, atom: c_ulong) -> c_ulong {
            let (mut kind, mut format, mut n, mut after, mut data): (c_ulong, c_int, c_ulong, c_ulong, *mut c_uchar) = (0, 0, 0, 0, std::ptr::null_mut());
            // SAFETY: one WINDOW asked for, read only when X says it gave one.
            unsafe {
                let ok = (xlib.XGetWindowProperty)(d, root, atom, 0, 1, 0, XA_WINDOW, &mut kind, &mut format, &mut n, &mut after, &mut data) == 0;
                let w = if ok && n == 1 && !data.is_null() { *(data as *const c_ulong) } else { 0 };
                if !data.is_null() {
                    (xlib.XFree)(data.cast());
                }
                w
            }
        }
        let before = active(&xlib, d, root, active_atom);
        (xlib.XCloseDisplay)(d);
        if before == 0 || before == window {
            return None;
        }
        // Watched while the window comes up: once it has the keyboard, the other window gets it back.
        Some(Box::new(move || {
            std::thread::spawn(move || {
                let d = (xlib.XOpenDisplay)(std::ptr::null());
                if d.is_null() {
                    return;
                }
                for _ in 0..60 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if active(&xlib, d, root, active_atom) == window {
                        let mut ev: XClientMessageEvent = std::mem::zeroed();
                        (ev.type_, ev.window, ev.message_type, ev.format) = (ClientMessage, before, active_atom, 32);
                        // A request as a taskbar makes it (source 2), at the current time.
                        ev.data.set_long(0, 2);
                        let mut e = XEvent { client_message: ev };
                        (xlib.XSendEvent)(d, root, 0, SubstructureRedirectMask | SubstructureNotifyMask, &mut e);
                        (xlib.XFlush)(d);
                        break;
                    }
                }
                (xlib.XCloseDisplay)(d);
            });
        }))
    }
}

/// Where the compositor tiles new windows and can be told otherwise (Sway's swaymsg, Hyprland's
/// hyprctl): once it shows this window, it floats it at `size` in its monitor's bottom right
/// corner, as the window is placed elsewhere (a question in the middle), rather than taking a
/// share of the screen and moving the person's windows.
#[cfg(all(feature = "gui", target_os = "linux"))]
fn wayland_float(size: [f32; 2], middle: bool) -> Option<Box<dyn FnOnce() + Send>> {
    use serde_json::Value;
    use std::process::Command;
    if !wayland_here() {
        return None;
    }
    let run = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_default();
    let newest = |dir: &std::path::Path, keep: &dyn Fn(&str) -> bool| {
        std::fs::read_dir(dir).ok()?.flatten().filter(|e| keep(&e.file_name().to_string_lossy())).max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()).map(|e| e.path())
    };
    let json = |cmd: &mut Command| cmd.output().ok().and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok());
    let me = u64::from(std::process::id());
    let (w, h) = (size[0].round() as i64, size[1].round() as i64);
    let corner = move |rect: (i64, i64, i64, i64)| {
        if middle { (rect.0 + (rect.2 - w).max(0) / 2, rect.1 + (rect.3 - h).max(0) / 2) } else { (rect.0 + (rect.2 - w - 16).max(0), rect.1 + (rect.3 - h - 16).max(0)) }
    };
    let wait = |find: &dyn Fn() -> bool| {
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if find() {
                return;
            }
        }
    };
    let rect = |v: &Value| Some((v.get("x")?.as_i64()?, v.get("y")?.as_i64()?, v.get("width")?.as_i64()?, v.get("height")?.as_i64()?));
    if let Some(sock) = std::env::var_os("SWAYSOCK").map(std::path::PathBuf::from).or_else(|| newest(&run, &|n| n.starts_with("sway-ipc.") && n.ends_with(".sock"))) {
        // This window's id and the rectangle of the output it is on.
        fn find(v: &Value, me: u64, output: Option<(i64, i64, i64, i64)>, rect: &dyn Fn(&Value) -> Option<(i64, i64, i64, i64)>) -> Option<(u64, (i64, i64, i64, i64))> {
            let output = if v.get("type").and_then(Value::as_str) == Some("output") { v.get("rect").and_then(rect) } else { output };
            if v.get("pid").and_then(Value::as_u64) == Some(me) {
                return Some((v.get("id")?.as_u64()?, output?));
            }
            ["nodes", "floating_nodes"].iter().filter_map(|k| v.get(*k)?.as_array()).flatten().find_map(|n| find(n, me, output, rect))
        }
        return Some(Box::new(move || {
            std::thread::spawn(move || {
                let found = std::cell::Cell::new(None);
                wait(&|| {
                    found.set(json(Command::new("swaymsg").arg("-s").arg(&sock).args(["-t", "get_tree", "-r"])).and_then(|t| find(&t, me, None, &rect)));
                    found.get().is_some()
                });
                if let Some((id, out)) = found.get() {
                    let (x, y) = corner(out);
                    let _ = Command::new("swaymsg").arg("-s").arg(&sock).arg(format!("[con_id={id}] floating enable, resize set width {w} px height {h} px, move absolute position {x} px {y} px")).output();
                }
            });
        }));
    }
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().filter(|s| !s.is_empty()).or_else(|| Some(newest(&run.join("hypr"), &|_| true)?.file_name()?.to_string_lossy().into_owned()))?;
    Some(Box::new(move || {
        std::thread::spawn(move || {
            let hyprctl = |args: &[&str]| {
                let mut c = Command::new("hyprctl");
                c.env("HYPRLAND_INSTANCE_SIGNATURE", &sig).args(args);
                c
            };
            let found = std::cell::RefCell::new(None);
            wait(&|| {
                *found.borrow_mut() = json(&mut hyprctl(&["-j", "clients"])).and_then(|all| {
                    let c = all.as_array()?.iter().find(|c| c.get("pid").and_then(Value::as_u64) == Some(me))?.clone();
                    Some((c.get("address")?.as_str()?.to_string(), c.get("monitor")?.as_i64()?))
                });
                found.borrow().is_some()
            });
            let Some((address, monitor)) = found.into_inner() else { return };
            // The monitor's area in the layout's (logical) pixels.
            let out = json(&mut hyprctl(&["-j", "monitors"])).and_then(|all| {
                let m = all.as_array()?.iter().find(|m| m.get("id").and_then(Value::as_i64) == Some(monitor))?.clone();
                let scale = m.get("scale").and_then(Value::as_f64).filter(|s| *s > 0.0).unwrap_or(1.0);
                let (x, y, mw, mh) = rect(&m)?;
                Some((x, y, (mw as f64 / scale).round() as i64, (mh as f64 / scale).round() as i64))
            });
            let Some(out) = out else { return };
            let (x, y) = corner(out);
            let batch = format!("dispatch setfloating address:{address} ; dispatch resizewindowpixel exact {w} {h},address:{address} ; dispatch movewindowpixel exact {x} {y},address:{address}");
            let _ = hyprctl(&["--batch", &batch]).output();
        });
    }))
}

/// Where the compositor takes the window in front and has a way to be told otherwise (Sway's
/// swaymsg, Hyprland's hyprctl): the window that had the keyboard gets it back once this one took it.
#[cfg(all(feature = "gui", target_os = "linux"))]
fn wayland_give_back() -> Option<Box<dyn FnOnce() + Send>> {
    use std::process::Command;
    let run = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_default();
    let newest = |dir: &std::path::Path, keep: &dyn Fn(&str) -> bool| {
        std::fs::read_dir(dir).ok()?.flatten().filter(|e| keep(&e.file_name().to_string_lossy())).max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()).map(|e| e.path())
    };
    let json = |cmd: &mut Command| cmd.output().ok().and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok());
    let me = std::process::id() as u64;
    // Sway: the tree's focused window, by its id, and whose process it is.
    let sway = std::env::var_os("SWAYSOCK").map(std::path::PathBuf::from).or_else(|| newest(&run, &|n| n.starts_with("sway-ipc.") && n.ends_with(".sock")));
    if let Some(sock) = sway {
        let focused = move || {
            fn find(v: &serde_json::Value) -> Option<(u64, u64)> {
                if v.get("focused").and_then(serde_json::Value::as_bool) == Some(true) {
                    return Some((v.get("id")?.as_u64()?, v.get("pid").and_then(serde_json::Value::as_u64).unwrap_or(0)));
                }
                ["nodes", "floating_nodes"].iter().filter_map(|k| v.get(*k)?.as_array()).flatten().find_map(find)
            }
            json(Command::new("swaymsg").arg("-s").arg(&sock).args(["-t", "get_tree", "-r"])).as_ref().and_then(find).map(|f| (f, sock.clone()))
        };
        let ((before, _), sock) = focused().filter(|((_, pid), _)| *pid != me)?;
        return Some(Box::new(move || {
            std::thread::spawn(move || {
                for _ in 0..40 {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    if focused().is_some_and(|((_, pid), _)| pid == me) {
                        let _ = Command::new("swaymsg").arg("-s").arg(&sock).arg(format!("[con_id={before}] focus")).output();
                        break;
                    }
                }
            });
        }));
    }
    // Hyprland: the active window, by its address, and whose process it is.
    let dir = newest(&run.join("hypr"), &|_| true)?;
    {
        let sig = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let active = move || {
            let v = json(Command::new("hyprctl").env("HYPRLAND_INSTANCE_SIGNATURE", &sig).args(["activewindow", "-j"]))?;
            Some((v.get("address")?.as_str()?.to_string(), v.get("pid").and_then(serde_json::Value::as_u64).unwrap_or(0), sig.clone()))
        };
        let (before, _, sig) = active().filter(|(_, pid, _)| *pid != me)?;
        Some(Box::new(move || {
            std::thread::spawn(move || {
                for _ in 0..40 {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    if active().is_some_and(|(_, pid, _)| pid == me) {
                        let _ = Command::new("hyprctl").env("HYPRLAND_INSTANCE_SIGNATURE", &sig).args(["dispatch", "focuswindow", &format!("address:{before}")]).output();
                        break;
                    }
                }
            });
        }))
    }
}

/// A desktop notification (org.freedesktop.Notifications) with buttons, for what a Wayland
/// window cannot say without taking the keyboard.
#[cfg(all(feature = "gui", target_os = "linux", feature = "remote"))]
mod note {
    use std::collections::HashMap;
    use std::sync::mpsc;

    use futures_util::StreamExt;
    use zbus::{Connection, Proxy, Variant};

    pub enum Act {
        End,
        Open,
    }

    pub struct Note {
        proxy: Proxy<'static>,
        id: u32,
    }

    const DEST: &str = "org.freedesktop.Notifications";

    impl Note {
        /// Shown, with what is pressed on it; None where no notifications with buttons are shown.
        pub fn show(summary: &str, body: &str) -> Option<(Note, mpsc::Receiver<Act>)> {
            zbus::block_on(async {
                let conn = Connection::session().await.ok()?;
                // Only a service already running: starting one by its name may start a waiter for
                // a desktop that is not there (Plasma's, under Sway or Hyprland), which never answers.
                let dbus = zbus::fdo::DBusProxy::new(&conn).await.ok()?;
                if !dbus.name_has_owner(zbus::names::BusName::try_from(DEST).ok()?).await.ok()? {
                    return None;
                }
                let proxy = Proxy::new(&conn, DEST, "/org/freedesktop/Notifications", DEST).await.ok()?;
                let caps: Vec<String> = proxy.call("GetCapabilities", &()).await.ok()?;
                if !caps.iter().any(|c| c == "actions") {
                    return None;
                }
                // Heard before it is shown, so no press is missed.
                let mut signals = proxy.receive_all_signals().await.ok()?;
                let actions = ["default", t!("Show"), "show", t!("Show"), "end", t!("End the connection")];
                let mut hints: HashMap<&str, Variant> = HashMap::new();
                // Kept until dismissed or the connection ends.
                hints.insert("urgency", Variant::U8(2));
                hints.insert("resident", Variant::Bool(true));
                let id: u32 = proxy.call("Notify", &("SSH Remote", 0u32, "", summary, body, &actions[..], hints, 0i32)).await.ok()?;
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    zbus::block_on(async {
                        while let Some(m) = signals.next().await {
                            let member = m.header().member().map(|n| n.to_string()).unwrap_or_default();
                            if member != "ActionInvoked" {
                                continue;
                            }
                            let Ok((which, key)) = m.body().deserialize::<(u32, String)>() else { continue };
                            if which == id && tx.send(if key == "end" { Act::End } else { Act::Open }).is_err() {
                                return;
                            }
                        }
                    })
                });
                Some((Note { proxy, id }, rx))
            })
        }

        pub fn close(&self) {
            let _ = zbus::block_on(self.proxy.call_noreply("CloseNotification", &(self.id,)));
        }
    }
}

/// The window's title while someone is connected by this computer's ID: what the taskbar shows.
#[cfg(feature = "gui")]
const CONNECTED_TITLE: &str = n!("SSH Remote: someone is connected");

/// Says something of the window's own to the agent.
#[cfg(feature = "gui")]
fn tell(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

/// The rights as switches; true when one changed.
#[cfg(feature = "gui")]
fn rights(ui: &mut egui::Ui, allowed: &mut u8) -> bool {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let mut changed = false;
        for (bit, label, _) in crate::grant::RIGHTS {
            let mut on = *allowed & bit != 0;
            if ui.checkbox(&mut on, crate::lang::tr(label)).changed() {
                *allowed = if on { *allowed | bit } else { *allowed & !bit };
                changed = true;
            }
        }
        changed
    })
    .inner
}

#[cfg(feature = "gui")]
impl ChatApp {
    /// Whether to let someone in who came by this computer's ID, and what they may do.
    fn ask(&mut self, ui: &mut egui::Ui, since: std::time::Instant) {
        let ctx = ui.ctx().clone();
        let left = 60u64.saturating_sub(since.elapsed().as_secs());
        let mut answer = None;
        // The answer stays at the bottom; what is asked scrolls above it on a small screen.
        let fill = ui.visuals().window_fill;
        egui::Panel::bottom("ask-answer").frame(egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(20, 12))).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add(crate::gui::kit::secondary(t!("Turn away"))).clicked() {
                    answer = Some(false);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(crate::gui::theme::primary(t!("Let them in")).min_size(egui::Vec2::new(0.0, crate::gui::kit::CONTROL_H))).clicked() {
                        answer = Some(true);
                    }
                });
            });
            ui.add_space(6.0);
            ui.label(RichText::new(t!("Turned away by itself in {} s", left)).small().weak());
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(fill).inner_margin(egui::Margin::same(20))).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::gui::ic::USER).size(30.0).color(crate::gui::brand()));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(t!("Someone wants to connect")).strong().size(16.0));
                        let how = if self.code.is_some() { t!("They have this computer's ID, not its password.") } else { t!("They have this computer's ID and its password.") };
                        ui.label(RichText::new(how).weak());
                    });
                });
                // Asked without a password: the code both screens show tells whether it is really them.
                if let Some(code) = &self.code {
                    ui.add_space(10.0);
                    egui::Frame::new().fill(crate::gui::brand().gamma_multiply(0.12)).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t!("Code")).weak());
                            ui.label(RichText::new(format!("{} {}", &code[..3], &code[3..])).monospace().size(22.0).strong());
                        });
                        ui.add(egui::Label::new(RichText::new(t!("Their screen shows a code too. Let them in only if it is this one: someone else in the middle would show another.")).small()).wrap());
                    });
                }
                ui.add_space(14.0);
                ui.label(RichText::new(t!("If you let them in, they may:")).strong());
                ui.add_space(4.0);
                rights(ui, &mut self.allowed);
                ui.add(egui::Label::new(RichText::new(t!("They always see the screen. You can change these and end the connection at any time; while they are connected this window stays in the taskbar.")).small().weak()).wrap());
            });
        });
        if left == 0 {
            answer = Some(false);
        }
        match answer {
            Some(true) => {
                tell(&format!("{ALLOW}{}", self.allowed));
                self.asking = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(crate::lang::tr(CONNECTED_TITLE).into()));
            }
            Some(false) => {
                tell(DENY);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            None => ctx.request_repaint_after(Duration::from_millis(500)),
        }
    }
}

#[cfg(feature = "gui")]
impl eframe::App for ChatApp {
    #[cfg(feature = "drive")]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        crate::gui::drive::hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        crate::gui::gpu::frame();
        if let Some(f) = self.give_back.take() {
            f();
        }
        self.draw(ui);
    }
}

#[cfg(feature = "gui")]
impl ChatApp {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        #[cfg(feature = "drive")]
        crate::gui::drive::end_of_frame(&ctx);
        for m in self.incoming.try_iter() {
            if m == "\u{0}" {
                self.ended = Some(std::time::Instant::now());
            } else if m == RING {
                self.call = Some(false);
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else if m == HUNG_UP {
                self.call = None;
            } else {
                self.lines.push((true, m, now()));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        crate::gui::chrome::behaviour(&ctx);
        let title = if self.asking.is_some() || self.by_id { "SSH Remote" } else { t!("SSH Remote chat") };
        let closed = crate::gui::chrome::title_bar(ui, |ui| {
            ui.label(RichText::new(title).size(13.0));
        })
        .close;
        let asked_to_close = ctx.input(|i| i.viewport().close_requested());
        if closed || asked_to_close {
            if self.asking.is_some() {
                // Closing the question turns the viewer away.
                tell(DENY);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if self.ended.is_none() {
                // While they are connected the window only goes to the taskbar: it is where the
                // connection is ended and what they may do is changed, so it must stay at hand.
                if asked_to_close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                self.minimized_once = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        // Out of the way in the bottom right corner, as the desktop's own notices are: the person
        // there, and whoever connected, go on seeing the middle of the screen. A question stays
        // where the desktop puts it, to be answered; a desktop that places windows itself
        // (Wayland) takes no position.
        if !self.placed && self.asking.is_none() {
            let (monitor, outer) = ctx.input(|i| (i.viewport().monitor_size, i.viewport().outer_rect));
            if let (Some(m), Some(o)) = (monitor, outer) {
                self.placed = true;
                // Above the taskbar or the Dock. The monitor's own origin is not known here, so
                // only a window on the primary one (at the origin) is moved.
                let below = if cfg!(target_os = "macos") { 96.0 } else { 64.0 };
                if egui::Rect::from_min_size(egui::Pos2::ZERO, m).contains(o.min) {
                    let at = egui::pos2((m.x - o.width() - 16.0).max(0.0), (m.y - o.height() - below).max(0.0));
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
                    self.cornered = true;
                }
            }
        }
        // Something to read or to change: the window grows to show it (upwards in the corner).
        if self.compact && self.ended.is_none() && (!self.lines.is_empty() || self.rights_open || self.call.is_some()) {
            self.compact = false;
            let grow = FULL_HEIGHT - COMPACT_HEIGHT;
            let outer = ctx.input(|i| i.viewport().outer_rect);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(ctx.input(|i| i.viewport().inner_rect.map_or(400.0, |r| r.width())), FULL_HEIGHT)));
            if let (true, Some(o)) = (self.cornered, outer) {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(o.min.x, (o.min.y - grow).max(0.0))));
            }
        }
        if let Some(since) = self.asking {
            if self.ended.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            self.ask(ui, since);
            return;
        }
        let card = ui.visuals().window_fill;
        let header = egui::Frame::new().fill(card).inner_margin(egui::Margin::symmetric(16, 10));
        egui::Panel::top("chat-header").frame(header).show_separator_line(!self.compact).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(34.0), egui::Sense::hover());
                let icon = egui::FontId::proportional(22.0);
                let fg = ui.visuals().strong_text_color();
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, crate::gui::ic::CHAT, icon, fg);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(crate::lang::tr_msg(&self.name)).strong().size(15.0));
                    let state = if self.ended.is_some() {
                        t!("the connection closed; this window closes shortly")
                    } else if self.by_id && self.code.is_some() {
                        t!("sees this screen and can use it: you let them in, without the password")
                    } else if self.by_id {
                        t!("sees this screen and can use it, with this computer's ID and password")
                    } else {
                        t!("viewing this screen over SSH Remote")
                    };
                    ui.label(RichText::new(state).small().weak());
                });
            });
            // A call: answered here, or not; hung up from here too.
            if let Some(talking) = self.call
                && self.ended.is_none()
            {
                ui.add_space(6.0);
                egui::Frame::new().fill(crate::gui::good().gamma_multiply(0.14)).corner_radius(egui::CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::gui::ic::PHONE).size(18.0).color(crate::gui::good()));
                        ui.label(RichText::new(if talking { t!("In a call") } else { t!("They are calling") }).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if talking {
                                let hang = egui::Button::new(RichText::new(format!("{}  {}", crate::gui::ic::PHONE_HANG_UP, t!("Hang up"))).color(Color32::WHITE)).fill(crate::gui::bad());
                                if ui.add(hang).clicked() {
                                    tell(HANG_UP);
                                    self.call = None;
                                }
                            } else {
                                if ui.add(crate::gui::kit::secondary(t!("Decline"))).clicked() {
                                    tell(DECLINE);
                                    self.call = None;
                                }
                                let answer = egui::Button::new(RichText::new(format!("{}  {}", crate::gui::ic::PHONE, t!("Answer"))).color(Color32::WHITE)).fill(crate::gui::good());
                                if ui.add(answer).clicked() {
                                    tell(ANSWER);
                                    self.call = Some(true);
                                }
                            }
                        });
                    });
                });
            }
            // The person at the screen can end it, and change what they may do, at any time.
            if self.by_id && self.ended.is_none() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let end = egui::Button::new(RichText::new(t!("End the connection")).color(Color32::WHITE).strong()).fill(crate::gui::bad());
                    if ui.add(end).on_hover_text(t!("End this connection now")).clicked() {
                        tell(END);
                        self.ended = Some(std::time::Instant::now());
                    }
                    let label = if self.rights_open { format!("{}  {}", crate::gui::ic::CHAT, t!("Back to the chat")) } else { format!("{}  {}", crate::gui::ic::SHIELD, t!("What they may do")) };
                    if ui.add(crate::gui::kit::ghost(label)).clicked() {
                        self.rights_open = !self.rights_open;
                    }
                });
                if !self.rights_open && self.allowed != crate::grant::ALL {
                    ui.label(RichText::new(t!("They may use: {}", crate::grant::shown(self.allowed))).small().weak());
                }
            }
            if self.minimized_once && self.ended.is_none() {
                ui.label(RichText::new(t!("Closing this window keeps it in the taskbar while they are connected.")).small().weak());
            }
        });
        let footer = egui::Frame::new().fill(card).inner_margin(egui::Margin::symmetric(12, 10));
        egui::Panel::bottom("chat-input").frame(footer).show_separator_line(!self.compact).show(ui, |ui| {
            if let Some(text) = input_row(ui, &mut self.input, self.ended.is_none()) {
                println!("{}", escape(&text));
                use std::io::Write;
                let _ = std::io::stdout().flush();
                self.lines.push((false, text, now()));
            }
        });
        // Compact, the header's colour fills what is between it and the line to write in.
        let body = egui::Frame::new().fill(if self.compact { card } else { ui.visuals().panel_fill }).inner_margin(egui::Margin::symmetric(14, 10));
        egui::CentralPanel::default().frame(body).show(ui, |ui| {
            if self.compact {
                return;
            }
            // What they may do takes the chat's place, scrolled when the window is small.
            if self.rights_open && self.by_id && self.ended.is_none() {
                egui::ScrollArea::vertical().id_salt("rights").auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(RichText::new(t!("What they may do")).strong());
                    ui.add_space(4.0);
                    if rights(ui, &mut self.allowed) {
                        tell(&format!("{ALLOW}{}", self.allowed));
                    }
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new(t!("They always see the screen. A change applies at once.")).small().weak()).wrap());
                });
                return;
            }
            let name = crate::lang::tr_msg(&self.name);
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| show_lines(ui, &self.lines, if self.by_id { t!("Viewer") } else { &name }, t!("You")));
        });
        if self.ended.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// Who is at the other end of `spec` (an ssh destination, an ~/.ssh/config alias, a
/// sshremote:// address, or this computer), as "user (address)": ssh's own view of an alias
/// gives its user and host name, and a host name is looked up.
pub fn peer_name(spec: &str) -> String {
    let me = || std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "user".into());
    let s = spec.trim();
    if s.is_empty() || ["local", "this computer", "test desktop"].contains(&s.to_lowercase().as_str()) {
        return t!("{} (this computer)", me());
    }
    let s = s.trim_start_matches("sshremote://");
    let (mut user, mut host) = match s.rsplit_once('@') {
        Some((u, h)) => (Some(u.to_string()), h.to_string()),
        None => (None, s.to_string()),
    };
    if !spec.starts_with("sshremote://") {
        if let Ok(out) = crate::sys::quiet(std::process::Command::new("ssh").arg("-G").arg("--").arg(s)).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(v) = line.strip_prefix("user ") {
                    user.get_or_insert_with(|| v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("hostname ") {
                    host = v.trim().to_string();
                }
            }
        }
    } else if let Some((h, _)) = host.rsplit_once(':').filter(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok()) {
        host = h.to_string();
    }
    let ip = if host.parse::<std::net::IpAddr>().is_ok() {
        host.clone()
    } else {
        use std::net::ToSocketAddrs;
        (host.as_str(), 22).to_socket_addrs().ok().and_then(|mut a| a.next()).map_or(host.clone(), |a| a.ip().to_string())
    };
    match user {
        Some(u) if !u.is_empty() => format!("{u} ({ip})"),
        _ => ip,
    }
}

/// `peer_name` worked out in the background (it may ask ssh and the name server), shown as
/// "Remote" until then.
#[derive(Clone)]
pub struct Peer(std::sync::Arc<std::sync::Mutex<String>>);

impl Peer {
    pub fn new(spec: &str) -> Peer {
        let p = Peer(std::sync::Arc::new(std::sync::Mutex::new(t!("Remote").into())));
        let (q, spec) = (p.clone(), spec.to_string());
        std::thread::spawn(move || {
            let name = peer_name(&spec);
            *q.0.lock().unwrap() = name;
        });
        p
    }

    pub fn get(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

/// A message: whether the other side wrote it, its text, and when (seconds since 1970; 0 unknown).
pub type Line = (bool, String, u64);

pub use crate::sys::now_secs as now;

/// When a message was written: the time today, the date and time before.
pub(crate) fn when(at: u64) -> String {
    let (then, today) = (crate::files::local_time(at), crate::files::local_time(now()));
    match (then.split_once(' '), today.split_once(' ')) {
        (Some((d, t)), Some((td, _))) if d == td => t.to_string(),
        _ => then,
    }
}

/// How many earlier messages are kept with each host.
const HISTORY_KEEP: usize = 300;
/// The longest message kept or shown; a longer one is cut there (a chat is not for files).
pub const MAX_MESSAGE: usize = 16 << 10;

/// `text` cut to `MAX_MESSAGE` bytes, at a character's edge, with an ellipsis when cut.
pub fn trimmed(text: &str) -> String {
    if text.len() <= MAX_MESSAGE {
        return text.to_string();
    }
    let mut end = MAX_MESSAGE;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &text[..end])
}

/// Where the conversation with `host` is kept between sessions.
fn history_path(host: &str) -> Option<std::path::PathBuf> {
    let safe: String =
        host.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.@".contains(c) { c } else { '_' }).collect();
    crate::hosts::config_dir().map(|d| d.join("chat").join(format!("{safe}.jsonl")))
}

/// The messages exchanged with `host` before, oldest first: (theirs, text).
pub fn load_history(host: &str) -> Vec<Line> {
    history_path(host).map(|p| load_from(&p)).unwrap_or_default()
}

/// Adds a message to the conversation kept with `host`.
pub fn append_history(host: &str, theirs: bool, text: &str) {
    if let Some(p) = history_path(host) {
        append_to(&p, theirs, text, now(), 512 << 10);
    }
}

/// The last messages kept in `path`, one JSON pair a line.
fn load_from(path: &std::path::Path) -> Vec<Line> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let lines: Vec<Line> = text
        .lines()
        .filter_map(|l| {
            serde_json::from_str::<Line>(l).ok().or_else(|| serde_json::from_str::<(bool, String)>(l).ok().map(|(t, s)| (t, s, 0)))
        })
        .collect();
    lines[lines.len().saturating_sub(HISTORY_KEEP)..].to_vec()
}

/// Adds a message to `path`; past `limit` bytes the file is written again with the last ones that
/// fill half of it, so it is not rewritten for every message after.
fn append_to(path: &std::path::Path, theirs: bool, text: &str, at: u64, limit: u64) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = trimmed(text);
    if std::fs::metadata(path).is_ok_and(|m| m.len() > limit) {
        let mut kept = load_from(path);
        kept.push((theirs, text, at));
        let lines: Vec<String> = kept.iter().map(|m| serde_json::to_string(m).expect("json") + "\n").collect();
        let (mut size, mut from) = (0, lines.len());
        while from > 0 && size + lines[from - 1].len() as u64 <= limit / 2 {
            from -= 1;
            size += lines[from].len() as u64;
        }
        // A crash midway leaves the old file whole.
        let _ = crate::sys::write_whole(path, lines[from..].concat().as_bytes(), false, false);
    } else if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = f.write_all((serde_json::to_string(&(theirs, text, at)).expect("json") + "\n").as_bytes());
    }
}

/// Forgets the conversation kept with `host`.
pub fn clear_history(host: &str) {
    if let Some(p) = history_path(host) {
        let _ = std::fs::remove_file(p);
    }
}

/// The conversation as message bubbles, as in a messaging app: the other side's on the left with
/// their name over the first of a run, one's own on the right in the accent colour.
#[cfg(feature = "gui")]
pub fn show_lines(ui: &mut egui::Ui, lines: &[Line], them: &str, me: &str) {
    show_lines_since(ui, lines, 0, them, me);
}

/// One message as a row of text, as in Slack: the first of a run under the sender's initial and
/// name, the next ones under it; no bubble, only a faint tint on the row under the pointer.
#[cfg(feature = "gui")]
fn message(ui: &mut egui::Ui, theirs: bool, who: &str, text: &str, at: u64, first: bool, width: f32) {
    const AVATAR: f32 = 28.0;
    const GAP: f32 = 10.0;
    if first {
        ui.add_space(10.0);
    }
    let behind = ui.painter().add(egui::Shape::Noop);
    let row = ui
        .horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(GAP, 2.0);
            if first {
                // Who wrote it, as a plain line icon: the person there, or this side.
                let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(AVATAR), egui::Sense::hover());
                let glyph = if theirs { crate::gui::ic::USER } else { crate::gui::ic::DESKTOP };
                let fg = ui.visuals().weak_text_color();
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(18.0), fg);
            } else {
                // Under the first message's text: the avatar's width and the gap after it.
                ui.add_space(AVATAR + GAP);
            }
            ui.vertical(|ui| {
                ui.set_max_width((width - AVATAR - GAP - 8.0).max(80.0));
                if first {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(RichText::new(who).strong());
                        if at > 0 {
                            ui.label(RichText::new(when(at)).small().weak());
                        }
                    });
                }
                ui.add(egui::Label::new(RichText::new(text).size(14.0)).wrap().selectable(true));
            });
        })
        .response;
    if ui.rect_contains_pointer(row.rect) {
        let tint = if crate::gui::theme::dark() { Color32::from_white_alpha(8) } else { Color32::from_black_alpha(8) };
        let r = row.rect.expand2(egui::vec2(6.0, 2.0));
        ui.painter().set(behind, egui::Shape::rect_filled(r, crate::gui::theme::R_SM, tint));
    }
}

/// Like `show_lines`, with the first `earlier` messages from before this session set apart.
#[cfg(feature = "gui")]
pub fn show_lines_since(ui: &mut egui::Ui, lines: &[Line], earlier: usize, them: &str, me: &str) {
    if lines.is_empty() {
        ui.add_space(ui.available_height() * 0.3);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(crate::gui::ic::CHAT).size(34.0).weak());
            ui.label(RichText::new(t!("No messages yet")).strong());
            ui.label(RichText::new(t!("Messages with {} appear here.", them)).small().weak());
        });
        return;
    }
    let width = ui.available_width();
    // Messages of one run sit close; a new sender starts a little further down (see `message`).
    ui.spacing_mut().item_spacing.y = 4.0;
    for (i, (theirs, text, at)) in lines.iter().enumerate() {
        let divider = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(4.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(text).small().weak()));
            ui.add_space(2.0);
        };
        if i == 0 && earlier > 0 {
            divider(ui, t!("Earlier"));
        }
        if i == earlier && earlier > 0 {
            divider(ui, t!("This session"));
        }
        let first = i == 0 || i == earlier || lines[i - 1].0 != *theirs;
        message(ui, *theirs, if *theirs { them } else { me }, text, *at, first, width);
    }
}

/// A message field with a send button inside it; Enter sends. Returns the text when sent.
#[cfg(feature = "gui")]
pub fn input_row(ui: &mut egui::Ui, input: &mut String, enabled: bool) -> Option<String> {
    let mut sent = None;
    let field = ui.visuals().extreme_bg_color;
    let line = ui.visuals().widgets.noninteractive.bg_stroke;
    egui::Frame::new()
        .fill(field)
        .stroke(line)
        .corner_radius(crate::gui::theme::R_MD)
        .inner_margin(egui::Margin { left: 14, right: 4, top: 4, bottom: 4 })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let edit = ui.add_enabled(
                    enabled,
                    egui::TextEdit::singleline(input)
                        .hint_text(t!("Message"))
                        .frame(egui::Frame::NONE)
                        .desired_width(ui.available_width() - 40.0)
                        .min_size(egui::Vec2::new(0.0, 30.0)),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ready = enabled && !input.trim().is_empty();
                let send = egui::Button::new(RichText::new(crate::gui::ic::SEND).size(16.0))
                    .frame_when_inactive(false)
                    .corner_radius(crate::gui::theme::R_SM)
                    .min_size(egui::Vec2::splat(32.0));
                if (ui.add_enabled(ready, send).on_hover_text(t!("Send (Enter)")).clicked() || enter) && ready {
                    sent = Some(std::mem::take(input).trim().to_string());
                    edit.request_focus();
                }
            });
        });
    sent
}

#[cfg(test)]
mod tests {
    /// What the window writes reaches the agent: messages unescaped, and the end of the
    /// connection as itself, which no typed message can be.
    #[test]
    fn the_window_can_end_the_connection() {
        let mut w = super::Window::default();
        let said = format!("{}\n{}\n", super::escape("two\nlines"), super::END);
        w.listen(Box::new(std::io::Cursor::new(said.into_bytes())));
        let mut got = Vec::new();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while got.len() < 2 && std::time::Instant::now() < until {
            got.extend(w.replies());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got, ["two\nlines".to_string(), super::END.to_string()]);
    }

    /// A huge message is cut where a chat stops being one, and many keep the file small: it is
    /// written again with half its room, not rewritten for every message after.
    #[test]
    fn huge_messages_and_long_histories_stay_small() {
        let cut = super::trimmed(&"한".repeat(10_000));
        assert!(cut.len() <= super::MAX_MESSAGE + 3 && cut.ends_with('\u{2026}'));
        let path = std::env::temp_dir().join(format!("sshremote-chat-big-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let limit = 64 << 10;
        for i in 0..100 {
            super::append_to(&path, i % 2 == 0, &"x".repeat(40_000), i, limit);
            assert!(std::fs::metadata(&path).unwrap().len() <= limit + 20_000, "at {i}");
        }
        assert!(!super::load_from(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// The conversation kept with a host: in order, with any text, trimmed to the last ones.
    #[test]
    fn history_round_trip() {
        let path = std::env::temp_dir().join(format!("sshremote-chat-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        super::append_to(&path, true, "hello\nsecond line", 100, 1 << 20);
        super::append_to(&path, false, "tab\tand \"quotes\" \u{d55c}\u{ae00}", 200, 1 << 20);
        // A line kept before messages had a time still reads.
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap() + "[true,\"older\"]\n").unwrap();
        assert_eq!(
            super::load_from(&path),
            [
                (true, "hello\nsecond line".to_string(), 100),
                (false, "tab\tand \"quotes\" \u{d55c}\u{ae00}".to_string(), 200),
                (true, "older".to_string(), 0)
            ]
        );
        for k in 0..400 {
            super::append_to(&path, k % 2 == 0, &format!("m{k}"), k, 2_000);
        }
        let kept = super::load_from(&path);
        assert!(kept.len() <= super::HISTORY_KEEP);
        assert_eq!(kept.last().unwrap().1, "m399");
        let _ = std::fs::remove_file(&path);
    }

    use super::*;

    #[test]
    fn lines_survive_escaping() {
        for s in ["hi", "two\nlines", "back\\slash\\n literal", "\u{C548}\u{B155}\u{D558}\u{C138}\u{C694}"] {
            assert_eq!(unescape(&escape(s)), s);
            assert!(!escape(s).contains('\n'));
        }
    }
}
