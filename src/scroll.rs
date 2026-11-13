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
    // Columns that keep their place (a scroll bar's thumb, a fixed side panel) would cut the run
    // short at every row they differ in; the move is looked for without them.
    let (x0, x1) = moving_columns(prev, cur, dy, (x0, x1), (y0, y1));
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

/// The widest span of columns of `x0..x1` that moved by `dy` with the rest: a column that differs
/// after the shift in clearly more rows than the typical (median) column stayed put. All of them
/// when no such span is at least 64 pixels wide.
fn moving_columns(prev: &Plane, cur: &Plane, dy: isize, (x0, x1): (usize, usize), (y0, y1): (usize, usize)) -> (usize, usize) {
    let bpp = cur.mode.bpp();
    let mut off = vec![0u32; x1 - x0];
    let mut rows = 0u32;
    for y in y0..y1 {
        let src = y as isize - dy;
        if src < y0 as isize || src >= y1 as isize {
            continue;
        }
        rows += 1;
        let (a, b) = (row(cur, y, x0, x1), row(prev, src as usize, x0, x1));
        if a == b {
            continue;
        }
        for (i, (pa, pb)) in a.chunks_exact(bpp).zip(b.chunks_exact(bpp)).enumerate() {
            if pa != pb {
                off[i] += 1;
            }
        }
    }
    let mut sorted = off.clone();
    sorted.sort_unstable();
    let limit = sorted[sorted.len() / 2] + (rows / 32).max(4);
    let (mut best, mut start) = ((0, 0), None);
    for (i, &o) in off.iter().chain(std::iter::once(&u32::MAX)).enumerate() {
        if o <= limit {
            start.get_or_insert(i);
        } else if let Some(s) = start.take()
            && i - s > best.1 - best.0
        {
            best = (s, i);
        }
    }
    if best.1 - best.0 < 64 { (x0, x1) } else { (x0 + best.0, x0 + best.1) }
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
                let r = ((x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761) >> 28;
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

    #[test]
    fn a_scroll_bar_that_stays_put_does_not_cut_the_move_short() {
        let mut prev = text(300, 400, 0);
        let mut cur = text(300, 400, 25);
        // A short thumb at the right edge, rows 40..60 before and 41..61 after: it moves, but not
        // with the text, and differs in only a tenth of the rows.
        for (p, top) in [(&mut prev, 40), (&mut cur, 41)] {
            for y in 0..400 {
                p.data[y * 300 + 292..y * 300 + 300].fill(if (top..top + 20).contains(&y) { 120 } else { 30 });
            }
        }
        let rects = diff(Some(&prev), &cur);
        let m = find(&prev, &cur, &rects).expect("scroll");
        assert_eq!(m.dy, -25);
        assert!(m.h > 330, "{m:?}");
        let mut moved = prev.clone();
        apply(&mut moved.data, 300, 1, &m);
        let rest: usize = diff(Some(&moved), &cur).iter().map(|r| r.w * r.h).sum();
        // The new rows at the bottom and the thumb's column, little else.
        assert!(rest < 25 * 300 + 8 * 400 + 2000, "rest {rest}");
    }
}
