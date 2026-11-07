//! The text and Markdown views: the scrollback as a document, the live rows where typing goes,
//! and terminal styles read back as Markdown.

use super::*;

/// Draws a shell and passes keys, paste and the mouse wheel to it.
/// The whole buffer as text, plain or read as Markdown.
/// The text and Markdown views of a tab, as one document in the manner of VS Code's editor and
/// Markdown preview: the scrollback above, and at its end the live rows where the prompt and the
/// cursor are, which take the keys as the terminal view does. Only those rows show a text cursor
/// for typing; the rest can be selected and copied.
pub(super) fn reader(ui: &mut egui::Ui, t: &mut Tab, font_size: f32, keys: bool, out: &mut Vec<Cmd>) {
    if t.view == View::Text {
        let font = FontId::monospace(font_size);
        let (cw, ch) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, CornerRadius::ZERO, scheme_bg());
        return text_editor(ui, t, &font, cw, ch, keys, out);
    }
    let font = FontId::monospace(font_size);
    let (cw, ch) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let (first, last) = input_rows(t);
    let now = ui.input(|i| i.time);
    let due = t.reading.is_none() || now - t.built_at >= REBUILD_EVERY;
    if !due {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REBUILD_EVERY));
    }
    if due && t.reading.as_ref().is_none_or(|r| r.0 != t.rev || r.1 != first) {
        t.built_at = now;
        let text = if t.term.mode().contains(TermMode::ALT_SCREEN) {
            // A full-screen program shows a window on a longer text: its screens are stitched
            // together, so a reply or a table cut off by scrolling keeps its shape.
            let end = if status_row(t) && first == t.term.grid().screen_lines() as i32 { first - 1 } else { first };
            stitch_screen(t, screen_markdown(t, 0, end).lines().map(str::to_string).collect());
            t.stitched.join("\n")
        } else {
            t.stitched.clear();
            t.last_screen.clear();
            t.overlays.clear();
            t.stitch_at = None;
            sync_rows(t);
            let rows: Vec<(String, bool)> = t.rows.md.iter().cloned().chain((0..first).map(|l| row_markdown(t, l))).collect();
            join_rows(rows)
        };
        let doc = crate::markdown::Doc::new(&text);
        t.reading = Some((t.rev, first, std::rc::Rc::new((text, doc))));
    }
    let read = t.reading.as_ref().expect("just built").2.clone();
    let doc = &read.1;
    let th = crate::markdown::Theme::terminal(scheme_bg(), scheme_fg());
    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, CornerRadius::ZERO, th.bg);
    ui.visuals_mut().override_text_color = Some(th.fg);
    let markdown = t.view == View::Markdown;
    let id = egui::Id::unique(("reader-input", t.id));
    let live_rows = (last - first + 1) as usize;
    let mut live: Option<(Rect, egui::Response)> = None;
    // The keys come to the live rows unless selected text has them; typing takes them back.
    let typing = ui.input(|i| i.events.iter().any(|e| matches!(e, Event::Text(_) | Event::Ime(_) | Event::Paste(_))));
    let elsewhere = ui.memory(|m| m.focused().is_some_and(|f| f != id));
    let wheel = if ui.rect_contains_pointer(area) { wheel_lines(ui, ch, 10.0) } else { 0 };
    let scroll =
        egui::ScrollArea::both().id_salt(("reading", t.id, markdown)).auto_shrink([false, false]).stick_to_bottom(true);
    let mut heights = std::mem::take(&mut t.md_heights);
    let doc_id = egui::Id::unique(("md-doc", t.id));
    // The document's identity: a new one is built when the output changes.
    let doc_key = std::rc::Rc::as_ptr(&read) as usize as u64;
    let shown = scroll.show_viewport(ui, |ui, view| {
        if markdown {
            // A readable column, as in the preview; the live rows sit under it on the same edge.
            // Only the blocks in view are laid out and drawn, so long output stays quick.
            let full = ui.available_width();
            let width = full.min(960.0);
            let pad = ((full - width) / 2.0).max(0.0) + 20.0;
            let column = width - 40.0;
            ui.spacing_mut().item_spacing.y = 6.0;
            let end =
                crate::markdown::show_in_view(ui, doc, doc_key, doc_id, &th, &mut heights, view, pad, column, 14.0);
            let origin = ui.max_rect().min;
            let rows = Rect::from_min_size(
                origin + Vec2::new(pad, end),
                Vec2::new(column.max(cw * 20.0), live_rows as f32 * ch),
            );
            let resp = ui.interact(rows, id, Sense::click());
            live = Some((rows, resp));
            // The content's size, for the scroll area.
            ui.allocate_rect(
                Rect::from_min_max(origin, Pos2::new(origin.x + full, rows.bottom() + 6.0)),
                Sense::hover(),
            );
        }
    });
    t.md_heights = heights;
    // At the top of the document the wheel asks the program to scroll back, and what it shows is
    // stitched on above; at the bottom it scrolls the program forward.
    let at_top = shown.state.clamped_offset().y <= 1.0;
    let at_bottom = shown.state.clamped_offset().y + shown.inner_rect.height() >= shown.content_size.y - 1.0;
    if program_scrolls(t) && t.exited.is_none() && ((wheel > 0 && at_top) || (wheel < 0 && at_bottom)) {
        let grid = t.term.grid();
        let send = wheel_bytes(*t.term.mode(), wheel, grid.columns() / 2, grid.screen_lines() / 2);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    }
    let Some((rect, resp)) = live else { return };
    let rect = rect.intersect(shown.inner_rect);
    let grab = keys && (resp.clicked() || typing || !elsewhere);
    let c = t.term.grid().cursor.point;
    let full = resp.rect;
    let at = Pos2::new(full.left() + c.column.0 as f32 * cw, full.top() + (c.line.0 - first) as f32 * ch);
    if keys {
        // Ctrl+C copies text selected in the document (egui copies it); with nothing selected it
        // interrupts the program, as in the terminal.
        let selection = ui.ctx().plugin_opt::<egui::text_selection::LabelSelectionState>();
        let copied = egui::Id::unique(("copied", t.id));
        // Copied the frame before: the selection goes, so the next Ctrl+C interrupts, as in
        // Windows Terminal. (Cleared any earlier, the copy itself would be lost.)
        if ui.ctx().data_mut(|d| d.remove_temp::<bool>(copied)).is_some()
            && let Some(p) = &selection
        {
            p.lock().clear_selection();
        }
        let selected = selection.is_some_and(|p| p.lock().has_selection());
        if selected && ui.input(|i| i.events.contains(&Event::Copy)) {
            ui.ctx().data_mut(|d| d.insert_temp(copied, true));
            ui.ctx().request_repaint();
        }
        let skip = KeySkip { select_all: false, copy: selected };
        keyboard_with(ui, t, id, rect, Rect::from_min_size(at, Vec2::new(cw, ch)), grab, skip, out);
    }
    let focused = ui.memory(|m| m.has_focus(id));
    let mut clip = ui.new_child(egui::UiBuilder::new().max_rect(shown.inner_rect));
    clip.set_clip_rect(shown.inner_rect);
    paint(&clip, t, full, &font, cw, ch, first, focused);
    // Only where typing goes shows a text cursor; elsewhere the pointer stays an arrow.
    if let Some(p) = ui.ctx().pointer_hover_pos().filter(|p| shown.inner_rect.contains(*p)) {
        let icon = if rect.contains(p) { egui::CursorIcon::Text } else { egui::CursorIcon::Default };
        ui.ctx().set_cursor_icon(icon);
    }
}

/// Rows of the live screen shown under the text and Markdown views, so what is typed can be seen:
/// from the cursor's row down to the last written row (an agent's input box and status).
pub(super) fn input_rows(t: &Tab) -> (i32, i32) {
    let grid = t.term.grid();
    let cursor = grid.cursor.point.line.0;
    let screen = grid.screen_lines() as i32;
    let cols = grid.columns();
    let written = |row: i32| (0..cols).any(|c| !matches!(grid[Line(row)][Column(c)].c, ' ' | '\0'));
    let last = (cursor..screen).rev().find(|&r| written(r)).unwrap_or(cursor).max(cursor);
    // From the cursor's row down: what is above it is already output, read as the document.
    let mut first = cursor.max(last - 9);
    // A frame a program draws around its input (a rule or the top of a box right above the
    // prompt) belongs with it.
    let rule = |row: i32| {
        let mut n = 0;
        let only_frame = (0..cols).all(|c| {
            let ch = grid[Line(row)][Column(c)].c;
            n += ('\u{2500}'..='\u{257f}').contains(&ch) as usize;
            matches!(ch, ' ' | '\0') || ('\u{2500}'..='\u{257f}').contains(&ch)
        });
        only_frame && n * 2 >= cols
    };
    while first > 0 && last - first < 12 && rule(first - 1) {
        first -= 1;
    }
    (first, last)
}

/// The rows `from` to `to` of the scrollback and screen, one line each as shown, without trailing
/// spaces, and the character offset of the terminal cursor in their joined text.
pub(super) fn screen_text(t: &Tab, from: i32, to: i32) -> (String, usize) {
    let grid = t.term.grid();
    let cols = grid.columns();
    let cursor = grid.cursor.point;
    let mut out = String::new();
    let mut at = 0;
    for line in from..=to {
        if line > from {
            out.push('\n');
        }
        let row = &grid[Line(line)];
        let mut text = String::new();
        let mut upto_cursor = 0;
        for c in 0..cols {
            let cell = &row[Column(c)];
            if line == cursor.line.0 && c == cursor.column.0 {
                upto_cursor = text.chars().count();
            }
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(if cell.c == '\0' { ' ' } else { cell.c });
        }
        if line == cursor.line.0 {
            // The cursor's row keeps its spaces up to the cursor, so the caret sits where it is.
            let keep = text.trim_end().chars().count().max(upto_cursor);
            let kept: String = text.chars().take(keep).collect();
            at = out.chars().count() + upto_cursor;
            out.push_str(&kept);
        } else {
            out.push_str(text.trim_end());
        }
    }
    (out, at)
}

/// The scrollback's rows as Markdown and as plain text, oldest first. Rows that have scrolled off
/// the screen do not change, so each is worked out once.
#[derive(Default)]
pub(super) struct Rows {
    md: std::collections::VecDeque<(String, bool)>,
    plain: std::collections::VecDeque<String>,
    cols: usize,
}

#[cfg(test)]
impl Rows {
    pub(super) fn len(&self) -> usize {
        self.plain.len()
    }

    pub(super) fn md(&self) -> Vec<(String, bool)> {
        self.md.iter().cloned().collect()
    }
}

/// One row as plain text without trailing spaces.
fn row_plain(t: &Tab, line: i32) -> String {
    let row = &t.term.grid()[Line(line)];
    let mut text = String::new();
    for c in 0..t.term.grid().columns() {
        let cell = &row[Column(c)];
        if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            text.push(if cell.c == '\0' { ' ' } else { cell.c });
        }
    }
    text.truncate(text.trim_end().len());
    text
}

/// Brings the cache up to the scrollback: rows that scrolled in are added, rows the scrollback
/// dropped are let go. When the two cannot be matched (cleared, resized) it is made anew.
pub(super) fn sync_rows(t: &mut Tab) {
    let grid = t.term.grid();
    let history = grid.total_lines() - grid.screen_lines();
    let cols = grid.columns();
    let cached = t.rows.plain.len();
    // The scrollback's row `h` (oldest first) as a line number of the grid.
    let line = |h: usize| h as i32 - history as i32;
    // Whether the cache matches the scrollback once its `d` oldest rows are dropped: the newest
    // rows and rows spread over the whole overlap must agree, so repeated output at the bottom
    // cannot pass for a scrollback that did not move.
    let matches = |t: &Tab, d: usize| {
        let overlap = cached - d;
        let spread = (0..32).map(|i| d + overlap * i / 32);
        let newest = (cached.saturating_sub(3).max(d)..cached).rev();
        newest.chain(spread).all(|j| row_plain(t, line(j - d)) == t.rows.plain[j])
    };
    let dropped = if t.rows.cols != cols || cached == 0 {
        None
    } else {
        // Rows that scrolled in come after the cached ones; the oldest may have dropped off the top.
        (cached.saturating_sub(history)..cached).find(|&d| matches(t, d))
    };
    match dropped {
        Some(d) => {
            t.rows.md.drain(..d);
            t.rows.plain.drain(..d);
            for h in cached - d..history {
                let md = row_markdown(t, line(h));
                let plain = row_plain(t, line(h));
                t.rows.md.push_back(md);
                t.rows.plain.push_back(plain);
            }
        }
        None => {
            let (md, plain) = (0..history).map(|h| (row_markdown(t, line(h)), row_plain(t, line(h)))).unzip();
            t.rows = Rows { md, plain, cols };
        }
    }
}

/// The text view: one read-only document of the whole scrollback and screen in the terminal's font,
/// with line numbers, selected as in VS Code (drag, double-click for a word, Shift+click, Ctrl+A,
/// Ctrl+C) and a caret where the shell's cursor is, which takes typing for the shell.
pub(super) fn text_editor(
    ui: &mut egui::Ui,
    t: &mut Tab,
    font: &FontId,
    cw: f32,
    ch: f32,
    keys: bool,
    out: &mut Vec<Cmd>,
) {
    let (_, last) = input_rows(t);
    let key = (t.rev, t.term.grid().cursor.point.line.0, t.term.grid().cursor.point.column.0);
    let now = ui.input(|i| i.time);
    let due = t.editor.is_none() || now - t.built_at >= REBUILD_EVERY;
    if !due {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REBUILD_EVERY));
    }
    if due && t.editor.as_ref().is_none_or(|e| e.0 != key) {
        t.built_at = now;
        // The scrollback from the cache, the screen worked out now (the cursor is there).
        sync_rows(t);
        let (screen, at) = screen_text(t, 0, last);
        let mut text = String::with_capacity(t.rows.plain.iter().map(|r| r.len() + 1).sum::<usize>() + screen.len());
        let mut before = 0;
        for row in &t.rows.plain {
            text.push_str(row);
            text.push('\n');
            before += row.chars().count() + 1;
        }
        text.push_str(&screen);
        t.editor = Some((key, std::rc::Rc::new((text, before + at))));
    }
    let (doc_key, doc) = t.editor.as_ref().map(|e| (e.0, e.1.clone())).expect("just built");
    // The laid-out text, made again only when the text, its font or its colour changes, not every
    // frame (a long scrollback takes a while to lay out). It is keyed by the document it was made
    // from: the terminal may already be ahead of a document waiting for its next rebuild.
    let fg_now = scheme_fg();
    let stale = t.editor_galley.as_ref().is_none_or(|g| g.0 != doc_key || g.1 != font.size || g.2 != fg_now);
    if stale {
        let job = ui.ctx().fonts_mut(|f| crate::markdown::mono_job(&doc.0, font, fg_now, f));
        t.editor_galley = Some((doc_key, font.size, fg_now, std::sync::Arc::new(job)));
    }
    // egui lays the job out again only when it or the font atlas changed; a galley kept across
    // frames would point into an atlas that may have been rebuilt.
    let job = t.editor_galley.as_ref().expect("just made").3.clone();
    let laid = ui.ctx().fonts_mut(|f| f.layout_job((*job).clone()));
    let (text, at) = (&doc.0, doc.1);
    let fg = scheme_fg();
    let id = egui::Id::unique(("text-view", t.id));
    // Typing collapses a selection onto the shell's cursor, as typing would in an editor.
    let typing = ui.input(|i| {
        i.events.iter().any(|e| matches!(e, Event::Text(_) | Event::Ime(_) | Event::Paste(_)))
            || i.events.iter().any(|e| {
                matches!(e, Event::Key { pressed: true, key, modifiers, .. }
                if !modifiers.ctrl && !modifiers.command && !matches!(key, Key::A | Key::C))
            })
    });
    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    let selecting = state.cursor.char_range().is_some_and(|r| !r.is_empty());
    if typing && selecting {
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(at))));
        state.store(ui.ctx(), id);
    }
    let lines = text.lines().count().max(1);
    let gutter = cw * (lines.to_string().len() as f32 + 2.0);
    let mut caret_rect = None;
    let own_wheel = if program_scrolls(t) {
        egui::scroll_area::ScrollSource::SCROLL_BAR
    } else {
        egui::scroll_area::ScrollSource::MOUSE_WHEEL | egui::scroll_area::ScrollSource::SCROLL_BAR
    };
    forward_wheel(ui, t, ui.available_rect_before_wrap(), ch, out);
    let scroll = egui::ScrollArea::both()
        .id_salt(("text-scroll", t.id))
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .scroll_source(own_wheel);
    let shown = scroll.show(ui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            // The numbers go where the text's rows are, which are not exactly the font's row height
            // apart; computed positions would drift away from long text.
            let (g, _) = ui.allocate_exact_size(Vec2::new(gutter, ch), Sense::hover());
            let mut layouter = |_: &egui::Ui, _: &dyn egui::TextBuffer, _: f32| laid.clone();
            let mut view: &str = text;
            let edit = egui::TextEdit::multiline(&mut view)
                .id(id)
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::ZERO)
                .desired_width(f32::INFINITY)
                .desired_rows(1)
                .layouter(&mut layouter)
                .show(ui);
            edit.response.context_menu(|ui| {
                if ui.button(format!("{}  Copy all", ic::COPY)).clicked() {
                    ui.ctx().copy_text(text.clone());
                    ui.close();
                }
                if ui.button("Paste").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    ui.ctx().memory_mut(|m| m.request_focus(id));
                    ui.close();
                }
            });
            let clip = ui.clip_rect();
            let number = fg.gamma_multiply(0.4);
            let mut line = 0;
            let mut new_line = true;
            for row in &edit.galley.rows {
                let r = row.rect().translate(edit.galley_pos.to_vec2());
                if new_line {
                    line += 1;
                    if r.bottom() >= clip.top() && r.top() <= clip.bottom() {
                        let pos = Pos2::new(g.right() - cw, r.top());
                        ui.painter().text(pos, Align2::RIGHT_TOP, line.to_string(), font.clone(), number);
                    }
                }
                new_line = row.ends_with_newline;
            }
            let p = edit.galley.pos_from_cursor(egui::text::CCursor::new(at));
            caret_rect = Some(p.translate(edit.galley_pos.to_vec2()).with_max_x(p.min.x + edit.galley_pos.x + cw));
        });
        ui.add_space(8.0);
    });
    let Some(caret) = caret_rect else { return };
    let caret = Rect::from_min_size(caret.min, Vec2::new(cw, ch));
    let selecting = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|s| s.cursor.char_range())
        .is_some_and(|r| !r.is_empty());
    // The keys go to the shell while the text has the focus; the view takes it when nothing has.
    let grab = keys && (typing || ui.memory(|m| m.focused().is_none()));
    if keys {
        let skip = KeySkip { select_all: true, copy: selecting };
        keyboard_with(ui, t, id, shown.inner_rect, caret, grab, skip, out);
        // Copied: the selection goes, so the next Ctrl+C interrupts, as in Windows Terminal.
        if selecting
            && ui.input(|i| i.events.contains(&Event::Copy))
            && let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id)
        {
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(at))));
            state.store(ui.ctx(), id);
        }
    }
    let focused = ui.memory(|m| m.has_focus(id));
    let painter = ui.painter_at(shown.inner_rect);
    if !t.preedit.is_empty() {
        let galley = painter.layout_no_wrap(t.preedit.clone(), font.clone(), fg);
        let r = Rect::from_min_size(caret.min, Vec2::new(galley.size().x.max(cw), ch));
        painter.rect_filled(r, CornerRadius::ZERO, scheme_bg());
        painter.galley(caret.min, galley, fg);
        painter.hline(r.x_range(), r.bottom() - 1.0, egui::Stroke::new(1.5, ACCENT));
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(r.right(), r.top()), Vec2::new(2.0, ch)),
            CornerRadius::ZERO,
            fg,
        );
    } else if focused && !selecting && t.exited.is_none() {
        let since = ui.input(|i| i.time) - t.typed_at;
        if since < 0.6 || ((since - 0.6) / 0.53) as u64 % 2 == 1 {
            painter.rect_filled(Rect::from_min_size(caret.min, Vec2::new(2.0, ch)), CornerRadius::ZERO, fg);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(0.53 - (since - 0.6).rem_euclid(0.53)));
    }
}

/// Markers an agent puts before a line, kept as they are ahead of the line's own formatting.
pub(super) const LINE_MARKERS: [&str; 6] = ["● ", "⏺ ", "❯ ", "› ", "• ", "▎ "];

/// The rows `from` to `to` (exclusive) as Markdown, with what the terminal showed as styles written
/// back as Markdown: a whole bold line becomes a heading (level 1 when also underlined or italic,
/// as Claude Code draws `#`), bold and italic runs become `**` and `*`. Agents draw Markdown with
/// these attributes and drop the marks, so this gives the Markdown view its structure back.
pub(super) fn screen_markdown(t: &Tab, from: i32, to: i32) -> String {
    join_rows((from..to).map(|line| row_markdown(t, line)))
}

/// Rows put together into lines: a row the terminal wrapped goes on in the next one, as one line.
pub(super) fn join_rows(rows: impl IntoIterator<Item = (String, bool)>) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut join = false;
    for (text, wraps) in rows {
        match lines.last_mut() {
            Some(last) if join => last.push_str(match text.split_once(SPLIT) {
                Some((_, md)) => md,
                None => text.trim_start_matches(CODE),
            }),
            _ => lines.push(text),
        }
        join = wraps;
    }
    fence_code(lines).join("\n").trim_end().to_string()
}

/// Mark a row read as code (see `row_markdown`), and one with a single colour that may be code
/// beside it; `join_rows` turns runs of them into fences and drops the marks.
const CODE: char = '\u{E000}';
const TINT: char = '\u{E001}';
const SPLIT: char = '\u{E002}';

/// Runs of code rows become fenced blocks with their indentation kept: blank rows inside a run
/// and lines of only closing brackets after one belong to it.
fn fence_code(mut lines: Vec<String>) -> Vec<String> {
    // A single-coloured row right next to code is code too ("else:", "}" in a keyword colour).
    let mut grew = true;
    while grew {
        grew = false;
        for i in 0..lines.len() {
            let near = |j: Option<usize>| j.and_then(|j| lines.get(j)).is_some_and(|l| l.starts_with(CODE));
            if lines[i].starts_with(TINT) && (near(i.checked_sub(1)) || near(Some(i + 1))) {
                let raw = lines[i][TINT.len_utf8()..].split(SPLIT).next().unwrap_or("").to_string();
                lines[i] = format!("{CODE}{raw}");
                grew = true;
            }
        }
    }
    for l in lines.iter_mut().filter(|l| l.starts_with(TINT)) {
        *l = l.split_once(SPLIT).map_or(String::new(), |(_, md)| md.to_string());
    }
    let is_code = |l: &str| l.starts_with(CODE);
    let closer = |l: &str| {
        let t = l.trim();
        !t.is_empty() && t.chars().count() <= 3 && t.chars().all(|c| "{}[]()<>;,".contains(c))
    };
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if !is_code(&lines[i]) {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        let (mut j, mut end) = (i, i);
        while j < lines.len() {
            let l = &lines[j];
            if is_code(l) || closer(l) {
                end = j + 1;
            } else if !l.trim().is_empty() {
                break;
            }
            j += 1;
        }
        let block: Vec<String> = lines[i..end].iter().map(|l| l.trim_start_matches(CODE).to_string()).collect();
        let indent = block.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
        let pad = " ".repeat(indent);
        out.push(format!("{pad}```"));
        out.extend(block);
        out.push(format!("{pad}```"));
        i = end;
    }
    out
}

/// One row as Markdown (see [`screen_markdown`]), and whether the terminal wrapped it into the next.
pub(super) fn row_markdown(t: &Tab, line: i32) -> (String, bool) {
    let grid = t.term.grid();
    let cols = grid.columns();
    let row = &grid[Line(line)];
    let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
    let mut cells: Vec<(char, bool, bool, bool)> = Vec::new();
    for c in 0..cols {
        let cell = &row[Column(c)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let ch = if cell.c == '\0' { ' ' } else { cell.c };
        let f = cell.flags;
        cells.push((ch, f.contains(Flags::BOLD), f.intersects(Flags::ITALIC), f.contains(Flags::UNDERLINE)));
    }
    // A row that goes on in the next keeps its last spaces: they separate the words there.
    while !wrapped && cells.last().is_some_and(|c| c.0 == ' ') {
        cells.pop();
    }
    let plain: String = cells.iter().map(|c| c.0).collect();
    // Code as programs print it, coloured by its syntax and without fences: several colours
    // taking turns on one row. It is kept as it is, to be fenced with its neighbours; a row with
    // one colour is marked too, and joins code right next to it.
    let mut colours: Vec<Color> = Vec::new();
    if !plain.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)) {
        for c in 0..cols {
            let cell = &row[Column(c)];
            let plain_fg = matches!(cell.fg, Color::Named(NamedColor::Foreground | NamedColor::DimForeground));
            if !matches!(cell.c, ' ' | '\0') && !plain_fg && !colours.contains(&cell.fg) {
                colours.push(cell.fg);
            }
        }
        if colours.len() >= 2 {
            return (format!("{CODE}{plain}"), wrapped);
        }
    }
    let (text, wraps) = row_markdown_styled(&cells, &plain, wrapped);
    if colours.len() == 1 {
        // Both readings: as code if it joins code, as Markdown if not.
        return (format!("{TINT}{plain}{SPLIT}{text}"), wraps);
    }
    (text, wraps)
}

/// A row without code colours as Markdown: headings, bold and italic.
fn row_markdown_styled(cells: &[(char, bool, bool, bool)], plain: &str, wrapped: bool) -> (String, bool) {
    // Leading spaces and an agent's marker stay as they are.
    let indent = plain.len() - plain.trim_start().len();
    let mut start = plain[..indent].chars().count();
    let rest: String = plain.trim_start().to_string();
    let mut prefix = plain[..indent].to_string();
    if let Some(m) = LINE_MARKERS.iter().find(|m| rest.starts_with(**m)) {
        prefix.push_str(m);
        start += m.chars().count();
    }
    let body = &cells[start.min(cells.len())..];
    let text: Vec<&(char, bool, bool, bool)> = body.iter().filter(|c| !c.0.is_whitespace()).collect();
    let boxy = body.iter().any(|c| ('\u{2500}'..='\u{257f}').contains(&c.0));
    let words: String = body.iter().map(|c| c.0).collect();
    // Text laid out in columns stays as it is, without marks.
    if crate::markdown::columned(&words) {
        return (format!("{prefix}{words}"), wrapped);
    }
    if !text.is_empty() && !boxy && text.iter().all(|c| c.1) {
        let level = if text.iter().all(|c| c.2 || c.3) { "#" } else { "##" };
        return (format!("{prefix}{level} {}", words.trim()), wrapped);
    }
    let mut out = prefix;
    let (mut bold, mut italic) = (false, false);
    for (i, &(ch, b, it, _)) in body.iter().enumerate() {
        if !ch.is_whitespace() {
            // Spaces keep the style around them, so marks hug the words.
            if (b, it) != (bold, italic) {
                let closing = out.len();
                if italic {
                    out.push('*');
                }
                if bold && !b {
                    out.push_str("**");
                }
                // A mark that closes and one that opens are kept apart by an invisible space, so
                // "**bold***italic*" cannot be read the other way round.
                if out.len() > closing && (b && !bold || it) {
                    out.push('\u{200B}');
                }
                if b && !bold {
                    out.push_str("**");
                }
                if it {
                    out.push('*');
                }
                (bold, italic) = (b, it);
            }
        } else if bold || italic {
            // A style ends before the space when the next word is not in it.
            match body[i..].iter().find(|c| !c.0.is_whitespace()) {
                None => break,
                Some(next) if (next.1, next.2) != (bold, italic) => {
                    if italic {
                        out.push('*');
                    }
                    if bold {
                        out.push_str("**");
                    }
                    (bold, italic) = (false, false);
                }
                Some(_) => {}
            }
        }
        out.push(ch);
    }
    let trailing = out.len() - out.trim_end().len();
    out.truncate(out.len() - trailing);
    if italic {
        out.push('*');
    }
    if bold {
        out.push_str("**");
    }
    (out, wrapped)
}

