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
                            .filter(|m| !m.camera())
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
        // A web app's page: a page's own controls, as a browser has them.
        if self.web {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (glyph, tip, keysym, mods) in [
                (ic::ARROW_LEFT, "Back (Alt+Left)", xk::LEFT, crate::proto::MOD_ALT),
                (ic::ARROW_RIGHT, "Forward (Alt+Right)", xk::RIGHT, crate::proto::MOD_ALT),
                (ic::ARROW_CLOCKWISE, "Reload (F5)", xk::F1 + 4, 0),
            ] {
                if ui.add(crate::gui::tool_button(glyph, false)).on_hover_text(tip).clicked() {
                    *action = Some(Box::new(move |r, _| r.send(ToAgent::Key { keysym, mods })));
                }
            }
            return;
        }
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
        if self.mirror {
            ui.label(RichText::new(format!("{}  View only", ic::EYE)).small())
                .on_hover_text("This computer's own screen: keys and clicks would come back to this window. Use its windows directly");
        } else if self.view_only && ui.button(RichText::new(format!("{}  View only  {}", ic::EYE, ic::X)).small()).on_hover_text("Mouse and keyboard do not reach the host. Click to control it again").clicked() {
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
            ui.label(RichText::new("Quality of this connection").strong());
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
            // Custom in use is marked as a preset is, with what it is; its controls open below.
            let custom = !auto && self.ctl.preset == "custom";
            let title = if custom { format!("Custom: {}", self.ctl.describe()) } else { "Custom".to_string() };
            let mark = if custom { RichText::new(title).color(ui.visuals().selection.stroke.color).strong() } else { RichText::new(title) };
            egui::CollapsingHeader::new(mark).id_salt("quality-custom").default_open(custom).show(ui, |ui| {
                let mut mode = self.ctl.s.mode;
                ui.label(RichText::new("Colours").weak());
                ui.horizontal_wrapped(|ui| {
                    for m in Mode::ALL {
                        ui.selectable_value(&mut mode, m, m.label());
                    }
                });
                let mut scale = self.ctl.s.scale;
                ui.add(egui::Slider::new(&mut scale, 20..=100).text("Picture size %"))
                    .on_hover_text("A smaller picture than the screen is sent (less to send); the screen's own resolution stays as it is");
                let mut fps = self.ctl.s.fps;
                ui.add(egui::Slider::new(&mut fps, 1..=crate::session::MOST_FPS).text("Frames per second"));
                if (mode, scale, fps) != (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) {
                    *action = Some(Box::new(move |r, _| r.custom(mode, scale, fps)));
                }
            });
            // The codec of what moves like a film; text and drawings go losslessly whatever it is.
            ui.separator();
            ui.label(RichText::new("Films in").strong())
                .on_hover_text("The codec for parts of the screen that move like a film (text and drawings always go losslessly). New connections take it too.");
            {
                use crate::proto::{CODEC_AUTO, CODEC_AV1, CODEC_H264, CODEC_H265};
                let now = crate::proto::codec_of(self.ctl.s.formats);
                let h265 = if !crate::hwdec::can_h265() {
                    Some("This computer has no H.265 decoder (on Windows, the HEVC Video Extensions bring one)")
                } else if !self.can_h265 {
                    Some("That computer has no video hardware that encodes H.265, or SSH Remote there is older than 0.25.51")
                } else {
                    None
                };
                ui.horizontal_wrapped(|ui| {
                    for (c, name, tip) in [
                        (CODEC_AUTO, "Auto", "AV1, or the video hardware's H.264 there when its processor falls behind"),
                        (CODEC_AV1, "AV1", "The fewest bytes for the same picture, from the processor there"),
                        (CODEC_H264, "H.264", "From the processor there, or its video hardware when the processor falls behind"),
                        (CODEC_H265, "H.265", "From the video hardware there, decoded by this computer's system"),
                    ] {
                        let can = c != CODEC_H265 || h265.is_none();
                        let r = ui.add_enabled_ui(can, |ui| ui.selectable_label(now == c, name)).inner;
                        let r = if can { r.on_hover_text(tip) } else { r.on_disabled_hover_text(h265.unwrap_or_default()) };
                        if r.clicked() && now != c {
                            *action = Some(Box::new(move |r, _| r.set_codec(c)));
                            ui.close();
                        }
                    }
                });
            }
            // The screen's own resolution (its display mode), not the picture's size.
            let (list, current, own) = &self.modes;
            if !list.is_empty() {
                ui.separator();
                ui.label(RichText::new("Resolution of its screen").strong())
                    .on_hover_text("The screen itself is set to it, as in its display settings; it is set back when you disconnect");
                let name = |&(w, h): &(u16, u16)| if (w, h) == *own { format!("{w} \u{d7} {h}  (its own)") } else { format!("{w} \u{d7} {h}") };
                egui::ScrollArea::vertical().id_salt("host-modes").max_height(170.0).show(ui, |ui| {
                    for &m in list {
                        if ui.selectable_label(m == *current, name(&m)).clicked() && m != *current {
                            *action = Some(Box::new(move |r, _| r.send(ToAgent::SetMode { w: m.0, h: m.1 })));
                            ui.close();
                        }
                    }
                });
                if current != own && ui.button(format!("Back to its own, {}", name(own).replace("  (its own)", ""))).clicked() {
                    *action = Some(Box::new(|r, _| r.send(ToAgent::SetMode { w: 0, h: 0 })));
                    ui.close();
                }
            } else if !matches!(self.sess.backend.as_str(), "camera" | "browser") {
                // Where it would be, and why it is not: looked for here.
                ui.separator();
                let why = if self.modes_heard {
                    "Resolution of its screen: it cannot be set there"
                } else {
                    "Resolution of its screen: SSH Remote there is too old for it (0.21 or later sets it)"
                };
                ui.label(RichText::new(why).weak());
            }
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
            ui.horizontal(|ui| {
                for p in [50, 75, 100, 125, 150, 200] {
                    pick(ui, Fit::Fixed(p), &format!("{p}%"));
                }
            });
            ui.separator();
            // Where every new connection starts: Settings, as the quality there is named.
            let default = crate::hosts::setting("preset").and_then(|v| v.parse::<usize>().ok()).and_then(|i| PRESETS.get(i)).map_or("Auto".to_string(), preset_text);
            if ui.button(format!("{}  New connections start with: {default}\u{2026}", ic::SLIDERS_HORIZONTAL)).on_hover_text("Settings, Connections").clicked() {
                crate::gui::ask_settings("connections");
                ui.close();
            }
        });
        // Its screen's own resolution is set here too, when it can be: said, as it is easy to miss.
        let resolution = match &self.modes {
            (list, (w, h), _) if !list.is_empty() => format!("; its screen {w} \u{d7} {h}, its resolution can be set here"),
            _ => String::new(),
        };
        display.on_hover_text(format!("Display: {}{resolution}; view {}", self.ctl.describe(), self.fit.label()));

        // A few screens: one button each, the one shown lit, as RustDesk has them.
        let screens: Vec<usize> = self.monitors.iter().enumerate().filter(|(_, m)| !m.camera()).map(|(i, _)| i).collect();
        if (2..=4).contains(&screens.len()) {
            for (n, &i) in screens.iter().enumerate() {
                let m = &self.monitors[i];
                let on = i == self.monitor && self.sharing.0 == 0;
                let digit = format!("{}", n + 1);
                let b = crate::gui::tool_button(&digit, on).min_size(Vec2::new(24.0, 30.0));
                let primary = if m.primary { ", primary" } else { "" };
                if ui.add(b).on_hover_text(format!("Its screen {}: {} ({}x{}{primary}). Ctrl+Alt+Shift+{}", n + 1, m.name, m.w, m.h, n + 1)).clicked() && !on {
                    *action = Some(Box::new(move |r, _| r.show_monitor(i)));
                }
            }
        }
        // The host's screens, when it has several: which one shows here, or all, a window each;
        // and screens added to it for the session, where it can have them.
        if self.monitors.len() > 1 || self.adds_screens || self.screens_need_helper {
            let screens = icon_menu(ui, ic::SQUARES_FOUR, |ui| {
                ui.label(RichText::new("Its screens").strong());
                for (i, m) in self.monitors.iter().enumerate() {
                    let primary = if m.primary { ", primary" } else { "" };
                    // A camera: to look at, shown by its name alone.
                    let added = if m.added { ", added" } else { "" };
                    let text = if m.camera() { format!("{}  {}", ic::CAMERA, m.name) } else { format!("{}  {} ({}x{}{primary}{added})", i + 1, m.name, m.w, m.h) };
                    if ui.selectable_label(i == self.monitor, text).clicked() {
                        *action = Some(Box::new(move |r, _| r.show_monitor(i)));
                        ui.close();
                    }
                }
                let screens = self.monitors.iter().filter(|m| !m.camera()).count();
                if screens > 1 {
                ui.separator();
                let locals = crate::sys::monitors().len();
                let tip = if locals > 1 {
                    "Each of the host's screens in a window of its own, full screen on one of this computer's monitors each, as Remote Desktop's \"use all my monitors\""
                } else {
                    "Each of the host's screens in a window of its own"
                };
                if ui.button(format!("{}  All its screens, a window each", ic::APP_WINDOW)).on_hover_text(tip).clicked() {
                    *action = Some(Box::new(|r, _| {
                        r.spread_asked = true;
                        Ok(())
                    }));
                    ui.close();
                }
                let tip = "Each of the host's screens beside the others in this tab, as they lie on the host, sized to fit; full screen keeps them together";
                if ui.button(format!("{}  All its screens here, side by side", ic::SPLIT_SIDE)).on_hover_text(tip).clicked() {
                    *action = Some(Box::new(|r, _| {
                        r.beside_asked = true;
                        Ok(())
                    }));
                    ui.close();
                }
                }
                if self.screens_need_helper {
                    ui.separator();
                    ui.add_enabled(false, egui::Button::new(format!("{}  Add a screen", ic::PLUS))).on_disabled_hover_text(
                        "That computer adds screens with a virtual display driver that comes with the helper: install the helper there (remote access started with the computer), then connect again",
                    );
                }
                if self.adds_screens {
                    ui.separator();
                    // This computer's monitor first: the screen added fills it in full screen.
                    let mine = crate::sys::monitors().iter().find(|m| m.primary).map(|m| (m.w.clamp(640, 7680) as u16, m.h.clamp(480, 4320) as u16));
                    let mut sizes: Vec<(u16, u16)> = mine.into_iter().collect();
                    for s in [(1920, 1080), (2560, 1440), (3840, 2160), (1366, 768), (1280, 800)] {
                        if !sizes.contains(&s) {
                            sizes.push(s);
                        }
                    }
                    let tip = "A screen of its own for this session, as a monitor plugged in there, with none needed: windows go to it and it shows here as its other screens do. It goes when you leave";
                    ui.menu_button(format!("{}  Add a screen", ic::PLUS), |ui| {
                        for &(w, h) in &sizes {
                            let here = if Some((w, h)) == mine { "  (this monitor's)" } else { "" };
                            if ui.button(format!("{w} \u{d7} {h}{here}")).clicked() {
                                *action = Some(Box::new(move |r, _| r.send(ToAgent::AddScreen { w, h })));
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text(tip);
                    for (i, m) in self.monitors.iter().enumerate().filter(|(_, m)| m.added) {
                        if ui.button(format!("{}  Take away screen {} ({}x{})", ic::X, i + 1, m.w, m.h)).on_hover_text("Unplugs the screen added: its windows go to the others").clicked() {
                            let name = m.name.clone();
                            *action = Some(Box::new(move |r, _| r.send(ToAgent::RemoveScreen(name))));
                            ui.close();
                        }
                    }
                }
            });
            screens.on_hover_text(format!("Its screens: {} of {} shows here; Ctrl+Alt+Shift+1, 2, ... picks one", self.monitor + 1, self.monitors.len()));
        }

        // By ID: through the relay, or straight once a path through the routers was found.
        if let Some(path) = self.sess.straight() {
            let (glyph, color, tip) = match path {
                Some(a) => (ic::LIGHTNING, crate::gui::good(), format!("Straight to the computer ({a}), past the relay; sealed end to end")),
                None => (
                    ic::GLOBE,
                    ui.visuals().weak_text_color(),
                    "Through the relay, sealed end to end (it sees no picture or key press); a straight path is taken when one is found".to_string(),
                ),
            };
            ui.label(egui::RichText::new(glyph).size(16.0).color(color)).on_hover_text(tip);
        }
        // The person at a computer reached by its ID keeps some things from this viewer.
        if self.allowed != crate::grant::ALL {
            let tip = format!("The person at that computer lets you use: {}", crate::grant::words(self.allowed));
            ui.label(egui::RichText::new(ic::SHIELD).size(17.0).color(crate::gui::warn())).on_hover_text(tip);
        }
        if icon(ui, ic::APP_WINDOW, "Share: the entire screen, one window or one application", self.sharing.0 != 0)
            .clicked()
        {
            *action = Some(Box::new(|r, _| {
                r.picker = Some((None, if r.sharing.0 == 0 { 0 } else { 1 }));
                r.send(ToAgent::ListWindows)
            }));
        }
        let mac = self.sess.backend == "mac";
        let keys = icon_menu(ui, ic::KEYBOARD, |ui| {
            // A Mac's own shortcuts there, named as macOS names them (the keys as that Mac has them).
            let combos: Vec<(&str, &str, Option<Vec<u32>>)> = if mac {
                vec![
                    ("Lock screen", "Ctrl+Cmd+Q", Some(vec![CONTROL_L, SUPER_L, 'q' as u32])),
                    ("Spotlight", "Cmd+Space", Some(vec![SUPER_L, ' ' as u32])),
                    ("Switch application", "Cmd+Tab", Some(vec![SUPER_L, xk::TAB])),
                    ("Mission Control", "Ctrl+Up", Some(vec![CONTROL_L, xk::UP])),
                    ("Force quit", "Option+Cmd+Esc", Some(vec![ALT_L, SUPER_L, xk::ESCAPE])),
                    ("Screenshot", "Shift+Cmd+5", Some(vec![SHIFT_L, SUPER_L, '5' as u32])),
                    ("Close window", "Cmd+W", Some(vec![SUPER_L, 'w' as u32])),
                    ("Quit application", "Cmd+Q", Some(vec![SUPER_L, 'q' as u32])),
                ]
            } else {
                vec![
                    ("Secure attention", "Ctrl+Alt+Del", None),
                    ("Lock screen", "Win+L", Some(vec![SUPER_L, 'l' as u32])),
                    ("Start menu", "Win", Some(vec![SUPER_L])),
                    ("Run", "Win+R", Some(vec![SUPER_L, 'r' as u32])),
                    ("File explorer", "Win+E", Some(vec![SUPER_L, 'e' as u32])),
                    ("Show desktop", "Win+D", Some(vec![SUPER_L, 'd' as u32])),
                    ("Switch window", "Alt+Tab", Some(vec![ALT_L, xk::TAB])),
                    ("Close window", "Alt+F4", Some(vec![ALT_L, xk::F1 + 3])),
                    ("Print screen", "PrtSc", Some(vec![xk::PRINT])),
                ]
            };
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
            if ui.button(if mac { "Cmd + next key" } else { "Win + next key" }).clicked() {
                *action = Some(Box::new(move |r, _| {
                    r.win_next = true;
                    r.notify(if mac { "Press a key to send with Cmd" } else { "Press a key to send with Win (Space: Win alone)" });
                    Ok(())
                }));
                ui.close();
            }
            // Between a Mac and another computer: each side's shortcuts with its own keys.
            if mac != cfg!(target_os = "macos") {
                let (label, tip) = if mac {
                    ("Ctrl here is Cmd there", "Ctrl+C, Ctrl+V and the other shortcuts work there as here; the Win key is Ctrl there")
                } else {
                    ("Cmd here is Ctrl there", "Cmd+C, Cmd+V and the other shortcuts work there as here; Ctrl is the Win key there")
                };
                let mut on = self.swap_cmd;
                if ui.checkbox(&mut on, label).on_hover_text(tip).changed() {
                    *action = Some(Box::new(move |r, _| {
                        // Nothing held stays down under the other name.
                        r.release_all()?;
                        r.swap_cmd = on;
                        Ok(())
                    }));
                    ui.close();
                }
            }
        });
        keys.on_hover_text("Send special keys");
        self.computer_menu(ui, action);
        ui.separator();
        }
        // The terminal under the screen, in a pane too, as with the chat; files have none.
        if self.mode() != crate::gui::workspace::PaneMode::Files {
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
                    // Back under the picture, which starts again (the mode change does that).
                    r.set_mode(crate::gui::workspace::PaneMode::Screen)?;
                    (r.term_detached, r.term_split) = (false, true);
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
                // The pane's Terminal mode: the picture pauses while it is not seen.
                *action = Some(Box::new(|r, _| {
                    (r.term_detached, r.term_split) = (false, false);
                    r.set_mode(crate::gui::workspace::PaneMode::Terminal)
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
                    // From the terminal in place of the screen, back to the screen, which starts again.
                    if r.term_inline {
                        r.set_mode(crate::gui::workspace::PaneMode::Screen)?;
                    }
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
        }
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
        // Recording: the button shows how long it has gone on.
        if screen {
            let tip = match &self.recording {
                Some(r) => {
                    let s = r.started.elapsed().as_secs();
                    format!("Recording for {}:{:02}: click to stop and save it", s / 60, s % 60)
                }
                None => "Record the screen to a video file (MP4, in Videos)".to_string(),
            };
            let b = tool_button(ic::RECORD, self.recording.is_some());
            let b = if self.recording.is_some() { b.fill(crate::gui::bad().gamma_multiply(0.35)) } else { b };
            if ui.add(b).on_hover_text(tip).clicked() {
                *action = Some(Box::new(|r, _| {
                    r.toggle_recording();
                    Ok(())
                }));
            }
        }
        // Paused: the play button shows as on, so the frozen state is plain to see.
        let (mut pause, mut frame, mut helper) = (false, false, false);
        if screen {
            let b = tool_button(pause_glyph, self.paused);
            pause = ui.add(b).on_hover_text(pause_tip).clicked();
            frame = self.paused && icon(ui, ic::ARROW_CLOCKWISE, "Fetch one frame", false).clicked();
        }

        // A voice call with the person at that computer: started, called off while it rings, hung up.
        if screen && crate::modules::on("sound") {
            use crate::chat::{CALL_RINGING, CALL_TALKING};
            let (glyph, tip, on) = match self.call {
                CALL_TALKING => (ic::PHONE_HANG_UP, "Hang up", true),
                CALL_RINGING => (ic::PHONE, "Calling\u{2026} (click to call off)", true),
                _ => (ic::PHONE, "Call the person at that computer: talk through the microphones", false),
            };
            if icon(ui, glyph, tip, on).clicked() {
                *action = Some(Box::new(|r, _| r.toggle_call()));
            }
        }
        let mut sound = self.sound.is_some();
        let mut sound_clicked = false;
        // The screen's options (view only, sound, system keys...): only where the screen is shown.
        if screen {
            let more = icon_menu(ui, ic::GEAR, |ui| {
                ui.label(RichText::new("This connection").strong());
                if self.mirror {
                    ui.add_enabled(false, egui::Checkbox::new(&mut true, "View only")).on_disabled_hover_text("This computer's own screen: keys and clicks would come back to this window");
                } else {
                    ui.checkbox(&mut self.view_only, "View only").on_hover_text("Watch without sending keys, clicks or the wheel");
                }
                ui.checkbox(&mut self.share_clipboard, "Share clipboard").on_hover_text("Copied text goes both ways");
                if !self.mirror && !self.web && ui.checkbox(&mut self.games, "Mouse for games").on_hover_text("The pointer is held in this window and its movement goes to the host, for games that turn with the mouse. Click the picture to hold it; Ctrl+Alt lets it go").changed() {
                    // Held at once, from the menu: Ctrl+Alt or this again lets it go.
                    self.games_held = self.games;
                    if self.games {
                        self.notify("The mouse is held for the game: Ctrl+Alt lets it go");
                    }
                }
                if crate::modules::on("sound") {
                    sound_clicked = ui.checkbox(&mut sound, "Sound (about 8 KB/s)").clicked();
                }
                if cfg!(windows) {
                    let tip = "While this screen is the active pane, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Print Screen go to the SSH host instead of this PC";
                    ui.checkbox(&mut self.grab_keys, "Send system keys").on_hover_text(tip);
                } else if cfg!(target_os = "linux") {
                    let tip = "While this screen is the active pane and the pointer is over it, Super, Alt+Tab and the desktop's other shortcuts go to the SSH host instead of this computer";
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
                r.toggle_chat();
                Ok(())
            }));
        }
        if shot {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Snapshot)));
        }
        if pause {
            let on = !self.paused;
            *action = Some(Box::new(move |r, _| r.set_paused(on)));
        }
        if frame {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Refresh)));
        }
        if sound_clicked {
            *action = Some(Box::new(move |r, _| r.set_sound(sound)));
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

impl Remote {
    /// The computer itself: lock and restart it, keep its own keyboard and mouse out, hide its
    /// screen, lock it on leaving. What it can do is asked the first time the menu opens.
    fn computer_menu(&self, ui: &mut egui::Ui, action: &mut Option<Deferred>) {
        use crate::hostctl::{BLOCK, HIDE, LOCK, LOCK_ON_LEAVE, QUERY, RESTART, bit};
        let asked = self.host_asked;
        let state = self.host;
        let menu = icon_menu(ui, ic::LOCK_KEY, |ui| {
            if !asked {
                *action = Some(Box::new(|r, _| {
                    r.host_asked = true;
                    r.send(ToAgent::Host { act: QUERY, on: false })
                }));
            }
            let Some((can, on)) = state else {
                ui.label(egui::RichText::new("Asking the computer\u{2026}").weak());
                return;
            };
            if can == 0 {
                ui.label(egui::RichText::new("The person at that computer does not let you do these.").weak());
                return;
            }
            let able = |a: u8| can & bit(a) != 0;
            let only = "Not on this computer (Windows only)";
            if ui.add_enabled(able(LOCK), egui::Button::new(format!("{}  Lock it", ic::LOCK_KEY))).clicked() {
                *action = Some(Box::new(|r, _| r.send(ToAgent::Host { act: LOCK, on: true })));
                ui.close();
            }
            ui.add_enabled_ui(able(RESTART), |ui| {
                ui.menu_button(format!("{}  Restart it", ic::ARROW_CLOCKWISE), |ui| {
                    ui.label(egui::RichText::new("Programs there close; unsaved work may be lost.").weak());
                    if ui.add(crate::gui::kit::danger("Restart now", true)).clicked() {
                        *action = Some(Box::new(|r, _| r.send(ToAgent::Host { act: RESTART, on: true })));
                        ui.close();
                    }
                });
            });
            ui.separator();
            for (act, label, tip) in [
                (BLOCK, "Keep its keyboard and mouse out", "Whoever sits there cannot type or click; Ctrl+P there takes them back"),
                (HIDE, "Hide its screen", "Black for whoever sits there while you still see it; its keyboard and mouse are kept out too"),
                (LOCK_ON_LEAVE, "Lock it when I leave", "Locks the computer as this connection ends"),
            ] {
                let mut now = on & bit(act) != 0;
                let r = ui.add_enabled(able(act), egui::Checkbox::new(&mut now, label));
                let r = if able(act) { r.on_hover_text(tip) } else { r.on_disabled_hover_text(only) };
                if r.changed() {
                    *action = Some(Box::new(move |r, _| r.send(ToAgent::Host { act, on: now })));
                }
            }
        });
        menu.on_hover_text("This computer: lock, restart, keep its keyboard out, hide its screen");
    }
}
