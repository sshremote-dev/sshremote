//! GNOME Wayland capture and input through Mutter's RemoteDesktop/ScreenCast
//! D-Bus API, which works from an SSH login without a consent dialog.
//! Session flow follows sshdesk (MIT).

use std::collections::HashMap;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use futures_util::StreamExt;
use zbus::{Connection, OwnedObjectPath, OwnedVariant, Proxy, Variant};

use super::region::{self, R};
use super::{Capture, Image, Input, Monitor, NEEDS_HELPER, Pixels, Share, WindowInfo};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

// Input goes to Mutter without waiting for its reply: a round trip per pointer move would hold up
// the next event and the next frame.
macro_rules! notify {
    ($s:expr, $method:literal, $body:expr) => {
        zbus::block_on($s.session.call_noreply($method, $body)).map_err(|e| format!("GNOME {}: {e}", $method))
    };
}

const RD: &str = "org.gnome.Mutter.RemoteDesktop";
/// The sshremote helper extension, which lists windows (GNOME Shell keeps its own list to its portal).
const HELPER: &str = "io.github.thinkbigx.SshRemote.Windows";
const HELPER_PATH: &str = "/io/github/thinkbigx/SshRemote/Windows";
const HELPER_UUID: &str = "sshremote-helper@thinkbigx.github.io";

/// id, title, program, x, y, width, height (stage coordinates), hidden (minimized or on another
/// workspace; older helpers tell only minimized), shareable, focused, owner id.
type HelperWin = (u64, String, String, i32, i32, i32, i32, bool, bool, bool, u64);

/// What a stream records.
#[derive(Clone, Copy, PartialEq)]
enum Src {
    Monitor,
    /// The focused window (without the helper, which would tell where it is to cut it out).
    Focused,
    /// Whatever was chosen in GNOME's screen-sharing dialog on the remote screen.
    Portal,
}

/// Sharing one program: its windows' part of the monitor, the rest greyed out.
/// What is cut out of the monitor stream: a program's windows, or one window, each with the
/// pop-ups it opened (menus, dialogs, tooltips), which a window's own stream leaves out.
#[derive(Clone, PartialEq)]
enum Cut {
    App(String),
    Window(u64),
}

struct AppShare {
    of: Cut,
    area: R,
    shown: Vec<R>,
    focused: bool,
    inside: bool,
    checked: std::time::Instant,
}
const SC: &str = "org.gnome.Mutter.ScreenCast";

pub struct Gnome {
    monitors: Vec<(String, Monitor, (i32, i32, f64))>,
    current: usize,
    conn: Connection,
    session: Proxy<'static>,
    stream_path: String,
    node: u32,
    desk: (usize, usize),
    /// The recorded monitor's picture size, as its stream sends it (it follows rotation and
    /// resolution changes, which the mode list can lag behind).
    screen: (usize, usize),
    target: (usize, usize),
    /// The picture made smaller for the viewer, by area averaging.
    shrink: super::Shrink,
    pipe: Option<Pipe>,
    /// The stream read here, with the pointer beside its frames (`pipe` reads it otherwise).
    pw: Option<super::pw::Stream>,
    /// Its newest frame as it came, for pictures made again without a new one.
    clean: Option<super::pw::Frame>,
    /// A picture is due from `clean`: the viewer's size, or the pointer drawn in, changed.
    dirty: bool,
    /// The viewer draws the pointer; the number of its picture last sent.
    apart: bool,
    sent_shape: u64,
    /// Where the picture starts on the stream's frames.
    origin: (i32, i32),
    src: Src,
    app: Option<AppShare>,
    /// The stream chosen in GNOME's own sharing dialog, and its input session.
    portal: Option<super::portal::Portal>,
    /// The dialog is open on the remote screen; its answer arrives here.
    picking: Option<std::sync::mpsc::Receiver<Result<super::portal::Portal, String>>>,
    /// When the last frame came, and when the shared window was last checked for having closed.
    last_frame: std::time::Instant,
    checked_alive: std::time::Instant,
    /// The shared window or program closed and the whole screen came back; the viewer is told once.
    ended: bool,
    /// The helper's interface, made once without a property cache: it is asked several times a
    /// second while a program is shared.
    helper: std::sync::OnceLock<Proxy<'static>>,
    /// What was typed last, text (true) or keys, and when (`settle`).
    last_typed: Option<(bool, std::time::Instant)>,
    /// What the stream read calls as something comes (`Capture::set_waker`).
    wake: super::Waker,
}

fn err(what: &str) -> impl Fn(zbus::Error) -> String + '_ {
    move |e| format!("GNOME {what} failed: {e}")
}

/// An SSH login has no session bus variables; point them at the user's desktop session.
pub fn ensure_env() {
    let uid = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("Uid:")).and_then(|l| l.split_whitespace().nth(1)).map(str::to_owned)
        })
        .unwrap_or_else(|| "1000".into());
    let run = format!("/run/user/{uid}");
    // SAFETY: called at startup before other threads use the environment.
    unsafe {
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            std::env::set_var("XDG_RUNTIME_DIR", &run);
        }
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={run}/bus"));
        }
    }
    if std::env::var_os("DISPLAY").is_some() && std::env::var_os("XAUTHORITY").is_some() {
        return;
    }
    // Asked once: a session without an X authority would otherwise ask at every monitor or share.
    static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ASKED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // The desktop session's display (Xwayland is not always :0) and its X authority come from the user manager.
    let env = std::process::Command::new("systemctl").args(["--user", "show-environment"]).output();
    if let Ok(out) = env {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((k @ ("DISPLAY" | "WAYLAND_DISPLAY" | "XAUTHORITY" | "XDG_CURRENT_DESKTOP" | "XDG_SESSION_TYPE"), v)) = line.split_once('=')
                && std::env::var_os(k).is_none()
            {
                // SAFETY: as above.
                unsafe { std::env::set_var(k, v) };
            }
        }
    }
    // A desktop that tells the user manager nothing (some X11 ones): the session's own programs
    // know their display.
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        for (k, v) in session_env(&uid) {
            if std::env::var_os(&k).is_none() {
                // SAFETY: as above.
                unsafe { std::env::set_var(k, v) };
            }
        }
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        let run = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| run.clone().into());
        if let Some(socket) = wayland_socket(&run) {
            // SAFETY: as above.
            unsafe {
                std::env::set_var("WAYLAND_DISPLAY", socket);
                if std::env::var_os("XDG_SESSION_TYPE").is_none() {
                    std::env::set_var("XDG_SESSION_TYPE", "wayland");
                }
            }
        }
    }
}

/// The programs a desktop session runs (its shell, compositor, session manager or panel), each
/// with the desktop it tells of: only theirs tell the desktop's display; any other (a test's
/// Xvfb) could point somewhere else.
const DESKTOPS: &[(&str, &str)] = &[
    ("gnome-shell", "GNOME"), ("gnome-session-b", "GNOME"), ("xfce4-session", "XFCE"), ("xfwm4", "XFCE"), ("xfce4-panel", "XFCE"),
    ("plasmashell", "KDE"), ("kwin_x11", "KDE"), ("kwin_wayland", "KDE"), ("ksmserver", "KDE"), ("lxqt-session", "LXQt"),
    ("lxqt-panel", "LXQt"), ("openbox", ""), ("mate-session", "MATE"), ("marco", "MATE"), ("mate-panel", "MATE"),
    ("cinnamon", "X-Cinnamon"), ("cinnamon-sessio", "X-Cinnamon"), ("muffin", "X-Cinnamon"), ("budgie-wm", "Budgie:GNOME"),
    ("budgie-panel", "Budgie:GNOME"), ("i3", "i3"), ("sway", "sway"), ("swaybar", "sway"), ("swaybg", "sway"), ("Hyprland", "Hyprland"),
    ("cosmic-comp", "COSMIC"), ("cosmic-session", "COSMIC"), ("cosmic-panel", "COSMIC"), ("labwc", "labwc"), ("wayfire", "wayfire"),
    ("river", "river"), ("niri", "niri"), ("waybar", ""),
];

/// The display variables of this user's desktop session, as its own programs have them: the
/// newest one that knows its display, else the newest at all (a compositor's own start has no
/// display yet); the desktop's name from which program it is when none says it.
fn session_env(uid: &str) -> Vec<(String, String)> {
    const WANTED: [&str; 5] = ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"];
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    // (knows its display, pid): the best first.
    type Vars = Vec<(String, String)>;
    let mut best: Option<((bool, u64), Vars)> = None;
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u64>().ok()) else { continue };
        let Some(&(_, name)) = std::fs::read_to_string(e.path().join("comm")).ok().and_then(|c| DESKTOPS.iter().find(|d| d.0 == c.trim())) else {
            continue;
        };
        let mine = std::fs::read_to_string(e.path().join("status")).ok().is_some_and(|s| s.lines().any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some(uid)));        let Some(env) = mine.then(|| std::fs::read(e.path().join("environ")).ok()).flatten() else { continue };
        let vars: Vec<(String, String)> = env
            .split(|b| *b == 0)
            .filter_map(|kv| std::str::from_utf8(kv).ok()?.split_once('='))
            .filter(|(k, _)| WANTED.contains(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        // Another session's, whose displays are not where this one looks for them.
        let theirs = env.split(|b| *b == 0).find_map(|kv| kv.strip_prefix(b"XDG_RUNTIME_DIR="));
        if let (Some(t), Some(ours)) = (theirs, std::env::var_os("XDG_RUNTIME_DIR"))
            && t != ours.as_encoded_bytes()
        {
            continue;
        }
        let mut vars = vars;
        if !name.is_empty() && !vars.iter().any(|(k, _)| k == "XDG_CURRENT_DESKTOP") {
            vars.push(("XDG_CURRENT_DESKTOP".into(), name.into()));
        }
        let rank = (vars.iter().any(|(k, _)| k == "DISPLAY" || k == "WAYLAND_DISPLAY"), pid);
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, vars));
        }
    }
    best.map(|(_, v)| v).unwrap_or_default()
}

/// The newest Wayland display of this user's that answers (`wayland-1`): a compositor whose
/// display no program tells of.
fn wayland_socket(run: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::FileTypeExt;
    let mut found: Option<(std::time::SystemTime, String)> = None;
    for e in std::fs::read_dir(run).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("wayland-") || name.ends_with(".lock") || !e.file_type().is_ok_and(|t| t.is_socket()) {
            continue;
        }
        let when = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if found.as_ref().is_none_or(|(w, _)| when > *w) && std::os::unix::net::UnixStream::connect(e.path()).is_ok() {
            found = Some((when, name));
        }
    }
    found.map(|(_, n)| n)
}

/// A capture pipeline; it stops when it goes (one dropped while playing keeps its threads and its
/// PipeWire connection).
pub(super) struct Pipe {
    p: gst::Pipeline,
    pub(super) sink: gst_app::AppSink,
    src: gst::Element,
    woken: std::sync::OnceLock<()>,
}

impl Pipe {
    /// Has the agent woken as samples come, from now on; they stay to be pulled.
    pub(super) fn wake_with(&self, wake: &super::Waker) {
        self.woken.get_or_init(|| {
            let wake = wake.clone();
            let callbacks = gst_app::AppSinkCallbacks::builder()
                .new_sample(move |_| {
                    super::wake(&wake);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build();
            self.sink.set_callbacks(callbacks);
        });
    }

    /// The size the stream sends now, before any scaling here.
    fn source_size(&self) -> Option<(usize, usize)> {
        let caps = self.src.static_pad("src")?.current_caps()?;
        let s = caps.structure(0)?;
        Some((s.get::<i32>("width").ok()? as usize, s.get::<i32>("height").ok()? as usize))
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let _ = self.p.set_state(gst::State::Null);
    }
}

/// A stream's first frame, from the reader that will read the rest.
enum First {
    Own(super::pw::Stream, super::pw::Frame),
    Gst(Pipe, Image),
}

/// `source` is what pipewiresrc reads: "path=NODE", or "fd=FD path=NODE" for a portal stream.
pub(super) fn pipeline(source: &str, size: Option<(usize, usize)>) -> Result<Pipe, String> {
    let caps = match size {
        Some((w, h)) => format!(",width={w},height={h},pixel-aspect-ratio=1/1"),
        None => String::new(),
    };
    let desc = format!(
        "pipewiresrc name=src {source} do-timestamp=true keepalive-time=250 \
         ! queue leaky=downstream max-size-buffers=1 \
         ! videoconvert n-threads=2 ! videoscale method=1 add-borders=false n-threads=2 \
         ! video/x-raw,format=BGRx{caps} \
         ! appsink name=sink max-buffers=1 drop=true sync=false"
    );
    let p = gst::parse::launch(&desc)
        .map_err(|e| format!("GNOME capture pipeline: {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "GNOME capture pipeline is not a pipeline".to_string())?;
    let sink =
        p.by_name("sink").and_then(|s| s.downcast::<gst_app::AppSink>().ok()).ok_or("GNOME capture sink missing")?;
    let src = p.by_name("src").ok_or("GNOME capture source missing")?;
    let pipe = Pipe { p, sink, src, woken: std::sync::OnceLock::new() };
    pipe.p.set_state(gst::State::Playing).map_err(|e| format!("GNOME capture did not start: {e}"))?;
    Ok(pipe)
}

pub(super) fn to_image(sample: &gst::Sample) -> Result<Image, String> {
    let caps = sample.caps().ok_or("frame without caps")?;
    let info = gst_video::VideoInfo::from_caps(caps).map_err(|e| e.to_string())?;
    let buf = sample.buffer().ok_or("frame without buffer")?;
    let map = buf.map_readable().map_err(|e| e.to_string())?;
    Ok(Image {
        w: info.width() as usize,
        h: info.height() as usize,
        stride: info.stride()[0] as usize,
        data: map.as_slice().to_vec(),
    })
}

/// A window stream without crop information (older PipeWire): the frame minus pure black rows and
/// columns at its right and bottom. Never for a monitor, whose dark edges are part of the picture.
fn content_box(img: &Pixels) -> (usize, usize, usize, usize) {
    let black_px = |x: usize, y: usize| img.data[y * img.stride + x * 4..y * img.stride + x * 4 + 3] == [0, 0, 0];
    let mut h = img.h;
    while h > 16 && (0..img.w).step_by(4).all(|x| black_px(x, h - 1)) {
        h -= 1;
    }
    let mut w = img.w;
    while w > 16 && (0..h).step_by(4).all(|y| black_px(w - 1, y)) {
        w -= 1;
    }
    (0, 0, w, h)
}

/// What a recording starts with on the bus: its session, the stream it records, the monitors, and
/// the signal that names the stream's PipeWire node.
struct Begun {
    conn: Connection,
    session: Proxy<'static>,
    stream_path: String,
    list: Vec<(String, Monitor, (i32, i32, f64))>,
    current: usize,
    added: zbus::proxy::SignalStream<'static>,
}

/// Starts a Mutter session recording monitor `index` (or the primary one), or a window, with the
/// pointer beside the frames (`own`) or in them.
async fn begin(index: Option<usize>, src: Src, own: bool) -> Result<Begun, String> {
    let conn = Connection::session().await.map_err(err("session bus"))?;
    let rd = Proxy::new(&conn, RD, "/org/gnome/Mutter/RemoteDesktop", RD).await.map_err(err("RemoteDesktop"))?;
    let spath: OwnedObjectPath = rd.call("CreateSession", &()).await.map_err(err("CreateSession"))?;
    let session = Proxy::new(&conn, RD, spath, "org.gnome.Mutter.RemoteDesktop.Session").await.map_err(err("session"))?;
    let sid: String = session.get_property("SessionId").await.map_err(err("SessionId"))?;

    let sc = Proxy::new(&conn, SC, "/org/gnome/Mutter/ScreenCast", SC).await.map_err(err("ScreenCast"))?;
    let mut props: HashMap<&str, Variant> = HashMap::new();
    props.insert("remote-desktop-session-id", Variant::from(sid.as_str()));
    let cpath: OwnedObjectPath = sc.call("CreateSession", &(props,)).await.map_err(err("ScreenCast session"))?;
    let cast = Proxy::new(&conn, SC, cpath, "org.gnome.Mutter.ScreenCast.Session").await.map_err(err("cast"))?;
    let mut sprops: HashMap<&str, Variant> = HashMap::new();
    // The pointer as metadata beside the frames (2), for the viewer to draw or drawn in here;
    // in the frames (1) where GStreamer reads them.
    sprops.insert("cursor-mode", Variant::U32(if own { 2 } else { 1 }));
    let list = monitors(&conn).await;
    let current = index.unwrap_or(0).min(list.len().saturating_sub(1));
    let connector = list.get(current).map_or(String::new(), |m| m.0.clone());
    // Without a window id Mutter records the window that has the focus.
    let stream: OwnedObjectPath = match src {
        Src::Monitor => cast.call("RecordMonitor", &(connector.as_str(), sprops)).await.map_err(err("RecordMonitor"))?,
        Src::Focused => cast.call("RecordWindow", &(sprops,)).await.map_err(err("RecordWindow"))?,
        Src::Portal => unreachable!("portal streams come from pick"),
    };
    let stream_path = stream.as_str().to_owned();
    let sproxy = Proxy::new(&conn, SC, stream, "org.gnome.Mutter.ScreenCast.Stream").await.map_err(err("stream"))?;
    let added = sproxy.receive_signal("PipeWireStreamAdded").await.map_err(err("signal"))?;
    session.call::<_, _, ()>("Start", &()).await.map_err(err("Start"))?;
    // Mutter makes the session's keyboard and pointer at their first event, and drops what
    // comes while it does (a viewer's first keys): both are made now, by events that do
    // nothing (a key no key makes, a move by nothing).
    let _ = session.call_noreply("NotifyKeyboardKeysym", &(0u32, true)).await;
    let _ = session.call_noreply("NotifyPointerMotionRelative", &(0f64, 0f64)).await;
    Ok(Begun { conn, session, stream_path, list, current, added })
}

/// Connectors and current modes of the monitors, primary first, from Mutter's DisplayConfig.
async fn monitors(conn: &Connection) -> Vec<(String, Monitor, (i32, i32, f64))> {
    let Ok(dc) = Proxy::new(
        conn,
        "org.gnome.Mutter.DisplayConfig",
        "/org/gnome/Mutter/DisplayConfig",
        "org.gnome.Mutter.DisplayConfig",
    )
    .await
    else {
        return Vec::new();
    };
    let state: Result<crate::display::MutterState, _> = dc.call("GetCurrentState", &()).await;
    let Ok((_, mons, logical, _)) = state else { return Vec::new() };
    let primary: Vec<String> = logical.iter().filter(|l| l.4).flat_map(|l| l.5.iter().map(|m| m.0.clone())).collect();
    let place = |connector: &str| {
        logical.iter().find(|l| l.5.iter().any(|m| m.0 == connector)).map_or((0, 0, 1.0), |l| (l.0, l.1, l.2))
    };
    // Turned a quarter (transforms 1, 3 and their flipped 5, 7): the picture is the mode on its side.
    let sideways = |connector: &str| logical.iter().any(|l| l.3 % 2 == 1 && l.5.iter().any(|m| m.0 == connector));
    let mut out: Vec<(String, Monitor, (i32, i32, f64))> = mons
        .into_iter()
        .filter_map(|((connector, vendor, product, _), modes, _)| {
            let current =
                modes.iter().find(|m| m.6.get("is-current").is_some_and(|v| bool::try_from(v).unwrap_or(false)))?;
            let name = format!("{connector} {vendor} {product}").trim().to_string();
            let primary = primary.contains(&connector);
            let at = place(&connector);
            let (w, h) = if sideways(&connector) { (current.2, current.1) } else { (current.1, current.2) };
            Some((connector, Monitor { name, w: w as usize, h: h as usize, primary }, at))
        })
        .collect();
    out.sort_by_key(|m| !m.1.primary);
    out
}

impl Gnome {
    pub fn open() -> Result<Gnome, String> {
        Gnome::open_on(None, Src::Monitor)
    }

    /// Records the monitor at `index` in the monitor list (or the primary one), or a window:
    /// read here with the pointer beside the frames where PipeWire can be, by GStreamer otherwise.
    fn open_on(index: Option<usize>, src: Src) -> Result<Gnome, String> {
        let mut reader_failed = false;
        if super::pw::usable() {
            match Gnome::record(index, src, true, &mut reader_failed) {
                Err(_) if reader_failed => super::pw::give_up(),
                r => return r,
            }
        }
        Gnome::record(index, src, false, &mut reader_failed)
    }

    fn record(index: Option<usize>, src: Src, own: bool, reader_failed: &mut bool) -> Result<Gnome, String> {
        ensure_env();
        gst::init().map_err(|e| format!("GStreamer init: {e}"))?;
        let Begun { conn, session, stream_path, list, current, mut added } = zbus::block_on(begin(index, src, own))?;
        const NO_FRAME: &str = "no frame from the GNOME stream within 5 s (is a desktop session logged in and unlocked?)";
        let started = (|| {
            let msg = zbus::block_on(added.next()).ok_or("GNOME did not publish a PipeWire stream")?;
            let (node,): (u32,) = msg.body().deserialize().map_err(err("PipeWire node"))?;
            if own {
                let s = super::pw::Stream::open(None, node, std::time::Duration::from_secs(5)).map_err(|(e, ours)| {
                    *reader_failed = ours;
                    if ours { e } else { NO_FRAME.to_string() }
                })?;
                let first = s.take().0.ok_or(NO_FRAME)?;
                return Ok::<_, String>((node, First::Own(s, first)));
            }
            let pipe = pipeline(&format!("path={node}"), None)?;
            let first = pipe.sink.try_pull_sample(gst::ClockTime::from_seconds(5)).ok_or(NO_FRAME)?;
            let img = to_image(&first)?;
            Ok((node, First::Gst(pipe, img)))
        })();
        // A session that started and failed is stopped, not left recording.
        let (node, first) = started.inspect_err(|_| {
            let _ = zbus::block_on(session.call_noreply("Stop", &()));
        })?;
        let (desk, pipe, pw, clean) = match first {
            First::Own(s, f) => ((f.w, f.h), None, Some(s), Some(f)),
            First::Gst(p, img) => ((img.w, img.h), Some(p), None, None),
        };
        let screen = match (src, list.get(current)) {
            (Src::Monitor, _) | (_, None) => desk,
            (_, Some(m)) => (m.1.w, m.1.h),
        };
        Ok(Gnome {
            monitors: list,
            current,
            conn,
            session,
            stream_path,
            node,
            desk,
            screen,
            target: desk,
            shrink: super::Shrink::default(),
            pipe,
            pw,
            clean,
            dirty: true,
            apart: false,
            sent_shape: 0,
            origin: (0, 0),
            src,
            app: None,
            portal: None,
            picking: None,
            last_frame: std::time::Instant::now(),
            checked_alive: std::time::Instant::now(),
            ended: false,
            helper: std::sync::OnceLock::new(),
            last_typed: None,
            wake: Default::default(),
        })
    }

    /// Takes over from a new recording, keeping whether the viewer draws the pointer and who to
    /// wake (the agent gives that once).
    fn take_over(&mut self, next: Gnome) {
        let (apart, wake) = (self.apart, std::mem::take(&mut self.wake));
        *self = next;
        (self.apart, self.wake) = (apart && self.pw.is_some(), wake);
    }

    /// A new pipeline on the same stream, for when cutting or scaling changes; the old one is
    /// stopped first. A stream read here makes its next picture from its last frame.
    fn restart_pipe(&mut self) -> Result<(), String> {
        if self.pw.is_some() {
            (self.target, self.dirty) = (self.desk, true);
            return Ok(());
        }
        self.pipe = None;
        self.target = self.desk;
        self.set_target(self.desk.0, self.desk.1)
    }

    /// Frames at their own size, cut and scaled here: windows change size, programs are cut out.
    fn source(&self) -> String {
        match &self.portal {
            Some(p) => p.source(),
            None => format!("path={}", self.node),
        }
    }

    /// Starts reading the stream chosen in the portal dialog; its first frame gives the size. What
    /// was shown goes on until that works, and stays if it does not.
    fn use_portal(&mut self, portal: super::portal::Portal) -> Result<(), String> {
        if portal.apart {
            match super::pw::Stream::open(Some(portal.fd()), portal.node, std::time::Duration::from_secs(5)) {
                Ok(s) => {
                    let f = s.take().0.ok_or("no picture from the chosen window")?;
                    self.app = None;
                    (self.desk, self.target) = ((f.w, f.h), (f.w, f.h));
                    (self.pipe, self.pw, self.clean, self.dirty) = (None, Some(s), Some(f), true);
                    self.portal = Some(portal);
                    self.src = Src::Portal;
                    return Ok(());
                }
                Err((e, false)) => return Err(format!("no picture from the chosen window within 5 s ({e})")),
                // Read by GStreamer, which leaves the pointer out of this stream's frames.
                Err(_) => super::pw::give_up(),
            }
        }
        let pipe = pipeline(&portal.source(), None)?;
        let first = pipe
            .sink
            .try_pull_sample(gst::ClockTime::from_seconds(5))
            .ok_or("no picture from the chosen window within 5 s")?;
        let img = to_image(&first)?;
        self.app = None;
        (self.desk, self.target) = ((img.w, img.h), (img.w, img.h));
        (self.pipe, self.pw, self.clean, self.apart) = (Some(pipe), None, None, false);
        self.portal = Some(portal);
        self.src = Src::Portal;
        Ok(())
    }

    /// Input for a stream chosen in the portal dialog goes through the portal.
    fn portal_input(&self, ev: Input) -> Result<(), String> {
        let Some(p) = &self.portal else { return Ok(()) };
        p.input(ev, self.desk)
    }

    fn native(&self) -> bool {
        self.src != Src::Monitor || self.app.is_some()
    }

    fn helper(&self) -> Result<&Proxy<'static>, String> {
        if let Some(p) = self.helper.get() {
            return Ok(p);
        }
        let p = zbus::block_on(
            zbus::proxy::Builder::new(&self.conn).destination(HELPER).path(HELPER_PATH).interface(HELPER).cache_properties(zbus::proxy::CacheProperties::No).build(),
        )
        .map_err(|e| e.to_string())?;
        Ok(self.helper.get_or_init(|| p))
    }

    fn helper_list(&self) -> Result<Vec<HelperWin>, String> {
        let p = self.helper()?;
        update_helper();
        zbus::block_on(p.call("List", &())).map_err(|e| {
            let e = e.to_string();
            if e.contains("ServiceUnknown") || e.contains("NameHasNoOwner") || e.contains("UnknownObject") {
                if helper_dir().join("extension.js").exists() {
                    format!(
                        "{NEEDS_HELPER} It is installed but not running yet: log out and back in on the SSH host once."
                    )
                } else {
                    format!(
                        "{NEEDS_HELPER} Install it from here if you like; GNOME loads it at the next login on the \
                         SSH host. Without it, share the active window."
                    )
                }
            } else {
                format!("GNOME helper: {e}")
            }
        })
    }

    /// `text` typed into the focused text field by the helper; false without it, or with no
    /// such field (an X11 program).
    fn helper_commit(&self, text: &str) -> bool {
        self.helper().ok().and_then(|p| zbus::block_on(p.call::<_, _, bool>("Commit", &(text,))).ok()).unwrap_or(false)
    }

    /// Waits until key events sent `GAP` ago or more and text committed so are both through:
    /// Mutter passes keys on through its input thread, and text the helper commits straight, so
    /// one sent right after the other could arrive first.
    fn settle(&mut self, now_text: bool) {
        const GAP: std::time::Duration = std::time::Duration::from_millis(25);
        if let Some((text, at)) = self.last_typed
            && text != now_text
        {
            let left = GAP.saturating_sub(at.elapsed());
            if !left.is_zero() {
                std::thread::sleep(left);
            }
        }
        self.last_typed = Some((now_text, std::time::Instant::now()));
    }

    fn helper_activate(&self, id: u64) {
        if let Ok(p) = self.helper() {
            let _ = zbus::block_on(p.call::<_, _, ()>("Activate", &(id,)));
        }
    }

    /// Whether the window stream (or the portal session) still exists; it goes away with its window.
    fn stream_alive(&self) -> bool {
        if let Some(p) = &self.portal {
            return p.alive();
        }
        zbus::block_on(async {
            let p = Proxy::new(&self.conn, SC, self.stream_path.as_str(), "org.gnome.Mutter.ScreenCast.Stream").await?;
            p.get_property::<HashMap<String, OwnedVariant>>("Parameters").await
        })
        .is_ok()
    }

    /// Back to the whole monitor after the shared window or program closed.
    fn back_to_screen(&mut self) -> Result<(), String> {
        self.reopen(Src::Monitor)?;
        self.ended = true;
        Ok(())
    }

    /// A new recording of the same monitor (or of `src` on it). The picture keeps the size the
    /// viewer asked for when its size has not changed: the agent asks again only when it has.
    fn reopen(&mut self, src: Src) -> Result<(), String> {
        let (desk, target) = (self.desk, self.target);
        self.take_over(Gnome::open_on(Some(self.current), src)?);
        if self.desk == desk && target != desk {
            self.set_target(target.0, target.1)?;
        }
        Ok(())
    }

    /// Mutter ends a monitor's stream, without a word, when the monitors change (turned, another
    /// resolution, one added or taken away): it is recorded again, a shared program cut out of it
    /// again.
    fn record_again(&mut self) -> Result<(), String> {
        let cut = self.app.as_ref().map(|a| a.of.clone());
        let (desk, target) = (self.desk, self.target);
        self.take_over(Gnome::open_on(Some(self.current), Src::Monitor)?);
        if let Some(cut) = cut {
            self.cut_out(cut)?;
        }
        if self.desk == desk && target != desk {
            self.set_target(target.0, target.1)?;
        }
        Ok(())
    }

    /// Shares the program `app` from the monitor stream: its windows' part, the rest greyed out.
    fn cut_out(&mut self, of: Cut) -> Result<(), String> {
        self.app = Some(AppShare {
            of,
            area: R { l: 0, t: 0, r: self.desk.0 as i32, b: self.desk.1 as i32 },
            shown: Vec::new(),
            focused: true,
            inside: true,
            checked: std::time::Instant::now(),
        });
        self.refresh_app(true);
        if let Some(a) = &self.app {
            self.desk = ((a.area.r - a.area.l) as usize, (a.area.b - a.area.t) as usize);
        }
        self.restart_pipe()
    }

    /// Where the shared program's windows are on the recorded monitor, at most every 150 ms.
    fn refresh_app(&mut self, force: bool) {
        let Some(a) = self.app.as_ref() else { return };
        if !force && a.checked.elapsed() < std::time::Duration::from_millis(150) {
            return;
        }
        let Ok(list) = self.helper_list() else { return };
        // A window is the cut's when it is (or its pop-up's root is) the shared window, or of the
        // shared program.
        let of = self.app.as_ref().map(|a| a.of.clone()).expect("checked above");
        let app_of = |id: u64| list.iter().find(|w| w.0 == id).map(|w| w.2.as_str());
        let mine = |w: &HelperWin| match &of {
            Cut::Window(id) => w.0 == *id || w.10 == *id,
            Cut::App(app) => w.2 == *app || app_of(w.10) == Some(app.as_str()),
        };
        let alive = match &of {
            Cut::Window(id) => list.iter().any(|w| w.0 == *id),
            Cut::App(app) => list.iter().any(|w| w.2 == *app),
        };
        if !alive {
            // The window closed, or the program its last window.
            self.app = None;
            self.desk = self.screen;
            let _ = self.restart_pipe();
            self.ended = true;
            return;
        }
        let (lx, ly, scale) = self.monitors.get(self.current).map_or((0, 0, 1.0), |m| m.2);
        let screen = R { l: 0, t: 0, r: self.screen.0 as i32, b: self.screen.1 as i32 };
        let px = |v: i32, o: i32| ((v - o) as f64 * scale).round() as i32;
        let a = self.app.as_mut().unwrap();
        let mut stack = Vec::new();
        let mut area: Option<R> = None;
        let mut focused = false;
        for win in &list {
            let (x, y, w, h, hidden, focus) = (win.3, win.4, win.5, win.6, win.7, win.9);
            // Not drawn: neither part of what is shared nor over it.
            if hidden {
                continue;
            }
            let r = R { l: px(x, lx), t: px(y, ly), r: px(x + w, lx), b: px(y + h, ly) };
            let mine = mine(win);
            // Its pop-ups too, so a menu reaching past its window is seen whole.
            if mine {
                area = Some(area.map_or(r, |u| u.union(&r)));
            }
            focused |= mine && focus;
            stack.push((r, mine));
        }
        a.checked = std::time::Instant::now();
        a.focused = focused;
        let Some(area) = area.map(|u| u.and(&screen)).filter(|u| !u.empty()) else {
            a.shown.clear();
            return;
        };
        a.area = area;
        a.shown = region::shown_parts(&stack, area)
            .into_iter()
            .map(|r| R { l: r.l - area.l, t: r.t - area.t, r: r.r - area.l, b: r.b - area.t })
            .collect();
    }

    /// A picture from the stream read here: its newest frame, or its last one again when the
    /// viewer's size or the pointer drawn in changed.
    fn grab_own(&mut self) -> Result<Option<Image>, String> {
        let window = !matches!(self.src, Src::Monitor);
        let Some(pw) = &self.pw else { return Ok(None) };
        // The shared window closed and took its stream with it, or Mutter ended the monitor's.
        let mut gone = pw.stopped().is_some();
        if !gone {
            let (frame, moved) = pw.take();
            if let Some(f) = frame {
                self.last_frame = std::time::Instant::now();
                if let Some(old) = self.clean.replace(f) {
                    pw.recycle(old.data);
                }
            } else if !(self.dirty || moved && !self.apart) {
                // Nothing comes while the screen is still, nor once the stream went without a
                // word: after a second, check it exists.
                let second = std::time::Duration::from_secs(1);
                if self.last_frame.elapsed() < second || self.checked_alive.elapsed() < second {
                    return Ok(None);
                }
                self.checked_alive = std::time::Instant::now();
                if self.stream_alive() {
                    return Ok(None);
                }
                gone = true;
            }
        }
        if gone {
            if window { self.back_to_screen()? } else { self.record_again()? }
            return Ok(None);
        }
        self.dirty = false;
        if let Some(f) = &self.clean
            && self.src == Src::Monitor
            && (f.w, f.h) != self.screen
        {
            // The host's resolution or rotation changed under the monitor stream.
            self.screen = (f.w, f.h);
            self.monitors = zbus::block_on(monitors(&self.conn));
            if self.app.is_none() {
                // The agent tells the viewer the new size and asks for a new target.
                (self.desk, self.target, self.dirty) = (self.screen, self.screen, true);
                return Ok(None);
            }
            self.refresh_app(true);
        }
        if self.app.is_some() {
            // Its last window closed: the whole monitor comes instead.
            self.refresh_app(false);
        }
        let Some(clean) = &self.clean else { return Ok(None) };
        let frame = clean.pixels();
        let (mut img, origin) = match &self.app {
            Some(a) => (region::crop_masked(&frame, a.area, &a.shown), (a.area.l, a.area.t)),
            None => {
                // A window stream's buffers can be larger than the window; the window is the crop
                // area. Mutter marks a monitor's frames with an empty crop.
                let monitor = self.src == Src::Monitor || self.portal.as_ref().is_some_and(|p| p.monitor);
                let (mut cx, mut cy, mut cw, mut ch) = match clean.crop {
                    Some(c) => c,
                    None if !monitor => content_box(&frame),
                    None => (0, 0, frame.w, frame.h),
                };
                if cx + cw > frame.w || cy + ch > frame.h {
                    (cx, cy, cw, ch) = (0, 0, frame.w, frame.h);
                }
                let mut data = Vec::with_capacity(cw * ch * 4);
                for y in cy..cy + ch {
                    data.extend_from_slice(&frame.data[y * frame.stride + cx * 4..][..cw * 4]);
                }
                (Image { w: cw, h: ch, stride: cw * 4, data }, (cx as i32, cy as i32))
            }
        };
        if (img.w, img.h) != self.desk {
            // A window was resized, or the shared program's windows moved: the agent sees the
            // new size and asks for a new target.
            self.desk = (img.w, img.h);
            self.target = self.desk;
        }
        self.origin = origin;
        if !self.apart
            && let Some(pw) = &self.pw
            && let p = pw.pointer()
            && let (Some((x, y)), Some(shape)) = (p.at, &p.shape)
        {
            super::pw::draw(&mut img.data, img.w, img.h, img.stride, shape, (x - origin.0, y - origin.1));
        }
        if self.target == self.desk {
            return Ok(Some(img));
        }
        Ok(Some(self.shrink.image(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)))
    }
}

/// A gsettings string list (as `gsettings get` prints it) with the helper added.
fn with_helper(list: &str) -> String {
    let mut names: Vec<String> = list
        .trim_start_matches("@as")
        .trim()
        .trim_matches(['[', ']'])
        .split(',')
        .map(|n| n.trim().trim_matches('\'').to_string())
        .filter(|n| !n.is_empty() && n != HELPER_UUID)
        .collect();
    names.push(HELPER_UUID.into());
    format!("[{}]", names.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", "))
}

/// The helper extension's files, as this program has them.
const HELPER_FILES: [(&str, &str); 2] = [
    ("metadata.json", include_str!("../../assets/gnome-helper/metadata.json")),
    ("extension.js", include_str!("../../assets/gnome-helper/extension.js")),
];

/// A helper installed by an older sshremote is brought up to date, once per agent; GNOME loads
/// the new files at the next login.
pub(super) fn update_helper() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = helper_dir();
        for (name, body) in HELPER_FILES {
            let path = dir.join(name);
            if path.exists() && std::fs::read_to_string(&path).ok().as_deref() != Some(body) {
                let _ = std::fs::write(&path, body);
            }
        }
    });
}

/// Where the helper extension lives for this user.
fn helper_dir() -> std::path::PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| "/tmp".into());
    data.join("gnome-shell/extensions").join(HELPER_UUID)
}

impl Capture for Gnome {
    fn name(&self) -> &'static str {
        "gnome"
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.desk
    }

    fn monitors(&self) -> Vec<Monitor> {
        self.monitors.iter().map(|m| m.1.clone()).collect()
    }

    fn pick(&mut self) -> Result<String, String> {
        // The whole screen first, so the dialog can be seen and clicked from the viewer.
        if self.src != Src::Monitor || self.app.is_some() {
            self.share(Share::Screen)?;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(super::portal::pick());
        });
        self.picking = Some(rx);
        Ok("A sharing dialog is open on the SSH host: switch on Allow Remote Interaction (needed to control it), \
            choose a window under Window, then click Share."
            .into())
    }

    fn poll_share(&mut self) -> Option<Result<(u8, String), String>> {
        if std::mem::take(&mut self.ended) {
            return Some(Ok((0, "the whole screen, as the shared window closed".into())));
        }
        let answer = self.picking.as_ref()?.try_recv();
        match answer {
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(_) => {
                self.picking = None;
                Some(Err("the sharing dialog went away".into()))
            }
            Ok(r) => {
                self.picking = None;
                Some(
                    r.and_then(|p| self.use_portal(p))
                        .map(|_| (1, "the window chosen on the SSH host's screen".into())),
                )
            }
        }
    }

    fn install_helper(&mut self) -> Result<String, String> {
        let dir = helper_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, body) in HELPER_FILES {
            std::fs::write(dir.join(name), body).map_err(|e| format!("{name}: {e}"))?;
        }
        // Enabled in the settings now; GNOME Shell on Wayland loads new extensions only at login.
        let get = |key: &str| {
            std::process::Command::new("gsettings")
                .args(["get", "org.gnome.shell", key])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        let enabled = get("enabled-extensions");
        if !enabled.contains(HELPER_UUID) {
            let list = with_helper(&enabled);
            let ok = std::process::Command::new("gsettings")
                .args(["set", "org.gnome.shell", "enabled-extensions", &list])
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                return Err("installed the helper, but could not enable it with gsettings".into());
            }
        }
        let mut text = "The sshremote helper is installed on the SSH host. GNOME loads it at the next login: log out \
                        and back in there once, then the window list appears here."
            .to_string();
        if get("disable-user-extensions") == "true" {
            text.push_str(" Extensions are switched off there; switch them on in the Extensions app.");
        }
        Ok(text)
    }

    fn current(&self) -> usize {
        self.current
    }

    fn screen_id(&self) -> Option<String> {
        self.monitors.get(self.current).map(|m| m.0.clone())
    }

    fn screen_changed(&mut self) {
        // The monitor recorded again at its new size (a window shared is recorded as it was);
        // one that kept its size keeps the picture's too.
        if self.src != Src::Monitor || self.reopen(Src::Monitor).is_err() {
            self.monitors = zbus::block_on(monitors(&self.conn));
        }
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.monitors.len() {
            return Err("no such monitor".into());
        }
        // A new remote-desktop session records the other monitor; dropping the old one stops it.
        self.take_over(Gnome::open_on(Some(index), Src::Monitor)?);
        Ok(())
    }

    fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        let list = self.helper_list()?;
        Ok(list
            .into_iter()
            .filter(|w| w.8 && w.5 >= 80 && w.6 >= 40)
            .map(|(id, title, app, _, _, w, h, ..)| WindowInfo { id, title, app, w: w as u16, h: h as u16 })
            .collect())
    }

    fn share(&mut self, scope: Share) -> Result<String, String> {
        let monitor = |g: &mut Gnome| -> Result<(), String> {
            if g.src != Src::Monitor {
                g.take_over(Gnome::open_on(Some(g.current), Src::Monitor)?);
            }
            Ok(())
        };
        match scope {
            Share::Screen => {
                monitor(self)?;
                self.app = None;
                self.desk = self.screen;
                self.restart_pipe()?;
                Ok("the whole screen".into())
            }
            Share::ActiveWindow => {
                // Cut out of the monitor with its pop-ups when the helper tells where it is; its own
                // stream (without them) otherwise.
                if let Ok(list) = self.helper_list()
                    && let Some(id) = list.iter().find(|w| w.9).map(|w| w.10)
                {
                    monitor(self)?;
                    self.cut_out(Cut::Window(id))?;
                } else {
                    self.take_over(Gnome::open_on(Some(self.current), Src::Focused)?);
                }
                Ok("the active window".into())
            }
            Share::ActiveApp => {
                // Cutting a program out needs to know where its other windows are, which only the helper tells.
                let list = self.helper_list().map_err(|_| {
                    "On GNOME the active application needs the sshremote helper, which tells where the program's \
                     other windows are. The active window works without it."
                        .to_string()
                })?;
                let id = list.iter().find(|w| w.9).map(|w| w.10).ok_or("no window has the focus")?;
                self.share(Share::App(id))
            }
            Share::Window(id) => {
                let list = self.helper_list()?;
                let title = list.iter().find(|w| w.0 == id).map(|w| w.1.clone()).ok_or("that window is gone")?;
                self.helper_activate(id);
                // Cut out of the monitor, so its menus and dialogs show with it.
                monitor(self)?;
                self.cut_out(Cut::Window(id))?;
                Ok(format!("the window \"{title}\""))
            }
            Share::App(id) => {
                let list = self.helper_list()?;
                let app = list.iter().find(|w| w.0 == id).map(|w| w.2.clone()).ok_or("that window is gone")?;
                self.helper_activate(id);
                monitor(self)?;
                self.cut_out(Cut::App(app.clone()))?;
                Ok(format!("every window of {app}"))
            }
        }
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        if self.pw.is_some() {
            // Frames come at their own size: made smaller here (a window's larger too).
            self.target = if self.native() { (w.max(1), h.max(1)) } else { (w.min(self.desk.0).max(1), h.min(self.desk.1).max(1)) };
            self.dirty = true;
            return Ok(());
        }
        if self.native() {
            // A window stream stays at its own size, which follows the window; it is scaled here,
            // up as well for a window smaller than the least the viewer asks for.
            if self.pipe.is_none() {
                self.pipe = Some(pipeline(&self.source(), None)?);
            }
            self.target = (w.max(1), h.max(1));
            return Ok(());
        }
        let t = (w.min(self.desk.0), h.min(self.desk.1));
        if t != self.target || self.pipe.is_none() {
            self.pipe = None;
            self.pipe = Some(pipeline(&self.source(), (t != self.desk).then_some(t))?);
            self.target = t;
        }
        Ok(())
    }

    fn pointer_apart(&mut self, on: bool) -> bool {
        if self.pw.is_none() {
            return false;
        }
        (self.apart, self.sent_shape, self.dirty) = (on, 0, true);
        true
    }

    fn pointer(&mut self) -> (Option<super::Pointer>, Option<(i32, i32)>) {
        let Some(pw) = &self.pw else { return (None, None) };
        let p = pw.pointer();
        let Some(shape) = p.shape.filter(|s| s.w > 0) else { return (None, None) };
        let new = (p.serial != self.sent_shape).then(|| shape.to_pointer());
        self.sent_shape = p.serial;
        (new, p.at.map(|(x, y)| (x - self.origin.0, y - self.origin.1)))
    }

    fn set_waker(&mut self, wake: Box<dyn Fn() + Send>) -> bool {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
        true
    }

    fn wakes(&self) -> bool {
        self.pw.is_some() || self.pipe.is_some()
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        // Whichever reads the stream now wakes the agent as it sends something.
        if let Some(pw) = &self.pw {
            pw.wake_with(&self.wake);
        }
        if let Some(pipe) = &self.pipe {
            pipe.wake_with(&self.wake);
        }
        if self.pw.is_some() {
            return self.grab_own();
        }
        let Some(pipe) = &self.pipe else {
            return Ok(None);
        };
        let window = !matches!(self.src, Src::Monitor);
        if let Some(bus) = pipe.p.bus()
            && let Some(m) = bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])
        {
            if window {
                // The shared window closed and took its stream with it.
                self.back_to_screen()?;
                return Ok(None);
            }
            return Err(format!("GNOME stream stopped: {:?}", m.view()));
        }
        let Some(s) = pipe.sink.try_pull_sample(gst::ClockTime::ZERO) else {
            // A window stream sends nothing while its window is still, and a monitor's (otherwise
            // repeated four times a second) nothing once Mutter ended it: after a second, check
            // it exists.
            let quiet = self.last_frame.elapsed() > std::time::Duration::from_secs(1);
            if quiet && self.checked_alive.elapsed() > std::time::Duration::from_secs(1) {
                self.checked_alive = std::time::Instant::now();
                if !self.stream_alive() {
                    if window {
                        self.back_to_screen()?;
                    } else {
                        self.record_again()?;
                    }
                }
            }
            return Ok(None);
        };
        self.last_frame = std::time::Instant::now();
        // The host's resolution or rotation changed under the monitor stream.
        let now = if self.src == Src::Monitor { pipe.source_size() } else { None };
        if let Some(now) = now.filter(|&n| n != self.screen) {
            self.screen = now;
            self.monitors = zbus::block_on(monitors(&self.conn));
            if self.app.is_none() {
                // The agent tells the viewer the new size and asks for a new target, which
                // remakes the pipeline; this frame was scaled for the old one.
                self.desk = now;
                return Ok(None);
            }
            self.refresh_app(true);
        }
        if !self.native() {
            return to_image(&s).map(Some);
        }
        // Cut and scaled straight from the stream's buffer: one copy of the part sent, not three
        // of the whole frame.
        let caps = s.caps().ok_or("frame without caps")?;
        let info = gst_video::VideoInfo::from_caps(caps).map_err(|e| e.to_string())?;
        let buf = s.buffer().ok_or("frame without buffer")?;
        let map = buf.map_readable().map_err(|e| e.to_string())?;
        let frame = Pixels { w: info.width() as usize, h: info.height() as usize, stride: info.stride()[0] as usize, data: map.as_slice() };
        if self.app.is_some() {
            self.refresh_app(false);
            // Its last window closed: the whole monitor comes next, the pipeline already restarted.
            let Some(a) = self.app.as_ref() else { return Ok(None) };
            let img = region::crop_masked(&frame, a.area, &a.shown);
            if (img.w, img.h) != self.desk {
                self.desk = (img.w, img.h);
                self.target = self.desk;
            }
            if self.target == self.desk {
                return Ok(Some(img));
            }
            return Ok(Some(self.shrink.image(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)));
        }
        // A window stream's buffers can be larger than the window; the window is the crop area.
        // Mutter marks a monitor's frames with an empty crop.
        let crop = buf.meta::<gst_video::VideoCropMeta>().map(|m| m.rect());
        let monitor = self.portal.as_ref().is_some_and(|p| p.monitor);
        let (mut cx, mut cy, mut cw, mut ch) = match crop {
            Some((x, y, w, h)) if w > 0 && h > 0 => (x as usize, y as usize, w as usize, h as usize),
            None if !monitor => content_box(&frame),
            _ => (0, 0, frame.w, frame.h),
        };
        if cx + cw > frame.w || cy + ch > frame.h {
            (cx, cy, cw, ch) = (0, 0, frame.w, frame.h);
        }
        if (cw, ch) != self.desk {
            // The window was resized: the agent sees the new size and asks for a new target.
            self.desk = (cw, ch);
            self.target = self.desk;
        }
        let from = &frame.data[cy * frame.stride + cx * 4..];
        if self.target != self.desk {
            return Ok(Some(self.shrink.image(from, cw, ch, frame.stride, self.target.0, self.target.1)));
        }
        let mut data = Vec::with_capacity(cw * ch * 4);
        for y in 0..ch {
            data.extend_from_slice(&from[y * frame.stride..y * frame.stride + cw * 4]);
        }
        Ok(Some(Image { w: cw, h: ch, stride: cw * 4, data }))
    }

    fn commit_text(&mut self, text: &str) -> bool {
        if self.portal.is_some() {
            return false;
        }
        self.settle(true);
        self.helper_commit(text)
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        if self.portal.is_some() {
            return self.portal_input(ev);
        }
        // Sharing a program or a window: clicks only on it, keys only while it has the focus.
        if let Some(a) = self.app.as_mut() {
            match &ev {
                Input::Move { x, y } => a.inside = a.shown.iter().any(|r| r.contains(*x as i32, *y as i32)),
                Input::Button { pressed: true, .. } if !a.inside => return Err(super::OUTSIDE.into()),
                Input::MoveBy { .. } => return Err(super::WHOLE_SCREEN_MOUSE.into()),
                Input::Scroll { .. } | Input::HScroll { .. } if !a.inside => return Ok(()),
                Input::Key { .. } | Input::KeyState { down: true, .. } if !a.focused => {
                    let what = if matches!(a.of, Cut::Window(_)) { "window" } else { "program" };
                    return Err(format!("keys go only to the shared {what}; click it first"));
                }
                _ => {}
            }
        }
        match ev {
            Input::Move { x, y } => {
                let x = x.clamp(0.0, self.desk.0 as f64 - 1.0);
                let y = y.clamp(0.0, self.desk.1 as f64 - 1.0);
                // A program is cut out of the monitor stream, whose coordinates the pointer uses.
                let (x, y) = match &self.app {
                    Some(a) => (x + a.area.l as f64, y + a.area.t as f64),
                    None => (x, y),
                };
                notify!(self, "NotifyPointerMotionAbsolute", &(self.stream_path.as_str(), x, y))
            }
            Input::Button { button, pressed } => {
                let code: i32 = match button {
                    1 => 0x110,
                    2 => 0x112,
                    3 => 0x111,
                    8 => 0x113,
                    9 => 0x114,
                    _ => return Ok(()),
                };
                notify!(self, "NotifyPointerButton", &(code, pressed))
            }
            Input::Scroll { steps } => notify!(self, "NotifyPointerAxisDiscrete", &(0u32, steps)),
            Input::HScroll { steps } => notify!(self, "NotifyPointerAxisDiscrete", &(1u32, steps)),
            Input::MoveBy { dx, dy } => notify!(self, "NotifyPointerMotionRelative", &(dx as f64, dy as f64)),
            Input::KeyState { keysym, down } => {
                self.settle(false);
                notify!(self, "NotifyKeyboardKeysym", &(keysym, down))
            }
            Input::SecureAttention => {
                // GNOME answers Ctrl+Alt+Del with its log-out dialog.
                for (c, down) in [(29u32, true), (56, true), (111, true), (111, false), (56, false), (29, false)] {
                    notify!(self, "NotifyKeyboardKeycode", &(c, down))?;
                }
                Ok(())
            }
            Input::Key { keysym, mods } => {
                self.settle(false);
                // evdev keycodes for the modifiers; the key itself goes by keysym.
                let held: Vec<u32> = [(MOD_CTRL, 29), (MOD_SHIFT, 42), (MOD_ALT, 56), (MOD_SUPER, 125)]
                    .into_iter()
                    .filter(|(m, _)| mods & m != 0)
                    .map(|(_, c)| c)
                    .collect();
                for &c in &held {
                    notify!(self, "NotifyKeyboardKeycode", &(c, true))?;
                }
                let r = notify!(self, "NotifyKeyboardKeysym", &(keysym, true))
                    .and_then(|_| notify!(self, "NotifyKeyboardKeysym", &(keysym, false)));
                for &c in held.iter().rev() {
                    notify!(self, "NotifyKeyboardKeycode", &(c, false))?;
                }
                r
            }
        }
    }
}

impl Drop for Gnome {
    fn drop(&mut self) {
        self.pipe = None;
        self.pw = None;
        let _ = zbus::block_on(self.session.call_noreply("Stop", &()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_joins_the_enabled_extensions() {
        assert_eq!(with_helper("@as []"), format!("['{HELPER_UUID}']"));
        assert_eq!(
            with_helper("['ding@rastersoft.com', 'ubuntu-dock@ubuntu.com']"),
            format!("['ding@rastersoft.com', 'ubuntu-dock@ubuntu.com', '{HELPER_UUID}']")
        );
    }

    /// The agent is woken as pictures come, also after the monitor was recorded again (its
    /// resolution changed, or a window shared and let go): the stream that takes over wakes it.
    /// In a private session: SSHREMOTE_GNOME_TEST=1 scripts/gnome-session.sh run TESTS --ignored
    /// the_agent_is_woken
    #[test]
    #[ignore]
    fn the_agent_is_woken_after_the_monitor_is_recorded_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        if std::env::var_os("SSHREMOTE_GNOME_TEST").is_none() {
            return;
        }
        let woken = std::sync::Arc::new(AtomicUsize::new(0));
        let mut g = Gnome::open().expect("a GNOME session");
        let w = woken.clone();
        assert!(g.set_waker(Box::new(move || _ = w.fetch_add(1, Ordering::Relaxed))));
        // Pictures taken while the pointer moves (each move changes the screen), the wakes counted.
        let count = |g: &mut Gnome| {
            woken.store(0, Ordering::Relaxed);
            for i in 0..40 {
                g.input(Input::Move { x: 200.0 + (i % 20) as f64 * 10.0, y: 200.0 }).unwrap();
                let _ = g.grab();
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            woken.load(Ordering::Relaxed)
        };
        let before = count(&mut g);
        g.screen_changed();
        let after = count(&mut g);
        eprintln!("woken {before} times before, {after} after the monitor was recorded again");
        assert!(before > 5 && after > 5);
    }

    /// GNOME's pointer kept apart: it comes beside the frames, its picture once with where it
    /// is, pictures leave it out, and put back it is drawn in again. In a private session:
    /// SSHREMOTE_GNOME_TEST=1 scripts/gnome-session.sh run TESTS --ignored the_gnome_pointer_comes_apart
    #[test]
    #[ignore]
    fn the_gnome_pointer_comes_apart() {
        if std::env::var_os("SSHREMOTE_GNOME_TEST").is_none() {
            return;
        }
        let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
        let next = |g: &mut Gnome| (0..150).find_map(|_| g.grab().unwrap().or_else(|| (pause(), None).1));
        // The pixels around a point, to tell whether the pointer is drawn there.
        let around = |img: &Image, (x, y): (usize, usize)| -> Vec<u8> {
            (y..y + 24).flat_map(|r| img.data[r * img.stride + x * 4..][..24 * 4].to_vec()).collect()
        };
        let mut g = Gnome::open().expect("a GNOME session");
        assert!(g.pw.is_some(), "the stream is read here");
        // A new session's first move can leave the pointer in its corner's column.
        g.input(Input::Move { x: 250.0, y: 150.0 }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        g.input(Input::Move { x: 300.0, y: 200.0 }).unwrap();
        assert!(g.pointer_apart(true));
        let without = next(&mut g).expect("a picture");
        let (mut shape, mut at) = (None, None);
        for _ in 0..150 {
            let (s, a) = g.pointer();
            shape = shape.or(s);
            at = a;
            if shape.is_some() && at == Some((300, 200)) {
                break;
            }
            pause();
        }
        let shape = shape.expect("the pointer's picture");
        assert!(shape.w > 0 && shape.rgba.len() == shape.w * shape.h * 4, "{}x{}", shape.w, shape.h);
        assert_eq!(at, Some((300, 200)));
        assert!(g.pointer().0.is_none(), "the same picture is not sent again");
        g.input(Input::Move { x: 500.0, y: 400.0 }).unwrap();
        assert!((0..150).any(|_| g.pointer().1 == Some((500, 400)) || (pause(), false).1), "where it went");
        // Moved by so much (the mouse for games), and back.
        g.input(Input::MoveBy { dx: 40, dy: -30 }).unwrap();
        assert!((0..150).any(|_| g.pointer().1 == Some((540, 370)) || (pause(), false).1), "moved by 40, -30: {:?}", g.pointer().1);
        g.input(Input::MoveBy { dx: -40, dy: 30 }).unwrap();
        assert!((0..150).any(|_| g.pointer().1 == Some((500, 400)) || (pause(), false).1), "and back");
        let still = next(&mut g).unwrap_or(without);
        let bare = around(&still, (490, 390));
        // Put back into the pictures, it is drawn where it is.
        assert!(g.pointer_apart(false));
        let with = next(&mut g).expect("a picture with the pointer");
        assert_ne!(around(&with, (490, 390)), bare, "the pointer drawn in");
        assert_eq!(around(&with, (100, 600)), around(&still, (100, 600)), "and nothing else changed");
    }
}
