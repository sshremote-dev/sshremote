//! The open view: connecting (an address field above the hosts known), this computer, the
//! extensions and the settings, with this computer's ID and password always in sight beside them.

use super::*;

impl App {
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
        let (_, cmd) = ssh_command(host, port);
        if let Err(e) = crate::termwin::spawn_system(&cmd) {
            self.error = Some(format!("Cannot open the system terminal: {e}"));
        }
    }

    /// The host lists again, after a tool changed them.
    fn reload_hosts(&mut self) {
        self.ssh_hosts = crate::hosts::ssh_config_hosts();
        self.recent = crate::hosts::load_recent();
        self.favorites = crate::hosts::favorites();
        self.seen = crate::hosts::last_seen();
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
            Ok(false) => {
                self.error = Some("Administrator rights were not given; sshremote goes on without them.".into())
            }
            Err(e) => self.error = Some(format!("Cannot ask for administrator rights: {e}")),
        }
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
            // Asked first when other windows or people connected by ID would go with it.
            self.close_clicked = true;
        }
        let mut chosen = None;
        egui::CentralPanel::default().show(ui, |ui| chosen = self.open_view(ui));
        if let Some(c) = chosen {
            self.choose(c, Place::Tab);
        }
    }

    /// What to open: the places on the left under this computer's ID, the chosen one on the right.
    pub(super) fn open_view(&mut self, ui: &mut egui::Ui) -> Option<Choice> {
        let mut go: Option<Choice> = None;
        let full = ui.available_rect_before_wrap();
        let side = Rect::from_min_size(full.min, Vec2::new(SIDE_W, full.height()));
        let body = full.with_min_x(side.right() + 16.0);
        let line = ui.visuals().widgets.noninteractive.bg_stroke.color;
        ui.painter().vline(side.right() + 6.0, full.y_range(), Stroke::new(1.0, line));
        let mut nav = ui.new_child(egui::UiBuilder::new().max_rect(side).layout(egui::Layout::top_down(egui::Align::Min)));
        self.side_bar(&mut nav);
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut ui;
        ui.add_space(4.0);
        match self.section {
            Section::Connect => self.connect_section(ui, &mut go),
            Section::Local => self.local_section(ui, &mut go),
            Section::Extensions => {
                page_title(ui, "Extensions", "Looks, commands, status bar items, panels and features. Each one can do only what its card says.");
                self.error_banner(ui);
                let exts = self.exts.get_or_insert_with(crate::ext::ExtTool::new);
                egui::ScrollArea::vertical().id_salt("extensions").auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_max_width((ui.available_width() - 14.0).min(760.0));
                    exts.ui(ui);
                    ui.add_space(12.0);
                });
            }
            Section::Settings => self.settings_section(ui),
        }
        go
    }

    /// The side bar: this computer's ID and password on top, then the places.
    fn side_bar(&mut self, nav: &mut egui::Ui) {
        nav.spacing_mut().item_spacing.y = 2.0;
        #[cfg(feature = "remote")]
        if crate::modules::on("screen") {
            let v = nav.visuals().clone();
            let mut to_local = false;
            egui::Frame::new()
                .fill(v.faint_bg_color)
                .stroke(Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color))
                .corner_radius(CornerRadius::same(theme::R_MD))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(nav, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 3.0;
                    to_local = self.access_summary(ui);
                });
            if to_local {
                self.go_to(Section::Local);
            }
            nav.add_space(10.0);
        }
        for (s, glyph, name) in [
            (Section::Connect, ic::PLUGS, "Connect"),
            (Section::Local, ic::DESKTOP, "This computer"),
            (Section::Extensions, ic::PUZZLE_PIECE, "Extensions"),
            (Section::Settings, ic::GEAR, "Settings"),
        ] {
            let on = self.section == s;
            let (rect, r) = nav.allocate_exact_size(Vec2::new(nav.available_width(), 34.0), egui::Sense::click());
            if on {
                nav.painter().rect_filled(rect, theme::R_SM, selected_fill());
                // The place shown is marked with the app's blue along its left edge.
                let mark = Rect::from_min_size(rect.left_top() + Vec2::new(0.0, 8.0), Vec2::new(3.0, rect.height() - 16.0));
                nav.painter().rect_filled(mark, 1.5, super::brand());
            } else if r.hovered() {
                nav.painter().rect_filled(rect, theme::R_SM, nav.visuals().widgets.hovered.weak_bg_fill);
            }
            let fg = if on { nav.visuals().strong_text_color() } else { nav.visuals().text_color() };
            nav.painter().text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, glyph, FontId::proportional(16.0), fg);
            nav.painter().text(rect.left_center() + Vec2::new(38.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(14.0), fg);
            // Updates waiting, as a count beside Extensions.
            if s == Section::Extensions
                && let Some(n) = crate::ext::updates_waiting().filter(|n| *n > 0)
            {
                let at = rect.right_center() - Vec2::new(14.0, 0.0);
                nav.painter().circle_filled(at, 8.0, super::brand());
                nav.painter().text(at, Align2::CENTER_CENTER, n.to_string(), FontId::proportional(10.5), Color32::WHITE);
            }
            let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
            if r.clicked() {
                self.go_to(s);
            }
        }
    }

    /// Shows a place; on Connect the address field takes the keyboard.
    pub(super) fn go_to(&mut self, s: Section) {
        if self.section != s {
            self.error = None;
        }
        self.section = s;
        self.focus_host = s == Section::Connect;
    }

    /// Connect: the address field and its button, what it opens, the options folded away, then
    /// the hosts known. Enter connects; a click on a host puts it in the field, a double click
    /// connects to it.
    fn connect_section(&mut self, ui: &mut egui::Ui, go: &mut Option<Choice>) {
        // Digits only: an ID that remote access gave another computer, reached through the relay.
        let digits: String = self.form.host.chars().filter(|c| !c.is_whitespace()).collect();
        let by_id = (6..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
        // An ID has a screen and files, no terminal.
        let mode = if by_id && self.form.mode == PaneMode::Terminal { PaneMode::Screen } else { self.form.mode };
        page_title(ui, "Connect", "A host you reach with SSH, or a computer by its remote access ID.");
        let mut enter = false;
        let mut connect = false;
        ui.horizontal(|ui| {
            let button_w = 132.0;
            let field = egui::TextEdit::singleline(&mut self.form.host)
                .hint_text("user@host, an alias of ~/.ssh/config, or an ID")
                .desired_width((ui.available_width() - button_w - 8.0).max(160.0))
                .font(FontId::proportional(15.0))
                .margin(egui::Margin::symmetric(10, 8));
            let r = ui.add(field);
            if std::mem::take(&mut self.focus_host) {
                r.request_focus();
            }
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let tip = format!("Open {} on it (Enter)", mode.name().to_lowercase());
            connect |= ui.add(theme::primary(format!("{}  Connect", mode.glyph())).min_size(Vec2::new(button_w, 36.0))).on_hover_text(tip).clicked();
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Opens").weak());
            for m in PaneMode::ALL.into_iter().filter(|m| m.on()) {
                let enabled = !(by_id && m == PaneMode::Terminal);
                let b = ui.add_enabled(enabled, choice(format!("{}  {}", m.glyph(), m.name()), mode == m));
                if b.on_disabled_hover_text("A computer reached by its ID shows its screen and files").clicked() {
                    self.form.mode = m;
                    crate::hosts::set_setting("connect_mode", m.key());
                }
            }
            if !by_id && icon(ui, ic::ARROW_SQUARE_OUT, "Open ssh in the system's own terminal instead", false).clicked() {
                *go = Some((self.form.host.trim().to_string(), self.form.port.clone(), How::System));
            }
        });
        if by_id {
            // The password set on that computer: needed, so it is not folded away.
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.form.password)
                        .password(true)
                        .hint_text("Password of that computer")
                        .desired_width(260.0)
                        .margin(egui::Margin::symmetric(8, 6)),
                );
                enter |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let r = ui.add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.form.remember, "Remember"))
                    .on_hover_text("Keep it in the system's password store for this ID, so it is not asked again");
                if r.changed() {
                    crate::hosts::set_setting("remember_password", if self.form.remember { "yes" } else { "no" });
                }
            });
        }
        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new("Options").size(13.0)).id_salt("connect-options").show(ui, |ui| {
            self.connect_options(ui, by_id);
        });
        self.error_banner(ui);
        if (connect || enter) && !self.form.host.trim().is_empty() {
            #[cfg(feature = "remote")]
            let target = if by_id { crate::access::address(&digits, &crate::access::relay()) } else { self.form.host.trim().to_string() };
            #[cfg(not(feature = "remote"))]
            let target = self.form.host.trim().to_string();
            let how = match mode {
                PaneMode::Terminal => How::Terminal,
                PaneMode::Screen => How::Screen,
                PaneMode::Files => How::Files,
            };
            *go = Some((target, self.form.port.clone(), how));
        } else if connect {
            self.error = Some("Enter a host, for example user@192.168.1.20, or a remote access ID.".into());
        }
        ui.add_space(14.0);
        self.host_section(ui, go);
    }

    /// The connection's options: port, password, quality, and how the agent starts.
    fn connect_options(&mut self, ui: &mut egui::Ui, by_id: bool) {
        egui::Grid::new("connect-options-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            if !by_id {
                ui.label(RichText::new("Port").weak());
                ui.add(egui::TextEdit::singleline(&mut self.form.port).hint_text("22").desired_width(70.0));
                ui.end_row();
                ui.label(RichText::new("Password").weak()).on_hover_text("Leave it empty to sign in with your SSH keys.");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.form.password).password(true).hint_text("optional with SSH keys").desired_width(220.0));
                    let r = ui.add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.form.remember, "Remember"))
                        .on_hover_text("Keep it in the system's password store for this host, so it is not asked again");
                    if r.changed() {
                        crate::hosts::set_setting("remember_password", if self.form.remember { "yes" } else { "no" });
                    }
                });
                ui.end_row();
            }
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
            if !by_id {
                ui.label(RichText::new("Agent command").weak()).on_hover_text("How the screen and files start sshremote on the host");
                ui.add(egui::TextEdit::singleline(&mut self.form.remote).desired_width(320.0));
                ui.end_row();
                ui.label(RichText::new("Capture").weak()).on_hover_text("How the host's screen is read: auto picks the one for its desktop");
                ui.horizontal(|ui| {
                    crate::gui::menu_look(ui);
                    let test = cfg!(feature = "test-desktop").then_some("synthetic");
                    for c in ["auto", "gnome", "windows"].into_iter().chain(test) {
                        ui.selectable_value(&mut self.form.capture, c.to_string(), c);
                    }
                });
                ui.end_row();
            }
        });
    }

    /// Why the last thing asked for did not happen, with a hint and a way to put it away.
    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(e) = self.error.clone() else { return };
        let (what, hint) = friendly(&e);
        ui.add_space(8.0);
        let mut close = false;
        egui::Frame::new()
            .fill(bad().gamma_multiply(0.12))
            .stroke(Stroke::new(1.0, bad().gamma_multiply(0.5)))
            .corner_radius(CornerRadius::same(theme::R_MD))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width((ui.available_width()).min(760.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ic::WARNING).size(16.0).color(bad()));
                    ui.vertical(|ui| {
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
        if close {
            self.error = None;
        }
    }

    /// The known hosts under the address field: favorites, recent ones and ~/.ssh/config aliases.
    fn host_section(&mut self, ui: &mut egui::Ui, go: &mut Option<Choice>) {
        let hosts = self.host_list();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Hosts").size(15.0).strong());
            if !hosts.is_empty() || !self.search.is_empty() {
                ui.label(RichText::new(hosts.len().to_string()).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(format!("{}  Search", ic::SEARCH)).desired_width(180.0));
                });
            }
        });
        ui.add_space(2.0);
        if hosts.is_empty() {
            let text = if self.search.is_empty() {
                "The hosts you connect to, your favorites and the aliases in ~/.ssh/config show here."
            } else {
                "No host matches the search."
            };
            ui.add_space(10.0);
            ui.label(RichText::new(text).weak());
            return;
        }
        let mut act = None;
        egui::ScrollArea::vertical().id_salt("hosts").auto_shrink([false, false]).show(ui, |ui| {
            for (i, (h, p, src)) in hosts.iter().enumerate() {
                if let Some(a) = self.host_row(ui, i, h, p, *src) {
                    act = Some((h.clone(), p.clone(), a));
                }
            }
        });
        let Some((h, p, a)) = act else { return };
        match a {
            RowAct::Pick => {
                (self.form.host, self.form.port) = (h, p);
                self.form.password.clear();
                self.focus_host = true;
            }
            RowAct::Open(how) => *go = Some((h, p, how)),
            RowAct::Star(on) => {
                crate::hosts::set_favorite(&h, on);
                self.favorites = crate::hosts::favorites();
            }
            RowAct::Forget => {
                self.recent.retain(|(r, _)| *r != h);
                crate::hosts::save_recent(&self.recent);
            }
        }
    }

    /// One host as a row: its mark, its name and when it was last reached, and on the right what
    /// can be opened on it, the star and a menu.
    fn host_row(&self, ui: &mut egui::Ui, i: usize, h: &str, p: &str, src: Source) -> Option<RowAct> {
        let mut act = None;
        let line = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 50.0), egui::Sense::click());
        let picked = self.form.host.trim() == h;
        if picked {
            ui.painter().rect_filled(rect, CornerRadius::same(theme::R_SM), selected_fill());
        } else if r.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(theme::R_SM), ui.visuals().widgets.hovered.weak_bg_fill);
        }
        if i > 0 && !picked {
            ui.painter().hline(rect.x_range().shrink(8.0), rect.top(), Stroke::new(1.0, line.gamma_multiply(0.5)));
        }
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row.style_mut().interaction.selectable_labels = false;
        badge(&mut row, h);
        #[cfg(feature = "remote")]
        let id = crate::net::Address::parse(h).map(|a| a.id);
        #[cfg(not(feature = "remote"))]
        let id: Option<String> = None;
        // The buttons first, from the right edge, so a long name is cut short before them.
        let mut right = row.new_child(egui::UiBuilder::new().max_rect(row.max_rect()).layout(egui::Layout::right_to_left(egui::Align::Center)));
        right.spacing_mut().item_spacing.x = 2.0;
        egui::containers::menu::MenuButton::from_button(tool_button(ic::DOTS, false)).ui(&mut right, |ui| {
            crate::gui::menu_look(ui);
            ui.set_min_width(210.0);
            if id.is_none() && ui.button(format!("{}  System terminal", ic::ARROW_SQUARE_OUT)).clicked() {
                act = Some(RowAct::Open(How::System));
            }
            if ui.button(format!("{}  Copy address", ic::COPY)).clicked() {
                ui.ctx().copy_text(id.clone().unwrap_or_else(|| h.to_string()));
            }
            if src == Source::Recent && ui.button(format!("{}  Remove from the list", ic::TRASH)).clicked() {
                act = Some(RowAct::Forget);
            }
        });
        let fav = src == Source::Favorite;
        let mut star = RichText::new(ic::STAR).size(16.0);
        star = if fav { star.family(filled()).color(warn()) } else { star.color(right.visuals().weak_text_color()) };
        let tip = if fav { "Remove from favorites" } else { "Add to favorites" };
        if right.add(egui::Button::new(star).frame_when_inactive(false).min_size(Vec2::splat(28.0))).on_hover_text(tip).clicked() {
            act = Some(RowAct::Star(!fav));
        }
        if PaneMode::Files.on() && icon(&mut right, ic::FOLDER, "Files", false).clicked() {
            act = Some(RowAct::Open(How::Files));
        }
        if PaneMode::Screen.on() && icon(&mut right, ic::MONITOR, "Screen sharing", false).clicked() {
            act = Some(RowAct::Open(How::Screen));
        }
        if id.is_none() && icon(&mut right, ic::TERMINAL, "Terminal", false).clicked() {
            act = Some(RowAct::Open(How::Terminal));
        }
        let room = (right.min_rect().left() - row.cursor().left() - 8.0).max(40.0);
        row.allocate_ui_with_layout(Vec2::new(room, 46.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.add_space(7.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            #[cfg(feature = "remote")]
            let name = match &id {
                Some(id) => format!("ID {}", crate::access::spaced(id)),
                None if p.is_empty() => h.to_string(),
                None => format!("{h}:{p}"),
            };
            #[cfg(not(feature = "remote"))]
            let name = if p.is_empty() { h.to_string() } else { format!("{h}:{p}") };
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
            let when = match (src, self.seen.get(h)) {
                (_, Some(t)) => crate::hosts::ago(*t),
                (Source::SshConfig, None) => "from ~/.ssh/config".into(),
                _ => "not reached yet".into(),
            };
            let how = if id.is_some() { "by ID" } else { "SSH" };
            ui.add(egui::Label::new(RichText::new(format!("{how}  ·  {when}")).small().weak()).truncate());
        });
        if act.is_none() {
            if r.double_clicked() {
                let mode = if id.is_some() && self.form.mode == PaneMode::Terminal { PaneMode::Screen } else { self.form.mode };
                act = Some(RowAct::Open(match mode {
                    PaneMode::Terminal => How::Terminal,
                    PaneMode::Screen => How::Screen,
                    PaneMode::Files => How::Files,
                }));
            } else if r.clicked() {
                act = Some(RowAct::Pick);
            }
        }
        r.on_hover_text("Click to put it in the field above, double-click to connect");
        act
    }

    /// This computer: remote access by ID, and its own terminals, screen and files.
    fn local_section(&mut self, ui: &mut egui::Ui, go: &mut Option<Choice>) {
        page_title(ui, "This computer", "Others reach it by its ID; you open its terminals, screen and files here.");
        self.error_banner(ui);
        egui::ScrollArea::vertical().id_salt("local").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width((ui.available_width() - 14.0).min(760.0));
            self.access_card(ui);
            ui.add_space(12.0);
            ui.label(RichText::new("Open here").size(15.0).strong());
            ui.add_space(2.0);
            for p in crate::termwin::local_profiles() {
                if place_row(ui, p.icon, &p.name, "A terminal on this computer") {
                    *go = Some((String::new(), String::new(), How::Local(p.clone())));
                }
            }
            if PaneMode::Screen.on() && place_row(ui, ic::MONITOR, "This computer's screen", "Through the local agent, as a remote host would be") {
                *go = Some((String::new(), String::new(), How::This(PaneMode::Screen)));
            }
            if PaneMode::Files.on() && place_row(ui, ic::FOLDER, "This computer's files", "The file manager on both sides") {
                *go = Some((String::new(), String::new(), How::This(PaneMode::Files)));
            }
            // A synthetic screen for trying the viewer, in development builds only.
            if cfg!(feature = "test-desktop") && place_row(ui, ic::APP_WINDOW, "Built-in test desktop", "A synthetic screen, to try the viewer") {
                *go = Some((String::new(), String::new(), How::TestDesktop));
            }
        });
    }

    /// Settings: the look and the SSH tools as a list, or the one open as a page with a way back.
    fn settings_section(&mut self, ui: &mut egui::Ui) {
        let Some(mut tool) = self.tool.take() else {
            page_title(ui, "Settings", "The look of the windows, and your SSH keys, hosts, passwords and backups.");
            self.error_banner(ui);
            egui::ScrollArea::vertical().id_salt("settings").auto_shrink([false, false]).show(ui, |ui| {
                ui.set_max_width((ui.available_width() - 14.0).min(760.0));
                if place_row(ui, ic::PALETTE, "Appearance", "Theme, fonts, colours, background pictures and the load overlay") {
                    self.tool = Some(Tool::Appearance);
                }
                if place_row(ui, ic::KEY, "SSH keys", "Your key pairs: copy one, install it on a host, or create one") {
                    self.tool = Some(Tool::Keys(crate::tools::KeyTool::new()));
                }
                if place_row(ui, ic::FILE_TEXT, "SSH config file", "The hosts of ~/.ssh/config: names, users, ports and keys") {
                    self.tool = Some(Tool::Config(crate::tools::ConfigTool::new()));
                }
                if place_row(ui, ic::LOCK_KEY, "Saved passwords", "Passwords for SSH sign-in, in the system's password store") {
                    self.tool = Some(Tool::Vault(crate::tools::VaultTool::new(self.form.host.trim())));
                }
                if crate::modules::on("backup") && place_row(ui, ic::ARCHIVE, "Backup and restore", "Hosts, settings and ~/.ssh, by hand or each day") {
                    self.tool = Some(Tool::Backup(crate::tools::BackupTool::new()));
                }
                if cfg!(windows) {
                    ui.add_space(10.0);
                    ui.label(RichText::new("Administrator rights").size(15.0).strong());
                    ui.label(RichText::new(crate::admin::ADDS).weak().size(12.0));
                    ui.add_space(4.0);
                    if !crate::admin::elevated() && ui.add(theme::primary(format!("{}  Restart as administrator", ic::SHIELD_CHECK))).clicked() {
                        self.restart_elevated(ui.ctx());
                    }
                    #[cfg(windows)]
                    {
                        let mut ask = crate::admin::ask_at_start();
                        if ui.checkbox(&mut ask, "Ask for administrator rights at start").changed() {
                            crate::admin::set_ask_at_start(ask);
                        }
                    }
                }
            });
            return;
        };
        let (glyph, title) = tool.title();
        let mut back = false;
        ui.horizontal(|ui| {
            back = icon(ui, ic::ARROW_LEFT, "Back to the settings (Esc)", false).clicked();
            ui.label(RichText::new(format!("{glyph}  {title}")).size(18.0).strong());
        });
        ui.add_space(6.0);
        let known: Vec<String> = if matches!(tool, Tool::Vault(_)) { self.host_list().into_iter().map(|h| h.0).collect() } else { Vec::new() };
        let mut actions = Vec::new();
        let mut changed = false;
        egui::ScrollArea::vertical().id_salt("tool-page").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width((ui.available_width() - 14.0).min(720.0));
            match &mut tool {
                Tool::Appearance => super::look::page(ui),
                Tool::Keys(k) => k.ui(ui, &mut actions),
                Tool::Config(c) => c.ui(ui, &mut changed),
                Tool::Vault(v) => v.ui(ui, &known),
                Tool::Backup(b) => b.ui(ui, &mut changed),
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
        // From a tool's form back to its list; from the list back to the settings.
        self.tool = if back || escape { tool.back() } else { Some(tool) };
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
                        // Shown on Connect, with the host it was about in the field.
                        self.error = Some(e);
                        if !host.is_empty() && self.form.host.trim() != host {
                            (self.form.host, self.form.port) = (host, port);
                            self.form.password.clear();
                        }
                        self.go_to(Section::Connect);
                        return;
                    }
                }
            }
        };
        self.queued.push((launch, place, start));
    }

    /// A launch for `host`: a plain ssh terminal (nothing needed on the host), or the agent. A
    /// password typed on Connect goes to the host in its field only, never to another one opened
    /// from the list.
    pub(super) fn host_launch(&mut self, host: &str, port: &str, mode: PaneMode) -> Result<Launch, String> {
        let host = host.trim().to_string();
        if host.is_empty() {
            return Err("Enter a host, for example user@192.168.1.20".into());
        }
        let typed_here = {
            let field = self.form.host.trim();
            let digits: String = field.chars().filter(|c| !c.is_whitespace()).collect();
            field == host || (!digits.is_empty() && host.contains(&format!("//{digits}")))
        };
        let typed = if typed_here { std::mem::take(&mut self.form.password) } else { String::new() };
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
            // Recent once reached, as other hosts are.
            self.pending_ports.insert(host.clone(), String::new());
            // Named as people read it; the recent list and the password store keep the address.
            let label = format!("ID {}", crate::access::spaced(&addr.id));
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

/// The side bar's width.
const SIDE_W: f32 = 196.0;

/// The places of the open view.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Section {
    Connect,
    Local,
    Extensions,
    Settings,
}

/// A page of Settings while it is open.
pub(super) enum Tool {
    Appearance,
    Keys(crate::tools::KeyTool),
    Config(crate::tools::ConfigTool),
    Vault(crate::tools::VaultTool),
    Backup(crate::tools::BackupTool),
}

impl Tool {
    fn title(&self) -> (&'static str, &'static str) {
        match self {
            Tool::Appearance => (ic::PALETTE, "Appearance"),
            Tool::Keys(_) => (ic::KEY, "SSH keys"),
            Tool::Config(_) => (ic::FILE_TEXT, "SSH config file"),
            Tool::Vault(_) => (ic::LOCK_KEY, "Saved passwords"),
            Tool::Backup(_) => (ic::ARCHIVE, "Backup and restore"),
        }
    }

    /// One step back: from a form to its tool's list, from there out of the tool (unless a
    /// change still has to be kept or thrown away).
    fn back(mut self) -> Option<Tool> {
        let sub = match &mut self {
            Tool::Appearance => false,
            Tool::Keys(t) => t.sub_page().then(|| t.back()).is_some(),
            Tool::Config(t) => t.sub_page().then(|| t.back()).is_some(),
            Tool::Vault(t) => t.sub_page().then(|| t.back()).is_some(),
            Tool::Backup(t) => t.sub_page().then(|| t.back()).is_some(),
        };
        let keep = match &mut self {
            Tool::Config(c) => c.keep_open(),
            _ => false,
        };
        (sub || keep).then_some(self)
    }
}

/// What a host row asks for.
enum RowAct {
    /// Put it in the address field.
    Pick,
    Open(How),
    Star(bool),
    Forget,
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

/// A page's title and a line about it.
fn page_title(ui: &mut egui::Ui, title: &str, about: &str) {
    ui.label(RichText::new(title).size(20.0).strong());
    ui.label(RichText::new(about).weak().size(12.5));
    ui.add_space(10.0);
}

/// A place to open or a tool, as a row: its icon, its name and a line about it.
fn place_row(ui: &mut egui::Ui, glyph: &str, title: &str, sub: &str) -> bool {
    let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), egui::Sense::click());
    if r.hovered() {
        ui.painter().rect_filled(rect, theme::R_SM, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let fg = ui.visuals().strong_text_color();
    ui.painter().text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, glyph, FontId::proportional(18.0), fg);
    ui.painter().text(Pos2::new(rect.left() + 44.0, rect.top() + 15.0), Align2::LEFT_CENTER, title, FontId::proportional(14.0), fg);
    let weak = ui.visuals().weak_text_color();
    ui.painter().text(Pos2::new(rect.left() + 44.0, rect.top() + 33.0), Align2::LEFT_CENTER, sub, FontId::proportional(12.0), weak);
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}
