//! Keys, the pointer and pasted text, sent to the remote host.

use super::*;

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
        let pairs =
            [(self.mods.shift, m.shift, SHIFT_L), (self.mods.ctrl, m.ctrl, CONTROL_L), (self.mods.alt, m.alt, ALT_L)];
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

    pub(in crate::gui) fn input(&mut self, ctx: &egui::Context, shown: Option<Rect>, ui_rects: &[Rect]) -> Result<(), String> {
        let (events, focused) = ctx.input(|i| (i.events.clone(), i.focused));
        if !focused {
            return self.release_all();
        }
        // Pointer events over any of this window's own controls (toolbar, menus, panels) stay here.
        let over_toolbar = |p: Pos2| {
            ui_rects.iter().any(|t| t.contains(p))
                || ctx.layer_id_at(p).is_some_and(|l| l.order != egui::Order::Background)
        };
        // Keys go to the remote unless a toolbar widget (a slider, a menu) or the terminal panel has them.
        let term_keys = self.term_split && self.term_focus && self.terms.is_some() && !self.term_detached;
        let local = ctx.memory(|m| m.focused().is_some()) || ctx.any_popup_open() || term_keys;
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
            match e {
                // Full screen here: the host does not see this one key.
                Event::Key { key: Key::Enter, modifiers, .. } if modifiers.ctrl && modifiers.alt => {}
                // Files alone on the clipboard make no paste event, only the V let go after it.
                Event::Key { key: Key::V, pressed: false, modifiers, .. } if !local && modifiers.command && !std::mem::take(&mut self.paste_seen) => {
                    let files = if self.share_clipboard { crate::clip::files_here() } else { Vec::new() };
                    // A picture alone (a screenshot): onto the host's clipboard, then pasted there.
                    let picture = if self.share_clipboard && files.is_empty() && !self.view_only { crate::clip::image_png() } else { None };
                    if let Some(png) = picture {
                        self.send(ToAgent::ClipImage(png))?;
                        self.tap('v' as u32)?;
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
                        let (sym, mods) = if sym == ' ' as u32 { (SUPER_L, 0) } else { (sym, MOD_SUPER) };
                        self.send(ToAgent::Key { keysym: sym, mods })?;
                        continue;
                    }
                    self.key(sym, pressed)?;
                }
                Event::ModifiersChanged(m) if !local => self.sync_mods(m)?,
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
                        self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                    }
                    let b = match button {
                        egui::PointerButton::Primary => 1,
                        egui::PointerButton::Middle => 2,
                        egui::PointerButton::Secondary => 3,
                        egui::PointerButton::Extra1 => 8,
                        egui::PointerButton::Extra2 => 9,
                    };
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
        Ok(())
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
