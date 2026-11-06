//! GNOME Wayland capture and input through Mutter's RemoteDesktop/ScreenCast
//! D-Bus API, which works from an SSH login without a consent dialog.
//! Session flow follows sshdesk (MIT).

use std::collections::HashMap;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::region::{self, R};
use super::{Capture, Image, Input, Monitor, NEEDS_HELPER, Share, WindowInfo};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

macro_rules! notify {
    ($s:expr, $method:literal, $body:expr) => {
        $s.session.call::<_, _, ()>($method, $body).map_err(|e| format!("GNOME {}: {e}", $method))
    };
}

const RD: &str = "org.gnome.Mutter.RemoteDesktop";
/// The desk helper extension, which lists windows (GNOME Shell keeps its own list to its portal).
const HELPER: &str = "io.github.thinkbigx.Desk.Windows";
const HELPER_PATH: &str = "/io/github/thinkbigx/Desk/Windows";
const HELPER_UUID: &str = "desk-helper@thinkbigx.github.io";

/// id, title, program, x, y, width, height (stage coordinates), minimized, shareable, focused, owner id.
type HelperWin = (u64, String, String, i32, i32, i32, i32, bool, bool, bool, u64);

/// What a stream records.
#[derive(Clone, Copy, PartialEq)]
enum Src {
    Monitor,
    /// The focused window.
    Focused,
    Window(u64),
}

/// Sharing one program: its windows' part of the monitor, the rest greyed out.
struct AppShare {
    app: String,
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
    target: (usize, usize),
    pipe: Option<(gst::Pipeline, gst_app::AppSink)>,
    src: Src,
    app: Option<AppShare>,
    /// For a shared window: whether it had the focus when last asked, and when that was.
    window_focus: (bool, std::time::Instant),
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
    // The desktop session's display (Xwayland is not always :0) and its X authority come from the user manager.
    let env = std::process::Command::new("systemctl").args(["--user", "show-environment"]).output();
    if let Ok(out) = env {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((k @ ("DISPLAY" | "WAYLAND_DISPLAY" | "XAUTHORITY"), v)) = line.split_once('=')
                && std::env::var_os(k).is_none()
            {
                // SAFETY: as above.
                unsafe { std::env::set_var(k, v) };
            }
        }
    }
}

fn pipeline(node: u32, size: Option<(usize, usize)>) -> Result<(gst::Pipeline, gst_app::AppSink), String> {
    let caps = match size {
        Some((w, h)) => format!(",width={w},height={h},pixel-aspect-ratio=1/1"),
        None => String::new(),
    };
    let desc = format!(
        "pipewiresrc path={node} do-timestamp=true keepalive-time=250 \
         ! queue leaky=downstream max-size-buffers=1 \
         ! videoconvert n-threads=2 ! videoscale method=1 n-threads=2 \
         ! video/x-raw,format=BGRx{caps} \
         ! appsink name=sink max-buffers=1 drop=true sync=false"
    );
    let p = gst::parse::launch(&desc)
        .map_err(|e| format!("GNOME capture pipeline: {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "GNOME capture pipeline is not a pipeline".to_string())?;
    let sink =
        p.by_name("sink").and_then(|s| s.downcast::<gst_app::AppSink>().ok()).ok_or("GNOME capture sink missing")?;
    p.set_state(gst::State::Playing).map_err(|e| format!("GNOME capture did not start: {e}"))?;
    Ok((p, sink))
}

fn to_image(sample: &gst::Sample) -> Result<Image, String> {
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

/// Without crop information: the frame minus pure black rows and columns at its right and bottom.
fn content_box(img: &Image) -> (usize, usize, usize, usize) {
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

type ModeT = (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedValue>);
type MonitorT = ((String, String, String, String), Vec<ModeT>, HashMap<String, OwnedValue>);
type LogicalT = (i32, i32, f64, u32, bool, Vec<(String, String, String, String)>, HashMap<String, OwnedValue>);

/// Connectors and current modes of the monitors, primary first, from Mutter's DisplayConfig.
fn monitors(conn: &Connection) -> Vec<(String, Monitor, (i32, i32, f64))> {
    let Ok(dc) = Proxy::new(
        conn,
        "org.gnome.Mutter.DisplayConfig",
        "/org/gnome/Mutter/DisplayConfig",
        "org.gnome.Mutter.DisplayConfig",
    ) else {
        return Vec::new();
    };
    let state: Result<(u32, Vec<MonitorT>, Vec<LogicalT>, HashMap<String, OwnedValue>), _> =
        dc.call("GetCurrentState", &());
    let Ok((_, mons, logical, _)) = state else { return Vec::new() };
    let primary: Vec<String> = logical.iter().filter(|l| l.4).flat_map(|l| l.5.iter().map(|m| m.0.clone())).collect();
    let place = |connector: &str| {
        logical.iter().find(|l| l.5.iter().any(|m| m.0 == connector)).map_or((0, 0, 1.0), |l| (l.0, l.1, l.2))
    };
    let mut out: Vec<(String, Monitor, (i32, i32, f64))> = mons
        .into_iter()
        .filter_map(|((connector, vendor, product, _), modes, _)| {
            let current =
                modes.iter().find(|m| m.6.get("is-current").is_some_and(|v| bool::try_from(v).unwrap_or(false)))?;
            let name = format!("{connector} {vendor} {product}").trim().to_string();
            let primary = primary.contains(&connector);
            let at = place(&connector);
            Some((connector, Monitor { name, w: current.1 as usize, h: current.2 as usize, primary }, at))
        })
        .collect();
    out.sort_by_key(|m| !m.1.primary);
    out
}

impl Gnome {
    pub fn open() -> Result<Gnome, String> {
        Gnome::open_on(None, Src::Monitor)
    }

    /// Records the monitor at `index` in the monitor list (or the primary one), or a window.
    fn open_on(index: Option<usize>, src: Src) -> Result<Gnome, String> {
        ensure_env();
        gst::init().map_err(|e| format!("GStreamer init: {e}"))?;
        let conn = Connection::session().map_err(err("session bus"))?;
        let rd = Proxy::new(&conn, RD, "/org/gnome/Mutter/RemoteDesktop", RD).map_err(err("RemoteDesktop"))?;
        let spath: OwnedObjectPath = rd.call("CreateSession", &()).map_err(err("CreateSession"))?;
        let session = Proxy::new(&conn, RD, spath, "org.gnome.Mutter.RemoteDesktop.Session").map_err(err("session"))?;
        let sid: String = session.get_property("SessionId").map_err(err("SessionId"))?;

        let sc = Proxy::new(&conn, SC, "/org/gnome/Mutter/ScreenCast", SC).map_err(err("ScreenCast"))?;
        let mut props: HashMap<&str, Value> = HashMap::new();
        props.insert("remote-desktop-session-id", Value::from(sid.as_str()));
        let cpath: OwnedObjectPath = sc.call("CreateSession", &(props,)).map_err(err("ScreenCast session"))?;
        let cast = Proxy::new(&conn, SC, cpath, "org.gnome.Mutter.ScreenCast.Session").map_err(err("cast"))?;
        let mut sprops: HashMap<&str, Value> = HashMap::new();
        // Cursor embedded in frames, so the remote pointer is visible.
        sprops.insert("cursor-mode", Value::U32(1));
        let list = monitors(&conn);
        let current = index.unwrap_or(0).min(list.len().saturating_sub(1));
        let connector = list.get(current).map_or(String::new(), |m| m.0.clone());
        // Without a window id Mutter records the window that has the focus.
        let stream: OwnedObjectPath = match src {
            Src::Monitor => cast.call("RecordMonitor", &(connector.as_str(), sprops)).map_err(err("RecordMonitor"))?,
            Src::Focused => cast.call("RecordWindow", &(sprops,)).map_err(err("RecordWindow"))?,
            Src::Window(id) => {
                sprops.insert("window-id", Value::U64(id));
                cast.call("RecordWindow", &(sprops,)).map_err(err("RecordWindow"))?
            }
        };
        let stream_path = stream.as_str().to_owned();
        let sproxy = Proxy::new(&conn, SC, stream, "org.gnome.Mutter.ScreenCast.Stream").map_err(err("stream"))?;
        let mut added = sproxy.receive_signal("PipeWireStreamAdded").map_err(err("signal"))?;
        session.call::<_, _, ()>("Start", &()).map_err(err("Start"))?;
        let msg = added.next().ok_or("GNOME did not publish a PipeWire stream")?;
        let (node,): (u32,) = msg.body().deserialize().map_err(err("PipeWire node"))?;

        let (p, sink) = pipeline(node, None)?;
        let first = sink
            .try_pull_sample(gst::ClockTime::from_seconds(5))
            .ok_or("no frame from the GNOME stream within 5 s (is a desktop session logged in and unlocked?)")?;
        let img = to_image(&first)?;
        let desk = (img.w, img.h);
        Ok(Gnome {
            monitors: list,
            current,
            conn,
            session,
            stream_path,
            node,
            desk,
            target: desk,
            pipe: Some((p, sink)),
            src,
            app: None,
            window_focus: (true, std::time::Instant::now()),
        })
    }

    /// A new pipeline on the same stream, for when cutting or scaling changes; the old one is stopped first.
    fn restart_pipe(&mut self) -> Result<(), String> {
        if let Some((p, _)) = self.pipe.take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.target = self.desk;
        self.set_target(self.desk.0, self.desk.1)
    }

    /// Frames at their own size, cut and scaled here: windows change size, programs are cut out.
    fn native(&self) -> bool {
        self.src != Src::Monitor || self.app.is_some()
    }

    fn helper_list(&self) -> Result<Vec<HelperWin>, String> {
        let p = Proxy::new(&self.conn, HELPER, HELPER_PATH, HELPER).map_err(|e| e.to_string())?;
        p.call("List", &()).map_err(|e| {
            let e = e.to_string();
            if e.contains("ServiceUnknown") || e.contains("NameHasNoOwner") || e.contains("UnknownObject") {
                if helper_dir().join("extension.js").exists() {
                    format!("{NEEDS_HELPER} It is installed but not running yet: log out and back in on the remote host once.")
                } else {
                    format!(
                        "{NEEDS_HELPER} Install it from here if you like; GNOME loads it at the next login on the \
                         remote host. Without it, share the active window."
                    )
                }
            } else {
                format!("GNOME helper: {e}")
            }
        })
    }

    fn helper_activate(&self, id: u64) {
        if let Ok(p) = Proxy::new(&self.conn, HELPER, HELPER_PATH, HELPER) {
            let _ = p.call::<_, _, ()>("Activate", &(id,));
        }
    }

    /// Where the shared program's windows are on the recorded monitor, at most every 150 ms.
    fn refresh_app(&mut self, force: bool) {
        let Some(a) = self.app.as_ref() else { return };
        if !force && a.checked.elapsed() < std::time::Duration::from_millis(150) {
            return;
        }
        let Ok(list) = self.helper_list() else { return };
        let (lx, ly, scale) = self.monitors.get(self.current).map_or((0, 0, 1.0), |m| m.2);
        let mon = self.monitors.get(self.current).map_or((self.desk.0, self.desk.1), |m| (m.1.w, m.1.h));
        let screen = R { l: 0, t: 0, r: mon.0 as i32, b: mon.1 as i32 };
        let px = |v: i32, o: i32| ((v - o) as f64 * scale).round() as i32;
        let a = self.app.as_mut().unwrap();
        let mut stack = Vec::new();
        let mut area: Option<R> = None;
        let mut focused = false;
        for (_, _, app, x, y, w, h, minimized, shareable, focus, _) in &list {
            if *minimized {
                continue;
            }
            let r = R { l: px(*x, lx), t: px(*y, ly), r: px(x + w, lx), b: px(y + h, ly) };
            let mine = *app == a.app;
            if mine && *shareable {
                area = Some(area.map_or(r, |u| u.union(&r)));
            }
            focused |= mine && *focus;
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

    /// Whether the shared window has the focus, asked at most twice a second.
    fn window_has_focus(&mut self) -> bool {
        let Src::Window(id) = self.src else { return true };
        if self.window_focus.1.elapsed() > std::time::Duration::from_millis(500) {
            let focused = match self.helper_list() {
                Ok(list) => list.iter().any(|w| w.9 && (w.0 == id || w.10 == id)),
                Err(_) => true,
            };
            self.window_focus = (focused, std::time::Instant::now());
        }
        self.window_focus.0
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

    fn install_helper(&mut self) -> Result<String, String> {
        let dir = helper_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, body) in [
            ("metadata.json", include_str!("../../assets/gnome-helper/metadata.json")),
            ("extension.js", include_str!("../../assets/gnome-helper/extension.js")),
        ] {
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
        let mut text = "The desk helper is installed on the remote host. GNOME loads it at the next login: log out \
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

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.monitors.len() {
            return Err("no such monitor".into());
        }
        // A new remote-desktop session records the other monitor; dropping the old one stops it.
        *self = Gnome::open_on(Some(index), Src::Monitor)?;
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
                *g = Gnome::open_on(Some(g.current), Src::Monitor)?;
            }
            Ok(())
        };
        match scope {
            Share::Screen => {
                monitor(self)?;
                self.app = None;
                self.target = self.desk;
                if let Some(m) = self.monitors.get(self.current) {
                    self.desk = (m.1.w, m.1.h);
                }
                self.restart_pipe()?;
                Ok("the whole screen".into())
            }
            Share::ActiveWindow => {
                *self = Gnome::open_on(Some(self.current), Src::Focused)?;
                Ok("the active window".into())
            }
            Share::ActiveApp => {
                // Cutting a program out needs to know where its other windows are, which only the helper tells.
                let list = self.helper_list().map_err(|_| {
                    "On GNOME the active application needs the desk helper, which tells where the program's \
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
                *self = Gnome::open_on(Some(self.current), Src::Window(id))?;
                Ok(format!("the window \"{title}\""))
            }
            Share::App(id) => {
                let list = self.helper_list()?;
                let app = list.iter().find(|w| w.0 == id).map(|w| w.2.clone()).ok_or("that window is gone")?;
                self.helper_activate(id);
                monitor(self)?;
                self.app = Some(AppShare {
                    app: app.clone(),
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
                self.restart_pipe()?;
                Ok(format!("every window of {app}"))
            }
        }
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        let t = (w.min(self.desk.0), h.min(self.desk.1));
        if self.native() {
            // A window stream stays at its own size, which follows the window; it is scaled here.
            if self.pipe.is_none() {
                self.pipe = Some(pipeline(self.node, None)?);
            }
            self.target = t;
            return Ok(());
        }
        if t != self.target || self.pipe.is_none() {
            if let Some((p, _)) = self.pipe.take() {
                let _ = p.set_state(gst::State::Null);
            }
            self.pipe = Some(pipeline(self.node, (t != self.desk).then_some(t))?);
            self.target = t;
        }
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        let Some((p, sink)) = &self.pipe else {
            return Ok(None);
        };
        if let Some(bus) = p.bus() {
            if let Some(m) = bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos]) {
                return Err(format!("GNOME stream stopped: {:?}", m.view()));
            }
        }
        let Some(s) = sink.try_pull_sample(gst::ClockTime::ZERO) else { return Ok(None) };
        let mut img = to_image(&s)?;
        if !self.native() {
            return Ok(Some(img));
        }
        if self.app.is_some() {
            self.refresh_app(false);
            let a = self.app.as_ref().unwrap();
            let img = region::crop_masked(&img, a.area, &a.shown);
            if (img.w, img.h) != self.desk {
                self.desk = (img.w, img.h);
                self.target = self.desk;
            }
            if self.target == self.desk {
                return Ok(Some(img));
            }
            return Ok(Some(super::scale_nearest(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)));
        }
        // A window stream's buffers can be larger than the window; the window is the crop area.
        let crop = s.buffer().and_then(|b| b.meta::<gst_video::VideoCropMeta>().map(|m| m.rect()));
        let (cx, cy, cw, ch) = match crop {
            Some((x, y, w, h)) if w > 0 && h > 0 => (x as usize, y as usize, w as usize, h as usize),
            _ => content_box(&img),
        };
        if (cx, cy, cw, ch) != (0, 0, img.w, img.h) && cx + cw <= img.w && cy + ch <= img.h {
            let mut data = Vec::with_capacity(cw * ch * 4);
            for y in cy..cy + ch {
                let row = y * img.stride + cx * 4;
                data.extend_from_slice(&img.data[row..row + cw * 4]);
            }
            img = Image { w: cw, h: ch, stride: cw * 4, data };
        }
        if (img.w, img.h) != self.desk {
            // The window was resized: the agent sees the new size and asks for a new target.
            self.desk = (img.w, img.h);
            self.target = self.desk;
        }
        if self.target == self.desk {
            return Ok(Some(img));
        }
        Ok(Some(super::scale_nearest(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        // Sharing a program or a window: clicks only on it, keys only while it has the focus.
        if let Some(a) = self.app.as_mut() {
            match &ev {
                Input::Move { x, y } => a.inside = a.shown.iter().any(|r| r.contains(*x as i32, *y as i32)),
                Input::Button { pressed: true, .. } | Input::Scroll { .. } | Input::HScroll { .. } if !a.inside => {
                    return Ok(());
                }
                Input::Key { .. } | Input::KeyState { down: true, .. } if !a.focused => {
                    return Err("keys go only to the shared program; click it first".into());
                }
                _ => {}
            }
        } else if matches!(ev, Input::Key { .. } | Input::KeyState { down: true, .. }) && !self.window_has_focus() {
            return Err("keys go only to the shared window; click it first".into());
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
            Input::KeyState { keysym, down } => notify!(self, "NotifyKeyboardKeysym", &(keysym, down)),
            Input::SecureAttention => {
                // GNOME answers Ctrl+Alt+Del with its log-out dialog.
                for (c, down) in [(29u32, true), (56, true), (111, true), (111, false), (56, false), (29, false)] {
                    notify!(self, "NotifyKeyboardKeycode", &(c, down))?;
                }
                Ok(())
            }
            Input::Key { keysym, mods } => {
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
        if let Some((p, _)) = self.pipe.take() {
            let _ = p.set_state(gst::State::Null);
        }
        let _ = self.session.call::<_, _, ()>("Stop", &());
    }
}
