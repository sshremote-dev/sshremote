//! The left column: this computer as others reach it. Its addresses (the remote access ID, its
//! IPs, its name, `user@IP` when an SSH server answers here), then remote access: its switch, the
//! password, its state and who is connected. Remote access's settings open in a sheet.

use super::*;

impl App {
    pub(super) fn this_computer(&mut self, ui: &mut egui::Ui) {
        self.look_here(ui.ctx());
        let here = self.here.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
        heading(ui, "This computer");
        let name = if here.name.is_empty() { "\u{2026}" } else { here.name.as_str() };
        let r = ui.add(egui::Label::new(RichText::new(name).size(19.0).strong()).truncate().sense(egui::Sense::click()));
        if r.on_hover_text("Click to copy the name").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            ui.ctx().copy_text(here.name.clone());
        }
        ui.add_space(14.0);
        #[cfg(feature = "remote")]
        let access = if crate::modules::on("screen") { self.access_status(&ui.ctx().clone()) } else { None };
        heading(ui, "Others reach it at");
        #[cfg(feature = "remote")]
        if let Some((cfg, _)) = access.as_ref().filter(|(c, _)| c.enabled) {
            address_row(ui, "ID", &crate::computers::spaced(&cfg.id), &cfg.id, "the remote access ID");
        }
        for ip in here.ips.iter().take(3) {
            address_row(ui, "IP", &ip.to_string(), &ip.to_string(), "address");
        }
        // Its name is the heading; a longer one the network knows it by is shown here.
        if !here.dns.is_empty() {
            address_row(ui, "Name", &here.dns, &here.dns, "full name");
        }
        if let Some(at) = here.ssh_address() {
            address_row(ui, "SSH", &at, &at, "SSH address");
        }
        if here.ips.is_empty() {
            let text = if here.name.is_empty() { "Looking\u{2026}" } else { "No network address now." };
            ui.label(RichText::new(text).weak().size(12.5));
        }
        #[cfg(feature = "remote")]
        if crate::modules::on("screen") {
            ui.add_space(18.0);
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

    /// Remote access by ID: on or off; on, its password, its state and who is connected.
    #[cfg(feature = "remote")]
    fn remote_access(&mut self, ui: &mut egui::Ui, access: Option<(crate::access::Config, super::super::access_ui::Service)>) {
        use crate::access;
        heading(ui, "Remote access by ID");
        let Some((mut cfg, svc)) = access else {
            let why = self.access.problem.clone().unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(format!("Not set up: {why}")).color(bad())).wrap());
            ui.horizontal(|ui| {
                if cfg!(windows) && !crate::admin::elevated() && ui.button(format!("{}  As administrator", ic::SHIELD_CHECK)).clicked() {
                    self.restart_elevated(ui.ctx());
                }
                if ui.button("Try again").clicked() {
                    self.access.problem = None;
                }
            });
            return;
        };
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Let others connect by ID");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut on = cfg.enabled;
                if super::super::access_ui::toggle(ui, &mut on).on_hover_text(if on { "Turn remote access off" } else { "Turn remote access on" }).clicked() {
                    cfg.enabled = on;
                    changed = true;
                }
            });
        });
        if !cfg.enabled {
            ui.add(egui::Label::new(RichText::new("Off: nobody can connect by ID. On, anyone with the ID and the password can see and use this computer, through the relay.").weak().size(12.0)).wrap());
        } else {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Password").weak().size(12.0));
                let shown = if self.access.show_password { cfg.password.clone() } else { "\u{2022}".repeat(cfg.password.chars().count()) };
                ui.label(RichText::new(shown).monospace().size(14.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon(ui, ic::ARROWS_CLOCKWISE, "A new random password (viewers need the new one)", false).clicked() {
                        cfg.password = access::new_password();
                        self.access.show_password = true;
                        changed = true;
                    }
                    if icon(ui, ic::COPY, "Copy the password", false).clicked() {
                        ui.ctx().copy_text(cfg.password.clone());
                    }
                    let (g, t) = if self.access.show_password { (ic::EYE_SLASH, "Hide the password") } else { (ic::EYE, "Show the password") };
                    if icon(ui, g, t, false).clicked() {
                        self.access.show_password = !self.access.show_password;
                    }
                });
            });
            let (color, text) = self.access_state(&cfg, svc, ui.visuals().weak_text_color());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 16.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, color);
                ui.add(egui::Label::new(RichText::new(text).color(color).size(12.0)).wrap());
            });
            ui.add_space(6.0);
            // Who is in now: served by this window, each with a way to end it; by the service,
            // which runs apart from it.
            if svc.runs {
                ui.label(RichText::new("The service serves the viewers, with no window needed.").weak().size(12.0));
            } else {
                let viewers = access::viewers();
                if viewers.is_empty() {
                    ui.label(RichText::new("Nobody is connected.").weak().size(12.0));
                }
                for (n, since) in viewers {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(ic::DESKTOP).size(14.0));
                        ui.label(format!("Connected {}", crate::hosts::ago(since)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Disconnect").clicked() {
                                access::disconnect(n);
                            }
                        });
                    });
                }
            }
        }
        ui.add_space(6.0);
        if ui.add(egui::Button::new(RichText::new(format!("{}  Remote access settings", ic::SLIDERS_HORIZONTAL)).size(12.5)).frame_when_inactive(false)).clicked() {
            // The relay field starts from the one in use; emptied, Use goes back to the default.
            self.access.relay = cfg.relay.clone();
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
            let (modal, fit) = super::super::modal(ctx, "access-settings", Vec2::new(520.0, 0.0));
            let r = modal.show(ctx, |ui| {
                ui.set_width(fit.x);
                ui.label(RichText::new(format!("{}  Remote access settings", ic::GLOBE)).size(16.0).strong());
                ui.add_space(10.0);
                ui.label(RichText::new("Start with the computer").strong());
                let what = if cfg!(windows) {
                    "The SSH Remote service: reachable after a restart, at the sign-in and lock screens, with no window open."
                } else {
                    "A systemd user service: reachable whenever you are signed in, with no window open."
                };
                ui.add(egui::Label::new(RichText::new(what).weak().size(12.0)).wrap());
                ui.horizontal(|ui| {
                    let admin = !cfg!(windows) || crate::admin::elevated();
                    if svc.installed && !svc.this {
                        if ui.add_enabled(admin, theme::primary(format!("{}  Update the service", ic::DOWNLOAD))).on_disabled_hover_text("Needs administrator rights").clicked() {
                            service = Some(true);
                        }
                    } else if svc.installed {
                        ui.label(RichText::new(format!("{}  Installed", ic::CHECK)).color(good()));
                        if ui.add_enabled(admin, egui::Button::new("Remove")).on_disabled_hover_text("Needs administrator rights").clicked() {
                            service = Some(false);
                        }
                    } else if admin {
                        if ui.add(theme::primary(format!("{}  Install", ic::DOWNLOAD))).clicked() {
                            cfg.enabled = true;
                            changed = true;
                            service = Some(true);
                        }
                    } else if ui.add(theme::primary(format!("{}  Restart as administrator to install", ic::SHIELD_CHECK))).clicked() {
                        restart = true;
                    }
                });
                if !svc.installed {
                    ui.label(RichText::new("Without it, remote access works while this window is open.").weak().size(12.0));
                }
                ui.add_space(12.0);
                egui::Grid::new("access-settings-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Your own password");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.access.new_password).password(true).hint_text("8 characters or more").desired_width(200.0));
                        if ui.add_enabled(self.access.new_password.trim().chars().count() >= 8, egui::Button::new("Set")).clicked() {
                            cfg.password = self.access.new_password.trim().to_string();
                            self.access.new_password.clear();
                            changed = true;
                        }
                    });
                    ui.end_row();
                    ui.label("Relay").on_hover_text("sshremote.dev, or your own: sshremote-relay on a server");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.access.relay).desired_width(200.0));
                        if ui.add_enabled(self.access.relay.trim() != cfg.relay, egui::Button::new("Use")).clicked() {
                            let r = self.access.relay.trim();
                            cfg.relay = if r.is_empty() { access::DEFAULT_RELAY.into() } else { r.to_string() };
                            self.access.relay = cfg.relay.clone();
                            changed = true;
                        }
                    });
                    ui.end_row();
                    ui.label("ID");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::computers::spaced(&cfg.id)).monospace());
                        if ui.button("Take a new ID").on_hover_text("Those who know the old ID can no longer use it").clicked() {
                            cfg.id = access::new_id();
                            changed = true;
                        }
                    });
                    ui.end_row();
                });
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.button("Done").clicked()).inner
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

/// One way to reach this computer: what it is, the value, and a button that copies it (a click
/// on the value copies it too).
fn address_row(ui: &mut egui::Ui, label: &str, shown: &str, value: &str, what: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), egui::Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.0), ui.visuals().weak_text_color());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let copy = icon(ui, ic::COPY, &format!("Copy the {what}"), false).clicked();
            let r = ui
                .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(shown).monospace().size(14.0)).truncate().sense(egui::Sense::click()))
                })
                .inner
                .on_hover_text(format!("Click to copy the {what}"))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if copy || r.clicked() {
                ui.ctx().copy_text(value.to_string());
            }
        });
    });
}
