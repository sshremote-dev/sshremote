//! Video regions: an area that keeps changing frame after frame (a playing
//! video, an animation, a scrolling photo) is sent as H.264 instead of lossless
//! tiles. When it calms down the agent sends the area losslessly once more.

use std::time::{Duration, Instant};

use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};

use crate::capture::Image;
use crate::color::Rect;

const TILE: usize = 32;
/// A tile counts as hot when it changed in this many of the last 8 processed frames.
const HOT: u32 = 5;
/// Fewest hot tiles that make a region (about 160x160 pixels).
const MIN_TILES: usize = 25;
const QUIET: Duration = Duration::from_millis(1200);

pub enum Event {
    None,
    /// The region ended; its pixels must be resent losslessly.
    Ended(Rect),
}

pub struct Video {
    cols: usize,
    rows: usize,
    size: (usize, usize),
    heat: Vec<u8>,
    pub region: Option<Rect>,
    enc: Option<Encoder>,
    yuv: Option<YUVBuffer>,
    crop: Vec<u8>,
    last_change: Instant,
}

impl Video {
    pub fn new() -> Video {
        Video {
            cols: 0,
            rows: 0,
            size: (0, 0),
            heat: Vec::new(),
            region: None,
            enc: None,
            yuv: None,
            crop: Vec::new(),
            last_change: Instant::now(),
        }
    }

    /// Forgets everything, as after a size or colour mode change.
    pub fn reset(&mut self) {
        *self = Video::new();
    }

    /// Updates tile heat from this frame's changed rectangles and moves the region.
    pub fn observe(&mut self, w: usize, h: usize, rects: &[Rect]) -> Event {
        if (w, h) != self.size {
            self.reset();
            self.size = (w, h);
            (self.cols, self.rows) = (w.div_ceil(TILE), h.div_ceil(TILE));
            self.heat = vec![0; self.cols * self.rows];
        }
        let mut changed = vec![false; self.heat.len()];
        for r in rects {
            for ty in r.y / TILE..(r.y + r.h).div_ceil(TILE) {
                for tx in r.x / TILE..(r.x + r.w).div_ceil(TILE) {
                    changed[ty * self.cols + tx] = true;
                }
            }
        }
        if let Some(reg) = self.region
            && rects.iter().any(|r| overlaps(r, &reg))
        {
            self.last_change = Instant::now();
        }
        for (heat, &c) in self.heat.iter_mut().zip(&changed) {
            *heat = (*heat << 1) | c as u8;
        }
        if let Some(reg) = self.region {
            if self.last_change.elapsed() > QUIET {
                self.region = None;
                self.enc = None;
                // Heat only decays on processed frames; start the area cold so it is not taken back at once.
                for ty in reg.y / TILE..(reg.y + reg.h).div_ceil(TILE) {
                    for tx in reg.x / TILE..(reg.x + reg.w).div_ceil(TILE) {
                        self.heat[ty * self.cols + tx] = 0;
                    }
                }
                return Event::Ended(reg);
            }
            return Event::None;
        }
        let hot: Vec<(usize, usize)> = (0..self.heat.len())
            .filter(|&i| self.heat[i].count_ones() >= HOT)
            .map(|i| (i % self.cols, i / self.cols))
            .collect();
        if hot.len() < MIN_TILES {
            return Event::None;
        }
        let (x0, x1) = (hot.iter().map(|t| t.0).min().unwrap(), hot.iter().map(|t| t.0).max().unwrap() + 1);
        let (y0, y1) = (hot.iter().map(|t| t.1).min().unwrap(), hot.iter().map(|t| t.1).max().unwrap() + 1);
        // Scattered changes (a clock here, a cursor there) do not make a video.
        if hot.len() * 2 < (x1 - x0) * (y1 - y0) {
            return Event::None;
        }
        let rw = ((x1 * TILE).min(w) - x0 * TILE) & !1;
        let rh = ((y1 * TILE).min(h) - y0 * TILE) & !1;
        if rw < 16 || rh < 16 {
            return Event::None;
        }
        self.region = Some(Rect { x: x0 * TILE, y: y0 * TILE, w: rw, h: rh });
        self.last_change = Instant::now();
        Event::None
    }

    /// Encodes the region from a BGRx capture; returns the H.264 access unit.
    pub fn encode(&mut self, img: &Image, fps: u8, bitrate: u32) -> Result<Vec<u8>, String> {
        let r = self.region.ok_or("no video region")?;
        if self.enc.is_none() {
            let config = EncoderConfig::new()
                .usage_type(UsageType::ScreenContentRealTime)
                .rate_control_mode(RateControlMode::Bitrate)
                .bitrate(BitRate::from_bps(bitrate))
                .max_frame_rate(FrameRate::from_hz(fps.max(1) as f32))
                .skip_frames(false);
            self.enc = Some(Encoder::with_api_config(OpenH264API::from_source(), config).map_err(|e| e.to_string())?);
            self.yuv = Some(YUVBuffer::new(r.w, r.h));
        }
        self.crop.clear();
        for y in r.y..r.y + r.h {
            let o = y * img.stride + r.x * 4;
            self.crop.extend_from_slice(&img.data[o..o + r.w * 4]);
        }
        let yuv = self.yuv.as_mut().unwrap();
        yuv.read_bgra8(BgraSliceU8::new(&self.crop, (r.w, r.h)));
        let enc = self.enc.as_mut().unwrap();
        Ok(enc.encode(&*yuv).map_err(|e| e.to_string())?.to_vec())
    }

    /// Asks for a key frame, as after the viewer lost its picture.
    pub fn restart(&mut self) {
        self.enc = None;
    }
}

pub fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Splits `r` into the parts outside `cut`.
pub fn subtract(r: &Rect, cut: &Rect) -> Vec<Rect> {
    if !overlaps(r, cut) {
        return vec![*r];
    }
    let mut out = Vec::new();
    let (rx1, ry1, cx1, cy1) = (r.x + r.w, r.y + r.h, cut.x + cut.w, cut.y + cut.h);
    if r.y < cut.y {
        out.push(Rect { x: r.x, y: r.y, w: r.w, h: cut.y - r.y });
    }
    if ry1 > cy1 {
        out.push(Rect { x: r.x, y: cy1, w: r.w, h: ry1 - cy1 });
    }
    let (my0, my1) = (r.y.max(cut.y), ry1.min(cy1));
    if r.x < cut.x {
        out.push(Rect { x: r.x, y: my0, w: cut.x - r.x, h: my1 - my0 });
    }
    if rx1 > cx1 {
        out.push(Rect { x: cx1, y: my0, w: rx1 - cx1, h: my1 - my0 });
    }
    out
}

/// Viewer side: one decoder per region, restarted when the region changes.
#[derive(Default)]
pub struct Player {
    rect: Option<(usize, usize, usize, usize)>,
    dec: Option<Decoder>,
    rgb: Vec<u8>,
}

impl Player {
    /// Decodes one access unit for `rect`; returns its RGB pixels when a picture is ready.
    pub fn decode(&mut self, rect: (usize, usize, usize, usize), data: &[u8]) -> Result<Option<&[u8]>, String> {
        if self.rect != Some(rect) || self.dec.is_none() {
            self.dec = Some(Decoder::new().map_err(|e| e.to_string())?);
            self.rect = Some(rect);
        }
        let dec = self.dec.as_mut().unwrap();
        let Some(pic) = dec.decode(data).map_err(|e| e.to_string())? else { return Ok(None) };
        let (w, h) = pic.dimensions();
        if (w, h) != (rect.2, rect.3) {
            return Err(format!("video picture is {w}x{h}, expected {}x{}", rect.2, rect.3));
        }
        self.rgb.resize(w * h * 3, 0);
        pic.write_rgb8(&mut self.rgb);
        Ok(Some(&self.rgb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtract_leaves_the_outside() {
        let r = Rect { x: 0, y: 0, w: 100, h: 100 };
        let cut = Rect { x: 20, y: 30, w: 40, h: 40 };
        let parts = subtract(&r, &cut);
        let area: usize = parts.iter().map(|p| p.w * p.h).sum();
        assert_eq!(area, 100 * 100 - 40 * 40);
        assert!(parts.iter().all(|p| !overlaps(p, &cut)));
    }

    #[test]
    fn a_busy_area_becomes_a_region_and_round_trips() {
        let (w, h) = (640, 360);
        let mut v = Video::new();
        let busy = [Rect { x: 64, y: 64, w: 256, h: 192 }];
        for _ in 0..8 {
            v.observe(w, h, &busy);
        }
        let r = v.region.expect("region");
        assert_eq!((r.x, r.y, r.w, r.h), (64, 64, 256, 192));
        let mut img = Image { w, h, stride: w * 4, data: vec![0; w * h * 4] };
        for (i, p) in img.data.chunks_mut(4).enumerate() {
            p.copy_from_slice(&[(i % 251) as u8, (i / w % 200) as u8, 90, 255]);
        }
        let au = v.encode(&img, 30, 2_000_000).unwrap();
        assert!(!au.is_empty());
        let mut p = Player::default();
        let rgb = p.decode((r.x, r.y, r.w, r.h), &au).unwrap().expect("picture");
        assert_eq!(rgb.len(), r.w * r.h * 3);
    }
}
