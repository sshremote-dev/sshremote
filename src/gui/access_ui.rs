//! The Remote access card under This computer: this computer's ID and password for viewers that
//! come through the relay, turning it on and off, and starting it with the computer.

use eframe::egui::{self, CornerRadius, RichText, Stroke};

use super::{App, bad, good, ic, icon, theme};

/// What the card keeps between frames.
#[derive(Default)]
pub(super) struct AccessUi {
    #[cfg(feature = "remote")]
    cfg: Option<crate::access::Config>,
    show_password: bool,
    new_password: String,
    relay: String,
    /// Whether the relay sees this ID now, looked up in the background every few seconds.
    online: std::sync::Arc<std::sync::Mutex<Option<Result<bool, String>>>>,
    looked: Option<std::time::Instant>,
    service: Option<bool>,
}

impl App {
    /// The Remote access card.
    pub(super) fn access_card(&mut self, ui: &mut egui::Ui) {
        let v = ui.visuals().clone();
        egui::Frame::new()
            .fill(v.faint_bg_color)
            .stroke(Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color))
            .corner_radius(CornerRadius::same(theme::R_LG))
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                #[cfg(feature = "remote")]
                self.access_body(ui);
                #[cfg(not(feature = "remote"))]
                {
                    ui.label(RichText::new(format!("{}  Remote access by ID", ic::GLOBE)).size(15.0).strong());
                    ui.label(RichText::new("Lets others reach this computer's screen with an ID and a password, through a relay: for computers on Wi-Fi or without a fixed address. It comes with the Remote desktop extension.").weak().size(12.0));
                    if ui.add(theme::primary(format!("{}  Get the Remote desktop extension", ic::PUZZLE_PIECE))).clicked() {
                        self.section = super::home::Section::Extensions;
                        let t = self.exts.get_or_insert_with(crate::ext::ExtTool::new);
                        t.tab = crate::ext::ExtTab::Browse;
                    }
                }
            });
    }

    #[cfg(feature = "remote")]
    fn access_body(&mut self, ui: &mut egui::Ui) {
        use crate::access;
        let a = &mut self.access;
        let mut cfg = a
            .cfg
            .get_or_insert_with(|| {
                // Kept at once, so the ID shown is the one others will use.
                let c = access::load();
                if access::file().is_some_and(|f| !f.exists()) {
                    let _ = access::save(&c);
                }
                c
            })
            .clone();
        if a.relay.is_empty() {
            a.relay = cfg.relay.clone();
        }
        let service = *a.service.get_or_insert_with(access::service_installed);
        // The relay's view of this ID, asked again every 5 s while the card shows.
        if cfg.enabled && a.looked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5)) {
            a.looked = Some(std::time::Instant::now());
            let (id, relay, slot, ctx) = (cfg.id.clone(), cfg.relay.clone(), a.online.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let r = access::online(&id, &relay);
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                ctx.request_repaint();
            });
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  Remote access by ID", ic::GLOBE)).size(15.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut on = cfg.enabled;
                if toggle(ui, &mut on).on_hover_text(if on { "Turn remote access off" } else { "Turn remote access on" }).clicked() {
                    cfg.enabled = on;
                    changed = true;
                }
            });
        });
        ui.label(RichText::new("Anyone with this ID and password can see and control this computer, from anywhere, through the relay. For computers on Wi-Fi or without a fixed address; SSH stays the way for one host you reach directly.").weak().size(12.0));
        ui.add_space(8.0);
        // The ID and the password, large, to read out or copy.
        egui::Grid::new("access-grid").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new("ID").weak());
            ui.horizontal(|ui| {
                ui.label(RichText::new(access::spaced(&cfg.id)).size(22.0).strong().monospace());
                if icon(ui, ic::COPY, "Copy the ID", false).clicked() {
                    ui.ctx().copy_text(cfg.id.clone());
                }
            });
            ui.end_row();
            ui.label(RichText::new("Password").weak());
            ui.horizontal(|ui| {
                let shown = if self.access.show_password { cfg.password.clone() } else { "\u{2022}".repeat(cfg.password.chars().count()) };
                ui.label(RichText::new(shown).size(18.0).monospace());
                let (g, t) = if self.access.show_password { (ic::EYE_SLASH, "Hide") } else { (ic::EYE, "Show") };
                if icon(ui, g, t, false).clicked() {
                    self.access.show_password = !self.access.show_password;
                }
                if icon(ui, ic::COPY, "Copy the password", false).clicked() {
                    ui.ctx().copy_text(cfg.password.clone());
                }
                if icon(ui, ic::ARROWS_CLOCKWISE, "A new random password (viewers need the new one)", false).clicked() {
                    cfg.password = access::new_password();
                    self.access.show_password = true;
                    changed = true;
                }
            });
            ui.end_row();
            ui.label(RichText::new("State").weak());
            let online = self.access.online.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let local = access::state();
            let (color, text) = if !cfg.enabled {
                (ui.visuals().weak_text_color(), "Off: nobody can connect by ID".to_string())
            } else {
                match (&local, online) {
                    (access::State::InUse(n), _) => (good(), format!("A viewer is connected ({n})")),
                    (access::State::Failed(e), _) => (bad(), e.clone()),
                    (_, Some(Ok(true))) => (good(), format!("Ready: waiting at {}", cfg.relay)),
                    (_, Some(Ok(false))) => (bad(), if service || access::service_runs() { "Not at the relay yet: the service connects in a moment".into() } else { "Connecting to the relay\u{2026}".into() }),
                    (_, Some(Err(e))) => (bad(), format!("The relay could not be reached: {e}")),
                    (_, None) => (ui.visuals().weak_text_color(), "Connecting\u{2026}".into()),
                }
            };
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 4.5, color);
                ui.label(RichText::new(text).color(color));
            });
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.separator();
        // Starting with the computer: the service, which also works at the sign-in screen.
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  Start with the computer", ic::SHIELD_CHECK)).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let admin = !cfg!(windows) || crate::admin::elevated();
                if service && !access::service_is_this() {
                    let b = ui.add_enabled(admin, theme::primary(format!("{}  Update the service", ic::DOWNLOAD)));
                    if b.on_hover_text("The installed service runs another copy of sshremote; this one has what it lacks").on_disabled_hover_text("Needs administrator rights").clicked() {
                        self.error = access::install_service().err().map(|e| format!("Service: {e}"));
                        self.access.service = None;
                        self.service = None;
                    }
                } else if service {
                    ui.label(RichText::new(format!("{}  Installed", ic::CHECK)).color(good()));
                    if ui.add_enabled(admin, egui::Button::new("Remove")).on_disabled_hover_text("Needs administrator rights").clicked() {
                        self.error = access::uninstall_service().err().map(|e| format!("Service: {e}"));
                        self.access.service = None;
                        self.service = None;
                    }
                } else if admin {
                    if ui.add(theme::primary(format!("{}  Install", ic::DOWNLOAD))).clicked() {
                        cfg.enabled = true;
                        let _ = access::save(&cfg);
                        self.error = access::install_service().err().map(|e| format!("Service: {e}"));
                        self.access.service = None;
                        self.service = None;
                    }
                } else if ui.add(theme::primary(format!("{}  Restart as administrator to install", ic::SHIELD_CHECK))).clicked() {
                    self.restart_elevated(ui.ctx());
                }
            });
        });
        let what = if cfg!(windows) {
            "Installs the SSH Remote service: reachable after a restart, at the sign-in and lock screens, with no window open."
        } else {
            "Adds a systemd user service: reachable whenever you are signed in, with no window open."
        };
        ui.label(RichText::new(what).weak().size(12.0));
        if !service {
            ui.label(RichText::new("Without it, remote access works while this window is open.").weak().size(12.0));
        }
        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new("More").size(12.0)).id_salt("access-more").show(ui, |ui| {
            egui::Grid::new("access-more-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Your own password").weak());
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.access.new_password).password(true).hint_text("6 characters or more").desired_width(180.0));
                    if ui.add_enabled(self.access.new_password.trim().len() >= 6, egui::Button::new("Set")).clicked() {
                        cfg.password = self.access.new_password.trim().to_string();
                        self.access.new_password.clear();
                        changed = true;
                    }
                });
                ui.end_row();
                ui.label(RichText::new("Relay").weak()).on_hover_text("sshremote.dev, or your own: sshremote --relay on a server with port 7071 open");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.access.relay).desired_width(180.0));
                    if ui.add_enabled(self.access.relay.trim() != cfg.relay, egui::Button::new("Use")).clicked() {
                        let r = self.access.relay.trim();
                        cfg.relay = if r.is_empty() { access::DEFAULT_RELAY.into() } else { r.to_string() };
                        self.access.relay = cfg.relay.clone();
                        changed = true;
                    }
                });
                ui.end_row();
                ui.label(RichText::new("New ID").weak());
                if ui.button("Take a new ID").on_hover_text("Those who know the old ID can no longer use it").clicked() {
                    cfg.id = access::new_id();
                    changed = true;
                }
                ui.end_row();
            });
        });
        if changed {
            match access::save(&cfg) {
                Ok(()) => {
                    self.access.cfg = Some(cfg);
                    self.access.looked = None;
                    *self.access.online.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                Err(e) => {
                    self.error = Some(e);
                    self.access.cfg = None;
                }
            }
        }
    }
}

/// An on/off switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(38.0, 20.0);
    let (rect, mut r) = ui.allocate_exact_size(size, egui::Sense::click());
    if r.clicked() {
        *on = !*on;
        r.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(r.id, *on);
    let fill = if *on { super::brand() } else { ui.visuals().widgets.inactive.bg_fill };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 7.0, egui::Color32::WHITE);
    r
}
