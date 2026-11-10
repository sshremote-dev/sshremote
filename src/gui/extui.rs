//! What extensions put in the workspace: the status bar along the bottom, the side panel on the
//! right with their panels, and the command palette (Ctrl+Shift+P).

use eframe::egui::{self, Align2, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use super::{ic, theme};

pub(super) const BAR: f32 = 24.0;
const SIDE: f32 = 360.0;

/// The side panel's state in a workspace.
#[derive(Default)]
pub(super) struct Side {
    pub open: bool,
    /// The panel shown, by extension and id.
    pub shown: Option<(String, String)>,
    /// The panel's document, made again when its text changes.
    doc: Option<(u64, crate::markdown::Doc)>,
    /// The newest panel change looked at.
    seen: u64,
    /// The last notice from an extension, and when it came.
    notice: Option<(String, f64)>,
}

/// Draws the status bar and the side panel at the edges of `area`; the room left for the panes.
pub(super) fn edges(ui: &mut egui::Ui, area: Rect, side: &mut Side) -> Rect {
    let now = ui.input(|i| i.time);
    if let Some(n) = crate::exthost::take_notices().pop() {
        side.notice = Some((n, now));
    }
    if side.notice.as_ref().is_some_and(|(_, t)| now - t > 8.0) {
        side.notice = None;
    }
    let items = crate::exthost::items();
    let panels = crate::exthost::panels();
    let mut area = area;
    if !items.is_empty() || side.notice.is_some() || !panels.is_empty() {
        let bar = Rect::from_min_max(Pos2::new(area.left(), area.bottom() - BAR), area.max);
        area.max.y -= BAR;
        status_bar(ui, bar, &items, !panels.is_empty(), side);
    }
    // A panel shown or changed by its extension comes to the front.
    if let Some(p) = panels.iter().max_by_key(|p| p.rev)
        && p.rev > side.seen
    {
        side.seen = p.rev;
        side.shown = Some((p.ext.clone(), p.id.clone()));
        side.open = true;
    }
    if side.open && !panels.is_empty() && area.width() > SIDE + 240.0 {
        let rect = Rect::from_min_max(Pos2::new(area.right() - SIDE, area.top()), area.max);
        area.max.x -= SIDE;
        side_panel(ui, rect, &panels, side);
    }
    area
}

fn status_bar(ui: &mut egui::Ui, rect: Rect, items: &[crate::exthost::Item], has_panels: bool, side: &mut Side) {
    let v = ui.visuals().clone();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, v.extreme_bg_color);
    ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(6.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    c.spacing_mut().item_spacing.x = 4.0;
    if has_panels {
        let glyph = if side.open { ic::LAYOUT } else { ic::ARTICLE };
        if c.add(egui::Button::new(RichText::new(glyph).size(13.0)).frame_when_inactive(false)).on_hover_text("Extension panels").clicked() {
            side.open = !side.open;
        }
    }
    for item in items.iter().rev() {
        let b = egui::Button::new(RichText::new(&item.text).size(12.0)).frame_when_inactive(false);
        let mut r = c.add_enabled(item.command.is_some() || !item.tooltip.is_empty(), b);
        if !item.tooltip.is_empty() {
            r = r.on_hover_text(&item.tooltip).on_disabled_hover_text(&item.tooltip);
        }
        if r.clicked()
            && let Some(cmd) = &item.command
        {
            crate::exthost::command(&item.ext, cmd, "");
        }
    }
    if let Some((n, _)) = &side.notice {
        c.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{}  {n}", ic::CHAT)).size(12.0).color(super::brand()));
        });
    }
}

fn side_panel(ui: &mut egui::Ui, rect: Rect, panels: &[crate::exthost::Panel], side: &mut Side) {
    let v = ui.visuals().clone();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, v.panel_fill);
    ui.painter().vline(rect.left(), rect.y_range(), Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color));
    let shown = side
        .shown
        .as_ref()
        .and_then(|(e, i)| panels.iter().find(|p| &p.ext == e && &p.id == i))
        .or_else(|| panels.first())
        .cloned();
    let Some(panel) = shown else { return };
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(8.0)).layout(egui::Layout::top_down(egui::Align::Min)));
    // One tab for each panel when there are several.
    c.horizontal_wrapped(|ui| {
        for p in panels {
            let on = p.ext == panel.ext && p.id == panel.id;
            let title = if p.title.is_empty() { p.id.clone() } else { p.title.clone() };
            if ui.selectable_label(on, RichText::new(title).strong()).clicked() {
                side.shown = Some((p.ext.clone(), p.id.clone()));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(egui::Button::new(ic::X).frame_when_inactive(false)).on_hover_text("Hide the panels").clicked() {
                side.open = false;
            }
        });
    });
    c.separator();
    if side.doc.as_ref().is_none_or(|(rev, _)| *rev != panel.rev) {
        side.doc = Some((panel.rev, crate::markdown::Doc::new(&panel.markdown)));
    }
    let th = crate::markdown::Theme::terminal(v.panel_fill, v.text_color());
    let doc = &side.doc.as_ref().expect("made").1;
    let ((), actions) = crate::markdown::catch_actions(|| {
        egui::ScrollArea::vertical().id_salt(("ext-panel", &panel.ext, &panel.id)).auto_shrink(false).show(&mut c, |ui| {
            crate::markdown::show(ui, doc, egui::Id::unique(("ext-panel-doc", &panel.ext, &panel.id)), &th);
        });
    });
    for a in actions {
        crate::exthost::action(&panel.ext, &panel.id, &a);
    }
}

/// One entry of the command palette.
pub(super) struct Entry {
    pub label: String,
    pub run: Run,
}

pub(super) enum Run {
    /// A command of a running extension.
    Host { ext: String, id: String },
    /// A command of a script extension.
    Script(crate::script::Command),
    /// Something the workspace does itself.
    Builtin(&'static str),
    Project(String),
}

/// What the palette offers: the workspace's own commands, the projects, the extensions' commands.
pub(super) fn entries() -> Vec<Entry> {
    let mut out: Vec<Entry> = [
        ("New tab", "tab"),
        ("Split right", "right"),
        ("Split down", "down"),
        ("Show or hide the extension panels", "side"),
    ]
    .into_iter()
    .map(|(label, id)| Entry { label: label.into(), run: Run::Builtin(id) })
    .collect();
    out.extend(super::project::list().into_iter().map(|p| Entry { label: format!("Open project: {p}"), run: Run::Project(p) }));
    out.extend(crate::exthost::commands().into_iter().map(|(ext, id, label)| Entry { label, run: Run::Host { ext, id } }));
    let names: std::collections::HashMap<String, String> = crate::ext::installed().into_iter().map(|m| (m.id, m.name)).collect();
    out.extend(crate::script::commands().into_iter().map(|c| Entry {
        label: format!("{}: {}", names.get(&c.ext).cloned().unwrap_or_default(), c.label),
        run: Run::Script(c),
    }));
    out
}

/// Whether every word typed is in the label, in any case.
pub(super) fn matches(label: &str, filter: &str) -> bool {
    let l = label.to_lowercase();
    filter.split_whitespace().all(|w| l.contains(&w.to_lowercase()))
}

/// The palette: a filter and the entries that match, Up and Down to move, Enter to run.
pub(super) fn palette(ctx: &egui::Context, filter: &mut String, at: &mut usize) -> Option<Option<Entry>> {
    let all: Vec<Entry> = entries().into_iter().filter(|e| matches(&e.label, filter)).collect();
    *at = (*at).min(all.len().saturating_sub(1));
    let (modal, fit) = super::modal(ctx, "command-palette", Vec2::new(560.0, 380.0));
    let mut chosen = None;
    let mut close = false;
    let r = modal.show(ctx, |ui| {
        ui.set_width(fit.x);
        let edit = ui.add(egui::TextEdit::singleline(filter).hint_text("Type a command").desired_width(fit.x));
        edit.request_focus();
        let (up, down, enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if up {
            *at = at.saturating_sub(1);
        }
        if down && *at + 1 < all.len() {
            *at += 1;
        }
        close = esc;
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(fit.y - 40.0).auto_shrink([false, true]).show(ui, |ui| {
            if all.is_empty() {
                ui.label(RichText::new("No command matches").weak());
            }
            for (n, e) in all.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(fit.x, 26.0), Sense::click());
                if n == *at {
                    ui.painter().rect_filled(rect, theme::R_SM, ui.visuals().selection.bg_fill.gamma_multiply(0.6));
                    resp.scroll_to_me(None);
                }
                let color = if n == *at { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
                ui.painter().text(rect.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER, &e.label, FontId::proportional(13.5), color);
                if resp.clicked() {
                    *at = n;
                    chosen = Some(n);
                }
            }
        });
        if enter && !all.is_empty() {
            chosen = Some(*at);
        }
    });
    if r.should_close() {
        close = true;
    }
    if let Some(n) = chosen {
        return Some(all.into_iter().nth(n));
    }
    close.then_some(None)
}
