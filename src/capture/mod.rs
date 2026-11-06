#[cfg(target_os = "linux")]
mod gnome;
mod synthetic;
#[cfg(windows)]
pub mod windows;

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

pub trait Capture {
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
