mod browser;
#[cfg(windows)]
mod dxgi;
#[cfg(target_os = "linux")]
mod gnome;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
mod wlinput;
pub mod region;
pub use crate::shared::{Image, Share, WindowInfo};
mod synthetic;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub(crate) mod winlist;

/// Input in desktop pixel coordinates; keys are X11 keysyms.
#[derive(Debug)]
pub enum Input {
    Key { keysym: u32, mods: u8 },
    KeyState { keysym: u32, down: bool },
    Move { x: f64, y: f64 },
    Button { button: u8, pressed: bool },
    Scroll { steps: i32 },
    HScroll { steps: i32 },
    SecureAttention,
}

/// One of the host's monitors, in its native pixels.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: String,
    pub w: usize,
    pub h: usize,
    pub primary: bool,
}

/// What a click outside the shared window or program answers, so the viewer can tell the way out.
pub const OUTSIDE: &str = "That is outside the shared window. To reach other windows, share the whole screen (the Share button, or Ctrl+] v in a terminal).";

/// Shown in the window chooser when GNOME Shell lacks the sshremote helper; the viewer offers to install it.
pub const NEEDS_HELPER: &str =
    "Picking from a list on GNOME needs the sshremote helper, a small GNOME Shell extension.";

pub trait Capture {
    /// Opens the desktop's own "choose what to share" dialog on the remote screen; returns what to
    /// tell the viewer. The choice arrives later through `poll_share`.
    fn pick(&mut self) -> Result<String, String> {
        Err("this desktop has no sharing dialog of its own; pick from the list".into())
    }
    /// A change of what is shared that the viewer did not ask for: the outcome of `pick`, or the
    /// shared window having closed. `kind` as in `Share`.
    fn poll_share(&mut self) -> Option<Result<(u8, String), String>> {
        None
    }
    /// Installs what listing windows needs (the GNOME Shell helper); returns what to tell the viewer.
    fn install_helper(&mut self) -> Result<String, String> {
        Err("nothing to install for this desktop".into())
    }
    /// The windows that could be shared on their own.
    fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        Err("This desktop can only share whole screens.".into())
    }
    /// Shares a window or a program instead of the whole screen; returns what is shown, in words.
    fn share(&mut self, scope: Share) -> Result<String, String> {
        match scope {
            Share::Screen => Ok("the whole screen".into()),
            _ => Err("This desktop can only share whole screens.".into()),
        }
    }
    fn name(&self) -> &'static str;
    /// The host's monitors, when the backend can tell them apart.
    fn monitors(&self) -> Vec<Monitor> {
        Vec::new()
    }
    /// Index of the monitor being captured.
    fn current(&self) -> usize {
        0
    }
    /// Captures another monitor; the desktop size changes with it.
    fn select(&mut self, _index: usize) -> Result<(), String> {
        Err("this capture backend has one screen".into())
    }
    fn desktop_size(&self) -> (usize, usize);
    /// Whether the screen takes the size it is shown at (a web page, laid out again), rather
    /// than being scaled to it.
    fn resizable(&self) -> bool {
        false
    }
    /// Where the captured area starts on the virtual desktop, to place accessibility rectangles.
    fn origin(&self) -> (i32, i32) {
        (0, 0)
    }
    /// The shared window, if one is shared on its own (an id as in `WindowInfo`).
    fn shared_window(&self) -> Option<u64> {
        None
    }
    /// The shared program's file, if a whole program is shared (Windows).
    fn shared_app(&self) -> Option<String> {
        None
    }
    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String>;
    /// The viewer's pixels to a point (2 on a HiDPI screen): a page lays itself out by it.
    fn set_density(&mut self, _d: f64) {}
    /// Returns a frame when one arrived since the last call.
    fn grab(&mut self) -> Result<Option<Image>, String>;
    fn input(&mut self, ev: Input) -> Result<(), String>;
}

/// Points an ssh-started process at the desktop session's bus and runtime directory.
#[cfg(target_os = "linux")]
pub fn ensure_session_env() {
    gnome::ensure_env();
}

/// No screen: what a session has when this computer's screen cannot be shared (a desktop with
/// no way in yet, nobody signed in): its terminals, files, clipboard and chat still work.
pub struct NoScreen(pub String);

impl Capture for NoScreen {
    fn name(&self) -> &'static str {
        "none"
    }

    fn desktop_size(&self) -> (usize, usize) {
        (1280, 720)
    }

    fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        Err(self.0.clone())
    }

    fn set_target(&mut self, _w: usize, _h: usize) -> Result<(), String> {
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        Ok(None)
    }

    fn input(&mut self, _ev: Input) -> Result<(), String> {
        Ok(())
    }
}

/// The capture for the desktop this session is signed in to: GNOME's own way in, any X11
/// desktop's through X itself.
#[cfg(target_os = "linux")]
fn linux_desktop() -> Result<Box<dyn Capture>, String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
    if desktop.to_ascii_lowercase().contains("gnome") || desktop.is_empty() && wayland {
        match gnome::Gnome::open() {
            Ok(g) => return Ok(Box::new(g)),
            // GNOME on Xorg: X11's own way in.
            Err(e) if wayland => return Err(e),
            Err(_) => {}
        }
    }
    if wayland {
        // Through the desktop's portal (X11 here would see only the programs under Xwayland).
        let name = if desktop.is_empty() { "this desktop".to_string() } else { desktop.replace(':', " / ") };
        return wayland::Wayland::open().map(|w| Box::new(w) as Box<dyn Capture>).map_err(|e| format!("{name} did not share its screen through its portal: {e}"));
    }
    Ok(Box::new(x11::X11::open()?))
}

pub fn open(name: &str) -> Result<Box<dyn Capture>, String> {
    match name {
        "synthetic" => Ok(Box::new(synthetic::Synthetic::new())),
        // A web app in a pane: a page of a browser run with no window.
        n if n.starts_with("browser:") => Ok(Box::new(browser::Browser::open(&n["browser:".len()..])?)),
        "synthetic2" => Ok(Box::new(synthetic::Synthetic::with_screens(2))),
        #[cfg(target_os = "linux")]
        "gnome" => Ok(Box::new(gnome::Gnome::open()?)),
        #[cfg(target_os = "linux")]
        "x11" => Ok(Box::new(x11::X11::open()?)),
        #[cfg(target_os = "linux")]
        "portal" => Ok(Box::new(wayland::Wayland::open()?)),
        #[cfg(target_os = "linux")]
        "auto" => linux_desktop(),
        #[cfg(windows)]
        "windows" | "auto" => Ok(Box::new(windows::Gdi::open()?)),
        _ => Err(format!("capture backend '{name}' is not available on this system")),
    }
}

/// A frame's pixels where they lie (a mapped capture buffer, say), BGRx rows `stride` apart.
pub struct Pixels<'a> {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub data: &'a [u8],
}

/// Nearest-neighbour BGRx resize for backends that cannot scale themselves.
pub fn scale_nearest(src: &[u8], sw: usize, sh: usize, sstride: usize, w: usize, h: usize) -> Image {
    let mut data = vec![0u8; w * h * 4];
    let xs: Vec<usize> = (0..w).map(|x| (x * sw / w) * 4).collect();
    for y in 0..h {
        let srow = &src[(y * sh / h) * sstride..];
        let drow = &mut data[y * w * 4..(y + 1) * w * 4];
        for (x, &sx) in xs.iter().enumerate() {
            drow[x * 4..x * 4 + 4].copy_from_slice(&srow[sx..sx + 4]);
        }
    }
    Image { w, h, stride: w * 4, data }
}
