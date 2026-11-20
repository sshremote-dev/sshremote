//! Move detection: when a band of the screen moved (scrolled vertically, or text moved along its
//! line as a character was typed into it), the client can copy what it already has instead of
//! receiving the moved pixels again.

use std::collections::HashMap;

use crate::color::{Plane, Rect};

/// Rows `y..y + h` of columns `x..x + w` now hold what rows `y - dy..y - dy + h` of columns
/// `x - dx..x - dx + w` held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub dy: isize,
    pub dx: isize,
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
    (best.1 >= 32).then_some(Move { x: x0, y: best.0, w: x1 - x0, h: best.1, dy, dx: 0 })
}

/// The farthest sideways shift looked for: a word taken out of a line.
const SIDEWAYS: usize = 96;

/// Looks for rows whose changes are what was there before shifted sideways: the rest of a line
/// of text a character was typed into (or taken out of). A move for each band of rows that
/// shifted alike, at most eight.
pub fn sideways(prev: &Plane, cur: &Plane, rects: &[Rect]) -> Vec<Move> {
    if rects.is_empty() || (prev.w, prev.h, prev.mode) != (cur.w, cur.h, cur.mode) {
        return Vec::new();
    }
    // Most of the screen changing is not text moving along its lines.
    let changed: usize = rects.iter().map(|r| r.w * r.h).sum();
    if changed * 4 > cur.w * cur.h {
        return Vec::new();
    }
    let bpp = cur.mode.bpp();
    fn at(p: &Plane, bpp: usize, x: usize, y: usize) -> &[u8] {
        &p.data[(y * p.w + x) * bpp..(y * p.w + x + 1) * bpp]
    }
    // Pixels of row `y` in `a..b` that changed, and of those, how many hold what was `dx` to
    // their left.
    let score = |y: usize, (a, b): (usize, usize), dx: isize| {
        let (mut changed, mut same) = (0, 0);
        for x in a..b {
            let c = at(cur, bpp, x, y);
            if c == at(prev, bpp, x, y) {
                continue;
            }
            changed += 1;
            let sx = x as isize - dx;
            if dx != 0 && sx >= 0 && (sx as usize) < cur.w && c == at(prev, bpp, sx as usize, y) {
                same += 1;
            }
        }
        (changed, same)
    };
    // The changed columns of each row.
    let mut span: Vec<Option<(usize, usize)>> = vec![None; cur.h];
    for r in rects {
        for s in &mut span[r.y..r.y + r.h] {
            let s = s.get_or_insert((r.x, r.x + r.w));
            *s = (s.0.min(r.x), s.1.max(r.x + r.w));
        }
    }
    let mut moves = Vec::new();
    // Bands looked at in vain one after another: the change is something else (a new page).
    let mut vain = 0;
    let mut y = 0;
    while y < cur.h && moves.len() < 8 && vain < 4 {
        // A band: rows with changes, one after another.
        if span[y].is_none() {
            y += 1;
            continue;
        }
        let y0 = y;
        while y < cur.h && span[y].is_some() {
            y += 1;
        }
        let band = y0..y;
        if band.len() > 160 {
            continue;
        }
        let found = moves.len();
        // Each row's shift. The shifts that explain most of the busiest row not yet explained
        // (some of its changed pixels, spread over it) are tried on the others, in a few rounds:
        // lines of a paragraph shift alike, but differently where a word went to the next line.
        let mut shift = vec![0isize; band.len()];
        for _ in 0..3 {
            let open = || band.clone().filter(|&r| shift[r - band.start] == 0);
            let Some(busiest) = open().step_by(3).max_by_key(|&r| score(r, span[r].unwrap(), 0).0) else { break };
            let (a, b) = span[busiest].unwrap();
            let changed: Vec<usize> = (a..b).filter(|&x| at(cur, bpp, x, busiest) != at(prev, bpp, x, busiest)).collect();
            let sample: Vec<usize> = changed.iter().step_by(changed.len().div_ceil(96).max(1)).copied().collect();
            let mut tried: Vec<(isize, usize)> = (1..=SIDEWAYS as isize)
                .flat_map(|d| [d, -d])
                .map(|dx| {
                    let same = sample.iter().filter(|&&x| {
                        let sx = x as isize - dx;
                        sx >= 0 && (sx as usize) < cur.w && at(cur, bpp, x, busiest) == at(prev, bpp, sx as usize, busiest)
                    });
                    (dx, same.count())
                })
                .filter(|&(_, n)| n > 0 && n * 2 >= sample.len())
                .collect();
            if tried.is_empty() {
                break;
            }
            tried.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
            tried.truncate(3);
            // A row takes the one that explains at least half of its change.
            let rows: Vec<usize> = open().collect();
            for r in rows {
                let s = span[r].unwrap();
                // On a tie, the shift that explained more of the busiest row.
                let best = tried.iter().map(|&(dx, _)| (dx, score(r, s, dx))).reduce(|a, b| if b.1.1 > a.1.1 { b } else { a });
                if let Some((dx, (changed, same))) = best
                    && changed > 0
                    && same * 2 >= changed
                {
                    shift[r - band.start] = dx;
                }
            }
        }
        // Rows shifting alike, as one move (rows that did not change between them join it).
        let mut i = 0;
        while i < shift.len() && moves.len() < 8 {
            let dx = shift[i];
            if dx == 0 {
                i += 1;
                continue;
            }
            let start = i;
            while i < shift.len() && (shift[i] == dx || (shift[i] == 0 && shift[i..].iter().find(|&&d| d != 0) == Some(&dx))) {
                i += 1;
            }
            let rows = band.start + start..band.start + i;
            let (mut a, mut b) = (usize::MAX, 0);
            for r in rows.clone() {
                let s = span[r].unwrap();
                (a, b) = (a.min(s.0), b.max(s.1));
            }
            // Only columns whose source is on the screen.
            let a = if dx > 0 { a.max(dx as usize) } else { a };
            let b = if dx < 0 { b.min((cur.w as isize + dx) as usize) } else { b };
            // Not a caret's column fading as it blinks.
            if rows.len() >= 2 && b >= a + 8 {
                moves.push(Move { x: a, y: rows.start, w: b - a, h: rows.len(), dy: 0, dx });
            }
        }
        vain = if moves.len() > found { 0 } else { vain + 1 };
    }
    moves
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
    let (a, len) = (m.x * bpp, m.w * bpp);
    let from = (m.x as isize - m.dx) as usize * bpp;
    let rows: Box<dyn Iterator<Item = usize>> = if m.dy > 0 { Box::new((0..m.h).rev()) } else { Box::new(0..m.h) };
    for i in rows {
        let dst = m.y + i;
        let src = (dst as isize - m.dy) as usize;
        buf.copy_within(src * w * bpp + from..src * w * bpp + from + len, dst * w * bpp + a);
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

    /// A line of text: glyph-like columns from `from`, each glyph `adv` wide, starting at `x0`.
    fn line(p: &mut Plane, y0: usize, x0: usize, glyphs: &[u8], adv: usize) {
        for (i, &g) in glyphs.iter().enumerate() {
            for dy in 0..14 {
                for dx in 0..adv - 2 {
                    let on = ((g as u32).wrapping_mul(2_654_435_761) ^ (dx as u32 * 40_503) ^ (dy as u32 * 9_973)).wrapping_mul(2_246_822_519) >> 29 < 3;
                    p.data[(y0 + dy) * p.w + x0 + i * adv + dx] = if on { 200 + g % 50 } else { 10 };
                }
            }
        }
    }

    #[test]
    fn a_character_typed_into_a_line_moves_its_rest_along() {
        let blank = || Plane { w: 400, h: 120, mode: Mode::Color256, data: vec![10; 400 * 120] };
        let text = b"the quick brown fox jumps";
        let (mut prev, mut cur) = (blank(), blank());
        line(&mut prev, 20, 8, text, 9);
        line(&mut prev, 60, 8, text, 9);
        // "X" typed after "the " on the first line; the second line keeps still.
        let typed: Vec<u8> = [&text[..4], b"X", &text[4..]].concat();
        line(&mut cur, 20, 8, &typed, 9);
        line(&mut cur, 60, 8, text, 9);
        let rects = diff(Some(&prev), &cur);
        let all: usize = rects.iter().map(|r| r.w * r.h).sum();
        let moves = sideways(&prev, &cur, &rects);
        assert_eq!(moves.len(), 1, "{moves:?}");
        assert_eq!((moves[0].dx, moves[0].dy, moves[0].y), (9, 0, 20));
        let mut moved = prev.clone();
        apply(&mut moved.data, 400, 1, &moves[0]);
        let rest: usize = diff(Some(&moved), &cur).iter().map(|r| r.w * r.h).sum();
        assert!(rest * 4 < all, "the new glyph is left, not the line: {rest} of {all}");
        // Taken out again: the rest moves back.
        let moves = sideways(&cur, &prev, &diff(Some(&cur), &prev));
        assert_eq!(moves.first().map(|m| m.dx), Some(-9), "{moves:?}");
        // Two lines of a paragraph, one row after the other, shifting differently: two moves.
        let (mut prev, mut cur) = (blank(), blank());
        line(&mut prev, 20, 8, text, 9);
        line(&mut prev, 34, 8, b"over the lazy dog again", 9);
        line(&mut cur, 20, 8, &typed, 9);
        line(&mut cur, 34, 8, b"the lazy dog again", 9);
        let moves = sideways(&prev, &cur, &diff(Some(&prev), &cur));
        let mut found: Vec<(usize, isize)> = moves.iter().map(|m| (m.y, m.dx)).collect();
        found.sort();
        assert_eq!(found, vec![(20, 9), (34, -45)], "{moves:?}");
        // A whole new screen is no such thing.
        let other = Plane { w: 400, h: 120, mode: Mode::Color256, data: (0..400 * 120).map(|i| (i * 31 % 251) as u8).collect() };
        assert!(sideways(&prev, &other, &diff(Some(&prev), &other)).is_empty());
    }

    #[test]
    fn a_sideways_move_reads_its_source_before_writing() {
        let mut buf: Vec<u8> = (0..20).collect();
        apply(&mut buf, 10, 1, &Move { x: 3, y: 1, w: 5, h: 1, dy: 0, dx: 2 });
        assert_eq!(&buf[10..20], &[10, 11, 12, 11, 12, 13, 14, 15, 18, 19]);
        apply(&mut buf, 10, 1, &Move { x: 0, y: 0, w: 4, h: 1, dy: 0, dx: -3 });
        assert_eq!(&buf[..10], &[3, 4, 5, 6, 4, 5, 6, 7, 8, 9]);
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
