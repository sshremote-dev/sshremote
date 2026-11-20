//! The left column: this computer as others reach it. A card of its addresses (the remote access
//! ID, its IPs, its full name, `user@IP` when an SSH server answers here), then a card of remote
//! access: its switch, state, password and who is connected. Its settings open in a sheet.

use super::*;
use crate::gui::kit;

impl App {
    pub(super) fn this_computer(&mut self, ui: &mut egui::Ui) {
        self.look_here(ui.ctx());
        let here = self.here.lock().unwrap_or_else(|e| e.into_inner()).clone();
        #[cfg(feature = "remote")]
        let access = if crate::modules::on("screen") { self.access_status(&ui.ctx().clone()) } else { None };
        kit::card(ui, |ui| {
            ui.horizontal(|ui| {
                kit::mark(ui, ic::HOUSE, super::super::brand());
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new("This computer").size(11.5).color(ui.visuals().weak_text_color()));
                    let name = here.as_ref().map_or("", |h| h.name.as_str());
                    ui.add(egui::Label::new(RichText::new(if name.is_empty() { "\u{2026}" } else { name }).size(17.0).strong()).truncate());
                });
            });
            ui.add_space(8.0);
            kit::heading(ui, "Others reach it at");
            let Some(here) = here else {
                kit::loading(ui, "Finding its addresses\u{2026}");
                return;
            };
            ui.spacing_mut().item_spacing.y = 2.0;
            #[cfg(feature = "remote")]
            if let Some((cfg, _)) = access.as_ref().filter(|(c, _)| c.enabled) {
                address_row(ui, "ID", &crate::computers::spaced(&cfg.id), &cfg.id, "the remote access ID");
            }
            for ip in here.ips.iter().take(3) {
                address_row(ui, "IP", &ip.to_string(), &ip.to_string(), "address");
            }
            if !here.dns.is_empty() {
                address_row(ui, "DNS", &here.dns, &here.dns, "full name");
            }
            if let Some(at) = here.ssh_address() {
                address_row(ui, "SSH", &at, &at, "SSH address");
            }
            if here.ips.is_empty() {
                ui.label(RichText::new("No network address now.").weak().size(12.5));
            }
        });
        #[cfg(feature = "remote")]
        if crate::modules::on("screen") {
            ui.add_space(kit::GAP);
            self.remote_access(ui, access);
        }
    }

    /// This computer's names and addresses, looked up off the window's thread every half
    /// minute; its SSH server every ten (each look shows in that server's log).
    fn look_here(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        if self.here_at.is_some_and(|t| now - t < std::time::Duration::from_secs(30)) {
            return;
        }
        self.here_at = Some(now);
        let ssh = self.here_ssh_at.is_none_or(|t| now - t > std::time::Duration::from_secs(600));
        if ssh {
            self.here_ssh_at = Some(now);
        }
        let (slot, ctx) = (self.here.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut h = crate::here::addresses();
            h.ssh_port = if ssh { crate::here::ssh_port() } else { slot.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|o| o.ssh_port) };
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
            ctx.request_repaint();
        });
    }

    /// Remote access by ID: on or off; on, its state, its password and who is connected.
    #[cfg(feature = "remote")]
    fn remote_access(&mut self, ui: &mut egui::Ui, access: Option<(crate::access::Config, super::super::access_ui::Service)>) {
        use crate::access;
        let mut changed = false;
        let mut open_settings = false;
        let mut restart = false;
        let cfg = kit::card(ui, |ui| {
            let Some((mut cfg, svc)) = access else {
                ui.label(RichText::new("Remote access by ID").strong());
                ui.add_space(4.0);
                let why = self.access.problem.clone().unwrap_or_default();
                kit::notice(ui, kit::Tone::Bad, "Its settings cannot be read or kept", &why);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if cfg!(windows) && !crate::admin::elevated() && ui.add(kit::secondary(format!("{}  As administrator", ic::SHIELD_CHECK))).clicked() {
                        restart = true;
                    }
                    if ui.add(kit::secondary("Try again")).clicked() {
                        self.access.problem = None;
                    }
                });
                return None;
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("Remote access by ID").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut on = cfg.enabled;
                    if super::super::access_ui::toggle(ui, &mut on).on_hover_text(if on { "Turn remote access off" } else { "Turn remote access on" }).clicked() {
                        cfg.enabled = on;
                        changed = true;
                    }
                });
            });
            if !cfg.enabled {
                ui.add(
                    egui::Label::new(RichText::new("Off. On, anyone with this computer's ID and password can see and use it, through the relay.").weak().size(12.5))
                        .wrap(),
                );
            } else {
                let (color, text) = self.access_state(&cfg, svc, ui.visuals().weak_text_color());
                ui.horizontal(|ui| {
                    if text.ends_with('\u{2026}') {
                        ui.add(egui::Spinner::new().size(12.0).color(color));
                    } else {
                        kit::dot(ui, color);
                    }
                    ui.add(egui::Label::new(RichText::new(text).color(color).size(12.5)).wrap());
                });
                ui.add_space(6.0);
                // The passwords viewers may give: the lasting one, the one-time one, or both, each
                // shown on asking, copied, or made anew.
                let rows: Vec<(bool, &str)> = [(cfg.passwords.lasting(), "Password"), (cfg.passwords.one_time(), "One-time")].into_iter().filter(|r| r.0).collect();
                for (_, name) in rows {
                    let once = name == "One-time";
                    let value = if once { cfg.one_time.clone() } else { cfg.password.clone() };
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(Vec2::new(64.0, 26.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(64.0);
                            let l = ui.label(RichText::new(name).weak().size(12.5));
                            if once {
                                l.on_hover_text("Works for one connection: a minute after it ends, a new one takes its place");
                            }
                        });
                        let shown = if self.access.show_password { value.clone() } else { "\u{2022}".repeat(value.chars().count()) };
                        ui.label(RichText::new(shown).monospace().size(14.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let fresh = if once { "A new one-time password now" } else { "A new random password (viewers need the new one)" };
                            if small_icon(ui, ic::ARROWS_CLOCKWISE, fresh).clicked() {
                                if once {
                                    cfg.one_time = crate::net::new_one_time();
                                } else {
                                    cfg.password = access::new_password();
                                }
                                self.access.show_password = true;
                                changed = true;
                            }
                            let at = egui::Id::unique(if once { "copied-one-time" } else { "copied-password" });
                            let (g, t) = if kit::copied(ui, at) { (ic::CHECK, "Copied") } else { (ic::COPY, "Copy the password") };
                            if small_icon(ui, g, t).clicked() {
                                kit::copy(ui, &value, at);
                            }
                            let (g, t) = if self.access.show_password { (ic::EYE_SLASH, "Hide the passwords") } else { (ic::EYE, "Show the passwords") };
                            if small_icon(ui, g, t).clicked() {
                                self.access.show_password = !self.access.show_password;
                            }
                        });
                    });
                }
                ui.add_space(6.0);
                kit::heading(ui, "Connected now");
                // Served by this window or by the service: each with how it came and a way to end it.
                let viewers = if svc.runs { access::service_viewers() } else { access::viewers() };
                if viewers.is_empty() {
                    ui.label(RichText::new("Nobody.").weak().size(12.5));
                }
                for v in viewers {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(ic::DESKTOP).size(14.0).color(good()));
                        ui.label(RichText::new(format!("A viewer, {}", v.describe())).size(13.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(kit::danger("End", false).min_size(Vec2::new(0.0, 26.0))).on_hover_text("End this connection now").clicked() {
                                if svc.runs {
                                    if let Err(e) = access::ask_to_end(v.n) {
                                        self.error = Some(e);
                                    }
                                } else {
                                    access::disconnect(v.n);
                                }
                            }
                        });
                    });
                }
                // The service's list is read again while it is shown.
                if svc.runs {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
                }
                ui.add_space(6.0);
                kit::heading(ui, "Lately");
                let lately = access::visits(6);
                if lately.is_empty() {
                    ui.label(RichText::new("Nobody has come by the ID yet.").weak().size(12.5));
                }
                for v in lately {
                    let (mark, colour) = if v.let_in { (ic::DESKTOP, ui.visuals().weak_text_color()) } else { (ic::WARNING, warn()) };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(mark).size(13.0).color(colour));
                        ui.add(egui::Label::new(RichText::new(v.describe()).size(12.0)).wrap());
                    });
                }
            }
            ui.add_space(4.0);
            ui.separator();
            open_settings = ui.add(kit::ghost(format!("{}  Settings\u{2026}", ic::SLIDERS_HORIZONTAL))).on_hover_text("Start with the computer, your own password, the relay, a new ID").clicked();
            Some(cfg)
        });
        if restart {
            self.restart_elevated(ui.ctx());
        }
        let Some(cfg) = cfg else { return };
        if open_settings {
            // The relay field starts from the one in use; emptied, Use goes back to the default.
            self.access.relay = cfg.relay.clone();
            self.access.from = cfg.from.clone();
            self.access_sheet_open = true;
        }
        if changed {
            self.save_access(cfg);
        }
    }

    /// Remote access's settings: starting with the computer, a password of one's own, the relay,
    /// and a new ID.
    pub(super) fn access_sheet(&mut self, ctx: &egui::Context) {
        if !self.access_sheet_open {
            return;
        }
        #[cfg(feature = "remote")]
        {
            use crate::access;
            let Some((mut cfg, svc)) = self.access_status(ctx) else {
                self.access_sheet_open = false;
                return;
            };
            let mut changed = false;
            let mut service: Option<bool> = None;
            let mut restart = false;
            let (modal, fit) = super::super::modal(ctx, "access-settings", Vec2::new(560.0, 0.0));
            let r = modal.show(ctx, |ui| {
                ui.set_width(fit.x);
                kit::title(ui, "Remote access settings", "How this computer is reached by its ID.");
                // What does not fit the window scrolls, between the title and Done.
                let room = (ctx.content_rect().height() - 220.0).max(160.0);
                egui::ScrollArea::vertical().id_salt("access-settings-body").max_height(room).auto_shrink([false, true]).show(ui, |ui| {
                    kit::heading(ui, "Start with the computer");
                    kit::card(ui, |ui| {
                        let what = if cfg!(windows) {
                            "The SSH Remote service: reachable after a restart, at the sign-in and lock screens, with no window open."
                        } else {
                            "A systemd user service: reachable whenever you are signed in, with no window open."
                        };
                        ui.add(egui::Label::new(RichText::new(what).size(12.5)).wrap());
                        ui.add_space(6.0);
                        let admin = !cfg!(windows) || crate::admin::elevated();
                        ui.horizontal(|ui| {
                            if !svc.known {
                                kit::loading(ui, "Looking for the service\u{2026}");
                            } else if svc.installed && !svc.this {
                                ui.label(RichText::new(format!("{}  An older copy is installed", ic::WARNING)).color(warn()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled(admin, theme::primary(format!("{}  Update it", ic::DOWNLOAD))).on_disabled_hover_text("Needs administrator rights").clicked() {
                                        service = Some(true);
                                    }
                                });
                            } else if svc.installed {
                                ui.label(RichText::new(format!("{}  Installed{}", ic::CHECK_CIRCLE, if svc.runs { " and running" } else { "" })).color(good()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled(admin, kit::danger("Remove", false)).on_disabled_hover_text("Needs administrator rights").clicked() {
                                        service = Some(false);
                                    }
                                });
                            } else {
                                ui.label(RichText::new("Not installed: remote access works while this window is open.").weak().size(12.5));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if admin {
                                        if ui.add(theme::primary(format!("{}  Install", ic::DOWNLOAD))).clicked() {
                                            cfg.enabled = true;
                                            changed = true;
                                            service = Some(true);
                                        }
                                    } else if ui.add(kit::secondary(format!("{}  As administrator", ic::SHIELD_CHECK))).on_hover_text("Installing needs administrator rights").clicked() {
                                        restart = true;
                                    }
                                });
                            }
                        });
                    });
                    ui.add_space(kit::GAP);
                    kit::heading(ui, "Access");
                    kit::card(ui, |ui| {
                        // Which passwords viewers may give.
                        kit::row(ui, "Passwords", |ui| {
                            let mut i = access::Passwords::ALL.iter().position(|p| *p == cfg.passwords).unwrap_or(0);
                            let names: Vec<&str> = access::Passwords::ALL.iter().map(|p| p.label()).collect();
                            if kit::segmented(ui, &mut i, &names) {
                                cfg.passwords = access::Passwords::ALL[i];
                                changed = true;
                            }
                        });
                        ui.add(
                            egui::Label::new(
                                RichText::new(match cfg.passwords {
                                    access::Passwords::Lasting => "Viewers give the lasting password, until you change it.",
                                    access::Passwords::OneTime => "Viewers give the one-time password shown here: it works for one connection, and a minute after that ends a new one takes its place, so nobody comes back later with it.",
                                    access::Passwords::Both => "Viewers give either: the one-time password to someone helping once, the lasting one for yourself.",
                                })
                                .weak()
                                .size(12.0),
                            )
                            .wrap(),
                        );
                        kit::row(ui, "Your own password", |ui| {
                            let ok = self.access.new_password.trim().chars().count() >= 8 && !crate::net::looks_one_time(&self.access.new_password);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(ok, kit::secondary("Set")).clicked() {
                                    cfg.password = self.access.new_password.trim().to_string();
                                    self.access.new_password.clear();
                                    changed = true;
                                }
                                ui.add(kit::field(&mut self.access.new_password, "8 characters or more").password(true));
                            });
                        });
                        kit::row(ui, "ID", |ui| {
                            ui.label(RichText::new(crate::computers::spaced(&cfg.id)).monospace().size(14.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(kit::secondary("Take a new ID")).on_hover_text("Those who know the old ID can no longer use it").clicked() {
                                    cfg.id = access::new_id();
                                    changed = true;
                                }
                            });
                        });
                        kit::row(ui, "Relay", |ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(self.access.relay.trim() != cfg.relay, kit::secondary("Use")).clicked() {
                                    let r = self.access.relay.trim();
                                    cfg.relay = if r.is_empty() { access::DEFAULT_RELAY.into() } else { crate::computers::relay_name(r) };
                                    self.access.relay = cfg.relay.clone();
                                    changed = true;
                                }
                                ui.add(kit::field(&mut self.access.relay, access::DEFAULT_RELAY));
                            });
                        });
                        ui.add(
                            egui::Label::new(RichText::new("The relay is where both ends meet; it passes on what it cannot read. relay.sshremote.dev, or sshremote-relay on a server of your own. Computers added by ID go through it too.").weak().size(12.0))
                                .wrap(),
                        );
                    });
                    ui.add_space(kit::GAP);
                    kit::heading(ui, "Who comes in, and what they may do");
                    kit::card(ui, |ui| {
                        if ui.checkbox(&mut cfg.ask, "Ask the person at this computer before letting anyone in").changed() {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(RichText::new("With the right password a viewer waits for a click here; nobody answering within a minute turns them away. Off: the password is enough, for a computer nobody sits at.").weak().size(12.0))
                                .wrap(),
                        );
                        ui.add_space(8.0);
                        if ui.checkbox(&mut cfg.requests, "Let people without the password ask to come in").changed() {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new("Someone helping you needs only this computer's ID: you are asked each time, and both screens show a code to compare first. One at a time.").weak().size(12.0),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new("Viewers may").strong());
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for (bit, label, _) in crate::grant::RIGHTS {
                                let mut on = cfg.allow & bit != 0;
                                if ui.checkbox(&mut on, label).changed() {
                                    cfg.allow = if on { cfg.allow | bit } else { cfg.allow & !bit };
                                    changed = true;
                                }
                            }
                        });
                        ui.add(
                            egui::Label::new(RichText::new("They always see the screen. The person at the computer can change these for a session in its window.").weak().size(12.0)).wrap(),
                        );
                        ui.add_space(4.0);
                        if ui.checkbox(&mut cfg.cameras, "Let viewers look at this computer's cameras").changed() {
                            changed = true;
                        }
                        kit::row(ui, "End a session left idle", |ui| {
                            let name = |m: u32| if m == 0 { "Never".to_string() } else { format!("After {m} min without input") };
                            egui::ComboBox::from_id_salt("idle-end").selected_text(name(cfg.idle)).show_ui(ui, |ui| {
                                for m in [0, 5, 15, 30, 60, 120] {
                                    changed |= ui.selectable_value(&mut cfg.idle, m, name(m)).changed();
                                }
                            });
                        });
                        ui.add(
                            egui::Label::new(RichText::new("They are listed after the monitors; the person at the computer is told each time one is looked at.").weak().size(12.0)).wrap(),
                        );
                        ui.add_space(8.0);
                        if ui.checkbox(&mut cfg.lan, "Let computers on this network find this one").changed() {
                            changed = true;
                        }
                        ui.add(egui::Label::new(RichText::new("They are told its ID, name and network card, never its password, and can add it without typing the ID.").weak().size(12.0)).wrap());
                        ui.add_space(8.0);
                        ui.label(RichText::new("Two-step sign-in").strong());
                        if !cfg.totp.is_empty() {
                            ui.horizontal(|ui| {
                                kit::dot(ui, good());
                                ui.label("On: viewers give the code of your authenticator app with the password.");
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add(kit::danger("Turn off", false)).clicked() {
                                        cfg.totp.clear();
                                        changed = true;
                                    }
                                });
                            });
                        } else if let Some(secret) = self.access.totp_new.clone() {
                            ui.add(egui::Label::new(RichText::new("In your authenticator app (Google Authenticator, Microsoft Authenticator, 1Password, Aegis…), add an account by its setup key:").size(12.5)).wrap());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(crate::totp::grouped(&secret)).monospace().size(14.0).strong());
                                let at = egui::Id::unique("totp-copy");
                                let copied = kit::copied(ui, at);
                                if ui.add(kit::ghost(if copied { "Copied" } else { "Copy" })).clicked() {
                                    kit::copy(ui, &secret, at);
                                }
                                if ui.add(kit::ghost("Copy as a link")).on_hover_text("otpauth://, for apps that take a link").clicked() {
                                    kit::copy(ui, &crate::totp::uri(&secret, &cfg.id), at);
                                }
                            });
                            ui.add(egui::Label::new(RichText::new("Or scan it with the app's camera:").size(12.5)).wrap());
                            qr(ui, &crate::totp::uri(&secret, &cfg.id), 200.0);
                            kit::row(ui, "Its code now", |ui| {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let ok = crate::totp::check(&secret, &self.access.totp_code);
                                    if ui.add_enabled(ok, kit::secondary("Turn on")).on_disabled_hover_text("Type the six digits the app shows").clicked() {
                                        cfg.totp = secret.clone();
                                        (self.access.totp_new, self.access.totp_code) = (None, String::new());
                                        changed = true;
                                    }
                                    if ui.add(kit::ghost("Cancel")).clicked() {
                                        (self.access.totp_new, self.access.totp_code) = (None, String::new());
                                    }
                                    ui.add(kit::field(&mut self.access.totp_code, "six digits"));
                                });
                            });
                        } else {
                            ui.add(egui::Label::new(RichText::new("Off: the password is enough. On, a viewer also gives the six-digit code of your authenticator app.").weak().size(12.0)).wrap());
                            if ui.add(kit::secondary("Set up\u{2026}")).clicked() {
                                self.access.totp_new = Some(crate::totp::new_secret());
                            }
                        }
                        ui.add_space(8.0);
                        if ui.checkbox(&mut cfg.direct, "Let viewers on this network come straight in").changed() {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(RichText::new(format!("On port {}, with the same password, without the relay: quicker, and nothing leaves the network.", crate::net::DIRECT_PORT)).weak().size(12.0))
                                .wrap(),
                        );
                        ui.add_space(8.0);
                        if ui.checkbox(&mut cfg.punch, "Let viewers by ID go straight through the routers").changed() {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new("Once the password is right, the two computers look for a path between them that skips the relay, and move to it if there is one: quicker, and the relay no longer carries the session. Each then knows the other's address.")
                                    .weak()
                                    .size(12.0),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new("Only from these addresses").strong());
                        let bad = access::bad_ranges(&self.access.from);
                        let typed = access::tidy_ranges(&self.access.from);
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 70.0).max(120.0);
                            ui.add(egui::TextEdit::singleline(&mut self.access.from).hint_text("Anywhere").desired_width(w).margin(egui::Margin::symmetric(8, 6)));
                            if ui.add_enabled(bad.is_empty() && typed != cfg.from, kit::secondary("Use")).clicked() {
                                cfg.from = typed.clone();
                                self.access.from = typed;
                                changed = true;
                            }
                        });
                        if !bad.is_empty() {
                            ui.label(RichText::new(format!("Not an address or a range: {}", bad.join(", "))).color(super::super::bad()).size(12.0));
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new("Addresses or ranges, like 203.0.113.7 or 192.168.1.0/24: nobody comes in from anywhere else, even with the password. Through the relay, it is where the relay sees the viewer from; through a web page, the address of its server. Empty: from anywhere.")
                                    .weak()
                                    .size(12.0),
                            )
                            .wrap(),
                        );
                    });
                });
                ui.add_space(kit::GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.add(theme::primary("Done")).clicked()).inner
            });
            if changed {
                self.save_access(cfg);
            }
            if let Some(install) = service {
                self.service_change(install);
            }
            if restart {
                self.restart_elevated(ctx);
            }
            if r.inner || r.should_close() {
                self.access_sheet_open = false;
            }
        }
        #[cfg(not(feature = "remote"))]
        {
            let _ = ctx;
            self.access_sheet_open = false;
        }
    }
}

/// A small icon button, for a row of several close together.
fn small_icon(ui: &mut egui::Ui, glyph: &str, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(glyph).size(14.0)).frame_when_inactive(false).min_size(Vec2::splat(26.0))).on_hover_text(tip)
}

/// One way to reach this computer: what it is, the value, and a button that copies it (a click on
/// the row copies it too, and says so for a moment).
fn address_row(ui: &mut egui::Ui, label: &str, shown: &str, value: &str, what: &str) {
    let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), egui::Sense::click());
    let at = egui::Id::unique("copied").with((label, value));
    let done = kit::copied(ui, at);
    if r.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(theme::R_SM), ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let p = ui.painter();
    let weak = ui.visuals().weak_text_color();
    p.text(rect.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(12.0), weak);
    // The value, cut short before the copy mark when it is long.
    let room = rect.width() - 50.0 - 30.0;
    let galley = ui.painter().layout(shown.to_string(), FontId::monospace(13.5), ui.visuals().text_color(), f32::INFINITY);
    let text = if galley.size().x > room {
        let keep = ((shown.chars().count() as f32) * room / galley.size().x).floor() as usize;
        format!("{}\u{2026}", shown.chars().take(keep.saturating_sub(1)).collect::<String>())
    } else {
        shown.to_string()
    };
    p.text(rect.left_center() + Vec2::new(50.0, 0.0), Align2::LEFT_CENTER, text, FontId::monospace(13.5), ui.visuals().text_color());
    let (glyph, color) = if done { (ic::CHECK, good()) } else { (ic::COPY, if r.hovered() { ui.visuals().text_color() } else { weak }) };
    p.text(rect.right_center() - Vec2::new(12.0, 0.0), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), color);
    let tip = if done { "Copied".to_string() } else { format!("Click to copy the {what}: {value}") };
    if r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        kit::copy(ui, value, at);
    }
}

/// `text` as a QR code `side` points wide: dark squares on white with the quiet border around them
/// that cameras look for, in either theme.
fn qr(ui: &mut egui::Ui, text: &str, side: f32) {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M) else { return };
    let n = code.width();
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::WHITE);
    // Four modules of white on each side, as the standard asks.
    let cell = side / (n + 8) as f32;
    let at = rect.min + egui::Vec2::splat(cell * 4.0);
    for (i, c) in code.to_colors().iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (x, y) = ((i % n) as f32, (i / n) as f32);
            let r = egui::Rect::from_min_size(at + egui::Vec2::new(x * cell, y * cell), egui::Vec2::splat(cell));
            // A hair larger, so no seams show between neighbours.
            painter.rect_filled(r.expand(0.25), 0.0, egui::Color32::BLACK);
        }
    }
}
