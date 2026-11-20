mod browser;
pub mod camera;
#[cfg(windows)]
mod dxgi;
#[cfg(target_os = "linux")]
mod gnome;

/// The GNOME helper extension brought up to date where it is installed (GNOME loads it at the
/// next login).
#[cfg(target_os = "linux")]
pub(crate) fn gnome_helper_update() {
    gnome::update_helper();
}
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "linux")]
mod pw;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
mod wlinput;
#[cfg(target_os = "linux")]
mod wlrcopy;
pub mod region;
mod shrink;
pub use shrink::Shrink;
pub use crate::shared::{Image, Share, WindowInfo};
mod synthetic;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub(crate) mod winlist;

/// What a backend's own thread calls as a picture or the pointer changes (see
/// `Capture::set_waker`): empty until the agent sets it.
#[allow(dead_code)]
pub type Waker = std::sync::Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send>>>>;

#[allow(dead_code)]
fn wake(w: &Waker) {
    if let Some(w) = w.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        w();
    }
}

/// The pointer's picture, RGBA not premultiplied, and the pixel of it that points.
pub struct Pointer {
    pub w: usize,
    pub h: usize,
    pub hot: (usize, usize),
    pub rgba: Vec<u8>,
}

/// Input in desktop pixel coordinates; keys are X11 keysyms.
#[derive(Debug)]
pub enum Input {
    Key { keysym: u32, mods: u8 },
    KeyState { keysym: u32, down: bool },
    Move { x: f64, y: f64 },
    /// Moved by this much, in the host's pixels (as a mouse moves, not to a place).
    MoveBy { dx: i32, dy: i32 },
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
    /// The system's name for the monitor shown (`DISPLAY1`, an output such as `DP-1`), by which
    /// its resolution is set (`display`); None where the backend cannot tell.
    fn screen_id(&self) -> Option<String> {
        None
    }
    /// The monitor shown is about to get another resolution (`screen_changed` follows).
    fn screen_changing(&mut self) {}
    /// The monitor shown was given another resolution (or was not after all): it is looked at again.
    fn screen_changed(&mut self) {}
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
    /// How many pictures a second the viewer takes: a backend that copies by itself copies no more.
    fn set_fps(&mut self, _fps: u8) {}
    /// `wake` is called whenever a new picture is ready for `grab`; false where the backend does
    /// not (it is looked at often instead).
    fn set_waker(&mut self, _wake: Box<dyn Fn() + Send>) -> bool {
        false
    }
    /// Whether the waker given to `set_waker` is called now: not while this can only be looked at
    /// (a camera shown, GDI standing in for duplication, a watch that ended), when the agent
    /// looks every millisecond again.
    fn wakes(&self) -> bool {
        true
    }
    /// A word for the viewer when something keeps the picture from coming (a locked screen this
    /// program may not read), once each time.
    fn notice(&mut self) -> Option<String> {
        None
    }
    /// Returns a frame when one arrived since the last call.
    fn grab(&mut self) -> Result<Option<Image>, String>;
    /// A picture `grab` gave that is done with: its buffer may hold a later one.
    fn recycle(&mut self, _img: Image) {}
    /// The viewer draws the pointer itself: the pictures leave it out from now on, or put it
    /// back. False where the backend cannot keep it apart (it stays in the pictures).
    fn pointer_apart(&mut self, _on: bool) -> bool {
        false
    }
    /// While the pointer is kept apart: its new shape if it changed since the last look, and
    /// where it is on the captured area, in its pixels (None while it is hidden).
    fn pointer(&mut self) -> (Option<Pointer>, Option<(i32, i32)>) {
        (None, None)
    }
    fn input(&mut self, ev: Input) -> Result<(), String>;

    /// Puts `text` into the focused text field as text, where keys cannot type it; false where
    /// this desktop cannot (the agent pastes it then).
    fn commit_text(&mut self, _text: &str) -> bool {
        false
    }
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
    // Started by the service or over ssh: the desktop's own variables first.
    gnome::ensure_env();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
    if desktop.to_ascii_lowercase().contains("gnome") || desktop.is_empty() && wayland {
        match gnome::Gnome::open() {
            Ok(g) => return Ok(Box::new(g)),
            // GNOME on Xorg: X11's own way in; a desktop that did not say which it is: its portal.
            Err(e) if wayland && !desktop.is_empty() => return Err(e),
            Err(_) => {}
        }
    }
    // wlroots and its kin (Hyprland, Sway...) copy their screen for the session's own programs:
    // no dialog for someone at the screen to answer. COSMIC's is left to its portal until tried.
    if wayland
        && !desktop.to_ascii_lowercase().contains("cosmic")
        && let Ok(w) = wlrcopy::WlrCopy::open()
    {
        return Ok(Box::new(w));
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
        "wlroots" => Ok(Box::new(wlrcopy::WlrCopy::open()?)),
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

