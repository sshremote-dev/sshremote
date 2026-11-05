//! Minimal sixel encoder for palette images with at most 256 colours.

use std::io::Write;

pub struct Sixel {
    masks: Vec<u8>,
    minx: [usize; 256],
    maxx: [usize; 256],
    inband: [bool; 256],
    list: Vec<u8>,
}

fn run(out: &mut Vec<u8>, ch: u8, n: usize) {
    if n >= 4 {
        let _ = write!(out, "!{n}");
        out.push(ch);
    } else {
        out.extend(std::iter::repeat_n(ch, n));
    }
}

impl Sixel {
    pub fn new() -> Sixel {
        Sixel { masks: Vec::new(), minx: [0; 256], maxx: [0; 256], inband: [false; 256], list: Vec::new() }
    }

    /// Appends one sixel image drawn at the current cursor position.
    pub fn encode(&mut self, out: &mut Vec<u8>, w: usize, h: usize, idx: &[u8], pal: &[[u8; 3]]) {
        if self.masks.len() < 256 * w {
            self.masks = vec![0; 256 * w];
        }
        let _ = write!(out, "\x1bP0;1;8q\"1;1;{w};{h}");
        let mut used = [false; 256];
        for &c in &idx[..w * h] {
            used[c as usize] = true;
        }
        for (c, rgb) in pal.iter().enumerate() {
            if used[c] {
                let pct = |v: u8| (v as u32 * 100 + 127) / 255;
                let _ = write!(out, "#{c};2;{};{};{}", pct(rgb[0]), pct(rgb[1]), pct(rgb[2]));
            }
        }
        let Sixel { masks, minx, maxx, inband, list } = self;
        let mut y0 = 0;
        while y0 < h {
            list.clear();
            for k in 0..(h - y0).min(6) {
                let row = &idx[(y0 + k) * w..(y0 + k + 1) * w];
                for (x, &c) in row.iter().enumerate() {
                    let ci = c as usize;
                    if !inband[ci] {
                        inband[ci] = true;
                        list.push(c);
                        minx[ci] = x;
                        maxx[ci] = x;
                    } else {
                        minx[ci] = minx[ci].min(x);
                        maxx[ci] = maxx[ci].max(x);
                    }
                    masks[ci * w + x] |= 1 << k;
                }
            }
            for (i, &c) in list.iter().enumerate() {
                let ci = c as usize;
                let _ = write!(out, "#{ci}");
                let (a, b) = (minx[ci], maxx[ci]);
                if a > 0 {
                    run(out, b'?', a);
                }
                let m = &mut masks[ci * w..ci * w + w];
                let mut x = a;
                while x <= b {
                    let v = m[x];
                    let mut n = 1;
                    while x + n <= b && m[x + n] == v {
                        n += 1;
                    }
                    run(out, 63 + v, n);
                    x += n;
                }
                m[a..=b].fill(0);
                inband[ci] = false;
                if i + 1 < list.len() {
                    out.push(b'$');
                }
            }
            y0 += 6;
            if y0 < h {
                out.push(b'-');
            }
        }
        out.extend_from_slice(b"\x1b\\");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_colour_band() {
        // 4x2 image: left half colour 1, right half colour 0.
        let idx = [1, 1, 0, 0, 1, 1, 0, 0];
        let mut out = Vec::new();
        Sixel::new().encode(&mut out, 4, 2, &idx, &[[0, 0, 0], [255, 255, 255]]);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "\x1bP0;1;8q\"1;1;4;2#0;2;0;0;0#1;2;100;100;100#1BB$#0??BB\x1b\\");
    }
}
