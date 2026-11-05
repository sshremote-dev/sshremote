#[cfg(target_os = "linux")]
mod gnome;
mod synthetic;

/// A BGRx frame at the size requested with `set_target`.
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub data: Vec<u8>,
}

/// Input in desktop pixel coordinates; keys are X11 keysyms.
#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum Input {
    Key { keysym: u32, mods: u8 },
    Move { x: f64, y: f64 },
    Button { button: u8, pressed: bool },
    Scroll { steps: i32 },
}

pub trait Capture {
    fn name(&self) -> &'static str;
    fn desktop_size(&self) -> (usize, usize);
    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String>;
    /// Returns a frame when one arrived since the last call.
    fn grab(&mut self) -> Result<Option<Image>, String>;
    fn input(&mut self, ev: Input) -> Result<(), String>;
}

pub fn open(name: &str) -> Result<Box<dyn Capture>, String> {
    match name {
        "synthetic" => Ok(Box::new(synthetic::Synthetic::new())),
        #[cfg(target_os = "linux")]
        "gnome" | "auto" => Ok(Box::new(gnome::Gnome::open()?)),
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
