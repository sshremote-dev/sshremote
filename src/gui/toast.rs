//! Notices stacked at the bottom right of an area, which fade out after a few seconds.

use super::*;

/// How long a notice stays, the last second of it fading.
const STAY: Duration = Duration::from_secs(6);
/// The most shown at once; an older one gives way.
const MOST: usize = 4;

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

    /// Draws them over `area`, newest at the bottom.
    pub(super) fn draw(&mut self, ctx: &egui::Context, area: Rect) {
        self.list.retain(|t| t.2.elapsed() < STAY);
        let mut y = area.bottom() - 16.0;
        for (i, (text, warn, at)) in self.list.iter().enumerate().rev() {
            let fade = (1.0 - (at.elapsed().as_secs_f32() - (STAY.as_secs_f32() - 1.0)).max(0.0)).clamp(0.0, 1.0);
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(("toast", i))));
            let galley = painter.layout(text.clone(), FontId::proportional(14.0), Color32::WHITE, 380.0);
            let size = galley.size() + Vec2::new(30.0, 20.0);
            let rect = Rect::from_min_size(Pos2::new(area.right() - 16.0 - size.x, y - size.y), size);
            painter.rect_filled(
                rect,
                CornerRadius::same(theme::R_MD),
                Color32::from_rgba_unmultiplied(32, 34, 41, (235.0 * fade) as u8),
            );
            let bar = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
            painter.rect_filled(bar, CornerRadius::same(2), (if *warn { super::warn() } else { ACCENT }).gamma_multiply(fade));
            painter.galley(rect.min + Vec2::new(18.0, 10.0), galley, Color32::WHITE.gamma_multiply(fade));
            y = rect.top() - 8.0;
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
