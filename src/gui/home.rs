//! The home screen: connecting, the host list, the tools menu and the windows beside it.

use super::*;

impl App {
    pub(super) fn connect(&mut self, ctx: &egui::Context, local: bool) {
        let f = self.form.clone();
        let target = if local {
            Target::Local { capture: if f.capture == "auto" { "synthetic".into() } else { f.capture.clone() } }
        } else {
            let host = f.host.trim().to_string();
            if host.is_empty() {
                self.error = Some("Enter a host, for example user@192.168.1.20".into());
                return;
            }
            if let Some(addr) = crate::net::Address::parse(&host) {
                if f.password.is_empty() {
                    self.error = Some("Enter the one-time password shown by sshremote --serve".into());
                    return;
                }
                let target = Target::Direct { addr, password: f.password.clone() };
                return self.start(ctx, target, host);
            }
            if f.remember
                && !f.password.is_empty()
                && let Err(e) = crate::vault::set(&host, &f.password)
            {
                self.save_error = Some(format!("The password was not saved: {e}"));
            }
            // A password typed now wins over a saved one.
            let password = Some(f.password.clone()).filter(|p| !p.is_empty()).or_else(|| crate::vault::get(&host));
            Target::Ssh {
                host,
                port: f.port.trim().parse().ok(),
                args: Vec::new(),
                remote: f.remote.clone(),
                capture: f.capture.clone(),
                password,
                unattended: true,
            }
        };
        let label = if local { "local test desktop".to_string() } else { f.host.trim().to_string() };
        self.start(ctx, target, label);
    }

    pub(super) fn start(&mut self, ctx: &egui::Context, target: Target, label: String) {
        let f = self.form.clone();
        let local = matches!(target, Target::Local { .. });
        // Remembered once the connection works, so typos do not pile up in the list.
        self.pending_host = (!local).then(|| (f.host.trim().to_string(), f.port.trim().to_string()));
        self.pending_ssh = match &target {
            Target::Ssh { host, port, args, .. } => {
                let mut cmd = vec!["ssh".to_string()];
                if let Some(p) = port {
                    cmd.extend(["-p".into(), p.to_string()]);
                }
                cmd.extend(args.iter().cloned());
                cmd.push(host.clone());
                Some(cmd)
            }
            _ => None,
        };
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = Session::open(&target, true).map(|(s, _tx, rx)| (s, rx));
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.error = self.save_error.take();
        self.form.password.clear();
        self.screen = Screen::Connecting { started: Instant::now(), rx, label };
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

    /// Favorites, then recent hosts, then ~/.ssh/config aliases, without repeats.
    pub(super) fn host_list(&self) -> Vec<(String, String, Source)> {
        let mut out: Vec<(String, String, Source)> = Vec::new();
        let port_of = |h: &str| self.recent.iter().find(|r| r.0 == h).map(|r| r.1.clone()).unwrap_or_default();
        for h in &self.favorites {
            out.push((h.clone(), port_of(h), Source::Favorite));
        }
        for (h, p) in &self.recent {
            if !out.iter().any(|o| &o.0 == h) {
                out.push((h.clone(), p.clone(), Source::Recent));
            }
        }
        for h in &self.ssh_hosts {
            if !out.iter().any(|o| &o.0 == h) {
                out.push((h.clone(), String::new(), Source::SshConfig));
            }
        }
        let q = self.search.trim().to_lowercase();
        out.retain(|o| q.is_empty() || o.0.to_lowercase().contains(&q));
        out
    }

    /// Opens an ssh session to `host` in a tab of the stand-alone terminal.
    pub(super) fn open_terminal(&mut self, host: &str, port: &str) {
        let host = host.trim();
        if host.is_empty() {
            self.error = Some("Enter a host, for example user@192.168.1.20".into());
            return;
        }
        if host.starts_with("sshremote://") {
            self.error = Some(
                "sshremote:// addresses reach the remote screen only; choose Remote screen in the ... menu.".into(),
            );
            return;
        }
        self.error = None;
        let profile = crate::termwin::ssh_profile(host, port);
        self.open_profile(profile);
        self.recent.retain(|(r, _)| r != host);
        self.recent.insert(0, (host.to_string(), port.trim().to_string()));
        self.recent.truncate(crate::hosts::RECENT_MAX);
        crate::hosts::save_recent(&self.recent);
        crate::hosts::mark_seen(host);
        self.seen = crate::hosts::last_seen();
    }

    pub(super) fn open_profile(&mut self, profile: crate::termwin::Profile) {
        match &mut self.terms {
            Some(t) => t.open(profile),
            None => self.terms = Some(crate::termwin::TermWindow::standalone(profile, Vec::new())),
        }
    }

    pub(super) fn system_terminal(&mut self, host: &str, port: &str) {
        let mut cmd = vec!["ssh".to_string()];
        if !port.trim().is_empty() {
            cmd.extend(["-p".into(), port.trim().to_string()]);
        }
        cmd.push(host.trim().to_string());
        if let Err(e) = crate::termwin::spawn_system(&cmd) {
            self.error = Some(format!("cannot open the system terminal: {e}"));
        }
    }

    /// The windows that live beside every screen: the terminal and the SSH tools.
    pub(super) fn side_windows(&mut self, ctx: &egui::Context) {
        if let Some(mut t) = self.terms.take() {
            let cmds = t.show(ctx, "");
            self.torn_terms.extend(t.take_torn());
            let closed = cmds.iter().any(|c| matches!(c, crate::termwin::Cmd::Close));
            if !closed {
                self.terms = Some(t);
            }
        }
        crate::termwin::TermWindow::show_torn(ctx, &mut self.torn_terms);
        let mut actions = Vec::new();
        if let Some(k) = &mut self.keys
            && !k.show(ctx, &mut actions)
        {
            self.keys = None;
        }
        for a in actions {
            match a {
                crate::tools::ToolAction::Terminal { title, argv } => self.open_profile(crate::termwin::Profile {
                    name: title,
                    icon: ic::KEY,
                    launch: crate::termwin::Launch::Local(argv),
                }),
            }
        }
        let mut changed = false;
        if let Some(c) = &mut self.config
            && !c.show(ctx, &mut changed)
        {
            self.config = None;
        }
        let known: Vec<String> =
            if self.vault.is_some() { self.host_list().into_iter().map(|h| h.0).collect() } else { Vec::new() };
        if let Some(v) = &mut self.vault
            && !v.show(ctx, &known)
        {
            self.vault = None;
        }
        if let Some(b) = &mut self.backup
            && !b.show(ctx, &mut changed)
        {
            self.backup = None;
        }
        if changed {
            self.ssh_hosts = crate::hosts::ssh_config_hosts();
            self.recent = crate::hosts::load_recent();
            self.favorites = crate::hosts::favorites();
            self.seen = crate::hosts::last_seen();
        }
    }

    /// Administrator or standard user; a click on the latter asks for the rights.
    pub(super) fn admin_badge(&mut self, ui: &mut egui::Ui) {
        let (glyph, text, color) = if crate::admin::elevated() {
            (ic::SHIELD_CHECK, "Administrator", GOOD)
        } else {
            (ic::SHIELD, "Standard user", ui.visuals().weak_text_color())
        };
        let b = egui::Button::new(RichText::new(format!("{glyph}  {text}")).size(12.5).color(color)).frame(false);
        let tip = if crate::admin::elevated() {
            crate::admin::ADDS.to_string()
        } else {
            format!("Click to restart with administrator rights.\n\n{}", crate::admin::ADDS)
        };
        if ui.add(b).on_hover_text(tip).clicked() && !crate::admin::elevated() {
            self.restart_elevated(ui.ctx());
        }
    }

    pub(super) fn restart_elevated(&mut self, ctx: &egui::Context) {
        match crate::admin::relaunch(&[]) {
            Ok(true) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Ok(false) => {
                self.error = Some("Administrator rights were not given; sshremote goes on without them.".into())
            }
            Err(e) => self.error = Some(format!("Cannot ask for administrator rights: {e}")),
        }
    }

    /// The tools menu's part about this computer: administrator rights and the service.
    pub(super) fn admin_items(&mut self, ui: &mut egui::Ui) {
        #[cfg(windows)]
        {
            let admin = crate::admin::elevated();
            ui.label(RichText::new("This computer").small().weak());
            let installed = *self.service.get_or_insert_with(crate::admin::service_installed);
            let need = "Needs administrator rights: restart sshremote as administrator first.";
            let (label, tip) = if installed {
                (format!("{}  Remove the sshremote service", ic::SHIELD), "Stop and remove the service")
            } else {
                (
                    format!("{}  Install the sshremote service", ic::SHIELD_CHECK),
                    "Lets this computer be controlled at the sign-in and lock screens and receive Ctrl+Alt+Del",
                )
            };
            let r = ui.add_enabled(admin, egui::Button::new(label)).on_hover_text(tip).on_disabled_hover_text(need);
            if r.clicked() {
                let result = if installed { crate::service::uninstall() } else { crate::service::install() };
                self.service = None;
                self.error = result.err().map(|e| format!("Service: {e}"));
                ui.close();
            }
            if !admin && ui.button(format!("{}  Restart as administrator", ic::SHIELD_CHECK)).clicked() {
                self.restart_elevated(ui.ctx());
                ui.close();
            }
            let mut ask = crate::admin::ask_at_start();
            if ui.checkbox(&mut ask, "Ask for administrator rights at start").changed() {
                crate::admin::set_ask_at_start(ask);
            }
        }
        #[cfg(not(windows))]
        let _ = ui;
    }

    pub(super) fn connect_screen(&mut self, ui: &mut egui::Ui) {
        // What the screen asks for: (host, port, how).
        #[derive(Clone, Copy, PartialEq)]
        enum How {
            Terminal,
            Screen,
            Files,
            System,
            TestDesktop,
        }
        let mut go: Option<(String, String, How)> = None;
        let mut starred: Option<(String, bool)> = None;
        let mut forget: Option<String> = None;
        let ctx = ui.ctx().clone();
        let dark = ui.visuals().dark_mode;
        let card = ui.visuals().window_fill;
        let line = ui.visuals().widgets.noninteractive.bg_stroke.color;

        // The title bar is the header: the app's name, its tools and the window's buttons.
        let bar = super::chrome::title_bar(ui, |ui| {
            ui.label(RichText::new("SSH Remote").size(14.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (glyph, tip) = if dark { (ic::SUN, "Light theme") } else { (ic::MOON, "Dark theme") };
                if icon(ui, glyph, tip, false).clicked() {
                    set_theme(ui.ctx(), !dark);
                }
                icon_menu(ui, ic::WRENCH, |ui| {
                    ui.set_min_width(220.0);
                    if ui.button(format!("{}  SSH keys", ic::KEY)).clicked() {
                        self.keys = Some(crate::tools::KeyTool::new());
                        ui.close();
                    }
                    if ui.button(format!("{}  SSH hosts (~/.ssh/config)", ic::GLOBE)).clicked() {
                        self.config = Some(crate::tools::ConfigTool::new());
                        ui.close();
                    }
                    if ui.button(format!("{}  Saved passwords", ic::LOCK_KEY)).clicked() {
                        self.vault = Some(crate::tools::VaultTool::new(self.form.host.trim()));
                        ui.close();
                    }
                    if ui.button(format!("{}  Backup and restore", ic::ARCHIVE)).clicked() {
                        self.backup = Some(crate::tools::BackupTool::new());
                        ui.close();
                    }
                    ui.separator();
                    ui.label(RichText::new("Local terminal").small().weak());
                    for p in crate::termwin::local_profiles() {
                        if ui.button(format!("{}  {}", p.icon, p.name)).clicked() {
                            self.open_profile(p);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button(format!("{}  Built-in test desktop", ic::MONITOR)).clicked() {
                        go = Some((String::new(), String::new(), How::TestDesktop));
                        ui.close();
                    }
                    if cfg!(windows) {
                        ui.separator();
                        self.admin_items(ui);
                    }
                })
                .on_hover_text("Tools: SSH keys, SSH hosts, local terminal");
                if cfg!(windows) {
                    self.admin_badge(ui);
                }
                let local = icon(ui, ic::TERMINAL, "Local terminal", false);
                if local.clicked()
                    && let Some(p) = crate::termwin::local_profiles().into_iter().next()
                {
                    self.open_profile(p);
                }
            });
        });
        if bar.close {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let width = ui.available_width().min(760.0);
            let side = ((ui.available_width() - width) / 2.0).max(0.0);
            let full = ui.available_rect_before_wrap();
            let column = Rect::from_min_size(full.min + Vec2::new(side, 14.0), Vec2::new(width, full.height() - 14.0));
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(column).layout(egui::Layout::top_down(egui::Align::Min)));
            let ui = &mut ui;

            // Quick connect: the host, Terminal by default, everything else behind "...".
            egui::Frame::new().fill(card).stroke(Stroke::new(1.0, line)).corner_radius(CornerRadius::same(12)).inner_margin(14.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Connect to a host").strong());
                ui.horizontal(|ui| {
                    let field_w = ui.available_width() - 190.0;
                    let host = ui.add(
                        egui::TextEdit::singleline(&mut self.form.host)
                            .hint_text("user@host  or an ~/.ssh/config alias")
                            .desired_width(field_w)
                            .margin(egui::Margin::symmetric(8, 7)),
                    );
                    let enter = host.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let b = egui::Button::new(RichText::new(format!("{}  Terminal", ic::TERMINAL)).color(Color32::WHITE))
                        .fill(ACCENT)
                        .min_size(Vec2::new(112.0, 32.0));
                    if ui.add(b).on_hover_text("Open an SSH terminal (Enter)").clicked() || enter {
                        go = Some((self.form.host.clone(), self.form.port.clone(), How::Terminal));
                    }
                    let more = egui::containers::menu::MenuButton::new(RichText::new(ic::DOTS).size(16.0));
                    more.ui(ui, |ui| {
                        ui.set_min_width(230.0);
                        if ui.button(format!("{}  Remote screen", ic::MONITOR)).clicked() {
                            go = Some((self.form.host.clone(), self.form.port.clone(), How::Screen));
                            ui.close();
                        }
                        if ui.button(format!("{}  Files", ic::FOLDER)).clicked() {
                            go = Some((self.form.host.clone(), self.form.port.clone(), How::Files));
                            ui.close();
                        }
                        if ui.button(format!("{}  System terminal", ic::ARROW_SQUARE_OUT)).clicked() {
                            go = Some((self.form.host.clone(), self.form.port.clone(), How::System));
                            ui.close();
                        }
                    })
                    .0
                    .on_hover_text("Remote screen, files and more");
                    let opts = ui.add(tool_button(ic::GEAR, self.show_options)).on_hover_text("Connection options");
                    if opts.clicked() {
                        self.show_options = !self.show_options;
                    }
                });
                if self.show_options {
                    ui.add_space(4.0);
                    egui::Grid::new("connect-options").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new("Port").weak());
                        ui.add(egui::TextEdit::singleline(&mut self.form.port).hint_text("22").desired_width(70.0));
                        ui.end_row();
                        ui.label(RichText::new("Password").weak()).on_hover_text(
                            "For the remote screen. Leave empty to sign in with SSH keys; sshremote:// addresses need the password sshremote --serve shows. Terminals ask in the tab.",
                        );
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.form.password).password(true).hint_text("optional with SSH keys").desired_width(260.0));
                            ui.add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.form.remember, "Remember"))
                                .on_hover_text("Keep it in the system's password store; terminal tabs use it too");
                        });
                        ui.end_row();
                        ui.label(RichText::new("Screen quality").weak());
                        let current = self.form.preset.map_or("Auto (adapts to the connection)".into(), |i| preset_text(&PRESETS[i]));
                        egui::ComboBox::from_id_salt("quality").selected_text(current).width(260.0).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.form.preset, None, "Auto (adapts to the connection)");
                            for (i, p) in PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut self.form.preset, Some(i), preset_text(p));
                            }
                        });
                        ui.end_row();
                        ui.label(RichText::new("Agent command").weak()).on_hover_text("How the remote screen starts sshremote on the host");
                        ui.add(egui::TextEdit::singleline(&mut self.form.remote).desired_width(260.0));
                        ui.end_row();
                        ui.label(RichText::new("Capture").weak());
                        ui.horizontal(|ui| {
                            for c in ["auto", "gnome", "windows", "synthetic"] {
                                ui.selectable_value(&mut self.form.capture, c.to_string(), c);
                            }
                        });
                        ui.end_row();
                    });
                }
                if let Some(e) = &self.error {
                    let (what, hint) = friendly(e);
                    ui.add_space(4.0);
                    egui::Frame::new()
                        .fill(BAD.gamma_multiply(0.15))
                        .stroke(Stroke::new(1.0, BAD.gamma_multiply(0.6)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("Could not connect").strong().color(BAD));
                            ui.label(&what).on_hover_text(e.as_str());
                            if !hint.is_empty() {
                                ui.label(RichText::new(hint).weak());
                            }
                        });
                }
            });

            ui.add_space(14.0);
            let hosts = self.host_list();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Connections").size(16.0).strong());
                ui.label(RichText::new(format!("{}", hosts.len())).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(format!("{}  Search", ic::SEARCH)).desired_width(200.0));
                });
            });
            ui.add_space(2.0);
            // Only the list scrolls; the header and the connect card stay put.
            egui::Frame::new().fill(card).stroke(Stroke::new(1.0, line)).corner_radius(CornerRadius::same(12)).inner_margin(4.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                // As tall as its rows, scrolling once they fill the window.
                let room = (ui.available_height() - 12.0).max(80.0);
                if hosts.is_empty() {
                    let text = if self.search.is_empty() {
                        "Hosts you connect to, and the aliases in ~/.ssh/config, appear here."
                    } else {
                        "No host matches the search."
                    };
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new(text).weak()));
                    return;
                }
                egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(room).show(ui, |ui| {
                    for (i, (h, p, src)) in hosts.iter().enumerate() {
                        let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), egui::Sense::click());
                        if r.hovered() || self.form.host == *h {
                            let fill = if r.hovered() { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().faint_bg_color };
                            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
                        }
                        if i > 0 {
                            ui.painter().hline(rect.x_range().shrink(12.0), rect.top(), Stroke::new(1.0, line.gamma_multiply(0.5)));
                        }
                        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(10.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                        row.style_mut().interaction.selectable_labels = false;
                        badge(&mut row, h);
                        row.vertical(|ui| {
                            ui.add_space(9.0);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let name = if p.is_empty() { h.clone() } else { format!("{h}:{p}") };
                            ui.label(RichText::new(name).strong());
                            let when = match (src, self.seen.get(h)) {
                                (_, Some(t)) => crate::hosts::ago(*t),
                                (Source::SshConfig, None) => "from ~/.ssh/config".into(),
                                _ => "never connected".into(),
                            };
                            let how = if h.starts_with("sshremote://") { "direct" } else { "SSH" };
                            ui.label(RichText::new(format!("{how}  ·  {when}")).small().weak());
                        });
                        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let direct = h.starts_with("sshremote://");
                            egui::containers::menu::MenuButton::from_button(tool_button(ic::DOTS, false)).ui(ui, |ui| {
                                ui.set_min_width(210.0);
                                if !direct && ui.button(format!("{}  Terminal", ic::TERMINAL)).clicked() {
                                    go = Some((h.clone(), p.clone(), How::Terminal));
                                }
                                if ui.button(format!("{}  Remote screen", ic::MONITOR)).clicked() {
                                    go = Some((h.clone(), p.clone(), How::Screen));
                                }
                                if ui.button(format!("{}  Files", ic::FOLDER)).clicked() {
                                    go = Some((h.clone(), p.clone(), How::Files));
                                }
                                if !direct && ui.button(format!("{}  System terminal", ic::ARROW_SQUARE_OUT)).clicked() {
                                    go = Some((h.clone(), p.clone(), How::System));
                                }
                                ui.separator();
                                if ui.button(format!("{}  Copy address", ic::COPY)).clicked() {
                                    ui.ctx().copy_text(h.clone());
                                }
                                if *src == Source::Recent && ui.button(format!("{}  Remove from the list", ic::TRASH)).clicked() {
                                    forget = Some(h.clone());
                                }
                            });
                            let fav = *src == Source::Favorite;
                            let mut star = RichText::new(ic::STAR).size(16.0);
                            star = if fav { star.family(filled()).color(WARN) } else { star.color(ui.visuals().weak_text_color()) };
                            let tip = if fav { "Remove from favorites" } else { "Add to favorites" };
                            if ui.add(egui::Button::new(star).frame_when_inactive(false).min_size(Vec2::splat(28.0))).on_hover_text(tip).clicked() {
                                starred = Some((h.clone(), !fav));
                            }
                            if icon(ui, ic::FOLDER, "Files: transfer files to and from the host", false).clicked() {
                                go = Some((h.clone(), p.clone(), How::Files));
                            }
                            if icon(ui, ic::MONITOR, "Remote screen", false).clicked() {
                                go = Some((h.clone(), p.clone(), How::Screen));
                            }
                            if !direct {
                                let b = egui::Button::new(format!("{}  Terminal", ic::TERMINAL)).min_size(Vec2::new(0.0, 28.0));
                                if ui.add(b).on_hover_text("Open an SSH terminal").clicked() {
                                    go = Some((h.clone(), p.clone(), How::Terminal));
                                }
                            }
                        });
                        if r.clicked() {
                            (self.form.host, self.form.port) = (h.clone(), p.clone());
                        }
                        if r.double_clicked() {
                            let how = if h.starts_with("sshremote://") { How::Screen } else { How::Terminal };
                            go = Some((h.clone(), p.clone(), how));
                        }
                        r.on_hover_text("Double-click for a terminal");
                    }
                });
            });
        });

        if let Some((h, on)) = starred {
            crate::hosts::set_favorite(&h, on);
            self.favorites = crate::hosts::favorites();
        }
        if let Some(h) = forget {
            self.recent.retain(|(r, _)| *r != h);
            crate::hosts::save_recent(&self.recent);
        }
        if let Some((h, p, how)) = go {
            match how {
                How::Terminal => self.open_terminal(&h, &p),
                How::System => self.system_terminal(&h, &p),
                How::Screen | How::Files => {
                    (self.form.host, self.form.port) = (h, p);
                    self.start_with = if how == How::Files { Start::Files } else { Start::Screen };
                    self.connect(&ctx, false);
                }
                How::TestDesktop => self.connect(&ctx, true),
            }
        }
    }
}
