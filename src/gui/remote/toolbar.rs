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
                ui.label(RichText::new(t!("Choose what to share")).size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon(ui, ic::X, t!("Close"), false).clicked() {
                        close = true;
                    }
                });
            });
            ui.label(RichText::new(t!("The SSH host sends only what you choose; clicks and keys reach only that.")).weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (i, (g, name)) in
                    [(ic::MONITOR, t!("Entire screen")), (ic::APP_WINDOW, t!("Window")), (ic::SQUARES_FOUR, t!("Application"))]
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
                        vec![(String::new(), t!("The only screen").into())]
                    } else {
                        monitors
                            .iter()
                            .filter(|m| !m.camera())
                            .map(|m| {
                                (m.name.clone(), if m.primary { t!("{}x{}, primary", m.w, m.h) } else { format!("{}x{}", m.w, m.h) })
                            })
                            .collect()
                    };
                    for (i, (name, sub)) in screens.iter().enumerate() {
                        let on = self.sharing.0 == 0 && i == current;
                        if row(ui, ic::MONITOR, t!("Screen {}  {}", i + 1, name).trim(), sub, on) {
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
                        let sub = t!("The window in front on the SSH host; click it there first");
                        if row(ui, ic::APP_WINDOW, t!("Active window"), sub, false) {
                            pick = Some(ToAgent::Share(Share::ActiveWindow));
                        }
                        // Pointing at the window itself: the click brings it to the front, then it is shared.
                        let sub = t!("Click the window you want in the picture of the SSH host");
                        if row(ui, ic::CURSOR_CLICK, t!("Click a window to share"), sub, false) {
                            click_pick = true;
                        }
                    } else if no_helper {
                        ui.add_enabled_ui(false, |ui| {
                            row(
                                ui,
                                ic::SQUARES_FOUR,
                                t!("Active application"),
                                t!("Needs the sshremote helper on GNOME"),
                                false,
                            )
                        });
                    } else {
                        let sub = t!("Every window of the program in front on the SSH host");
                        if row(ui, ic::SQUARES_FOUR, t!("Active application"), sub, false) {
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
                (ic::ARROW_LEFT, t!("Back (Alt+Left)"), xk::LEFT, crate::proto::MOD_ALT),
                (ic::ARROW_RIGHT, t!("Forward (Alt+Right)"), xk::RIGHT, crate::proto::MOD_ALT),
                (ic::ARROW_CLOCKWISE, t!("Reload (F5)"), xk::F1 + 4, 0),
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
            ui.label(RichText::new(format!("{}  {}", ic::EYE, t!("View only"))).small())
                .on_hover_text(t!("This computer's own screen: keys and clicks would come back to this window. Use its windows directly"));
        } else if self.view_only && ui.button(RichText::new(format!("{}  {}  {}", ic::EYE, t!("View only"), ic::X)).small()).on_hover_text(t!("Mouse and keyboard do not reach the host. Click to control it again")).clicked() {
            self.view_only = false;
        }
        if screen {
        if self.sharing.0 != 0 {
            let name: String = crate::lang::tr_msg(&self.sharing.1).chars().take(18).collect();
            let chip = egui::Button::new(RichText::new(format!("{}  {name}  {}", ic::APP_WINDOW, ic::X)).small())
                .fill(super::selected_fill())
                .stroke(egui::Stroke::new(1.0, super::selected_stroke()))
                .corner_radius(CornerRadius::same(crate::gui::theme::R_MD));
            if ui
                .add(chip)
                .on_hover_text(t!("Sharing {}. Click to share the whole screen again.", crate::lang::tr_msg(&self.sharing.1)))
                .clicked()
            {
                *action = Some(Box::new(|r, _| r.send(ToAgent::Share(Share::Screen))));
            }
        }
        ui.separator();

        let display = icon_menu(ui, ic::MONITOR, |ui| {
            ui.label(RichText::new(t!("Quality of this connection")).strong());
            let auto = self.ctl.auto.is_some();
            if ui.selectable_label(auto, t!("Auto (adapts to the connection)")).clicked() {
                *action = Some(Box::new(|r, _| r.menu('a')));
                ui.close();
            }
            for (i, p) in PRESETS.iter().enumerate() {
                let on = !auto && self.ctl.preset == p.name;
                if ui.selectable_label(on, crate::lang::tr_msg(&preset_text(p))).clicked() {
                    let c = char::from(b'0' + i as u8);
                    *action = Some(Box::new(move |r, _| r.menu(c)));
                    ui.close();
                }
            }
            // Custom in use is marked as a preset is, with what it is; its controls open below.
            let custom = !auto && self.ctl.preset == "custom";
            let title = if custom { t!("Custom: {}", crate::lang::tr_msg(&self.ctl.describe())) } else { t!("Custom").to_string() };
            let mark = if custom { RichText::new(title).color(ui.visuals().selection.stroke.color).strong() } else { RichText::new(title) };
            egui::CollapsingHeader::new(mark).id_salt("quality-custom").default_open(custom).show(ui, |ui| {
                let mut mode = self.ctl.s.mode;
                ui.label(RichText::new(t!("Colours")).weak());
                ui.horizontal_wrapped(|ui| {
                    for m in Mode::ALL {
                        ui.selectable_value(&mut mode, m, crate::lang::tr(m.label()));
                    }
                });
                let mut scale = self.ctl.s.scale;
                ui.add(egui::Slider::new(&mut scale, 20..=100).text(t!("Picture size %")))
                    .on_hover_text(t!("A smaller picture than the screen is sent (less to send); the screen's own resolution stays as it is"));
                let mut fps = self.ctl.s.fps;
                ui.add(egui::Slider::new(&mut fps, 1..=crate::session::MOST_FPS).text(t!("Frames per second")));
                if (mode, scale, fps) != (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) {
                    *action = Some(Box::new(move |r, _| r.custom(mode, scale, fps)));
                }
            });
            // The codec of what moves like a film; text and drawings go losslessly whatever it is.
            ui.separator();
            ui.label(RichText::new(t!("Films in")).strong())
                .on_hover_text(t!("The codec for parts of the screen that move like a film (text and drawings always go losslessly). New connections take it too."));
            {
                use crate::proto::{CODEC_AUTO, CODEC_AV1, CODEC_H264, CODEC_H265};
                let now = crate::proto::codec_of(self.ctl.s.formats);
                let h265 = if !crate::hwdec::can_h265() {
                    Some(t!("This computer has no H.265 decoder (on Windows, the HEVC Video Extensions bring one)"))
                } else if !self.can_h265 {
                    Some(t!("That computer has no video hardware that encodes H.265, or SSH Remote there is older than 0.25.51"))
                } else {
                    None
                };
                ui.horizontal_wrapped(|ui| {
                    for (c, name, tip) in [
                        (CODEC_AUTO, t!("Auto"), t!("AV1, or the video hardware's H.264 there when its processor falls behind")),
                        (CODEC_AV1, "AV1", t!("The fewest bytes for the same picture, from the processor there")),
                        (CODEC_H264, "H.264", t!("From the processor there, or its video hardware when the processor falls behind")),
                        (CODEC_H265, "H.265", t!("From the video hardware there, decoded by this computer's system")),
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
                ui.label(RichText::new(t!("Resolution of its screen")).strong())
                    .on_hover_text(t!("The screen itself is set to it, as in its display settings; it is set back when you disconnect"));
                let name = |&(w, h): &(u16, u16)| if (w, h) == *own { t!("{} \u{d7} {}  (its own)", w, h) } else { format!("{w} \u{d7} {h}") };
                egui::ScrollArea::vertical().id_salt("host-modes").max_height(170.0).show(ui, |ui| {
                    for &m in list {
                        if ui.selectable_label(m == *current, name(&m)).clicked() && m != *current {
                            *action = Some(Box::new(move |r, _| r.send(ToAgent::SetMode { w: m.0, h: m.1 })));
                            ui.close();
                        }
                    }
                });
                if current != own && ui.button(t!("Back to its own, {}", format!("{} \u{d7} {}", own.0, own.1))).clicked() {
                    *action = Some(Box::new(|r, _| r.send(ToAgent::SetMode { w: 0, h: 0 })));
                    ui.close();
                }
            } else if !matches!(self.sess.backend.as_str(), "camera" | "browser") {
                // Where it would be, and why it is not: looked for here.
                ui.separator();
                let why = if self.modes_heard {
                    t!("Resolution of its screen: it cannot be set there")
                } else {
                    t!("Resolution of its screen: SSH Remote there is too old for it (0.21 or later sets it)")
                };
                ui.label(RichText::new(why).weak());
            }
            ui.separator();
            ui.label(RichText::new(t!("View")).strong());
            let mut pick = |ui: &mut egui::Ui, fit: Fit, text: &str| {
                if ui.selectable_label(self.fit == fit, text).clicked() {
                    *action = Some(Box::new(move |r, _| {
                        (r.fit, r.pan) = (fit, Vec2::ZERO);
                        Ok(())
                    }));
                    ui.close();
                }
            };
            pick(ui, Fit::Window, t!("Fit to window"));
            pick(ui, Fit::Stretch, t!("Stretch to fill"));
            ui.checkbox(&mut self.fill_full, t!("Fill the whole screen in full screen"))
                .on_hover_text(t!("No bars when this screen's shape differs from the host's: the picture stretches a little"));
            ui.horizontal(|ui| {
                for p in [50, 75, 100, 125, 150, 200] {
                    pick(ui, Fit::Fixed(p), &format!("{p}%"));
                }
            });
            // The mouse as the computer there should take it.
            let mut swap = self.swap_buttons;
            if ui.checkbox(&mut swap, t!("Trade the left and right buttons")).on_hover_text(t!("For a mouse set for the other hand here or there")).changed() {
                self.swap_buttons = swap;
                crate::hosts::set_setting("swap_buttons", if swap { "yes" } else { "no" });
            }
            let mut reverse = self.reverse_wheel;
            if ui.checkbox(&mut reverse, t!("Turn the wheel the other way")).on_hover_text(t!("Scrolls there the other way from here, as between natural and classic scrolling")).changed() {
                self.reverse_wheel = reverse;
                crate::hosts::set_setting("reverse_wheel", if reverse { "yes" } else { "no" });
            }
            let mut follow = self.follow;
            if ui.checkbox(&mut follow, t!("Follow the pointer there")).on_hover_text(t!("At a size larger than this pane, the picture moves with the pointer when the person there or a program moves it")).changed() {
                self.follow = follow;
                crate::hosts::set_setting("follow_pointer", if follow { "yes" } else { "no" });
            }
            let mut screens = self.follow_screens;
            if ui
                .checkbox(&mut screens, t!("Go to the monitor the pointer goes to"))
                .on_hover_text(t!("With several monitors there, the picture changes to the one the person there or a program moves the pointer onto"))
                .changed()
            {
                self.follow_screens = screens;
                crate::hosts::set_setting("follow_screens", if screens { "yes" } else { "no" });
            }
            ui.separator();
            // Where every new connection starts: Settings, as the quality there is named.
            let default = crate::hosts::setting("preset").and_then(|v| v.parse::<usize>().ok()).and_then(|i| PRESETS.get(i)).map_or(t!("Auto").to_string(), |p| crate::lang::tr_msg(&preset_text(p)).into_owned());
            if ui.button(format!("{}  {}", ic::SLIDERS_HORIZONTAL, t!("New connections start with: {}\u{2026}", default))).on_hover_text(t!("Settings, Connections")).clicked() {
                crate::gui::ask_settings("connections");
                ui.close();
            }
        });
        // Its screen's own resolution is set here too, when it can be: said, as it is easy to miss.
        let (describe, fit) = (crate::lang::tr_msg(&self.ctl.describe()).into_owned(), crate::lang::tr_msg(&self.fit.label()).into_owned());
        let tip = match &self.modes {
            (list, (w, h), _) if !list.is_empty() => t!("Display: {}; its screen {} \u{d7} {}, its resolution can be set here; view {}", describe, w, h, fit),
            _ => t!("Display: {}; view {}", describe, fit),
        };
        display.on_hover_text(tip);

        // A few screens: one button each, the one shown lit, as RustDesk has them.
        let screens: Vec<usize> = self.monitors.iter().enumerate().filter(|(_, m)| !m.camera()).map(|(i, _)| i).collect();
        if (2..=4).contains(&screens.len()) {
            for (n, &i) in screens.iter().enumerate() {
                let m = &self.monitors[i];
                let on = i == self.monitor && self.sharing.0 == 0;
                let digit = format!("{}", n + 1);
                let b = crate::gui::tool_button(&digit, on).min_size(Vec2::new(24.0, 30.0));
                let tip = if m.primary {
                    t!("Its screen {}: {} ({}x{}, primary). Ctrl+Alt+Shift+{}", n + 1, m.name, m.w, m.h, n + 1)
                } else {
                    t!("Its screen {}: {} ({}x{}). Ctrl+Alt+Shift+{}", n + 1, m.name, m.w, m.h, n + 1)
                };
                if ui.add(b).on_hover_text(tip).clicked() && !on {
                    *action = Some(Box::new(move |r, _| r.show_monitor(i)));
                }
            }
        }
        // The host's screens, when it has several: which one shows here, or all, a window each;
        // and screens added to it for the session, where it can have them.
        if self.monitors.len() > 1 || self.adds_screens || self.screens_need_helper {
            let screens = icon_menu(ui, ic::SQUARES_FOUR, |ui| {
                ui.label(RichText::new(t!("Its screens")).strong());
                for (i, m) in self.monitors.iter().enumerate() {
                    let size = match (m.primary, m.added) {
                        (false, false) => format!("{}x{}", m.w, m.h),
                        (true, false) => t!("{}x{}, primary", m.w, m.h),
                        (false, true) => t!("{}x{}, added", m.w, m.h),
                        (true, true) => t!("{}x{}, primary, added", m.w, m.h),
                    };
                    // A camera: to look at, shown by its name alone.
                    let text = if m.camera() { format!("{}  {}", ic::CAMERA, m.name) } else { format!("{}  {} ({size})", i + 1, m.name) };
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
                    t!("Each of the host's screens in a window of its own, full screen on one of this computer's monitors each, as Remote Desktop's \"use all my monitors\"")
                } else {
                    t!("Each of the host's screens in a window of its own")
                };
                if ui.button(format!("{}  {}", ic::APP_WINDOW, t!("All its screens, a window each"))).on_hover_text(tip).clicked() {
                    *action = Some(Box::new(|r, _| {
                        r.spread_asked = true;
                        Ok(())
                    }));
                    ui.close();
                }
                let tip = t!("Each of the host's screens beside the others in this tab, as they lie on the host, sized to fit; full screen keeps them together");
                if ui.button(format!("{}  {}", ic::SPLIT_SIDE, t!("All its screens here, side by side"))).on_hover_text(tip).clicked() {
                    *action = Some(Box::new(|r, _| {
                        r.beside_asked = true;
                        Ok(())
                    }));
                    ui.close();
                }
                }
                if self.screens_need_helper {
                    ui.separator();
                    ui.add_enabled(false, egui::Button::new(format!("{}  {}", ic::PLUS, t!("Add a screen")))).on_disabled_hover_text(
                        t!("That computer adds screens with a virtual display driver that comes with the helper: install the helper there (remote access started with the computer), then connect again"),
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
                    let tip = t!("A screen of its own for this session, as a monitor plugged in there, with none needed: windows go to it and it shows here as its other screens do. It goes when you leave");
                    ui.menu_button(format!("{}  {}", ic::PLUS, t!("Add a screen")), |ui| {
                        for &(w, h) in &sizes {
                            let text = if Some((w, h)) == mine { t!("{} \u{d7} {}  (this monitor's)", w, h) } else { format!("{w} \u{d7} {h}") };
                            if ui.button(text).clicked() {
                                *action = Some(Box::new(move |r, _| r.send(ToAgent::AddScreen { w, h })));
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text(tip);
                    for (i, m) in self.monitors.iter().enumerate().filter(|(_, m)| m.added) {
                        if ui.button(format!("{}  {}", ic::X, t!("Take away screen {} ({}x{})", i + 1, m.w, m.h))).on_hover_text(t!("Unplugs the screen added: its windows go to the others")).clicked() {
                            let name = m.name.clone();
                            *action = Some(Box::new(move |r, _| r.send(ToAgent::RemoveScreen(name))));
                            ui.close();
                        }
                    }
                }
            });
            screens.on_hover_text(t!("Its screens: {} of {} shows here; Ctrl+Alt+Shift+1, 2, ... picks one", self.monitor + 1, self.monitors.len()));
        }

        // By ID: through the relay, or straight once a path through the routers was found.
        if let Some(path) = self.sess.straight() {
            let (glyph, color, tip) = match path {
                Some(a) => (ic::LIGHTNING, crate::gui::good(), t!("Straight to the computer ({}), past the relay; sealed end to end", a)),
                None => (
                    ic::GLOBE,
                    ui.visuals().weak_text_color(),
                    t!("Through the relay, sealed end to end (it sees no picture or key press); a straight path is taken when one is found").to_string(),
                ),
            };
            ui.label(egui::RichText::new(glyph).size(16.0).color(color)).on_hover_text(tip);
        }
        // The person at a computer reached by its ID keeps some things from this viewer.
        if self.allowed != crate::grant::ALL {
            let tip = t!("The person at that computer lets you use: {}", crate::grant::shown(self.allowed));
            ui.label(egui::RichText::new(ic::SHIELD).size(17.0).color(crate::gui::warn())).on_hover_text(tip);
        }
        if icon(ui, ic::APP_WINDOW, t!("Share: the entire screen, one window or one application"), self.sharing.0 != 0)
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
                    (t!("Lock screen"), "Ctrl+Cmd+Q", Some(vec![CONTROL_L, SUPER_L, 'q' as u32])),
                    ("Spotlight", "Cmd+Space", Some(vec![SUPER_L, ' ' as u32])),
                    (t!("Switch application"), "Cmd+Tab", Some(vec![SUPER_L, xk::TAB])),
                    (t!("Mission Control"), "Ctrl+Up", Some(vec![CONTROL_L, xk::UP])),
                    (t!("Force quit"), "Option+Cmd+Esc", Some(vec![ALT_L, SUPER_L, xk::ESCAPE])),
                    (t!("Screenshot"), "Shift+Cmd+5", Some(vec![SHIFT_L, SUPER_L, '5' as u32])),
                    (t!("Close window"), "Cmd+W", Some(vec![SUPER_L, 'w' as u32])),
                    (t!("Quit application"), "Cmd+Q", Some(vec![SUPER_L, 'q' as u32])),
                ]
            } else {
                vec![
                    (t!("Secure attention"), "Ctrl+Alt+Del", None),
                    (t!("Lock screen"), "Win+L", Some(vec![SUPER_L, 'l' as u32])),
                    (t!("Start menu"), "Win", Some(vec![SUPER_L])),
                    (t!("Run"), "Win+R", Some(vec![SUPER_L, 'r' as u32])),
                    (t!("File explorer"), "Win+E", Some(vec![SUPER_L, 'e' as u32])),
                    (t!("Show desktop"), "Win+D", Some(vec![SUPER_L, 'd' as u32])),
                    (t!("Switch window"), "Alt+Tab", Some(vec![ALT_L, xk::TAB])),
                    (t!("Close window"), "Alt+F4", Some(vec![ALT_L, xk::F1 + 3])),
                    (t!("Print screen"), "PrtSc", Some(vec![xk::PRINT])),
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
            if ui.button(if mac { t!("Cmd + next key") } else { t!("Win + next key") }).clicked() {
                *action = Some(Box::new(move |r, _| {
                    r.win_next = true;
                    r.notify(if mac { t!("Press a key to send with Cmd") } else { t!("Press a key to send with Win (Space: Win alone)") });
                    Ok(())
                }));
                ui.close();
            }
            // Between a Mac and another computer: each side's shortcuts with its own keys.
            if mac != cfg!(target_os = "macos") {
                let (label, tip) = if mac {
                    (t!("Ctrl here is Cmd there"), t!("Ctrl+C, Ctrl+V and the other shortcuts work there as here; the Win key is Ctrl there"))
                } else {
                    (t!("Cmd here is Ctrl there"), t!("Cmd+C, Cmd+V and the other shortcuts work there as here; Ctrl is the Win key there"))
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
            // The account's password there, typed at its sign-in or lock screen: as typed here, or
            // as kept in this computer's password store for it.
            ui.separator();
            ui.label(egui::RichText::new(t!("Sign-in password there")).strong());
            let name = crate::vault::sign_in_name(self.host());
            let kept = *self.sign_in_kept.get_or_insert_with(|| crate::vault::get(&name).is_some_and(|p| !p.is_empty()));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.sign_in).password(true).desired_width(150.0).hint_text(t!("password")));
                if ui.add_enabled(!self.sign_in.is_empty(), egui::Button::new(t!("Type it"))).on_hover_text(t!("Typed there with Enter after it, at its sign-in or lock screen")).clicked() {
                    let (text, keep) = (std::mem::take(&mut self.sign_in), self.keep_sign_in);
                    let name = name.clone();
                    *action = Some(Box::new(move |r, _| {
                        if keep {
                            crate::vault::set(&name, &text)?;
                            r.sign_in_kept = Some(true);
                        }
                        r.type_text(&text, true)
                    }));
                    ui.close();
                }
            });
            ui.checkbox(&mut self.keep_sign_in, t!("Keep it for this computer"))
                .on_hover_text(t!("In this computer's password store, apart from the password that reaches it"));
            if kept {
                ui.horizontal(|ui| {
                    if ui.button(t!("Type the kept one")).clicked() {
                        let name = name.clone();
                        *action = Some(Box::new(move |r, _| {
                            let p = crate::vault::get(&name).ok_or(t!("No password is kept for this computer."))?;
                            r.type_text(&p, true)
                        }));
                        ui.close();
                    }
                    if ui.button(t!("Forget it")).clicked() {
                        let name = name.clone();
                        *action = Some(Box::new(move |r, _| {
                            r.sign_in_kept = Some(false);
                            crate::vault::delete(&name)
                        }));
                        ui.close();
                    }
                });
            }
            // A keyboard of another layout than the computer's: what it says on the keys, or
            // where they are (for games, and the computer's own layout).
            let mut on = self.labelled;
            if ui
                .checkbox(&mut on, t!("Type what my keys say"))
                .on_hover_text(t!("Characters as this keyboard and its input method make them (another layout, Korean, Japanese, accents). Off: keys by where they are, as the computer's own layout reads them (games)."))
                .changed()
            {
                *action = Some(Box::new(move |r, _| {
                    r.release_all()?;
                    r.labelled = on;
                    crate::hosts::set_setting("type_labelled", if on { "yes" } else { "no" });
                    Ok(())
                }));
                ui.close();
            }
        });
        keys.on_hover_text(t!("Send special keys"));
        self.computer_menu(ui, action);
        ui.separator();
        }
        // The terminal under the screen, in a pane too, as with the chat; files have none.
        if self.mode() != crate::gui::workspace::PaneMode::Files {
        let (glyph, tip) = if self.term_inline && self.terms.is_some() {
            (ic::MONITOR, t!("Back to the remote screen, with the terminal under it"))
        } else {
            (ic::TERMINAL, t!("Terminal: a shell on the host in a panel under the screen (right click for more)"))
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
            if ui.button(format!("{}  {}", ic::CORNERS_OUT, t!("Terminal in place of the screen"))).clicked() {
                // The pane's Terminal mode: the picture pauses while it is not seen.
                *action = Some(Box::new(|r, _| {
                    (r.term_detached, r.term_split) = (false, false);
                    r.set_mode(crate::gui::workspace::PaneMode::Terminal)
                }));
                ui.close();
            }
            if ui.button(format!("{}  {}", ic::TERMINAL, t!("Terminal in its own window"))).clicked() {
                *action = Some(Box::new(|r, _| {
                    if r.terms.is_none() {
                        r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    }
                    (r.term_inline, r.term_detached, r.term_split) = (false, true, false);
                    Ok(())
                }));
                ui.close();
            }
            if self.terms.is_some() && ui.button(format!("{}  {}", ic::X, t!("Close all shells"))).clicked() {
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
        let chat_tip = t!("Chat with the person at the SSH host's screen");
        let shot_tip = t!("Screenshot: save the SSH host's screen as a PNG in Pictures");
        let (pause_glyph, pause_tip) = if self.paused {
            (ic::PLAY, t!("Resume the picture"))
        } else {
            (ic::PAUSE, t!("Pause the picture to save traffic; fetch single frames when needed"))
        };
        // Chat is with the person at the screen: shown with the screen only.
        let chat = screen && crate::modules::on("chat") && icon(ui, &chat_glyph, chat_tip, self.chat.is_some()).clicked();
        let shot = screen && icon(ui, ic::CAMERA, shot_tip, false).clicked();
        // Recording: the button shows how long it has gone on.
        if screen {
            let tip = match &self.recording {
                Some(r) => {
                    let s = r.started.elapsed().as_secs();
                    t!("Recording for {}: click to stop and save it", format!("{}:{:02}", s / 60, s % 60))
                }
                None => t!("Record the screen to a video file (MP4, in Videos)").to_string(),
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
            frame = self.paused && icon(ui, ic::ARROW_CLOCKWISE, t!("Fetch one frame"), false).clicked();
        }

        // A voice call with the person at that computer: started, called off while it rings, hung up.
        if screen && crate::modules::on("sound") {
            use crate::chat::{CALL_RINGING, CALL_TALKING};
            let (glyph, tip, on) = match self.call {
                CALL_TALKING => (ic::PHONE_HANG_UP, t!("Hang up"), true),
                CALL_RINGING => (ic::PHONE, t!("Calling\u{2026} (click to call off)"), true),
                _ => (ic::PHONE, t!("Call the person at that computer: talk through the microphones"), false),
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
                ui.label(RichText::new(t!("This connection")).strong());
                if self.mirror {
                    ui.add_enabled(false, egui::Checkbox::new(&mut true, t!("View only"))).on_disabled_hover_text(t!("This computer's own screen: keys and clicks would come back to this window"));
                } else {
                    ui.checkbox(&mut self.view_only, t!("View only")).on_hover_text(t!("Watch without sending keys, clicks or the wheel"));
                }
                ui.checkbox(&mut self.share_clipboard, t!("Share clipboard")).on_hover_text(t!("Copied text goes both ways"));
                if !self.mirror && !self.web && ui.checkbox(&mut self.games, t!("Mouse for games")).on_hover_text(t!("The pointer is held in this window and its movement goes to the host, for games that turn with the mouse. Click the picture to hold it; Ctrl+Alt lets it go")).changed() {
                    // Held at once, from the menu: Ctrl+Alt or this again lets it go.
                    self.games_held = self.games;
                    if self.games {
                        self.notify(t!("The mouse is held for the game: Ctrl+Alt lets it go"));
                    }
                }
                if crate::modules::on("sound") {
                    sound_clicked = ui.checkbox(&mut sound, t!("Sound (about 8 KB/s)")).clicked();
                }
                if cfg!(windows) {
                    let tip = t!("While this screen is the active pane, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Print Screen go to the SSH host instead of this PC");
                    ui.checkbox(&mut self.grab_keys, t!("Send system keys")).on_hover_text(tip);
                } else if cfg!(target_os = "linux") {
                    let tip = t!("While this screen is the active pane and the pointer is over it, Super, Alt+Tab and the desktop's other shortcuts go to the SSH host instead of this computer");
                    ui.checkbox(&mut self.grab_keys, t!("Send system keys")).on_hover_text(tip);
                }
                ui.separator();
                if ui.button(t!("Repaint the whole screen")).clicked() {
                    *action = Some(Box::new(|r, _| r.menu('r')));
                    ui.close();
                }
                // A GNOME host shows single windows and programs with its helper only.
                if self.label.ends_with("gnome") {
                    let tip = t!("Lets you share one window or program of this GNOME host; it starts at the host's next login");
                    if ui.button(format!("{}  {}", ic::DOWNLOAD, t!("Install the window helper on this host"))).on_hover_text(tip).clicked() {
                        helper = true;
                        ui.close();
                    }
                }
            });
            more.on_hover_text(t!("Options"));
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
                ui.label(t!("Asking the SSH host for its windows..."));
            });
        }
        Some(Err(e)) => {
            ui.add_space(8.0);
            ui.label(crate::lang::tr_msg(e));
            if e.starts_with(crate::capture::NEEDS_HELPER) && e.contains("Install it") {
                ui.add_space(6.0);
                let b = crate::gui::theme::primary(t!("Install the helper on the SSH host"));
                if ui.add(b).clicked() {
                    *pick = Some(ToAgent::InstallHelper);
                }
            }
        }
        Some(Ok(wins)) if wins.is_empty() => {
            ui.label(RichText::new(t!("No windows are open on the SSH host.")).weak());
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
                let sub = if n == 1 { t!("1 window").to_string() } else { t!("{} windows", n) };
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
        let (unelevated, elevation_asked) = (self.unelevated, self.elevation_asked);
        let menu = icon_menu(ui, ic::LOCK_KEY, |ui| {
            // The sides switched: the person there gets a window onto this computer, by its ID
            // and its one-time password (which changes once used).
            #[cfg(feature = "remote")]
            {
                let here = crate::access::stored().ok().flatten().filter(|c| c.enabled && c.passwords.one_time() && crate::access::valid_id(&c.id));
                let b = ui
                    .add_enabled(here.is_some(), egui::Button::new(format!("{}  {}", ic::ARROWS_LEFT_RIGHT, t!("Switch sides"))))
                    .on_hover_text(t!("A window onto this computer opens there, with its one-time password (which changes once used): the person there sees and uses this computer"))
                    .on_disabled_hover_text(t!("Needs remote access on here, with one-time passwords (Remote access settings)"));
                if b.clicked()
                    && let Some(c) = here
                {
                    let (address, password) = (crate::access::address(&c.id, &c.relay), c.one_time.clone());
                    *action = Some(Box::new(move |r, _| r.send(ToAgent::SwitchSides { address, password })));
                    ui.close();
                }
                ui.separator();
            }
            // Its remote access without administrator rights: its person is asked for them.
            if unelevated {
                let b = ui
                    .add_enabled(!elevation_asked, egui::Button::new(format!("{}  {}", ic::SHIELD_CHECK, t!("Ask for administrator rights there"))))
                    .on_hover_text(t!("For programs run as administrator and Windows' security prompts: the person there is asked, and SSH Remote there opens again as administrator"))
                    .on_disabled_hover_text(t!("Asked: the person there is asked to allow it"));
                if b.clicked() {
                    *action = Some(Box::new(|r, _| {
                        r.elevation_asked = true;
                        r.notify(t!("Asked: the person there is asked to allow it. The session goes on by itself after a moment if they do."));
                        r.send(ToAgent::AskElevation)
                    }));
                    ui.close();
                }
                ui.separator();
            }
            if !asked {
                *action = Some(Box::new(|r, _| {
                    r.host_asked = true;
                    r.send(ToAgent::Host { act: QUERY, on: false })
                }));
            }
            let Some((can, on)) = state else {
                ui.label(egui::RichText::new(t!("Asking the computer\u{2026}")).weak());
                return;
            };
            if can == 0 {
                ui.label(egui::RichText::new(t!("The person at that computer does not let you do these.")).weak());
                return;
            }
            let able = |a: u8| can & bit(a) != 0;
            let only = t!("Not on this computer (Windows only)");
            if ui.add_enabled(able(LOCK), egui::Button::new(format!("{}  {}", ic::LOCK_KEY, t!("Lock it")))).clicked() {
                *action = Some(Box::new(|r, _| r.send(ToAgent::Host { act: LOCK, on: true })));
                ui.close();
            }
            ui.add_enabled_ui(able(RESTART), |ui| {
                ui.menu_button(format!("{}  {}", ic::ARROW_CLOCKWISE, t!("Restart it")), |ui| {
                    ui.label(egui::RichText::new(t!("Programs there close; unsaved work may be lost.")).weak());
                    if ui.add(crate::gui::kit::danger(t!("Restart now"), true)).clicked() {
                        *action = Some(Box::new(|r, _| {
                            r.restarting = true;
                            r.send(ToAgent::Host { act: RESTART, on: true })
                        }));
                        ui.close();
                    }
                });
            });
            ui.separator();
            for (act, label, tip) in [
                (BLOCK, t!("Keep its keyboard and mouse out"), t!("Whoever sits there cannot type or click; Ctrl+P there takes them back")),
                (HIDE, t!("Hide its screen"), t!("Black for whoever sits there while you still see it; its keyboard and mouse are kept out too")),
                (LOCK_ON_LEAVE, t!("Lock it when I leave"), t!("Locks the computer as this connection ends")),
            ] {
                let mut now = on & bit(act) != 0;
                let r = ui.add_enabled(able(act), egui::Checkbox::new(&mut now, label));
                let r = if able(act) { r.on_hover_text(tip) } else { r.on_disabled_hover_text(only) };
                if r.changed() {
                    *action = Some(Box::new(move |r, _| r.send(ToAgent::Host { act, on: now })));
                }
            }
        });
        menu.on_hover_text(t!("This computer: lock, restart, keep its keyboard out, hide its screen"));
    }
}
