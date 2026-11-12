//! Full screen for a workspace window, and the dock that stands in for its title bar there.
//!
//! The dock is a small pill hanging from the top edge: the active pane's state and a caret. It
//! shows for a moment when full screen begins, then only while the pointer is at the top edge or
//! on it, so the picture is whole. Its caret drops a band with the title bar's tabs and controls
//! down below it, and folds it up again; unpinned, the band also folds up by itself a moment after
//! the pointer leaves. The pill is dragged along the top edge by its grip.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Vec2, ViewportCommand};

use super::ic;

/// The window's full screen, kept in step with the system's.
#[derive(Default)]
pub(super) struct FullScreen {
    /// The window is meant to cover the monitor.
    pub on: bool,
    /// It was maximized before. A maximized window without a system frame keeps its client area
    /// to the monitor's work area on Windows, which leaves the taskbar's band empty in full screen,
    /// so it is restored first and maximized again on the way out.
    was_maximized: bool,
    /// The system has reported full screen since it was asked for.
    seen: bool,
    /// The pane zoomed on the way in (a screen in a split tab), zoomed out again on the way out.
    pub zoomed: Option<u64>,
}

impl FullScreen {
    /// Enters or leaves full screen. Returns whether anything changed.
    pub fn set(&mut self, ctx: &egui::Context, on: bool) -> bool {
        if on == self.on {
            return false;
        }
        self.on = on;
        self.seen = false;
        if on {
            self.was_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            if self.was_maximized {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
            }
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            if self.was_maximized {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
            }
        }
        true
    }

    /// Follows the system when it ends full screen by itself (a window manager's key, the macOS
    /// button). Returns whether full screen ended that way.
    pub fn follow_system(&mut self, ctx: &egui::Context) -> bool {
        match ctx.input(|i| i.viewport().fullscreen) {
            Some(true) if self.on => self.seen = true,
            Some(false) if self.on && self.seen => {
                self.on = false;
                self.seen = false;
                return true;
            }
            _ => {}
        }
        false
    }
}

/// What the folded dock says about the active pane.
pub(super) struct Status {
    pub glyph: &'static str,
    pub name: String,
    /// The round trip in milliseconds, for a connection with a screen.
    pub rtt: Option<f32>,
    pub view_only: bool,
    /// Shown on hover.
    pub details: String,
}

pub(super) struct Dock {
    /// The band with the bar's controls hangs below the pill.
    pub open: bool,
    /// The band stays down; remembered.
    pub pinned: bool,
    /// The pill's middle along the top edge, 0 left to 1 right; remembered.
    x: f32,
    /// The pill's width last frame, to keep it on the screen.
    w: f32,
    /// Until when an unpinned band stays after the pointer left the dock (in `ctx` time).
    until: f64,
    /// Until when the pill shows.
    shown_until: f64,
    /// Where the pill and the band were drawn last frame, to know when the pointer is on them.
    rect: Rect,
}

impl Default for Dock {
    fn default() -> Dock {
        Dock {
            open: false,
            pinned: crate::hosts::setting("toolbar_pinned").is_some_and(|v| v == "yes"),
            x: crate::hosts::setting("toolbar_x").and_then(|v| v.parse().ok()).unwrap_or(0.5),
            w: 220.0,
            until: 0.0,
            shown_until: 0.0,
            rect: Rect::NOTHING,
        }
    }
}

/// How long an unpinned band waits after the pointer left the dock before it folds up.
const LINGER: f64 = 2.0;
/// How long the pill shows when full screen begins, and after the pointer left it.
const GREET: f64 = 3.0;
const STAY: f64 = 1.0;
/// The band's bar height.
const BAR_H: f32 = 36.0;

impl Dock {
    /// Full screen began: the pill shows a moment (with the band if pinned), then steps aside.
    pub fn greet(&mut self, now: f64) {
        self.open = self.pinned;
        self.shown_until = now + GREET;
        self.until = now + LINGER;
    }

    /// Drops the band down or folds it up (the caret, Ctrl+Alt+Home).
    pub fn toggle(&mut self, now: f64) {
        self.open = !self.open;
        self.until = now + LINGER;
        self.shown_until = self.shown_until.max(now + STAY);
    }

    /// Draws the dock over the top of `screen` when it is to be seen: the pill at the top edge
    /// and, unfolded, the band below it, where `bar` draws the controls into a bar `bar_w` wide
    /// (narrower if the screen is). The pill never moves or changes width, so its caret stays
    /// under the pointer. Returns the dock's rectangle.
    pub fn show(&mut self, ctx: &egui::Context, screen: Rect, status: &Status, bar_w: f32, bar: impl FnOnce(&mut egui::Ui)) -> Option<Rect> {
        let now = ctx.input(|i| i.time);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let at_edge = pointer.is_some_and(|p| p.y <= screen.top() + 4.0 && screen.x_range().contains(p.x));
        let on_it = pointer.is_some_and(|p| self.rect.expand(16.0).contains(p));
        let menus = super::menu_open(ctx) || ctx.any_popup_open();
        if at_edge || on_it || menus {
            self.shown_until = self.shown_until.max(now + STAY);
        }
        if on_it || menus {
            self.until = now + LINGER;
        }
        // Unpinned, the band folds up a moment after the pointer left the dock.
        if self.open && !self.pinned && now > self.until {
            self.open = false;
        }
        if !self.open && now >= self.shown_until {
            self.rect = Rect::NOTHING;
            return None;
        }
        let pill = self.pill(ctx, screen, status, now);
        let mut rect = pill;
        if self.open {
            rect = rect.union(self.band(ctx, screen, pill, bar_w, bar));
        }
        self.rect = rect;
        // Wakes up to fold or step aside on time, with no input to wake it.
        let next = if self.open && !self.pinned { self.until } else { self.shown_until };
        if next > now {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(next - now + 0.05));
        }
        Some(rect)
    }

    /// The pill hanging from the top edge: the grip, the active pane's state and the caret.
    fn pill(&mut self, ctx: &egui::Context, screen: Rect, status: &Status, now: f64) -> Rect {
        let left = self.left(screen);
        let resp = egui::Area::new(egui::Id::unique("full-screen-dock-pill"))
            .fixed_pos(Pos2::new(left, screen.top()))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let radius = CornerRadius { nw: 0, ne: 0, sw: crate::gui::theme::R_MD, se: crate::gui::theme::R_MD };
                frame(ui, radius, egui::Margin { left: 4, right: 4, top: 1, bottom: 2 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        self.grip(ui, screen);
                        status_chip(ui, status);
                        let (glyph, tip) = if self.open {
                            (ic::CARET_UP, "Fold the toolbar up (Ctrl+Alt+Home)")
                        } else {
                            (ic::CARET_DOWN, "Drop the toolbar down (Ctrl+Alt+Home)")
                        };
                        let b = egui::Button::new(RichText::new(glyph).size(14.0)).frame_when_inactive(self.open).selected(self.open).min_size(Vec2::new(26.0, 26.0));
                        if ui.add(b).on_hover_text(tip).clicked() {
                            self.toggle(now);
                        }
                    });
                });
            });
        let rect = resp.response.rect;
        self.w = rect.width();
        rect
    }

    /// The band below the pill: the title bar's tabs and controls, and the pin.
    fn band(&mut self, ctx: &egui::Context, screen: Rect, pill: Rect, bar_w: f32, bar: impl FnOnce(&mut egui::Ui)) -> Rect {
        let w = bar_w.min(screen.width() - 80.0).max(320.0);
        // Its full width: the bar, the pin and the frame's margins.
        let full = w + 26.0 + 4.0 + 12.0;
        let left = (pill.center().x - full / 2.0).clamp(screen.left() + 4.0, (screen.right() - full - 4.0).max(screen.left() + 4.0));
        egui::Area::new(egui::Id::unique("full-screen-dock-band"))
            .fixed_pos(Pos2::new(left, pill.bottom() + 4.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                frame(ui, CornerRadius::same(crate::gui::theme::R_MD), egui::Margin::symmetric(6, 2)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.allocate_ui_with_layout(Vec2::new(w, BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // The bar lays its right-hand group against the far edge: all of it is the band's.
                            ui.set_min_size(Vec2::new(w, BAR_H));
                            ui.set_clip_rect(ui.max_rect().expand(2.0));
                            bar(ui);
                        });
                        self.pin(ui);
                    });
                });
            })
            .response
            .rect
    }

    /// The dock's left edge for its remembered place, kept inside the screen.
    fn left(&self, screen: Rect) -> f32 {
        let centre = screen.left() + self.x * screen.width();
        (centre - self.w / 2.0).clamp(screen.left() + 4.0, (screen.right() - self.w - 4.0).max(screen.left() + 4.0))
    }

    /// The handle that moves the dock along the top edge; where it was let go is remembered.
    fn grip(&mut self, ui: &mut egui::Ui, screen: Rect) {
        let g = ui.add(egui::Label::new(RichText::new(ic::GRIP).size(15.0).weak()).sense(egui::Sense::click_and_drag()).selectable(false));
        let g = g.on_hover_text("Drag to move the toolbar along the top edge");
        if g.dragged() {
            self.x = (self.x + g.drag_delta().x / screen.width().max(1.0)).clamp(0.0, 1.0);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if g.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if g.drag_stopped() {
            crate::hosts::set_setting("toolbar_x", &format!("{:.3}", self.x));
        }
    }

    /// Pinned: the band stays down. The pin is filled while on.
    fn pin(&mut self, ui: &mut egui::Ui) {
        let (text, tip) = if self.pinned {
            (RichText::new(ic::PUSH_PIN).size(15.0).family(super::theme::filled()), "Pinned: the toolbar stays down. Click to let it fold up by itself")
        } else {
            (RichText::new(ic::PUSH_PIN).size(15.0).color(ui.visuals().weak_text_color()), "Pin: keep the toolbar down")
        };
        let b = egui::Button::new(text).frame_when_inactive(self.pinned).selected(self.pinned).min_size(Vec2::splat(26.0));
        if ui.add(b).on_hover_text(tip).clicked() {
            self.pinned = !self.pinned;
            crate::hosts::set_setting("toolbar_pinned", if self.pinned { "yes" } else { "no" });
        }
    }
}

/// The dock's frames: the window's fill and outline, with a soft shadow.
fn frame(ui: &egui::Ui, radius: CornerRadius, margin: egui::Margin) -> egui::Frame {
    let v = ui.visuals();
    egui::Frame::new()
        .fill(v.window_fill)
        .stroke(v.window_stroke)
        .corner_radius(radius)
        .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(80) })
        .inner_margin(margin)
}

/// The active pane at a glance: a dot coloured by the round trip (for a screen), its name, the
/// round trip, and an eye while it only watches. Details on hover.
fn status_chip(ui: &mut egui::Ui, s: &Status) {
    let resp = ui
        .scope(|ui| {
            // A long name is cut short with an ellipsis.
            ui.set_max_width(260.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_space(2.0);
            match s.rtt {
                Some(rtt) => {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, rtt_colour(rtt));
                }
                None => {
                    ui.label(RichText::new(s.glyph).size(14.0));
                }
            }
            ui.add(egui::Label::new(RichText::new(&s.name).size(13.0)).truncate());
            if let Some(rtt) = s.rtt {
                // A fixed width, so the caret stays put as the round trip changes.
                let text = if rtt >= 999.5 { "999+ ms".to_string() } else { format!("{rtt:.0} ms") };
                ui.add_sized([46.0, 18.0], egui::Label::new(RichText::new(text).size(12.0).weak()));
            }
            if s.view_only {
                ui.label(RichText::new(ic::EYE).size(14.0).weak());
            }
        })
        .response;
    resp.on_hover_text(&s.details);
}

/// Green below 80 ms, amber below 250 ms, red above.
pub(super) fn rtt_colour(rtt: f32) -> Color32 {
    match rtt {
        r if r < 80.0 => super::good(),
        r if r < 250.0 => super::warn(),
        _ => super::bad(),
    }
}
