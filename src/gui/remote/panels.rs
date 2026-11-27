//! The chat and terminal panels beside a remote screen.

use super::*;

impl Remote {
    /// The chat panel; returns its rectangle while open.
    /// Opens the terminal panel under the picture (with the keyboard) and the chat beside it.
    pub(in crate::gui) fn open_panels(&mut self, terminal: bool, chat: bool) {
        if terminal {
            self.terms.get_or_insert_with(|| crate::termwin::TermWindow::new(self.ssh_cmd.clone()));
            (self.term_split, self.term_focus) = (true, true);
        }
        if chat && self.chat.is_none() {
            self.chat = Some((std::mem::take(&mut self.chat_lines), String::new()));
            self.unread = 0;
        }
    }

    /// The area beside the chat panel, and the panel's own, with the splitter between to drag.
    pub(super) fn chat_split(&mut self, ui: &mut egui::Ui, area: Rect) -> (Rect, Option<Rect>) {
        if self.chat.is_none() {
            return (area, None);
        }
        // At most six tenths of the pane, so the picture keeps the rest however narrow it is.
        let most = area.width() * 0.6;
        let w = self.chat_w.clamp(260.0_f32.min(most), most.max(1.0));
        let panel = area.with_min_x(area.right() - w);
        let handle = Rect::from_min_max(Pos2::new(panel.left() - 3.0, area.top()), Pos2::new(panel.left() + 3.0, area.bottom()));
        let r = ui.interact(handle, ui.scope_id().with("chat-splitter"), egui::Sense::drag());
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        self.chat_w = w - r.drag_delta().x;
        (area.with_max_x(panel.left()), Some(panel))
    }

    /// The picture above, the terminal panel below, and the splitter between them to drag.
    pub(super) fn term_split_at(&mut self, ui: &mut egui::Ui, area: Rect) -> (Rect, Option<Rect>) {
        let h = self.term_h.clamp(120.0, (area.height() - 120.0).max(120.0));
        let panel = area.with_min_y(area.bottom() - h);
        let handle = Rect::from_min_max(Pos2::new(area.left(), panel.top() - 3.0), Pos2::new(area.right(), panel.top() + 3.0));
        let r = ui.interact(handle, ui.scope_id().with("terminal-splitter"), egui::Sense::drag());
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        self.term_h = h - r.drag_delta().y;
        (area.with_max_y(panel.top()), Some(panel))
    }

    /// The terminal under the picture: a header to hide it, fill the window with it or move it to a
    /// window of its own, then the shells.
    pub(super) fn terminal_panel(&mut self, ui: &mut egui::Ui, area: Rect) -> Result<(), String> {
        let fill = ui.visuals().panel_fill;
        ui.painter().rect_filled(area, CornerRadius::ZERO, fill);
        let line = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().hline(area.x_range(), area.top(), line);
        let head = Rect::from_min_size(area.min, Vec2::new(area.width(), 28.0));
        let mut what = None;
        let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(head.shrink2(Vec2::new(10.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        bar.label(RichText::new(t!("TERMINAL")).small().strong());
        bar.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let small = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                ui.add(egui::Button::new(RichText::new(glyph).size(14.0)).frame_when_inactive(false).min_size(Vec2::splat(24.0)))
                    .on_hover_text(tip)
                    .clicked()
            };
            if small(ui, ic::CARET_DOWN, t!("Hide the panel (the shells keep running)")) {
                what = Some('h');
            }
            if small(ui, ic::ARROW_SQUARE_OUT, t!("Move the terminal to a window of its own")) {
                what = Some('w');
            }
            if small(ui, ic::CORNERS_OUT, t!("Fill the window with the terminal (the remote screen waits behind it)")) {
                what = Some('f');
            }
        });
        match what {
            Some('h') => self.term_split = false,
            Some('w') => (self.term_split, self.term_detached) = (false, true),
            Some('f') => (self.term_split, self.term_inline) = (false, true),
            _ => {}
        }
        if what.is_some() {
            self.term_focus = false;
            return self.release_all();
        }
        let body = area.with_min_y(head.bottom());
        let Some(mut t) = self.terms.take() else { return Ok(()) };
        t.keys_on = self.term_focus;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("terminal-panel"));
        let cmds = t.embedded(&mut child);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Upload { tab, paths } => self.term_upload(tab, paths)?,
                crate::termwin::Cmd::CancelUpload { tab } => self.term_upload_cancel(tab)?,
                crate::termwin::Cmd::Close => {
                    self.terms = None;
                    self.term_split = false;
                    self.term_focus = false;
                }
            }
        }
        Ok(())
    }

    /// The chat docked beside the picture: earlier messages with this host, then this session's.
    pub(in crate::gui) fn chat_panel(&mut self, ui: &mut egui::Ui, area: Rect) -> Result<Option<Rect>, String> {
        let host = self.host().to_string();
        let peer = self.peer.get();
        let earlier = self.chat_earlier;
        let Some((lines, input)) = self.chat.as_mut() else { return Ok(None) };
        let mut sent = None;
        let (mut close, mut clear) = (false, false);
        let fill = ui.visuals().panel_fill;
        ui.painter().rect_filled(area, CornerRadius::ZERO, fill);
        ui.painter().vline(area.left(), area.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
        let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(area.shrink2(Vec2::new(12.0, 8.0))).id_salt("chat-panel"));
        panel.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  {}", ic::CHAT, t!("Chat"))).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let small = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                    ui.add(egui::Button::new(RichText::new(glyph).size(14.0)).frame_when_inactive(false).min_size(Vec2::splat(24.0)))
                        .on_hover_text(tip)
                        .clicked()
                };
                close = small(ui, ic::X, t!("Close the chat (the conversation is kept)"));
                clear = earlier + lines.len() > 0 && small(ui, ic::TRASH, t!("Clear the history kept with this host"));
            });
        });
        panel.label(RichText::new(t!("With the person at the SSH host's screen; a window opens there.")).small().weak());
        let h = (panel.available_height() - 52.0).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("chat-lines")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(h)
            .show(&mut panel, |ui| crate::chat::show_lines_since(ui, lines, earlier, &peer, t!("You")));
        panel.add_space(4.0);
        sent = crate::chat::input_row(&mut panel, input, true).or(sent);
        if clear {
            crate::chat::clear_history(&host);
            lines.clear();
            self.chat_earlier = 0;
        }
        if let Some(text) = sent {
            if let Some((lines, _)) = self.chat.as_mut() {
                lines.push((false, text.clone(), crate::chat::now()));
            }
            crate::chat::append_history(&host, false, &text);
            self.send(ToAgent::Chat(text))?;
        }
        if close && let Some((lines, _)) = self.chat.take() {
            self.chat_lines = lines;
        }
        Ok(Some(area))
    }

    /// The terminal in this window, with a bar to go back to the screen or give it its own window.
    pub(in crate::gui) fn inline_terminal(&mut self, ui: &mut egui::Ui, area: Rect) -> Result<(), String> {
        let Some(mut t) = self.terms.take() else { return Ok(()) };
        t.keys_on = self.active;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).id_salt("inline-terminal"));
        let cmds = t.embedded(&mut child);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Upload { tab, paths } => self.term_upload(tab, paths)?,
                crate::termwin::Cmd::CancelUpload { tab } => self.term_upload_cancel(tab)?,
                crate::termwin::Cmd::Close => {
                    self.terms = None;
                    self.term_inline = false;
                    // A pane that was the shell goes with it, as a plain shell's pane does.
                    self.pane_closed = true;
                }
            }
        }
        Ok(())
    }

    pub(in crate::gui) fn term_window(&mut self, ctx: &egui::Context) -> Result<(), String> {
        if !self.term_detached {
            return Ok(());
        }
        let Some(mut t) = self.terms.take() else { return Ok(()) };
        t.keys_on = true;
        let host = self.host().to_string();
        let cmds = t.show(ctx, &host);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Upload { tab, paths } => self.term_upload(tab, paths)?,
                crate::termwin::Cmd::CancelUpload { tab } => self.term_upload_cancel(tab)?,
                crate::termwin::Cmd::Close => {
                    self.terms = None;
                    self.term_detached = false;
                }
            }
        }
        Ok(())
    }
}
