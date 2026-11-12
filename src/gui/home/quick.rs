//! Quick open, over a workspace (its `+`, or Ctrl+T): the home window's address field and
//! computers, and what is chosen opens in that workspace, where it was asked for.

use super::*;

impl App {
    /// Quick open over workspace `w`, when it asks for it.
    pub(in crate::gui) fn quick_open(&mut self, ctx: &egui::Context, w: &mut workspace::Workspace) {
        // Opened just now (in this window): the field takes the keyboard, so typing starts at once.
        let shown = egui::Id::unique("quick-open-shown").with(ctx.viewport_id());
        let Some(place) = w.picker else {
            ctx.data_mut(|d| d.remove::<bool>(shown));
            return;
        };
        if ctx.data_mut(|d| d.get_temp::<bool>(shown).is_none()) {
            ctx.data_mut(|d| d.insert_temp(shown, true));
            self.focus_host = true;
            self.picked = None;
        }
        // What is opened from here, a tool's terminal too, opens in this workspace.
        let before = self.queued.len();
        let (modal, fit) = modal(ctx, "quick-open", Vec2::new(640.0, 540.0));
        let mut go = None;
        let modal = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.set_height(fit.y);
            let close = ui
                .horizontal(|ui| {
                    ui.label(RichText::new("Open").size(16.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.add(tool_button(ic::X, false)).on_hover_text("Close (Esc)").clicked())
                        .inner
                })
                .inner;
            ui.add_space(6.0);
            self.error_banner(ui);
            self.going_out(ui, &mut go);
            close
        });
        // Esc closes it once the field is empty (the field takes the first Esc to clear itself).
        let shut = modal.should_close() || modal.inner;
        if let Some(c) = go {
            self.choose(c, place);
        }
        self.computer_sheet(ctx);
        // Opened here, not in the main window: an address typed wrong keeps it open with the error.
        for (launch, place, start) in self.queued.drain(before.min(self.queued.len())..) {
            w.set_preset(self.form.preset);
            w.add(launch, place, start);
            w.picker = None;
        }
        if shut {
            w.picker = None;
        }
    }
}
