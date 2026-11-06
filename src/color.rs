use crate::capture::Image;

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
                    for p in row.chunks_exact(4) {
                        data.extend_from_slice(&[p[2], p[1], p[0]]);
                    }
                }
                Mode::Color16 | Mode::Color256 => {
                    let lut = self.lut.as_ref().unwrap();
                    data.extend(row.chunks_exact(4).map(|p| lut.get(p[2], p[1], p[0])));
                }
                Mode::Mono | Mode::Gray4 | Mode::Gray16 => {
                    let levels = match self.mode {
                        Mode::Mono => 1,
                        Mode::Gray4 => 3,
                        _ => 15,
                    };
                    data.extend(row.chunks_exact(4).map(|p| {
                        let luma = (77 * p[2] as u32 + 150 * p[1] as u32 + 29 * p[0] as u32) >> 8;
                        ((luma * levels + 127) / 255) as u8
                    }));
                }
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
    for p in px.chunks_exact_mut(3) {
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
    for p in px.chunks_exact_mut(3) {
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
