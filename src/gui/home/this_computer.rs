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
            // The team that manages it, or the person's, over it.
            #[cfg(feature = "remote")]
            super::super::team::branding(ui);
            ui.horizontal(|ui| {
                kit::mark(ui, ic::HOUSE, super::super::brand());
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(t!("This computer")).size(11.5).color(ui.visuals().weak_text_color()));
                    let name = here.as_ref().map_or("", |h| h.name.as_str());
                    ui.add(egui::Label::new(RichText::new(if name.is_empty() { "\u{2026}" } else { name }).size(17.0).strong()).truncate());
                });
            });
            ui.add_space(8.0);
            kit::heading(ui, t!("Others reach it at"));
            let Some(here) = here else {
                kit::loading(ui, t!("Finding its addresses\u{2026}"));
                return;
            };
            ui.spacing_mut().item_spacing.y = 2.0;
            #[cfg(feature = "remote")]
            if let Some((cfg, _)) = access.as_ref().filter(|(c, _)| c.enabled) {
                address_row(ui, t!("ID"), &crate::computers::spaced(&cfg.id), &cfg.id, &t!("Click to copy the remote access ID: {}", cfg.id));
            }
            for ip in here.ips.iter().take(3) {
                address_row(ui, "IP", &ip.to_string(), &ip.to_string(), &t!("Click to copy the address: {}", ip));
            }
            if !here.dns.is_empty() {
                address_row(ui, "DNS", &here.dns, &here.dns, &t!("Click to copy the full name: {}", here.dns));
            }
            if let Some(at) = here.ssh_address() {
                address_row(ui, "SSH", &at, &at, &t!("Click to copy the SSH address: {}", at));
            }
            if here.ips.is_empty() {
                ui.label(RichText::new(t!("No network address now.")).weak().size(12.5));
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
                ui.label(RichText::new(t!("Remote access by ID")).strong());
                ui.add_space(4.0);
                let why = crate::lang::tr_msg(&self.access.problem.clone().unwrap_or_default()).into_owned();
                // On Windows they hold the password and open to administrators only: a window
                // started without the rights asks for them here, when they are wanted.
                let ask = cfg!(windows) && !crate::admin::elevated();
                if ask {
                    let mut why = why.clone();
                    if let Some(first) = why.get_mut(..1) {
                        first.make_ascii_uppercase();
                    }
                    kit::notice(ui, kit::Tone::Info, t!("Its settings open to administrators"), &t!("{}. The window opens again as administrator, with its tabs.", why));
                } else {
                    kit::notice(ui, kit::Tone::Bad, t!("Its settings cannot be read or kept"), &why);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ask && ui.add(theme::primary(format!("{}  {}", ic::SHIELD_CHECK, t!("Open as administrator")))).clicked() {
                        restart = true;
                    }
                    if ui.add(kit::secondary(t!("Try again"))).clicked() {
                        self.access.problem = None;
                    }
                });
                return None;
            };
            ui.horizontal(|ui| {
                // The switch first, at the right; the heading takes what is left, cut short in a
                // language that says it at length.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut on = cfg.enabled;
                    if super::super::access_ui::toggle(ui, &mut on).on_hover_text(if on { t!("Turn remote access off") } else { t!("Turn remote access on") }).clicked() {
                        cfg.enabled = on;
                        changed = true;
                    }
                    ui.add_space(6.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(t!("Remote access by ID")).strong()).truncate());
                    });
                });
            });
            if !cfg.enabled {
                ui.add(
                    egui::Label::new(RichText::new(t!("Off. On, anyone with this computer's ID and password can see and use it, through the relay.")).weak().size(12.5))
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
                    ui.add(egui::Label::new(RichText::new(crate::lang::tr_msg(&text)).color(color).size(12.5)).wrap());
                });
                ui.add_space(6.0);
                // The passwords viewers may give: the lasting one, the one-time one, or both, each
                // shown on asking, copied, or made anew.
                // A team that lets in its own people alone: no password opens this computer.
                let teams_only = crate::team::client::policy().is_some_and(|p| !p.password);
                if teams_only {
                    ui.add(egui::Label::new(RichText::new(t!("Your team's people come in with their own sign-in to the team: no password is needed or taken here.")).size(12.5)).wrap());
                }
                let rows: Vec<(bool, &str)> =
                    [(cfg.passwords.lasting(), n!("Password")), (cfg.passwords.one_time(), n!("One-time"))].into_iter().filter(|r| r.0 && !teams_only).collect();
                for (_, name) in rows {
                    let once = name == "One-time";
                    let value = if once { cfg.one_time.clone() } else { cfg.password.clone() };
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(Vec2::new(64.0, 26.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(64.0);
                            let l = ui.label(RichText::new(crate::lang::tr(name)).weak().size(12.5));
                            if once {
                                l.on_hover_text(t!("Works for one connection: a minute after it ends, a new one takes its place"));
                            }
                        });
                        let shown = if self.access.show_password { value.clone() } else { "\u{2022}".repeat(value.chars().count()) };
                        ui.label(RichText::new(shown).monospace().size(14.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let fresh = if once { t!("A new one-time password now") } else { t!("A new random password (viewers need the new one)") };
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
                            let (g, t) = if kit::copied(ui, at) { (ic::CHECK, t!("Copied")) } else { (ic::COPY, t!("Copy the password")) };
                            if small_icon(ui, g, t).clicked() {
                                kit::copy(ui, &value, at);
                            }
                            let (g, t) = if self.access.show_password { (ic::EYE_SLASH, t!("Hide the passwords")) } else { (ic::EYE, t!("Show the passwords")) };
                            if small_icon(ui, g, t).clicked() {
                                self.access.show_password = !self.access.show_password;
                            }
                        });
                    });
                }
                ui.add_space(6.0);
                kit::heading(ui, t!("Connected now"));
                // Served by this window or by the service: each with how it came and a way to end it.
                let viewers = if svc.runs { access::service_viewers() } else { access::viewers() };
                if viewers.is_empty() {
                    ui.label(RichText::new(t!("Nobody.")).weak().size(12.5));
                }
                for v in viewers {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(ic::DESKTOP).size(14.0).color(good()));
                        ui.label(RichText::new(t!("A viewer, {}", crate::lang::tr_msg(&v.describe()))).size(13.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(kit::danger(t!("End"), false).min_size(Vec2::new(0.0, 26.0))).on_hover_text(t!("End this connection now")).clicked() {
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
                kit::heading(ui, t!("Lately"));
                let lately = access::visits(6);
                if lately.is_empty() {
                    ui.label(RichText::new(t!("Nobody has come by the ID yet.")).weak().size(12.5));
                }
                for v in lately {
                    let (mark, colour) = if v.let_in { (ic::DESKTOP, ui.visuals().weak_text_color()) } else { (ic::WARNING, warn()) };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(mark).size(13.0).color(colour));
                        ui.add(egui::Label::new(RichText::new(crate::lang::tr_msg(&v.describe())).size(12.0)).wrap());
                    });
                }
            }
            ui.add_space(4.0);
            ui.separator();
            open_settings = ui.add(kit::ghost(format!("{}  {}", ic::SLIDERS_HORIZONTAL, t!("Settings\u{2026}")))).on_hover_text(t!("Start with the computer, your own password, the relay, a new ID")).clicked();
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
            self.access.ids = cfg.ids.clone();
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
            // What the computer's team fixes: shown as the team set it, not to be changed here.
            let policy = crate::team::client::policy();
            let fixed_by = |v: Option<bool>| policy.as_ref().and(v);
            let teams_only = policy.as_ref().is_some_and(|p| !p.password);
            let mut changed = false;
            let mut service: Option<bool> = None;
            let mut restart = false;
            let (modal, fit) = super::super::modal(ctx, "access-settings", Vec2::new(560.0, 0.0));
            let r = modal.show(ctx, |ui| {
                ui.set_width(fit.x);
                kit::title(ui, t!("Remote access settings"), t!("How this computer is reached by its ID."));
                // What does not fit the window scrolls, between the title and Done.
                let room = (ctx.content_rect().height() - 220.0).max(160.0);
                egui::ScrollArea::vertical().id_salt("access-settings-body").max_height(room).auto_shrink([false, true]).show(ui, |ui| {
                    kit::heading(ui, t!("Start with the computer"));
                    kit::card(ui, |ui| {
                        let what = if cfg!(windows) {
                            t!("The SSH Remote service: reachable after a restart, at the sign-in and lock screens, with no window open.")
                        } else if cfg!(target_os = "macos") {
                            t!("A launch agent: reachable whenever you are signed in, with no window open; also at the login window once an administrator allows it.")
                        } else if login_screen_on() {
                            t!("A systemd user service: reachable whenever you are signed in, with no window open; also at the login screen and on a locked screen.")
                        } else {
                            t!("A systemd user service: reachable whenever you are signed in, with no window open. Installing it also asks an administrator to let it serve the login screen and a locked screen.")
                        };
                        ui.add(egui::Label::new(RichText::new(what).size(12.5)).wrap());
                        ui.add_space(6.0);
                        let admin = !cfg!(windows) || crate::admin::elevated();
                        ui.horizontal(|ui| {
                            if !svc.known {
                                kit::loading(ui, t!("Looking for the service\u{2026}"));
                            } else if svc.installed && !svc.this {
                                ui.label(RichText::new(format!("{}  {}", ic::WARNING, t!("An older copy is installed"))).color(warn()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled(admin, theme::primary(format!("{}  {}", ic::DOWNLOAD, t!("Update it")))).on_disabled_hover_text(t!("Needs administrator rights")).clicked() {
                                        service = Some(true);
                                    }
                                });
                            } else if svc.installed {
                                ui.label(RichText::new(format!("{}  {}", ic::CHECK_CIRCLE, if svc.runs { t!("Installed and running") } else { t!("Installed") })).color(good()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled(admin, kit::danger(t!("Remove"), false)).on_disabled_hover_text(t!("Needs administrator rights")).clicked() {
                                        service = Some(false);
                                    }
                                });
                            } else {
                                ui.label(RichText::new(t!("Not installed: remote access works while this window is open.")).weak().size(12.5));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if admin {
                                        if ui.add(theme::primary(format!("{}  {}", ic::DOWNLOAD, t!("Install")))).clicked() {
                                            cfg.enabled = true;
                                            changed = true;
                                            service = Some(true);
                                        }
                                    } else if ui.add(kit::secondary(format!("{}  {}", ic::SHIELD_CHECK, t!("As administrator")))).on_hover_text(t!("Installing needs administrator rights")).clicked() {
                                        restart = true;
                                    }
                                });
                            }
                        });
                        // Nobody sits at a computer the service keeps reachable to click "Update".
                        ui.add_space(4.0);
                        if ui
                            .checkbox(&mut cfg.auto_update, t!("Update by itself"))
                            .on_hover_text(t!("The service puts new versions in place by itself, checked against sshremote.dev's signed list, when nobody is connected, and starts again on them"))
                            .changed()
                        {
                            changed = true;
                        }
                    });
                    ui.add_space(kit::GAP);
                    kit::heading(ui, t!("Access"));
                    kit::card(ui, |ui| {
                        // Which passwords viewers may give.
                        kit::row(ui, t!("Passwords"), |ui| {
                            let mut i = access::Passwords::ALL.iter().position(|p| *p == cfg.passwords).unwrap_or(0);
                            let names: Vec<&str> = access::Passwords::ALL.iter().map(|p| crate::lang::tr(p.label())).collect();
                            if kit::segmented(ui, &mut i, &names) {
                                cfg.passwords = access::Passwords::ALL[i];
                                changed = true;
                            }
                        });
                        ui.add(
                            egui::Label::new(
                                RichText::new(match cfg.passwords {
                                    access::Passwords::Lasting => t!("Viewers give the lasting password, until you change it."),
                                    access::Passwords::OneTime => t!("Viewers give the one-time password shown here: it works for one connection, and a minute after that ends a new one takes its place, so nobody comes back later with it."),
                                    access::Passwords::Both => t!("Viewers give either: the one-time password to someone helping once, the lasting one for yourself."),
                                })
                                .weak()
                                .size(12.0),
                            )
                            .wrap(),
                        );
                        kit::row(ui, t!("Your own password"), |ui| {
                            let ok = self.access.new_password.trim().chars().count() >= 8 && !crate::net::looks_one_time(&self.access.new_password);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(ok, kit::secondary(t!("Set"))).clicked() {
                                    cfg.password = self.access.new_password.trim().to_string();
                                    self.access.new_password.clear();
                                    changed = true;
                                }
                                ui.add(kit::field(&mut self.access.new_password, t!("8 characters or more")).password(true));
                            });
                        });
                        kit::row(ui, t!("ID"), |ui| {
                            ui.label(RichText::new(crate::computers::spaced(&cfg.id)).monospace().size(14.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let in_team = cfg.team.is_object();
                                if ui
                                    .add_enabled(!in_team, kit::secondary(t!("Take a new ID")))
                                    .on_hover_text(t!("Those who know the old ID can no longer use it"))
                                    .on_disabled_hover_text(t!("This computer's team knows it by its ID: leave the team first"))
                                    .clicked()
                                {
                                    (cfg.id, cfg.own_id) = (access::new_id(), false);
                                    changed = true;
                                }
                            });
                        });
                        // One of one's own choosing, while no other computer has it.
                        kit::row(ui, t!("Your own ID"), |ui| {
                            let typed: String = self.access.own_id.chars().filter(|c| !c.is_whitespace()).collect();
                            let ok = access::own_id_ok(&typed) && typed != cfg.id && !cfg.team.is_object();
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(ok, kit::secondary(t!("Use"))).on_hover_text(t!("If another computer has it, the relay says so and this one keeps trying until you choose another")).clicked() {
                                    (cfg.id, cfg.own_id) = (typed.clone(), true);
                                    self.access.own_id.clear();
                                    changed = true;
                                }
                                ui.add(kit::field(&mut self.access.own_id, t!("6 to 12 digits, not starting with 0")));
                            });
                        });
                        kit::row(ui, t!("Relay"), |ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(self.access.relay.trim() != cfg.relay, kit::secondary(t!("Use"))).clicked() {
                                    let r = self.access.relay.trim();
                                    cfg.relay = if r.is_empty() { access::DEFAULT_RELAY.into() } else { crate::computers::relay_name(r) };
                                    self.access.relay = cfg.relay.clone();
                                    changed = true;
                                }
                                ui.add(kit::field(&mut self.access.relay, access::DEFAULT_RELAY));
                            });
                        });
                        ui.add(
                            egui::Label::new(RichText::new(t!("The relay is where both ends meet; it passes on what it cannot read. relay.sshremote.dev, or sshremote-relay on a server of your own. Computers added by ID go through it too.")).weak().size(12.0))
                                .wrap(),
                        );
                    });
                    ui.add_space(kit::GAP);
                    kit::heading(ui, t!("Who comes in, and what they may do"));
                    kit::card(ui, |ui| {
                        if policy.is_some() {
                            let words = if teams_only {
                                t!("This computer's team sets some of these. It lets in the team's people alone: passwords, one-time passwords and asking to come in do not work here.")
                            } else {
                                t!("This computer's team sets some of these: they show as it set them.")
                            };
                            kit::notice(ui, kit::Tone::Info, t!("Set by your team"), words);
                            ui.add_space(6.0);
                        }
                        if locked_checkbox(ui, &mut cfg.ask, fixed_by(policy.as_ref().and_then(|p| p.ask)), t!("Ask the person at this computer before letting anyone in")) {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(RichText::new(t!("With the right password a viewer waits for a click here; nobody answering within a minute turns them away. Off: the password is enough, for a computer nobody sits at.")).weak().size(12.0))
                                .wrap(),
                        );
                        ui.add_space(8.0);
                        if locked_checkbox(ui, &mut cfg.requests, teams_only.then_some(false), t!("Let people without the password ask to come in")) {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(t!("Someone helping you needs only this computer's ID: you are asked each time, and both screens show a code to compare first. One at a time.")).weak().size(12.0),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new(t!("Viewers may")).strong());
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for (bit, label, _) in crate::grant::RIGHTS {
                                let mut on = cfg.allow & bit != 0;
                                let capped = policy.as_ref().is_some_and(|p| p.rights & bit == 0).then_some(false);
                                if locked_checkbox(ui, &mut on, capped, crate::lang::tr(label)) {
                                    cfg.allow = if on { cfg.allow | bit } else { cfg.allow & !bit };
                                    changed = true;
                                }
                            }
                        });
                        ui.add(
                            egui::Label::new(RichText::new(t!("They always see the screen. The person at the computer can change these for a session in its window.")).weak().size(12.0)).wrap(),
                        );
                        // Which way copied things and files go, for whatever the rights above let in.
                        let way = |w: u8| match w {
                            1 => t!("From this computer only"),
                            2 => t!("To this computer only"),
                            _ => t!("Both ways"),
                        };
                        for (label, id, value) in [(t!("Clipboard"), "clip-way", &mut cfg.clip_way), (t!("Files"), "files-way", &mut cfg.files_way)] {
                            kit::row(ui, label, |ui| {
                                egui::ComboBox::from_id_salt(id).selected_text(way(*value)).show_ui(ui, |ui| {
                                    for w in 0..=2u8 {
                                        changed |= ui.selectable_value(value, w, way(w)).changed();
                                    }
                                });
                            });
                        }
                        ui.add_space(4.0);
                        if ui
                            .checkbox(&mut cfg.plain_wallpaper, t!("Take the desktop picture away during sessions"))
                            .on_hover_text(t!("A plain color is quicker to send; the picture comes back when the session ends (Windows, GNOME, Cinnamon, MATE)"))
                            .changed()
                        {
                            changed = true;
                        }
                        ui.add_space(4.0);
                        if ui.checkbox(&mut cfg.cameras, t!("Let viewers look at this computer's cameras")).changed() {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(RichText::new(t!("They are listed after the monitors; the person at the computer is told each time one is looked at.")).weak().size(12.0)).wrap(),
                        );
                        ui.add_space(4.0);
                        let folder = crate::access::recordings_dir().map(|d| d.display().to_string()).unwrap_or_default();
                        if locked_checkbox(ui, &mut cfg.record, fixed_by(policy.as_ref().and_then(|p| p.record)), t!("Record each session by ID")) {
                            changed = true;
                        }
                        ui.add(egui::Label::new(RichText::new(t!("As its viewer sees the screen, into {}; the viewer is told", folder)).weak().size(12.0)).wrap());
                        ui.add_space(4.0);
                        let idle_fixed = policy.as_ref().and_then(|p| p.idle);
                        kit::row(ui, t!("End a session left idle"), |ui| {
                            let name = |m: u32| if m == 0 { t!("Never").to_string() } else { t!("After {} min without input", m) };
                            match idle_fixed {
                                Some(m) => {
                                    ui.add_enabled(false, egui::Button::new(name(m))).on_disabled_hover_text(t!("Set by your team"));
                                }
                                None => {
                                    egui::ComboBox::from_id_salt("idle-end").selected_text(name(cfg.idle)).show_ui(ui, |ui| {
                                        for m in [0, 5, 15, 30, 60, 120] {
                                            changed |= ui.selectable_value(&mut cfg.idle, m, name(m)).changed();
                                        }
                                    });
                                }
                            }
                        });
                        ui.add_space(8.0);
                        let lan_fixed = fixed_by(policy.as_ref().and_then(|p| p.lan));
                        if locked_checkbox(ui, &mut cfg.lan, lan_fixed, t!("Let computers on this network find this one")) {
                            changed = true;
                        }
                        ui.add(egui::Label::new(RichText::new(t!("They are told its ID, name and network card, never its password, and can add it without typing the ID.")).weak().size(12.0)).wrap());
                        ui.add_space(8.0);
                        ui.label(RichText::new(t!("Two-step sign-in")).strong());
                        if !cfg.totp.is_empty() {
                            ui.horizontal(|ui| {
                                kit::dot(ui, good());
                                ui.label(t!("On: viewers give the code of your authenticator app with the password."));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add(kit::danger(t!("Turn off"), false)).clicked() {
                                        cfg.totp.clear();
                                        cfg.trusted.clear();
                                        changed = true;
                                    }
                                });
                            });
                            // Devices let in without the code, which can be forgotten here.
                            let trusted: Vec<&crate::access::Trusted> = cfg.trusted_now().collect();
                            if !trusted.is_empty() {
                                let mut forget = false;
                                ui.horizontal(|ui| {
                                    let n = trusted.len();
                                    let latest = trusted.iter().map(|t| t.used).max().unwrap_or(0);
                                    let days = crate::access::TRUST_DAYS;
                                    let text = match (n == 1, latest == 0) {
                                        (true, true) => t!("{} trusted device may come in without the code (each for {} days)", n, days),
                                        (false, true) => t!("{} trusted devices may come in without the code (each for {} days)", n, days),
                                        (true, false) => t!("{} trusted device may come in without the code (each for {} days); the last came {}", n, days, crate::sys::local_time(latest)),
                                        (false, false) => t!("{} trusted devices may come in without the code (each for {} days); the last came {}", n, days, crate::sys::local_time(latest)),
                                    };
                                    ui.label(RichText::new(text).size(12.5))
                                        .on_hover_text(trusted.iter().map(|t| {
                                            let from = if t.from.is_empty() { "?" } else { &t.from };
                                            if t.used == 0 { t!("from {}, since {}, not back yet", from, crate::sys::local_time(t.at)) } else { t!("from {}, since {}", from, crate::sys::local_time(t.at)) }
                                        }).collect::<Vec<_>>().join("\n"));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        forget = ui.add(kit::ghost(t!("Forget them"))).on_hover_text(t!("They give the code again next time")).clicked();
                                    });
                                });
                                if forget {
                                    cfg.trusted.clear();
                                    changed = true;
                                }
                            }
                        } else if let Some(secret) = self.access.totp_new.clone() {
                            ui.add(egui::Label::new(RichText::new(t!("In your authenticator app (Google Authenticator, Microsoft Authenticator, 1Password, Aegis…), add an account by its setup key:")).size(12.5)).wrap());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(crate::totp::grouped(&secret)).monospace().size(14.0).strong());
                                let at = egui::Id::unique("totp-copy");
                                let copied = kit::copied(ui, at);
                                if ui.add(kit::ghost(if copied { t!("Copied") } else { t!("Copy") })).clicked() {
                                    kit::copy(ui, &secret, at);
                                }
                                if ui.add(kit::ghost(t!("Copy as a link"))).on_hover_text(t!("otpauth://, for apps that take a link")).clicked() {
                                    kit::copy(ui, &crate::totp::uri(&secret, &cfg.id), at);
                                }
                            });
                            ui.add(egui::Label::new(RichText::new(t!("Or scan it with the app's camera:")).size(12.5)).wrap());
                            qr(ui, &crate::totp::uri(&secret, &cfg.id), 200.0);
                            kit::row(ui, t!("Its code now"), |ui| {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let ok = crate::totp::check(&secret, &self.access.totp_code);
                                    if ui.add_enabled(ok, kit::secondary(t!("Turn on"))).on_disabled_hover_text(t!("Type the six digits the app shows")).clicked() {
                                        cfg.totp = secret.clone();
                                        cfg.trusted.clear();
                                        (self.access.totp_new, self.access.totp_code) = (None, String::new());
                                        changed = true;
                                    }
                                    if ui.add(kit::ghost(t!("Cancel"))).clicked() {
                                        (self.access.totp_new, self.access.totp_code) = (None, String::new());
                                    }
                                    ui.add(kit::field(&mut self.access.totp_code, t!("six digits")));
                                });
                            });
                        } else {
                            ui.add(egui::Label::new(RichText::new(t!("Off: the password is enough. On, a viewer also gives the six-digit code of your authenticator app.")).weak().size(12.0)).wrap());
                            if ui.add(kit::secondary(t!("Set up\u{2026}"))).clicked() {
                                self.access.totp_new = Some(crate::totp::new_secret());
                            }
                        }
                        ui.add_space(8.0);
                        if locked_checkbox(ui, &mut cfg.direct, lan_fixed.filter(|l| !l), t!("Let viewers on this network come straight in")) {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(RichText::new(t!("On port {}, with the same password, without the relay: quicker, and nothing leaves the network.", crate::net::DIRECT_PORT)).weak().size(12.0))
                                .wrap(),
                        );
                        ui.add_space(8.0);
                        if locked_checkbox(ui, &mut cfg.punch, fixed_by(policy.as_ref().and_then(|p| p.direct)), t!("Let viewers by ID go straight through the routers")) {
                            changed = true;
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(t!("Once the password is right, the two computers look for a path between them that skips the relay, and move to it if there is one: quicker, and the relay no longer carries the session. Each then knows the other's address."))
                                    .weak()
                                    .size(12.0),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new(t!("Only from these addresses")).strong());
                        let bad = access::bad_ranges(&self.access.from);
                        let typed = access::tidy_ranges(&self.access.from);
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 70.0).max(120.0);
                            ui.add(egui::TextEdit::singleline(&mut self.access.from).hint_text(t!("Anywhere")).desired_width(w).margin(egui::Margin::symmetric(8, 6)));
                            if ui.add_enabled(bad.is_empty() && typed != cfg.from, kit::secondary(t!("Use"))).clicked() {
                                cfg.from = typed.clone();
                                self.access.from = typed;
                                changed = true;
                            }
                        });
                        if !bad.is_empty() {
                            ui.label(RichText::new(t!("Not an address or a range: {}", bad.join(", "))).color(super::super::bad()).size(12.0));
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(t!("Addresses or ranges, like 203.0.113.7 or 192.168.1.0/24: nobody comes in from anywhere else, even with the password. Through the relay, it is where the relay sees the viewer from; through a web page, the address of its server. Empty: from anywhere."))
                                    .weak()
                                    .size(12.0),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new(t!("Only these IDs")).strong());
                        let bad = access::bad_ids(&self.access.ids);
                        let typed = access::tidy_ids(&self.access.ids);
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 70.0).max(120.0);
                            ui.add(egui::TextEdit::singleline(&mut self.access.ids).hint_text(t!("Any")).desired_width(w).margin(egui::Margin::symmetric(8, 6)));
                            if ui.add_enabled(bad.is_empty() && typed != cfg.ids, kit::secondary(t!("Use"))).clicked() {
                                cfg.ids = typed.clone();
                                access::keep_listed_devices(&mut cfg);
                                self.access.ids = typed;
                                changed = true;
                            }
                        });
                        if !bad.is_empty() {
                            ui.label(RichText::new(t!("Not an ID: {}", bad.join(", "))).color(super::super::bad()).size(12.0));
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(t!("The IDs of the computers viewers come from, like 123 456 789, or 98765* for all that start so: no other comes in, even with the password. Each ID comes in only from the device it first came from (its key shows it). A viewer's ID is the one its remote access shows, on or off. Empty: any."))
                                    .weak()
                                    .size(12.0),
                            )
                            .wrap(),
                        );
                        let mut forget = None;
                        for (i, (id, _, at)) in cfg.id_keys.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{}  {}", ic::DESKTOP, crate::computers::spaced(id))).monospace());
                                ui.label(RichText::new(t!("its device since {}", crate::lang::tr_msg(&crate::hosts::ago(*at)))).weak().size(12.0));
                                if ui.add(egui::Button::new(RichText::new(t!("Forget its device")).small())).on_hover_text(t!("Its next device that comes in with this ID is kept in its place")).clicked() {
                                    forget = Some(i);
                                }
                            });
                        }
                        if let Some(i) = forget {
                            cfg.id_keys.remove(i);
                            changed = true;
                        }
                    });
                });
                ui.add_space(kit::GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.add(theme::primary(t!("Done"))).clicked()).inner
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
fn address_row(ui: &mut egui::Ui, label: &str, shown: &str, value: &str, tip: &str) {
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
    let tip = if done { t!("Copied") } else { tip };
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

/// Linux: whether root's remote access at the login screen is set up.
fn login_screen_on() -> bool {
    #[cfg(all(target_os = "linux", feature = "remote"))]
    return crate::loginscreen::installed();
    #[allow(unreachable_code)]
    false
}

/// A setting's checkbox; one the computer's team fixes (`fixed`) shows its value there and cannot
/// be changed. True when it was changed.
#[cfg(feature = "remote")]
fn locked_checkbox(ui: &mut egui::Ui, value: &mut bool, fixed: Option<bool>, label: impl Into<egui::WidgetText>) -> bool {
    match fixed {
        Some(v) => {
            let mut shown = v;
            ui.add_enabled(false, egui::Checkbox::new(&mut shown, label)).on_disabled_hover_text(t!("Set by your team"));
            false
        }
        None => ui.checkbox(value, label).changed(),
    }
}
