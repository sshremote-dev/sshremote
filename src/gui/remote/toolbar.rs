//! The remote pane's toolbar and its share picker.

use super::*;

impl Remote {
    /// Chooses what the remote host shares, as in a video call: a screen, a window or a program.
    pub(in crate::gui) fn share_picker(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some((list, tab)) = self.picker.as_mut() else { return Ok(()) };
        let mut pick: Option<ToAgent> = None;
        let mut close = false;
        let mut click_pick = false;
        let monitors = self.monitors.clone();
        let current = self.monitor;
        let (modal, fit) = crate::gui::modal(ctx, "sshremote-share-picker", Vec2::new(560.0, 440.0));
        let modal = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.set_max_height(fit.y);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Choose what to share").size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon(ui, ic::X, "Close", false).clicked() {
                        close = true;
                    }
                });
            });
            ui.label(RichText::new("The SSH host sends only what you choose; clicks and keys reach only that.").weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (i, (g, name)) in
                    [(ic::MONITOR, "Entire screen"), (ic::APP_WINDOW, "Window"), (ic::SQUARES_FOUR, "Application")]
                        .into_iter()
                        .enumerate()
                {
                    if ui.selectable_label(*tab == i as u8, format!("{g}  {name}")).clicked() {
                        *tab = i as u8;
                    }
                }
            });
            ui.separator();
            let row = |ui: &mut egui::Ui, glyph: &str, title: &str, sub: &str, on: bool| -> bool {
                let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), egui::Sense::click());
                if on {
                    ui.painter().rect_stroke(rect, CornerRadius::same(crate::gui::theme::R_SM), egui::Stroke::new(1.0, super::selected_stroke()), egui::StrokeKind::Inside);
                }
                let fill = if on {
                    super::selected_fill()
                } else if r.hovered() {
                    ui.visuals().widgets.hovered.bg_fill
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, CornerRadius::same(crate::gui::theme::R_MD), fill);
                let text = ui.visuals().text_color();
                let weak = ui.visuals().weak_text_color();
                ui.painter().text(
                    rect.left_center() + Vec2::new(14.0, 0.0),
                    Align2::LEFT_CENTER,
                    glyph,
                    FontId::proportional(20.0),
                    text,
                );
                let galley =
                    ui.painter().layout(title.to_string(), FontId::proportional(14.0), text, rect.width() - 70.0);
                ui.painter().galley(rect.min + Vec2::new(46.0, 5.0), galley, text);
                ui.painter().text(
                    rect.min + Vec2::new(46.0, 25.0),
                    Align2::LEFT_TOP,
                    sub,
                    FontId::proportional(11.5),
                    weak,
                );
                r.clicked()
            };
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((fit.y - 110.0).clamp(80.0, 330.0)).show(ui, |ui| match *tab {
                0 => {
                    let screens: Vec<(String, String)> = if monitors.is_empty() {
                        vec![(String::new(), "The only screen".into())]
                    } else {
                        monitors
                            .iter()
                            .map(|m| {
                                (m.name.clone(), format!("{}x{}{}", m.w, m.h, if m.primary { ", primary" } else { "" }))
                            })
                            .collect()
                    };
                    for (i, (name, sub)) in screens.iter().enumerate() {
                        let on = self.sharing.0 == 0 && i == current;
                        if row(ui, ic::MONITOR, format!("Screen {}  {name}", i + 1).trim(), sub, on) {
                            pick = Some(if i == current || monitors.len() < 2 {
                                ToAgent::Share(Share::Screen)
                            } else {
                                ToAgent::SelectMonitor(i as u8)
                            });
                        }
                    }
                }
                t => {
                    // The window in front, or its program: no list needed.
                    let no_helper = matches!(list, Some(Err(e)) if e.starts_with(crate::capture::NEEDS_HELPER));
                    if t == 1 {
                        let sub = "The window in front on the SSH host; click it there first";
                        if row(ui, ic::APP_WINDOW, "Active window", sub, false) {
                            pick = Some(ToAgent::Share(Share::ActiveWindow));
                        }
                        // Pointing at the window itself: the click brings it to the front, then it is shared.
                        let sub = "Click the window you want in the picture of the SSH host";
                        if row(ui, ic::CURSOR_CLICK, "Click a window to share", sub, false) {
                            click_pick = true;
                        }
                    } else if no_helper {
                        ui.add_enabled_ui(false, |ui| {
                            row(
                                ui,
                                ic::SQUARES_FOUR,
                                "Active application",
                                "Needs the sshremote helper on GNOME",
                                false,
                            )
                        });
                    } else {
                        let sub = "Every window of the program in front on the SSH host";
                        if row(ui, ic::SQUARES_FOUR, "Active application", sub, false) {
                            pick = Some(ToAgent::Share(Share::ActiveApp));
                        }
                    }
                    ui.separator();
                    share_list(ui, list, t, &row, &mut pick);
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if click_pick {
            self.click_pick = true;
            self.pick_pressed = false;
            close = true;
            if self.sharing.0 != 0 {
                // The whole screen first, so every window can be seen and clicked.
                self.send(ToAgent::Share(Share::Screen))?;
            }
        }
        if let Some(m) = pick {
            // The chooser stays open for the install's answer.
            let installing = m == ToAgent::InstallHelper;
            self.send(m)?;
            close = !installing;
            if installing && let Some(p) = self.picker.as_mut() {
                p.0 = None;
            }
        }
        if close {
            self.picker = None;
        }
        Ok(())
    }

    /// The pane's controls in the workspace's title bar (or its full-screen dock): the screen's
    /// display, sharing, keys, terminal, chat, screenshot, pause and options.
    pub(in crate::gui) fn toolbar_items(&mut self, ui: &mut egui::Ui, action: &mut Option<Deferred>) {
        let screen = self.screen_controls();
        // A pane showing its shell: the shell's own controls first.
        if self.mode() == super::workspace::PaneMode::Terminal
            && let Some(t) = self.terms.as_mut()
        {
            t.controls(ui);
            ui.separator();
        }
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
        // Input off: said here, one click turns it back on.
        if self.view_only && ui.button(RichText::new(format!("{}  View only  {}", ic::EYE, ic::X)).small()).on_hover_text("Mouse and keyboard do not reach the host. Click to control it again").clicked() {
            self.view_only = false;
        }
        if screen {
        if self.sharing.0 != 0 {
            let name: String = self.sharing.1.chars().take(18).collect();
            let chip = egui::Button::new(RichText::new(format!("{}  {name}  {}", ic::APP_WINDOW, ic::X)).small())
                .fill(super::selected_fill())
                .stroke(egui::Stroke::new(1.0, super::selected_stroke()))
                .corner_radius(CornerRadius::same(crate::gui::theme::R_MD));
            if ui
                .add(chip)
                .on_hover_text(format!("Sharing {}. Click to share the whole screen again.", self.sharing.1))
                .clicked()
            {
                *action = Some(Box::new(|r, _| r.send(ToAgent::Share(Share::Screen))));
            }
        }
        ui.separator();

        let display = icon_menu(ui, ic::MONITOR, |ui| {
            ui.label(RichText::new("Quality").strong());
            let auto = self.ctl.auto.is_some();
            if ui.selectable_label(auto, "Auto (adapts to the connection)").clicked() {
                *action = Some(Box::new(|r, _| r.menu('a')));
                ui.close();
            }
            for (i, p) in PRESETS.iter().enumerate() {
                let on = !auto && self.ctl.preset == p.name;
                if ui.selectable_label(on, preset_text(p)).clicked() {
                    let c = char::from(b'0' + i as u8);
                    *action = Some(Box::new(move |r, _| r.menu(c)));
                    ui.close();
                }
            }
            ui.collapsing("Custom", |ui| {
                let mut mode = self.ctl.s.mode;
                ui.label(RichText::new("Colours").weak());
                ui.horizontal_wrapped(|ui| {
                    for m in Mode::ALL {
                        ui.selectable_value(&mut mode, m, m.label());
                    }
                });
                let mut scale = self.ctl.s.scale;
                ui.add(egui::Slider::new(&mut scale, 20..=100).text("Resolution %"));
                let mut fps = self.ctl.s.fps;
                ui.add(egui::Slider::new(&mut fps, 1..=60).text("Frames per second"));
                if (mode, scale, fps) != (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) {
                    *action = Some(Box::new(move |r, _| r.custom(mode, scale, fps)));
                }
            });
            ui.separator();
            ui.label(RichText::new("View").strong());
            let mut pick = |ui: &mut egui::Ui, fit: Fit, text: &str| {
                if ui.selectable_label(self.fit == fit, text).clicked() {
                    *action = Some(Box::new(move |r, _| {
                        (r.fit, r.pan) = (fit, Vec2::ZERO);
                        Ok(())
                    }));
                    ui.close();
                }
            };
            pick(ui, Fit::Window, "Fit to window");
            pick(ui, Fit::Stretch, "Stretch to fill");
            ui.checkbox(&mut self.fill_full, "Fill the whole screen in full screen")
                .on_hover_text("No bars when this screen's shape differs from the host's: the picture stretches a little");
            ui.horizontal_wrapped(|ui| {
                for p in [50, 75, 100, 125, 150, 200] {
                    pick(ui, Fit::Fixed(p), &format!("{p}%"));
                }
            });
            if self.monitors.len() > 1 {
                ui.separator();
                ui.label(RichText::new("Screen").strong());
                for (i, m) in self.monitors.iter().enumerate() {
                    let primary = if m.primary { ", primary" } else { "" };
                    let text = format!("{}  {} ({}x{}{primary})", i + 1, m.name, m.w, m.h);
                    if ui.selectable_label(i == self.monitor, text).clicked() {
                        *action = Some(Box::new(move |r, _| {
                            r.chosen_monitor = Some(i as u8);
                            r.send(ToAgent::SelectMonitor(i as u8))
                        }));
                        ui.close();
                    }
                }
            }
        });
        display.on_hover_text(format!("Display: {}; view {}", self.ctl.describe(), self.fit.label()));

        if icon(ui, ic::APP_WINDOW, "Share: the entire screen, one window or one application", self.sharing.0 != 0)
            .clicked()
        {
            *action = Some(Box::new(|r, _| {
                r.picker = Some((None, if r.sharing.0 == 0 { 0 } else { 1 }));
                r.send(ToAgent::ListWindows)
            }));
        }
        let keys = icon_menu(ui, ic::KEYBOARD, |ui| {
            let combos: [(&str, &str, Option<Vec<u32>>); 9] = [
                ("Secure attention", "Ctrl+Alt+Del", None),
                ("Lock screen", "Win+L", Some(vec![SUPER_L, 'l' as u32])),
                ("Start menu", "Win", Some(vec![SUPER_L])),
                ("Run", "Win+R", Some(vec![SUPER_L, 'r' as u32])),
                ("File explorer", "Win+E", Some(vec![SUPER_L, 'e' as u32])),
                ("Show desktop", "Win+D", Some(vec![SUPER_L, 'd' as u32])),
                ("Switch window", "Alt+Tab", Some(vec![ALT_L, xk::TAB])),
                ("Close window", "Alt+F4", Some(vec![ALT_L, xk::F1 + 3])),
                ("Print screen", "PrtSc", Some(vec![xk::PRINT])),
            ];
            for (label, shortcut, keys) in combos {
                if ui.add(egui::Button::new(label).shortcut_text(shortcut)).clicked() {
                    *action = Some(Box::new(move |r, _| match keys {
                        None => r.send(ToAgent::SecureAttention),
                        Some(k) => r.combo(&k),
                    }));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Win + next key").clicked() {
                *action = Some(Box::new(|r, _| {
                    r.win_next = true;
                    r.notify("Press a key to send with Win (Space: Win alone)");
                    Ok(())
                }));
                ui.close();
            }
        });
        keys.on_hover_text("Send special keys");
        ui.separator();
        }
        // The terminal under the screen, in a pane too, as with the chat.
        let (glyph, tip) = if self.term_inline && self.terms.is_some() {
            (ic::MONITOR, "Back to the remote screen, with the terminal under it")
        } else {
            (ic::TERMINAL, "Terminal: a shell on the host in a panel under the screen (right click for more)")
        };
        let showing = self.terms.is_some() && (self.term_split || self.term_inline || self.term_detached);
        let term = icon(ui, glyph, tip, showing);
        if term.clicked() {
            *action = Some(Box::new(|r, _| {
                if r.terms.is_none() {
                    r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    r.term_split = false;
                }
                if r.term_inline || r.term_detached {
                    // Back under the picture.
                    (r.term_inline, r.term_detached, r.term_split) = (false, false, true);
                } else {
                    r.term_split = !r.term_split;
                }
                r.term_focus = r.term_split;
                r.release_all()
            }));
        }
        term.context_menu(|ui| {
            crate::gui::menu_look(ui);
            if ui.button(format!("{}  Terminal in place of the screen", ic::CORNERS_OUT)).clicked() {
                *action = Some(Box::new(|r, _| {
                    if r.terms.is_none() {
                        r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    }
                    (r.term_inline, r.term_detached, r.term_split) = (true, false, false);
                    r.release_all()
                }));
                ui.close();
            }
            if ui.button(format!("{}  Terminal in its own window", ic::TERMINAL)).clicked() {
                *action = Some(Box::new(|r, _| {
                    if r.terms.is_none() {
                        r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    }
                    (r.term_inline, r.term_detached, r.term_split) = (false, true, false);
                    Ok(())
                }));
                ui.close();
            }
            if self.terms.is_some() && ui.button(format!("{}  Close all shells", ic::X)).clicked() {
                *action = Some(Box::new(|r, _| {
                    if let Some(t) = r.terms.take() {
                        for c in t.close_all() {
                            if let crate::termwin::Cmd::Send(m) = c {
                                r.send(m)?;
                            }
                        }
                    }
                    (r.term_inline, r.term_detached, r.term_split) = (false, false, false);
                    Ok(())
                }));
                ui.close();
            }
        });
        let chat_glyph = if self.unread > 0 { format!("{} {}", ic::CHAT, self.unread) } else { ic::CHAT.into() };
        let chat_tip = "Chat with the person at the SSH host's screen";
        let shot_tip = "Screenshot: save the SSH host's screen as a PNG in Pictures";
        let (pause_glyph, pause_tip) = if self.paused {
            (ic::PLAY, "Resume the picture")
        } else {
            (ic::PAUSE, "Pause the picture to save traffic; fetch single frames when needed")
        };
        // Chat is with the person at the screen: shown with the screen only.
        let chat = screen && crate::modules::on("chat") && icon(ui, &chat_glyph, chat_tip, self.chat.is_some()).clicked();
        let shot = screen && icon(ui, ic::CAMERA, shot_tip, false).clicked();
        // Paused: the play button shows as on, so the frozen state is plain to see.
        let (mut pause, mut frame, mut helper) = (false, false, false);
        if screen {
            let b = tool_button(pause_glyph, self.paused);
            pause = ui.add(b).on_hover_text(pause_tip).clicked();
            frame = self.paused && icon(ui, ic::ARROW_CLOCKWISE, "Fetch one frame", false).clicked();
        }

        let mut sound = self.sound.is_some();
        let mut sound_clicked = false;
        // The screen's options (view only, sound, system keys...): only where the screen is shown.
        if screen {
            let more = icon_menu(ui, ic::GEAR, |ui| {
                ui.checkbox(&mut self.view_only, "View only")
                    .on_hover_text("Watch without sending keys, clicks or the wheel");
                ui.checkbox(&mut self.share_clipboard, "Share clipboard").on_hover_text("Copied text goes both ways");
                if crate::modules::on("sound") {
                    sound_clicked = ui.checkbox(&mut sound, "Sound (about 8 KB/s)").clicked();
                }
                if cfg!(windows) {
                    let tip = "While this screen is the active pane, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Print Screen go to the SSH host instead of this PC";
                    ui.checkbox(&mut self.grab_keys, "Send system keys").on_hover_text(tip);
                }
                ui.separator();
                if ui.button("Repaint the whole screen").clicked() {
                    *action = Some(Box::new(|r, _| r.menu('r')));
                    ui.close();
                }
                // A GNOME host shows single windows and programs with its helper only.
                if self.label.ends_with("gnome") {
                    let tip = "Lets you share one window or program of this GNOME host; it starts at the host's next login";
                    if ui.button(format!("{}  Install the window helper on this host", ic::DOWNLOAD)).on_hover_text(tip).clicked() {
                        helper = true;
                        ui.close();
                    }
                }
            });
            more.on_hover_text("Options");
        }
        if helper {
            // The answer shows in the window chooser, opened on its windows' page.
            *action = Some(Box::new(|r, _| {
                r.picker = Some((None, 1));
                r.send(ToAgent::InstallHelper)
            }));
        }
        if chat {
            *action = Some(Box::new(|r, _| {
                match r.chat.take() {
                    Some((lines, _)) => r.chat_lines = lines,
                    None => {
                        r.chat = Some((std::mem::take(&mut r.chat_lines), String::new()));
                        r.unread = 0;
                    }
                }
                Ok(())
            }));
        }
        if shot {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Snapshot)));
        }
        if pause {
            let on = !self.paused;
            (self.paused, self.user_paused) = (on, on);
            *action = Some(Box::new(move |r, _| r.send(ToAgent::Pause(on))));
        }
        if frame {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Refresh)));
        }
        if sound_clicked {
            *action = Some(Box::new(move |r, _| {
                if sound {
                    match crate::audio::Player::new() {
                        Ok(p) => r.sound = Some(p),
                        Err(e) => {
                            r.notify(&format!("Sound: {e}"));
                            return Ok(());
                        }
                    }
                } else {
                    r.sound = None;
                }
                r.send(ToAgent::Audio(sound))
            }));
        }
    }
}

/// The windows or programs to pick from, or why there is no list.
fn share_list(
    ui: &mut egui::Ui,
    list: &Option<WindowList>,
    t: u8,
    row: &dyn Fn(&mut egui::Ui, &str, &str, &str, bool) -> bool,
    pick: &mut Option<ToAgent>,
) {
    match list {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Asking the SSH host for its windows...");
            });
        }
        Some(Err(e)) => {
            ui.add_space(8.0);
            ui.label(e.as_str());
            if e.starts_with(crate::capture::NEEDS_HELPER) && e.contains("Install it") {
                ui.add_space(6.0);
                let b = crate::gui::theme::primary("Install the helper on the SSH host");
                if ui.add(b).clicked() {
                    *pick = Some(ToAgent::InstallHelper);
                }
            }
        }
        Some(Ok(wins)) if wins.is_empty() => {
            ui.label(RichText::new("No windows are open on the SSH host.").weak());
        }
        Some(Ok(wins)) if t == 1 => {
            for w in wins {
                let sub = format!("{}  {}x{}", w.app, w.w, w.h);
                if row(ui, ic::APP_WINDOW, &w.title, &sub, false) {
                    *pick = Some(ToAgent::Share(Share::Window(w.id)));
                }
            }
        }
        Some(Ok(wins)) => {
            let mut apps: Vec<(String, u64, usize)> = Vec::new();
            for w in wins {
                match apps.iter_mut().find(|a| a.0 == w.app) {
                    Some(a) => a.2 += 1,
                    None => apps.push((w.app.clone(), w.id, 1)),
                }
            }
            for (app, id, n) in apps {
                let sub = if n == 1 { "1 window".to_string() } else { format!("{n} windows") };
                if row(ui, ic::SQUARES_FOUR, &app, &sub, false) {
                    *pick = Some(ToAgent::Share(Share::App(id)));
                }
            }
        }
    }
}
