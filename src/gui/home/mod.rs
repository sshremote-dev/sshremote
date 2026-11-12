//! The home window. On the left, this computer as others reach it; on the right, going out: one
//! address field and the computers known. Settings, a computer's sheet and remote access's
//! settings open over it. Each fact is shown once, and no field hides in a drop-down.

use super::*;
use crate::gui::kit;

pub(super) mod connect;
pub(super) mod quick;
mod sheet;
mod this_computer;

pub(super) use sheet::Sheet;

/// The left column's width.
const LEFT_W: f32 = 320.0;

impl App {
    /// The home window: its title bar, then the two columns, and what opens over them.
    pub(super) fn home(&mut self, ui: &mut egui::Ui) {
        let dark = ui.visuals().dark_mode;
        let mut open_settings = false;
        let bar = super::chrome::title_bar(ui, |ui| {
            ui.label(RichText::new("SSH Remote").size(14.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                open_settings = icon(ui, ic::GEAR, "Settings", false).clicked();
                let (glyph, tip) = if dark { (ic::SUN, "Light theme") } else { (ic::MOON, "Dark theme") };
                if icon(ui, glyph, tip, false).clicked() {
                    set_theme(ui.ctx(), !dark);
                }
                if cfg!(windows) {
                    self.admin_badge(ui);
                }
            });
        });
        if bar.close {
            // Asked first when other windows or people connected by ID would go with it.
            self.close_clicked = true;
        }
        if open_settings {
            self.settings = Some(super::settings::Settings::new());
        }
        let mut go: Option<Choice> = None;
        let page = kit::PAGE;
        let frame = egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin { left: page, right: page, top: 20, bottom: 16 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            let left = Rect::from_min_size(full.min, Vec2::new(LEFT_W.min(full.width() * 0.42), full.height()));
            let right = full.with_min_x(left.right() + 24.0);
            let mut l = ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::top_down(egui::Align::Min)));
            egui::ScrollArea::vertical().id_salt("this-computer").auto_shrink([false, false]).show(&mut l, |ui| self.this_computer(ui));
            let mut r = ui.new_child(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::top_down(egui::Align::Min)));
            self.error_banner(&mut r);
            self.going_out(&mut r, &mut go);
        });
        if let Some(c) = go {
            self.choose(c, Place::Tab);
        }
        let ctx = ui.ctx().clone();
        self.computer_sheet(&ctx);
        self.access_sheet(&ctx);
        self.settings_window(&ctx);
    }

    /// What went wrong last, over the right column, until dismissed or another thing is done.
    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(e) = self.error.clone() else { return };
        let (what, hint) = friendly(&e);
        let mut close = false;
        egui::Frame::new()
            .fill(bad().gamma_multiply(0.12))
            .stroke(Stroke::new(1.0, bad().gamma_multiply(0.5)))
            .corner_radius(CornerRadius::same(theme::R_MD))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ic::WARNING).size(16.0).color(bad()));
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - 30.0);
                        ui.add(egui::Label::new(RichText::new(&what).color(ui.visuals().strong_text_color())).wrap()).on_hover_text(e.as_str());
                        if !hint.is_empty() {
                            ui.add(egui::Label::new(RichText::new(hint).weak()).wrap());
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        close = ui.add(egui::Button::new(RichText::new(ic::X).size(13.0)).frame_when_inactive(false)).on_hover_text("Dismiss").clicked();
                    });
                });
            });
        ui.add_space(10.0);
        if close {
            self.error = None;
        }
    }

    /// The command line's connection (`sshremote --gui HOST`, `--local`), opened in the workspace.
    pub(super) fn connect(&mut self, local: bool) {
        let mode = match self.start_with {
            Start::Terminal => PaneMode::Terminal,
            Start::Files => PaneMode::Files,
            Start::Screen => PaneMode::Screen,
        };
        let start = PaneStart { panel: self.start_panels.0, chat: self.start_panels.1, fullscreen: self.start_fullscreen };
        let launch = if local {
            let capture = if self.form.capture == "auto" { "synthetic".to_string() } else { self.form.capture.clone() };
            Launch::Agent { target: Target::Local { capture }, label: "Test desktop".into(), ssh_cmd: None, mode }
        } else {
            let host = self.form.host.clone();
            let port = self.form.port.clone();
            // A terminal asked for here goes through the agent, as before.
            match self.host_launch(&host, &port, if mode == PaneMode::Terminal { PaneMode::Screen } else { mode }) {
                Ok(Launch::Agent { target, label, ssh_cmd, .. }) => Launch::Agent { target, label, ssh_cmd, mode },
                Ok(l) => l,
                Err(e) => {
                    self.error = Some(e);
                    return;
                }
            }
        };
        self.queued.push((launch, Place::Tab, start));
    }

    pub(super) fn options(&self) -> Options {
        Options {
            host: None,
            local: false,
            gui: true,
            remote: String::new(),
            capture: String::new(),
            ssh_args: Vec::new(),
            password: None,
            preset: self.form.preset,
            budget: self.budget,
            mode: None,
            scale: None,
            fps: None,
            cell: None,
            probe: None,
            probe_type: String::new(),
            probe_scroll: 0,
            probe_send: None,
            probe_fetch: None,
            probe_monitor: None,
            sound: false,
            open: String::new(),
        }
    }

    pub(super) fn open_profile(&mut self, profile: crate::termwin::Profile) {
        self.choose((String::new(), String::new(), How::Local(profile)), Place::Tab);
    }

    pub(super) fn system_terminal(&mut self, host: &str, port: &str) {
        let (_, cmd) = ssh_command(host, port);
        if let Err(e) = crate::termwin::spawn_system(&cmd) {
            self.error = Some(format!("Cannot open the system terminal: {e}"));
        }
    }

    /// Administrator or not: a quiet shield when it is, and "Standard user" to click for the rights
    /// when it is not, since that is the one that asks for something.
    pub(super) fn admin_badge(&mut self, ui: &mut egui::Ui) {
        if crate::admin::elevated() {
            ui.label(RichText::new(ic::SHIELD_CHECK).size(15.0).color(ui.visuals().weak_text_color()))
                .on_hover_text(format!("Running as administrator.\n\n{}", crate::admin::ADDS));
            return;
        }
        let b = egui::Button::new(RichText::new(format!("{}  Standard user", ic::SHIELD)).size(12.5).color(ui.visuals().weak_text_color())).frame(false);
        let tip = format!("Click to restart with administrator rights.\n\n{}", crate::admin::ADDS);
        if ui.add(b).on_hover_text(tip).clicked() {
            self.restart_elevated(ui.ctx());
        }
    }

    pub(super) fn restart_elevated(&mut self, ctx: &egui::Context) {
        match crate::admin::relaunch(&[]) {
            Ok(true) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Ok(false) => self.error = Some("Administrator rights were not given; sshremote goes on without them.".into()),
            Err(e) => self.error = Some(format!("Cannot ask for administrator rights: {e}")),
        }
    }

    /// Opens what was chosen: in a new tab, or beside the active pane.
    pub(super) fn choose(&mut self, (host, port, how): Choice, place: Place) {
        self.error = None;
        let start = PaneStart::default();
        let launch = match how {
            How::System => {
                self.system_terminal(&host, &port);
                return;
            }
            How::Local(profile) => Launch::Shell {
                label: profile.name.clone(),
                profile,
                target: Target::Local { capture: "auto".into() },
                ssh_cmd: None,
            },
            How::This(mode) => {
                Launch::Agent { target: Target::Local { capture: "auto".into() }, label: "This computer".into(), ssh_cmd: None, mode }
            }
            How::TestDesktop => Launch::Agent {
                target: Target::Local { capture: "synthetic".into() },
                label: "Test desktop".into(),
                ssh_cmd: None,
                mode: PaneMode::Screen,
            },
            How::Terminal | How::Screen | How::Files => {
                let mode = match how {
                    How::Terminal => PaneMode::Terminal,
                    How::Files => PaneMode::Files,
                    _ => PaneMode::Screen,
                };
                match self.host_launch(&host, &port, mode) {
                    Ok(l) => l,
                    Err(e) => {
                        // Shown over the computers, with the address it was about in the field.
                        self.error = Some(e);
                        if !host.is_empty() && !self.in_field(&host) {
                            self.form.host = with_port(&host, &port);
                            self.form.password.clear();
                        }
                        self.focus_host = true;
                        return;
                    }
                }
            }
        };
        self.queued.push((launch, place, start));
    }

    /// A launch for `host`: a plain ssh terminal (nothing needed on the host), or the agent. A
    /// password typed in the address row goes to the computer in the field only, never to another
    /// one opened from the list.
    pub(super) fn host_launch(&mut self, host: &str, port: &str, mode: PaneMode) -> Result<Launch, String> {
        let host = host.trim().to_string();
        if host.is_empty() {
            return Err("Enter a computer: an alias, user@host, an IP or an ID".into());
        }
        let typed = if self.in_field(&host) { std::mem::take(&mut self.form.password) } else { String::new() };
        // Kept once the connection works (a terminal signs in inside itself: kept at once).
        let by_agent = mode != PaneMode::Terminal || host.starts_with("sshremote://");
        if !typed.is_empty() && self.form.remember {
            if by_agent {
                self.pending_passwords.insert(host.clone(), typed.clone());
            } else if let Err(e) = crate::vault::set(&host, &typed) {
                self.error = Some(format!("The password was not saved: {e}"));
            }
        }
        // A password typed now wins over a saved one.
        let password = Some(typed).filter(|p| !p.is_empty()).or_else(|| crate::vault::get(&host));
        #[cfg(feature = "remote")]
        if let Some(addr) = crate::net::Address::parse(&host) {
            if mode == PaneMode::Terminal {
                return Err("A computer reached by its ID shows its screen and files, not a terminal.".into());
            }
            let Some(password) = password else {
                return Err("Enter the password set on that computer for its remote access ID.".into());
            };
            // In the list once reached, as other computers are.
            self.pending_ports.insert(host.clone(), String::new());
            let label = crate::computers::get(&host).map_or_else(|| format!("ID {}", crate::computers::spaced(&addr.id)), |c| c.label());
            let target = Target::Direct { addr, password };
            return Ok(Launch::Agent { target, label, ssh_cmd: None, mode });
        }
        let (port_n, cmd) = ssh_command(&host, port);
        let target = Target::Ssh {
            host: host.clone(),
            port: port_n,
            args: Vec::new(),
            remote: self.form.remote.clone(),
            capture: self.form.capture.clone(),
            password,
            unattended: true,
        };
        let label = crate::computers::get(&host).map_or_else(|| host.clone(), |c| c.label());
        // A terminal is recorded when opened (it signs in in its tab); the screen and files once
        // the agent is reached, so mistyped hosts do not pile up.
        if mode == PaneMode::Terminal {
            self.remember_host(&host, port.trim());
        } else {
            self.pending_ports.insert(host.clone(), port.trim().to_string());
        }
        Ok(if mode == PaneMode::Terminal {
            Launch::Shell { profile: crate::termwin::ssh_profile(&host, port), label, target, ssh_cmd: Some(cmd) }
        } else {
            Launch::Agent { target, label, ssh_cmd: Some(cmd), mode }
        })
    }

    /// Whether `host` is the computer the address field holds (with a port after it or not): a
    /// password typed there is for it alone.
    fn in_field(&self, host: &str) -> bool {
        match connect::typed(&self.form.host) {
            connect::Typed::Ssh { host: h, .. } => h == host,
            // The ID typed, not one that starts with the same digits.
            connect::Typed::Id { address, id } => address == host || host.ends_with(&format!("//{id}")) || host.contains(&format!("//{id}@")),
            _ => false,
        }
    }

    /// Opened now: first among the recently used, with its port.
    pub(super) fn remember_host(&mut self, host: &str, port: &str) {
        crate::computers::used(host, port.trim().parse().ok());
        self.computers_at = None;
    }
}

/// What the home window asks to open.
#[derive(Clone)]
pub(super) enum How {
    Terminal,
    Screen,
    Files,
    System,
    TestDesktop,
    Local(crate::termwin::Profile),
    This(PaneMode),
}

/// An address, its port, and what to open on it.
pub(super) type Choice = (String, String, How);

/// An address as the field takes it: `host:port`, `[v6]:port`, or the host alone.
fn with_port(host: &str, port: &str) -> String {
    let port = port.trim();
    if port.is_empty() || port == "22" {
        return host.to_string();
    }
    let (user, at) = host.rsplit_once('@').map_or((String::new(), host), |(u, h)| (format!("{u}@"), h));
    if at.contains(':') { format!("{user}[{at}]:{port}") } else { format!("{user}{at}:{port}") }
}

