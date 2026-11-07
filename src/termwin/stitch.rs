//! The screens of full-screen programs stitched into one document as they scroll: their fixed
//! rows and floating hints found by comparing screens.

use super::*;

/// The most lines kept of a full-screen program's stitched document.
pub(super) const STITCH_MAX: usize = 5000;

/// Puts a new screen of a full-screen program into the document stitched from its earlier screens.
/// Programs such as Claude Code scroll inside their own screen, so each screen is a window on a
/// longer text; where it overlaps the document (scrolled back, or new lines at the end) it replaces
/// the overlapped lines and extends the document; a screen that matches nothing starts it anew.
#[cfg(test)]
pub(super) fn stitch(doc: &mut Vec<String>, screen: &[String]) {
    stitch_near(doc, screen, None);
}

/// [`stitch`], looking first within a few screens of `near` (where the last screen went), and
/// through the whole document only when nothing fits there. Returns where the screen went.
pub(super) fn stitch_near(doc: &mut Vec<String>, screen: &[String], near: Option<i64>) -> Option<i64> {
    let m = screen.len();
    if m == 0 {
        return near;
    }
    let n = doc.len();
    let all = -(m as i64 - 1)..=(n as i64);
    if let Some(at) = near {
        let span = 3 * m as i64;
        let window = (at - span).max(*all.start())..=(at + span).min(*all.end());
        if let Some(p) = best_place(doc, screen, window) {
            return Some(place(doc, screen, p));
        }
    }
    match best_place(doc, screen, all) {
        Some(p) => Some(place(doc, screen, p)),
        None => {
            *doc = screen.to_vec();
            Some(0)
        }
    }
}

/// Where `screen` best overlaps `doc` among the positions `range`, if it overlaps well enough.
pub(super) fn best_place(doc: &[String], screen: &[String], range: std::ops::RangeInclusive<i64>) -> Option<i64> {
    let (m, n) = (screen.len(), doc.len());
    let mut best: Option<(i64, usize)> = None;
    for p in range {
        let from = (-p).max(0) as usize;
        let to = (m as i64).min(n as i64 - p).max(0) as usize;
        if from >= to {
            continue;
        }
        let overlap = to - from;
        let mut score = 0;
        let mut written = 0;
        for i in from..to {
            let (a, b) = (&doc[(p + i as i64) as usize], &screen[i]);
            if a == b {
                score += 1;
                written += !a.trim().is_empty() as usize;
            }
        }
        // Most of the overlap must agree, a few lines that are still being written aside, and it
        // must agree on text rather than on blank lines alone.
        if written >= 2.min(m) && score * 10 >= overlap * 7 && best.is_none_or(|(_, s)| score >= s) {
            best = Some((p, score));
        }
    }
    best.map(|b| b.0)
}

/// Puts `screen` into `doc` at `p`: the overlapped lines replaced, the rest extended.
pub(super) fn place(doc: &mut Vec<String>, screen: &[String], p: i64) -> i64 {
    let (m, n) = (screen.len(), doc.len());
    let mut out: Vec<String> = Vec::with_capacity(n + m);
    if p > 0 {
        out.extend_from_slice(&doc[..p as usize]);
    }
    for (i, line) in screen.iter().enumerate() {
        let at = p + i as i64;
        let known = (0..n as i64).contains(&at).then(|| &doc[at as usize]);
        match known {
            // A line cut short (by a hint over it) keeps the whole text already known.
            Some(k) if line.len() < k.len() && !line.trim().is_empty() && k.starts_with(line.as_str()) => {
                out.push(k.clone())
            }
            _ => out.push(line.clone()),
        }
    }
    let after = (p + m as i64).max(0) as usize;
    if after < n {
        out.extend_from_slice(&doc[after..]);
    }
    if out.len() > STITCH_MAX {
        out.drain(..out.len() - STITCH_MAX);
    }
    *doc = out;
    p
}

/// Rows of a full-screen program that stay put while the rest of its screen moves: an input line,
/// a status bar, a pinned header. Counted from the top and the bottom by comparing two screens.
pub(super) fn fixed_rows(prev: &[String], cur: &[String]) -> (usize, usize) {
    if prev.len() != cur.len() || prev == cur {
        return (0, 0);
    }
    let top = prev.iter().zip(cur).take_while(|(a, b)| a == b).count();
    let bottom = prev.iter().rev().zip(cur.iter().rev()).take_while(|(a, b)| a == b).count();
    if top + bottom >= cur.len() {
        return (0, 0);
    }
    (top.min(cur.len() / 3), bottom.min(cur.len() / 2))
}

/// Adds a full-screen program's new screen to the tab's stitched document, leaving out its fixed
/// rows: the first time they are seen they may have gone in, so they are taken off the end again.
pub(super) fn stitch_screen(t: &mut Tab, screen: Vec<String>) {
    let (top, bottom) = fixed_rows(&t.last_screen, &screen);
    let mut piece: Vec<String> = screen[top..screen.len() - bottom].to_vec();
    // A hint floating at a fixed place over moving text: the end of a row that stays the same while
    // the text before it changes is the hint's, not the text's.
    if t.last_screen.len() == screen.len() {
        for (k, row) in piece.iter_mut().enumerate() {
            let prev = &t.last_screen[top + k];
            if let Some(cut) = overlay_start(prev, row) {
                let hint = row[cut..].trim().to_string();
                if !t.overlays.contains(&hint) {
                    t.overlays.push(hint);
                }
                row.truncate(cut);
                let kept = row.trim_end().len();
                row.truncate(kept);
            }
        }
    }
    while piece.last().is_some_and(|l| l.trim().is_empty()) {
        piece.pop();
    }
    if bottom > 0 {
        let footer: Vec<&String> = screen[screen.len() - bottom..].iter().filter(|l| !l.trim().is_empty()).collect();
        while t.stitched.last().is_some_and(|l| l.trim().is_empty() || footer.contains(&l)) {
            t.stitched.pop();
        }
    }
    t.last_screen = screen;
    let mut doc = std::mem::take(&mut t.stitched);
    t.stitch_at = stitch_near(&mut doc, &piece, t.stitch_at);
    // Hints seen floating since are taken off lines that went in before they were recognised.
    for line in doc.iter_mut() {
        if let Some(h) = t.overlays.iter().find(|h| line.trim_end().ends_with(h.as_str()) && line.trim() != h.as_str())
        {
            let keep = line.trim_end().len() - h.len();
            line.truncate(keep);
            let kept = line.trim_end().len();
            line.truncate(kept);
        }
    }
    t.stitched = doc;
}

/// Where a floating hint starts in `cur`: a long common end of two rows whose beginnings differ.
pub(super) fn overlay_start(prev: &str, cur: &str) -> Option<usize> {
    let (p, c) = (prev.trim_end(), cur.trim_end());
    if p == c {
        return None;
    }
    let common: usize =
        p.chars().rev().zip(c.chars().rev()).take_while(|(a, b)| a == b).map(|(_, b)| b.len_utf8()).sum();
    let tail = &c[c.len() - common..];
    // At least a few words of it, and something else before it in both rows.
    let drawn = tail.chars().any(|ch| ('\u{2500}'..='\u{257f}').contains(&ch));
    (!drawn && tail.split_whitespace().count() >= 2 && tail.trim().len() >= 8 && common < c.len() && common < p.len())
        .then(|| c.len() - common)
}

/// Whether the screen's last row is a status line such as tmux draws: one background colour, not
/// the terminal's own, across the whole width.
pub(super) fn status_row(t: &Tab) -> bool {
    let grid = t.term.grid();
    let row = &grid[Line(grid.screen_lines() as i32 - 1)];
    let first = row[Column(0)].bg;
    let default = matches!(first, Color::Named(alacritty_terminal::vte::ansi::NamedColor::Background));
    !default && (0..grid.columns()).all(|c| row[Column(c)].bg == first)
}
