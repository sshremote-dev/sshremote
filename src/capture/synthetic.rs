//! Deterministic test desktop: text-like rows, a ticking progress bar, a pointer
//! and typed characters. Lets the whole pipeline run without a real desktop. Opened as
//! "synthetic2" it has two screens, the second with a bar of another colour.

use std::time::Instant;

use crate::xk;
use super::{Capture, Image, Input};

const W: usize = 1280;
const H: usize = 720;

pub struct Synthetic {
    target: (usize, usize),
    /// The picture made smaller for the viewer, by area averaging.
    shrink: super::Shrink,
    start: Instant,
    tick: u64,
    pointer: (usize, usize),
    /// The viewer draws the pointer (its shape sent once).
    apart: bool,
    shape_sent: bool,
    typed: Vec<u32>,
    clicks: u32,
    scrolled: usize,
    /// Frames of the moving picture started by typing `v`, like a playing video.
    playing: Option<u32>,
    dirty: bool,
    /// How many screens it has, and the one shown.
    screens: usize,
    screen: usize,
}

impl Synthetic {
    pub fn new() -> Synthetic {
        Synthetic {
            target: (W, H),
            shrink: super::Shrink::default(),
            start: Instant::now(),
            tick: u64::MAX,
            pointer: (W / 2, H / 2),
            apart: false,
            shape_sent: false,
            typed: Vec::new(),
            clicks: 0,
            scrolled: 0,
            playing: None,
            dirty: true,
            screens: 1,
            screen: 0,
        }
    }

    /// The test desktop with `n` screens.
    pub fn with_screens(n: usize) -> Synthetic {
        Synthetic { screens: n.max(1), ..Synthetic::new() }
    }

    fn render(&self) -> Vec<u8> {
        let mut px = vec![0u8; W * H * 4];
        let mut fill = |x0: usize, y0: usize, w: usize, h: usize, rgb: [u8; 3]| {
            for y in y0.min(H)..(y0 + h).min(H) {
                for x in x0.min(W)..(x0 + w).min(W) {
                    px[(y * W + x) * 4..(y * W + x) * 4 + 4].copy_from_slice(&[rgb[2], rgb[1], rgb[0], 255]);
                }
            }
        };
        fill(0, 0, W, H, [40, 44, 52]);
        let bar = match (self.screen, self.clicks.is_multiple_of(2)) {
            (0, true) => [60, 64, 72],
            (0, false) => [30, 90, 160],
            _ => [150, 60, 40],
        };
        fill(0, 0, W, 40, bar);
        for row in 0..24 {
            let y = 60 + row * 22;
            let line = row + self.scrolled;
            let mut x = 20 + (line % 3) * 30;
            let mut seed = (line as u32).wrapping_mul(2654435761);
            while x < 760 {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                let word = 2 + (seed >> 16) as usize % 7;
                for _ in 0..word {
                    fill(x + 1, y + 3, 7, 13, [200, 200, 200]);
                    x += 10;
                }
                x += 10;
            }
        }
        let progress = (self.tick % 40) as usize * 10;
        fill(820, 80, 400, 24, [70, 74, 82]);
        fill(820, 80, progress, 24, [97, 175, 239]);
        for (i, _) in self.typed.iter().enumerate() {
            let (cx, cy) = (820 + (i % 40) * 10, 140 + (i / 40) * 22);
            fill(cx + 1, cy + 3, 7, 13, [229, 192, 123]);
        }
        if let Some(f) = self.playing {
            for y in 0..240 {
                for x in 0..320 {
                    let (a, b) = (x as u32 + f * 3, y as u32 + f * 2);
                    let n = (a.wrapping_mul(2_654_435_761) ^ b.wrapping_mul(40_503)) >> 27;
                    let rgb = [((a ^ b) & 255) as u8, ((a * 2 + b) & 255) as u8, (n * 8) as u8];
                    fill(860 + x, 260 + y, 1, 1, rgb);
                }
            }
        }
        if !self.apart {
            let (px0, py0) = self.pointer;
            fill(px0.saturating_sub(1), py0.saturating_sub(1), 12, 12, [0, 0, 0]);
            fill(px0, py0, 10, 10, [255, 255, 255]);
        }
        px
    }
}

impl Capture for Synthetic {
    fn name(&self) -> &'static str {
        "synthetic"
    }

    fn desktop_size(&self) -> (usize, usize) {
        (W, H)
    }

    fn monitors(&self) -> Vec<super::Monitor> {
        if self.screens < 2 {
            return Vec::new();
        }
        (0..self.screens).map(|i| super::Monitor { name: format!("Synthetic {}", i + 1), w: W, h: H, primary: i == 0 }).collect()
    }

    fn current(&self) -> usize {
        self.screen
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.screens {
            return Err("no such screen".into());
        }
        self.screen = index;
        self.dirty = true;
        Ok(())
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.clamp(16, W), h.clamp(16, H));
        self.dirty = true;
        Ok(())
    }

    fn pointer_apart(&mut self, on: bool) -> bool {
        (self.apart, self.shape_sent, self.dirty) = (on, false, true);
        true
    }

    fn pointer(&mut self) -> (Option<super::Pointer>, Option<(i32, i32)>) {
        // The same square as drawn in the picture otherwise: white in a black rim.
        let shape = (!std::mem::replace(&mut self.shape_sent, true)).then(|| {
            let rgba = (0..144).flat_map(|i| if (1..11).contains(&(i % 12)) && (1..11).contains(&(i / 12)) { [255, 255, 255, 255] } else { [0, 0, 0, 255] }).collect();
            super::Pointer { w: 12, h: 12, hot: (1, 1), rgba }
        });
        (shape, Some((self.pointer.0 as i32, self.pointer.1 as i32)))
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        let tick = self.start.elapsed().as_millis() as u64 / 500;
        if tick == self.tick && !self.dirty && self.playing.is_none() {
            return Ok(None);
        }
        if let Some(f) = self.playing.as_mut() {
            *f += 1;
        }
        self.tick = tick;
        self.dirty = false;
        let (w, h) = self.target;
        Ok(Some(self.shrink.image(&self.render(), W, H, W * 4, w, h)))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        match ev {
            Input::Move { x, y } => self.pointer = ((x as usize).min(W - 1), (y as usize).min(H - 1)),
            Input::Button { pressed: true, .. } => self.clicks += 1,
            Input::Key { keysym: xk::BACKSPACE, .. } => {
                self.typed.pop();
            }
            Input::Key { keysym: 0x76, .. } => self.playing = if self.playing.is_some() { None } else { Some(0) },
            Input::Key { keysym, .. } => self.typed.push(keysym),
            Input::Scroll { steps } => self.scrolled = self.scrolled.saturating_add_signed(steps as isize),
            Input::KeyState { keysym, down: true } => self.typed.push(keysym),
            _ => return Ok(()),
        }
        self.dirty = true;
        Ok(())
    }
}
