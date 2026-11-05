//! Deterministic test desktop: text-like rows, a ticking progress bar, a pointer
//! and typed characters. Lets the whole pipeline run without a real desktop.

use std::time::Instant;

use super::{Capture, Image, Input, scale_nearest};

const W: usize = 1280;
const H: usize = 720;

pub struct Synthetic {
    target: (usize, usize),
    start: Instant,
    tick: u64,
    pointer: (usize, usize),
    typed: Vec<u32>,
    clicks: u32,
    scrolled: usize,
    dirty: bool,
}

impl Synthetic {
    pub fn new() -> Synthetic {
        Synthetic {
            target: (W, H),
            start: Instant::now(),
            tick: u64::MAX,
            pointer: (W / 2, H / 2),
            typed: Vec::new(),
            clicks: 0,
            scrolled: 0,
            dirty: true,
        }
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
        let bar = if self.clicks % 2 == 0 { [60, 64, 72] } else { [30, 90, 160] };
        fill(0, 0, W, 40, bar);
        for row in 0..24 {
            let y = 60 + row * 22;
            let line = row + self.scrolled;
            let mut x = 20 + (line % 3) * 30;
            let mut seed = line as u32 * 2654435761;
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
        let (px0, py0) = self.pointer;
        fill(px0.saturating_sub(1), py0.saturating_sub(1), 12, 12, [0, 0, 0]);
        fill(px0, py0, 10, 10, [255, 255, 255]);
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

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.clamp(16, W), h.clamp(16, H));
        self.dirty = true;
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        let tick = self.start.elapsed().as_millis() as u64 / 500;
        if tick == self.tick && !self.dirty {
            return Ok(None);
        }
        self.tick = tick;
        self.dirty = false;
        let (w, h) = self.target;
        Ok(Some(scale_nearest(&self.render(), W, H, W * 4, w, h)))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        match ev {
            Input::Move { x, y } => self.pointer = ((x as usize).min(W - 1), (y as usize).min(H - 1)),
            Input::Button { pressed: true, .. } => self.clicks += 1,
            Input::Key { keysym: 0xff08, .. } => {
                self.typed.pop();
            }
            Input::Key { keysym, .. } => self.typed.push(keysym),
            Input::Scroll { steps } => self.scrolled = self.scrolled.saturating_add_signed(steps as isize),
            _ => return Ok(()),
        }
        self.dirty = true;
        Ok(())
    }
}
