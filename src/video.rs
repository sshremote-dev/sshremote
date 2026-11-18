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
/// Fewest tiles that make a region at once when they changed in two frames in a row (about
/// 256x256 pixels): a film's first frames would otherwise each go losslessly, a large burst.
const FAST_TILES: usize = 64;
/// Changes further apart than this are not frame after frame: the heat starts over.
const GAP: Duration = Duration::from_millis(400);
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
    last_seen: Option<Instant>,
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
            last_seen: None,
        }
    }

    /// Forgets everything, as after a size or colour mode change.
    pub fn reset(&mut self) {
        *self = Video::new();
    }

    /// Updates tile heat from this frame's changed rectangles and moves the region. The rows a
    /// scroll uncovered (`scrolled`) are new text, not a film, and heat nothing. `rich` says
    /// whether an area now looks like a photograph: only such an area becomes a region, and a
    /// region that stops looking like one ends at once, since text and drawings cost little
    /// losslessly and blur in a film.
    pub fn observe(&mut self, w: usize, h: usize, rects: &[Rect], scrolled: bool, rich: &dyn Fn(&Rect) -> bool) -> Event {
        self.observe_at(w, h, rects, scrolled, rich, Instant::now())
    }

    fn observe_at(&mut self, w: usize, h: usize, rects: &[Rect], scrolled: bool, rich: &dyn Fn(&Rect) -> bool, now: Instant) -> Event {
        if (w, h) != self.size {
            self.reset();
            self.size = (w, h);
            (self.cols, self.rows) = (w.div_ceil(TILE), h.div_ceil(TILE));
            self.heat = vec![0; self.cols * self.rows];
        }
        if self.last_seen.replace(now).is_some_and(|t| now.duration_since(t) > GAP) {
            self.heat.fill(0);
        }
        let mut changed = vec![false; self.heat.len()];
        for r in rects.iter().filter(|_| !scrolled) {
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
            let plain = rects.iter().any(|r| overlaps(r, &reg)) && !rich(&reg);
            if plain || self.last_change.elapsed() > QUIET {
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
        let tiles = |hot: &dyn Fn(u8) -> bool| -> Vec<(usize, usize)> {
            (0..self.heat.len()).filter(|&i| hot(self.heat[i])).map(|i| (i % self.cols, i / self.cols)).collect()
        };
        // Scattered changes (a clock here, a cursor there) do not make a video.
        let dense = |hot: &[(usize, usize)], least: usize| {
            if hot.len() < least {
                return None;
            }
            let (x0, x1) = (hot.iter().map(|t| t.0).min()?, hot.iter().map(|t| t.0).max()? + 1);
            let (y0, y1) = (hot.iter().map(|t| t.1).min()?, hot.iter().map(|t| t.1).max()? + 1);
            (hot.len() * 2 >= (x1 - x0) * (y1 - y0)).then_some((x0, x1, y0, y1))
        };
        let box_of = |(x0, x1, y0, y1): (usize, usize, usize, usize)| Rect { x: x0 * TILE, y: y0 * TILE, w: ((x1 * TILE).min(w) - x0 * TILE), h: ((y1 * TILE).min(h) - y0 * TILE) };
        let Some((x0, x1, y0, y1)) = dense(&tiles(&|heat| heat.count_ones() >= HOT), MIN_TILES)
            .or_else(|| dense(&tiles(&|heat| heat & 3 == 3), FAST_TILES))
            .filter(|b| rich(&box_of(*b)))
        else {
            return Event::None;
        };
        let rw = ((x1 * TILE).min(w) - x0 * TILE) & !1;
        let rh = ((y1 * TILE).min(h) - y0 * TILE) & !1;
        if rw < 16 || rh < 16 {
            return Event::None;
        }
        self.region = Some(Rect { x: x0 * TILE, y: y0 * TILE, w: rw, h: rh });
        self.last_change = Instant::now();
        Event::None
    }

    /// Encodes the region from a BGRx capture, in grey if asked; returns the H.264 access unit.
    pub fn encode(&mut self, img: &Image, fps: u8, bitrate: u32, grey: bool) -> Result<Vec<u8>, String> {
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
        if grey {
            for px in self.crop.as_chunks_mut::<4>().0 {
                let l = ((px[0] as u32 * 29 + px[1] as u32 * 150 + px[2] as u32 * 77) >> 8) as u8;
                (px[0], px[1], px[2]) = (l, l, l);
            }
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

/// The 32x32 tiles where two captures of one size differ.
pub fn changed_tiles(prev: &Image, cur: &Image) -> Vec<Rect> {
    if (prev.w, prev.h) != (cur.w, cur.h) {
        return vec![Rect { x: 0, y: 0, w: cur.w, h: cur.h }];
    }
    let mut out = Vec::new();
    for ty in (0..cur.h).step_by(TILE) {
        let th = TILE.min(cur.h - ty);
        for tx in (0..cur.w).step_by(TILE) {
            let tw = TILE.min(cur.w - tx);
            fn row(img: &Image, y: usize, x: usize, w: usize) -> &[u8] {
                &img.data[y * img.stride + x * 4..y * img.stride + (x + w) * 4]
            }
            if (ty..ty + th).any(|y| row(prev, y, tx, tw) != row(cur, y, tx, tw)) {
                out.push(Rect { x: tx, y: ty, w: tw, h: th });
            }
        }
    }
    out
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
            v.observe(w, h, &busy, false, &|_| true);
        }
        let r = v.region.expect("region");
        assert_eq!((r.x, r.y, r.w, r.h), (64, 64, 256, 192));
        let mut img = Image { w, h, stride: w * 4, data: vec![0; w * h * 4] };
        for (i, p) in img.data.chunks_mut(4).enumerate() {
            p.copy_from_slice(&[(i % 251) as u8, (i / w % 200) as u8, 90, 255]);
        }
        let au = v.encode(&img, 30, 2_000_000, false).unwrap();
        assert!(!au.is_empty());
        let mut p = Player::default();
        let rgb = p.decode((r.x, r.y, r.w, r.h), &au).unwrap().expect("picture");
        assert_eq!(rgb.len(), r.w * r.h * 3);
    }

    #[test]
    fn a_large_area_changing_twice_in_a_row_is_a_film_at_once() {
        let (w, h) = (1280, 800);
        let film = [Rect { x: 200, y: 160, w: 854, h: 480 }];
        let t = Instant::now();
        let mut v = Video::new();
        v.observe_at(w, h, &film, false, &|_| true, t);
        assert!(v.region.is_none());
        v.observe_at(w, h, &film, false, &|_| true, t + Duration::from_millis(33));
        assert!(v.region.is_some());
        // Text over it ends it at once, to be sent losslessly.
        assert!(matches!(v.observe_at(w, h, &film, false, &|_| false, t + Duration::from_millis(66)), Event::Ended(_)));
        // A window drawn twice as it opens is no film.
        let mut v = Video::new();
        v.observe_at(w, h, &film, false, &|_| false, t);
        v.observe_at(w, h, &film, false, &|_| false, t + Duration::from_millis(33));
        assert!(v.region.is_none());
    }

    #[test]
    fn pages_turned_seconds_apart_and_scrolling_are_no_film() {
        let (w, h) = (1280, 800);
        let page = [Rect { x: 40, y: 0, w: 1200, h: 800 }];
        let t = Instant::now();
        let mut v = Video::new();
        for i in 0..10 {
            v.observe_at(w, h, &page, false, &|_| true, t + Duration::from_secs(2 * i));
        }
        assert!(v.region.is_none());
        let strip = [Rect { x: 40, y: 750, w: 1200, h: 50 }];
        for i in 0..20 {
            v.observe_at(w, h, &strip, true, &|_| true, t + Duration::from_secs(30) + Duration::from_millis(100 * i));
        }
        assert!(v.region.is_none());
    }
}
