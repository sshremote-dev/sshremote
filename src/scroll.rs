//! Scroll detection: when a band of the screen moved vertically, the client can
//! copy what it already has instead of receiving the moved pixels again.

use std::collections::HashMap;

use crate::color::{Plane, Rect};

/// Rows `y..y + h` of columns `x..x + w` now hold what rows `y - dy..y - dy + h` held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub dy: isize,
}

fn row(p: &Plane, y: usize, x0: usize, x1: usize) -> &[u8] {
    let bpp = p.mode.bpp();
    &p.data[(y * p.w + x0) * bpp..(y * p.w + x1) * bpp]
}

fn hash(b: &[u8]) -> u64 {
    // FNV-1a over 8-byte words; enough to find candidates, which are then compared exactly.
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for c in b.chunks(8) {
        let mut w = [0u8; 8];
        w[..c.len()].copy_from_slice(c);
        h = (h ^ u64::from_le_bytes(w)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Looks for one vertical shift explaining most of the changed area.
pub fn find(prev: &Plane, cur: &Plane, rects: &[Rect]) -> Option<Move> {
    if rects.is_empty() || (prev.w, prev.h, prev.mode) != (cur.w, cur.h, cur.mode) {
        return None;
    }
    let changed: usize = rects.iter().map(|r| r.w * r.h).sum();
    if changed * 20 < cur.w * cur.h {
        return None;
    }
    let x0 = rects.iter().map(|r| r.x).min()?;
    let x1 = rects.iter().map(|r| r.x + r.w).max()?;
    let y0 = rects.iter().map(|r| r.y).min()?;
    let y1 = rects.iter().map(|r| r.y + r.h).max()?;
    if y1 - y0 < 64 {
        return None;
    }
    let flat = |r: &[u8]| {
        let bpp = cur.mode.bpp();
        r.chunks(bpp).all(|p| p == &r[..bpp])
    };
    let mut seen: HashMap<u64, Vec<usize>> = HashMap::new();
    for y in y0..y1 {
        let r = row(prev, y, x0, x1);
        if !flat(r) {
            let v = seen.entry(hash(r)).or_default();
            if v.len() < 4 {
                v.push(y);
            }
        }
    }
    let mut votes: HashMap<isize, usize> = HashMap::new();
    for y in y0..y1 {
        let r = row(cur, y, x0, x1);
        if flat(r) {
            continue;
        }
        for &p in seen.get(&hash(r)).map_or(&[][..], |v| v.as_slice()) {
            if p != y {
                *votes.entry(y as isize - p as isize).or_default() += 1;
            }
        }
    }
    let (&dy, &n) = votes.iter().max_by_key(|(_, n)| **n)?;
    if n < 16 {
        return None;
    }
    // The longest run of rows that match exactly after the shift.
    let (mut best, mut start, mut run) = ((0, 0), 0, 0);
    for y in y0..y1 {
        let src = y as isize - dy;
        let ok = src >= y0 as isize && src < y1 as isize && row(cur, y, x0, x1) == row(prev, src as usize, x0, x1);
        if ok {
            if run == 0 {
                start = y;
            }
            run += 1;
            if run > best.1 {
                best = (start, run);
            }
        } else {
            run = 0;
        }
    }
    (best.1 >= 32).then_some(Move { x: x0, y: best.0, w: x1 - x0, h: best.1, dy })
}

/// Applies `m` to a row-major buffer of `w`-pixel rows with `bpp` bytes per pixel.
pub fn apply(buf: &mut [u8], w: usize, bpp: usize, m: &Move) {
    let (a, b) = (m.x * bpp, (m.x + m.w) * bpp);
    let rows: Box<dyn Iterator<Item = usize>> = if m.dy > 0 { Box::new((0..m.h).rev()) } else { Box::new(0..m.h) };
    for i in rows {
        let dst = m.y + i;
        let src = (dst as isize - m.dy) as usize;
        buf.copy_within(src * w * bpp + a..src * w * bpp + b, dst * w * bpp + a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{Mode, diff};

    fn text(w: usize, h: usize, offset: usize) -> Plane {
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w + offset);
                // Pseudo-random glyph pixels, so no other shift lines up by accident.
                let r = (x as u32 * 73_856_093 ^ y as u32 * 19_349_663).wrapping_mul(2_654_435_761) >> 28;
                if x > 40 && r < 4 && y % 20 > 4 { 200 } else { 10 }
            })
            .collect();
        Plane { w, h, mode: Mode::Color256, data }
    }

    #[test]
    fn finds_a_scroll_and_leaves_little_to_send() {
        let prev = text(300, 400, 0);
        let mut cur = text(300, 400, 37);
        // A static sidebar on the left does not scroll.
        for y in 0..400 {
            cur.data[y * 300..y * 300 + 40].fill(5);
        }
        let mut prev = prev;
        for y in 0..400 {
            prev.data[y * 300..y * 300 + 40].fill(5);
        }
        let rects = diff(Some(&prev), &cur);
        let m = find(&prev, &cur, &rects).expect("scroll");
        assert_eq!(m.dy, -37);
        let mut moved = prev.clone();
        apply(&mut moved.data, 300, 1, &m);
        let rest: usize = diff(Some(&moved), &cur).iter().map(|r| r.w * r.h).sum();
        let all: usize = rects.iter().map(|r| r.w * r.h).sum();
        assert!(rest * 5 < all, "rest {rest} of {all}");
    }
}
