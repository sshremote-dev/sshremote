//! Full screen for a workspace window, and the dock that stands in for its title bar there.
//!
//! Folded, the dock is a tiny tab hanging from the top edge: the active pane's state and a caret.
//! It shows for a moment when full screen begins, then only while the pointer is at the top edge or
//! on it, so the picture is whole. A click on it unfolds it: the tab gives way to a band in its
//! place with the title bar's tabs and controls, whose own caret folds it back into the tab;
//! unpinned, the band also folds by itself a moment after the pointer leaves. Dragging the tab
//! moves the dock along the top edge.

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
    /// Unfolded: the band with the bar's controls stands in for the tab.
    pub open: bool,
    /// The band stays down; remembered.
    pub pinned: bool,
    /// The dock's middle along the top edge, 0 left to 1 right; remembered.
    x: f32,
    /// The tab's width last frame, to keep it on the screen.
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
            w: 160.0,
            until: 0.0,
            shown_until: 0.0,
            rect: Rect::NOTHING,
        }
    }
}

/// How long an unpinned band waits after the pointer left the dock before it folds up.
const LINGER: f64 = 2.0;
/// How long the tab shows when full screen begins, and after the pointer left it.
const GREET: f64 = 3.0;
const STAY: f64 = 1.0;
/// The band's bar height.
const BAR_H: f32 = 36.0;
/// The folded tab's text size and the longest name it shows.
const TAB_TEXT: f32 = 11.0;
const TAB_NAME_W: f32 = 150.0;

impl Dock {
    /// Full screen began: the tab shows a moment (the band, if pinned), then steps aside.
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

    /// Draws the dock over the top of `screen` when it is to be seen: folded the tab, unfolded the
    /// band in its place, where `bar` draws the controls into a bar `bar_w` wide (narrower if the
    /// screen is). Returns the dock's rectangle.
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
        let rect = if self.open { self.band(ctx, screen, bar_w, bar, now) } else { self.tab(ctx, screen, status, now) };
        self.rect = rect;
        // Wakes up to fold or step aside on time, with no input to wake it.
        let next = if self.open && !self.pinned { self.until } else { self.shown_until };
        if next > now {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(next - now + 0.05));
        }
        Some(rect)
    }

    /// The folded tab at the top edge: the active pane's state and a caret, all small. A click
    /// unfolds the band; a drag moves the dock along the edge, and where it was let go is kept.
    fn tab(&mut self, ctx: &egui::Context, screen: Rect, status: &Status, now: f64) -> Rect {
        let left = self.left(screen);
        let resp = egui::Area::new(egui::Id::unique("full-screen-dock-tab"))
            .fixed_pos(Pos2::new(left, screen.top()))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let radius = CornerRadius { nw: 0, ne: 0, sw: crate::gui::theme::R_SM, se: crate::gui::theme::R_SM };
                let inner = frame(ui, radius, egui::Margin { left: 7, right: 5, top: 1, bottom: 2 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        status_chip(ui, status);
                        ui.label(RichText::new(ic::CARET_DOWN).size(TAB_TEXT).weak());
                    });
                });
                // The whole tab is the button, and the handle.
                let r = ui.interact(inner.response.rect, ui.scope_id().with("tab"), egui::Sense::click_and_drag());
                let r = r.on_hover_text(format!("{}\n\nClick for the toolbar (Ctrl+Alt+Home); drag to move it along the edge", status.details));
                if r.dragged() {
                    self.x = (self.x + r.drag_delta().x / screen.width().max(1.0)).clamp(0.0, 1.0);
                    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                } else if r.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if r.drag_stopped() {
                    crate::hosts::set_setting("toolbar_x", &format!("{:.3}", self.x));
                }
                if r.clicked() {
                    self.toggle(now);
                }
                if r.hovered() || r.dragged() {
                    ui.painter().rect_stroke(inner.response.rect, radius, egui::Stroke::new(1.0, super::ACCENT), egui::StrokeKind::Inside);
                }
            });
        let rect = resp.response.rect;
        self.w = rect.width();
        rect
    }

    /// The band in the tab's place: the title bar's tabs and controls, the pin and the caret that
    /// folds it back into the tab.
    fn band(&mut self, ctx: &egui::Context, screen: Rect, bar_w: f32, bar: impl FnOnce(&mut egui::Ui), now: f64) -> Rect {
        let w = bar_w.min(screen.width() - 100.0).max(320.0);
        // Its full width: the bar, the pin, the caret and the frame's margins.
        let full = w + 2.0 * 26.0 + 2.0 * 4.0 + 12.0;
        let centre = screen.left() + self.x * screen.width();
        let left = (centre - full / 2.0).clamp(screen.left() + 4.0, (screen.right() - full - 4.0).max(screen.left() + 4.0));
        egui::Area::new(egui::Id::unique("full-screen-dock-band"))
            .fixed_pos(Pos2::new(left, screen.top()))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let radius = CornerRadius { nw: 0, ne: 0, sw: crate::gui::theme::R_MD, se: crate::gui::theme::R_MD };
                frame(ui, radius, egui::Margin { left: 6, right: 6, top: 0, bottom: 2 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.allocate_ui_with_layout(Vec2::new(w, BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // The bar lays its right-hand group against the far edge: all of it is the band's.
                            ui.set_min_size(Vec2::new(w, BAR_H));
                            ui.set_clip_rect(ui.max_rect().expand(2.0));
                            bar(ui);
                        });
                        self.pin(ui);
                        let fold = egui::Button::new(RichText::new(ic::CARET_UP).size(14.0)).frame_when_inactive(false).min_size(Vec2::splat(26.0));
                        if ui.add(fold).on_hover_text("Fold the toolbar into the tab (Ctrl+Alt+Home)").clicked() {
                            self.toggle(now);
                        }
                    });
                });
            })
            .response
            .rect
    }

    /// The tab's left edge for its remembered place, kept inside the screen.
    fn left(&self, screen: Rect) -> f32 {
        let centre = screen.left() + self.x * screen.width();
        (centre - self.w / 2.0).clamp(screen.left() + 4.0, (screen.right() - self.w - 4.0).max(screen.left() + 4.0))
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

/// The active pane at a glance, small: a dot coloured by the round trip (for a screen), its name,
/// the round trip, and an eye while it only watches.
fn status_chip(ui: &mut egui::Ui, s: &Status) {
    ui.spacing_mut().item_spacing.x = 4.0;
    match s.rtt {
        Some(rtt) => {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 3.0, rtt_colour(rtt));
        }
        None => {
            ui.label(RichText::new(s.glyph).size(TAB_TEXT));
        }
    }
    // A long name is cut short with an ellipsis.
    ui.scope(|ui| {
        ui.set_max_width(TAB_NAME_W);
        ui.add(egui::Label::new(RichText::new(&s.name).size(TAB_TEXT)).truncate().selectable(false));
    });
    if let Some(rtt) = s.rtt {
        let text = if rtt >= 999.5 { "999+ ms".to_string() } else { format!("{rtt:.0} ms") };
        // A fixed width, so the tab does not jitter as the round trip changes.
        ui.add_sized([38.0, 14.0], egui::Label::new(RichText::new(text).size(TAB_TEXT - 1.0).weak()).selectable(false));
    }
    if s.view_only {
        ui.label(RichText::new(ic::EYE).size(TAB_TEXT).weak());
    }
}

/// Green below 80 ms, amber below 250 ms, red above.
pub(super) fn rtt_colour(rtt: f32) -> Color32 {
    match rtt {
        r if r < 80.0 => super::good(),
        r if r < 250.0 => super::warn(),
        _ => super::bad(),
    }
}
