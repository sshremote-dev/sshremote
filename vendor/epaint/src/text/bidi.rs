//! Right-to-left text, added for sshremote: the Unicode Bidirectional Algorithm (UAX #9) without
//! explicit embeddings, enough for interface text in Arabic, Persian, Urdu and Hebrew mixed with
//! Latin words and numbers. Text is shaped and broken into rows in reading order; each row is
//! then put in display order (`reorder_row`).

use super::text_layout_types::Row;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    L,
    R,
    AL,
    EN,
    ES,
    ET,
    AN,
    CS,
    NSM,
    BN,
    S,
    WS,
    ON,
}

fn class(c: char) -> Class {
    use Class::*;
    let u = c as u32;
    match u {
        0x30..=0x39 | 0xB2 | 0xB3 | 0xB9 | 0x6F0..=0x6F9 | 0x2070..=0x2079 | 0x2080..=0x2089 | 0xFF10..=0xFF19 => EN,
        0x2B | 0x2D | 0x207A | 0x207B | 0x208A | 0x208B | 0x2212 | 0xFB29 | 0xFE62 | 0xFE63 | 0xFF0B | 0xFF0D => ES,
        0x23..=0x25 | 0xA2..=0xA5 | 0xB0 | 0xB1 | 0x609 | 0x60A | 0x66A | 0x2030..=0x2034 | 0x20A0..=0x20CF | 0x212E | 0x2213 | 0xFF03..=0xFF05 | 0xFFE0 | 0xFFE1 | 0xFFE5 | 0xFFE6 => ET,
        0x600..=0x605 | 0x660..=0x669 | 0x66B | 0x66C | 0x6DD | 0x890 | 0x891 | 0x8E2 | 0x10D30..=0x10D39 => AN,
        0x2C | 0x2E | 0x2F | 0x3A | 0xA0 | 0x60C | 0x202F | 0x2044 | 0xFE50 | 0xFE52 | 0xFE55 | 0xFF0C | 0xFF0E | 0xFF0F | 0xFF1A => CS,
        0x09 | 0x0B | 0x1F => S,
        0x20 | 0x0C | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x205F | 0x3000 => WS,
        0x200E => L,
        0x200F => R,
        0x61C => AL,
        0..=0x08 | 0x0E..=0x1B | 0x7F..=0x84 | 0x86..=0x9F | 0xAD | 0x180E | 0x200B..=0x200D | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF => BN,
        _ if super::unicode::is_combining_mark(c) => NSM,
        0x590..=0x5FF | 0x7C0..=0x85F | 0xFB1D..=0xFB4F | 0x10800..=0x10CFF | 0x10D40..=0x10EBF | 0x10F00..=0x10F2F | 0x10F70..=0x10FFF | 0x1E800..=0x1EC6F | 0x1ECC0..=0x1ECFF | 0x1ED50..=0x1EDFF | 0x1EF00..=0x1EFFF => R,
        0x600..=0x6FF | 0x700..=0x7BF | 0x860..=0x8FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFE | 0x10D00..=0x10D3F | 0x10EC0..=0x10EFF | 0x10F30..=0x10F6F | 0x1EC70..=0x1ECBF | 0x1ED00..=0x1ED4F | 0x1EE00..=0x1EEFF => AL,
        _ if c.is_alphanumeric() => L,
        _ if c.is_whitespace() => WS,
        _ if c.is_ascii() || is_symbol_or_punctuation(c) => ON,
        _ => L,
    }
}

fn is_symbol_or_punctuation(c: char) -> bool {
    use unicode_general_category::{GeneralCategory::*, get_general_category};
    matches!(
        get_general_category(c),
        ConnectorPunctuation
            | DashPunctuation
            | OpenPunctuation
            | ClosePunctuation
            | InitialPunctuation
            | FinalPunctuation
            | OtherPunctuation
            | MathSymbol
            | CurrencySymbol
            | ModifierSymbol
            | OtherSymbol
    )
}

/// Whether `text` has anything written right to left (else its levels are all 0).
pub(crate) fn has_rtl(text: &str) -> bool {
    text.chars().any(|c| (0x590..=0x8FF).contains(&(c as u32)) || matches!(class(c), Class::R | Class::AL))
}

/// The resolved embedding level of each char of a paragraph: even left to right, odd right to
/// left. The paragraph's own level comes from its first strong letter.
pub(crate) fn levels(text: &str) -> Vec<u8> {
    use Class::*;
    let mut t: Vec<Class> = text.chars().map(class).collect();
    let para: u8 = match t.iter().find(|c| matches!(c, L | R | AL)) {
        Some(R | AL) => 1,
        _ => 0,
    };
    let sos = if para == 1 { R } else { L };
    // BN takes the type before it (X9 removes them; this keeps indices aligned).
    for i in 0..t.len() {
        if t[i] == BN {
            t[i] = if i == 0 { sos } else { t[i - 1] };
        }
    }
    // W1: a mark takes the type of what it marks.
    for i in 0..t.len() {
        if t[i] == NSM {
            t[i] = if i == 0 { sos } else { t[i - 1] };
        }
    }
    // W2: European digits after Arabic letters are Arabic numbers. W3: AL is R.
    let mut last_strong = sos;
    for c in &mut t {
        match *c {
            L | R | AL => last_strong = *c,
            EN if last_strong == AL => *c = AN,
            _ => {}
        }
    }
    for c in &mut t {
        if *c == AL {
            *c = R;
        }
    }
    // W4: one separator between two numbers of a kind joins them.
    for i in 1..t.len().saturating_sub(1) {
        let (a, b) = (t[i - 1], t[i + 1]);
        if t[i] == ES && a == EN && b == EN {
            t[i] = EN;
        } else if t[i] == CS && a == b && matches!(a, EN | AN) {
            t[i] = a;
        }
    }
    // W5: terminators next to European digits are digits.
    let mut i = 0;
    while i < t.len() {
        if t[i] == ET {
            let start = i;
            while i < t.len() && t[i] == ET {
                i += 1;
            }
            let near = (start > 0 && t[start - 1] == EN) || (i < t.len() && t[i] == EN);
            if near {
                t[start..i].fill(EN);
            }
        } else {
            i += 1;
        }
    }
    // W6: other separators and terminators are neutral. W7: digits in left-to-right text are L.
    let mut last_strong = sos;
    for c in &mut t {
        match *c {
            ES | ET | CS => *c = ON,
            L | R => last_strong = *c,
            EN if last_strong == L => *c = L,
            _ => {}
        }
    }
    // N1, N2: neutrals between two of one direction take it, else the paragraph's.
    let strong = |c: Class| match c {
        L => Some(L),
        R | EN | AN => Some(R),
        _ => None,
    };
    let mut i = 0;
    while i < t.len() {
        if strong(t[i]).is_some() {
            i += 1;
            continue;
        }
        let start = i;
        while i < t.len() && strong(t[i]).is_none() {
            i += 1;
        }
        let before = if start == 0 { sos } else { strong(t[start - 1]).unwrap_or(sos) };
        let after = if i == t.len() { sos } else { strong(t[i]).unwrap_or(sos) };
        let to = if before == after { before } else if para == 1 { R } else { L };
        t[start..i].fill(to);
    }
    // I1, I2.
    let mut levels: Vec<u8> = t
        .iter()
        .map(|c| match (para % 2, c) {
            (0, R) => para + 1,
            (0, AN | EN) => para + 2,
            (1, L | EN | AN) => para + 1,
            _ => para,
        })
        .collect();
    // L1: whitespace at the end goes back to the paragraph's level.
    for (c, level) in text.chars().rev().zip(levels.iter_mut().rev()) {
        if !matches!(class(c), WS | S | BN) {
            break;
        }
        *level = para;
    }
    levels
}

/// Puts a row's glyphs, laid out in reading order, where they show: runs right to left reversed
/// (L2). A run keeps its shaped glyphs together and moves by whole pixels.
pub(crate) fn reorder_row(row: &mut Row, pixels_per_point: f32) {
    if row.glyphs.iter().all(|g| g.bidi_level == 0) {
        return;
    }
    struct Seg {
        glyphs: std::ops::Range<usize>,
        level: u8,
        x0: f32,
        x1: f32,
    }
    let mut segs: Vec<Seg> = Vec::new();
    for (i, g) in row.glyphs.iter().enumerate() {
        match segs.last_mut() {
            Some(s) if row.glyphs[s.glyphs.start].bidi_run == g.bidi_run => s.glyphs.end = i + 1,
            _ => segs.push(Seg { glyphs: i..i + 1, level: g.bidi_level, x0: 0.0, x1: 0.0 }),
        }
    }
    for s in &mut segs {
        let glyphs = &row.glyphs[s.glyphs.clone()];
        let wide: Vec<_> = glyphs.iter().filter(|g| g.advance_width > 0.0).collect();
        let pick = if wide.is_empty() { glyphs.iter().collect() } else { wide };
        s.x0 = pick.iter().map(|g| g.pos.x).fold(f32::INFINITY, f32::min);
        s.x1 = pick.iter().map(|g| g.pos.x + g.advance_width).fold(f32::NEG_INFINITY, f32::max);
    }
    let start = segs.iter().map(|s| s.x0).fold(f32::INFINITY, f32::min);
    // Spaces where the row was broken stay at its right end, not as an indent on the left.
    let blank = |s: &Seg| row.glyphs[s.glyphs.clone()].iter().all(|g| g.chr.is_whitespace());
    let trailing = segs.iter().rev().take_while(|s| blank(s)).count();
    let mut order: Vec<usize> = (0..segs.len() - trailing).collect();
    let high = segs.iter().map(|s| s.level).max().unwrap_or(0);
    let low_odd = segs.iter().map(|s| s.level).filter(|l| l % 2 == 1).min().unwrap_or(1);
    for level in (low_odd..=high).rev() {
        let mut i = 0;
        while i < order.len() {
            if segs[order[i]].level >= level {
                let from = i;
                while i < order.len() && segs[order[i]].level >= level {
                    i += 1;
                }
                order[from..i].reverse();
            } else {
                i += 1;
            }
        }
    }
    order.extend(segs.len() - trailing..segs.len());
    let mut x = start;
    for &k in &order {
        let s = &segs[k];
        let shift = ((x - s.x0) * pixels_per_point).round() / pixels_per_point;
        for g in &mut row.glyphs[s.glyphs.clone()] {
            g.pos.x += shift;
        }
        x += s.x1 - s.x0;
    }
}
