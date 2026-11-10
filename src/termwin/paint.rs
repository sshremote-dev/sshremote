//! The terminal view: the cells, the cursor, selection and links, and what the mouse does there.

use super::*;

pub(super) fn color(c: Color, colors: &alacritty_terminal::term::color::Colors) -> Color32 {
    let rgb = |r: Rgb| Color32::from_rgb(r.r, r.g, r.b);
    match c {
        Color::Spec(r) => rgb(r),
        Color::Indexed(i) => colors[i as usize].map(rgb).unwrap_or_else(|| indexed(i)),
        Color::Named(n) => {
            if let Some(r) = colors[n] {
                return rgb(r);
            }
            match n {
                NamedColor::Foreground | NamedColor::Cursor => scheme_fg(),
                NamedColor::Background => scheme_bg(),
                NamedColor::BrightForeground => Color32::WHITE,
                NamedColor::DimForeground => scheme_fg().gamma_multiply(0.66),
                n if (n as usize) < 16 => pal(n as usize),
                // DimBlack.. DimWhite follow the eight base colours.
                n => pal((n as usize).saturating_sub(NamedColor::DimBlack as usize) % 8).gamma_multiply(0.66),
            }
        }
    }
}

/// The xterm 256-colour palette.
pub(super) fn indexed(i: u8) -> Color32 {
    match i {
        0..16 => pal(i as usize),
        16..232 => {
            let v = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            let n = i - 16;
            Color32::from_rgb(v(n / 36), v(n / 6 % 6), v(n % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

/// Paints the terminal's rows from `first` on into `area`: backgrounds, then text in runs of the
/// same colour, the cursor (hollow without the focus) and an input method's composition.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint(ui: &egui::Ui, t: &Tab, area: Rect, font: &FontId, cw: f32, ch: f32, first: i32, focused: bool) {
    let origin = area.min;
    let painter = ui.painter_at(area);
    let content = t.term.renderable_content();
    let colors = content.colors;
    let sel = content.selection;
    let cursor = content.cursor.point;
    let offset = content.display_offset as i32;
    let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR) && offset == 0 && t.exited.is_none();
    let mut run = String::new();
    let mut run_start = Pos2::ZERO;
    let mut run_color = scheme_fg();
    let flush = |run: &mut String, at: Pos2, color: Color32| {
        if !run.trim().is_empty() {
            painter.text(at, Align2::LEFT_TOP, run.as_str(), font.clone(), color);
        }
        run.clear();
    };
    let mut last_row = i32::MIN;
    let mut caret_cell: Option<Rect> = None;
    // Backgrounds go under all the text, a rectangle per run of one colour along a row.
    let bg_slot = painter.add(egui::Shape::Noop);
    let mut bgs: Vec<egui::Shape> = Vec::new();
    let mut bg_run: Option<(Rect, Color32)> = None;
    let end_bg = |bg_run: &mut Option<(Rect, Color32)>, bgs: &mut Vec<egui::Shape>| {
        if let Some((r, c)) = bg_run.take() {
            bgs.push(egui::Shape::rect_filled(r, CornerRadius::ZERO, c));
        }
    };
    let shape = CURSOR.load(std::sync::atomic::Ordering::Relaxed);
    let block = shape == 1;
    // Blinking as in VS Code: on and off every half second, and steady for a moment after typing.
    let now = ui.input(|i| i.time);
    let since = now - t.typed_at;
    let lit = !focused || since < 0.6 || ((since - 0.6) / 0.53) as u64 % 2 == 1;
    if focused && show_cursor {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(0.53 - (since - 0.6).rem_euclid(0.53)));
    }
    for cell in content.display_iter {
        let row = cell.point.line.0 + offset;
        if row < first {
            continue;
        }
        let col = cell.point.column.0;
        if row != last_row {
            flush(&mut run, run_start, run_color);
            last_row = row;
        }
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let pos = Pos2::new(origin.x + col as f32 * cw, origin.y + (row - first) as f32 * ch);
        let wide = flags.contains(Flags::WIDE_CHAR);
        let width = if wide { 2.0 * cw } else { cw };
        let mut fg = color(cell.fg, colors);
        let mut bg = color(cell.bg, colors);
        if flags.contains(Flags::BOLD)
            && let Color::Named(n) = cell.fg
            && (n as usize) < 8
        {
            fg = pal(n as usize + 8);
        }
        if flags.contains(Flags::DIM) {
            fg = fg.gamma_multiply(0.66);
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if sel.is_some_and(|s| s.contains(cell.point)) {
            bg = selection();
        }
        let at_cursor = show_cursor && cell.point == cursor;
        let cell_rect = Rect::from_min_size(pos, Vec2::new(width, ch));
        if at_cursor {
            caret_cell = Some(cell_rect);
            if focused && block && lit && t.preedit.is_empty() {
                (fg, bg) = (scheme_bg(), scheme_fg());
            }
        }
        if bg == scheme_bg() {
            end_bg(&mut bg_run, &mut bgs);
        } else {
            match &mut bg_run {
                Some((r, c)) if *c == bg && r.top() == cell_rect.top() && (r.right() - cell_rect.left()).abs() < 0.5 => {
                    r.max.x = cell_rect.right();
                }
                _ => {
                    end_bg(&mut bg_run, &mut bgs);
                    bg_run = Some((cell_rect, bg));
                }
            }
        }
        if flags.contains(Flags::UNDERLINE) {
            painter.hline(cell_rect.x_range(), cell_rect.bottom() - 1.5, egui::Stroke::new(1.0, fg));
        }
        let c = if flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
        // ASCII shares the grid exactly; anything else is placed on its own cell.
        if c.is_ascii() && !wide && fg == run_color && !run.is_empty() {
            run.push(c);
            continue;
        }
        flush(&mut run, run_start, run_color);
        if c.is_ascii() && !wide {
            run_start = pos;
            run_color = fg;
            run.push(c);
        } else if c != ' ' {
            painter.text(cell_rect.center(), Align2::CENTER_CENTER, c, font.clone(), fg);
        }
    }
    flush(&mut run, run_start, run_color);
    end_bg(&mut bg_run, &mut bgs);
    painter.set(bg_slot, egui::Shape::Vec(bgs));
    let mut caret = caret_cell;
    if !t.preedit.is_empty() && show_cursor {
        // The composition in progress, underlined, with the caret after it as in a text field.
        let at =
            Pos2::new(origin.x + cursor.column.0 as f32 * cw, origin.y + (cursor.line.0 + offset - first) as f32 * ch);
        let galley = painter.layout_no_wrap(t.preedit.clone(), font.clone(), scheme_fg());
        let r = Rect::from_min_size(at, Vec2::new(galley.size().x.max(cw), ch));
        painter.rect_filled(r, CornerRadius::ZERO, scheme_bg());
        painter.galley(at, galley, scheme_fg());
        painter.hline(r.x_range(), r.bottom() - 1.0, egui::Stroke::new(1.5, ACCENT));
        caret = Some(Rect::from_min_size(Pos2::new(r.right(), r.top()), Vec2::new(cw, ch)));
    }
    let Some(r) = caret else { return };
    let color = if focused { scheme_fg() } else { scheme_fg().gamma_multiply(0.45) };
    if !focused && block {
        // A pane without the focus shows a hollow block.
        let stroke = egui::Stroke::new(1.0, color);
        painter.rect_stroke(r.shrink(0.5), CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
    } else if lit && !(block && t.preedit.is_empty()) {
        let bar = match shape {
            2 => Rect::from_min_max(Pos2::new(r.left(), r.bottom() - 2.0), r.max),
            1 => r,
            _ => Rect::from_min_size(r.min, Vec2::new(2.0, r.height())),
        };
        painter.rect_filled(bar, CornerRadius::ZERO, color);
    }
}

/// The terminal sized to what a view shows, `cols` by `rows` cells: the program is told, and the
/// reading views made again, as the rows wrap anew at the new width.
pub(super) fn fit(t: &mut Tab, cols: f32, rows: f32, out: &mut Vec<Cmd>) {
    let (cols, rows) = ((cols.floor() as u16).max(2), (rows.floor() as u16).max(1));
    if (cols, rows) != t.size {
        t.size = (cols, rows);
        t.term.resize(Size { cols: cols as usize, rows: rows as usize });
        t.rev += 1;
        if t.exited.is_none() {
            out.push(Cmd::Send(ToAgent::TermResize { id: t.id, cols, rows }));
        }
    }
}

pub(super) fn screen(
    ui: &mut egui::Ui,
    t: &mut Tab,
    font_size: f32,
    copy_on_select: bool,
    focused: bool,
    keys: bool,
    out: &mut Vec<Cmd>,
) -> PaneAsk {
    let mut ask = PaneAsk::default();
    let font = FontId::monospace(font_size);
    let (cw, ch) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let area = ui.available_rect_before_wrap();
    fit(t, area.width() / cw, area.height() / ch, out);
    let (cols, rows) = t.size;
    let resp = ui.allocate_rect(area, Sense::click_and_drag());
    ask.focus = resp.clicked() || resp.drag_started() || resp.secondary_clicked() || resp.middle_clicked();
    let origin = area.min;
    let offset = t.term.grid().display_offset() as i32;
    let mode = *t.term.mode();
    // A program that asked for the mouse gets clicks and drags; Shift keeps them for selecting here.
    let app_mouse = mode.intersects(TermMode::MOUSE_MODE) && !ui.input(|i| i.modifiers.shift) && t.exited.is_none();
    if app_mouse && (focused || ask.focus) {
        let mut send = Vec::new();
        let at = |p: Pos2| {
            let col = (((p.x - origin.x) / cw).floor().max(0.0) as usize).min(cols as usize - 1);
            let row = (((p.y - origin.y) / ch).floor().max(0.0) as usize).min(rows as usize - 1);
            (col, row)
        };
        for e in ui.ctx().input(|i| i.events.clone()) {
            match e {
                Event::PointerButton { pos, button, pressed, .. }
                    if area.contains(pos) || (!pressed && t.mouse_held.is_some()) =>
                {
                    let b = match button {
                        egui::PointerButton::Primary => 0,
                        egui::PointerButton::Middle => 1,
                        egui::PointerButton::Secondary => 2,
                        _ => continue,
                    };
                    let (col, row) = at(pos);
                    send.extend(mouse_report(mode, b, col, row, pressed));
                    t.mouse_held = pressed.then_some(b);
                }
                Event::PointerMoved(pos)
                    if area.contains(pos) && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) =>
                {
                    if let Some(b) = t.mouse_held {
                        let (col, row) = at(pos);
                        send.extend(mouse_report(mode, b + 32, col, row, true));
                    } else if mode.contains(TermMode::MOUSE_MOTION) {
                        let (col, row) = at(pos);
                        send.extend(mouse_report(mode, 35, col, row, true));
                    }
                }
                _ => {}
            }
        }
        if !send.is_empty() {
            out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
        }
    } else {
        t.mouse_held = None;
    }
    // Cell under a screen position, as a grid point.
    let cell_at = |p: Pos2| {
        let col = (((p.x - origin.x) / cw).floor().max(0.0) as usize).min(cols as usize - 1);
        let row = (((p.y - origin.y) / ch).floor().max(0.0) as i32).min(rows as i32 - 1);
        let side = if (p.x - origin.x) / cw - col as f32 > 0.5 { Side::Right } else { Side::Left };
        (Point::new(Line(row - offset), Column(col)), side)
    };

    // Mouse: drag selects, double click selects a word, the wheel scrolls back.
    if !app_mouse
        && resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (pt, side) = cell_at(p);
        t.term.selection = Some(Selection::new(SelectionType::Simple, pt, side));
    }
    if !app_mouse
        && resp.dragged_by(egui::PointerButton::Primary)
        && let (Some(p), Some(sel)) = (resp.interact_pointer_pos(), t.term.selection.as_mut())
    {
        let (pt, side) = cell_at(p);
        sel.update(pt, side);
    }
    if resp.drag_stopped()
        && copy_on_select
        && let Some(s) = t.term.selection_to_string().filter(|s| !s.is_empty())
    {
        ui.ctx().copy_text(s);
    }
    let ctrl = ui.input(|i| i.modifiers.ctrl);
    if resp.hovered()
        && ctrl
        && let Some(p) = resp.hover_pos()
    {
        let (pt, _) = cell_at(p);
        let line =
            t.term.bounds_to_string(Point::new(pt.line, Column(0)), Point::new(pt.line, Column(cols as usize - 1)));
        if url_at(&line, pt.column.0).is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    if !app_mouse
        && resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (pt, side) = cell_at(p);
        t.term.selection = Some(Selection::new(SelectionType::Semantic, pt, side));
    } else if resp.clicked() {
        t.term.selection = None;
        if !ctrl
            && !app_mouse
            && focused
            && let Some(p) = resp.interact_pointer_pos()
        {
            let (pt, side) = cell_at(p);
            let col = pt.column.0 + usize::from(side == Side::Right);
            input::click_cursor(t, pt.line.0, col.min(cols as usize - 1), out);
        }
        if ctrl && let Some(p) = resp.interact_pointer_pos() {
            let (pt, _) = cell_at(p);
            let line =
                t.term.bounds_to_string(Point::new(pt.line, Column(0)), Point::new(pt.line, Column(cols as usize - 1)));
            if let Some(url) = url_at(&line, pt.column.0) {
                open_url(&url);
            }
        }
    }
    // Find (Ctrl+Shift+F): a bar at the top right, Enter for the next match upwards.
    if focused && ui.input(|i| i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(Key::F)) {
        t.search = Some(t.search.take().unwrap_or_default());
    }
    let mut close_search = false;
    let mut search_step: Option<alacritty_terminal::index::Direction> = None;
    if let Some(search) = t.search.as_mut().filter(|_| focused) {
        let bar = Rect::from_min_size(Pos2::new(area.right() - 330.0, area.top() + 4.0), Vec2::new(322.0, 34.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layer_id(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(("find", t.id)))),
        );
        egui::Frame::new()
            .fill(Color32::from_rgb(36, 38, 46))
            .corner_radius(CornerRadius::same(crate::gui::theme::R_MD))
            .inner_margin(4.0)
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    let edit =
                        ui.add(egui::TextEdit::singleline(&mut search.query).hint_text("Find").desired_width(170.0));
                    if !edit.has_focus() && search.found.is_none() && !search.missing {
                        edit.request_focus();
                    }
                    if edit.changed() {
                        search.found = None;
                        search.missing = false;
                    }
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    let shift = ui.input(|i| i.modifiers.shift);
                    if enter {
                        search_step = Some(if shift {
                            alacritty_terminal::index::Direction::Right
                        } else {
                            alacritty_terminal::index::Direction::Left
                        });
                    }
                    if ui.small_button(ic::CARET_UP).on_hover_text("Previous (Enter)").clicked() {
                        search_step = Some(alacritty_terminal::index::Direction::Left);
                    }
                    if ui.small_button(ic::CARET_DOWN).on_hover_text("Next (Shift+Enter)").clicked() {
                        search_step = Some(alacritty_terminal::index::Direction::Right);
                    }
                    if search.missing {
                        ui.label(RichText::new("none").small().color(crate::gui::bad()));
                    }
                    if ui.small_button(ic::X).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        close_search = true;
                    }
                });
            });
    }
    if close_search {
        t.search = None;
    }
    if let (Some(dir), Some(search)) = (search_step, t.search.as_mut())
        && !search.query.is_empty()
    {
        let escaped: String = search
            .query
            .chars()
            .map(|c| if "\\.+*?()|[]{}^$#&-~".contains(c) { format!("\\{c}") } else { c.to_string() })
            .collect();
        if let Ok(mut re) = alacritty_terminal::term::search::RegexSearch::new(&escaped) {
            let origin = match (&search.found, dir) {
                (Some(m), alacritty_terminal::index::Direction::Left) => *m.start(),
                (Some(m), _) => *m.end(),
                (None, _) => Point::new(Line(t.term.grid().screen_lines() as i32 - 1), Column(cols as usize - 1)),
            };
            let origin = if search.found.is_some() {
                match dir {
                    alacritty_terminal::index::Direction::Left => {
                        origin.sub(&t.term, alacritty_terminal::index::Boundary::Grid, 1)
                    }
                    _ => origin.add(&t.term, alacritty_terminal::index::Boundary::Grid, 1),
                }
            } else {
                origin
            };
            match t.term.search_next(&mut re, origin, dir, Side::Left, None) {
                Some(m) => {
                    let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                    sel.update(*m.end(), Side::Right);
                    t.term.selection = Some(sel);
                    t.term.scroll_to_point(*m.start());
                    search.found = Some(m);
                    search.missing = false;
                }
                None => search.missing = true,
            }
        }
    }
    let mut copy_now = false;
    let mut paste_now = false;
    let mut select_all = false;
    let mut clear = false;
    let mut find = false;
    let mut view = None;
    let mut script: Option<crate::script::Command> = None;
    resp.context_menu(|ui| {
        crate::gui::menu_look(ui);
        if ui.add(egui::Button::new("Copy").shortcut_text("Ctrl+Shift+C, Ctrl+Insert")).clicked() {
            copy_now = true;
            ui.close();
        }
        if ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+Shift+V, Shift+Insert")).clicked() {
            paste_now = true;
            ui.close();
        }
        if ui.button("Select all").clicked() {
            select_all = true;
            ui.close();
        }
        ui.separator();
        if ui.add(egui::Button::new("Find").shortcut_text("Ctrl+Shift+F")).clicked() {
            find = true;
            ui.close();
        }
        if ui.button("Read as text").clicked() {
            view = Some(View::Text);
            ui.close();
        }
        if ui.button("Read as Markdown").clicked() {
            view = Some(View::Markdown);
            ui.close();
        }
        ui.separator();
        if ui.add(egui::Button::new("Split right").shortcut_text("Alt+Shift+=")).clicked() {
            ask.split = Some(true);
            ui.close();
        }
        if ui.add(egui::Button::new("Split down").shortcut_text("Alt+Shift+-")).clicked() {
            ask.split = Some(false);
            ui.close();
        }
        if ui.add(egui::Button::new("Close pane").shortcut_text("Ctrl+Shift+W")).clicked() {
            ask.close = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Clear scrollback").clicked() {
            clear = true;
            ui.close();
        }
        // Commands of installed script extensions.
        let commands = crate::script::commands();
        if !commands.is_empty() {
            ui.separator();
            ui.menu_button("Extensions", |ui| {
                for c in commands {
                    if ui.button(&c.label).clicked() {
                        script = Some(c);
                        ui.close();
                    }
                }
            });
        }
    });
    if let Some(c) = script {
        let selection = t.term.selection_to_string().unwrap_or_default();
        match crate::script::run_command(&c, &selection, "") {
            // Typed as a paste, so nothing runs before it is looked at.
            Ok((text, true)) if !text.is_empty() => ui.ctx().input_mut(|i| i.events.push(egui::Event::Paste(text))),
            Ok((text, _)) => ui.ctx().copy_text(text),
            Err(e) => ui.ctx().copy_text(format!("sshremote: {e}")),
        }
    }
    if select_all {
        let grid = t.term.grid();
        let history = (grid.total_lines() - grid.screen_lines()) as i32;
        let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(-history), Column(0)), Side::Left);
        sel.update(Point::new(Line(grid.screen_lines() as i32 - 1), Column(grid.columns() - 1)), Side::Right);
        t.term.selection = Some(sel);
    }
    if find {
        t.search = Some(Search::default());
    }
    if let Some(v) = view {
        t.view = v;
    }
    if clear {
        // Erase saved lines, as Windows Terminal's "Clear buffer" does for the scrollback.
        t.parser.advance(&mut t.term, b"\x1b[3J");
    }
    if copy_now && let Some(s) = t.term.selection_to_string() {
        ui.ctx().copy_text(s);
    }
    if paste_now {
        // The paste arrives as an Event::Paste on the next frame.
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }
    let mode = *t.term.mode();
    let keys = keys && t.search.is_none();
    if keys {
        let c = t.term.grid().cursor.point;
        let at = Pos2::new(origin.x + c.column.0 as f32 * cw, origin.y + (c.line.0 + offset) as f32 * ch);
        let grab = ask.focus || ui.memory(|m| m.focused().is_none_or(|f| f == resp.id));
        keyboard(ui, t, resp.id, area, Rect::from_min_size(at, Vec2::new(cw, ch)), grab, out);
    } else {
        t.preedit.clear();
    }
    // The wheel scrolls the pane under the pointer back through its scrollback; a full-screen
    // program or one that asked for the mouse scrolls itself.
    if resp.hovered() {
        let lines = wheel_lines(ui, ch, rows as f32);
        if lines != 0 && program_scrolls(t) && t.exited.is_none() {
            let (col, row) = resp.hover_pos().map_or((0, 0), |p| {
                let (pt, _) = cell_at(p);
                (pt.column.0, (pt.line.0 + offset).max(0) as usize)
            });
            out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: wheel_bytes(mode, lines, col, row) }));
        } else if lines != 0 {
            t.term.scroll_display(Scroll::Delta(lines));
        }
    }
    paint(ui, t, area, &font, cw, ch, 0, focused);
    let painter = ui.painter_at(area);
    if let Some(text) = &t.exited {
        let r = Rect::from_min_size(Pos2::new(area.left(), area.bottom() - 28.0), Vec2::new(area.width(), 28.0));
        painter.rect_filled(r, CornerRadius::same(crate::gui::theme::R_SM), Color32::from_rgba_unmultiplied(40, 42, 50, 235));
        painter.text(
            r.left_center() + Vec2::new(10.0, 0.0),
            Align2::LEFT_CENTER,
            format!("{text}. Close the tab or open a new one."),
            FontId::proportional(13.0),
            scheme_fg(),
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    ask
}

/// A web address under column `col` of a line, for Ctrl+click.
pub(super) fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() {
        return None;
    }
    let stop = |c: char| c.is_whitespace() || "\"'<>()[]{}`".contains(c);
    let mut a = col;
    while a > 0 && !stop(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b < chars.len() && !stop(chars[b]) {
        b += 1;
    }
    let word: String = chars[a..b].iter().collect();
    let word = word.trim_end_matches(['.', ',', ';', ':']);
    (word.starts_with("http://") || word.starts_with("https://")).then(|| word.to_string())
}

pub(crate) fn open_url(url: &str) {
    if cfg!(windows) {
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    } else {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}
