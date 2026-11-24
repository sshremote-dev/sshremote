//! Keys, the pointer and pasted text, sent to the remote host.

use super::*;

/// Ctrl as the command key and the command key as Ctrl.
fn swapped(keysym: u32) -> u32 {
    match keysym {
        CONTROL_L => SUPER_L,
        xk::CONTROL_R => xk::SUPER_R,
        SUPER_L => CONTROL_L,
        xk::SUPER_R => xk::CONTROL_R,
        k => k,
    }
}

impl Remote {
    /// Moves a picture larger than the window when the pointer rests near an edge.
    pub(in crate::gui) fn edge_pan(&mut self, ctx: &egui::Context, area: Rect) {
        if !matches!(self.fit, Fit::Fixed(_)) {
            return;
        }
        let Some(p) = ctx.pointer_hover_pos().filter(|p| area.contains(*p)) else { return };
        const EDGE: f32 = 28.0;
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        let mut d = Vec2::ZERO;
        if p.x < area.min.x + EDGE {
            d.x = -1.0;
        } else if p.x > area.max.x - EDGE {
            d.x = 1.0;
        }
        if p.y < area.min.y + EDGE {
            d.y = -1.0;
        } else if p.y > area.max.y - EDGE {
            d.y = 1.0;
        }
        if d != Vec2::ZERO {
            self.pan += d * 900.0 * dt;
            ctx.request_repaint();
        }
    }

    pub(in crate::gui) fn remote_point(&self, rect: Rect, pos: Pos2) -> Option<(u16, u16)> {
        if !rect.contains(pos) || self.fb.w == 0 {
            return None;
        }
        let x = ((pos.x - rect.min.x) / rect.width() * self.fb.w as f32) as usize;
        let y = ((pos.y - rect.min.y) / rect.height() * self.fb.h as f32) as usize;
        Some((x.min(self.fb.w - 1) as u16, y.min(self.fb.h - 1) as u16))
    }

    pub(in crate::gui) fn key(&mut self, keysym: u32, down: bool) -> Result<(), String> {
        let keysym = if self.swap_cmd { swapped(keysym) } else { keysym };
        if down {
            self.held.insert(keysym);
        } else if !self.held.remove(&keysym) {
            return Ok(());
        }
        self.send(ToAgent::KeyState { keysym, down })
    }

    /// Taps a key, for the Copy/Cut/Paste shortcuts the window system turns into events.
    pub(in crate::gui) fn tap(&mut self, keysym: u32) -> Result<(), String> {
        self.key(keysym, true)?;
        self.key(keysym, false)
    }

    pub(in crate::gui) fn sync_mods(&mut self, m: Modifiers) -> Result<(), String> {
        // (⌘ on a Mac here; the Win key elsewhere comes as a key of its own.)
        let pairs = [
            (self.mods.shift, m.shift, SHIFT_L),
            (self.mods.ctrl, m.ctrl, CONTROL_L),
            (self.mods.alt, m.alt, ALT_L),
            (self.mods.mac_cmd, m.mac_cmd, SUPER_L),
        ];
        for (was, now, sym) in pairs {
            if was != now {
                self.key(sym, now)?;
            }
        }
        self.mods = m;
        Ok(())
    }

    /// Releases everything held, as when the window loses focus mid-shortcut.
    pub(in crate::gui) fn release_all(&mut self) -> Result<(), String> {
        for k in std::mem::take(&mut self.held) {
            self.send(ToAgent::KeyState { keysym: k, down: false })?;
        }
        self.mods = Modifiers::NONE;
        Ok(())
    }

    /// While its keys go to the host, the picture holds egui's keyboard focus with Tab, the
    /// arrows and Esc locked to it: with nothing focused egui gives Tab to the toolbar's first
    /// button, and every key after it would stay here. A click on the picture takes the focus
    /// back from a field that had it.
    pub(in crate::gui) fn hold_keys(&mut self, ui: &egui::Ui, area: Rect, keys: bool) {
        let id = self.keys_id;
        let ctx = ui.ctx();
        // A dialog over the window (a modal) or a menu keeps the focus for its own fields.
        let keys = keys && !ctx.any_popup_open() && ctx.memory(|m| m.allows_interaction(ui.layer_id()));
        if !keys {
            ctx.memory_mut(|m| m.surrender_focus(id));
            return;
        }
        ui.interact(area, id, egui::Sense::focusable_noninteractive());
        let pressed_here = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten()).is_some_and(|p| {
            area.contains(p) && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background)
        });
        let lock = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
        ctx.memory_mut(|m| {
            if m.focused().is_none() || pressed_here {
                m.request_focus(id);
            }
            m.set_focus_lock_filter(id, lock);
        });
    }

    /// A widget of this window other than the picture has the keyboard (a field being typed in).
    pub(in crate::gui) fn keys_elsewhere(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.focused().is_some_and(|f| f != self.keys_id))
    }

    pub(in crate::gui) fn input(&mut self, ctx: &egui::Context, shown: Option<Rect>, ui_rects: &[Rect]) -> Result<(), String> {
        let (events, focused) = ctx.input(|i| (i.events.clone(), i.focused));
        if !focused {
            self.games_held = false;
            return self.release_all();
        }
        // Pointer events over any of this window's own controls (toolbar, menus, panels) stay here.
        let over_toolbar = |p: Pos2| {
            ui_rects.iter().any(|t| t.contains(p))
                || ctx.layer_id_at(p).is_some_and(|l| l.order != egui::Order::Background)
        };
        // Keys go to the remote unless a toolbar widget (a slider, a menu) or the terminal panel has them.
        let term_keys = self.term_split && self.term_focus && self.terms.is_some() && !self.term_detached;
        let local = self.keys_elsewhere(ctx) || ctx.any_popup_open() || term_keys;
        if let Some(rect) = shown.filter(|_| !local) {
            // Composed text (Korean, Japanese, Chinese input methods) arrives as IME commits.
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Terminal,
                    rect,
                    cursor_rect: Rect::from_min_size(rect.min, Vec2::new(1.0, 16.0)),
                    should_interrupt_composition: false,
                })
            });
        }
        for e in events {
            if self.click_pick && matches!(e, Event::Key { key: Key::Escape, .. }) {
                if matches!(e, Event::Key { pressed: true, .. }) {
                    self.click_pick = false;
                    self.notify("Choosing a window cancelled");
                }
                continue;
            }
            // The mouse for games: its movement goes as it is, never where the pointer is here;
            // Ctrl+Alt lets it go, and a click on the picture holds it again.
            if self.games && !local {
                if matches!(e, Event::Key { pressed: true, .. } | Event::PointerButton { pressed: true, .. } | Event::MouseWheel { .. }) {
                    self.let_go_armed = false;
                }
                match &e {
                    Event::MouseMoved(d) if self.grab_set => {
                        self.moved_by += *d;
                        continue;
                    }
                    Event::PointerMoved(_) if self.grab_set => continue,
                    // Ctrl+Alt pressed and let go with nothing between lets go of the mouse; with a
                    // key between (Ctrl+Alt+Delete, a game's own) the keys are the host's.
                    Event::ModifiersChanged(m) if self.games_held => {
                        if m.ctrl && m.alt {
                            self.let_go_armed = true;
                        } else if std::mem::take(&mut self.let_go_armed) {
                            self.games_held = false;
                            self.notify("The mouse is yours again: click the picture to hold it for the game");
                        }
                    }
                    Event::PointerButton { pos, button, pressed: true, .. } if !self.games_held && shown.is_some_and(|r| r.contains(*pos)) && !over_toolbar(*pos) => {
                        self.games_held = true;
                        self.hold_click = Some(*button);
                        self.notify("The mouse is held for the game: Ctrl+Alt lets it go");
                        continue;
                    }
                    Event::PointerButton { button, pressed: false, .. } if self.hold_click == Some(*button) => {
                        self.hold_click = None;
                        continue;
                    }
                    Event::PointerButton { pos: _, button, pressed, modifiers } if self.grab_set => {
                        self.sync_mods(*modifiers)?;
                        self.send(ToAgent::Button { button: button_number(*button), pressed: *pressed })?;
                        continue;
                    }
                    _ => {}
                }
            }
            match e {
                // Full screen here: the host does not see this one key.
                Event::Key { key: Key::Enter, modifiers, .. } if modifiers.ctrl && modifiers.alt => {}
                // Files alone on the clipboard make no paste event, only the V let go after it.
                Event::Key { key: Key::V, pressed: false, modifiers, .. } if !local && modifiers.command && !std::mem::take(&mut self.paste_seen) => {
                    let files = if self.share_clipboard { crate::clip::files_here() } else { Vec::new() };
                    // A picture alone (a screenshot): onto the host's clipboard, then pasted there;
                    // not one that was here before something was copied there (that is newer).
                    let picture = if self.share_clipboard && files.is_empty() && !self.view_only { crate::clip::image_png() } else { None };
                    let stale = *self.stale_picture.lock().unwrap_or_else(|e| e.into_inner());
                    let picture = picture.filter(|(mark, _)| Some(*mark) != stale);
                    if let Some((mark, png)) = picture {
                        if png.len() > crate::clip::IMAGE_MAX {
                            // Too large to go as a picture: as a file, as files are pasted.
                            if let Some(path) = crate::clip::image_here() {
                                self.paste_files(vec![path])?;
                            }
                        } else {
                            self.send(ToAgent::ClipImage(png))?;
                            self.tap('v' as u32)?;
                        }
                        // Sent: that clipboard holds it now.
                        *self.stale_picture.lock().unwrap_or_else(|e| e.into_inner()) = Some(mark);
                    } else if files.is_empty() {
                        self.key('v' as u32, false)?;
                    } else {
                        self.paste_files(files)?;
                    }
                }
                Event::Key { key, physical_key, pressed, modifiers, .. } if !local => {
                    self.sync_mods(modifiers)?;
                    // A computer gets the keys where they are (its own layout makes the
                    // characters); a web page gets the characters this keyboard makes, as text,
                    // and with Ctrl, Alt or the command key a key as it is labelled here.
                    let command = modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd;
                    let key = if self.web {
                        if !command && keysym(key).is_some_and(|s| (0x20..0x7f).contains(&s)) {
                            continue;
                        }
                        key
                    } else {
                        physical_key.unwrap_or(key)
                    };
                    let Some(sym) = keysym(key) else { continue };
                    if pressed && std::mem::take(&mut self.win_next) {
                        // (Cmd+Space on a Mac is Spotlight's.)
                        let (sym, mods) = if sym == ' ' as u32 && self.sess.backend != "mac" { (SUPER_L, 0) } else { (sym, MOD_SUPER) };
                        self.send(ToAgent::Key { keysym: sym, mods })?;
                        continue;
                    }
                    self.key(sym, pressed)?;
                }
                Event::ModifiersChanged(m) if !local => self.sync_mods(m)?,
                // Shift+Insert's paste is over with the key (no V to let go of).
                Event::Key { key: Key::Insert, pressed: false, .. } => self.paste_seen = false,
                // Clipboard keys reach here as copy, cut and paste: the host gets the keys pressed,
                // Ctrl+C, X and V, or Ctrl+Insert, Shift+Delete and Shift+Insert (with the
                // modifiers already held there), never a bare letter.
                Event::Copy if !local => self.tap(if crate::sys::insert_held() { xk::INSERT } else { 'c' as u32 })?,
                Event::Cut if !local => self.tap(if self.mods.shift && !self.mods.ctrl { xk::DELETE } else { 'x' as u32 })?,
                Event::Paste(text) if !local => {
                    self.paste_seen = true;
                    // Files copied in a file manager here are sent over and pasted there as files.
                    let files = if self.share_clipboard { crate::clip::files_here() } else { Vec::new() };
                    if !files.is_empty() {
                        self.paste_files(files)?;
                        continue;
                    }
                    // What was copied here a moment ago may not have gone over yet (the clipboard
                    // is looked at twice a second): it goes first, then the keys that paste it.
                    if self.share_clipboard && !text.is_empty() {
                        self.send(ToAgent::Clipboard(text))?;
                    }
                    self.tap(if self.mods.shift && !self.mods.ctrl { xk::INSERT } else { 'v' as u32 })?
                }
                Event::Text(text) if !local && self.web => {
                    for c in text.chars() {
                        self.send(ToAgent::Key { keysym: xk::unicode(c as u32), mods: 0 })?;
                    }
                }
                Event::Ime(egui::ImeEvent::Commit(text)) if !local => {
                    for c in text.chars() {
                        let sym = xk::of_char(c);
                        self.send(ToAgent::Key { keysym: sym, mods: 0 })?;
                    }
                }
                Event::PointerMoved(pos) if !over_toolbar(pos) => {
                    if let Some(p) = shown.and_then(|r| self.remote_point(r, pos))
                        && p != self.pointer
                    {
                        self.pointer = p;
                        // The host does not echo a move back: its pointer is where it was sent.
                        self.host_pointer = Some((p.0 as i16, p.1 as i16));
                        self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                    }
                }
                Event::PointerButton { pos, button, pressed, modifiers } => {
                    if pressed && (over_toolbar(pos) || local || super::chrome::on_edge(ctx, pos)) {
                        continue;
                    }
                    // Shift+click, Ctrl+click: the modifiers held here are held there for the click.
                    if !local {
                        self.sync_mods(modifiers)?;
                    }
                    let Some(p) = shown.and_then(|r| self.remote_point(r, pos)) else { continue };
                    if p != self.pointer {
                        self.pointer = p;
                        self.host_pointer = Some((p.0 as i16, p.1 as i16));
                        self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                    }
                    let b = button_number(button);
                    self.send(ToAgent::Button { button: b, pressed })?;
                    if self.click_pick && b == 1 && pressed {
                        self.pick_pressed = true;
                    }
                    if self.click_pick && self.pick_pressed && b == 1 && !pressed {
                        self.pick_pressed = false;
                        // Once the window has come to the front, it becomes what is shared.
                        self.click_pick = false;
                        self.pick_at = Some(Instant::now() + Duration::from_millis(400));
                    }
                }
                Event::MouseWheel { unit, delta, modifiers, .. } => {
                    let Some(pos) = ctx.pointer_hover_pos() else { continue };
                    if over_toolbar(pos) || !shown.is_some_and(|r| r.contains(pos)) {
                        continue;
                    }
                    // Ctrl+wheel zooms there, Shift+wheel scrolls sideways there.
                    if !local {
                        self.sync_mods(modifiers)?;
                    }
                    let lines = match unit {
                        egui::MouseWheelUnit::Point => delta / 40.0,
                        egui::MouseWheelUnit::Line => delta,
                        egui::MouseWheelUnit::Page => delta * 10.0,
                    };
                    // Whole steps go now, the rest adds up with what comes next.
                    self.wheel -= lines;
                    let (sy, sx) = (self.wheel.y.trunc() as i32, self.wheel.x.trunc() as i32);
                    self.wheel -= Vec2::new(sx as f32, sy as f32);
                    if sy != 0 {
                        self.send(ToAgent::Scroll { steps: sy.clamp(-10, 10) as i8 })?;
                    }
                    if sx != 0 {
                        self.send(ToAgent::HScroll { steps: sx.clamp(-10, 10) as i8 })?;
                    }
                }
                _ => {}
            }
        }
        // The movement in whole pixels, the rest added to the next.
        let (dx, dy) = (self.moved_by.x.trunc(), self.moved_by.y.trunc());
        if dx != 0.0 || dy != 0.0 {
            self.moved_by -= Vec2::new(dx, dy);
            self.send(ToAgent::PointerBy { dx: dx.clamp(-32768.0, 32767.0) as i16, dy: dy.clamp(-32768.0, 32767.0) as i16 })?;
        }
        Ok(())
    }

    /// Whether the pointer is held for the game this frame (`shown`: the picture, when its keys
    /// go to the host): hidden, where only its movement counts, and put back in the middle where
    /// the window cannot lock it in place. Not while a menu, or a field of this window, is in use.
    /// The window itself holds it (`pointer_grab`), for the pane in front only, so a pane that
    /// goes (its connection lost, closed, another tab shown) lets it go with it.
    pub(in crate::gui) fn hold_pointer(&mut self, ctx: &egui::Context, shown: Option<Rect>) -> Result<(), String> {
        let hold = self.games && self.games_held && shown.is_some() && !ctx.any_popup_open() && !self.keys_elsewhere(ctx);
        if !hold {
            self.games_held &= self.games;
            self.moved_by = Vec2::ZERO;
        }
        self.grab_set = hold;
        if let Some(r) = shown.filter(|_| hold) {
            ctx.set_cursor_icon(egui::CursorIcon::None);
            // Kept from the window's edges, where it would stop.
            if !pointer_locks() && ctx.input(|i| i.pointer.latest_pos()).is_none_or(|p| (p - r.center()).length() > r.size().min_elem() / 4.0) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorPosition(r.center()));
            }
        }
        Ok(())
    }

    /// The pointer is held for the game.
    pub(in crate::gui) fn holds_pointer(&self) -> bool {
        self.grab_set
    }

    /// The mouse for games on and holding, as a click on the picture would leave it.
    #[cfg(test)]
    pub(in crate::gui) fn hold_for_game(&mut self) {
        (self.games, self.games_held) = (true, true);
    }

    /// Text pasted into the host from an extension or the command palette: a screen gets it on
    /// its clipboard and then Ctrl+V; a terminal types it.
    pub(in crate::gui) fn paste_text(&mut self, ctx: &egui::Context, text: String) -> Result<(), String> {
        use super::workspace::PaneMode;
        match self.mode() {
            PaneMode::Screen if !self.view_only => {
                self.send(ToAgent::Clipboard(text))?;
                self.release_all()?;
                self.send(ToAgent::Key { keysym: 'v' as u32, mods: crate::proto::MOD_CTRL })
            }
            PaneMode::Screen => {
                self.notify("View only: nothing was pasted");
                Ok(())
            }
            // The terminal takes it as a paste from the keyboard would.
            PaneMode::Terminal => {
                ctx.input_mut(|i| i.events.push(egui::Event::Paste(text)));
                Ok(())
            }
            PaneMode::Files => {
                self.notify("Nothing to paste into here");
                Ok(())
            }
        }
    }
}

/// The host's number for a mouse button.
fn button_number(b: egui::PointerButton) -> u8 {
    match b {
        egui::PointerButton::Primary => 1,
        egui::PointerButton::Middle => 2,
        egui::PointerButton::Secondary => 3,
        egui::PointerButton::Extra1 => 8,
        egui::PointerButton::Extra2 => 9,
    }
}

/// Whether the window can lock the pointer in place (Wayland); elsewhere it is kept inside it.
fn pointer_locks() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var("WINIT_UNIX_BACKEND").ok().is_none_or(|b| b != "x11")
}

/// The window's hold on the pointer for a game, or none.
pub(in crate::gui) fn pointer_grab(ctx: &egui::Context, hold: bool) {
    let grab = match (hold, pointer_locks()) {
        (false, _) => egui::CursorGrab::None,
        (true, true) => egui::CursorGrab::Locked,
        (true, false) => egui::CursorGrab::Confined,
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(grab));
    ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!hold));
}
