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
    /// Whether the relay sees this ID now (for which ID and relay), looked up in the background
    /// every few seconds, one lookup at a time.
    online: Shared<Option<(String, String, Result<bool, String>)>>,
    looked: Option<std::time::Instant>,
    /// The service: installed, running, and running this program file; looked up in the background.
    service: Shared<Option<Service>>,
    service_looked: Option<std::time::Instant>,
    /// Why the stored settings could not be read or kept, shown instead of an ID.
    #[cfg(feature = "remote")]
    problem: Option<String>,
    /// What the Remote desktop extension's program says of remote access (the Terminal edition).
    #[cfg(not(feature = "remote"))]
    by_extension: Shared<Option<Option<serde_json::Value>>>,
}

type Shared<T> = std::sync::Arc<std::sync::Mutex<T>>;

#[derive(Clone, Copy, Default)]
struct Service {
    installed: bool,
    runs: bool,
    this: bool,
}

/// Runs `look` on a thread of its own unless one still runs for `slot`, then repaints.
fn in_background<T: Send + 'static>(slot: &Shared<Option<T>>, busy: &'static std::sync::atomic::AtomicBool, ctx: &egui::Context, look: impl FnOnce() -> T + Send + 'static) {
    use std::sync::atomic::Ordering;
    if busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let (slot, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        // Free again even if the lookup panics.
        struct Free(&'static std::sync::atomic::AtomicBool);
        impl Drop for Free {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _free = Free(busy);
        let v = look();
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
        ctx.request_repaint();
    });
}

#[cfg(feature = "remote")]
static ONLINE_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "remote")]
static SERVICE_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
                if crate::ext::provider("remote").is_some() {
                    self.access_by_extension(ui);
                } else {
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
        if a.cfg.is_none() && a.problem.is_none() {
            // Kept at once, so the ID shown is the one others will use; settings that cannot be
            // read or kept are said so, never shown as made-up ones.
            match access::stored() {
                Ok(Some(c)) => a.cfg = Some(c),
                Ok(None) => {
                    let c = access::fresh();
                    match access::save(&c) {
                        Ok(()) => a.cfg = Some(c),
                        Err(e) => a.problem = Some(e),
                    }
                }
                Err(e) => a.problem = Some(e),
            }
        }
        let Some(mut cfg) = a.cfg.clone() else {
            ui.label(RichText::new(format!("{}  Remote access by ID", ic::GLOBE)).size(15.0).strong());
            ui.label(RichText::new("Lets others reach this computer with an ID and a password through the relay, for computers on Wi-Fi or without a fixed address.").weak().size(12.0));
            ui.add_space(6.0);
            let why = a.problem.clone().unwrap_or_default();
            ui.label(RichText::new(format!("Not set up yet: {why}")).color(bad()));
            ui.horizontal(|ui| {
                if cfg!(windows) && !crate::admin::elevated() && ui.add(theme::primary(format!("{}  Restart as administrator to set it up", ic::SHIELD_CHECK))).clicked() {
                    self.restart_elevated(ui.ctx());
                }
                if ui.button("Try again").clicked() {
                    self.access.problem = None;
                }
            });
            return;
        };
        if a.relay.is_empty() {
            a.relay = cfg.relay.clone();
        }
        let ctx = ui.ctx().clone();
        if a.service_looked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(10)) {
            a.service_looked = Some(std::time::Instant::now());
            in_background(&a.service, &SERVICE_BUSY, &ctx, || Service { installed: access::service_installed(), runs: access::service_runs(), this: access::service_is_this() });
        }
        let svc = a.service.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or_default();
        let service = svc.installed;
        // The relay's view of this ID, asked again every 5 s while the card shows.
        if cfg.enabled && a.looked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5)) {
            a.looked = Some(std::time::Instant::now());
            let (id, relay) = (cfg.id.clone(), cfg.relay.clone());
            in_background(&a.online, &ONLINE_BUSY, &ctx, move || {
                let r = access::online(&id, &relay);
                (id, relay, r)
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
        // Off, the card is only its switch: the ID, the password and the service show once it is on.
        'on: {
            if !cfg.enabled {
                break 'on;
            }
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
                // Only an answer about this ID at this relay counts.
                let online = self.access.online.lock().unwrap_or_else(|e| e.into_inner()).clone().filter(|(id, relay, _)| *id == cfg.id && *relay == cfg.relay).map(|(_, _, r)| r);
                // When the service runs remote access, this window's own loop is off: the relay says.
                let local = if svc.runs { access::State::Off } else { access::state() };
                let (color, text) = if !cfg.enabled {
                    (ui.visuals().weak_text_color(), "Off: nobody can connect by ID".to_string())
                } else {
                    match (&local, online) {
                        (access::State::InUse(n), _) => (good(), format!("A viewer is connected ({n})")),
                        (access::State::Failed(e), _) => (bad(), e.clone()),
                        (_, Some(Ok(true))) => (good(), format!("Ready: waiting at {}", cfg.relay)),
                        (_, Some(Ok(false))) => (bad(), if svc.runs { "Not at the relay yet: the service connects in a moment".into() } else { "Connecting to the relay\u{2026}".into() }),
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
                    if service && !svc.this {
                        let b = ui.add_enabled(admin, theme::primary(format!("{}  Update the service", ic::DOWNLOAD)));
                        if b.on_hover_text("The installed service runs another copy of sshremote; this one has what it lacks").on_disabled_hover_text("Needs administrator rights").clicked() {
                            self.error = access::install_service().err().map(|e| format!("Service: {e}"));
                            self.access.service_looked = None;
                            *self.access.service.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            self.service = None;
                        }
                    } else if service {
                        ui.label(RichText::new(format!("{}  Installed", ic::CHECK)).color(good()));
                        if ui.add_enabled(admin, egui::Button::new("Remove")).on_disabled_hover_text("Needs administrator rights").clicked() {
                            self.error = access::uninstall_service().err().map(|e| format!("Service: {e}"));
                            self.access.service_looked = None;
                            *self.access.service.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            self.service = None;
                        }
                    } else if admin {
                        if ui.add(theme::primary(format!("{}  Install", ic::DOWNLOAD))).clicked() {
                            cfg.enabled = true;
                            let _ = access::save(&cfg);
                            self.error = access::install_service().err().map(|e| format!("Service: {e}"));
                            self.access.service_looked = None;
                            *self.access.service.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
                        ui.add(egui::TextEdit::singleline(&mut self.access.new_password).password(true).hint_text("8 characters or more").desired_width(180.0));
                        if ui.add_enabled(self.access.new_password.trim().chars().count() >= 8, egui::Button::new("Set")).clicked() {
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
        }
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

/// What the Remote desktop extension's program says of remote access, as JSON.
#[cfg(not(feature = "remote"))]
fn ext_settings() -> Option<serde_json::Value> {
    let (program, _) = crate::ext::provider("remote")?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(["remote-access", "json"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

#[cfg(not(feature = "remote"))]
static EXT_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(not(feature = "remote"))]
impl App {
    /// The card when the Remote desktop extension brings remote access: its program changes the
    /// settings and keeps the line to the relay while this window is open.
    fn access_by_extension(&mut self, ui: &mut egui::Ui) {
        let call = |args: &[&str]| crate::ext::call_provided("remote", &args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
        if self.access.looked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3)) {
            self.access.looked = Some(std::time::Instant::now());
            in_background(&self.access.by_extension, &EXT_BUSY, ui.ctx(), || {
                // The first time, `status` makes the settings.
                ext_settings().or_else(|| {
                    let _ = crate::ext::call_provided("remote", &["remote-access".into(), "status".into()]);
                    ext_settings()
                })
            });
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
        let v = self.access.by_extension.lock().unwrap_or_else(|e| e.into_inner()).clone().flatten();
        let get = |k: &str| v.as_ref().and_then(|v| v.get(k)).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let on = v.as_ref().and_then(|v| v.get("enabled")).and_then(|x| x.as_bool()).unwrap_or(false);
        // While on, the extension's program keeps the line to the relay; it ends with this
        // window (`--parent`), and leaves it to the service when one runs.
        static CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);
        {
            let mut c = CHILD.lock().unwrap_or_else(|e| e.into_inner());
            if c.as_mut().is_some_and(|ch| ch.try_wait().ok().flatten().is_some()) {
                *c = None;
            }
            // Versions before 1.6.1 do not end with the window: they are not started from here.
            let fit = crate::ext::installed().iter().any(|m| m.provides.iter().any(|p| p == "remote") && !crate::ext::newer("1.6.1", &m.version));
            if on && c.is_none()
                && fit
                && let Some((program, _)) = crate::ext::provider("remote")
            {
                let mut cmd = std::process::Command::new(program);
                cmd.args(["remote-access", "run", "--parent", &std::process::id().to_string()]);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000);
                }
                *c = cmd.spawn().ok();
                if let Some(child) = c.as_ref() {
                    crate::sys::tie(child);
                }
            }
        }
        let mut result: Option<Result<(), String>> = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  Remote access by ID", ic::GLOBE)).size(15.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut want = on;
                if toggle(ui, &mut want).clicked() {
                    result = Some(call(&["remote-access", if want { "on" } else { "off" }]));
                }
            });
        });
        ui.label(RichText::new("Anyone with this ID and password can see and control this computer through the relay. Brought by the Remote desktop extension.").weak().size(12.0));
        // Off, only the switch; the ID, the password and the service show once it is on.
        if on {
            ui.add_space(6.0);
            let id = get("id");
            let spaced: String = id.as_bytes().chunks(3).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ");
            egui::Grid::new("access-ext").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new("ID").weak());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(spaced).size(22.0).strong().monospace());
                    if icon(ui, ic::COPY, "Copy the ID", false).clicked() {
                        ui.ctx().copy_text(id.clone());
                    }
                });
                ui.end_row();
                ui.label(RichText::new("Password").weak());
                ui.horizontal(|ui| {
                    let pw = get("password");
                    let shown = if self.access.show_password { pw.clone() } else { "\u{2022}".repeat(pw.chars().count()) };
                    ui.label(RichText::new(shown).size(18.0).monospace());
                    if icon(ui, if self.access.show_password { ic::EYE_SLASH } else { ic::EYE }, "Show or hide", false).clicked() {
                        self.access.show_password = !self.access.show_password;
                    }
                    if icon(ui, ic::COPY, "Copy the password", false).clicked() {
                        ui.ctx().copy_text(pw);
                    }
                    if icon(ui, ic::ARROWS_CLOCKWISE, "A new random password", false).clicked() {
                        result = Some(call(&["remote-access", "password"]));
                    }
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}  Start with the computer", ic::SHIELD_CHECK)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let admin = !cfg!(windows) || crate::admin::elevated();
                    if ui.add_enabled(admin, egui::Button::new("Remove")).on_disabled_hover_text("Needs administrator rights").clicked() {
                        result = Some(call(&["remote-access", "uninstall"]));
                    }
                    if ui.add_enabled(admin, theme::primary(format!("{}  Install", ic::DOWNLOAD))).on_disabled_hover_text("Needs administrator rights").clicked() {
                        result = Some(call(&["remote-access", "install"]));
                    }
                });
            });
        }
        if let Some(r) = result {
            self.access.looked = None;
            if let Err(e) = r {
                self.error = Some(format!("Remote access: {e}"));
            }
        }
    }
}
