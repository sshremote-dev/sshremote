//! One look for the home window, its sheets and the settings: the spacing, cards, headings,
//! form rows, buttons of four kinds, notices, and a spinner for what is still being looked up.

use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke, Vec2};

use super::{bad, ic, theme, warn};

/// Around a window's content.
pub(crate) const PAGE: i8 = 24;
/// Between groups in a column.
pub(crate) const GAP: f32 = 18.0;
/// Inside a card.
pub(crate) const PAD: i8 = 14;
/// A form's label column.
pub(crate) const LABEL_W: f32 = 112.0;
/// The height of buttons and fields.
pub(crate) const CONTROL_H: f32 = 32.0;

/// A group on a faint fill with a hairline edge, as wide as there is room.
pub(crate) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let v = ui.visuals();
    let (fill, edge) = (if v.dark_mode { v.faint_bg_color } else { Color32::WHITE }, v.widgets.noninteractive.bg_stroke.color);
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, edge))
        .corner_radius(CornerRadius::same(theme::R_MD))
        .inner_margin(egui::Margin::same(PAD))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A list's frame: a card with room for its rows' own highlight.
pub(crate) fn list<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let v = ui.visuals();
    let (fill, edge) = (if v.dark_mode { v.faint_bg_color } else { Color32::WHITE }, v.widgets.noninteractive.bg_stroke.color);
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, edge))
        .corner_radius(CornerRadius::same(theme::R_MD))
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner
}

/// A small heading over a group.
pub(crate) fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(11.0).strong().color(ui.visuals().weak_text_color()));
    ui.add_space(2.0);
}

/// A page's or a sheet's title, and a line about it.
pub(crate) fn title(ui: &mut egui::Ui, title: &str, about: &str) {
    ui.label(RichText::new(title).size(18.0).strong());
    if !about.is_empty() {
        ui.add(egui::Label::new(RichText::new(about).weak().size(12.5)).wrap());
    }
    ui.add_space(12.0);
}

/// A row of a form: its label in a column of its own, then what `add` puts beside it.
pub(crate) fn row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(LABEL_W, CONTROL_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(LABEL_W);
            ui.label(RichText::new(label).color(ui.visuals().weak_text_color()));
        });
        add(ui)
    })
    .inner
}

/// A one-line field as tall as the buttons, filling what is left of its row.
pub(crate) fn field<'t>(text: &'t mut dyn egui::TextBuffer, hint: &str) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(8, 7))
}

/// A secondary action: outlined, as tall as the main one.
pub(crate) fn secondary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(text.into()).min_size(Vec2::new(0.0, CONTROL_H))
}

/// A quiet action in the app's blue, framed only under the mouse.
pub(crate) fn ghost(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(super::brand())).frame_when_inactive(false).min_size(Vec2::new(0.0, 28.0))
}

/// An action that removes something: red, framed only under the mouse until it is to be confirmed.
pub(crate) fn danger(text: impl Into<String>, confirm: bool) -> egui::Button<'static> {
    if confirm {
        egui::Button::new(RichText::new(text.into()).strong().color(Color32::WHITE)).fill(bad()).min_size(Vec2::new(0.0, CONTROL_H))
    } else {
        egui::Button::new(RichText::new(text.into()).color(bad())).frame_when_inactive(false).min_size(Vec2::new(0.0, CONTROL_H))
    }
}

/// A small spinner and what is being waited for.
pub(crate) fn loading(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(14.0).color(ui.visuals().weak_text_color()));
        ui.label(RichText::new(text).weak().size(12.5));
    });
}

/// How a notice reads.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tone {
    Info,
    Warn,
    Bad,
}

/// A notice: its icon and its words on a tint of its colour.
pub(crate) fn notice(ui: &mut egui::Ui, tone: Tone, title: &str, body: &str) {
    let (color, glyph) = match tone {
        Tone::Info => (super::brand(), ic::INFO),
        Tone::Warn => (warn(), ic::WARNING),
        Tone::Bad => (bad(), ic::X_CIRCLE),
    };
    egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.35)))
        .corner_radius(CornerRadius::same(theme::R_MD))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(glyph).size(16.0).color(color));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    if !title.is_empty() {
                        ui.add(egui::Label::new(RichText::new(title).strong().color(ui.visuals().strong_text_color())).wrap());
                    }
                    if !body.is_empty() {
                        ui.add(egui::Label::new(RichText::new(body).size(12.5)).wrap());
                    }
                });
            });
        });
}

/// A choice of a few shown side by side as one control, the chosen one filled.
pub(crate) fn segmented(ui: &mut egui::Ui, chosen: &mut usize, options: &[&str]) -> bool {
    let mut changed = false;
    let v = ui.visuals().clone();
    egui::Frame::new()
        .fill(v.extreme_bg_color)
        .corner_radius(CornerRadius::same(theme::R_SM + 2))
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (i, o) in options.iter().enumerate() {
                    let on = *chosen == i;
                    let text = if on { RichText::new(*o).strong().color(v.strong_text_color()) } else { RichText::new(*o).color(v.weak_text_color()) };
                    let mut b = egui::Button::new(text).min_size(Vec2::new(0.0, 28.0)).frame_when_inactive(on);
                    if on {
                        b = b.fill(if v.dark_mode { v.widgets.active.bg_fill } else { Color32::WHITE }).stroke(Stroke::NONE);
                    }
                    if ui.add(b).clicked() && !on {
                        *chosen = i;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// A dot in a status colour, before a status line.
pub(crate) fn dot(ui: &mut egui::Ui, color: Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 16.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), 4.0, color);
}

/// A round mark with a glyph, as a computer's in the lists.
pub(crate) fn mark(ui: &mut egui::Ui, glyph: &str, color: Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(34.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), 17.0, color.gamma_multiply(0.16));
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(16.0), color);
}

/// Copies `text`, and says so for a moment where `at` is.
pub(crate) fn copy(ui: &egui::Ui, text: &str, at: egui::Id) {
    ui.ctx().copy_text(text.to_string());
    let now = ui.input(|i| i.time);
    ui.ctx().data_mut(|d| d.insert_temp(at, now));
}

/// Whether `copy` was done for `at` in the last moment (and the window is redrawn until not).
pub(crate) fn copied(ui: &egui::Ui, at: egui::Id) -> bool {
    let Some(t) = ui.ctx().data(|d| d.get_temp::<f64>(at)) else { return false };
    let left = 1.5 - (ui.input(|i| i.time) - t);
    if left > 0.0 {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(left));
    }
    left > 0.0
}
