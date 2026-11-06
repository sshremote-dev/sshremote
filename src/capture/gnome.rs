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

use super::{Capture, Image, Input, Monitor};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

macro_rules! notify {
    ($s:expr, $method:literal, $body:expr) => {
        $s.session.call::<_, _, ()>($method, $body).map_err(|e| format!("GNOME {}: {e}", $method))
    };
}

const RD: &str = "org.gnome.Mutter.RemoteDesktop";
const SC: &str = "org.gnome.Mutter.ScreenCast";

pub struct Gnome {
    monitors: Vec<(String, Monitor)>,
    current: usize,
    _conn: Connection,
    session: Proxy<'static>,
    stream_path: String,
    node: u32,
    desk: (usize, usize),
    target: (usize, usize),
    pipe: Option<(gst::Pipeline, gst_app::AppSink)>,
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

type ModeT = (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedValue>);
type MonitorT = ((String, String, String, String), Vec<ModeT>, HashMap<String, OwnedValue>);
type LogicalT = (i32, i32, f64, u32, bool, Vec<(String, String, String, String)>, HashMap<String, OwnedValue>);

/// Connectors and current modes of the monitors, primary first, from Mutter's DisplayConfig.
fn monitors(conn: &Connection) -> Vec<(String, Monitor)> {
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
    let mut out: Vec<(String, Monitor)> = mons
        .into_iter()
        .filter_map(|((connector, vendor, product, _), modes, _)| {
            let current =
                modes.iter().find(|m| m.6.get("is-current").is_some_and(|v| bool::try_from(v).unwrap_or(false)))?;
            let name = format!("{connector} {vendor} {product}").trim().to_string();
            let primary = primary.contains(&connector);
            Some((connector, Monitor { name, w: current.1 as usize, h: current.2 as usize, primary }))
        })
        .collect();
    out.sort_by_key(|m| !m.1.primary);
    out
}

impl Gnome {
    pub fn open() -> Result<Gnome, String> {
        Gnome::open_on(None)
    }

    /// Records the monitor at `index` in the monitor list, or the primary one.
    fn open_on(index: Option<usize>) -> Result<Gnome, String> {
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
        let stream: OwnedObjectPath =
            cast.call("RecordMonitor", &(connector.as_str(), sprops)).map_err(err("RecordMonitor"))?;
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
            _conn: conn,
            session,
            stream_path,
            node,
            desk,
            target: desk,
            pipe: Some((p, sink)),
        })
    }
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

    fn current(&self) -> usize {
        self.current
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.monitors.len() {
            return Err("no such monitor".into());
        }
        // A new remote-desktop session records the other monitor; dropping the old one stops it.
        *self = Gnome::open_on(Some(index))?;
        Ok(())
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        let t = (w.min(self.desk.0), h.min(self.desk.1));
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
        match sink.try_pull_sample(gst::ClockTime::ZERO) {
            Some(s) => to_image(&s).map(Some),
            None => Ok(None),
        }
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        match ev {
            Input::Move { x, y } => {
                let x = x.clamp(0.0, self.desk.0 as f64 - 1.0);
                let y = y.clamp(0.0, self.desk.1 as f64 - 1.0);
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
