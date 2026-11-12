//! The title bar: tabs and +, the active pane's mode and controls, and the layout buttons.

use super::*;

impl Workspace {
    /// The title bar's contents at their natural width: the tabs at their widest (up to four), the
    /// +, the mode, the pane's controls as measured last frame, the layout and full-screen buttons.
    pub(super) fn bar_width(&self) -> f32 {
        let tabs = self.tabs.len().clamp(1, 4) as f32 * TAB_MAX + PLUS_W + 12.0;
        tabs + self.mode_w + 8.0 + self.controls_w + LAYOUT_W + 46.0 + 16.0
    }

    /// The title bar's content: tabs and + on the left; on the right the active pane's mode and
    /// controls, then the layout buttons. What does not fit folds into one menu.
    pub(super) fn bar(&mut self, ui: &mut egui::Ui, action: &mut Option<(u64, remote::Deferred)>) {
        ui.spacing_mut().item_spacing.x = 2.0;
        let avail = ui.available_width();
        // Widths: the mode drop-down and the pane's controls (as measured last frame), the "more"
        // menu; the layout buttons' is `LAYOUT_W`.
        const MORE_W: f32 = 36.0;
        let mode_w = self.mode_w + 8.0;
        let now_for = self.active_id().and_then(|id| self.index(id).map(|i| (id, self.mode_of_pane(i))));
        if now_for != self.measured_for {
            self.measured_for = now_for;
            // Tried unfolded once; measured there, it folds again if it does not fit.
            self.controls_w = 0.0;
        }
        // The controls fold before the tabs shrink below three at their narrowest.
        // The window's full-screen button and its separator stay at the right in any width.
        let room = avail - self.tabs.len().min(3) as f32 * TAB_MIN - PLUS_W - 46.0;
        let fold_controls = room < mode_w + self.controls_w + LAYOUT_W;
        let fold_layout = room < mode_w + MORE_W + LAYOUT_W;
        // Narrower still: the mode drop-down shows its icon only (its full width is about 150).
        self.mode_compact = room < 150.0 + MORE_W + 40.0;
        // The right-hand group first, from the right edge, so the tabs take exactly what it leaves.
        let full = ui.max_rect();
        let mut right = ui.new_child(egui::UiBuilder::new().id_salt("bar-right").max_rect(full).layout(egui::Layout::right_to_left(egui::Align::Center)));
        if let Some(active) = self.active_id() {
            let ui = &mut right;
            ui.spacing_mut().item_spacing.x = 2.0;
            // The whole window on the monitor, beside the window's own buttons.
            // A shared screen keeps F11 for the remote computer: its key is Ctrl+Alt+Enter.
            let screen = self.index(active).is_some_and(|i| matches!(&self.panes[i].state, State::Live(r) if r.mode() == PaneMode::Screen));
            let (glyph, tip) = match (self.full.on, screen) {
                (true, false) => (ic::ARROWS_IN, "Leave full screen (F11)"),
                (true, true) => (ic::ARROWS_IN, "Leave full screen (Ctrl+Alt+Enter; F11 goes to the remote computer)"),
                (false, false) => (ic::ARROWS_OUT, "Full screen: the window on the whole monitor, as it is (F11)"),
                (false, true) => (
                    ic::ARROWS_OUT,
                    "Full screen: the window on the whole monitor, as it is (F11 goes to the remote computer; Ctrl+Alt+Enter fills the screen with this one)",
                ),
            };
            if ui.add(tool_button(glyph, self.full.on)).on_hover_text(tip).clicked() {
                let on = !self.full.on;
                self.set_full(ui.ctx(), on);
            }
            ui.separator();
            if !fold_layout {
                self.layout_buttons(ui, true);
                ui.separator();
            }
            if fold_controls {
                // One menu for what does not fit: the pane's controls, and the layout if needed.
                let more = icon_menu(ui, ic::DOTS, |ui| {
                    ui.set_max_width(440.0);
                    ui.horizontal_wrapped(|ui| self.controls(ui, active, action));
                    if fold_layout {
                        ui.separator();
                        ui.horizontal_wrapped(|ui| self.layout_buttons(ui, false));
                    }
                });
                more.on_hover_text("More");
                self.mode_menu(ui, active);
            } else {
                // The mode then its controls, left to right, at the width they took last time.
                let w = self.controls_w + self.mode_w + 4.0;
                ui.allocate_ui_with_layout(Vec2::new(w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    self.mode_menu(ui, active);
                    let start = ui.cursor().left();
                    self.controls(ui, active, action);
                    self.controls_w = (ui.cursor().left() - start).max(0.0);
                });
            }
        }
        let right_left = right.min_rect().left().min(full.right());
        let width = (right_left - ui.cursor().left() - PLUS_W - 12.0).max(84.0);
        let tab_rects = self.tab_strip(ui, width);
        if ui.add(tool_button(ic::PLUS, false)).on_hover_text("Open: a terminal, a screen or files in a new tab (Ctrl+Shift+T)").clicked() {
            self.picker = Some(Place::Tab);
        }
        self.drag_tab(ui, &tab_rects);
    }

    /// The tabs, as a terminal draws them: the current one in the panes' colour and joined to
    /// them, the others flat with a thin rule between. They share `width` down to `TAB_MIN`
    /// each; past that the strip scrolls (wheel, arrows, the current tab kept in view) and a
    /// list of every tab opens from the end. Returns where each tab is drawn (for dragging); a
    /// tab's close asks first.
    pub(super) fn tab_strip(&mut self, ui: &mut egui::Ui, width: f32) -> Vec<Rect> {
        let n = self.tabs.len();
        let over = n as f32 * TAB_MIN > width + 0.5;
        // Too little room for arrows too: the current tab alone, and the list of all of them.
        let cramped = over && width < TAB_MIN + 3.0 * 32.0;
        let mut view_w = width;
        if cramped {
            view_w = width - 34.0;
        } else if over {
            let start = ui.cursor().left();
            if ui.add(tool_button(ic::CARET_LEFT, false)).on_hover_text("Scroll the tabs").clicked() {
                self.tab_first = self.tab_first.saturating_sub(1);
            }
            // The arrow's width, with its spacing, for it, the other arrow and the list.
            let step = ui.cursor().left() - start;
            view_w = width - 3.0 * step - ui.spacing().item_spacing.x;
        }
        // Whole tabs only: as many as fit at their narrowest, sharing the room.
        let shown = if cramped { 1 } else if over { ((view_w / TAB_MIN).floor() as usize).max(1) } else { n };
        let fit = if over { view_w / shown as f32 } else { (width / n.max(1) as f32).clamp(TAB_MIN, TAB_MAX) };
        let bar = ui.max_rect();
        let (view, strip) = ui.allocate_exact_size(Vec2::new(fit * shown as f32, bar.height()), egui::Sense::hover());
        let view = Rect::from_x_y_ranges(view.x_range(), bar.y_range());
        // The wheel over the tabs steps through them; the current one comes into view when it
        // or the room changes.
        if over && strip.hovered() {
            let d = ui.input(|i| i.smooth_scroll_delta);
            self.tab_wheel -= if d.x != 0.0 { d.x } else { d.y };
            while self.tab_wheel.abs() >= 40.0 {
                let step = self.tab_wheel.signum();
                self.tab_wheel -= step * 40.0;
                self.tab_first = if step > 0.0 { self.tab_first + 1 } else { self.tab_first.saturating_sub(1) };
            }
        }
        if self.scrolled_for != Some((self.current, shown)) {
            self.scrolled_for = Some((self.current, shown));
            self.tab_first = self.tab_first.clamp((self.current + 1).saturating_sub(shown), self.current);
        }
        self.tab_first = self.tab_first.min(n - shown);
        let first = self.tab_first;
        let painter = ui.painter().with_clip_rect(view.intersect(ui.clip_rect()));
        let visuals = ui.visuals().clone();
        let mut rects = vec![Rect::NOTHING; n];
        let mut hovered = None;
        let mut shapes = Vec::new();
        // Each tab has its slot; the one being dragged along the bar floats under the pointer,
        // drawn over the others.
        let floating = self.dragging.filter(|_| self.drag_out.is_none());
        let pointer_x = ui.input(|i| i.pointer.latest_pos()).map(|p| p.x);
        let float_painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("tab-float"))).with_clip_rect(view);
        let mut draw: Vec<usize> = (first..first + shown).filter(|&t| Some(t) != floating).collect();
        draw.extend(floating.filter(|&t| t >= first && t < first + shown));
        for t in draw {
            let x = view.left() + (t - first) as f32 * fit;
            let slot = Rect::from_min_max(Pos2::new(x, view.top() + 6.0), Pos2::new(x + fit, view.bottom()));
            rects[t] = slot;
            let float = Some(t) == floating;
            // Tabs slide to their new places as the dragged one passes them.
            let key = self.order(t).first().copied().unwrap_or(0);
            let slide = egui::Id::unique(("tab-x", key));
            let rect = match (float, pointer_x) {
                (true, Some(px)) => {
                    let left = (px - self.grab_dx).clamp(view.left(), view.right() - fit);
                    ui.ctx().animate_value_with_time(slide, left, 0.0);
                    slot.translate(Vec2::new(left - slot.left(), 0.0))
                }
                _ => {
                    let left = ui.ctx().animate_value_with_time(slide, slot.left(), 0.12);
                    slot.translate(Vec2::new(left - slot.left(), 0.0))
                }
            };
            let painter = if float { &float_painter } else { &painter };
            if float {
                let shadow = egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(90) };
                painter.add(shadow.as_shape(rect, egui::CornerRadius { nw: theme::R_LG, ne: theme::R_LG, sw: 0, se: 0 }));
            }
            let id = self.tabs[t].active;
            let order_len = self.order(t).len();
            let (name, rtt) = self.name(id);
            let (glyph, down) = self.glyph(id);
            let on = t == self.current;
            let r = ui.interact(slot, egui::Id::unique(("tab", t)), egui::Sense::click_and_drag());
            let x_rect = Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.center().y), Vec2::splat(20.0));
            let xr = ui.interact(x_rect, egui::Id::unique(("tab-close", t)), egui::Sense::click());
            let hot = (r.hovered() || xr.hovered()) && self.dragging.is_none();
            if hot {
                hovered = Some(t);
            }
            let top = egui::CornerRadius { nw: theme::R_LG, ne: theme::R_LG, sw: 0, se: 0 };
            if on {
                painter.rect_filled(rect, top, visuals.panel_fill);
                painter.rect_stroke(rect.expand2(Vec2::new(0.0, 2.0)).translate(Vec2::new(0.0, 2.0)), top, visuals.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
            } else if hot {
                painter.rect_filled(rect.with_max_y(rect.bottom() - 4.0), theme::R_LG, visuals.widgets.hovered.weak_bg_fill);
            }
            let fg = if on { visuals.strong_text_color() } else { visuals.text_color() };
            let weak = visuals.weak_text_color();
            let mid = rect.center().y - 1.0;
            let icon_color = if down { warn() } else { weak };
            painter.text(Pos2::new(rect.left() + 12.0, mid), Align2::LEFT_CENTER, glyph, FontId::proportional(14.0), icon_color);
            let text_left = rect.left() + 34.0;
            let text_rect = Rect::from_min_max(Pos2::new(text_left, rect.top()), Pos2::new(rect.right() - 30.0, rect.bottom()));
            let mut job = egui::text::LayoutJob::default();
            let font = FontId::proportional(13.0);
            job.append(&name, 0.0, egui::TextFormat::simple(font.clone(), fg));
            if let Some(ms) = rtt {
                job.append(&format!("{:.0} ms", ms.min(999.0)), 8.0, egui::TextFormat::simple(FontId::proportional(11.5), weak));
            }
            if order_len > 1 {
                job.append(&format!("{} {}", ic::SQUARES_FOUR, order_len), 8.0, egui::TextFormat::simple(FontId::proportional(11.5), weak));
            }
            job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width().max(8.0));
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(Pos2::new(text_left, mid - galley.size().y / 2.0), galley, fg);
            if xr.hovered() {
                painter.rect_filled(x_rect, theme::R_SM, visuals.widgets.hovered.weak_bg_fill);
            }
            if on || hot {
                painter.text(x_rect.center(), Align2::CENTER_CENTER, ic::X, FontId::proportional(12.0), fg);
            }
            if xr.clicked() || r.middle_clicked() {
                self.confirm = Some(Confirm::Tab(id));
            } else if r.clicked() {
                self.select_tab(t);
            }
            if r.drag_started() {
                self.dragging = Some(t);
                self.drag_last = None;
                self.select_tab(t);
                self.grab_dx = r.interact_pointer_pos().map_or(fit / 2.0, |p| p.x - slot.left());
            }
            let mut tip = name;
            if let Some(ms) = rtt {
                tip.push_str(&format!("  ·  {:.0} ms", ms));
            }
            if order_len > 1 {
                tip.push_str(&format!("  ·  {order_len} panes"));
            }
            shapes.push((r, tip));
        }
        // Thin rules between tabs that are neither current nor under the pointer.
        for (t, rect) in rects.iter().enumerate().take(first + shown).skip(first + 1) {
            let quiet = |k: usize| k != self.current && Some(k) != hovered && Some(k) != floating;
            if quiet(t - 1) && quiet(t) {
                let x = rect.left();
                let y = rect.center().y - 1.0;
                painter.vline(x, (y - 8.0)..=(y + 8.0), Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));
            }
        }
        for (r, tip) in shapes {
            if self.dragging.is_none() {
                r.on_hover_text(tip);
            }
        }
        // A tab dragged over from another window: where it would go in.
        if let Some(at) = self.drop_hint {
            let x = if at < first {
                view.left()
            } else if at < first + shown {
                rects[at].left()
            } else {
                view.left() + shown as f32 * fit
            };
            let accent = super::ACCENT;
            painter.rect_filled(view.with_min_y(view.top() + 6.0), theme::R_LG, accent.gamma_multiply(0.10));
            ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("tab-drop"))).rect_filled(
                Rect::from_center_size(Pos2::new(x.clamp(view.left() + 1.5, view.right() - 1.5), view.center().y + 3.0), Vec2::new(3.0, view.height() - 10.0)),
                1.5,
                accent,
            );
        }
        // Where the tabs are on the screen, in pixels, for drops from windows on other monitors.
        let ctx = ui.ctx().clone();
        let on_screen = |r: Rect| Rect::from_min_max(screen_point(&ctx, r.min), screen_point(&ctx, r.max));
        self.strip_screen = on_screen(view);
        self.tabs_screen = rects.iter().map(|r| if r.is_finite() && r.is_positive() { on_screen(*r) } else { Rect::NOTHING }).collect();
        if over {
            if !cramped && ui.add(tool_button(ic::CARET_RIGHT, false)).on_hover_text("Scroll the tabs").clicked() {
                self.tab_first += 1;
            }
            let names: Vec<(String, &'static str)> = (0..n).map(|t| (self.name(self.tabs[t].active).0, self.glyph(self.tabs[t].active).0)).collect();
            let current = self.current;
            let mut pick = None;
            // Its own id: the bar's right-hand group is laid out first, in a child of this ui.
            ui.push_id("tab-list", |ui| {
                let r = icon_menu(ui, ic::CARET_DOWN, |ui| {
                    for (t, (name, glyph)) in names.iter().enumerate() {
                        if ui.selectable_label(t == current, format!("{glyph}  {name}")).clicked() {
                            pick = Some(t);
                            ui.close();
                        }
                    }
                });
                r.on_hover_text(format!("All {n} tabs"));
            });
            if let Some(t) = pick {
                self.select_tab(t);
            }
        }
        rects
    }

    /// The icon of a pane's mode, and whether it is down (connecting again, or given up).
    pub(super) fn glyph(&self, id: u64) -> (&'static str, bool) {
        let Some(p) = self.index(id).map(|i| &self.panes[i]) else { return (ic::TERMINAL, false) };
        let mode = match &p.state {
            State::Live(r) => r.mode(),
            State::Shell(_) => PaneMode::Terminal,
            State::Down(_) => p.mode,
        };
        let glyph = match mode {
            PaneMode::Terminal => ic::TERMINAL,
            PaneMode::Screen => ic::DESKTOP,
            PaneMode::Files => ic::FOLDER,
        };
        (glyph, matches!(p.state, State::Down(_)))
    }

    /// A tab being dragged: along the bar it changes places with the others; away from the bar
    /// the app shows where it would go, and takes it there when let go.
    pub(super) fn drag_tab(&mut self, ui: &egui::Ui, rects: &[Rect]) {
        let Some(d) = self.dragging else { return };
        let (pos, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released() || !i.pointer.any_down()));
        // Let go outside the window, the pointer may be gone in the same frame: where it was last.
        if let Some(p) = pos {
            self.drag_last = Some(p);
        }
        let Some(p) = pos.or(self.drag_last) else {
            if released {
                self.dragging = None;
                self.drag_out = None;
            }
            return;
        };
        let size = ui.ctx().content_rect().size();
        let away = p.y > super::chrome::HEIGHT + 40.0 || p.y < -20.0 || p.x < -20.0 || p.x > size.x + 20.0;
        let at = screen_point(ui.ctx(), p);
        if released {
            self.dragging = None;
            self.drag_out = None;
            if away && d < self.tabs.len() {
                self.dropped = Some((d, at));
            }
            return;
        }
        self.drag_out = away.then_some(at);
        if away {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            return;
        }
        // It changes places once the middle of the floating tab passes into another's slot.
        let width = rects.get(d).map_or(0.0, |r| r.width());
        let middle = p.x - self.grab_dx + width / 2.0;
        if let Some(to) = rects.iter().position(|r| middle >= r.left() && middle < r.right())
            && to != d
            && d < self.tabs.len()
        {
            let tab = self.tabs.remove(d);
            self.tabs.insert(to, tab);
            self.current = to;
            self.dragging = Some(to);
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    /// The drop-down that turns the active pane into a terminal, the screen or the files.
    pub(super) fn mode_menu(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(i) = self.index(id) else { return };
        let start = ui.cursor().left();
        let cur = match &self.panes[i].state {
            State::Live(r) => r.mode(),
            _ => self.panes[i].mode,
        };
        let mut chosen = None;
        let text = if self.mode_compact {
            format!("{} {}", cur.glyph(), ic::CARET_DOWN)
        } else {
            format!("{}  {}  {}", cur.glyph(), cur.name(), ic::CARET_DOWN)
        };
        let label = RichText::new(text).size(13.0);
        let b = egui::Button::new(label).frame_when_inactive(false).min_size(Vec2::new(0.0, 30.0));
        let (r, _) = egui::containers::menu::MenuButton::from_button(b)
            .ui(ui, |ui| {
                crate::gui::menu_look(ui);
                ui.set_min_width(190.0);
                for m in PaneMode::ALL.into_iter().filter(|m| m.on() && (cfg!(feature = "remote") || *m == PaneMode::Terminal)) {
                    if ui.add(egui::Button::selectable(cur == m, format!("{}  {}", m.glyph(), m.name()))).clicked() {
                        chosen = Some(m);
                        ui.close();
                    }
                }
            });
        r.on_hover_text("What this pane shows of its connection; switching does not connect again");
        // Laid out left to right here or right to left in the bar: the width either way.
        let w = (ui.cursor().left() - start).abs();
        if w > 20.0 {
            self.mode_w = w;
        }
        if let Some(m) = chosen.filter(|m| *m != cur) {
            self.set_mode(id, m);
        }
    }

    /// The active pane's own controls (the screen's quality, keys, chat...).
    pub(super) fn controls(&mut self, ui: &mut egui::Ui, id: u64, action: &mut Option<(u64, remote::Deferred)>) {
        let Some(i) = self.index(id) else { return };
        // Close comes last, after the mode's controls.
        match &mut self.panes[i].state {
            State::Live(r) => {
                let mut a = None;
                r.toolbar_items(ui, &mut a);
                if let Some(f) = a {
                    *action = Some((id, f));
                }
            }
            // A plain shell: how it is shown and its settings.
            State::Shell(t) => t.controls(ui),
            State::Down(_) => {}
        }
        // Close last: the pane closes (and its connection ends), and with the tab's last pane the tab.
        let (glyph, tip) = if self.is_remote(id) || !self.is_shell(id) {
            (ic::X, "Close: end this connection and close the pane (Ctrl+Shift+W)")
        } else {
            (ic::X, "Close this pane (Ctrl+Shift+W)")
        };
        if ui.add(tool_button(glyph, false)).on_hover_text(tip).clicked() {
            self.confirm = Some(Confirm::Pane(id));
        }
    }

    /// The tab's layout: the active pane alone or the grid, split beside or below, arrange, and
    /// move the active pane.
    pub(super) fn layout_buttons(&mut self, ui: &mut egui::Ui, rtl: bool) {
        let zoomed = self.zoom.is_some();
        let mut buttons: Vec<(&str, &str, u8)> = vec![
            (
                if zoomed { ic::CORNERS_IN } else { ic::CORNERS_OUT },
                if zoomed { "Back to the grid: every pane of the tab (Ctrl+Shift+Enter)" } else { "This pane alone: it fills the tab, the others wait (Ctrl+Shift+Enter)" },
                0,
            ),
            (ic::SPLIT_SIDE, "Split: a new pane beside this one (Alt+Shift+=)", 1),
            (ic::SPLIT_BELOW, "Split: a new pane below this one (Alt+Shift+-)", 2),
            (ic::CARET_LEFT, "Move this pane left (Alt+arrows go between panes)", 3),
            (ic::CARET_RIGHT, "Move this pane right", 4),
        ];
        // The arrangement menu after the splits.
        buttons.insert(3, (ic::LAYOUT, "Layout: arrange this tab's panes", 5));
        // One pane: nothing to zoom, as nothing to arrange; both show, greyed.
        let many = self.order(self.current).len() > 1;
        if rtl {
            buttons.reverse();
        }
        for (glyph, tip, what) in buttons {
            if what == 5 {
                self.layout_menu(ui, glyph, tip);
                continue;
            }
            let on = what == 0 && zoomed;
            // Zoom and moving need another pane; splitting is how one gets there.
            let enabled = matches!(what, 1 | 2) || many || (what == 0 && zoomed);
            let button = ui.add_enabled(enabled, tool_button(glyph, on)).on_hover_text(tip);
            let button = button.on_disabled_hover_text(format!("{tip}: for a tab of two panes or more"));
            if !button.clicked() {
                continue;
            }
            match what {
                0 => {
                    let z = if zoomed { None } else { self.active_id() };
                    self.set_zoom(z);
                }
                1 | 2 => self.picker = Some(Place::Split { below: what == 2 }),
                _ => self.move_active(what == 3),
            }
        }
    }

    /// The tab's arrangements: the same sizes, side by side, stacked, a main pane, a grid.
    pub(super) fn layout_menu(&mut self, ui: &mut egui::Ui, glyph: &str, tip: &str) {
        let many = self.order(self.current).len() > 1;
        let even = self.tabs.get(self.current).is_none_or(|t| t.root.is_even());
        let mut chosen = None;
        ui.add_enabled_ui(many, |ui| {
            icon_menu(ui, glyph, |ui| {
                for (layout, g, text) in [
                    (Layout::Even, ic::EQUALS, "Even sizes  (keep the arrangement)"),
                    (Layout::Side, ic::SPLIT_SIDE, "Side by side"),
                    (Layout::Stacked, ic::SPLIT_BELOW, "Stacked"),
                    (Layout::MainLeft, ic::LAYOUT, "Main pane left, the others stacked"),
                    (Layout::MainTop, ic::ROWS, "Main pane on top, the others side by side"),
                    (Layout::Grid, ic::SQUARES_FOUR, "Grid"),
                ] {
                    // Sizes already even: nothing it would change.
                    let item = ui.add_enabled(layout != Layout::Even || !even, egui::Button::new(format!("{g}  {text}")));
                    if item.on_disabled_hover_text("The panes are already the same size").clicked() {
                        chosen = Some(layout);
                        ui.close();
                    }
                }
            })
            .on_hover_text(tip)
            .on_disabled_hover_text("Layout: for a tab of two panes or more");
        });
        if let Some(layout) = chosen {
            self.arrange(layout);
        }
    }
}
