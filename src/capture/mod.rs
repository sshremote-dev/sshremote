#[cfg(target_os = "linux")]
mod gnome;
pub mod region;
mod synthetic;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
mod winlist;

/// A BGRx frame at the size requested with `set_target`.
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub data: Vec<u8>,
}

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

/// A top-level window that can be shared on its own.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    /// The program it belongs to, e.g. "chrome".
    pub app: String,
    pub w: u16,
    pub h: u16,
}

/// What part of the desktop is shared, as in a video call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Share {
    /// The whole monitor.
    Screen,
    /// One window, with its menus and dialogs.
    Window(u64),
    /// Every window of the program the given window belongs to.
    App(u64),
    /// Whichever window has the focus now.
    ActiveWindow,
}

impl Share {
    pub fn from_wire(kind: u8, id: u64) -> Option<Share> {
        Some(match kind {
            0 => Share::Screen,
            1 => Share::Window(id),
            2 => Share::App(id),
            3 => Share::ActiveWindow,
            _ => return None,
        })
    }
}

/// Shown in the window chooser when GNOME Shell lacks the desk helper; the viewer offers to install it.
pub const NEEDS_HELPER: &str = "The window list on GNOME needs the desk helper, a small GNOME Shell extension.";

pub trait Capture {
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
    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String>;
    /// Returns a frame when one arrived since the last call.
    fn grab(&mut self) -> Result<Option<Image>, String>;
    fn input(&mut self, ev: Input) -> Result<(), String>;
}

/// Points an ssh-started process at the desktop session's bus and runtime directory.
#[cfg(target_os = "linux")]
pub fn ensure_session_env() {
    gnome::ensure_env();
}

pub fn open(name: &str) -> Result<Box<dyn Capture>, String> {
    match name {
        "synthetic" => Ok(Box::new(synthetic::Synthetic::new())),
        #[cfg(target_os = "linux")]
        "gnome" | "auto" => Ok(Box::new(gnome::Gnome::open()?)),
        #[cfg(windows)]
        "windows" | "auto" => Ok(Box::new(windows::Gdi::open()?)),
        _ => Err(format!("capture backend '{name}' is not available on this system")),
    }
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
