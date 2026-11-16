//! The resolution of the host's screen that is shown, set from the viewer (as the display mode,
//! not the picture's size) and set back as it was when the session ends. On Windows through the
//! display settings, without saving them (signing out puts the saved ones back too); in X11
//! sessions through RandR; under Wayland through the desktop: GNOME's display configuration, KDE
//! Plasma's kscreen-doctor, Sway's swaymsg and Hyprland's hyprctl.

/// A screen's resolutions, largest first, and the one it has.
#[derive(Debug, Clone, PartialEq)]
pub struct Modes {
    pub list: Vec<(u16, u16)>,
    pub current: (u16, u16),
}

/// Smaller than this is no desktop to work in.
const SMALLEST: (u16, u16) = (640, 480);

/// The resolutions once each, largest first.
fn tidy(mut list: Vec<(u16, u16)>) -> Vec<(u16, u16)> {
    list.retain(|&(w, h)| w >= SMALLEST.0 && h >= SMALLEST.1);
    list.sort_by_key(|&(w, h)| std::cmp::Reverse((w as u32 * h as u32, w)));
    list.dedup();
    list
}

/// How screens are set here.
enum Way {
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "linux")]
    RandR,
    #[cfg(target_os = "linux")]
    Mutter,
    #[cfg(target_os = "linux")]
    Sway(std::path::PathBuf),
    #[cfg(target_os = "linux")]
    Hypr(String),
    #[cfg(target_os = "linux")]
    KScreen,
}

impl Way {
    /// For the capture backend named `backend`, where this desktop lets its screens be set.
    fn find(backend: &str) -> Option<Way> {
        match backend {
            #[cfg(windows)]
            "windows" => Some(Way::Windows),
            #[cfg(target_os = "linux")]
            "x11" => Some(Way::RandR),
            #[cfg(target_os = "linux")]
            "gnome" => Some(Way::Mutter),
            #[cfg(target_os = "linux")]
            "portal" | "portal-view" => linux::wayland(),
            _ => None,
        }
    }

    /// Every screen it knows, by name, that is on.
    fn screens(&self) -> Vec<String> {
        match self {
            #[cfg(windows)]
            Way::Windows => Vec::new(),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_screens(),
            #[cfg(target_os = "linux")]
            Way::Mutter => linux::mutter_screens(),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_outputs(sock).into_iter().map(|o| o.name).collect(),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_monitors(sig).into_iter().map(|o| o.name).collect(),
            #[cfg(target_os = "linux")]
            Way::KScreen => linux::kscreen_outputs().into_iter().map(|o| o.name).collect(),
        }
    }

    fn modes(&self, id: &str) -> Option<Modes> {
        let m = match self {
            #[cfg(windows)]
            Way::Windows => win::modes(id),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_modes(id),
            #[cfg(target_os = "linux")]
            Way::Mutter => linux::mutter_modes(id),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_outputs(sock).into_iter().find(|o| o.name == id).map(|o| o.modes()),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_monitors(sig).into_iter().find(|o| o.name == id).map(|o| o.modes()),
            #[cfg(target_os = "linux")]
            Way::KScreen => linux::kscreen_outputs().into_iter().find(|o| o.name == id).map(|o| o.modes()),
        }?;
        Some(Modes { list: tidy(m.list), current: m.current })
    }

    fn set(&self, id: &str, w: u16, h: u16) -> Result<(), String> {
        match self {
            #[cfg(windows)]
            Way::Windows => win::set(id, w, h),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_set(id, w, h),
            #[cfg(target_os = "linux")]
            Way::Mutter => linux::mutter_set(id, w, h),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_set(sock, id, w, h),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_set(sig, id, w, h),
            #[cfg(target_os = "linux")]
            Way::KScreen => linux::kscreen_set(id, w, h),
        }
    }
}

/// The screens a session set, set back as they were when it ends.
pub struct Session {
    way: Option<Way>,
    /// Each screen set, and the resolution it had before.
    own: Vec<(String, (u16, u16))>,
}

impl Session {
    pub fn new(backend: &str) -> Session {
        Session { way: Way::find(backend), own: Vec::new() }
    }

    /// The screen shown: as the capture names it, else the only one there is.
    pub fn screen(&self, named: Option<String>) -> Option<String> {
        let way = self.way.as_ref()?;
        named.or_else(|| {
            let all = way.screens();
            if all.len() == 1 { all.into_iter().next() } else { None }
        })
    }

    /// Screen `id`'s resolutions and the one it has, and its own (before this session set it).
    pub fn modes(&self, id: &str) -> Option<(Modes, (u16, u16))> {
        let m = self.way.as_ref()?.modes(id)?;
        let own = self.own.iter().find(|(s, _)| s == id).map_or(m.current, |(_, o)| *o);
        Some((m, own))
    }

    /// Sets screen `id` to `w`x`h`, one of its resolutions (0x0: its own again).
    pub fn set(&mut self, id: &str, w: u16, h: u16) -> Result<(), String> {
        let way = self.way.as_ref().ok_or("the resolution of this computer's screen cannot be set from here")?;
        let m = way.modes(id).ok_or("that screen's resolutions are not known")?;
        let own = self.own.iter().find(|(s, _)| s == id).map(|(_, o)| *o);
        let (w, h) = if (w, h) == (0, 0) { own.unwrap_or(m.current) } else { (w, h) };
        if (w, h) == m.current {
            return Ok(());
        }
        if !m.list.contains(&(w, h)) && Some((w, h)) != own {
            return Err(format!("that screen has no {w}x{h}"));
        }
        way.set(id, w, h)?;
        // A screen may say yes and keep its size (one in a window of another desktop): what it
        // has a moment later is what counts.
        let now = (0..10).find_map(|i| {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            way.modes(id).map(|m| m.current).filter(|c| *c == (w, h))
        });
        if now.is_none() {
            // Asked back, so nothing is left waiting to take it later.
            let _ = way.set(id, m.current.0, m.current.1);
            return Err(format!("the screen kept its resolution; {w}x{h} was not taken"));
        }
        if own.is_none() {
            self.own.push((id.to_string(), m.current));
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let Some(way) = self.way.as_ref() else { return };
        for (id, (w, h)) in self.own.drain(..) {
            if way.modes(&id).is_some_and(|m| m.current != (w, h)) {
                let _ = way.set(&id, w, h);
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use super::Modes;
    use windows_sys::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_BITSPERPEL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
        EnumDisplaySettingsExW,
    };

    fn device(id: &str) -> Vec<u16> {
        let full = if id.starts_with(r"\\.\") { id.to_string() } else { format!(r"\\.\{id}") };
        full.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn mode(dev: &[u16], i: u32) -> Option<DEVMODEW> {
        // SAFETY: a zeroed DEVMODEW with its size set, as the call expects.
        let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
        dm.dmSize = size_of::<DEVMODEW>() as u16;
        (unsafe { EnumDisplaySettingsExW(dev.as_ptr(), i, &mut dm, 0) } != 0).then_some(dm)
    }

    fn all(dev: &[u16]) -> Vec<DEVMODEW> {
        (0..).map_while(|i| mode(dev, i)).collect()
    }

    pub fn modes(id: &str) -> Option<Modes> {
        let dev = device(id);
        let cur = mode(&dev, ENUM_CURRENT_SETTINGS)?;
        let list = all(&dev).iter().filter(|m| m.dmBitsPerPel == cur.dmBitsPerPel).map(|m| (m.dmPelsWidth as u16, m.dmPelsHeight as u16)).collect();
        Some(Modes { list, current: (cur.dmPelsWidth as u16, cur.dmPelsHeight as u16) })
    }

    pub fn set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let dev = device(id);
        let cur = mode(&dev, ENUM_CURRENT_SETTINGS).ok_or("the screen's mode is not known")?;
        // That size at the rate in use, else at its highest.
        let mut dm = all(&dev)
            .into_iter()
            .filter(|m| (m.dmPelsWidth, m.dmPelsHeight, m.dmBitsPerPel) == (w as u32, h as u32, cur.dmBitsPerPel))
            .max_by_key(|m| (m.dmDisplayFrequency == cur.dmDisplayFrequency, m.dmDisplayFrequency))
            .ok_or_else(|| format!("the screen has no {w}x{h}"))?;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_BITSPERPEL | DM_DISPLAYFREQUENCY;
        // Not saved: what Windows keeps for the screen is not changed.
        let r = unsafe { ChangeDisplaySettingsExW(dev.as_ptr(), &dm, std::ptr::null_mut(), 0, std::ptr::null()) };
        if r == DISP_CHANGE_SUCCESSFUL { Ok(()) } else { Err(format!("Windows did not set {w}x{h} (code {r})")) }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::Value;
    use x11rb::connection::Connection as _;
    use x11rb::protocol::randr::{self, ConnectionExt as _};
    use x11rb::protocol::xproto::ConnectionExt as _;
    use zbus::zvariant::OwnedValue;

    use super::{Modes, Way};

    /// Under Wayland, with the screen-sharing portal: the desktop's own way, by what answers.
    pub(super) fn wayland() -> Option<Way> {
        let run = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_default();
        let newest = |dir: &Path, keep: &dyn Fn(&str) -> bool| {
            std::fs::read_dir(dir).ok()?.flatten().filter(|e| keep(&e.file_name().to_string_lossy())).max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()).map(|e| e.path())
        };
        if let Some(sock) = std::env::var_os("SWAYSOCK").map(PathBuf::from).or_else(|| newest(&run, &|n| n.starts_with("sway-ipc.") && n.ends_with(".sock")))
            && !sway_outputs(&sock).is_empty()
        {
            return Some(Way::Sway(sock));
        }
        if let Some(dir) = newest(&run.join("hypr"), &|_| true) {
            let sig = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !hypr_monitors(&sig).is_empty() {
                return Some(Way::Hypr(sig));
            }
        }
        (!kscreen_outputs().is_empty()).then_some(Way::KScreen)
    }

    fn json(cmd: &mut Command) -> Option<Value> {
        let out = cmd.output().ok().filter(|o| o.status.success())?;
        serde_json::from_slice(&out.stdout).ok()
    }

    /// An output as a desktop's own tool tells of it.
    pub(super) struct Output {
        pub name: String,
        pub current: (u16, u16),
        /// Each mode: its size, rate (Hz) and what the tool calls it.
        pub modes: Vec<((u16, u16), f64, String)>,
    }

    impl Output {
        pub fn modes(&self) -> Modes {
            Modes { list: self.modes.iter().map(|m| m.0).collect(), current: self.current }
        }

        /// The mode of that size at the rate nearest the one in use.
        fn pick(&self, w: u16, h: u16) -> Result<&((u16, u16), f64, String), String> {
            let rate = self.modes.iter().find(|m| m.0 == self.current).map_or(60.0, |m| m.1);
            self.modes
                .iter()
                .filter(|m| m.0 == (w, h))
                .min_by(|a, b| (a.1 - rate).abs().total_cmp(&(b.1 - rate).abs()))
                .ok_or_else(|| format!("{} has no {w}x{h}", self.name))
        }
    }

    /// Sizes offered for a screen that takes any (a virtual one, which lists no modes).
    const COMMON: [(u16, u16); 12] =
        [(3840, 2160), (2560, 1440), (1920, 1200), (1920, 1080), (1680, 1050), (1600, 900), (1440, 900), (1366, 768), (1280, 1024), (1280, 800), (1280, 720), (1024, 768)];

    /// The common sizes and the one it has, named as `name` says, for a screen without modes.
    fn any_size(current: (u16, u16), name: impl Fn(u16, u16) -> String) -> Vec<((u16, u16), f64, String)> {
        COMMON.iter().copied().chain([current]).map(|(w, h)| ((w, h), 60.0, name(w, h))).collect()
    }

    fn num(v: &Value) -> u16 {
        v.as_u64().unwrap_or(0).min(u16::MAX as u64) as u16
    }

    pub(super) fn sway_outputs(sock: &Path) -> Vec<Output> {
        json(Command::new("swaymsg").arg("-s").arg(sock).args(["-t", "get_outputs", "-r"])).map_or_else(Vec::new, |all| sway_parse(&all))
    }

    pub(super) fn sway_parse(all: &Value) -> Vec<Output> {
        all.as_array()
            .into_iter()
            .flatten()
            .filter(|o| o["active"].as_bool() == Some(true))
            .map(|o| {
                let current = (num(&o["current_mode"]["width"]), num(&o["current_mode"]["height"]));
                let mut modes: Vec<_> = o["modes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| {
                        let rate = m["refresh"].as_f64().unwrap_or(60000.0) / 1000.0;
                        let (w, h) = (num(&m["width"]), num(&m["height"]));
                        ((w, h), rate, format!("{w}x{h}@{rate:.3}Hz"))
                    })
                    .collect();
                if modes.is_empty() {
                    modes = any_size(current, |w, h| format!("--custom {w}x{h}"));
                }
                Output { name: o["name"].as_str().unwrap_or("").to_string(), current, modes }
            })
            .collect()
    }

    pub(super) fn sway_set(sock: &Path, id: &str, w: u16, h: u16) -> Result<(), String> {
        let out = sway_outputs(sock).into_iter().find(|o| o.name == id).ok_or("that screen is gone")?;
        let mode = &out.pick(w, h)?.2;
        let done = Command::new("swaymsg").arg("-s").arg(sock).arg(format!("output \"{id}\" mode {mode}")).output().map_err(|e| e.to_string())?;
        if done.status.success() { Ok(()) } else { Err(format!("Sway did not set {w}x{h}")) }
    }

    fn hyprctl(sig: &str) -> Command {
        let mut c = Command::new("hyprctl");
        c.env("HYPRLAND_INSTANCE_SIGNATURE", sig);
        c
    }

    /// Hyprland's monitors, with where each is and its scale (to set it again as it is).
    pub(super) fn hypr_monitors(sig: &str) -> Vec<Output> {
        hypr_raw(sig).into_iter().map(|(o, _)| o).collect()
    }

    fn hypr_raw(sig: &str) -> Vec<(Output, String)> {
        json(hyprctl(sig).args(["-j", "monitors"])).map_or_else(Vec::new, |all| hypr_parse(&all))
    }

    pub(super) fn hypr_parse(all: &Value) -> Vec<(Output, String)> {
        all.as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                let current = (num(&m["width"]), num(&m["height"]));
                let mut modes: Vec<_> = m["availableModes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| {
                        let s = s.as_str()?;
                        let (size, rate) = s.split_once('@')?;
                        let (w, h) = size.split_once('x')?;
                        Some(((w.parse().ok()?, h.parse().ok()?), rate.trim_end_matches("Hz").parse().unwrap_or(60.0), s.trim_end_matches("Hz").to_string()))
                    })
                    .collect();
                if modes.is_empty() {
                    modes = any_size(current, |w, h| format!("{w}x{h}"));
                }
                let place = format!("{}x{},{}", m["x"].as_i64().unwrap_or(0), m["y"].as_i64().unwrap_or(0), m["scale"].as_f64().unwrap_or(1.0));
                (Output { name: m["name"].as_str().unwrap_or("").to_string(), current, modes }, place)
            })
            .collect()
    }

    pub(super) fn hypr_set(sig: &str, id: &str, w: u16, h: u16) -> Result<(), String> {
        let all = hypr_raw(sig);
        let (out, place) = all.iter().find(|(o, _)| o.name == id).ok_or("that screen is gone")?;
        let mode = &out.pick(w, h)?.2;
        let done = hyprctl(sig).args(["keyword", "monitor", &format!("{id},{mode},{place}")]).output().map_err(|e| e.to_string())?;
        if done.status.success() && String::from_utf8_lossy(&done.stdout).trim() == "ok" { Ok(()) } else { Err(format!("Hyprland did not set {w}x{h}")) }
    }

    pub(super) fn kscreen_outputs() -> Vec<Output> {
        json(Command::new("kscreen-doctor").arg("-j")).map_or_else(Vec::new, |all| kscreen_parse(&all))
    }

    pub(super) fn kscreen_parse(all: &Value) -> Vec<Output> {
        all["outputs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|o| o["enabled"].as_bool() == Some(true))
            .map(|o| {
                let id = |v: &Value| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())).unwrap_or_default();
                let modes: Vec<((u16, u16), f64, String)> = o["modes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| ((num(&m["size"]["width"]), num(&m["size"]["height"])), m["refreshRate"].as_f64().unwrap_or(60.0), id(&m["id"])))
                    .collect();
                let current = id(&o["currentModeId"]);
                let current = modes.iter().find(|m| m.2 == current).map_or((num(&o["size"]["width"]), num(&o["size"]["height"])), |m| m.0);
                Output { name: o["name"].as_str().unwrap_or("").to_string(), current, modes }
            })
            .collect()
    }

    pub(super) fn kscreen_set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let out = kscreen_outputs().into_iter().find(|o| o.name == id).ok_or("that screen is gone")?;
        let mode = &out.pick(w, h)?.2;
        let done = Command::new("kscreen-doctor").arg(format!("output.{id}.mode.{mode}")).output().map_err(|e| e.to_string())?;
        if done.status.success() { Ok(()) } else { Err(format!("KDE did not set {w}x{h}")) }
    }

    fn x() -> Option<(x11rb::rust_connection::RustConnection, u32)> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        conn.randr_query_version(1, 3).ok()?.reply().ok()?;
        Some((conn, root))
    }

    /// The outputs that show something: name, output, its CRTC.
    fn randr_outputs(conn: &impl x11rb::connection::Connection, res: &randr::GetScreenResourcesCurrentReply) -> Vec<(String, randr::GetOutputInfoReply, u32)> {
        res.outputs
            .iter()
            .filter_map(|&o| {
                let info = conn.randr_get_output_info(o, res.config_timestamp).ok()?.reply().ok()?;
                let name = String::from_utf8_lossy(&info.name).into_owned();
                let crtc = info.crtc;
                (crtc != 0).then_some((name, info, crtc))
            })
            .collect()
    }

    pub(super) fn randr_screens() -> Vec<String> {
        let Some((conn, root)) = x() else { return Vec::new() };
        let Some(res) = conn.randr_get_screen_resources_current(root).ok().and_then(|c| c.reply().ok()) else { return Vec::new() };
        randr_outputs(&conn, &res).into_iter().map(|o| o.0).collect()
    }

    fn sideways(rotation: randr::Rotation) -> bool {
        rotation.contains(randr::Rotation::ROTATE90) || rotation.contains(randr::Rotation::ROTATE270)
    }

    fn rate(m: &randr::ModeInfo) -> f64 {
        let total = m.htotal as f64 * m.vtotal as f64;
        if total > 0.0 { m.dot_clock as f64 / total } else { 0.0 }
    }

    pub(super) fn randr_modes(id: &str) -> Option<Modes> {
        let (conn, root) = x()?;
        let res = conn.randr_get_screen_resources_current(root).ok()?.reply().ok()?;
        let (_, info, crtc) = randr_outputs(&conn, &res).into_iter().find(|o| o.0 == id)?;
        let c = conn.randr_get_crtc_info(crtc, res.config_timestamp).ok()?.reply().ok()?;
        let size = |m: &randr::ModeInfo| if sideways(c.rotation) { (m.height, m.width) } else { (m.width, m.height) };
        let list = info.modes.iter().filter_map(|id| res.modes.iter().find(|m| m.id == *id)).map(size).collect();
        Some(Modes { list, current: (c.width, c.height) })
    }

    pub(super) fn randr_set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let e = |e: &dyn std::fmt::Display| format!("X11: {e}");
        let (conn, root) = x().ok_or("no X display with RandR")?;
        let res = conn.randr_get_screen_resources_current(root).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        let outputs = randr_outputs(&conn, &res);
        let (_, info, crtc) = outputs.iter().find(|o| o.0 == id).ok_or("that screen is gone")?;
        let c = conn.randr_get_crtc_info(*crtc, res.config_timestamp).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        let side = sideways(c.rotation);
        let modes: Vec<&randr::ModeInfo> = info.modes.iter().filter_map(|m| res.modes.iter().find(|x| x.id == *m)).collect();
        let now = modes.iter().find(|m| m.id == c.mode).map_or(60.0, |m| rate(m));
        let mode = modes
            .iter()
            .filter(|m| if side { (m.height, m.width) == (w, h) } else { (m.width, m.height) == (w, h) })
            .min_by(|a, b| (rate(a) - now).abs().total_cmp(&(rate(b) - now).abs()))
            .ok_or_else(|| format!("that screen has no {w}x{h}"))?;
        // The X screen spans every output: as large as they are once this one has its new size.
        let mut right = c.x as i32 + w as i32;
        let mut bottom = c.y as i32 + h as i32;
        for (_, _, other) in outputs.iter().filter(|o| o.2 != *crtc) {
            if let Ok(o) = conn.randr_get_crtc_info(*other, res.config_timestamp).map_err(|x| e(&x)).and_then(|r| r.reply().map_err(|x| e(&x))) {
                right = right.max(o.x as i32 + o.width as i32);
                bottom = bottom.max(o.y as i32 + o.height as i32);
            }
        }
        let geo = conn.get_geometry(root).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        let screen = &conn.setup().roots.iter().find(|s| s.root == root).ok_or("no X screen")?.clone();
        let mm = |px: i32, old_px: u16, old_mm: u16| ((px as f64 * old_mm as f64 / old_px.max(1) as f64).round() as u32).max(1);
        let sized = |conn: &x11rb::rust_connection::RustConnection| {
            conn.randr_set_screen_size(root, right as u16, bottom as u16, mm(right, screen.width_in_pixels, screen.width_in_millimeters), mm(bottom, screen.height_in_pixels, screen.height_in_millimeters))
                .map_err(|x| e(&x))
                .and_then(|c| c.check().map_err(|x| e(&x)))
        };
        let grows = right > geo.width as i32 || bottom > geo.height as i32;
        if grows {
            sized(&conn)?;
        }
        let r = conn
            .randr_set_crtc_config(*crtc, x11rb::CURRENT_TIME, res.config_timestamp, c.x, c.y, mode.id, c.rotation, &c.outputs)
            .map_err(|x| e(&x))?
            .reply()
            .map_err(|x| e(&x))?;
        if r.status != randr::SetConfig::SUCCESS {
            return Err(format!("the X server did not set {w}x{h}"));
        }
        if !grows && (right != geo.width as i32 || bottom != geo.height as i32) {
            sized(&conn)?;
        }
        conn.flush().map_err(|x| e(&x))?;
        Ok(())
    }

    pub(super) type MutterMode = (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedValue>);
    pub(super) type MutterMonitor = ((String, String, String, String), Vec<MutterMode>, HashMap<String, OwnedValue>);
    pub(super) type MutterLogical = (i32, i32, f64, u32, bool, Vec<(String, String, String, String)>, HashMap<String, OwnedValue>);
    pub(super) type MutterState = (u32, Vec<MutterMonitor>, Vec<MutterLogical>, HashMap<String, OwnedValue>);

    fn mutter() -> Option<zbus::blocking::Proxy<'static>> {
        let conn = zbus::blocking::Connection::session().ok()?;
        zbus::blocking::Proxy::new(&conn, "org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig", "org.gnome.Mutter.DisplayConfig").ok()
    }

    fn mutter_state() -> Option<MutterState> {
        mutter()?.call("GetCurrentState", &()).ok()
    }

    fn flag(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
        props.get(key).is_some_and(|v| bool::try_from(v).unwrap_or(false))
    }

    pub(super) fn mutter_screens() -> Vec<String> {
        mutter_state().map(|s| s.2.iter().flat_map(|l| l.5.iter().map(|m| m.0.clone())).collect()).unwrap_or_default()
    }

    pub(super) fn mutter_modes(id: &str) -> Option<Modes> {
        let (_, monitors, ..) = mutter_state()?;
        let (_, modes, _) = monitors.into_iter().find(|m| m.0.0 == id)?;
        let current = modes.iter().find(|m| flag(&m.6, "is-current"))?;
        let current = (current.1 as u16, current.2 as u16);
        Some(Modes { list: modes.iter().map(|m| (m.1 as u16, m.2 as u16)).collect(), current })
    }

    /// A logical monitor as ApplyMonitorsConfig takes it: place, scale, turn, primary, and each
    /// monitor's connector and mode.
    pub(super) type Placed = (i32, i32, f64, u32, bool, Vec<(String, String, HashMap<String, OwnedValue>)>);

    /// The layout with monitor `id` at `w`x`h`, the ones beside or below it moved to keep touching
    /// it, and the properties to keep.
    pub(super) fn mutter_layout(state: &MutterState, id: &str, w: u16, h: u16) -> Result<(Vec<Placed>, HashMap<String, OwnedValue>), String> {
        let (_, monitors, logical, props) = state;
        let current_of = |c: &str| monitors.iter().find(|m| m.0.0 == c).and_then(|m| m.1.iter().find(|x| flag(&x.6, "is-current")).cloned());
        let (_, modes, _) = monitors.iter().find(|m| m.0.0 == id).ok_or("that screen is gone")?;
        let old = current_of(id).ok_or("that screen shows nothing")?;
        let new = modes
            .iter()
            .filter(|m| (m.1, m.2) == (w as i32, h as i32))
            .min_by(|a, b| (a.3 - old.3).abs().total_cmp(&(b.3 - old.3).abs()))
            .ok_or_else(|| format!("that screen has no {w}x{h}"))?;
        // In the logical layout a monitor takes its size divided by its scale.
        let physical = props.get("layout-mode").and_then(|v| u32::try_from(v).ok()) == Some(2);
        let own = logical.iter().find(|l| l.5.iter().any(|m| m.0 == id)).ok_or("that screen is not laid out")?;
        let scale = if new.5.iter().any(|s| (s - own.2).abs() < 1e-6) { own.2 } else { new.4 };
        let span = |px: i32, s: f64| if physical { px } else { (px as f64 / s).round() as i32 };
        let side = own.3 % 2 == 1;
        let (ow, oh) = if side { (old.2, old.1) } else { (old.1, old.2) };
        let (nw, nh) = if side { (new.2, new.1) } else { (new.1, new.2) };
        let (dx, dy) = (span(nw, scale) - span(ow, own.2), span(nh, scale) - span(oh, own.2));
        let (right, bottom) = (own.0 + span(ow, own.2), own.1 + span(oh, own.2));
        let layout = logical
            .iter()
            .map(|l| {
                let mine = l.5.iter().any(|m| m.0 == id);
                let x = if !mine && l.0 >= right { l.0 + dx } else { l.0 };
                let y = if !mine && l.1 >= bottom { l.1 + dy } else { l.1 };
                let monitors = l.5.iter().filter_map(|m| Some((m.0.clone(), if m.0 == id { new.0.clone() } else { current_of(&m.0)?.0 }, HashMap::new()))).collect();
                (x, y, if mine { scale } else { l.2 }, l.3, l.4, monitors)
            })
            .collect();
        let mut keep: HashMap<String, OwnedValue> = HashMap::new();
        if let Some(mode) = props.get("layout-mode").and_then(|v| v.try_clone().ok()) {
            keep.insert("layout-mode".into(), mode);
        }
        Ok((layout, keep))
    }

    pub(super) fn mutter_set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let state = mutter_state().ok_or("GNOME's display configuration does not answer")?;
        let (layout, keep) = mutter_layout(&state, id, w, h)?;
        // Temporary (1): not saved, so the saved layout comes back with the next login.
        let r: Result<(), zbus::Error> = mutter().ok_or("GNOME's display configuration does not answer")?.call("ApplyMonitorsConfig", &(state.0, 1u32, layout, keep));
        r.map_err(|e| format!("GNOME did not set {w}x{h}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_are_listed_once_largest_first() {
        assert_eq!(tidy(vec![(1280, 720), (1920, 1080), (1280, 720), (800, 600), (320, 200), (1280, 1024)]), vec![(1920, 1080), (1280, 1024), (1280, 720), (800, 600)]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_desktops_tools_are_read() {
        use serde_json::json;
        let sway = linux::sway_parse(&json!([
            {"name": "DP-1", "active": true, "current_mode": {"width": 2560, "height": 1440, "refresh": 59951},
             "modes": [{"width": 2560, "height": 1440, "refresh": 59951}, {"width": 1920, "height": 1080, "refresh": 60000}, {"width": 1920, "height": 1080, "refresh": 50000}]},
            {"name": "HEADLESS-1", "active": true, "current_mode": {"width": 1280, "height": 800, "refresh": 0}, "modes": []},
            {"name": "HDMI-A-1", "active": false, "modes": []}]));
        assert_eq!(sway.len(), 2);
        assert_eq!((sway[0].name.as_str(), sway[0].current, sway[0].modes().list.len()), ("DP-1", (2560, 1440), 3));
        assert!(sway[1].modes.iter().any(|m| m.2 == "--custom 1920x1080"), "a virtual screen takes any size");
        let hypr = linux::hypr_parse(&json!([{"name": "eDP-1", "width": 1920, "height": 1200, "x": 0, "y": 0, "scale": 1.25, "availableModes": ["1920x1200@60.00Hz", "1920x1200@48.00Hz", "1280x800@60.00Hz"]}]));
        assert_eq!(hypr[0].1, "0x0,1.25");
        assert_eq!(hypr[0].0.modes().list, vec![(1920, 1200), (1920, 1200), (1280, 800)]);
        let kde = linux::kscreen_parse(&json!({"outputs": [{"name": "Virtual-0", "enabled": true, "currentModeId": "2", "size": {"width": 1280, "height": 800},
            "modes": [{"id": "1", "refreshRate": 60, "size": {"width": 1920, "height": 1080}}, {"id": "2", "refreshRate": 60, "size": {"width": 1280, "height": 800}}]}]}));
        assert_eq!((kde[0].current, kde[0].modes().list.len()), ((1280, 800), 2));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gnome_moves_the_screen_beside_one_that_grows() {
        use std::collections::HashMap;
        use zbus::zvariant::{OwnedValue, Value};
        let yes = || -> HashMap<String, OwnedValue> { HashMap::from([("is-current".to_string(), OwnedValue::try_from(Value::from(true)).unwrap())]) };
        let mode = |id: &str, w, h, current: bool| (id.to_string(), w, h, 60.0, 1.0, vec![1.0, 2.0], if current { yes() } else { HashMap::new() });
        let spec = |c: &str| (c.to_string(), "v".to_string(), "p".to_string(), "s".to_string());
        let state: linux::MutterState = (
            7,
            vec![
                (spec("DP-1"), vec![mode("a", 1920, 1080, false), mode("b", 1280, 720, true)], HashMap::new()),
                (spec("DP-2"), vec![mode("c", 1920, 1080, true)], HashMap::new()),
            ],
            vec![(0, 0, 1.0, 0, true, vec![spec("DP-1")], HashMap::new()), (1280, 0, 1.0, 0, false, vec![spec("DP-2")], HashMap::new())],
            HashMap::new(),
        );
        let (layout, _) = linux::mutter_layout(&state, "DP-1", 1920, 1080).unwrap();
        assert_eq!((layout[0].0, layout[0].5[0].1.as_str()), (0, "a"));
        assert_eq!((layout[1].0, layout[1].5[0].1.as_str()), (1920, "c"), "the one on its right moves along");
        assert!(linux::mutter_layout(&state, "DP-1", 800, 600).is_err());
    }

    /// This computer's screens and their resolutions, nothing set: cargo test -- --ignored --nocapture screens_here
    #[test]
    #[ignore]
    fn screens_here_are_listed() {
        #[cfg(windows)]
        let (way, ids) = (Way::Windows, (1..=4).map(|i| format!("DISPLAY{i}")).collect::<Vec<_>>());
        #[cfg(target_os = "linux")]
        let way = if std::env::var_os("WAYLAND_DISPLAY").is_some() { linux::wayland().unwrap_or(Way::Mutter) } else { Way::RandR };
        #[cfg(target_os = "linux")]
        let ids = way.screens();
        for id in ids {
            if let Some(m) = way.modes(&id) {
                println!("{id}: now {}x{}, {} resolutions: {:?}", m.current.0, m.current.1, tidy(m.list.clone()).len(), tidy(m.list));
            }
        }
    }

    /// The first screen set to another resolution for a few seconds, then set back as the
    /// session ends: cargo test -- --ignored --nocapture screen_here_is_set
    #[test]
    #[ignore]
    fn screen_here_is_set_and_set_back() {
        #[cfg(windows)]
        let mut s = Session::new("windows");
        #[cfg(target_os = "linux")]
        let mut s = Session::new(if std::env::var_os("WAYLAND_DISPLAY").is_some() { "portal" } else { "x11" });
        let id = s.screen(if cfg!(windows) { Some("DISPLAY1".into()) } else { None }).expect("a screen");
        let (m, own) = s.modes(&id).expect("its resolutions");
        let want = [(1280, 720), (1024, 768)].into_iter().chain(m.list.iter().copied()).find(|r| m.list.contains(r) && *r != own).expect("another resolution");
        println!("{id}: {}x{}, set to {}x{}", own.0, own.1, want.0, want.1);
        s.set(&id, want.0, want.1).expect("set");
        println!("now {:?}", s.modes(&id).map(|m| m.0.current));
        std::thread::sleep(std::time::Duration::from_secs(3));
        drop(s);
        let back = Way::find(if cfg!(windows) { "windows" } else { "x11" }).and_then(|w| w.modes(&id)).map(|m| m.current);
        println!("after the session: {back:?}");
        assert_eq!(back, Some(own));
    }

    #[test]
    fn a_session_where_screens_cannot_be_set_says_so() {
        let mut s = Session::new("browser");
        assert_eq!(s.screen(Some("x".into())), None);
        assert!(s.set("x", 800, 600).is_err());
    }
}
