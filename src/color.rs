use crate::shared::Image;

/// Palette slots that changed, each with its colour, as a frame carries them.
pub type Slots = Vec<(u16, [u8; 3])>;

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
            Mode::Mono => n!("Black and white"),
            Mode::Gray4 => n!("4 greys"),
            Mode::Gray16 => n!("16 greys"),
            Mode::Color16 => n!("16 colours"),
            Mode::Color256 => n!("256 colours"),
            Mode::True => n!("Full colour"),
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

/// BGRA pixels to RGB, four at a time as three words: byte by byte it was most of a frame's work.
fn true_row(row: &[[u8; 4]], out: &mut [u8]) {
    let rgb = |p: &[u8; 4]| u32::from_le_bytes(*p).swap_bytes() >> 8;
    let (quads, rest) = row.as_chunks::<4>();
    let (words, tail) = out.as_chunks_mut::<12>();
    for (q, o) in quads.iter().zip(words.iter_mut()) {
        let [a, b, c, d] = [rgb(&q[0]), rgb(&q[1]), rgb(&q[2]), rgb(&q[3])];
        o[0..4].copy_from_slice(&(a | b << 24).to_le_bytes());
        o[4..8].copy_from_slice(&(b >> 8 | c << 16).to_le_bytes());
        o[8..12].copy_from_slice(&(c >> 16 | d << 8).to_le_bytes());
    }
    for (p, o) in rest.iter().zip(tail.as_chunks_mut::<3>().0) {
        *o = [p[2], p[1], p[0]];
    }
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

    #[cfg(test)]
    pub fn reduce(&self, img: &Image) -> Plane {
        self.reduce_into(img, Vec::new())
    }

    /// The same, filling `data` (a buffer done with) instead of a new one.
    pub fn reduce_into(&self, img: &Image, mut data: Vec<u8>) -> Plane {
        let (w, h) = (img.w, img.h);
        if self.mode == Mode::Mono {
            // Text and lines on any background, not only dark on light (see `bilevel`).
            let luma: Vec<u8> = (0..h)
                .flat_map(|y| img.data[y * img.stride..y * img.stride + w * 4].as_chunks::<4>().0.iter())
                .map(|p| ((77 * p[2] as u32 + 150 * p[1] as u32 + 29 * p[0] as u32) >> 8) as u8)
                .collect();
            return Plane { w, h, mode: self.mode, data: crate::bilevel::binarize(&luma, w, h) };
        }
        let bpp = self.mode.bpp();
        // Filled row by row in place: pushed a pixel at a time it took a full-screen frame 4 ms.
        data.clear();
        data.resize(w * h * bpp, 0);
        for (y, out) in data.chunks_exact_mut((w * bpp).max(1)).enumerate() {
            let row = img.data[y * img.stride..y * img.stride + w * 4].as_chunks::<4>().0;
            match self.mode {
                Mode::True => true_row(row, out),
                Mode::Color16 | Mode::Color256 => {
                    let lut = self.lut.as_ref().unwrap();
                    for (d, p) in out.iter_mut().zip(row) {
                        *d = lut.get(p[2], p[1], p[0]);
                    }
                }
                Mode::Gray4 | Mode::Gray16 => {
                    let levels = if self.mode == Mode::Gray4 { 3 } else { 15 };
                    for (d, p) in out.iter_mut().zip(row) {
                        let luma = (77 * p[2] as u32 + 150 * p[1] as u32 + 29 * p[0] as u32) >> 8;
                        *d = ((luma * levels + 127) / 255) as u8;
                    }
                }
                Mode::Mono => unreachable!(),
            }
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
/// compression finds again). Each frame says which of its slots changed; a slot not used in the
/// frame being made is taken over when a new colour needs room. 256 slots, or `WIDE` for a viewer
/// that reads them (`proto::FORMAT_WIDE`): text smoothed in grey shades, or in colour fringes,
/// often has more than 256 colours on a screen.
pub struct Palette {
    keys: Vec<u32>,
    slot: Vec<u16>,
    colours: Vec<u32>,
    used: Vec<u32>,
    frame: u32,
}

const EMPTY: u32 = u32::MAX;
/// The most slots a palette has.
pub const WIDE: usize = 4096;
/// Hash places for the colours: sparse enough for short probes.
const SLOTS: usize = 8192;

fn key(px: &[u8]) -> u32 {
    (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32
}

fn rgb(k: u32) -> [u8; 3] {
    [(k >> 16) as u8, (k >> 8) as u8, k as u8]
}

/// How many colours a rectangle of `area` pixels may have and still go as slots of the palette:
/// text and drawings have few for their size (a band of smoothed text one in 150 pixels or so),
/// photographs (better predicted) many.
fn few(area: usize, wide: bool) -> usize {
    if wide { (area / 64).clamp(256, WIDE) } else { 256 }
}

impl Default for Palette {
    fn default() -> Palette {
        Palette { keys: vec![EMPTY; SLOTS], slot: vec![0; SLOTS], colours: Vec::with_capacity(256), used: vec![0; WIDE], frame: 0 }
    }
}

impl Palette {
    fn home(key: u32) -> usize {
        (key.wrapping_mul(2_654_435_761) >> 19) as usize
    }

    fn at(keys: &[u32], key: u32) -> usize {
        let mut i = Palette::home(key);
        while keys[i] != EMPTY && keys[i] != key {
            i = (i + 1) & (SLOTS - 1);
        }
        i
    }

    fn get(&self, key: u32) -> Option<u16> {
        let i = Palette::at(&self.keys, key);
        (self.keys[i] == key).then(|| self.slot[i])
    }

    /// Takes `key` out of the table, moving back the ones probed past it.
    fn remove(&mut self, key: u32) {
        let mut i = Palette::at(&self.keys, key);
        if self.keys[i] != key {
            return;
        }
        self.keys[i] = EMPTY;
        let mut j = i;
        loop {
            j = (j + 1) & (SLOTS - 1);
            if self.keys[j] == EMPTY {
                return;
            }
            let h = Palette::home(self.keys[j]);
            // It stays where its probe would still reach it.
            let reached = if i <= j { i < h && h <= j } else { i < h || h <= j };
            if !reached {
                (self.keys[i], self.slot[i]) = (self.keys[j], self.slot[j]);
                self.keys[j] = EMPTY;
                i = j;
            }
        }
    }

    /// The slot for `key` in this frame, taking a free one or one this frame does not use;
    /// None when all `cap` are in use here.
    fn take(&mut self, key: u32, cap: usize, changed: &mut Slots) -> Option<u16> {
        if let Some(n) = self.get(key) {
            self.used[n as usize] = self.frame;
            return Some(n);
        }
        // Slots that take one byte (below 255 in a wide palette) go first, taken over from
        // colours this frame does not use; the others only when this frame needs more.
        let low = if cap > 256 { 255 } else { 256 };
        let stale = |used: &[u32], frame: u32| used.iter().enumerate().filter(|(_, u)| **u != frame).min_by_key(|(_, u)| **u).map(|(n, _)| n);
        let n = if self.colours.len() < low {
            self.colours.push(key);
            self.colours.len() - 1
        } else if let Some(n) = stale(&self.used[..low], self.frame) {
            self.remove(self.colours[n]);
            self.colours[n] = key;
            n
        } else if self.colours.len() < cap {
            self.colours.push(key);
            self.colours.len() - 1
        } else {
            let n = stale(&self.used[..cap], self.frame)?;
            self.remove(self.colours[n]);
            self.colours[n] = key;
            n
        };
        let i = Palette::at(&self.keys, key);
        (self.keys[i], self.slot[i]) = (key, n as u16);
        self.used[n] = self.frame;
        changed.retain(|(s, _)| *s as usize != n);
        changed.push((n as u16, rgb(key)));
        Some(n as u16)
    }

    /// Packs true colour rectangles: each one's data starts with its form, 1 for slots of the
    /// palette (one byte each), 2 for slots of a wide palette (one byte below 255; else 255 and
    /// two bytes), 0 for `pack`'s lossless prediction (photographs). Returns whether the viewer
    /// must forget its palette first (`fresh`, as for a whole new picture), the slots that
    /// changed, and the data.
    pub fn pack(&mut self, p: &Plane, rects: &[Rect], fresh: bool, wide: bool) -> (bool, Slots, Vec<Vec<u8>>) {
        debug_assert_eq!(p.mode, Mode::True);
        let cap = if wide { WIDE } else { 256 };
        // Back to 256 slots (another viewer's settings): the palette starts over.
        let fresh = fresh || self.colours.len() > cap;
        if fresh {
            *self = Palette::default();
        }
        self.frame = self.frame.wrapping_add(1);
        let mut changed = Vec::new();
        let mut seen = Seen::default();
        let data = rects
            .iter()
            .map(|r| {
                // A rectangle of too many colours keeps the prediction, without touching the palette.
                seen.clear();
                seen.limit = few(r.w * r.h, wide);
                let mut last = EMPTY;
                let fits = (r.y..r.y + r.h).all(|row| {
                    let s = (row * p.w + r.x) * 3;
                    p.data[s..s + r.w * 3].as_chunks::<3>().0.iter().all(|px| {
                        let k = key(px);
                        let fits = k == last || seen.add(k);
                        last = k;
                        fits
                    })
                });
                if !fits || !seen.list.iter().all(|&k| self.take(k, cap, &mut changed).is_some()) {
                    let mut d = vec![0];
                    d.extend(pack(p, r.x, r.y, r.w, r.h));
                    return d;
                }
                let high = seen.list.iter().any(|&k| self.get(k).is_some_and(|n| n > 255));
                let mut d = Vec::with_capacity(1 + r.w * r.h);
                d.push(if high { 2 } else { 1 });
                let (mut last, mut n) = (EMPTY, 0);
                for row in r.y..r.y + r.h {
                    let s = (row * p.w + r.x) * 3;
                    for px in p.data[s..s + r.w * 3].as_chunks::<3>().0 {
                        let k = key(px);
                        if k != last {
                            last = k;
                            n = self.get(k).unwrap_or(0);
                        }
                        if !high || n < 255 {
                            d.push(n as u8);
                        } else {
                            d.push(255);
                            d.extend_from_slice(&n.to_le_bytes());
                        }
                    }
                }
                d
            })
            .collect();
        (fresh, changed, data)
    }
}

/// The distinct colours of one rectangle, up to `limit`.
struct Seen {
    keys: Vec<u32>,
    list: Vec<u32>,
    limit: usize,
}

impl Default for Seen {
    fn default() -> Seen {
        Seen { keys: vec![EMPTY; SLOTS], list: Vec::with_capacity(256), limit: 256 }
    }
}

impl Seen {
    fn clear(&mut self) {
        // Only the places in use, found before any is emptied.
        let places: Vec<usize> = self.list.iter().map(|&k| Palette::at(&self.keys, k)).collect();
        for i in places {
            self.keys[i] = EMPTY;
        }
        self.list.clear();
    }

    /// Notes `k`; false when it would be one colour over the limit.
    fn add(&mut self, k: u32) -> bool {
        let i = Palette::at(&self.keys, k);
        if self.keys[i] == k {
            return true;
        }
        if self.list.len() >= self.limit {
            return false;
        }
        self.keys[i] = k;
        self.list.push(k);
        true
    }
}

/// Changed areas side by side in one band of tile rows, joined into one rectangle each run, so
/// that rows of text lie whole in a rectangle's data, where the stream's compression finds the
/// words again (cut into tiles, every row of a glyph is cut short). A run ends at a gap wider than
/// a tile, and in true colour with a palette where it would hold too many colours to use it.
pub fn join_rows(p: &Plane, rects: &[Rect], palette: bool, wide: bool) -> Vec<Rect> {
    let mut sorted = rects.to_vec();
    sorted.sort_by_key(|r| (r.y / TILE, r.x));
    let mut seen = Seen::default();
    // Adds the colours of an area to `seen`; false past 256.
    let colours = |seen: &mut Seen, x: usize, y: usize, w: usize, h: usize| {
        (y..y + h).all(|row| p.data[(row * p.w + x) * 3..(row * p.w + x + w) * 3].as_chunks::<3>().0.iter().all(|px| seen.add(key(px))))
    };
    let mut out: Vec<Rect> = Vec::with_capacity(sorted.len());
    for r in sorted {
        if let Some(last) = out.last_mut()
            && last.y / TILE == r.y / TILE
            && r.x >= last.x + last.w
            && r.x <= last.x + last.w + TILE
        {
            let (top, bottom) = (last.y.min(r.y), (last.y + last.h).max(r.y + r.h));
            let joined = Rect { x: last.x, y: top, w: r.x + r.w - last.x, h: bottom - top };
            seen.limit = few(joined.w * joined.h, wide);
            // In true colour, the colours the joined rectangle adds: the new part, and the rows
            // it grew by over the run so far.
            let fits = !palette
                || (colours(&mut seen, last.x + last.w, top, joined.x + joined.w - last.x - last.w, joined.h)
                    && colours(&mut seen, last.x, top, last.w, last.y - top)
                    && colours(&mut seen, last.x, last.y + last.h, last.w, bottom - last.y - last.h));
            if fits {
                *last = joined;
                continue;
            }
        }
        if palette {
            seen.clear();
            seen.limit = few(r.w * r.h, wide);
            colours(&mut seen, r.x, r.y, r.w, r.h);
        }
        out.push(r);
    }
    out
}

/// Whether `r` of the captured picture looks like a photograph or a film rather than text and
/// drawings: in most of nine 32x32 samples spread over it no colour covers a quarter of the
/// pixels, where text, windows and drawings have a background that covers most of theirs. The
/// capture is judged, not the picture made of it, whose fewer colours would hide a photograph.
pub fn rich(img: &Image, r: &Rect) -> bool {
    let (mut samples, mut many) = (0, 0);
    let mut keys = Vec::with_capacity(1024);
    for j in 0..3 {
        for i in 0..3 {
            let (sw, sh) = (32.min(r.w), 32.min(r.h));
            if sw * sh < 64 || r.x + r.w > img.w || r.y + r.h > img.h {
                continue;
            }
            // At a quarter, half and three quarters of the way: an area found by whole tiles has
            // some of its surroundings at the edges.
            let (x, y) = (r.x + (r.w - sw) * (i + 1) / 4, r.y + (r.h - sh) * (j + 1) / 4);
            keys.clear();
            for row in y..y + sh {
                let s = row * img.stride + x * 4;
                keys.extend(img.data[s..s + sw * 4].as_chunks::<4>().0.iter().map(|px| (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32));
            }
            keys.sort_unstable();
            let top = keys.chunk_by(|a, b| a == b).map(<[u32]>::len).max().unwrap_or(0);
            samples += 1;
            many += (top * 4 < keys.len()) as usize;
        }
    }
    many * 2 > samples
}

/// A quicker look at one small area (a cell of 64x64): whether 64 of its pixels, spread evenly,
/// have no colour that covers a quarter of them, as in a photograph.
pub fn photo_cell(img: &Image, r: &Rect) -> bool {
    if r.w < 8 || r.h < 8 || r.x + r.w > img.w || r.y + r.h > img.h {
        return false;
    }
    let mut keys = [0u32; 64];
    for (k, key) in keys.iter_mut().enumerate() {
        let (x, y) = (r.x + r.w * (2 * (k % 8) + 1) / 16, r.y + r.h * (2 * (k / 8) + 1) / 16);
        let px = &img.data[y * img.stride + x * 4..][..3];
        *key = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
    }
    keys.sort_unstable();
    keys.chunk_by(|a, b| a == b).map(<[u32]>::len).max().unwrap_or(0) * 4 < keys.len()
}

/// The viewer's copy of the agent's `Palette`.
#[derive(Clone)]
pub struct Colours {
    rgb: Vec<Option<[u8; 3]>>,
}

impl Default for Colours {
    fn default() -> Colours {
        Colours { rgb: vec![None; WIDE] }
    }
}

impl Colours {
    pub fn update(&mut self, fresh: bool, changed: &[(u16, [u8; 3])]) {
        if fresh {
            self.rgb.fill(None);
        }
        for &(n, c) in changed {
            if let Some(slot) = self.rgb.get_mut(n as usize) {
                *slot = Some(c);
            }
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
            (2, rest) => {
                let mut out = Vec::with_capacity(w * h * 3);
                let mut at = 0;
                for _ in 0..w * h {
                    let mut n = *rest.get(at)? as usize;
                    at += 1;
                    if n == 255 {
                        n = u16::from_le_bytes([*rest.get(at)?, *rest.get(at + 1)?]) as usize;
                        at += 2;
                    }
                    out.extend_from_slice(self.rgb.get(n)?.as_ref()?);
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
    let line = cur.w * bpp;
    let mut rows = Vec::with_capacity(TILE);
    for ty in (0..cur.h).step_by(TILE) {
        let th = TILE.min(cur.h - ty);
        if let Some(prev) = prev.filter(|_| same_shape) {
            // Most rows are as they were: each is looked at whole once, not a tile at a time.
            rows.clear();
            rows.extend((ty..ty + th).filter(|&r| cur.data[r * line..(r + 1) * line] != prev.data[r * line..(r + 1) * line]));
            if rows.is_empty() {
                continue;
            }
        }
        for tx in (0..cur.w).step_by(TILE) {
            let tw = TILE.min(cur.w - tx);
            if !same_shape {
                out.push(Rect { x: tx, y: ty, w: tw, h: th });
                continue;
            }
            let prev = prev.unwrap();
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            for &row in &rows {
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
    fn a_photograph_cell_is_told_from_text() {
        let (w, h) = (128, 64);
        let noisy = (0..w * h * 4).map(|i| ((i as u64).wrapping_mul(2_654_435_761) >> 11) as u8).collect();
        let photo = Image { w, h, stride: w * 4, data: noisy };
        // Dark letters on a light page: the page covers most of it.
        let text = Image { w, h, stride: w * 4, data: (0..w * h * 4).map(|i| if (i / 4) % 7 == 0 { 20 } else { 240 }).collect() };
        let cell = Rect { x: 64, y: 0, w: 64, h: 64 };
        assert!(photo_cell(&photo, &cell));
        assert!(!photo_cell(&text, &cell));
        assert!(!photo_cell(&photo, &Rect { x: 100, y: 0, w: 64, h: 64 }), "out of the picture");
    }

    #[test]
    fn changes_side_by_side_join_along_their_band() {
        let r = |x, y, w, h| Rect { x, y, w, h };
        let gray = plane(Mode::Gray16, 256, 96, |_| 0);
        // Two tiles touching, one a tile away, one further, one in the next band.
        let rects = [r(0, 5, 32, 10), r(32, 2, 32, 20), r(96, 6, 10, 10), r(170, 6, 10, 10), r(0, 40, 32, 8)];
        assert_eq!(join_rows(&gray, &rects, false, false), vec![r(0, 2, 106, 20), r(170, 6, 10, 10), r(0, 40, 32, 8)]);
        // In true colour with a palette, a run stops where it would hold more than 256 colours.
        let busy = plane(Mode::True, 256, 32, |i| if (i / 3) % 256 >= 64 { (i.wrapping_mul(2_654_435_761) >> 13) as u8 } else { 200 });
        let joined = join_rows(&busy, &[r(0, 0, 32, 32), r(32, 0, 32, 32), r(64, 0, 32, 32), r(96, 0, 32, 32)], true, false);
        assert_eq!(joined[0], r(0, 0, 64, 32), "the plain part joins: {joined:?}");
        assert!(joined.len() > 1, "the busy part stays apart");
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
        let (fresh, changed, data) = agent.pack(&p, &rects, true, false);
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
        let (fresh, changed, again) = agent.pack(&p, &rects[..1], false, false);
        assert!(!fresh && changed.is_empty());
        assert_eq!(again[0], data[0]);
        // A viewer that lost the palette cannot read it, rather than showing wrong colours.
        assert!(Colours::default().unpack(&data[0], 32, 32).is_none());
        let img = Image { w, h, stride: w * 4, data: p.data.as_chunks::<3>().0.iter().flat_map(|c| [c[2], c[1], c[0], 255]).collect() };
        assert!(!rich(&img, &Rect { x: 0, y: 0, w: 64, h: 40 }));
        assert!(rich(&img, &Rect { x: 64, y: 0, w: 32, h: 40 }));
    }

    #[test]
    fn a_full_palette_gives_up_the_slots_this_frame_does_not_use() {
        let mut agent = Palette::default();
        let mut viewer = Colours::default();
        // 300 one-colour rectangles in turn, one frame each: the palette keeps the last 256.
        for c in 0..300u32 {
            let p = Plane { w: 4, h: 4, mode: Mode::True, data: [(c >> 8) as u8, c as u8, 7].repeat(16) };
            let (fresh, changed, data) = agent.pack(&p, &[Rect { x: 0, y: 0, w: 4, h: 4 }], c == 0, false);
            viewer.update(fresh, &changed);
            assert_eq!(viewer.unpack(&data[0], 4, 4).unwrap(), p.data);
        }
    }

    #[test]
    fn text_in_many_shades_takes_a_wide_palette() {
        // Text smoothed in 600 grey-blue shades on white: few colours for its size.
        let (w, h) = (1024, 64);
        let p = Plane {
            w,
            h,
            mode: Mode::True,
            data: (0..w * h)
                .flat_map(|i| if i % 7 < 4 { [255, 255, 255] } else { let s = (i * 37 % 600) as u16; [(s / 3) as u8, (s / 3) as u8, (s % 3 * 60) as u8] })
                .collect(),
        };
        let rects = [Rect { x: 0, y: 0, w, h }];
        // A viewer of 256 slots gets it predicted; one that reads more, as slots.
        let (_, _, narrow) = Palette::default().pack(&p, &rects, true, false);
        assert_eq!(narrow[0][0], 0);
        let (mut agent, mut viewer) = (Palette::default(), Colours::default());
        let (fresh, changed, data) = agent.pack(&p, &rects, true, true);
        assert_eq!(data[0][0], 2);
        assert!(changed.iter().any(|(n, _)| *n > 255) && changed.len() <= WIDE);
        viewer.update(fresh, &changed);
        assert_eq!(viewer.unpack(&data[0], w, h).unwrap(), p.data);
        assert!(data[0].len() < w * h * 3 / 2, "{} bytes", data[0].len());
        // A photograph's band, as many colours for its size as smoothed text has few: predicted.
        let photo = Plane { w: 854, h: 32, mode: Mode::True, data: (0..854 * 32u32).flat_map(|i| { let c = i % 1500; [(c >> 4) as u8, (c * 7) as u8, (c * 13) as u8] }).collect() };
        let (_, _, d) = Palette::default().pack(&photo, &[Rect { x: 0, y: 0, w: 854, h: 32 }], true, true);
        assert_eq!(d[0][0], 0);
        // Back to 256 slots: the palette starts over.
        let (fresh, ..) = agent.pack(&p, &[Rect { x: 0, y: 0, w: 8, h: 8 }], false, false);
        assert!(fresh);
    }

    #[test]
    fn a_wide_palette_takes_over_unused_slots_without_losing_any() {
        let mut agent = Palette::default();
        let mut viewer = Colours::default();
        // More colours than slots over a few frames (1000 new ones each): every picture reads back.
        for f in 0..6u32 {
            let p = Plane { w: 256, h: 256, mode: Mode::True, data: (0..256 * 256u32).flat_map(|i| { let c = f * 1000 + i % 1000; [(c >> 16) as u8, (c >> 8) as u8, c as u8] }).collect() };
            let (fresh, changed, data) = agent.pack(&p, &[Rect { x: 0, y: 0, w: 256, h: 256 }], f == 0, true);
            assert_eq!(data[0][0], 2, "frame {f} as slots");
            viewer.update(fresh, &changed);
            assert_eq!(viewer.unpack(&data[0], 256, 256).unwrap(), p.data, "frame {f}");
        }
    }

    /// Reduced and compared the plain way, a pixel at a time, to check the quicker ones by.
    fn reduce_by_pixel(red: &Reducer, img: &Image) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..img.h {
            for x in 0..img.w {
                let p = &img.data[y * img.stride + x * 4..][..4];
                let (r, g, b) = (p[2], p[1], p[0]);
                let luma = (77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8;
                match red.mode {
                    Mode::True => out.extend_from_slice(&[r, g, b]),
                    Mode::Color16 | Mode::Color256 => out.push(red.lut.as_ref().unwrap().get(r, g, b)),
                    Mode::Gray4 => out.push(((luma * 3 + 127) / 255) as u8),
                    Mode::Gray16 => out.push(((luma * 15 + 127) / 255) as u8),
                    Mode::Mono => unreachable!(),
                }
            }
        }
        out
    }

    fn diff_by_pixel(prev: &Plane, cur: &Plane) -> Vec<Rect> {
        let bpp = cur.mode.bpp();
        let mut out = Vec::new();
        for ty in (0..cur.h).step_by(TILE) {
            for tx in (0..cur.w).step_by(TILE) {
                let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
                for y in ty..(ty + TILE).min(cur.h) {
                    for x in tx..(tx + TILE).min(cur.w) {
                        let i = (y * cur.w + x) * bpp;
                        if cur.data[i..i + bpp] != prev.data[i..i + bpp] {
                            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                        }
                    }
                }
                if x0 != usize::MAX {
                    out.push(Rect { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 });
                }
            }
        }
        out
    }

    #[test]
    fn reduce_and_diff_match_a_pixel_by_pixel_look() {
        let mut seed = 7u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed >> 8
        };
        // Not a whole number of tiles, rows with room after them.
        let (w, h, stride) = (101, 67, 101 * 4 + 12);
        let mut img = Image { w, h, stride, data: (0..stride * h).map(|_| next() as u8).collect() };
        for mode in [Mode::Gray4, Mode::Gray16, Mode::Color16, Mode::Color256, Mode::True] {
            let red = Reducer::new(mode);
            let before = red.reduce(&img);
            assert_eq!(before.data, reduce_by_pixel(&red, &img), "{mode:?}");
            for _ in 0..20 {
                let at = (next() as usize % h) * stride + (next() as usize % w) * 4;
                img.data[at + next() as usize % 3] ^= 0x80;
            }
            for y in 40..47 {
                img.data[y * stride + 64 * 4..y * stride + 90 * 4].fill(next() as u8);
            }
            let after = red.reduce(&img);
            assert_eq!(after.data, reduce_by_pixel(&red, &img), "{mode:?}");
            assert_eq!(diff(Some(&before), &after), diff_by_pixel(&before, &after), "{mode:?}");
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
