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
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(Vec2::new(64.0, 26.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_width(64.0);
                        ui.label(RichText::new("Password").weak().size(12.5));
                    });
                    let shown = if self.access.show_password { cfg.password.clone() } else { "\u{2022}".repeat(cfg.password.chars().count()) };
                    ui.label(RichText::new(shown).monospace().size(14.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if small_icon(ui, ic::ARROWS_CLOCKWISE, "A new random password (viewers need the new one)").clicked() {
                            cfg.password = access::new_password();
                            self.access.show_password = true;
                            changed = true;
                        }
                        let at = egui::Id::unique("copied-password");
                        let (g, t) = if kit::copied(ui, at) { (ic::CHECK, "Copied") } else { (ic::COPY, "Copy the password") };
                        if small_icon(ui, g, t).clicked() {
                            kit::copy(ui, &cfg.password, at);
                        }
                        let (g, t) = if self.access.show_password { (ic::EYE_SLASH, "Hide the password") } else { (ic::EYE, "Show the password") };
                        if small_icon(ui, g, t).clicked() {
                            self.access.show_password = !self.access.show_password;
                        }
                    });
                });
                ui.add_space(6.0);
                kit::heading(ui, "Connected now");
                // Served by this window, each with a way to end it; by the service, apart from it.
                if svc.runs {
                    ui.label(RichText::new("The service serves them, with no window needed.").weak().size(12.5));
                } else {
                    let viewers = access::viewers();
                    if viewers.is_empty() {
                        ui.label(RichText::new("Nobody.").weak().size(12.5));
                    }
                    for (n, since) in viewers {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(ic::DESKTOP).size(14.0).color(good()));
                            ui.label(RichText::new(format!("A viewer, {}", crate::hosts::ago(since))).size(13.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(kit::danger("Disconnect", false).min_size(Vec2::new(0.0, 26.0))).clicked() {
                                    access::disconnect(n);
                                }
                            });
                        });
                    }
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
                    kit::row(ui, "Your own password", |ui| {
                        let ok = self.access.new_password.trim().chars().count() >= 8;
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
                                cfg.relay = if r.is_empty() { access::DEFAULT_RELAY.into() } else { r.to_string() };
                                self.access.relay = cfg.relay.clone();
                                changed = true;
                            }
                            ui.add(kit::field(&mut self.access.relay, access::DEFAULT_RELAY));
                        });
                    });
                    ui.add(
                        egui::Label::new(RichText::new("The relay is where both ends meet; it passes on what it cannot read. sshremote.dev, or sshremote-relay on a server of your own. Computers added by ID go through it too.").weak().size(12.0))
                            .wrap(),
                    );
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
