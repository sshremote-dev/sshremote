use crate::shared::Image;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Mono,
    Gray4,
    Gray16,
    Color16,
    Color256,
    True,
}

impl Mode {
    pub const ALL: [Mode; 6] = [Mode::Mono, Mode::Gray4, Mode::Gray16, Mode::Color16, Mode::Color256, Mode::True];

    pub fn from_u8(v: u8) -> Option<Mode> {
        Self::ALL.get(v as usize).copied()
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn bits(self) -> usize {
        match self {
            Mode::Mono => 1,
            Mode::Gray4 => 2,
            Mode::Gray16 | Mode::Color16 => 4,
            Mode::Color256 => 8,
            Mode::True => 24,
        }
    }

    /// Bytes per pixel in an unpacked plane.
    pub fn bpp(self) -> usize {
        if self == Mode::True { 3 } else { 1 }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Mono => "mono",
            Mode::Gray4 => "gray4",
            Mode::Gray16 => "gray16",
            Mode::Color16 => "16c",
            Mode::Color256 => "256c",
            Mode::True => "true",
        }
    }

    /// The colour depth in words, for menus.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Mono => "Black and white",
            Mode::Gray4 => "4 greys",
            Mode::Gray16 => "16 greys",
            Mode::Color16 => "16 colours",
            Mode::Color256 => "256 colours",
            Mode::True => "Full colour",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        Self::ALL.iter().copied().find(|m| m.name() == s)
    }

    pub fn next(self) -> Mode {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }

    /// Fixed palette shared by agent and client, so palettes never cross the wire.
    pub fn palette(self) -> Vec<[u8; 3]> {
        match self {
            Mode::Mono => vec![[0, 0, 0], [255, 255, 255]],
            Mode::Gray4 => (0..4).map(|i| [i * 85; 3]).collect(),
            Mode::Gray16 => (0..16).map(|i| [i * 17; 3]).collect(),
            Mode::Color16 => vec![
                [0, 0, 0],
                [128, 0, 0],
                [0, 128, 0],
                [128, 128, 0],
                [0, 0, 128],
                [128, 0, 128],
                [0, 128, 128],
                [192, 192, 192],
                [128, 128, 128],
                [255, 0, 0],
                [0, 255, 0],
                [255, 255, 0],
                [0, 0, 255],
                [255, 0, 255],
                [0, 255, 255],
                [255, 255, 255],
            ],
            Mode::Color256 | Mode::True => {
                // xterm 6x6x6 cube plus 24 grays; fits Windows Terminal's 256 sixel registers.
                const L: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let mut p = Vec::with_capacity(240);
                for r in L {
                    for g in L {
                        for b in L {
                            p.push([r, g, b]);
                        }
                    }
                }
                p.extend((0..24).map(|i| [8 + 10 * i as u8; 3]));
                p
            }
        }
    }
}

/// Nearest-palette lookup on 15-bit RGB.
pub struct Lut {
    table: Vec<u8>,
}

impl Lut {
    pub fn new(pal: &[[u8; 3]]) -> Lut {
        let mut table = vec![0u8; 1 << 15];
        for (i, slot) in table.iter_mut().enumerate() {
            let r = (((i >> 10) & 31) << 3 | 4) as i32;
            let g = (((i >> 5) & 31) << 3 | 4) as i32;
            let b = ((i & 31) << 3 | 4) as i32;
            let mut best = (i32::MAX, 0);
            for (j, c) in pal.iter().enumerate() {
                let (dr, dg, db) = (r - c[0] as i32, g - c[1] as i32, b - c[2] as i32);
                let d = 3 * dr * dr + 4 * dg * dg + 2 * db * db;
                if d < best.0 {
                    best = (d, j);
                }
            }
            *slot = best.1 as u8;
        }
        Lut { table }
    }

    #[inline]
    pub fn get(&self, r: u8, g: u8, b: u8) -> u8 {
        self.table[((r as usize >> 3) << 10) | ((g as usize >> 3) << 5) | (b as usize >> 3)]
    }
}

/// A screen reduced to the wire colour mode: one byte per pixel, or RGB for true colour.
#[derive(Clone)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub mode: Mode,
    pub data: Vec<u8>,
}

pub struct Reducer {
    mode: Mode,
    lut: Option<Lut>,
}

impl Reducer {
    pub fn new(mode: Mode) -> Reducer {
        let lut = matches!(mode, Mode::Color16 | Mode::Color256).then(|| Lut::new(&mode.palette()));
        Reducer { mode, lut }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn reduce(&self, img: &Image) -> Plane {
        let (w, h) = (img.w, img.h);
        let mut data = Vec::with_capacity(w * h * self.mode.bpp());
        for y in 0..h {
            let row = &img.data[y * img.stride..y * img.stride + w * 4];
            match self.mode {
                Mode::True => {
                    for p in row.as_chunks::<4>().0 {
                        data.extend_from_slice(&[p[2], p[1], p[0]]);
                    }
                }
                Mode::Color16 | Mode::Color256 => {
                    let lut = self.lut.as_ref().unwrap();
                    data.extend(row.as_chunks::<4>().0.iter().map(|p| lut.get(p[2], p[1], p[0])));
                }
                Mode::Mono => {}
                Mode::Gray4 | Mode::Gray16 => {
                    let levels = if self.mode == Mode::Gray4 { 3 } else { 15 };
                    data.extend(row.as_chunks::<4>().0.iter().map(|p| {
                        let luma = (77 * p[2] as u32 + 150 * p[1] as u32 + 29 * p[0] as u32) >> 8;
                        ((luma * levels + 127) / 255) as u8
                    }));
                }
            }
        }
        if self.mode == Mode::Mono {
            // Text and lines on any background, not only dark on light (see `bilevel`).
            let luma: Vec<u8> = (0..h)
                .flat_map(|y| img.data[y * img.stride..y * img.stride + w * 4].as_chunks::<4>().0.iter())
                .map(|p| ((77 * p[2] as u32 + 150 * p[1] as u32 + 29 * p[0] as u32) >> 8) as u8)
                .collect();
            data = crate::bilevel::binarize(&luma, w, h);
        }
        Plane { w, h, mode: self.mode, data }
    }
}

/// Packs a rectangle of a plane into the wire form (bit-packed indices or RGB).
pub fn pack(p: &Plane, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
    let bpp = p.mode.bpp();
    let bits = p.mode.bits();
    if bits >= 8 {
        let mut out = Vec::with_capacity(w * h * bpp);
        for row in y..y + h {
            let s = (row * p.w + x) * bpp;
            out.extend_from_slice(&p.data[s..s + w * bpp]);
        }
        if p.mode == Mode::True {
            med_forward(&mut out, w, h);
        }
        return out;
    }
    let mut out = Vec::with_capacity((w * h * bits).div_ceil(8));
    let (mut acc, mut n) = (0u32, 0usize);
    for row in y..y + h {
        let s = row * p.w + x;
        for &v in &p.data[s..s + w] {
            acc = (acc << bits) | v as u32;
            n += bits;
            if n >= 8 {
                n -= 8;
                out.push((acc >> n) as u8);
            }
        }
    }
    if n > 0 {
        out.push((acc << (8 - n)) as u8);
    }
    out
}

/// The colours a session's true colour frames index, kept by agent and viewer alike for the
/// whole session, so the same text has the same bytes from frame to frame (which the stream's
/// compression finds again). Each frame says which of its 256 slots changed; a slot not used in
/// the frame being made is taken over when a new colour needs room.
pub struct Palette {
    keys: Vec<u32>,
    slot: Vec<u8>,
    colours: Vec<u32>,
    used: Vec<u32>,
    frame: u32,
}

const EMPTY: u32 = u32::MAX;
/// Hash slots for the 256 colours: sparse enough for short probes.
const SLOTS: usize = 1024;

fn key(px: &[u8]) -> u32 {
    (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32
}

fn rgb(k: u32) -> [u8; 3] {
    [(k >> 16) as u8, (k >> 8) as u8, k as u8]
}

impl Default for Palette {
    fn default() -> Palette {
        Palette { keys: vec![EMPTY; SLOTS], slot: vec![0; SLOTS], colours: Vec::with_capacity(256), used: vec![0; 256], frame: 0 }
    }
}

impl Palette {
    fn at(keys: &[u32], key: u32) -> usize {
        let mut i = (key.wrapping_mul(2_654_435_761) >> 22) as usize;
        while keys[i] != EMPTY && keys[i] != key {
            i = (i + 1) & (SLOTS - 1);
        }
        i
    }

    fn get(&self, key: u32) -> Option<u8> {
        let i = Palette::at(&self.keys, key);
        (self.keys[i] == key).then(|| self.slot[i])
    }

    fn rehash(&mut self) {
        self.keys.fill(EMPTY);
        for (n, &k) in self.colours.iter().enumerate() {
            let i = Palette::at(&self.keys, k);
            self.keys[i] = k;
            self.slot[i] = n as u8;
        }
    }

    /// The slot for `key` in this frame, taking a free one or one this frame does not use;
    /// None when all 256 are in use here.
    fn take(&mut self, key: u32, changed: &mut Vec<(u8, [u8; 3])>) -> Option<u8> {
        if let Some(n) = self.get(key) {
            self.used[n as usize] = self.frame;
            return Some(n);
        }
        let n = if self.colours.len() < 256 {
            let i = Palette::at(&self.keys, key);
            (self.keys[i], self.slot[i]) = (key, self.colours.len() as u8);
            self.colours.push(key);
            self.colours.len() - 1
        } else {
            let (n, _) = self.used.iter().enumerate().filter(|(_, u)| **u != self.frame).min_by_key(|(_, u)| **u)?;
            self.colours[n] = key;
            // A colour leaves the table: probes past its place would stop short, so all go in anew.
            self.rehash();
            n
        };
        self.used[n] = self.frame;
        changed.retain(|(s, _)| *s as usize != n);
        changed.push((n as u8, rgb(key)));
        Some(n as u8)
    }

    /// Packs true colour rectangles: each one's data starts with its form, 1 for slots of the
    /// palette (when its colours fit) and 0 for `pack`'s lossless prediction (photographs).
    /// Returns whether the viewer must forget its palette first (`fresh`, as for a whole new
    /// picture), the slots that changed, and the data.
    pub fn pack(&mut self, p: &Plane, rects: &[Rect], fresh: bool) -> (bool, Vec<(u8, [u8; 3])>, Vec<Vec<u8>>) {
        debug_assert_eq!(p.mode, Mode::True);
        if fresh {
            *self = Palette::default();
        }
        self.frame = self.frame.wrapping_add(1);
        let mut changed = Vec::new();
        let mut seen = Seen::default();
        let data = rects
            .iter()
            .map(|r| {
                // A rectangle of more than 256 colours keeps the prediction, without touching the palette.
                seen.clear();
                let mut last = EMPTY;
                let few = (r.y..r.y + r.h).all(|row| {
                    let s = (row * p.w + r.x) * 3;
                    p.data[s..s + r.w * 3].chunks_exact(3).all(|px| {
                        let k = key(px);
                        let fits = k == last || seen.add(k);
                        last = k;
                        fits
                    })
                });
                if !few || !seen.list.iter().all(|&k| self.take(k, &mut changed).is_some()) {
                    let mut d = vec![0];
                    d.extend(pack(p, r.x, r.y, r.w, r.h));
                    return d;
                }
                let mut d = Vec::with_capacity(1 + r.w * r.h);
                d.push(1);
                let (mut last, mut n) = (EMPTY, 0);
                for row in r.y..r.y + r.h {
                    let s = (row * p.w + r.x) * 3;
                    d.extend(p.data[s..s + r.w * 3].chunks_exact(3).map(|px| {
                        let k = key(px);
                        if k != last {
                            last = k;
                            n = self.get(k).unwrap_or(0);
                        }
                        n
                    }));
                }
                d
            })
            .collect();
        (fresh, changed, data)
    }
}

/// The distinct colours of one rectangle, up to 256.
struct Seen {
    keys: Vec<u32>,
    list: Vec<u32>,
}

impl Default for Seen {
    fn default() -> Seen {
        Seen { keys: vec![EMPTY; SLOTS], list: Vec::with_capacity(256) }
    }
}

impl Seen {
    fn clear(&mut self) {
        if !self.list.is_empty() {
            self.keys.fill(EMPTY);
            self.list.clear();
        }
    }

    /// Notes `k`; false when it would be the 257th colour.
    fn add(&mut self, k: u32) -> bool {
        let i = Palette::at(&self.keys, k);
        if self.keys[i] == k {
            return true;
        }
        if self.list.len() == 256 {
            return false;
        }
        self.keys[i] = k;
        self.list.push(k);
        true
    }
}

/// Whether `r` looks like a photograph or a film rather than text and drawings: in most of nine
/// 32x32 samples spread over it no colour covers a quarter of the pixels, where text, windows
/// and drawings have a background that covers most of theirs.
pub fn rich(p: &Plane, r: &Rect) -> bool {
    if !matches!(p.mode, Mode::True | Mode::Color256) {
        return false;
    }
    let bpp = p.mode.bpp();
    let (mut samples, mut many) = (0, 0);
    let mut keys = Vec::with_capacity(1024);
    for j in 0..3 {
        for i in 0..3 {
            let (sw, sh) = (32.min(r.w), 32.min(r.h));
            if sw * sh < 64 {
                continue;
            }
            // At a quarter, half and three quarters of the way: an area found by whole tiles has
            // some of its surroundings at the edges.
            let (x, y) = (r.x + (r.w - sw) * (i + 1) / 4, r.y + (r.h - sh) * (j + 1) / 4);
            keys.clear();
            for row in y..y + sh {
                let s = (row * p.w + x) * bpp;
                keys.extend(p.data[s..s + sw * bpp].chunks_exact(bpp).map(|px| if bpp == 3 { key(px) } else { px[0] as u32 }));
            }
            keys.sort_unstable();
            let top = keys.chunk_by(|a, b| a == b).map(<[u32]>::len).max().unwrap_or(0);
            samples += 1;
            many += (top * 4 < keys.len()) as usize;
        }
    }
    many * 2 > samples
}

/// The viewer's copy of the agent's `Palette`.
#[derive(Clone)]
pub struct Colours {
    rgb: Vec<Option<[u8; 3]>>,
}

impl Default for Colours {
    fn default() -> Colours {
        Colours { rgb: vec![None; 256] }
    }
}

impl Colours {
    pub fn update(&mut self, fresh: bool, changed: &[(u8, [u8; 3])]) {
        if fresh {
            self.rgb.fill(None);
        }
        for &(n, c) in changed {
            self.rgb[n as usize] = Some(c);
        }
    }

    /// Inverse of one rectangle of `Palette::pack`: RGB, three bytes per pixel. None for data
    /// that is cut short or names a colour this side was never given (the two sides went apart:
    /// a new whole picture brings them together again).
    pub fn unpack(&self, data: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
        match data.split_first()? {
            (0, rest) => unpack(rest, Mode::True, w, h),
            (1, rest) => {
                let mut out = Vec::with_capacity(w * h * 3);
                for &i in rest.get(..w * h)? {
                    out.extend_from_slice(self.rgb[i as usize].as_ref()?);
                }
                Some(out)
            }
            _ => None,
        }
    }
}

fn med(a: u8, b: u8, c: u8) -> u8 {
    if c >= a.max(b) {
        a.min(b)
    } else if c <= a.min(b) {
        a.max(b)
    } else {
        a.wrapping_add(b).wrapping_sub(c)
    }
}

/// LOCO-I median prediction per channel, with green removed from red and blue.
/// Flat areas and gradients become runs of zeros that zstd compresses well.
fn med_forward(px: &mut [u8], w: usize, h: usize) {
    for p in px.as_chunks_mut::<3>().0 {
        p[0] = p[0].wrapping_sub(p[1]);
        p[2] = p[2].wrapping_sub(p[1]);
    }
    let src = px.to_vec();
    for y in 0..h {
        for x in 0..w {
            for ch in 0..3 {
                let at = |xx: usize, yy: usize| src[(yy * w + xx) * 3 + ch];
                let a = if x > 0 { at(x - 1, y) } else { 0 };
                let b = if y > 0 { at(x, y - 1) } else { 0 };
                let c = if x > 0 && y > 0 { at(x - 1, y - 1) } else { 0 };
                let i = (y * w + x) * 3 + ch;
                px[i] = src[i].wrapping_sub(med(a, b, c));
            }
        }
    }
}

fn med_inverse(px: &mut [u8], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            for ch in 0..3 {
                let at = |xx: usize, yy: usize| px[(yy * w + xx) * 3 + ch];
                let a = if x > 0 { at(x - 1, y) } else { 0 };
                let b = if y > 0 { at(x, y - 1) } else { 0 };
                let c = if x > 0 && y > 0 { at(x - 1, y - 1) } else { 0 };
                let i = (y * w + x) * 3 + ch;
                px[i] = px[i].wrapping_add(med(a, b, c));
            }
        }
    }
    for p in px.as_chunks_mut::<3>().0 {
        p[0] = p[0].wrapping_add(p[1]);
        p[2] = p[2].wrapping_add(p[1]);
    }
}

/// Inverse of `pack` for a `w` x `h` rectangle: one byte per pixel (three for true colour).
pub fn unpack(data: &[u8], mode: Mode, w: usize, h: usize) -> Option<Vec<u8>> {
    let bits = mode.bits();
    let count = w * h;
    if bits >= 8 {
        let len = count * mode.bpp();
        let mut out = data.get(..len)?.to_vec();
        if mode == Mode::True {
            med_inverse(&mut out, w, h);
        }
        return Some(out);
    }
    if data.len() * 8 < count * bits {
        return None;
    }
    let mask = (1u8 << bits) - 1;
    let per = 8 / bits;
    Some((0..count).map(|i| (data[i / per] >> (8 - bits * (i % per + 1))) & mask).collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

const TILE: usize = 32;

/// Returns the tight changed box inside every changed 32x32 tile.
pub fn diff(prev: Option<&Plane>, cur: &Plane) -> Vec<Rect> {
    let bpp = cur.mode.bpp();
    let same_shape = prev.is_some_and(|p| p.w == cur.w && p.h == cur.h && p.mode == cur.mode);
    let mut out = Vec::new();
    for ty in (0..cur.h).step_by(TILE) {
        let th = TILE.min(cur.h - ty);
        for tx in (0..cur.w).step_by(TILE) {
            let tw = TILE.min(cur.w - tx);
            if !same_shape {
                out.push(Rect { x: tx, y: ty, w: tw, h: th });
                continue;
            }
            let prev = prev.unwrap();
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            for row in ty..ty + th {
                let s = (row * cur.w + tx) * bpp;
                let a = &cur.data[s..s + tw * bpp];
                let b = &prev.data[s..s + tw * bpp];
                if a == b {
                    continue;
                }
                let first = (0..tw).find(|&i| a[i * bpp..(i + 1) * bpp] != b[i * bpp..(i + 1) * bpp]).unwrap();
                let last = (0..tw).rev().find(|&i| a[i * bpp..(i + 1) * bpp] != b[i * bpp..(i + 1) * bpp]).unwrap();
                x0 = x0.min(first);
                x1 = x1.max(last);
                y0 = y0.min(row);
                y1 = row;
            }
            if x0 != usize::MAX {
                out.push(Rect { x: tx + x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(mode: Mode, w: usize, h: usize, f: impl Fn(usize) -> u8) -> Plane {
        let data = (0..w * h * mode.bpp()).map(f).collect();
        Plane { w, h, mode, data }
    }

    #[test]
    fn pack_roundtrip_all_modes() {
        for mode in Mode::ALL {
            let max = if mode == Mode::True { 255 } else { (mode.palette().len() - 1) as u8 };
            let p = plane(mode, 37, 11, |i| (i * 7 % (max as usize + 1)) as u8);
            let packed = pack(&p, 3, 2, 29, 7);
            let back = unpack(&packed, mode, 29, 7).unwrap();
            let bpp = mode.bpp();
            for row in 0..7 {
                let s = ((row + 2) * 37 + 3) * bpp;
                assert_eq!(&back[row * 29 * bpp..(row + 1) * 29 * bpp], &p.data[s..s + 29 * bpp], "{mode:?}");
            }
        }
    }

    #[test]
    fn paletted_rectangles_round_trip_keep_their_slots_and_photos_keep_the_prediction() {
        let (w, h) = (96, 40);
        // Left: text-like, four colours; right: a different colour in every pixel.
        let p = Plane {
            w,
            h,
            mode: Mode::True,
            data: (0..w * h)
                .flat_map(|i| {
                    let (x, y) = (i % w, i / w);
                    if x < 64 { [[250, 250, 250], [20, 20, 20], [120, 120, 130], [0, 90, 200]][(x * 7 + y * 3) % 5 % 4] } else { [(i % 251) as u8, (i / 251) as u8, (x * y) as u8] }
                })
                .collect(),
        };
        let rects = [Rect { x: 0, y: 0, w: 32, h: 32 }, Rect { x: 32, y: 8, w: 32, h: 32 }, Rect { x: 64, y: 0, w: 32, h: 32 }];
        let (mut agent, mut viewer) = (Palette::default(), Colours::default());
        let (fresh, changed, data) = agent.pack(&p, &rects, true);
        assert!(fresh);
        assert_eq!(changed.len(), 4);
        assert_eq!([data[0][0], data[1][0], data[2][0]], [1, 1, 0]);
        viewer.update(fresh, &changed);
        for (r, d) in rects.iter().zip(&data) {
            let back = viewer.unpack(d, r.w, r.h).unwrap();
            for row in 0..r.h {
                let s = ((r.y + row) * w + r.x) * 3;
                assert_eq!(&back[row * r.w * 3..(row + 1) * r.w * 3], &p.data[s..s + r.w * 3]);
            }
        }
        // The same text again: the same bytes, and no colour to send.
        let (fresh, changed, again) = agent.pack(&p, &rects[..1], false);
        assert!(!fresh && changed.is_empty());
        assert_eq!(again[0], data[0]);
        // A viewer that lost the palette cannot read it, rather than showing wrong colours.
        assert!(Colours::default().unpack(&data[0], 32, 32).is_none());
        assert!(!rich(&p, &Rect { x: 0, y: 0, w: 64, h: 40 }));
        assert!(rich(&p, &Rect { x: 64, y: 0, w: 32, h: 40 }));
    }

    #[test]
    fn a_full_palette_gives_up_the_slots_this_frame_does_not_use() {
        let mut agent = Palette::default();
        let mut viewer = Colours::default();
        // 300 one-colour rectangles in turn, one frame each: the palette keeps the last 256.
        for c in 0..300u32 {
            let p = Plane { w: 4, h: 4, mode: Mode::True, data: [(c >> 8) as u8, c as u8, 7].repeat(16) };
            let (fresh, changed, data) = agent.pack(&p, &[Rect { x: 0, y: 0, w: 4, h: 4 }], c == 0);
            viewer.update(fresh, &changed);
            assert_eq!(viewer.unpack(&data[0], 4, 4).unwrap(), p.data);
        }
    }

    #[test]
    fn diff_finds_tight_box() {
        let a = plane(Mode::Color256, 100, 70, |_| 0);
        let mut b = plane(Mode::Color256, 100, 70, |_| 0);
        b.data[40 * 100 + 50] = 9;
        b.data[41 * 100 + 52] = 9;
        assert_eq!(diff(Some(&a), &b), vec![Rect { x: 50, y: 40, w: 3, h: 2 }]);
        assert!(diff(Some(&a), &a).is_empty());
        assert_eq!(diff(None, &a).len(), 4 * 3);
    }

    #[test]
    fn palettes_fit_sixel_registers() {
        for mode in Mode::ALL {
            assert!(mode.palette().len() <= 256);
        }
        let lut = Lut::new(&Mode::Gray16.palette());
        for i in 0..16u8 {
            assert_eq!(lut.get(i * 17, i * 17, i * 17), i);
        }
    }
}
