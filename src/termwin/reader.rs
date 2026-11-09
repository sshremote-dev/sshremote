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
        // As wide as the text beside the line numbers (up to six digits and their margin).
        fit(t, (area.width() - 8.0 * cw - 12.0) / cw, area.height() / ch, out);
        ui.painter().rect_filled(area, CornerRadius::ZERO, scheme_bg());
        return text_editor(ui, t, &font, cw, ch, keys, out);
    }
    markdown_view(ui, t, font_size, keys, out);
}

/// What the Markdown view finds: the query, the blocks that hold it, and the one shown.
#[derive(Default)]
pub(super) struct Find {
    query: String,
    hits: Vec<usize>,
    at: usize,
    /// For the query and document the hits are for.
    for_key: (String, u64),
    focus: bool,
    /// The current hit changed: the view goes to it.
    go: bool,
}

/// The Markdown view: a record to read, search and copy, not typed into. A full-screen program's
/// record is gathered from what settles on its screen (see `collect`), so an agent's answers, a
/// file paged through, stay together as one document while its live status is left out; a shell's
/// record is its scrollback and screen. The wheel scrolls the record; at either end it goes on to
/// a program that scrolls itself, and what that brings into view is added.
fn markdown_view(ui: &mut egui::Ui, t: &mut Tab, font_size: f32, keys: bool, out: &mut Vec<Cmd>) {
    let font = FontId::monospace(font_size);
    let ch = ui.ctx().fonts_mut(|f| f.row_height(&font));
    let area = ui.available_rect_before_wrap();
    let now = ui.input(|i| i.time);
    let alt = t.term.mode().contains(TermMode::ALT_SCREEN);
    if t.md_copy {
        // Copy from the menu: egui copies the selection when it sees the copy event.
        t.md_copy = false;
        ui.ctx().input_mut(|i| i.events.push(Event::Copy));
    }
    // A shell's record is built again once its output settles, or now and then while it streams.
    if t.seen_rev != t.rev {
        t.seen_rev = t.rev;
        t.changed_at = now;
    }
    let key = if alt { (true, t.collect.rev, 0) } else { (false, t.rev, t.md_skip) };
    let settled = now - t.changed_at >= SETTLE || now - t.built_at >= SETTLE_AT_MOST;
    let due = alt || t.reading.is_none() || (settled && now - t.built_at >= REBUILD_EVERY);
    if !due {
        let wait = (SETTLE - (now - t.changed_at)).min(SETTLE_AT_MOST - (now - t.built_at)).max(REBUILD_EVERY);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait));
    }
    let rebuilt = due && t.reading.as_ref().is_none_or(|r| r.0 != key);
    if rebuilt {
        t.built_at = now;
        let text = join_rows(record_md(t));
        let doc = crate::markdown::Doc::new(&text);
        t.reading = Some((key, std::rc::Rc::new((text, doc))));
    }
    let read = t.reading.as_ref().expect("just built").1.clone();
    let doc = &read.1;
    let th = crate::markdown::Theme::terminal(scheme_bg(), scheme_fg());
    ui.painter().rect_filled(area, CornerRadius::ZERO, th.bg);
    ui.visuals_mut().override_text_color = Some(th.fg);
    let doc_id = egui::Id::unique(("md-doc", t.id));
    let doc_key = std::rc::Rc::as_ptr(&read) as usize as u64;
    let width = area.width().min(960.0);
    let pad = ((area.width() - width) / 2.0).max(0.0) + 20.0;
    let column = width - 40.0;
    ui.spacing_mut().item_spacing.y = 6.0;

    // Ctrl+F searches; Escape closes the search.
    if keys && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F)) {
        let f = t.find.get_or_insert_with(Find::default);
        f.focus = true;
    }
    let bar_h = if t.find.is_some() { 36.0 } else { 0.0 };
    let view_rect = Rect::from_min_max(area.min + Vec2::new(0.0, bar_h), area.max);

    let mut heights = std::mem::take(&mut t.md_heights);
    let end = crate::markdown::lay_out(ui, doc, doc_key, doc_id, &th, &mut heights, pad, column, 14.0);
    let content = end + 24.0;
    let mut scroll = egui::ScrollArea::both().id_salt(("reading", t.id)).auto_shrink([false, false]);
    if t.md_laid != doc_key {
        if t.md_bottom {
            // At the bottom it stays there, with this document's height, not the last one's.
            scroll = scroll.vertical_scroll_offset((content - view_rect.height()).max(0.0));
        } else if t.collect.prepended > 0 {
            // Rows added above: what is in view stays where it is.
            scroll = scroll.vertical_scroll_offset(t.md_offset + (end - t.md_end).max(0.0));
        }
        t.collect.prepended = 0;
    }
    // Search: the blocks that hold the query, and the view goes to the current one.
    if let Some(f) = &mut t.find {
        let k = (f.query.to_lowercase(), doc_key);
        if f.for_key != k {
            f.hits = if k.0.is_empty() {
                Vec::new()
            } else {
                doc.blocks.iter().enumerate().filter(|(_, b)| crate::markdown::block_text(b).to_lowercase().contains(&k.0)).map(|(n, _)| n).collect()
            };
            f.go = f.for_key.0 != k.0;
            f.at = f.at.min(f.hits.len().saturating_sub(1));
            f.for_key = k;
        }
        if f.go {
            f.go = false;
            if let Some(&(y, _)) = f.hits.get(f.at).and_then(|&n| heights.tops().get(n)) {
                scroll = scroll.vertical_scroll_offset((y - 40.0).max(0.0));
            }
        }
    }
    let hits: Vec<(usize, bool)> = t.find.as_ref().map(|f| f.hits.iter().enumerate().map(|(i, &n)| (n, i == f.at)).collect()).unwrap_or_default();
    let wheel = if ui.rect_contains_pointer(view_rect) { wheel_lines(ui, ch, 10.0) } else { 0 };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(view_rect));
    let shown = scroll.show_viewport(&mut child, |ui, view| {
        let origin = ui.max_rect().min;
        let hl = egui::Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), 40);
        for &(n, current) in &hits {
            if let Some(&(y, h)) = heights.tops().get(n) {
                let r = Rect::from_min_size(origin + Vec2::new(pad - 6.0, y - 3.0), Vec2::new(column + 12.0, h + 6.0));
                ui.painter().rect_filled(r, CornerRadius::same(4), hl);
                if current {
                    ui.painter().rect_stroke(r, CornerRadius::same(4), egui::Stroke::new(1.5, ACCENT), egui::StrokeKind::Outside);
                }
            }
        }
        crate::markdown::show_in_view(ui, doc, doc_key, doc_id, &th, &mut heights, view, pad, column, 14.0);
        if doc.blocks.is_empty() {
            let msg = if alt {
                "Nothing gathered yet: what the program shows is added here once it settles, and \
                 scrolling it adds more."
            } else {
                "Nothing to show yet."
            };
            ui.put(
                Rect::from_min_size(origin + Vec2::new(pad, 14.0), Vec2::new(column, 40.0)),
                egui::Label::new(RichText::new(msg).color(th.fg.gamma_multiply(0.6))),
            );
        }
        ui.allocate_rect(Rect::from_min_max(origin, Pos2::new(origin.x + ui.available_width(), origin.y + content)), Sense::hover());
    });
    t.md_heights = heights;
    t.md_laid = doc_key;
    t.md_end = end;
    let offset = shown.state.clamped_offset().y;
    let room = (shown.content_size.y - shown.inner_rect.height()).max(0.0);
    let (at_top, at_bottom) = (offset <= 1.0, offset >= room - 2.0);
    t.md_offset = offset;
    t.md_bottom = at_bottom;
    // At either end of the record the wheel goes on to a program that scrolls itself.
    if alt && program_scrolls(t) && t.exited.is_none() && ((wheel > 0 && at_top) || (wheel < 0 && at_bottom)) {
        let grid = t.term.grid();
        let send = wheel_bytes(*t.term.mode(), wheel, grid.columns() / 2, grid.screen_lines() / 2);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
        t.collect.up = wheel > 0;
        t.collect.wheel_at = now;
    }
    find_bar(ui, t, Rect::from_min_size(area.min, Vec2::new(area.width(), bar_h)), keys);
    md_tools(ui, t, view_rect, &read.0);
}

/// The record as Markdown rows: a full-screen program's gathered rows, or a shell's scrollback
/// and screen (after what was cleared by hand).
fn record_md(t: &mut Tab) -> Vec<(String, bool)> {
    if t.term.mode().contains(TermMode::ALT_SCREEN) {
        let rows = t.collect.rows();
        // Markdown source (a file open in an editor) is read as it is, not from its colours.
        let source = rows.iter().any(|k| k.plain.trim_start().starts_with("```"))
            || rows.iter().filter(|k| ["# ", "## ", "### "].iter().any(|h| k.plain.starts_with(h))).count() >= 2;
        return rows.into_iter().map(|k| if source { (k.plain, false) } else { k.md }).collect();
    }
    sync_rows(t);
    let last = last_written(t);
    let rows: Vec<(String, bool)> = t.rows.md.iter().cloned().chain((0..=last).map(|l| row_markdown(t, l))).collect();
    rows.into_iter().skip(t.md_skip).collect()
}

/// The record as plain text, to copy.
fn record_plain(t: &mut Tab) -> String {
    if t.term.mode().contains(TermMode::ALT_SCREEN) {
        return t.collect.rows().into_iter().map(|k| k.plain).collect::<Vec<_>>().join("\n");
    }
    sync_rows(t);
    let last = last_written(t);
    let rows: Vec<String> = t.rows.plain.iter().cloned().chain((0..=last).map(|l| row_plain(t, l))).collect();
    rows.into_iter().skip(t.md_skip).collect::<Vec<_>>().join("\n").trim_end().to_string()
}

/// The screen's last row with text (or the cursor's, below it).
fn last_written(t: &Tab) -> i32 {
    let grid = t.term.grid();
    let cursor = grid.cursor.point.line.0;
    (0..grid.screen_lines() as i32).rev().find(|&r| !row_plain(t, r).is_empty()).unwrap_or(0).max(cursor)
}

/// Clears the Markdown view's record: what is there now is not shown again, what comes is.
fn clear_record(t: &mut Tab) {
    if t.term.mode().contains(TermMode::ALT_SCREEN) {
        t.collect.clear_by_hand();
    } else {
        sync_rows(t);
        t.md_skip = t.rows.plain.len() + last_written(t) as usize + 1;
    }
    t.md_bottom = true;
}

/// The search bar above the record: the query, how many blocks hold it, and the way between them.
fn find_bar(ui: &mut egui::Ui, t: &mut Tab, rect: Rect, keys: bool) {
    let Some(f) = &mut t.find else { return };
    let mut close = false;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(12.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let ui = &mut child;
    ui.painter().rect_filled(rect, CornerRadius::ZERO, ui.visuals().faint_bg_color);
    ui.label(RichText::new(ic::SEARCH).size(15.0));
    let edit = ui.add(egui::TextEdit::singleline(&mut f.query).hint_text("Search the record").desired_width(260.0));
    if f.focus {
        edit.request_focus();
        f.focus = false;
    }
    let (enter, shift, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
    let n = f.hits.len();
    ui.label(if f.query.is_empty() { String::new() } else if n == 0 { "No results".into() } else { format!("{} of {n}", f.at + 1) });
    let prev = ui.add_enabled(n > 1, crate::gui::tool_button(ic::CARET_UP, false)).on_hover_text("Previous (Shift+Enter)").clicked();
    let next = ui.add_enabled(n > 1, crate::gui::tool_button(ic::CARET_DOWN, false)).on_hover_text("Next (Enter)").clicked();
    if ui.add(crate::gui::tool_button(ic::X, false)).on_hover_text("Close (Escape)").clicked() {
        close = true;
    }
    let typed_enter = enter && (edit.lost_focus() || edit.has_focus());
    if n > 0 && (next || (typed_enter && !shift)) {
        f.at = (f.at + 1) % n;
        f.go = true;
    } else if n > 0 && (prev || (typed_enter && shift)) {
        f.at = (f.at + n - 1) % n;
        f.go = true;
    }
    if typed_enter {
        f.focus = true;
    }
    if close || (keys && esc) {
        t.find = None;
    }
}

/// The record's tools: search and clear at its top right, and the same with copying on a right
/// click anywhere over it.
fn md_tools(ui: &mut egui::Ui, t: &mut Tab, rect: Rect, markdown: &str) {
    let mut act = None;
    let tools = Rect::from_min_size(Pos2::new(rect.right() - 86.0, rect.top() + 8.0), Vec2::new(70.0, 30.0));
    // On a backing of the view's colour, so text scrolled under them does not show through.
    let back = scheme_bg().gamma_multiply(0.92);
    ui.painter().rect_filled(tools.expand(3.0), CornerRadius::same(8), back);
    let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(tools).layout(egui::Layout::right_to_left(egui::Align::Center)));
    if bar.add(crate::gui::tool_button(ic::ERASER, false)).on_hover_text("Clear: start the record again from here").clicked() {
        act = Some(MdAct::Clear);
    }
    if bar.add(crate::gui::tool_button(ic::SEARCH, t.find.is_some())).on_hover_text("Search (Ctrl+F)").clicked() {
        act = Some(MdAct::FindToggle);
    }
    let menu_id = egui::Id::unique(("md-menu", t.id));
    let (secondary, pos) = ui.input(|i| (i.pointer.secondary_clicked(), i.pointer.interact_pos()));
    if secondary && let Some(p) = pos.filter(|p| rect.contains(*p)) {
        ui.ctx().data_mut(|d| d.insert_temp(menu_id, p));
    }
    if let Some(p) = ui.ctx().data(|d| d.get_temp::<Pos2>(menu_id)) {
        let selected = ui.ctx().plugin_opt::<egui::text_selection::LabelSelectionState>().is_some_and(|s| s.lock().has_selection());
        let shown = egui::Area::new(menu_id).fixed_pos(p).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                crate::gui::menu(ui, |ui| {
                    for (a, text, on) in [
                        (MdAct::Copy, format!("{}  Copy (Ctrl+C)", ic::COPY), selected),
                        (MdAct::CopyMd, format!("{}  Copy all as Markdown", ic::ARTICLE), !markdown.is_empty()),
                        (MdAct::CopyText, format!("{}  Copy all as text", ic::TEXT_T), !markdown.is_empty()),
                        (MdAct::Find, format!("{}  Search  (Ctrl+F)", ic::SEARCH), true),
                        (MdAct::Clear, format!("{}  Clear", ic::ERASER), true),
                    ] {
                        if a == MdAct::Clear {
                            ui.separator();
                        }
                        if ui.add_enabled(on, egui::Button::new(text)).clicked() {
                            act = Some(a);
                        }
                    }
                });
            });
        });
        let elsewhere = ui.input(|i| i.pointer.any_pressed()) && !secondary && !ui.ctx().pointer_hover_pos().is_some_and(|q| shown.response.rect.contains(q));
        if act.is_some() || elsewhere || ui.input(|i| i.key_pressed(Key::Escape)) {
            ui.ctx().data_mut(|d| d.remove_temp::<Pos2>(menu_id));
        }
    }
    match act {
        Some(MdAct::Copy) => t.md_copy = true,
        Some(MdAct::CopyMd) => ui.ctx().copy_text(markdown.to_string()),
        Some(MdAct::CopyText) => ui.ctx().copy_text(record_plain(t)),
        Some(MdAct::Find) => t.find.get_or_insert_with(Find::default).focus = true,
        Some(MdAct::FindToggle) if t.find.is_some() => t.find = None,
        Some(MdAct::FindToggle) => t.find = Some(Find { focus: true, ..Default::default() }),
        Some(MdAct::Clear) => clear_record(t),
        None => {}
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MdAct {
    Copy,
    CopyMd,
    CopyText,
    Find,
    FindToggle,
    Clear,
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
pub(super) fn row_plain(t: &Tab, line: i32) -> String {
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
            t.md_skip = t.md_skip.saturating_sub(d);
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
    let mut clicked_at = None;
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
            if edit.response.clicked()
                && !edit.response.double_clicked()
                && let Some(range) = edit.cursor_range
                && range.is_empty()
            {
                clicked_at = Some(range.primary.index.0);
            }
            edit.response.context_menu(|ui| {
                crate::gui::menu_look(ui);
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
    if keys
        && let Some(index) = clicked_at
    {
        // The document is the scrollback's rows, one line each, then the screen's.
        let before = text.chars().take(index).filter(|&c| c == '\n').count();
        let line = before as i32 - t.rows.plain.len() as i32;
        let in_line = text.chars().take(index).collect::<String>().rsplit('\n').next().map_or(0, |l| l.chars().count());
        if line >= 0 {
            let grid = t.term.grid();
            let row = &grid[Line(line.min(grid.screen_lines() as i32 - 1))];
            let mut seen = 0;
            let mut col = grid.columns();
            for c in 0..grid.columns() {
                if row[Column(c)].flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                if seen == in_line {
                    col = c;
                    break;
                }
                seen += 1;
            }
            let col = if col == grid.columns() { in_line.min(grid.columns() - 1) } else { col };
            super::input::click_cursor(t, line, col, out);
        }
    }
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

/// Whether the screen's last row is a status line such as tmux draws: one background colour, not
/// the terminal's own, across the whole width.
pub(super) fn status_row(t: &Tab) -> bool {
    let grid = t.term.grid();
    let row = &grid[Line(grid.screen_lines() as i32 - 1)];
    let first = row[Column(0)].bg;
    let default = matches!(first, Color::Named(alacritty_terminal::vte::ansi::NamedColor::Background));
    !default && (0..grid.columns()).all(|c| row[Column(c)].bg == first)
}

/// Markers an agent puts before a line, kept as they are ahead of the line's own formatting.
pub(super) const LINE_MARKERS: [&str; 6] = ["● ", "⏺ ", "❯ ", "› ", "• ", "▎ "];

/// The rows `from` to `to` (exclusive) as Markdown, with what the terminal showed as styles written
/// back as Markdown: a whole bold line becomes a heading (level 1 when also underlined or italic,
/// as Claude Code draws `#`), bold and italic runs become `**` and `*`. Agents draw Markdown with
/// these attributes and drop the marks, so this gives the Markdown view its structure back.
#[cfg(test)]
pub(super) fn screen_markdown(t: &Tab, from: i32, to: i32) -> String {
    join_rows((from..to).map(|line| row_markdown(t, line)))
}

/// Rows put together into lines: a row the terminal wrapped goes on in the next one, as one line.
pub(super) fn join_rows(rows: impl IntoIterator<Item = (String, bool)>) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut join = false;
    for (text, wraps) in rows {
        match lines.last_mut() {
            Some(last) if join => {
                let more = match text.split_once(SPLIT) {
                    Some((_, md)) => md,
                    None => text.trim_start_matches(CODE),
                };
                // A program that fills its rows and indents the next (a framed prompt) leaves a
                // run of spaces at the break: one is the gap between the words.
                if last.ends_with("  ") && more.starts_with(' ') {
                    last.truncate(last.trim_end().len());
                    last.push(' ');
                    last.push_str(more.trim_start());
                } else {
                    last.push_str(more);
                }
            }
            _ => lines.push(text),
        }
        join = wraps;
    }
    fence_code(lines).join("\n").trim_end().to_string()
}

/// Hints full-screen programs draw over their own text (Claude Code's, when scrolled back).
pub(super) const FLOATING_HINTS: [&str; 1] = ["Jump to bottom (ctrl+End) \u{2193}"];

/// Whether a line reads as code by itself: it ends or starts as code does, not as a sentence.
fn codelike(raw: &str) -> bool {
    let t = raw.trim();
    !t.is_empty()
        && (t.ends_with([';', '{', '}', ')', ']', ',', '(', '[']) || t.starts_with(['#', '}', ')', ']', '.']) || t.starts_with("//"))
}

/// Mark a row read as code (see `row_markdown`), and one with a single colour that may be code
/// beside it; `join_rows` turns runs of them into fences and drops the marks.
const CODE: char = '\u{E000}';
const TINT: char = '\u{E001}';
const SPLIT: char = '\u{E002}';

/// Runs of code rows become fenced blocks with their indentation kept: blank rows inside a run
/// and lines of only closing brackets after one belong to it.
fn fence_code(mut lines: Vec<String>) -> Vec<String> {
    // Rows next to code join it: one with a single colour ("else:", "}" in a keyword colour, a
    // comment), or a plain one that is not a sentence ("Empty,", "Ok(x)", "id"). Across up to
    // two blank lines (between functions) when the row reads as code.
    let mut grew = true;
    while grew {
        grew = false;
        for i in 0..lines.len() {
            let l = &lines[i];
            if l.starts_with(CODE) || l.trim().is_empty() {
                continue;
            }
            let tint = l.starts_with(TINT);
            let (raw, md) = match l.strip_prefix(TINT) {
                Some(rest) => rest.split_once(SPLIT).unwrap_or((rest, rest)),
                None => (l.as_str(), l.as_str()),
            };
            let (r, m) = (raw.trim(), md.trim_start());
            let comment = r.starts_with('#') || r.starts_with("//") || r.starts_with("--");
            // A heading the program drew in bold, a list item, a status line, an agent's marker,
            // a table: never code.
            let numbered = m.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            let prose = (m.starts_with('#') && (!tint || !r.starts_with('#')))
                || m.starts_with("- ")
                || m.starts_with("* ")
                || m.starts_with('|')
                || numbered
                || m.contains(['\u{B7}', '\u{2026}'])
                || r.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c))
                || LINE_MARKERS.iter().any(|mk| m.starts_with(mk));
            if prose {
                continue;
            }
            // Prose: a sentence, bold words, or a row of mostly plain words (a wrapped paragraph).
            let words: Vec<&str> = r.split_whitespace().collect();
            let plain = words
                .iter()
                .filter(|w| w.trim_end_matches([',', '.', ':', ';', '!', '?']).chars().all(|c| c.is_alphabetic() || c == '\''))
                .count();
            let sentence = !comment
                && ((r.ends_with(['.', '!', '?', ':']) && words.len() >= 4 && r.chars().next().is_some_and(char::is_uppercase))
                    || m.contains("**")
                    || (words.len() >= 8 && plain * 10 >= words.len() * 7));
            let code_at = |j: Option<usize>| j.and_then(|j| lines.get(j)).is_some_and(|l| l.starts_with(CODE));
            let next = code_at(i.checked_sub(1)) || code_at(Some(i + 1));
            let reach = |up: bool| {
                let mut j = i;
                for _ in 0..3 {
                    j = match if up { j.checked_sub(1) } else { Some(j + 1) } {
                        Some(j) if j < lines.len() => j,
                        _ => return false,
                    };
                    if lines[j].starts_with(CODE) {
                        return true;
                    }
                    if !lines[j].trim().is_empty() {
                        return false;
                    }
                }
                false
            };
            let apart = !sentence && (reach(true) || reach(false)) && (tint || codelike(raw) || comment);
            // Alone (the rest of its block off the screen) when it can only be code ("use x;").
            let alone = tint && r.ends_with([';', '{']) || r.starts_with("#[");
            if (next && (tint || !sentence)) || apart || alone {
                let code = format!("{CODE}{raw}");
                lines[i] = code;
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
            // A status line ("Thinking… (3s · 9 tokens)") stays a line of its own.
            let l = lines[i].trim_start();
            let next = lines.get(i + 1).is_some_and(|n| !n.trim().is_empty() && !is_code(n));
            if next && !l.starts_with('|') && l.contains(['\u{B7}', '\u{2026}']) {
                out.push(String::new());
            }
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
    let mut at: Vec<usize> = Vec::new();
    for c in 0..cols {
        let cell = &row[Column(c)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let ch = if cell.c == '\0' { ' ' } else { cell.c };
        let f = cell.flags;
        cells.push((ch, f.contains(Flags::BOLD), f.intersects(Flags::ITALIC), f.contains(Flags::UNDERLINE)));
        at.push(c);
    }
    // Hints a program floats over its text are not part of it.
    let mut hidden = vec![false; cols];
    let chars: Vec<char> = cells.iter().map(|c| c.0).collect();
    for hint in FLOATING_HINTS {
        let h: Vec<char> = hint.chars().collect();
        if let Some(i) = chars.windows(h.len()).position(|w| w == h.as_slice()) {
            for k in i..i + h.len() {
                cells[k].0 = ' ';
                hidden[at[k]] = true;
            }
        }
    }
    // An agent's marker has a colour of its own: it says nothing about code.
    let lead = chars.iter().take_while(|c| **c == ' ').count();
    let rest: String = chars[lead..].iter().collect();
    if let Some(m) = LINE_MARKERS.iter().find(|m| rest.starts_with(**m)) {
        for k in lead..(lead + m.chars().count()).min(at.len()) {
            hidden[at[k]] = true;
        }
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
    // Status lines ("Thinking… (10s · ↓ 561 tokens)") are coloured but not code.
    let status = plain.chars().any(|c| matches!(c, '\u{B7}' | '\u{2026}' | '\u{2191}' | '\u{2193}'));
    // Pictures drawn with block characters (a banner) are not code either.
    let art = plain.chars().any(|c| ('\u{2580}'..='\u{259f}').contains(&c));
    if !art && !status && !plain.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)) {
        for c in 0..cols {
            let cell = &row[Column(c)];
            let plain_fg = matches!(cell.fg, Color::Named(NamedColor::Foreground | NamedColor::DimForeground));
            if !hidden[c] && !matches!(cell.c, ' ' | '\0') && !plain_fg && !colours.contains(&cell.fg) {
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
    let spaces = out.split_off(out.len() - trailing);
    if italic {
        out.push('*');
    }
    if bold {
        out.push_str("**");
    }
    // A row that goes on in the next keeps the space it ends with: it is the gap between the word
    // here and the word there, wherever the terminal broke the line.
    if wrapped {
        out.push_str(&spaces);
    }
    (out, wrapped)
}

