//! Notices stacked at the bottom right of an area (or where no program's own window hides them),
//! which fade out after a few seconds.

use super::*;

/// How long a notice stays, the last second of it fading.
const STAY: Duration = Duration::from_secs(6);
/// The most shown at once; an older one gives way.
const MOST: usize = 4;
/// How wide a notice's lines may be, and its room from the area's edges and the next one.
const WRAP: f32 = 380.0;
const MARGIN: f32 = 16.0;
const GAP: f32 = 8.0;

#[derive(Default)]
pub(super) struct Toasts {
    /// Text, whether it is a warning, and when it came.
    list: Vec<(String, bool, Instant)>,
}

impl Toasts {
    /// Adds a notice; one that says something failed is marked as a warning.
    pub(super) fn push(&mut self, text: &str) {
        let l = text.to_lowercase();
        let warn =
            ["fail", "error", "unavailable", "cannot", "could not", "denied", "sound:"].iter().any(|w| l.contains(w));
        // The same notice again only comes back to the front.
        self.list.retain(|t| t.0 != text);
        self.list.push((text.to_string(), warn, Instant::now()));
        if self.list.len() > MOST {
            self.list.remove(0);
        }
    }

    /// Draws them over `area`, newest at the bottom, at its bottom right; or at another of its
    /// corners where none is under a program's own window held over a pane (`held`, which would
    /// hide them). With all four under one, the newest shows over the title bar (`bar`) instead;
    /// in full screen, at the top of `area`, where the program steps aside for it as for the dock.
    /// A click on one puts it away.
    pub(super) fn draw(&mut self, ctx: &egui::Context, area: Rect, held: &[Rect], bar: Option<Rect>) {
        self.list.retain(|t| t.2.elapsed() < STAY);
        if self.list.is_empty() {
            return;
        }
        let font = FontId::proportional(14.0);
        let measure = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("toast-measure")));
        let size = |text: &str, wrap: f32| measure.layout(text.to_string(), font.clone(), Color32::WHITE, wrap).size() + Vec2::new(30.0, 20.0);
        let sizes: Vec<Vec2> = self.list.iter().map(|t| size(&t.0, WRAP)).collect();
        let w = sizes.iter().map(|s| s.x).fold(0.0, f32::max);
        let h = sizes.iter().map(|s| s.y).sum::<f32>() + GAP * (sizes.len() - 1) as f32;
        // (right, bottom) for each corner, the bottom right first.
        let stack = |right: bool, bottom: bool| {
            let x = if right { area.right() - MARGIN - w } else { area.left() + MARGIN };
            let y = if bottom { area.bottom() - MARGIN - h } else { area.top() + MARGIN };
            Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
        };
        let free = [(true, true), (false, true), (true, false), (false, false)].into_iter().find(|(r, b)| !held.iter().any(|p| p.intersects(stack(*r, *b))));
        // Each notice's place and how wide its lines may be, newest first.
        let mut places: Vec<(usize, Rect, f32)> = Vec::new();
        match (free, bar) {
            (Some((right, bottom)), _) => {
                let mut y = if bottom { area.bottom() - MARGIN } else { area.top() + MARGIN };
                for (i, s) in sizes.iter().enumerate().rev() {
                    let x = if right { area.right() - MARGIN - s.x } else { area.left() + MARGIN };
                    let rect = Rect::from_min_size(Pos2::new(x, if bottom { y - s.y } else { y }), *s);
                    y = if bottom { rect.top() - GAP } else { rect.bottom() + GAP };
                    places.push((i, rect, WRAP));
                }
            }
            (None, Some(bar)) => {
                let i = self.list.len() - 1;
                let wrap = (bar.width() - 2.0 * MARGIN - 30.0).clamp(WRAP, 640.0);
                places.push((i, Rect::from_center_size(bar.center(), size(&self.list[i].0, wrap)), wrap));
            }
            (None, None) => {
                let mut y = area.top() + 48.0;
                for (i, s) in sizes.iter().enumerate().rev() {
                    let rect = Rect::from_min_size(Pos2::new(area.center().x - s.x / 2.0, y), *s);
                    y = rect.bottom() + GAP;
                    places.push((i, rect, WRAP));
                }
            }
        }
        let mut gone = None;
        for (i, rect, wrap) in places {
            let (text, warn, at) = &self.list[i];
            let fade = (1.0 - (at.elapsed().as_secs_f32() - (STAY.as_secs_f32() - 1.0)).max(0.0)).clamp(0.0, 1.0);
            let clicked = egui::Area::new(egui::Id::unique(("toast", i)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .fade_in(false)
                .show(ctx, |ui| {
                    let (rect, resp) = ui.allocate_exact_size(rect.size(), egui::Sense::click());
                    let painter = ui.painter();
                    painter.rect_filled(rect, CornerRadius::same(theme::R_MD), Color32::from_rgba_unmultiplied(32, 34, 41, (235.0 * fade) as u8));
                    let side = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
                    painter.rect_filled(side, CornerRadius::same(2), (if *warn { super::warn() } else { ACCENT }).gamma_multiply(fade));
                    let galley = painter.layout(text.clone(), font.clone(), Color32::WHITE, wrap);
                    painter.galley(rect.min + Vec2::new(18.0, 10.0), galley, Color32::WHITE.gamma_multiply(fade));
                    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                })
                .inner;
            if clicked {
                gone = Some(i);
            }
        }
        if let Some(i) = gone {
            self.list.remove(i);
            ctx.request_repaint();
        }
        if let Some(first) = self.list.iter().map(|t| t.2).min() {
            // Steady until the last second, which fades.
            let until_fade = (STAY - Duration::from_secs(1)).saturating_sub(first.elapsed());
            ctx.request_repaint_after(if until_fade.is_zero() { Duration::from_millis(50) } else { until_fade });
        }
    }

    #[cfg(test)]
    pub(super) fn texts(&self) -> Vec<&str> {
        self.list.iter().map(|t| t.0.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::snapshot::RunTest;

    const SCREEN: Vec2 = Vec2::new(1000.0, 700.0);

    fn frame(ctx: &egui::Context, t: &mut Toasts, held: &[Rect], bar: Option<Rect>, events: Vec<egui::Event>) {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)), events, ..Default::default() };
        let area = Rect::from_min_max(Pos2::new(0.0, 40.0), SCREEN.to_pos2());
        let _ = ctx.run_test(input, |ui| t.draw(ui.ctx(), area, held, bar));
    }

    /// Two frames: a new area is measured, unseen, in its first.
    fn frames(ctx: &egui::Context, t: &mut Toasts, held: &[Rect], bar: Option<Rect>) {
        frame(ctx, t, held, bar, Vec::new());
        frame(ctx, t, held, bar, Vec::new());
    }

    /// Where the notice drawn `i`th from the newest is.
    fn place(ctx: &egui::Context, i: usize) -> Rect {
        ctx.memory(|m| m.area_rect(egui::Id::unique(("toast", i)))).unwrap()
    }

    #[test]
    fn notices_go_where_no_held_program_hides_them() {
        let ctx = egui::Context::default();
        let bar = Rect::from_min_max(Pos2::ZERO, Pos2::new(SCREEN.x, 40.0));
        let mut t = Toasts::default();
        t.push("python3 did not close, so this window stays open.");
        frames(&ctx, &mut t, &[], Some(bar));
        let r = place(&ctx, 0);
        assert!((r.right() - 984.0).abs() < 1.0 && (r.bottom() - 684.0).abs() < 1.0, "bottom right: {r:?}");
        // A program held over the right half: the bottom left.
        let right = Rect::from_min_max(Pos2::new(500.0, 40.0), SCREEN.to_pos2());
        frames(&ctx, &mut t, &[right], Some(bar));
        let r = place(&ctx, 0);
        assert!((r.left() - 16.0).abs() < 1.0 && (r.bottom() - 684.0).abs() < 1.0, "bottom left: {r:?}");
        // Over the whole area: over the title bar, in one line.
        let whole = Rect::from_min_max(Pos2::new(0.0, 40.0), SCREEN.to_pos2());
        frames(&ctx, &mut t, &[whole], Some(bar));
        let r = place(&ctx, 0);
        assert!(bar.contains_rect(r) && (r.center().x - 500.0).abs() < 1.0, "in the title bar: {r:?}");
        // In full screen, no title bar: at the top of the area.
        frames(&ctx, &mut t, &[whole], None);
        let r = place(&ctx, 0);
        assert!((r.top() - 88.0).abs() < 1.0 && (r.center().x - 500.0).abs() < 1.0, "at the top: {r:?}");
        // A click puts it away.
        frames(&ctx, &mut t, &[whole], Some(bar));
        let p = place(&ctx, 0).center();
        let button = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&ctx, &mut t, &[whole], Some(bar), vec![egui::Event::PointerMoved(p), button(true)]);
        frame(&ctx, &mut t, &[whole], Some(bar), vec![button(false)]);
        assert!(t.texts().is_empty());
    }
}
