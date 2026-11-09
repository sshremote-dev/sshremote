//! The Markdown view's record of a full-screen program (an agent, an editor, a pager, tmux):
//! rows that stay on its screen for a moment are gathered into one document. Each new screen is
//! placed in the record by the rows it shares with it, as a diff lines two texts up, so scrolling
//! the program fills the record in either direction. Rows that keep changing (a spinner, a timer)
//! wait until they settle, and a program's own frame (an input box, a status line, title and help
//! bars) is left out. The record starts again when the screen is cleared or the program leaves.

use super::*;

/// How long a row stays as it is before it goes into the record.
pub(super) const STABLE: f64 = 0.5;

/// One row of the record: as plain text (to place screens by) and as Markdown.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Kept {
    pub(super) plain: String,
    pub(super) md: (String, bool),
}

impl Kept {
    fn blank() -> Self {
        Kept { plain: String::new(), md: (String::new(), false) }
    }

    fn is_blank(&self) -> bool {
        self.plain.trim().is_empty()
    }
}

#[derive(Default)]
pub(super) struct Collect {
    /// The record in pieces, each a run of rows known to follow one another; a screen that
    /// joins two makes them one.
    pieces: Vec<Vec<Kept>>,
    /// The piece the last screen went on.
    cur: usize,
    /// Changes with every change of the record.
    pub(super) rev: u64,
    /// Rows the last change added above the record's top: the view keeps its place.
    pub(super) prepended: usize,
    alt: bool,
    cols: usize,
    seen_rev: u64,
    /// For each screen row, what is there (a hash).
    hashes: Vec<u64>,
    /// Since when each row's text has been on the screen, wherever it is: text that scrolls
    /// along stays settled, text that changes starts again.
    seen: std::collections::HashMap<u64, f64>,
    /// The rows the last merge took.
    merged: std::collections::HashSet<u64>,
    /// The current piece's row for the screen's first row at the last merge.
    place: Option<i64>,
    /// The screen was cleared since the last merge.
    cleared: bool,
    /// The wheel went up last: a screen that cannot be placed goes before the record.
    pub(super) up: bool,
    /// When the wheel last went to the program: while it scrolls, nothing starts the record again.
    pub(super) wheel_at: f64,
}

impl Collect {
    /// The record's rows, its pieces one after another with a blank row between.
    pub(super) fn rows(&self) -> Vec<Kept> {
        let mut out = Vec::new();
        for (n, piece) in self.pieces.iter().enumerate() {
            if n > 0 {
                out.push(Kept::blank());
            }
            out.extend(piece.iter().cloned());
        }
        let lead = out.iter().take_while(|k| k.is_blank()).count();
        out.split_off(lead)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pieces.iter().all(Vec::is_empty)
    }

    /// Forgets the record (the screen was cleared, the program left, or asked by hand).
    pub(super) fn clear(&mut self) {
        self.pieces.clear();
        self.cur = 0;
        self.place = None;
        self.cleared = false;
        self.prepended = 0;
        self.rev += 1;
    }

    /// Cleared by hand: what the screen shows now stays out until it changes.
    pub(super) fn clear_by_hand(&mut self) {
        self.clear();
        self.merged = self.hashes.iter().copied().collect();
    }

    /// Output on its way to the screen: a clear of the whole screen is noted.
    pub(super) fn saw(&mut self, data: &[u8]) {
        let clears = [&b"\x1b[2J"[..], b"\x1b[3J", b"\x1bc"];
        if clears.iter().any(|c| data.windows(c.len()).any(|w| w == *c)) {
            self.cleared = true;
        }
    }
}

/// Brings the record up to the screen; when to look again (rows still settling), if at all.
pub(super) fn tick(t: &mut Tab, now: f64) -> Option<f64> {
    let mut c = std::mem::take(&mut t.collect);
    let next = step(&mut c, t, now);
    t.collect = c;
    next
}

fn step(c: &mut Collect, t: &Tab, now: f64) -> Option<f64> {
    let grid = t.term.grid();
    let (n, cols) = (grid.screen_lines(), grid.columns());
    let alt = t.term.mode().contains(TermMode::ALT_SCREEN);
    // A program starting or leaving, or a new width (its rows wrap anew): a new record.
    if alt != c.alt || cols != c.cols || c.hashes.len() != n {
        if !c.is_empty() {
            c.clear();
        }
        c.alt = alt;
        c.cols = cols;
        c.hashes = vec![0; n];
        c.seen.clear();
        c.merged.clear();
        c.place = None;
        c.seen_rev = t.rev.wrapping_sub(1);
    }
    if !alt {
        return None;
    }
    if c.seen_rev != t.rev {
        c.seen_rev = t.rev;
        for p in 0..n {
            c.hashes[p] = row_hash(t, p as i32);
        }
        let seen = std::mem::take(&mut c.seen);
        c.seen = c.hashes.iter().map(|h| (*h, seen.get(h).copied().unwrap_or(now))).collect();
    }
    let (end, skip) = content_rows(t);
    let since: Vec<f64> = c.hashes.iter().map(|h| c.seen.get(h).copied().unwrap_or(now)).collect();
    let stable = |p: usize| now - since[p] >= STABLE;
    let settling = (0..end).filter(|&p| !stable(p) && !skip[p]).map(|p| since[p] + STABLE).fold(None, |a: Option<f64>, b| {
        Some(a.map_or(b, |a| a.min(b)))
    });
    let fresh = (0..end).any(|p| stable(p) && !skip[p] && !c.merged.contains(&c.hashes[p]));
    if fresh {
        let scrolling = now - c.wheel_at < 2.0;
        merge(c, t, end, &skip, scrolling, &|p| stable(p) && !skip[p]);
    }
    settling
}

fn row_hash(t: &Tab, line: i32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let row = &t.term.grid()[Line(line)];
    for c in 0..t.term.grid().columns() {
        let cell = &row[Column(c)];
        cell.c.hash(&mut h);
        format!("{:?}{:?}{:?}", cell.fg, cell.bg, cell.flags).hash(&mut h);
    }
    h.finish()
}

/// The screen rows that hold the program's text: above its input box (two rules close together
/// near the bottom, as an agent draws it) and a status line, without bars drawn in reverse video
/// and lines that keep changing ("Thinking… (12s · 300 tokens)").
fn content_rows(t: &Tab) -> (usize, Vec<bool>) {
    let grid = t.term.grid();
    let (n, cols) = (grid.screen_lines(), grid.columns());
    let rule = |p: usize| {
        let mut k = 0;
        let only = (0..cols).all(|c| {
            let ch = grid[Line(p as i32)][Column(c)].c;
            k += (ch == '\u{2500}') as usize;
            matches!(ch, ' ' | '\0' | '\u{2500}')
        });
        only && k * 10 >= cols * 8
    };
    let mut end = n;
    let low = n.saturating_sub(15);
    if let Some(r2) = (low..n).rev().find(|&p| rule(p)) {
        end = (r2.saturating_sub(6).max(low)..r2).rev().find(|&p| rule(p)).unwrap_or(r2);
    }
    if status_row(t) {
        end = end.min(n - 1);
    }
    let skip = (0..n)
        .map(|p| {
            let row = &grid[Line(p as i32)];
            let cells: Vec<_> = (0..cols).map(|c| &row[Column(c)]).filter(|c| !matches!(c.c, ' ' | '\0')).collect();
            // An editor's title and help bars: reverse video on the top row or the last three.
            let edge = p == 0 || p + 3 >= n;
            let bar = edge && cells.iter().any(|c| c.flags.contains(Flags::INVERSE));
            // A row a hint is drawn over is not known as it is.
            let plain = row_plain(t, p as i32);
            let hinted = FLOATING_HINTS.iter().any(|h| plain.contains(h));
            bar || hinted || busy(t, p)
        })
        .collect();
    (end, skip)
}

/// A line that keeps changing while a program works ("Thinking\u{2026} (12s \u{B7} 300 tokens)").
fn busy(t: &Tab, p: usize) -> bool {
    let plain = row_plain(t, p as i32);
    let spinner = plain.trim_start().chars().next().is_some_and(|c| "\u{B7}\u{2722}\u{2733}\u{2736}\u{273B}\u{273D}*".contains(c));
    plain.contains('\u{2026}') && (plain.contains('(') || spinner) || plain.contains("esc to interrupt")
}

/// The record and what it is made from, to look into in tests.
#[cfg(test)]
pub(super) fn debug(t: &Tab) -> String {
    let c = &t.collect;
    let (end, skip) = content_rows(t);
    format!("alt {} rows {} place {:?} cleared {} end {end} skip {:?} merged {}", c.alt, c.rows().len(), c.place, c.cleared, skip.iter().filter(|s| **s).count(), c.merged.len())
}

/// Puts the screen's settled rows into the record.
fn merge(c: &mut Collect, t: &Tab, end: usize, skip: &[bool], scrolling: bool, take: &dyn Fn(usize) -> bool) {
    let screen: Vec<Option<Kept>> = (0..end)
        .map(|p| {
            take(p).then(|| {
                let plain = row_plain(t, p as i32);
                let (md, wraps) = row_markdown(t, p as i32);
                // Programs that draw their own screen break their own lines: a row goes on in the
                // next only when it fills the width.
                let full = plain.chars().count() + 1 >= t.term.grid().columns();
                Kept { plain, md: (md, wraps && full) }
            })
        })
        .collect();
    let taken: std::collections::HashSet<u64> = (0..end).filter(|&p| take(p)).map(|p| c.hashes[p]).collect();
    // Empty: no text, or an editor's bar. A hint or a busy line stands for text not known yet.
    let blank_now = |p: usize| {
        let plain = row_plain(t, p as i32);
        let known = busy(t, p) || FLOATING_HINTS.iter().any(|h| plain.contains(h));
        skip[p] && !known || plain.trim().is_empty()
    };
    let Some(last_any) = (0..end).rev().find(|&p| !blank_now(p)) else {
        // An empty screen: what comes next starts a new record unless it goes on from this one.
        c.cleared = true;
        c.merged = taken;
        return;
    };
    let last = (0..=last_any).rev().find(|&p| screen[p].as_ref().is_some_and(|k| !k.is_blank()));
    // Room left below the text: the screen shows where the document ends.
    let shows_end = end - 1 - last_any >= 2;
    let screen = &screen[..=last_any];
    let before: usize = c.pieces.iter().map(Vec::len).sum();
    // The piece the screen goes on (the one it agrees with most, the last one when as much).
    let found = c
        .pieces
        .iter()
        .enumerate()
        .filter_map(|(i, piece)| {
            let near = if i == c.cur { c.place.unwrap_or(0) } else { 0 };
            place(piece, screen, near).map(|(o, s)| (i, o, s))
        })
        .max_by_key(|&(i, _, s)| (s, i == c.cur));
    let same_place = |i: usize, o: i64| i == c.cur && Some(o) == c.place;
    let mut above = 0;
    let (i, o) = match found {
        Some((i, o, s)) if !(c.cleared && !same_place(i, o)) && (s >= 3 || o == 0 || !shows_end) => (i, o),
        _ if c.pieces.is_empty() || !scrolling && (shows_end || c.cleared) => {
            c.pieces = vec![Vec::new()];
            (0, 0)
        }
        // Too far from anything in the record to place it: a piece of its own, before or after
        // where the record was, as scrolled.
        _ => {
            let at = if c.up { c.cur } else { c.cur + 1 }.min(c.pieces.len());
            c.pieces.insert(at, Vec::new());
            if c.up {
                above += 1;
            }
            (at, 0)
        }
    };
    above += write(&mut c.pieces[i], o, screen.iter().cloned(), true);
    if shows_end && let Some(l) = last {
        // Nothing comes after: the piece ends here, and so does the record.
        let o = o.max(0) as usize;
        c.pieces[i].truncate(o + l + 1);
        c.pieces.truncate(i + 1);
    }
    while c.pieces[i].last().is_some_and(Kept::is_blank) {
        c.pieces[i].pop();
    }
    // Pieces the screen has joined up: one where it begins or ends inside this one.
    let mut i = i;
    let mut k = 0;
    while k < c.pieces.len() {
        if k == i {
            k += 1;
            continue;
        }
        let other: Vec<Option<Kept>> = c.pieces[k].iter().cloned().map(Some).collect();
        match place(&c.pieces[i], &other, 0) {
            Some((o, s)) if s >= 3 => {
                let rows = c.pieces.remove(k);
                if k < i {
                    i -= 1;
                }
                above += write(&mut c.pieces[i], o, rows.into_iter().map(Some), false);
                k = 0;
            }
            _ => k += 1,
        }
    }
    c.cur = i;
    c.place = Some(o.max(0));
    c.prepended = if above > 0 { c.pieces.iter().map(Vec::len).sum::<usize>().saturating_sub(before).max(1) } else { 0 };
    c.cleared = false;
    c.merged = taken;
    c.rev += 1;
}

/// Writes `rows` into `piece` with the first at `o` (before its start when negative). A screen
/// overwrites what it covers (the program changed it); a piece joined in only adds what is
/// missing at either end. How many rows went before the piece's start.
fn write(piece: &mut Vec<Kept>, o: i64, rows: impl Iterator<Item = Option<Kept>>, over: bool) -> usize {
    let added = if o < 0 { (-o) as usize } else { 0 };
    if added > 0 {
        piece.splice(0..0, std::iter::repeat_n(Kept::blank(), added));
    }
    let o = o.max(0) as usize;
    let had = (added, piece.len());
    for (p, row) in rows.enumerate() {
        let i = o + p;
        while piece.len() < i {
            piece.push(Kept::blank());
        }
        let inside = i >= had.0 && i < had.1;
        match row {
            // A blank row does not wipe text the record has (something drawn over it for a moment).
            Some(k) if i < piece.len() => {
                if (over || !inside) && !(k.is_blank() && !piece[i].is_blank()) {
                    piece[i] = k;
                }
            }
            Some(k) => piece.push(k),
            None if i >= piece.len() => piece.push(Kept::blank()),
            None => {}
        }
    }
    added
}

/// Where the screen's first row falls in `rows`, and how many rows agree there: the place most
/// of its text rows match, nearest `near` when two agree as well.
fn place(rows: &[Kept], screen: &[Option<Kept>], near: i64) -> Option<(i64, usize)> {
    let mut at: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (j, r) in rows.iter().enumerate() {
        if !r.is_blank() {
            at.entry(r.plain.as_str()).or_default().push(j);
        }
    }
    let mut score: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut text = 0;
    for (p, k) in screen.iter().enumerate() {
        let Some(k) = k.as_ref().filter(|k| !k.is_blank()) else { continue };
        text += 1;
        for &j in at.get(k.plain.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
            *score.entry(j as i64 - p as i64).or_default() += 1;
        }
    }
    let (o, s) = score.into_iter().max_by_key(|&(o, s)| (s, std::cmp::Reverse((o - near).abs())))?;
    // Enough of it agrees: three rows, or all of a short screen, and half of what overlaps.
    let overlap = screen
        .iter()
        .enumerate()
        .filter(|(p, k)| {
            let i = o + *p as i64;
            k.as_ref().is_some_and(|k| !k.is_blank()) && i >= 0 && (i as usize) < rows.len()
        })
        .count();
    (s >= 3.min(text) && s * 2 >= overlap).then_some((o, s))
}
