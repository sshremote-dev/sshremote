//! The home screen: connecting, the host list, the tools menu and the windows beside it.

use super::*;

impl App {
    /// The command line's connection (`sshremote --gui HOST`, `--local`), opened in the workspace.
    pub(super) fn connect(&mut self, local: bool) {
        let mode = match self.start_with {
            Start::Terminal => PaneMode::Terminal,
            Start::Files => PaneMode::Files,
            Start::Screen => PaneMode::Screen,
        };
        let start = (self.start_panels.0, self.start_panels.1, self.start_fullscreen);
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

    pub(super) fn open_profile(&mut self, profile: crate::termwin::Profile) {
        self.choose((String::new(), String::new(), How::Local(profile)), Place::Tab);
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
    }

    /// The host lists again, after a tool changed them.
    fn reload_hosts(&mut self) {
        {
            self.ssh_hosts = crate::hosts::ssh_config_hosts();
            self.recent = crate::hosts::load_recent();
            self.favorites = crate::hosts::favorites();
            self.seen = crate::hosts::last_seen();
        }
    }

    /// Administrator or standard user; a click on the latter asks for the rights.
    pub(super) fn admin_badge(&mut self, ui: &mut egui::Ui) {
        let (glyph, text, color) = if crate::admin::elevated() {
            (ic::SHIELD_CHECK, "Administrator", good())
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

    /// The open window: its title bar, then the open view.
    pub(super) fn connect_screen(&mut self, ui: &mut egui::Ui) {
        let dark = ui.visuals().dark_mode;
        let bar = super::chrome::title_bar(ui, |ui| {
            ui.label(RichText::new("SSH Remote").size(14.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let mut chosen = None;
        egui::CentralPanel::default().show(ui, |ui| chosen = self.open_view(ui));
        if let Some(c) = chosen {
            self.choose(c, Place::Tab);
        }
    }

    /// What to open, as a file dialog lays it out: the places on the left (a new host, this
    /// computer, the SSH hosts, the tools), their choices on the right.
    pub(super) fn open_view(&mut self, ui: &mut egui::Ui) -> Option<Choice> {
        let mut go: Option<Choice> = None;
        let full = ui.available_rect_before_wrap();
        let side = Rect::from_min_size(full.min, Vec2::new(176.0, full.height()));
        let body = full.with_min_x(side.right() + 12.0);
        let line = ui.visuals().widgets.noninteractive.bg_stroke.color;
        ui.painter().vline(side.right() + 4.0, full.y_range(), Stroke::new(1.0, line));
        let mut nav = ui.new_child(egui::UiBuilder::new().max_rect(side).layout(egui::Layout::top_down(egui::Align::Min)));
        nav.spacing_mut().item_spacing.y = 2.0;
        nav.add_space(4.0);
        for (s, glyph, name) in [
            (Section::New, ic::PLUS, "New connection"),
            (Section::Local, ic::DESKTOP, "This computer"),
            (Section::Ssh, ic::GLOBE, "SSH hosts"),
            (Section::Tools, ic::WRENCH, "Tools"),
        ] {
            let on = self.section == s;
            let (rect, r) = nav.allocate_exact_size(Vec2::new(side.width() - 8.0, 32.0), egui::Sense::click());
            if on {
                nav.painter().rect_filled(rect, theme::R_SM, selected_fill());
                nav.painter().rect_stroke(rect, theme::R_SM, Stroke::new(1.0, selected_stroke()), egui::StrokeKind::Inside);
            } else if r.hovered() {
                nav.painter().rect_filled(rect, theme::R_SM, nav.visuals().widgets.hovered.weak_bg_fill);
            }
            let fg = if on { nav.visuals().strong_text_color() } else { nav.visuals().text_color() };
            nav.painter().text(rect.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER, glyph, FontId::proportional(15.0), fg);
            nav.painter().text(rect.left_center() + Vec2::new(34.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(13.5), fg);
            if r.clicked() {
                self.section = s;
            }
        }
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut ui;
        ui.add_space(4.0);
        match self.section {
            Section::New => self.new_section(ui, &mut go),
            Section::Local => {
                ui.label(RichText::new("This computer").size(16.0).strong());
                ui.add_space(6.0);
                let row = place_row;
                for p in crate::termwin::local_profiles() {
                    if row(ui, p.icon, &p.name, "A terminal here") {
                        go = Some((String::new(), String::new(), How::Local(p.clone())));
                    }
                }
                ui.separator();
                if row(ui, ic::MONITOR, "This computer's screen", "Through the local agent, as a remote host would be") {
                    go = Some((String::new(), String::new(), How::This(PaneMode::Screen)));
                }
                if row(ui, ic::FOLDER, "This computer's files", "The file manager on both sides") {
                    go = Some((String::new(), String::new(), How::This(PaneMode::Files)));
                }
                // A synthetic screen for trying the viewer, in development builds only.
                if cfg!(feature = "test-desktop") && row(ui, ic::APP_WINDOW, "Built-in test desktop", "A synthetic screen, to try the viewer") {
                    go = Some((String::new(), String::new(), How::TestDesktop));
                }
            }
            Section::Ssh => self.ssh_section(ui, &mut go),
            Section::Tools => self.tools_section(ui),
        }
        go
    }

    /// The tools: a list of them, or the one open, as a page with a way back.
    fn tools_section(&mut self, ui: &mut egui::Ui) {
        let open = [self.keys.is_some(), self.config.is_some(), self.vault.is_some(), self.backup.is_some()];
        let Some(which) = open.iter().position(|&o| o) else {
            ui.label(RichText::new("Tools").size(16.0).strong());
            ui.add_space(6.0);
            if place_row(ui, ic::KEY, "SSH keys", "Your key pairs: copy one, install it on a host, or create one") {
                self.keys = Some(crate::tools::KeyTool::new());
            }
            if place_row(ui, ic::GLOBE, "SSH hosts", "The hosts of ~/.ssh/config: names, users, ports and keys") {
                self.config = Some(crate::tools::ConfigTool::new());
            }
            if place_row(ui, ic::LOCK_KEY, "Saved passwords", "Passwords for SSH sign-in, in the system's password store") {
                self.vault = Some(crate::tools::VaultTool::new(self.form.host.trim()));
            }
            if place_row(ui, ic::ARCHIVE, "Backup and restore", "Hosts, settings and ~/.ssh, by hand or each day") {
                self.backup = Some(crate::tools::BackupTool::new());
            }
            if place_row(ui, ic::EYE, "Appearance", "Fonts, colours, a background picture, the logo and an overlay of the load and time") {
                super::look::open(ui.ctx());
            }
            if cfg!(windows) {
                ui.add_space(6.0);
                ui.separator();
                self.admin_items(ui);
            }
            return;
        };
        let (glyph, title) = [
            (ic::KEY, "SSH keys"),
            (ic::GLOBE, "SSH hosts (~/.ssh/config)"),
            (ic::LOCK_KEY, "Saved passwords"),
            (ic::ARCHIVE, "Backup and restore"),
        ][which];
        let mut back = false;
        ui.horizontal(|ui| {
            back = icon(ui, ic::ARROW_LEFT, "Back to the tools", false).clicked();
            ui.label(RichText::new(format!("{glyph}  {title}")).size(16.0).strong());
        });
        ui.add_space(4.0);
        let known: Vec<String> = if which == 2 { self.host_list().into_iter().map(|h| h.0).collect() } else { Vec::new() };
        let mut actions = Vec::new();
        let mut changed = false;
        egui::ScrollArea::vertical().id_salt("tool-page").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width((ui.available_width() - 14.0).min(720.0));
            if let Some(k) = &mut self.keys {
                k.ui(ui, &mut actions);
            } else if let Some(c) = &mut self.config {
                c.ui(ui, &mut changed);
            } else if let Some(v) = &mut self.vault {
                v.ui(ui, &known);
            } else if let Some(b) = &mut self.backup {
                b.ui(ui, &mut changed);
            }
            ui.add_space(12.0);
        });
        for a in actions {
            match a {
                crate::tools::ToolAction::Terminal { title, argv } => self.open_profile(crate::termwin::Profile {
                    name: title,
                    icon: ic::KEY,
                    launch: crate::termwin::Launch::Local(argv),
                }),
            }
        }
        if changed {
            self.reload_hosts();
        }
        let escape = !ui.ctx().any_popup_open() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if back || escape {
            // From a form back to its tool's list; from the list back to the tools.
            let form = self.keys.as_ref().is_some_and(|t| t.sub_page())
                || self.config.as_ref().is_some_and(|t| t.sub_page())
                || self.vault.as_ref().is_some_and(|t| t.sub_page())
                || self.backup.as_ref().is_some_and(|t| t.sub_page());
            if form {
                if let Some(t) = self.keys.as_mut() {
                    t.back();
                }
                if let Some(t) = self.config.as_mut() {
                    t.back();
                }
                if let Some(t) = self.vault.as_mut() {
                    t.back();
                }
                if let Some(t) = self.backup.as_mut() {
                    t.back();
                }
            } else if !self.config.as_mut().is_some_and(|c| c.keep_open()) {
                self.keys = None;
                self.config = None;
                self.vault = None;
                self.backup = None;
            }
        }
    }

    /// A host not in the list: its address, what to open, and the connection's options.
    fn new_section(&mut self, ui: &mut egui::Ui, go: &mut Option<Choice>) {
        ui.label(RichText::new("New connection").size(16.0).strong());
        ui.add_space(6.0);
        let host = ui.add(
            egui::TextEdit::singleline(&mut self.form.host)
                .hint_text("user@host  or an ~/.ssh/config alias")
                .desired_width(ui.available_width())
                .margin(egui::Margin::symmetric(8, 7)),
        );
        let enter = host.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for mode in PaneMode::ALL {
                let b = theme::primary(format!("{}  {}", mode.glyph(), mode.name())).min_size(Vec2::new(0.0, 32.0));
                if ui.add(b).clicked() || (enter && mode == PaneMode::Terminal) {
                    let how = match mode {
                        PaneMode::Terminal => How::Terminal,
                        PaneMode::Screen => How::Screen,
                        PaneMode::Files => How::Files,
                    };
                    *go = Some((self.form.host.clone(), self.form.port.clone(), how));
                }
            }
            if icon(ui, ic::ARROW_SQUARE_OUT, "The system's own terminal (ssh)", false).clicked() {
                *go = Some((self.form.host.clone(), self.form.port.clone(), How::System));
            }
        });
        ui.add_space(10.0);
        egui::Grid::new("connect-options").num_columns(2).spacing([12.0, 8.0]).striped(false).show(ui, |ui| {
            ui.label(RichText::new("Port").weak());
            ui.add(egui::TextEdit::singleline(&mut self.form.port).hint_text("22").desired_width(70.0));
            ui.end_row();
            ui.label(RichText::new("Password").weak()).on_hover_text(
                "For the remote screen and files. Leave empty to sign in with SSH keys; sshremote:// addresses need the password sshremote --serve shows.",
            );
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.form.password).password(true).hint_text("optional with SSH keys").desired_width(220.0));
                ui.add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.form.remember, "Remember"))
                    .on_hover_text("Keep it in the system's password store");
            });
            ui.end_row();
            ui.label(RichText::new("Screen quality").weak());
            let current = self.form.preset.map_or("Auto (adapts to the connection)".into(), |i| preset_text(&PRESETS[i]));
            combo("quality", current).width(260.0).show_ui(ui, |ui| {
                crate::gui::menu_look(ui);
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
                // Chosen as in a menu: a quiet tint, not a colour.
                crate::gui::menu_look(ui);
                for c in ["auto", "gnome", "windows", "synthetic"] {
                    ui.selectable_value(&mut self.form.capture, c.to_string(), c);
                }
            });
            ui.end_row();
        });
        if let Some(e) = &self.error {
            let (what, hint) = friendly(e);
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(bad().gamma_multiply(0.15))
                .stroke(Stroke::new(1.0, bad().gamma_multiply(0.6)))
                .corner_radius(CornerRadius::same(theme::R_MD))
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Could not open").strong().color(bad()));
                    ui.label(&what).on_hover_text(e.as_str());
                    if !hint.is_empty() {
                        ui.label(RichText::new(hint).weak());
                    }
                });
        }
    }

    /// The known hosts: favorites, recent ones and ~/.ssh/config aliases, with search.
    fn ssh_section(&mut self, ui: &mut egui::Ui, go: &mut Option<Choice>) {
        let mut starred: Option<(String, bool)> = None;
        let mut forget: Option<String> = None;
        let hosts = self.host_list();
        let line = ui.visuals().widgets.noninteractive.bg_stroke.color;
        ui.horizontal(|ui| {
            ui.label(RichText::new("SSH hosts").size(16.0).strong());
            ui.label(RichText::new(format!("{}", hosts.len())).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(format!("{}  Search", ic::SEARCH)).desired_width(180.0));
            });
        });
        ui.add_space(4.0);
        if hosts.is_empty() {
            let text = if self.search.is_empty() {
                "Hosts you connect to, and the aliases in ~/.ssh/config, appear here."
            } else {
                "No host matches the search."
            };
            ui.add_space(16.0);
            ui.label(RichText::new(text).weak());
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, (h, p, src)) in hosts.iter().enumerate() {
                let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), egui::Sense::click());
                if r.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(theme::R_SM), ui.visuals().widgets.hovered.weak_bg_fill);
                }
                if i > 0 {
                    ui.painter().hline(rect.x_range().shrink(8.0), rect.top(), Stroke::new(1.0, line.gamma_multiply(0.5)));
                }
                let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                row.style_mut().interaction.selectable_labels = false;
                badge(&mut row, h);
                row.vertical(|ui| {
                    ui.add_space(8.0);
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
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let direct = h.starts_with("sshremote://");
                    egui::containers::menu::MenuButton::from_button(tool_button(ic::DOTS, false)).ui(ui, |ui| {
                        crate::gui::menu_look(ui);
                        ui.set_min_width(210.0);
                        if !direct && ui.button(format!("{}  System terminal", ic::ARROW_SQUARE_OUT)).clicked() {
                            *go = Some((h.clone(), p.clone(), How::System));
                        }
                        if ui.button(format!("{}  Copy address", ic::COPY)).clicked() {
                            ui.ctx().copy_text(h.clone());
                        }
                        if *src == Source::Recent && ui.button(format!("{}  Remove from the list", ic::TRASH)).clicked() {
                            forget = Some(h.clone());
                        }
                    });
                    let fav = *src == Source::Favorite;
                    let mut star = RichText::new(ic::STAR).size(16.0);
                    star = if fav { star.family(filled()).color(warn()) } else { star.color(ui.visuals().weak_text_color()) };
                    let tip = if fav { "Remove from favorites" } else { "Add to favorites" };
                    if ui.add(egui::Button::new(star).frame_when_inactive(false).min_size(Vec2::splat(28.0))).on_hover_text(tip).clicked() {
                        starred = Some((h.clone(), !fav));
                    }
                    if icon(ui, ic::FOLDER, "Files", false).clicked() {
                        *go = Some((h.clone(), p.clone(), How::Files));
                    }
                    if icon(ui, ic::MONITOR, "Screen sharing", false).clicked() {
                        *go = Some((h.clone(), p.clone(), How::Screen));
                    }
                    if !direct && icon(ui, ic::TERMINAL, "Terminal", false).clicked() {
                        *go = Some((h.clone(), p.clone(), How::Terminal));
                    }
                });
                if r.clicked() {
                    (self.form.host, self.form.port) = (h.clone(), p.clone());
                }
                if r.double_clicked() {
                    let how = if h.starts_with("sshremote://") { How::Screen } else { How::Terminal };
                    *go = Some((h.clone(), p.clone(), how));
                }
                r.on_hover_text("Double-click for a terminal");
            }
        });
        if let Some((h, on)) = starred {
            crate::hosts::set_favorite(&h, on);
            self.favorites = crate::hosts::favorites();
        }
        if let Some(h) = forget {
            self.recent.retain(|(r, _)| *r != h);
            crate::hosts::save_recent(&self.recent);
        }
    }

    /// Opens what was chosen: in a new tab, or beside the active pane.
    pub(super) fn choose(&mut self, (host, port, how): Choice, place: Place) {
        self.error = None;
        let start = (false, false, false);
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
                        self.error = Some(e);
                        self.section = Section::New;
                        return;
                    }
                }
            }
        };
        self.queued.push((launch, place, start));
    }

    /// A launch for `host`: a plain ssh terminal (nothing needed on the host), or the agent.
    pub(super) fn host_launch(&mut self, host: &str, port: &str, mode: PaneMode) -> Result<Launch, String> {
        let host = host.trim().to_string();
        if host.is_empty() {
            return Err("Enter a host, for example user@192.168.1.20".into());
        }
        if let Some(addr) = crate::net::Address::parse(&host) {
            if mode == PaneMode::Terminal {
                return Err("sshremote:// addresses reach the screen and files only.".into());
            }
            if self.form.password.is_empty() {
                return Err("Enter the one-time password shown by sshremote --serve".into());
            }
            let target = Target::Direct { addr, password: std::mem::take(&mut self.form.password) };
            return Ok(Launch::Agent { target, label: host, ssh_cmd: None, mode });
        }
        let f = self.form.clone();
        if f.remember && !f.password.is_empty() && let Err(e) = crate::vault::set(&host, &f.password) {
            self.save_error = Some(format!("The password was not saved: {e}"));
        }
        // A password typed now wins over a saved one.
        let password = Some(f.password.clone()).filter(|p| !p.is_empty()).or_else(|| crate::vault::get(&host));
        self.form.password.clear();
        let port_n: Option<u16> = port.trim().parse().ok();
        let target = Target::Ssh {
            host: host.clone(),
            port: port_n,
            args: Vec::new(),
            remote: f.remote.clone(),
            capture: f.capture.clone(),
            password,
            unattended: true,
        };
        let mut cmd = vec!["ssh".to_string()];
        if let Some(p) = port_n {
            cmd.extend(["-p".into(), p.to_string()]);
        }
        cmd.push(host.clone());
        // A terminal is recorded when opened (it signs in in its tab); the screen and files once
        // the agent is reached, so mistyped hosts do not pile up.
        if mode == PaneMode::Terminal {
            self.remember_host(&host, port.trim());
        } else {
            self.pending_ports.insert(host.clone(), port.trim().to_string());
        }
        Ok(if mode == PaneMode::Terminal {
            Launch::Shell { profile: crate::termwin::ssh_profile(&host, port), label: host, target, ssh_cmd: Some(cmd) }
        } else {
            Launch::Agent { target, label: host, ssh_cmd: Some(cmd), mode }
        })
    }

    /// Puts `host` first in the recent list.
    pub(super) fn remember_host(&mut self, host: &str, port: &str) {
        self.recent.retain(|(r, _)| r != host);
        self.recent.insert(0, (host.to_string(), port.to_string()));
        self.recent.truncate(crate::hosts::RECENT_MAX);
        crate::hosts::save_recent(&self.recent);
        crate::hosts::mark_seen(host);
        self.seen = crate::hosts::last_seen();
    }
}

/// The places of the open view.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Section {
    New,
    Local,
    Ssh,
    Tools,
}

/// What the open view asks to open.
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

/// A host, its port, and what to open on it.
pub(super) type Choice = (String, String, How);

/// A place to open or a tool, as a row: its icon, its name and a line about it.
fn place_row(ui: &mut egui::Ui, glyph: &str, title: &str, sub: &str) -> bool {
    let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), egui::Sense::click());
    if r.hovered() {
        ui.painter().rect_filled(rect, theme::R_SM, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let fg = ui.visuals().strong_text_color();
    ui.painter().text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, glyph, FontId::proportional(18.0), fg);
    ui.painter().text(Pos2::new(rect.left() + 44.0, rect.top() + 14.0), Align2::LEFT_CENTER, title, FontId::proportional(14.0), fg);
    let weak = ui.visuals().weak_text_color();
    ui.painter().text(Pos2::new(rect.left() + 44.0, rect.top() + 32.0), Align2::LEFT_CENTER, sub, FontId::proportional(11.5), weak);
    r.clicked()
}
