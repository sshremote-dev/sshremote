//! Video regions: an area that keeps changing frame after frame (a playing
//! video, an animation, a scrolling photo) is sent as a film (AV1 where the viewer reads it,
//! else H.264) instead of lossless tiles. When it calms down the agent sends the area
//! losslessly once more.

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
/// Fewest tiles of a photograph-like area that make a region from its very first change (about
/// 512x256 pixels): its first frame alone would cost hundreds of kilobytes losslessly, when a film
/// starts there. Were it a still picture, it is sent losslessly once it has been quiet a while.
const FIRST_TILES: usize = 128;
/// The cells looked at for a photograph in a change, CELL x CELL pixels (2x2 tiles).
const CELL: usize = 2 * TILE;
/// Changes further apart than this are not frame after frame: the heat starts over.
const GAP: Duration = Duration::from_millis(400);
const QUIET: Duration = Duration::from_millis(1200);

pub enum Event {
    None,
    /// The region ended; its pixels must be resent losslessly.
    Ended(Rect),
}

/// The first byte of an H.265 film picture (AV1's is `av1::MARK`; H.264 starts with a zero).
pub const H265_MARK: u8 = 0x13;

pub struct Video {
    cols: usize,
    rows: usize,
    size: (usize, usize),
    heat: Vec<u8>,
    pub region: Option<Rect>,
    enc: Option<Encoder>,
    /// The viewer reads AV1: films go in it (`aom`), else in H.264 (`enc`).
    pub av1: bool,
    /// The codec the viewer chose (`proto::CODEC_*`), and whether it reads H.265.
    pub choice: u8,
    pub reads_h265: bool,
    aom: Option<crate::av1::Encoder>,
    /// The film's hardware encoder once it went there (`on_hw`); hardware that failed is not
    /// asked again (`hw_failed`), nor for this film hardware that was no quicker (`hw_slower`).
    hw: Option<Box<dyn crate::hwenc::Encoder>>,
    on_hw: bool,
    hw_codec: crate::hwenc::Codec,
    hw_failed: bool,
    h265_failed: bool,
    hw_slower: bool,
    /// The time for a picture of this film (ms, on average, and over how many pictures) on the
    /// processor and on the hardware.
    slow: (f64, u32),
    hw_time: (f64, u32),
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
            av1: false,
            choice: crate::proto::CODEC_AUTO,
            reads_h265: false,
            aom: None,
            hw: None,
            on_hw: false,
            hw_codec: crate::hwenc::Codec::H264,
            hw_failed: false,
            h265_failed: false,
            hw_slower: false,
            slow: (0.0, 0),
            hw_time: (0.0, 0),
            yuv: None,
            crop: Vec::new(),
            last_change: Instant::now(),
            last_seen: None,
        }
    }

    /// Forgets everything, as after a size or colour mode change.
    pub fn reset(&mut self) {
        let kept = (self.av1, self.choice, self.reads_h265, self.hw_failed, self.h265_failed);
        *self = Video::new();
        (self.av1, self.choice, self.reads_h265, self.hw_failed, self.h265_failed) = kept;
    }

    /// What the viewer reads and chose; a change starts the film again from a key picture.
    pub fn choose(&mut self, av1: bool, choice: u8, reads_h265: bool) {
        if (self.av1, self.choice, self.reads_h265) != (av1, choice, reads_h265) {
            (self.av1, self.choice, self.reads_h265) = (av1, choice, reads_h265);
            self.new_film();
        }
    }

    /// Updates tile heat from this frame's changed rectangles and moves the region. The rows a
    /// scroll uncovered (`scrolled`) are new text, not a film, and heat nothing. `rich` says
    /// whether an area now looks like a photograph: only such an area becomes a region, and a
    /// region that stops looking like one ends at once, since text and drawings cost little
    /// losslessly and blur in a film. `cell` is a quicker look at one CELL-sized cell, to find a
    /// photograph within a larger change.
    pub fn observe(&mut self, w: usize, h: usize, rects: &[Rect], scrolled: bool, rich: &dyn Fn(&Rect) -> bool, cell: &dyn Fn(&Rect) -> bool) -> Event {
        self.observe_at(w, h, rects, scrolled, rich, cell, Instant::now())
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_at(&mut self, w: usize, h: usize, rects: &[Rect], scrolled: bool, rich: &dyn Fn(&Rect) -> bool, cell: &dyn Fn(&Rect) -> bool, now: Instant) -> Event {
        if (w, h) != self.size {
            self.reset();
            self.size = (w, h);
            (self.cols, self.rows) = (w.div_ceil(TILE), h.div_ceil(TILE));
            self.heat = vec![0; self.cols * self.rows];
        }
        // The first picture since a start or a new size is the whole screen shown, never a film.
        let before = self.last_seen.replace(now);
        if before.is_some_and(|t| now.duration_since(t) > GAP) {
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
            self.last_change = now;
        }
        for (heat, &c) in self.heat.iter_mut().zip(&changed) {
            *heat = (*heat << 1) | c as u8;
        }
        if let Some(reg) = self.region {
            let plain = rects.iter().any(|r| overlaps(r, &reg)) && !rich(&reg);
            if plain || now.saturating_duration_since(self.last_change) > QUIET {
                self.region = None;
                self.new_film();
                // Heat only decays on processed frames; start the area cold so it is not taken back at once.
                for ty in reg.y / TILE..(reg.y + reg.h).div_ceil(TILE) {
                    for tx in reg.x / TILE..(reg.x + reg.w).div_ceil(TILE) {
                        self.heat[ty * self.cols + tx] = 0;
                    }
                }
                return Event::Ended(reg);
            }
            // A film larger than the area first taken for it (its first frames changed only part
            // of it, or it grew): the tiles around it that keep changing join it, else they would
            // go losslessly frame after frame.
            if let Some(grown) = self.grown(reg, w, h, rich) {
                self.region = Some(grown);
                self.new_film();
                return Event::None;
            }
            // An area taken wrongly (the page that changed as the film began, with part of the
            // film): the region moves to where the changes keep coming now, and what it leaves
            // goes once more losslessly.
            if let Some(film) = self.refit(reg, w, h, rich) {
                self.region = Some(film);
                self.new_film();
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
        // A photograph changed at once, maybe with a page around it: the cells of it that look
        // like one, never (almost) the whole screen.
        let first = || {
            if before.is_none() || tiles(&|heat| heat & 1 == 1).len() < FIRST_TILES {
                return None;
            }
            let (cols, rows) = (self.cols, self.rows);
            let changed = |tx: usize, ty: usize| tx < cols && ty < rows && self.heat[ty * cols + tx] & 1 == 1;
            let mut photo = Vec::new();
            for cy in 0..rows.div_ceil(2) {
                for cx in 0..cols.div_ceil(2) {
                    let (tx, ty) = (cx * 2, cy * 2);
                    if [(0, 0), (1, 0), (0, 1), (1, 1)].iter().any(|(dx, dy)| changed(tx + dx, ty + dy))
                        && cell(&Rect { x: cx * CELL, y: cy * CELL, w: CELL.min(w - cx * CELL), h: CELL.min(h - cy * CELL) })
                    {
                        photo.extend([(tx, ty), (tx + 1, ty), (tx, ty + 1), (tx + 1, ty + 1)].into_iter().filter(|&(x, y)| x < cols && y < rows));
                    }
                }
            }
            // A cell only partly over the photograph is not taken for one: what changed within a
            // cell around them is the rest of it, or a film's edges would go losslessly each frame.
            let (x0, x1, y0, y1) = dense(&photo, FIRST_TILES)?;
            let (ex0, ex1, ey0, ey1) = (x0.saturating_sub(2), (x1 + 2).min(cols), y0.saturating_sub(2), (y1 + 2).min(rows));
            let near = (ey0..ey1).flat_map(|ty| (ex0..ex1).map(move |tx| (tx, ty))).filter(|&(tx, ty)| changed(tx, ty));
            let b = near.fold((x0, x1, y0, y1), |b, (tx, ty)| (b.0.min(tx), b.1.max(tx + 1), b.2.min(ty), b.3.max(ty + 1)));
            Some(b).filter(|b| (b.1 - b.0) * (b.3 - b.2) * 4 < cols * rows * 3)
        };
        // Most of a film's cells look like a photograph: not a page that kept changing as the film
        // began, beside part of it (which `rich`'s few samples can take for one).
        let mostly_photo = |b: &(usize, usize, usize, usize)| {
            let r = box_of(*b);
            let (mut cells, mut photos) = (0, 0);
            for cy in (r.y..r.y + r.h).step_by(CELL) {
                for cx in (r.x..r.x + r.w).step_by(CELL) {
                    cells += 1;
                    photos += cell(&Rect { x: cx, y: cy, w: CELL.min(r.x + r.w - cx), h: CELL.min(r.y + r.h - cy) }) as usize;
                }
            }
            photos * 5 >= cells * 3
        };
        let Some((x0, x1, y0, y1)) = dense(&tiles(&|heat| heat.count_ones() >= HOT), MIN_TILES)
            .filter(&mostly_photo)
            .or_else(|| dense(&tiles(&|heat| heat & 3 == 3), FAST_TILES).filter(&mostly_photo))
            .or_else(first)
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
        self.last_change = now;
        Event::None
    }

    /// The region `reg` with the tiles near it (two tiles or less from it, again and again) that
    /// changed in most of the last frames, if there are any and the whole still looks like a
    /// photograph and is not most of the screen.
    fn grown(&self, reg: Rect, w: usize, h: usize, rich: &dyn Fn(&Rect) -> bool) -> Option<Rect> {
        let (cols, rows) = (self.cols, self.rows);
        let hot = |tx: usize, ty: usize| self.heat[ty * cols + tx].count_ones() >= HOT;
        let start = (reg.x / TILE, (reg.x + reg.w).div_ceil(TILE), reg.y / TILE, (reg.y + reg.h).div_ceil(TILE));
        let mut b = start;
        loop {
            let (x0, x1, y0, y1) = b;
            let mut n = b;
            for ty in y0.saturating_sub(2)..(y1 + 2).min(rows) {
                for tx in x0.saturating_sub(2)..(x1 + 2).min(cols) {
                    if (tx < x0 || tx >= x1 || ty < y0 || ty >= y1) && hot(tx, ty) {
                        n = (n.0.min(tx), n.1.max(tx + 1), n.2.min(ty), n.3.max(ty + 1));
                    }
                }
            }
            if n == b {
                break;
            }
            b = n;
        }
        let (x0, x1, y0, y1) = b;
        if b == start || (x1 - x0) * (y1 - y0) * 4 >= cols * rows * 3 {
            return None;
        }
        let r = Rect { x: x0 * TILE, y: y0 * TILE, w: ((x1 * TILE).min(w) - x0 * TILE) & !1, h: ((y1 * TILE).min(h) - y0 * TILE) & !1 };
        rich(&r).then_some(r)
    }

    /// Where a film is by the changes alone, when that reaches out of `reg`: the box of the
    /// largest group of tiles that changed in most of the last frames (two tiles apart at most),
    /// when the group overlaps `reg`, fills most of its box, looks like a photograph and is not
    /// most of the screen.
    fn refit(&self, reg: Rect, w: usize, h: usize, rich: &dyn Fn(&Rect) -> bool) -> Option<Rect> {
        let (cols, rows) = (self.cols, self.rows);
        let hot = |i: usize| self.heat[i].count_ones() >= HOT;
        let mut seen = vec![false; self.heat.len()];
        let mut best: Vec<usize> = Vec::new();
        for start in (0..self.heat.len()).filter(|&i| hot(i)) {
            if seen[start] {
                continue;
            }
            seen[start] = true;
            let (mut group, mut todo) = (Vec::new(), vec![start]);
            while let Some(i) = todo.pop() {
                group.push(i);
                let (tx, ty) = (i % cols, i / cols);
                for ny in ty.saturating_sub(2)..(ty + 3).min(rows) {
                    for nx in tx.saturating_sub(2)..(tx + 3).min(cols) {
                        let j = ny * cols + nx;
                        if !seen[j] && hot(j) {
                            seen[j] = true;
                            todo.push(j);
                        }
                    }
                }
            }
            if group.len() > best.len() {
                best = group;
            }
        }
        if best.len() < MIN_TILES {
            return None;
        }
        let (x0, x1) = (best.iter().map(|i| i % cols).min()?, best.iter().map(|i| i % cols).max()? + 1);
        let (y0, y1) = (best.iter().map(|i| i / cols).min()?, best.iter().map(|i| i / cols).max()? + 1);
        if best.len() * 2 < (x1 - x0) * (y1 - y0) || (x1 - x0) * (y1 - y0) * 4 >= cols * rows * 3 {
            return None;
        }
        let r = Rect { x: x0 * TILE, y: y0 * TILE, w: ((x1 * TILE).min(w) - x0 * TILE) & !1, h: ((y1 * TILE).min(h) - y0 * TILE) & !1 };
        let inside = r.x >= reg.x && r.y >= reg.y && r.x + r.w <= reg.x + reg.w && r.y + r.h <= reg.y + reg.h;
        (!inside && overlaps(&r, &reg) && rich(&r)).then_some(r)
    }

    /// Encodes the region from a BGRx capture, in grey if asked; returns its access unit (AV1
    /// or H.264, as `av1` says). `bitrate` is H.264's; half of it is AV1's ceiling, which a film
    /// seldom reaches (`av1::FLOOR`).
    pub fn encode(&mut self, img: &Image, fps: u8, bitrate: u32, grey: bool) -> Result<Vec<u8>, String> {
        use crate::hwenc::{Codec, Want};
        use crate::proto::{CODEC_AV1, CODEC_H265};
        let r = self.region.ok_or("no video region")?;
        let want = crate::hwenc::wanted();
        // Chosen by the viewer, H.265 comes from the video hardware only.
        let h265 = self.choice == CODEC_H265 && self.reads_h265 && !self.h265_failed && want != Want::Never;
        // The processor spends more than half of each picture's time on the film: the video
        // hardware takes it, in H.264 (unless the viewer chose AV1).
        let behind = self.slow.1 >= 10 && self.slow.0 > 500.0 / fps.max(1) as f64;
        let h264 = self.choice != CODEC_AV1 && !self.hw_failed && !self.hw_slower && (want == Want::Always || (want == Want::WhenBehind && behind));
        if !self.on_hw && (h265 || h264) {
            (self.on_hw, self.hw_codec) = (true, if h265 { Codec::H265 } else { Codec::H264 });
        }
        if self.on_hw && self.hw.is_none() {
            match crate::hwenc::open(self.hw_codec, r.w, r.h, fps, bitrate) {
                Ok(e) => {
                    eprintln!("sshremote: a {}x{} film goes to {} now, in {:?}", r.w, r.h, e.name(), self.hw_codec);
                    self.hw = Some(e);
                }
                Err(e) => {
                    eprintln!("sshremote: {e}");
                    self.failed();
                }
            }
        }
        if self.hw.is_some() {
            let began = Instant::now();
            if self.yuv.as_ref().is_none_or(|y| y.dimensions() != (r.w, r.h)) {
                self.yuv = Some(YUVBuffer::new(r.w, r.h));
            }
            self.convert(img, r, grey);
            let yuv = self.yuv.as_ref().unwrap();
            match self.hw.as_mut().unwrap().encode(yuv.y(), yuv.u(), yuv.v(), yuv.strides()) {
                Ok(au) => {
                    self.hw_time = average(self.hw_time, began.elapsed().as_secs_f64() * 1000.0);
                    if self.hw_codec == Codec::H265 {
                        let mut out = Vec::with_capacity(au.len() + 1);
                        out.push(H265_MARK);
                        out.extend_from_slice(&au);
                        return Ok(out);
                    }
                    // No quicker than the processor (a fast one, a slow card): back to it, from
                    // a key picture.
                    if want != Want::Always && self.hw_time.1 >= 10 && self.hw_time.0 >= self.slow.0 {
                        eprintln!("sshremote: the film goes back to the processor ({:.1} ms a picture there, {:.1} ms on the hardware)", self.slow.0, self.hw_time.0);
                        (self.hw, self.on_hw, self.hw_slower) = (None, false, true);
                        (self.enc, self.aom) = (None, None);
                    }
                    return Ok(au);
                }
                // Back to the processor, from a key picture.
                Err(e) => {
                    eprintln!("sshremote: {e}");
                    self.hw = None;
                    self.failed();
                    (self.enc, self.aom) = (None, None);
                }
            }
        }
        let began = Instant::now();
        let au = self.encode_here(img, fps, bitrate, grey, r);
        self.slow = average(self.slow, began.elapsed().as_secs_f64() * 1000.0);
        au
    }

    /// The hardware failed: not asked again for its codec this session.
    fn failed(&mut self) {
        match self.hw_codec {
            crate::hwenc::Codec::H264 => self.hw_failed = true,
            crate::hwenc::Codec::H265 => self.h265_failed = true,
        }
        self.on_hw = false;
    }

    /// The film's picture encoded by the processor: AV1 for a viewer that reads it (and did not
    /// choose H.264), else H.264.
    fn encode_here(&mut self, img: &Image, fps: u8, bitrate: u32, grey: bool, r: Rect) -> Result<Vec<u8>, String> {
        if self.av1 && self.choice != crate::proto::CODEC_H264 {
            if self.aom.is_none() {
                self.aom = Some(crate::av1::Encoder::new(r.w, r.h, fps, bitrate / 2)?);
                self.yuv = Some(YUVBuffer::new(r.w, r.h));
            }
            self.convert(img, r, grey);
            let yuv = self.yuv.as_ref().unwrap();
            return self.aom.as_mut().unwrap().encode(yuv.y(), yuv.u(), yuv.v(), yuv.strides());
        }
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
        self.convert(img, r, grey);
        let yuv = self.yuv.as_ref().unwrap();
        let enc = self.enc.as_mut().unwrap();
        Ok(enc.encode(yuv).map_err(|e| e.to_string())?.to_vec())
    }

    /// The region of `img` into `yuv`, in grey if asked.
    fn convert(&mut self, img: &Image, r: Rect, grey: bool) {
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
        self.yuv.as_mut().unwrap().read_bgra8(BgraSliceU8::new(&self.crop, (r.w, r.h)));
    }

    /// Whether the region has been still long enough to end, though no new picture came to say
    /// so: a film paused, a photograph shown once.
    pub fn quiet(&self) -> bool {
        self.region.is_some() && self.last_change.elapsed() > QUIET
    }

    /// Asks for a key frame, as after the viewer lost its picture.
    pub fn restart(&mut self) {
        (self.enc, self.aom, self.hw) = (None, None, None);
    }

    /// A new film, or the same at another size: new encoders, the processor timed again.
    fn new_film(&mut self) {
        (self.enc, self.aom, self.hw) = (None, None, None);
        (self.on_hw, self.hw_slower, self.slow, self.hw_time) = (false, false, (0.0, 0), (0.0, 0));
    }
}

/// A running average (of the last pictures most) with one more `ms`, and the count.
fn average((mean, n): (f64, u32), ms: f64) -> (f64, u32) {
    if n == 0 { (ms, 1) } else { (mean * 0.8 + ms * 0.2, n + 1) }
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
    aom: Option<crate::av1::Decoder>,
    /// The system's H.265 decoder, for films that come so (`H265_MARK`).
    hevc: Option<Box<dyn crate::hwdec::Decoder>>,
    rgb: Vec<u8>,
}

impl Player {
    /// Decodes one access unit for `rect`; returns its RGB pixels when a picture is ready.
    pub fn decode(&mut self, rect: (usize, usize, usize, usize), data: &[u8]) -> Result<Option<&[u8]>, String> {
        if self.rect != Some(rect) {
            (self.dec, self.aom, self.hevc) = (None, None, None);
            self.rect = Some(rect);
        }
        if data.first() == Some(&H265_MARK) {
            if self.hevc.is_none() {
                self.hevc = Some(crate::hwdec::open_h265()?);
            }
            let shown = self.hevc.as_mut().unwrap().decode(&data[1..], rect.2, rect.3, &mut self.rgb)?;
            return Ok(shown.then_some(&self.rgb[..]));
        }
        if data.first() == Some(&crate::av1::MARK) {
            if self.aom.is_none() {
                self.aom = Some(crate::av1::Decoder::new()?);
            }
            let shown = self.aom.as_mut().unwrap().decode(data, rect.2, rect.3, &mut self.rgb)?;
            return Ok(shown.then_some(&self.rgb[..]));
        }
        if self.dec.is_none() {
            self.dec = Some(Decoder::new().map_err(|e| e.to_string())?);
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
        // In H.264, and in AV1 for a viewer that reads it: one player takes either.
        for av1 in [false, true] {
            let (w, h) = (640, 360);
            let mut v = Video::new();
            v.av1 = av1;
            let busy = [Rect { x: 64, y: 64, w: 256, h: 192 }];
            for _ in 0..8 {
                v.observe(w, h, &busy, false, &|_| true, &|_| true);
            }
            let r = v.region.expect("region");
            assert_eq!((r.x, r.y, r.w, r.h), (64, 64, 256, 192));
            // Smooth, as a photograph is: what a film keeps.
            let px = |x: usize, y: usize| [(x / 3 + 20) as u8, (y % 200 + 30) as u8, 90];
            let mut img = Image { w, h, stride: w * 4, data: vec![0; w * h * 4] };
            for (i, p) in img.data.chunks_mut(4).enumerate() {
                let [b, g, r] = px(i % w, i / w);
                p.copy_from_slice(&[b, g, r, 255]);
            }
            let au = v.encode(&img, 30, 2_000_000, false).unwrap();
            assert_eq!(au.first() == Some(&crate::av1::MARK), av1);
            let mut p = Player::default();
            let rgb = p.decode((r.x, r.y, r.w, r.h), &au).unwrap().expect("picture").to_vec();
            assert_eq!(rgb.len(), r.w * r.h * 3);
            // Near the region's own pixels (BGRx in, RGB out).
            let at = |x: usize, y: usize| {
                let [b, g, red] = px(r.x + x, r.y + y);
                [red, g, b]
            };
            let points: Vec<(usize, usize)> = (0..r.h).step_by(7).flat_map(|y| (0..r.w).step_by(5).map(move |x| (x, y))).collect();
            let off = points.iter().map(|&(x, y)| (0..3).map(|c| (rgb[(y * r.w + x) * 3 + c] as i32 - at(x, y)[c] as i32).abs()).sum::<i32>()).sum::<i32>() as f64 / (points.len() * 3) as f64;
            assert!(off < 8.0, "av1 {av1}: off by {off} on average");
        }
    }

    /// A film first taken from part of it (its first frames changed only its top) takes in the
    /// rest as it keeps changing, so no part of it goes losslessly frame after frame; changes far
    /// from it do not join it.
    #[test]
    fn a_region_grows_over_the_rest_of_its_film() {
        let (w, h) = (1280, 800);
        let t = Instant::now();
        let mut v = Video::new();
        let top = [Rect { x: 224, y: 160, w: 832, h: 256 }];
        v.observe_at(w, h, &top, false, &|_| true, &|_| true, t);
        v.observe_at(w, h, &top, false, &|_| true, &|_| true, t + Duration::from_millis(33));
        let r = v.region.expect("a region");
        assert_eq!((r.y, r.h), (160, 256), "the top alone at first");
        let film = [Rect { x: 224, y: 160, w: 832, h: 480 }, Rect { x: 0, y: 760, w: 64, h: 32 }];
        for k in 2..10 {
            v.observe_at(w, h, &film, false, &|_| true, &|_| true, t + Duration::from_millis(33 * k));
        }
        let r = v.region.expect("still a region");
        assert_eq!((r.x, r.y, r.w, r.h), (224, 160, 832, 480), "the whole film, not the clock in the corner");
    }

    /// A region taken wrongly, a strip of the page that changed as the film began with part of the
    /// film, moves to the film once the changes show where it is; the strip goes once losslessly.
    #[test]
    fn a_region_taken_wrongly_moves_to_the_film() {
        let (w, h) = (1280, 800);
        let t = Instant::now();
        let mut v = Video::new();
        let strip = [Rect { x: 0, y: 0, w: 512, h: 800 }];
        v.observe_at(w, h, &strip, false, &|_| true, &|_| true, t);
        v.observe_at(w, h, &strip, false, &|_| true, &|_| true, t + Duration::from_millis(33));
        let r = v.region.expect("a region");
        assert_eq!((r.x, r.w, r.h), (0, 512, 800), "the strip at first");
        // The film beside the page: the strip and the film together do not look like one photograph.
        let film = [Rect { x: 224, y: 160, w: 832, h: 480 }];
        let photo = |r: &Rect| r.y >= 128 && r.y + r.h <= 672;
        let mut moved = None;
        for k in 2..12 {
            if let Event::Ended(old) = v.observe_at(w, h, &film, false, &photo, &|_| true, t + Duration::from_millis(33 * k)) {
                moved = Some(old);
            }
        }
        assert_eq!(moved.map(|o| (o.x, o.w, o.h)), Some((0, 512, 800)), "the strip goes once losslessly");
        let r = v.region.expect("a region");
        assert_eq!((r.x, r.y, r.w, r.h), (224, 160, 832, 480), "on the film");
    }

    /// Pages turned fast and then a film: the page's strip, which kept changing, is not taken
    /// with part of the film for a film; the film is.
    #[test]
    fn a_page_turned_fast_is_not_taken_for_the_film_beside_it() {
        let (w, h) = (1280, 800);
        let t = Instant::now();
        let mut v = Video::new();
        let page = [Rect { x: 0, y: 0, w: 512, h: 800 }];
        // Text until the film comes: then its cells look like a photograph.
        let film_on = std::cell::Cell::new(false);
        let photo = |r: &Rect| film_on.get() && r.x >= 192 && r.y >= 128 && r.x + r.w <= 1088 && r.y + r.h <= 672;
        for k in 0..4 {
            v.observe_at(w, h, &page, false, &|_| true, &photo, t + Duration::from_millis(300 * k));
        }
        assert!(v.region.is_none(), "pages are no film");
        film_on.set(true);
        let film = [Rect { x: 0, y: 0, w: 1280, h: 800 }];
        v.observe_at(w, h, &film, false, &|_| true, &photo, t + Duration::from_millis(1200));
        let r = v.region.expect("a region");
        assert!(r.x >= 128 && r.x + r.w <= 1152, "the film, not the page: {r:?}");
    }

    #[test]
    fn a_large_photo_like_change_is_a_film_at_once() {
        let (w, h) = (1280, 800);
        let film = [Rect { x: 200, y: 160, w: 854, h: 480 }];
        let t = Instant::now();
        // The screen first shown, then the film.
        let shown = |v: &mut Video| _ = v.observe_at(w, h, &[], false, &|_| true, &|_| true, t - Duration::from_secs(1));
        let mut v = Video::new();
        shown(&mut v);
        v.observe_at(w, h, &film, false, &|_| true, &|_| true, t);
        assert!(v.region.is_some(), "from its first frame");
        // A smaller one, from its second.
        let mut v = Video::new();
        let small = [Rect { x: 200, y: 160, w: 320, h: 256 }];
        v.observe_at(w, h, &small, false, &|_| true, &|_| true, t);
        assert!(v.region.is_none());
        v.observe_at(w, h, &small, false, &|_| true, &|_| true, t + Duration::from_millis(33));
        assert!(v.region.is_some());
        // Still, it ends: its pixels go again losslessly.
        let mut v = Video::new();
        shown(&mut v);
        v.observe_at(w, h, &film, false, &|_| true, &|_| true, t);
        assert!(matches!(v.observe_at(w, h, &[], false, &|_| true, &|_| true, t + Duration::from_millis(600)), Event::None));
        assert!(matches!(v.observe_at(w, h, &[], false, &|_| true, &|_| true, t + QUIET + Duration::from_millis(100)), Event::Ended(_)));
        // A photograph shown with the page around it: the photograph alone.
        let mut v = Video::new();
        let page = [Rect { x: 0, y: 0, w: 1280, h: 700 }];
        let photo = |r: &Rect| r.x >= 192 && r.y >= 128 && r.x + r.w <= 1088 && r.y + r.h <= 640;
        shown(&mut v);
        v.observe_at(w, h, &page, false, &|_| true, &photo, t);
        let r = v.region.expect("the photograph");
        // All of it, and what changed within a cell around it.
        assert!(r.x <= 192 && r.y <= 128 && r.x + r.w >= 1088 && r.y + r.h >= 640, "{r:?}");
        assert!(r.x >= 128 && r.y >= 64 && r.x + r.w <= 1152 && r.y + r.h <= 704, "{r:?}");
        // A page of text alone is not.
        let mut v = Video::new();
        v.observe_at(w, h, &page, false, &|_| true, &|_| false, t);
        assert!(v.region.is_none());
        // The whole screen first shown is not, even with a photograph in it.
        let mut v = Video::new();
        v.observe_at(w, h, &[Rect { x: 0, y: 0, w, h }], false, &|_| true, &photo, t);
        assert!(v.region.is_none());
        // Later, the whole screen changing to show one is.
        v.observe_at(w, h, &[Rect { x: 0, y: 0, w, h }], false, &|_| true, &photo, t + Duration::from_secs(3));
        assert!(v.region.is_some());
        let mut v = Video::new();
        v.observe_at(w, h, &film, false, &|_| true, &|_| true, t);
        v.observe_at(w, h, &film, false, &|_| true, &|_| true, t + Duration::from_millis(33));
        assert!(v.region.is_some());
        // Text over it ends it at once, to be sent losslessly.
        assert!(matches!(v.observe_at(w, h, &film, false, &|_| false, &|_| false, t + Duration::from_millis(66)), Event::Ended(_)));
        // A window drawn twice as it opens is no film.
        let mut v = Video::new();
        v.observe_at(w, h, &film, false, &|_| false, &|_| false, t);
        v.observe_at(w, h, &film, false, &|_| false, &|_| false, t + Duration::from_millis(33));
        assert!(v.region.is_none());
    }

    #[test]
    fn pages_turned_seconds_apart_and_scrolling_are_no_film() {
        let (w, h) = (1280, 800);
        let page = [Rect { x: 40, y: 0, w: 1200, h: 800 }];
        let t = Instant::now();
        let mut v = Video::new();
        for i in 0..10 {
            v.observe_at(w, h, &page, false, &|_| true, &|_| true, t + Duration::from_secs(2 * i));
        }
        assert!(v.region.is_none());
        let strip = [Rect { x: 40, y: 750, w: 1200, h: 50 }];
        for i in 0..20 {
            v.observe_at(w, h, &strip, true, &|_| true, &|_| true, t + Duration::from_secs(30) + Duration::from_millis(100 * i));
        }
        assert!(v.region.is_none());
    }
}
