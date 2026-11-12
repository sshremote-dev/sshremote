//! Keys, pastes, input-method text and the mouse, turned into what a terminal program reads.

use super::*;

/// Bytes a key sends to the shell, honouring application cursor mode.
pub(super) fn key_bytes(key: Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let csi = |code: &str, letter: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if app_cursor && code.is_empty() {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{code}{letter}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier > 1 { format!("\x1b[{n};{modifier}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let ss3 = |letter: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else {
            format!("\x1bO{letter}").into_bytes()
        }
    };
    Some(match key {
        // Alt+Enter is Meta+Return (Claude Code's new line); Shift and Ctrl change nothing.
        Key::Enter if m.alt => b"\x1b\r".to_vec(),
        Key::Enter => b"\r".to_vec(),
        // Ctrl+Space is NUL (Emacs' set-mark), with Alt as Meta.
        Key::Space if m.ctrl => if m.alt { b"\x1b\x00".to_vec() } else { vec![0] },
        Key::Backspace if m.ctrl => b"\x08".to_vec(),
        Key::Backspace if m.alt => b"\x1b\x7f".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab if m.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        Key::ArrowUp => csi("", 'A'),
        Key::ArrowDown => csi("", 'B'),
        Key::ArrowRight => csi("", 'C'),
        Key::ArrowLeft => csi("", 'D'),
        Key::Home => csi("", 'H'),
        Key::End => csi("", 'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => ss3('P'),
        Key::F2 => ss3('Q'),
        Key::F3 => ss3('R'),
        Key::F4 => ss3('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ => {
            // Ctrl or Alt with a letter, digit or symbol; plain text arrives as Event::Text.
            let name = key.symbol_or_name();
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            let c = c.to_ascii_lowercase();
            if m.ctrl {
                let byte = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    ' ' | '2' | '@' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '6' | '^' => 0x1e,
                    '/' | '-' | '7' => 0x1f,
                    '8' => 0x7f,
                    _ => return None,
                };
                // Ctrl+Alt+X is Meta+Ctrl+X: ESC then the control character. Not on Windows,
                // where AltGr arrives as Ctrl+Alt and its character comes as text.
                return match (m.alt, cfg!(windows)) {
                    (false, _) => Some(vec![byte]),
                    (true, false) => Some(vec![0x1b, byte]),
                    (true, true) => None,
                };
            }
            if m.alt && !m.ctrl && c.is_ascii_graphic() {
                let c = if m.shift { c.to_ascii_uppercase() } else { c };
                return Some(vec![0x1b, c as u8]);
            }
            return None;
        }
    })
}

/// Keys, text, input-method compositions and pastes for the shell of `t`, typed into the widget
/// `id`, whose input method composes at `cursor`. `grab` takes the keyboard focus for it.
pub(super) fn keyboard(
    ui: &egui::Ui,
    t: &mut Tab,
    id: egui::Id,
    area: Rect,
    cursor: Rect,
    grab: bool,
    out: &mut Vec<Cmd>,
) {
    keyboard_with(ui, t, id, area, cursor, grab, KeySkip::default(), out);
}

/// Keys the text view keeps for its own selection instead of sending them to the shell.
#[derive(Default, Clone, Copy)]
pub(super) struct KeySkip {
    /// Ctrl+A selects the whole text.
    pub(super) select_all: bool,
    /// Ctrl+C copies the selected text.
    pub(super) copy: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn keyboard_with(
    ui: &egui::Ui,
    t: &mut Tab,
    id: egui::Id,
    area: Rect,
    cursor: Rect,
    grab: bool,
    skip: KeySkip,
    out: &mut Vec<Cmd>,
) {
    // Holding the focus keeps Tab, the arrows and Escape for the shell instead of moving between the
    // window's buttons, where Enter or Space would then press one. It is asked for only when it is
    // elsewhere: each request also ends an input method's composition.
    let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
    let focused = ui.memory_mut(|m| {
        if grab && !m.has_focus(id) {
            m.request_focus(id);
        }
        if m.has_focus(id) {
            m.set_focus_lock_filter(id, filter);
        }
        m.has_focus(id)
    });
    if !focused {
        t.preedit.clear();
        return;
    }
    // An input method composes at the terminal's cursor, as in a text field.
    ui.ctx().output_mut(|o| {
        o.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Terminal,
            rect: area,
            cursor_rect: cursor,
            should_interrupt_composition: false,
        })
    });
    let mode = *t.term.mode();
    let mut send: Vec<u8> = Vec::new();
    // A key sent as Alt+key (ESC, then its character) comes again as text right after it; that
    // text is not typed a second time.
    let mut meta_sent: Option<String> = None;
    for e in ui.ctx().input(|i| i.events.clone()) {
        match e {
            Event::Text(s) if meta_sent.take().is_some_and(|m| m.eq_ignore_ascii_case(&s)) => {}
            Event::Text(s) => send.extend_from_slice(s.as_bytes()),
            Event::Ime(egui::ImeEvent::Preedit { text, .. }) => t.preedit = text,
            Event::Ime(egui::ImeEvent::Commit(s)) => {
                t.preedit.clear();
                send.extend_from_slice(s.as_bytes());
            }
            Event::Copy if skip.copy => {}
            Event::Key { key: Key::A, modifiers, .. } if skip.select_all && modifiers.command => {}
            Event::Copy => match t.term.selection_to_string() {
                Some(s) if !s.is_empty() => {
                    ui.ctx().copy_text(s);
                    t.term.selection = None;
                }
                // Ctrl+Insert arrives as a copy too: with nothing selected it does nothing,
                // where Ctrl+C interrupts.
                _ if insert_held() => {}
                _ => send.push(0x03),
            },
            // Shift+Delete arrives as a cut too: a terminal gets it as the key it is (xterm's
            // Shift+Delete), Ctrl+X as Ctrl+X.
            Event::Cut if ui.input(|i| i.modifiers.shift && !i.modifiers.ctrl) => send.extend_from_slice(b"\x1b[3;2~"),
            Event::Cut => send.push(0x18),
            Event::Paste(s) => {
                let s = s.replace("\r\n", "\r").replace('\n', "\r").replace("\x1b[201~", "");
                if mode.contains(TermMode::BRACKETED_PASTE) {
                    send.extend_from_slice(b"\x1b[200~");
                    send.extend_from_slice(s.as_bytes());
                    send.extend_from_slice(b"\x1b[201~");
                } else {
                    send.extend_from_slice(s.as_bytes());
                }
            }
            Event::Key { key: Key::C, pressed: true, modifiers, .. } if modifiers.ctrl && modifiers.shift => {
                if let Some(s) = t.term.selection_to_string().filter(|s| !s.is_empty()) {
                    ui.ctx().copy_text(s);
                }
            }
            Event::Key { key: Key::V, pressed: true, modifiers, .. } if modifiers.ctrl && modifiers.shift => {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            // Ctrl+Insert copies and Shift+Insert pastes, as in GNOME Terminal and xterm (on
            // Windows they arrive as copy and paste already).
            Event::Key { key: Key::Insert, pressed: true, modifiers, .. } if modifiers.ctrl && !modifiers.shift => {
                if let Some(s) = t.term.selection_to_string().filter(|s| !s.is_empty()) {
                    ui.ctx().copy_text(s);
                }
            }
            Event::Key { key: Key::Insert, pressed: true, modifiers, .. } if modifiers.shift && !modifiers.ctrl => {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            Event::Key { key, pressed: true, modifiers, .. } => {
                // The window's own shortcuts (new or close tab, next tab, text size) are not typed.
                let cs = modifiers.ctrl && modifiers.shift;
                let alt_shift = modifiers.alt && modifiers.shift && !modifiers.ctrl;
                let alt = modifiers.alt && !modifiers.shift && !modifiers.ctrl;
                if (cs && matches!(key, Key::T | Key::W | Key::F))
                    || (modifiers.ctrl && matches!(key, Key::Tab | Key::Equals | Key::Plus | Key::Minus))
                    || (alt_shift && matches!(key, Key::Equals | Key::Plus | Key::Minus | Key::D))
                    || (alt && t.nav_keys && matches!(key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown))
                {
                    continue;
                }
                if modifiers.shift && matches!(key, Key::PageUp | Key::PageDown) {
                    t.term.scroll_display(if key == Key::PageUp { Scroll::PageUp } else { Scroll::PageDown });
                    continue;
                }
                if let Some(b) = key_bytes(key, modifiers, mode.contains(TermMode::APP_CURSOR)) {
                    if modifiers.alt && b.len() == 2 && b[0] == 0x1b && b[1].is_ascii_graphic() {
                        meta_sent = Some((b[1] as char).to_string());
                    }
                    send.extend_from_slice(&b);
                }
            }
            _ => {}
        }
    }
    if !send.is_empty() && t.exited.is_none() {
        t.seek = None;
        t.term.scroll_display(Scroll::Bottom);
        t.typed_at = ui.input(|i| i.time);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    } else if !t.preedit.is_empty() {
        t.typed_at = ui.input(|i| i.time);
    }
}

/// Whether Insert is held: on Windows Ctrl+Insert reaches us only as a copy, not as its key.
fn insert_held() -> bool {
    crate::sys::insert_held()
}

/// Where a click asked the cursor to go (a screen line and column), reached with arrow keys a
/// batch at a time as the program moves the cursor: in a full-screen program Up and Down, then
/// Left and Right; on the shell's own line Left and Right only (Up and Down are its history).
pub(super) struct Seek {
    line: i32,
    col: usize,
    screen: bool,
    /// Where the cursor was when the last keys went, and when.
    last: Option<((i32, usize), std::time::Instant)>,
    tries: u8,
}

/// A click at a screen cell: the cursor goes there, as far as the program lets it. A program that
/// asked for the mouse gets the click itself; the shell's cursor moves only along the line it is
/// typing; a click while scrolled back, or after the program ended, does nothing.
pub(super) fn click_cursor(t: &mut Tab, line: i32, col: usize, out: &mut Vec<Cmd>) {
    let grid = t.term.grid();
    if t.exited.is_some() || grid.display_offset() != 0 || line < 0 || line >= grid.screen_lines() as i32 {
        return;
    }
    let mode = *t.term.mode();
    if mode.intersects(TermMode::MOUSE_MODE) {
        let mut send = mouse_report(mode, 0, col, line as usize, true);
        send.extend(mouse_report(mode, 0, col, line as usize, false));
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
        return;
    }
    let screen = mode.contains(TermMode::ALT_SCREEN);
    let cursor = grid.cursor.point.line.0;
    // A pager (less, man, git log) keeps its cursor on the last row, at its prompt: there the
    // arrows would scroll, not move a cursor, so a click leaves it be. Editors keep theirs in the text.
    if screen && cursor == grid.screen_lines() as i32 - 1 {
        return;
    }
    // The shell's line: the cursor's row and the rows it wraps across.
    let wraps = |l: i32| grid[Line(l)][Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE);
    if !screen && !(line.min(cursor)..line.max(cursor)).all(wraps) {
        return;
    }
    // Not past the end of the row's text: Right there would go on to the next line.
    let end = row_end(t, line);
    let col = if screen || line != cursor || col > grid.cursor.point.column.0 { col.min(end) } else { col };
    t.seek = Some(Seek { line, col, screen, last: None, tries: 0 });
    seek_step(t, out);
}

/// Cells up to the last one with text on a screen row.
fn row_end(t: &Tab, line: i32) -> usize {
    let row = &t.term.grid()[Line(line)];
    (0..t.term.grid().columns()).rev().find(|&c| !matches!(row[Column(c)].c, ' ' | '\0')).map_or(0, |c| c + 1)
}

/// Characters (not cells: a wide one takes two) from one screen point to another, in reading order.
fn chars_between(t: &Tab, from: (i32, usize), to: (i32, usize)) -> usize {
    let grid = t.term.grid();
    let cols = grid.columns();
    // A point from before the terminal changed size is kept inside it.
    let (top, bottom) = (-(grid.history_size() as i32), grid.screen_lines() as i32 - 1);
    let fit = |(l, c): (i32, usize)| (l.clamp(top, bottom), c.min(cols.saturating_sub(1)));
    let (from, to) = (fit(from), fit(to));
    let (a, b) = if from <= to { (from, to) } else { (to, from) };
    let mut n = 0;
    let (mut l, mut c) = a;
    while (l, c) < b {
        if !grid[Line(l)][Column(c)].flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            n += 1;
        }
        c += 1;
        if c == cols {
            c = 0;
            l += 1;
        }
    }
    n
}

/// Sends the next arrow keys of a click's cursor move once the cursor has answered the last;
/// true while the move goes on. It stops on arrival, when the keys stop bringing the cursor
/// closer (a tab, the end of the text), or when the program does not move it at all.
pub(super) fn seek_step(t: &mut Tab, out: &mut Vec<Cmd>) -> bool {
    let Some(s) = t.seek.as_ref() else { return false };
    let p = t.term.grid().cursor.point;
    let at = (p.line.0, p.column.0);
    let target = (s.line, s.col);
    let far = |q: (i32, usize)| {
        if s.screen {
            (s.line - q.0).unsigned_abs() as usize * 10_000 + q.1.abs_diff(s.col)
        } else {
            chars_between(t, q, target)
        }
    };
    if let Some((before, when)) = s.last {
        if before == at {
            // Not moved yet: wait for the program a little, then leave it.
            if when.elapsed() > std::time::Duration::from_millis(700) {
                t.seek = None;
            }
            return t.seek.is_some();
        }
        if far(at) >= far(before) || s.tries >= 8 {
            t.seek = None;
            return false;
        }
    }
    if at == target {
        t.seek = None;
        return false;
    }
    let app = t.term.mode().contains(TermMode::APP_CURSOR);
    let key = |k: u8| if app { [0x1b, b'O', k] } else { [0x1b, b'[', k] };
    let (k, n) = if s.screen && at.0 != s.line {
        (if s.line < at.0 { b'A' } else { b'B' }, (s.line - at.0).unsigned_abs() as usize)
    } else if s.screen {
        (if s.col < at.1 { b'D' } else { b'C' }, chars_between(t, (at.0, at.1.min(s.col)), (at.0, at.1.max(s.col))))
    } else {
        (if target < at { b'D' } else { b'C' }, chars_between(t, at, target))
    };
    if n == 0 {
        t.seek = None;
        return false;
    }
    let send: Vec<u8> = (0..n.min(400)).flat_map(|_| key(k)).collect();
    out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    let s = t.seek.as_mut().expect("seeking");
    s.last = Some((at, std::time::Instant::now()));
    s.tries += 1;
    true
}

/// A mouse report for a program that asked for the mouse: `button` 0 left, 1 middle, 2 right,
/// 64 and 65 the wheel up and down, plus 32 while dragging; `col` and `row` from 0. SGR form when
/// the program asked for it, the classic form otherwise.
pub(super) fn mouse_report(mode: TermMode, button: u8, col: usize, row: usize, pressed: bool) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        format!("\x1b[<{button};{};{}{}", col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
    } else {
        let b = if pressed { button } else { 3 };
        let at = |v: usize| (32 + v + 1).min(255) as u8;
        vec![0x1b, b'[', b'M', 32 + b, at(col), at(row)]
    }
}

/// What the wheel sends to a full-screen program, `lines` up when positive: wheel reports when it
/// asked for the mouse (Claude Code, vim, htop), otherwise arrow keys (less, man).
pub(super) fn wheel_bytes(mode: TermMode, lines: i32, col: usize, row: usize) -> Vec<u8> {
    let mut send = Vec::new();
    for _ in 0..lines.unsigned_abs() {
        if mode.intersects(TermMode::MOUSE_MODE) {
            send.extend(mouse_report(mode, if lines > 0 { 64 } else { 65 }, col, row, true));
        } else {
            let app = mode.contains(TermMode::APP_CURSOR);
            send.extend_from_slice(match (lines > 0, app) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            });
        }
    }
    send
}

/// Wheel steps in lines from this frame's events, up positive.
pub(super) fn wheel_lines(ui: &egui::Ui, ch: f32, page: f32) -> i32 {
    // What is left of a line from earlier small steps (touchpads scroll by a few points).
    let rest_id = egui::Id::unique("terminal-wheel-rest");
    let mut total = ui.ctx().data(|d| d.get_temp::<f32>(rest_id)).unwrap_or(0.0);
    let mut any = false;
    for e in ui.ctx().input(|i| i.events.clone()) {
        let Event::MouseWheel { unit, delta, .. } = e else { continue };
        if delta.y == 0.0 {
            continue;
        }
        any = true;
        total += match unit {
            egui::MouseWheelUnit::Point => delta.y / ch,
            egui::MouseWheelUnit::Line => delta.y,
            egui::MouseWheelUnit::Page => delta.y * page,
        };
    }
    if !any {
        return 0;
    }
    let whole = total.trunc();
    ui.ctx().data_mut(|d| d.insert_temp(rest_id, total - whole));
    whole as i32
}

/// Whether the wheel belongs to the program rather than to the view: a full-screen program, or one
/// that asked for the mouse.
pub(super) fn program_scrolls(t: &Tab) -> bool {
    t.term.mode().intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
}

/// The text and Markdown views of a program that scrolls itself: the wheel over them goes to the
/// program, at the middle of its screen.
pub(super) fn forward_wheel(ui: &egui::Ui, t: &Tab, area: Rect, ch: f32, out: &mut Vec<Cmd>) {
    if !program_scrolls(t) || !ui.rect_contains_pointer(area) || t.exited.is_some() {
        return;
    }
    let grid = t.term.grid();
    let lines = wheel_lines(ui, ch, grid.screen_lines() as f32);
    if lines != 0 {
        let send = wheel_bytes(*t.term.mode(), lines, grid.columns() / 2, grid.screen_lines() / 2);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    }
}
