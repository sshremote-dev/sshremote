//! The right column: going out. One address field that knows what is typed into it (an SSH
//! address, a remote access ID, or words to find a computer by) and says what it can open there;
//! under it the computers known, this computer first, then the favorites and the last opened.

use super::*;
use crate::computers::Computer;
use crate::gui::kit;

/// What the address field holds.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::gui) enum Typed {
    Empty,
    /// A remote access ID, with the address it is reached at.
    Id { address: String, id: String },
    /// An SSH address (an alias, a name, `user@host`, an IP), with the port typed after it.
    Ssh { host: String, port: String },
    /// Words that are no address: they only find computers in the list.
    Words,
}

/// Reads the address field.
pub(in crate::gui) fn typed(text: &str) -> Typed {
    let t = text.trim();
    if t.is_empty() {
        return Typed::Empty;
    }
    let digits: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    if (6..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        return Typed::Id { address: id_address(&digits), id: digits };
    }
    if let Some(rest) = t.strip_prefix("sshremote://") {
        let id = rest.split('@').next().unwrap_or("").trim_end_matches('/');
        return if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) { Typed::Id { address: t.into(), id: id.into() } } else { Typed::Words };
    }
    if t.contains(char::is_whitespace) || t.starts_with('-') {
        return Typed::Words;
    }
    // A port after the host: host:2222, or [v6]:2222; a bare IPv6 address has no port.
    let (user, at) = match t.rsplit_once('@') {
        Some((u, h)) => (format!("{u}@"), h),
        None => (String::new(), t),
    };
    let (host, port) = if let Some(v6) = at.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, rest)) => (h.to_string(), rest.strip_prefix(':').unwrap_or("").to_string()),
            None => (v6.to_string(), String::new()),
        }
    } else {
        match at.split_once(':') {
            Some((h, p)) if !p.contains(':') && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h.to_string(), p.to_string()),
            _ => (at.to_string(), String::new()),
        }
    };
    if host.is_empty() {
        return Typed::Words;
    }
    Typed::Ssh { host: format!("{user}{host}"), port }
}

/// The address an ID is reached at, through the relay this computer uses.
fn id_address(id: &str) -> String {
    #[cfg(feature = "remote")]
    return crate::access::address(id, &crate::access::relay());
    #[cfg(not(feature = "remote"))]
    return format!("sshremote://{id}");
}

/// Whether `c` is found by the words `q` (lower case): its name, its address, its ID's digits.
fn finds(c: &Computer, q: &str) -> bool {
    let digits: String = q.chars().filter(|c| !c.is_whitespace()).collect();
    c.name.to_lowercase().contains(q)
        || c.address.to_lowercase().contains(q)
        || (!digits.is_empty() && c.id().is_some_and(|id| id.contains(&digits)))
}

/// A line of the list.
#[derive(Clone)]
enum Entry {
    Here,
    Computer(Computer),
}

/// What a row of the list asks for.
enum RowAct {
    Pick,
    Open(How),
    Star(bool),
    Edit,
    Remove,
    ForgetPassword,
    Wake,
}

/// Looking for computers on this network: the answer coming, what answered, and what to say.
#[derive(Default)]
pub(in crate::gui) struct Lan {
    coming: Option<std::sync::mpsc::Receiver<Vec<crate::lan::Found>>>,
    found: Vec<crate::lan::Found>,
    looked: bool,
    said: String,
}

impl App {
    /// The computers as kept, read again now and then (~/.ssh/config can change under the app).
    pub(super) fn load_computers(&mut self) {
        if self.computers_at.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5)) {
            self.computers = crate::computers::all();
            self.computers_at = Some(std::time::Instant::now());
        }
    }

    /// The field, what it holds, and the computers.
    pub(super) fn going_out(&mut self, ui: &mut egui::Ui, go: &mut Option<Choice>) {
        self.load_computers();
        let what = typed(&self.form.host);
        let entries = self.entries(&what);
        let r = ui.add(
            egui::TextEdit::singleline(&mut self.form.host)
                .hint_text(format!("{}  user@host, an alias, an IP or an ID", ic::SEARCH))
                .desired_width(f32::INFINITY)
                .font(FontId::proportional(15.0))
                .margin(egui::Margin::symmetric(12, 11)),
        );
        if std::mem::take(&mut self.focus_host) {
            r.request_focus();
        }
        if r.changed() {
            self.picked = None;
            self.ssh_password = false;
        }
        let mut enter = false;
        // The field lets go of the keyboard on Esc and Enter itself: it is still the one asked.
        if r.has_focus() || r.lost_focus() {
            let mut cleared = false;
            ui.input_mut(|i| {
                let last = entries.len().checked_sub(1);
                let before = self.picked;
                if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                    self.picked = match (self.picked, last) {
                        (_, None) => None,
                        (None, Some(_)) => Some(0),
                        (Some(p), Some(l)) => Some((p + 1).min(l)),
                    };
                }
                if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                    self.picked = self.picked.and_then(|p| p.checked_sub(1));
                }
                self.picked_moved = self.picked != before;
                // Esc clears the field first; on an empty one it closes what holds it.
                let clears = self.picked.is_some() || !self.form.host.is_empty();
                if clears && i.consume_key(egui::Modifiers::NONE, Key::Escape) {
                    if self.picked.is_some() {
                        self.picked = None;
                    } else {
                        self.form.host.clear();
                        self.form.password.clear();
                    }
                    cleared = true;
                }
            });
            if cleared {
                self.focus_host = true;
            }
            enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if enter {
                self.focus_host = true;
            }
        }
        ui.add_space(8.0);
        self.what_is_typed(ui, &what, enter && self.picked.is_none(), go);
        ui.add_space(kit::GAP);
        let picked = self.picked.filter(|p| *p < entries.len());
        if enter && let Some(p) = picked {
            *go = self.default_open(&entries[p]);
        }
        self.computer_list(ui, &what, &entries, picked, go);
    }

    /// The lines of the list for what is typed: this computer, then the computers found.
    fn entries(&self, what: &Typed) -> Vec<Entry> {
        // An address finds itself without the port typed after it.
        let q = match what {
            Typed::Empty => String::new(),
            Typed::Id { id, .. } => id.clone(),
            Typed::Ssh { host, .. } => host.to_lowercase(),
            Typed::Words => self.form.host.trim().to_lowercase(),
        };
        let here = self.here.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|h| h.name.to_lowercase()).unwrap_or_default();
        let mut out = Vec::new();
        if q.is_empty() || "this computer".contains(&q) || here.contains(&q) {
            out.push(Entry::Here);
        }
        out.extend(self.computers.iter().filter(|c| q.is_empty() || finds(c, &q)).cloned().map(Entry::Computer));
        out
    }

    /// Under the field: what it holds, and the ways to open it.
    fn what_is_typed(&mut self, ui: &mut egui::Ui, what: &Typed, enter: bool, go: &mut Option<Choice>) {
        let weak = ui.visuals().weak_text_color();
        let (address, port, by_id) = match what {
            Typed::Empty => {
                ui.label(RichText::new("Type where to go, or pick a computer below. Enter opens it; the arrow keys pick one in the list.").color(weak).size(12.5));
                return;
            }
            Typed::Words => {
                ui.label(RichText::new(format!("{}  Finding computers by these words.", ic::SEARCH)).color(weak).size(12.5));
                return;
            }
            Typed::Id { address, .. } => (address.clone(), String::new(), true),
            Typed::Ssh { host, port } => (host.clone(), port.clone(), false),
        };
        let known = self.computers.iter().find(|c| c.address == address).cloned();
        // A computer in the list opens on its own port unless another is typed.
        let port = if port.is_empty() { known.as_ref().and_then(|c| c.port).map(|p| p.to_string()).unwrap_or_default() } else { port };
        let mut open: Option<PaneMode> = None;
        kit::card(ui, |ui| {
                ui.horizontal(|ui| {
                    let chip = if by_id { "ID" } else { "SSH" };
                    egui::Frame::new().fill(super::super::brand().gamma_multiply(0.16)).corner_radius(CornerRadius::same(4)).inner_margin(egui::Margin::symmetric(7, 3)).show(ui, |ui| {
                        ui.label(RichText::new(chip).size(11.0).strong().color(super::super::brand()));
                    });
                    let shown = match what {
                        Typed::Id { id, .. } => crate::computers::spaced(id),
                        _ if port.is_empty() => address.clone(),
                        _ => format!("{address}  port {port}"),
                    };
                    // The lesser ways first, from the right edge, so a long address is cut before them.
                    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).layout(egui::Layout::right_to_left(egui::Align::Center)));
                    right.spacing_mut().item_spacing.x = 2.0;
                    if known.is_none() && icon(&mut right, ic::PLUS, "Add it to the computers, with a name, a key and tunnels", false).clicked() {
                        self.sheet = Some(Sheet::new(what));
                    }
                    if !by_id {
                        let tip = if self.ssh_password { "Sign in with SSH keys" } else { "Sign in with a password instead of SSH keys" };
                        if icon(&mut right, ic::KEY, tip, self.ssh_password).clicked() {
                            self.ssh_password = !self.ssh_password;
                        }
                        if icon(&mut right, ic::ARROW_SQUARE_OUT, "Open ssh in the system's own terminal", false).clicked() {
                            *go = Some((address.clone(), port.clone(), How::System));
                        }
                    }
                    let room = (right.min_rect().left() - ui.cursor().left() - 8.0).max(60.0);
                    ui.allocate_ui_with_layout(Vec2::new(room, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(shown).strong().size(14.0)).truncate());
                        if let Some(c) = known.as_ref().filter(|c| !c.name.is_empty()) {
                            ui.add(egui::Label::new(RichText::new(format!("\u{00b7} {}", c.name)).color(weak)).truncate());
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let default = self.default_mode(by_id);
                    for m in PaneMode::ALL.into_iter().filter(|m| m.on() && !(by_id && *m == PaneMode::Terminal)) {
                        let text = format!("{}  {}", m.glyph(), m.name());
                        let b = if m == default { theme::primary(text) } else { kit::secondary(text) };
                        let tip = if m == default { format!("Open {} on it (Enter)", m.name().to_lowercase()) } else { format!("Open {} on it", m.name().to_lowercase()) };
                        if ui.add(b.min_size(Vec2::new(104.0, kit::CONTROL_H))).on_hover_text(tip).clicked() {
                            open = Some(m);
                        }
                    }
                });
                // An ID needs the password set on that computer; SSH only when asked for, or
                // when the computer refused the keys.
                let saved = crate::vault::has(&address);
                let refused = self.error.as_deref().is_some_and(|e| e.to_lowercase().contains("permission denied"));
                if by_id || self.ssh_password || refused {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let hint = match (by_id, saved) {
                            (true, true) => "Password saved; type another to replace it",
                            (true, false) => "Password set on that computer",
                            (false, true) => "Password saved for it",
                            (false, false) => "Password on that computer",
                        };
                        let r = ui.add(egui::TextEdit::singleline(&mut self.form.password).password(true).hint_text(hint).desired_width(260.0).margin(egui::Margin::symmetric(8, 7)));
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            open = Some(self.default_mode(by_id));
                        }
                        let r = ui
                            .add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.form.remember, "Remember"))
                            .on_hover_text("Keep it in the system's password store, so it is not asked again");
                        if r.changed() {
                            crate::hosts::set_setting("remember_password", if self.form.remember { "yes" } else { "no" });
                        }
                    });
                }
        });
        if enter {
            open = Some(self.default_mode(by_id));
        }
        if let Some(m) = open {
            if m != PaneMode::Terminal || !by_id {
                self.form.mode = m;
                crate::hosts::set_setting("connect_mode", m.key());
            }
            *go = Some((address, port, how(m)));
        }
    }

    /// What Enter and a double click open: the last one chosen, a screen for an ID.
    pub(in crate::gui) fn default_mode(&self, by_id: bool) -> PaneMode {
        let m = self.form.mode;
        if (by_id && m == PaneMode::Terminal) || !m.on() { PaneMode::ALL.into_iter().find(|m| m.on() && *m != PaneMode::Terminal).unwrap_or(PaneMode::Screen) } else { m }
    }

    fn default_open(&self, e: &Entry) -> Option<Choice> {
        Some(match e {
            Entry::Here => match self.default_mode(false) {
                PaneMode::Terminal => (String::new(), String::new(), How::Local(crate::termwin::local_profiles().into_iter().next()?)),
                m => (String::new(), String::new(), How::This(m)),
            },
            Entry::Computer(c) => (c.address.clone(), c.port.map(|p| p.to_string()).unwrap_or_default(), how(self.default_mode(c.id().is_some()))),
        })
    }

    /// The computers: a heading with a way to add one, then a row each.
    fn computer_list(&mut self, ui: &mut egui::Ui, what: &Typed, entries: &[Entry], picked: Option<usize>, go: &mut Option<Choice>) {
        ui.horizontal(|ui| {
            let count = entries.iter().filter(|e| matches!(e, Entry::Computer(_))).count();
            kit::heading(ui, &format!("Computers  {count}"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(kit::ghost(format!("{}  Add a computer", ic::PLUS))).on_hover_text("By SSH or by its ID, with a name, a key, a password and tunnels").clicked() {
                    self.sheet = Some(Sheet::new(what));
                }
                let busy = self.lan.coming.is_some();
                if ui.add_enabled(!busy, kit::ghost(format!("{}  On this network", ic::SEARCH))).on_hover_text("Computers nearby with remote access on, found without typing their ID").clicked() {
                    self.look_on_network(ui.ctx());
                }
            });
        });
        self.network_card(ui);
        let searching = !matches!(what, Typed::Empty);
        let mut act: Option<(Entry, RowAct)> = None;
        egui::ScrollArea::vertical().id_salt("computers").auto_shrink([false, false]).show(ui, |ui| {
          kit::list(ui, |ui| {
            let line = ui.visuals().widgets.noninteractive.bg_stroke;
            for (i, e) in entries.iter().enumerate() {
                if i > 0 {
                    ui.painter().hline(ui.max_rect().x_range().shrink(14.0), ui.cursor().top(), line);
                }
                let a = match e {
                    Entry::Here => self.here_row(ui, picked == Some(i)),
                    Entry::Computer(c) => self.computer_row(ui, c, picked == Some(i)),
                };
                if let Some(a) = a {
                    act = Some((e.clone(), a));
                    if matches!(act, Some((_, RowAct::Pick))) {
                        self.picked = Some(i);
                    }
                }
            }
            let note = if self.computers.is_empty() && !searching {
                "The computers you open show here, with the aliases of ~/.ssh/config. Star the ones you use most to keep them first."
            } else if searching && entries.iter().all(|e| matches!(e, Entry::Here)) {
                "No computer in the list matches."
            } else {
                ""
            };
            if !note.is_empty() {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 12)).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(note).weak().size(12.5)).wrap());
                });
            }
          });
        });
        let Some((e, a)) = act else { return };
        let Entry::Computer(c) = e else {
            if let RowAct::Open(h) = a {
                *go = Some((String::new(), String::new(), h));
            }
            return;
        };
        let port = c.port.map(|p| p.to_string()).unwrap_or_default();
        match a {
            RowAct::Pick => {}
            RowAct::Open(how) => *go = Some((c.address.clone(), port, how)),
            RowAct::Star(on) => {
                crate::computers::update(&c.address, |k| k.favorite = on);
                self.computers_at = None;
            }
            RowAct::Edit => self.sheet = Some(Sheet::open(&c)),
            RowAct::Remove => {
                crate::tunnels::stop_others(&c.address, &[]);
                crate::computers::forget(&c.address);
                self.computers_at = None;
                self.picked = None;
            }
            RowAct::ForgetPassword => {
                if let Err(e) = crate::vault::delete(&c.address) {
                    self.error = Some(format!("The password was not forgotten: {e}"));
                }
            }
            RowAct::Wake => match crate::lan::wake(&c.mac) {
                Ok(()) => self.lan.said = format!("Wake-up sent to {}: it may take a minute to come up.", c.label()),
                Err(e) => self.error = Some(format!("{} was not woken: {e}", c.label())),
            },
        }
    }

    /// Asks this network who is there, in the background.
    fn look_on_network(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::lan::find(std::time::Duration::from_millis(1500)));
            ctx.request_repaint();
        });
        self.lan = Lan { coming: Some(rx), ..Default::default() };
    }

    /// What answered on this network, each with a way to add it; or that nothing did.
    fn network_card(&mut self, ui: &mut egui::Ui) {
        if let Some(found) = self.lan.coming.as_ref().and_then(|rx| rx.try_recv().ok()) {
            (self.lan.coming, self.lan.found, self.lan.looked) = (None, found, true);
        }
        if self.lan.coming.is_none() && !self.lan.looked && self.lan.said.is_empty() {
            return;
        }
        let mut close = false;
        let mut add = None;
        kit::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("On this network").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(egui::Button::new(RichText::new(ic::X).size(13.0)).frame_when_inactive(false)).on_hover_text("Close").clicked();
                });
            });
            if !self.lan.said.is_empty() {
                ui.add(egui::Label::new(RichText::new(&self.lan.said).color(super::super::brand())).wrap());
            }
            if self.lan.coming.is_some() {
                kit::loading(ui, "Asking the computers nearby\u{2026}");
                return;
            }
            if !self.lan.looked {
                return;
            }
            if self.lan.found.is_empty() {
                ui.add(egui::Label::new(RichText::new("No computer answered. Those with remote access on, and finding allowed in its settings, answer here; a firewall may keep the question out.").weak().size(12.5)).wrap());
                return;
            }
            for f in &self.lan.found {
                ui.horizontal(|ui| {
                    kit::mark(ui, ic::GLOBE, super::super::brand());
                    ui.vertical(|ui| {
                        ui.label(RichText::new(if f.name.is_empty() { "A computer" } else { &f.name }).strong());
                        ui.label(RichText::new(format!("ID {}  ·  {}", crate::computers::spaced(&f.id), f.ip)).weak().size(12.0));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let known = self.computers.iter().any(|c| c.id() == Some(f.id.as_str()));
                        if known {
                            ui.label(RichText::new("In the list").weak().size(12.5));
                        } else if ui.add(kit::secondary("Add")).clicked() {
                            add = Some(f.clone());
                        }
                    });
                });
            }
        });
        ui.add_space(kit::GAP);
        if let Some(f) = add {
            crate::computers::update(&f.address(), |c| {
                c.name = f.name.clone();
                c.mac = f.mac.clone();
            });
            self.computers_at = None;
            self.lan.said = format!("{} was added: open it with its password.", if f.name.is_empty() { "The computer" } else { &f.name });
        }
        if close {
            self.lan = Lan::default();
        }
    }

    /// Notes whose menu is open (`key`: a computer's address, empty for this computer).
    fn menu_open(&mut self, key: &str, open: bool) {
        if open {
            self.row_menu = Some(key.to_string());
        } else if self.row_menu.as_deref() == Some(key) {
            self.row_menu = None;
        }
    }

    /// A row: where it is drawn, its look when picked or under the mouse, and the
    /// space for its buttons on the right. Returns the row's response and its inner ui.
    fn row_frame(&mut self, ui: &mut egui::Ui, picked: bool, key: &str) -> (egui::Response, egui::Ui, bool) {
        let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), egui::Sense::click());
        // Its menu open keeps its buttons there, the pointer gone over to the menu.
        let picked = picked || self.row_menu.as_deref() == Some(key);
        // Brought into view when the arrow keys reach it, not while the wheel scrolls past it.
        if picked && std::mem::take(&mut self.picked_moved) {
            r.scroll_to_me(None);
        }
        let lit = picked || r.hovered() || r.contains_pointer();
        if picked {
            ui.painter().rect_filled(rect, CornerRadius::same(theme::R_SM), selected_fill());
        } else if lit {
            ui.painter().rect_filled(rect, CornerRadius::same(theme::R_SM), ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row.style_mut().interaction.selectable_labels = false;
        (r, row, lit)
    }

    /// This computer as the first row: its terminals, its screen and its files.
    fn here_row(&mut self, ui: &mut egui::Ui, picked: bool) -> Option<RowAct> {
        let mut act = None;
        let (r, mut row, lit) = self.row_frame(ui, picked, "");
        kit::mark(&mut row, ic::HOUSE, super::super::brand());
        let mut right = row.new_child(egui::UiBuilder::new().max_rect(row.max_rect()).layout(egui::Layout::right_to_left(egui::Align::Center)));
        right.spacing_mut().item_spacing.x = 2.0;
        let profiles = crate::termwin::local_profiles();
        if lit {
            if profiles.len() > 1 || cfg!(feature = "test-desktop") {
                let (_, open) = egui::containers::menu::MenuButton::from_button(tool_button(ic::DOTS, false)).ui(&mut right, |ui| {
                    crate::gui::menu_look(ui);
                    ui.set_min_width(200.0);
                    for p in &profiles {
                        if ui.button(format!("{}  {}", p.icon, p.name)).clicked() {
                            act = Some(RowAct::Open(How::Local(p.clone())));
                        }
                    }
                    if cfg!(feature = "test-desktop") && ui.button(format!("{}  Test desktop", ic::APP_WINDOW)).clicked() {
                        act = Some(RowAct::Open(How::TestDesktop));
                    }
                });
                self.menu_open("", open.is_some());
            }
            if PaneMode::Files.on() && icon(&mut right, ic::FOLDER, "This computer's files", false).clicked() {
                act = Some(RowAct::Open(How::This(PaneMode::Files)));
            }
            if PaneMode::Screen.on() && icon(&mut right, ic::MONITOR, "This computer's screen", false).clicked() {
                act = Some(RowAct::Open(How::This(PaneMode::Screen)));
            }
            if let Some(p) = profiles.first()
                && icon(&mut right, ic::TERMINAL, &format!("A terminal here ({})", p.name), false).clicked()
            {
                act = Some(RowAct::Open(How::Local(p.clone())));
            }
        }
        let room = (right.min_rect().left() - row.cursor().left() - 8.0).max(40.0);
        row.allocate_ui_with_layout(Vec2::new(room, 46.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.add_space(7.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(RichText::new("This computer").strong()).truncate());
            ui.add(egui::Label::new(RichText::new("Its own terminals, screen and files").small().weak()).truncate());
        });
        if act.is_none() {
            if r.double_clicked() {
                act = self.default_open(&Entry::Here).map(|c| RowAct::Open(c.2));
            } else if r.clicked() {
                act = Some(RowAct::Pick);
            }
        }
        act
    }

    /// One computer: its mark, its name, how it is reached and when it was last opened; under
    /// the mouse, what can be opened on it, the star and a menu.
    fn computer_row(&mut self, ui: &mut egui::Ui, c: &Computer, picked: bool) -> Option<RowAct> {
        let mut act = None;
        let (r, mut row, lit) = self.row_frame(ui, picked, &c.address);
        let by_id = c.id().is_some();
        let weak = row.visuals().weak_text_color();
        kit::mark(&mut row, if by_id { ic::GLOBE } else { ic::HARD_DRIVES }, weak);
        let mut right = row.new_child(egui::UiBuilder::new().max_rect(row.max_rect()).layout(egui::Layout::right_to_left(egui::Align::Center)));
        right.spacing_mut().item_spacing.x = 2.0;
        if lit {
            let (_, open) = egui::containers::menu::MenuButton::from_button(tool_button(ic::DOTS, false)).ui(&mut right, |ui| {
                crate::gui::menu_look(ui);
                ui.set_min_width(220.0);
                if ui.button(format!("{}  Edit\u{2026}", ic::PENCIL)).clicked() {
                    act = Some(RowAct::Edit);
                }
                if !by_id && ui.button(format!("{}  System terminal", ic::ARROW_SQUARE_OUT)).clicked() {
                    act = Some(RowAct::Open(How::System));
                }
                if ui.button(format!("{}  Copy address", ic::COPY)).clicked() {
                    ui.ctx().copy_text(c.id().map_or_else(|| c.address.clone(), str::to_string));
                }
                if crate::vault::has(&c.address) && ui.button(format!("{}  Forget the saved password", ic::LOCK_KEY)).clicked() {
                    act = Some(RowAct::ForgetPassword);
                }
                if !c.mac.is_empty() && ui.button(format!("{}  Wake it up", ic::LIGHTNING)).on_hover_text(format!("Wake-on-LAN to {}, on this network", c.mac)).clicked() {
                    act = Some(RowAct::Wake);
                }
                if !c.in_ssh_config && ui.button(format!("{}  Remove from the list", ic::TRASH)).clicked() {
                    act = Some(RowAct::Remove);
                }
            });
            self.menu_open(&c.address, open.is_some());
        }
        if lit || c.favorite {
            let mut star = RichText::new(ic::STAR).size(16.0);
            star = if c.favorite { star.family(filled()).color(warn()) } else { star.color(right.visuals().weak_text_color()) };
            let tip = if c.favorite { "Remove from favorites" } else { "Add to favorites" };
            if right.add(egui::Button::new(star).frame_when_inactive(false).min_size(Vec2::splat(28.0))).on_hover_text(tip).clicked() {
                act = Some(RowAct::Star(!c.favorite));
            }
        }
        if lit {
            if PaneMode::Files.on() && icon(&mut right, ic::FOLDER, "Files", false).clicked() {
                act = Some(RowAct::Open(How::Files));
            }
            if PaneMode::Screen.on() && icon(&mut right, ic::MONITOR, "Screen", false).clicked() {
                act = Some(RowAct::Open(How::Screen));
            }
            if !by_id && icon(&mut right, ic::TERMINAL, "Terminal", false).clicked() {
                act = Some(RowAct::Open(How::Terminal));
            }
        }
        let room = (right.min_rect().left() - row.cursor().left() - 8.0).max(40.0);
        row.allocate_ui_with_layout(Vec2::new(room, 46.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.add_space(7.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(RichText::new(c.label()).strong()).truncate());
            let mut sub: Vec<String> = Vec::new();
            if !c.name.is_empty() {
                sub.push(c.id().map_or_else(|| c.address.clone(), |id| format!("ID {}", crate::computers::spaced(id))));
            }
            sub.push(match (by_id, c.port) {
                (true, _) if !c.name.is_empty() => String::new(),
                (true, _) => "by ID".into(),
                (false, Some(p)) => format!("SSH, port {p}"),
                (false, None) if c.in_ssh_config => "SSH, ~/.ssh/config".into(),
                (false, None) => "SSH".into(),
            });
            sub.push(if c.last_used > 0 { crate::hosts::ago(c.last_used) } else { "never opened".into() });
            sub.retain(|s| !s.is_empty());
            let open = crate::tunnels::open_count(&c.address, &c.tunnels);
            if open > 0 {
                sub.push(format!("{open} tunnel{} open", if open == 1 { "" } else { "s" }));
            }
            ui.add(egui::Label::new(RichText::new(sub.join("  \u{00b7}  ")).small().weak()).truncate());
        });
        if act.is_none() {
            if r.double_clicked() {
                act = Some(RowAct::Open(how(self.default_mode(by_id))));
            } else if r.clicked() {
                act = Some(RowAct::Pick);
            }
        }
        r.on_hover_text("Double-click to open; the buttons open a terminal, the screen or the files");
        act
    }
}

fn how(m: PaneMode) -> How {
    match m {
        PaneMode::Terminal => How::Terminal,
        PaneMode::Screen => How::Screen,
        PaneMode::Files => How::Files,
    }
}

#[cfg(test)]
mod tests {
    use super::{Typed, typed};

    #[test]
    fn the_address_field_tells_ssh_from_ids_and_words() {
        let ssh = |h: &str, p: &str| Typed::Ssh { host: h.into(), port: p.into() };
        assert_eq!(typed(""), Typed::Empty);
        assert_eq!(typed("web1"), ssh("web1", ""));
        assert_eq!(typed(" dev@10.0.0.5:2222 "), ssh("dev@10.0.0.5", "2222"));
        assert_eq!(typed("dev@[fe80::1]:22"), ssh("dev@fe80::1", "22"));
        assert_eq!(typed("2001:db8::7"), ssh("2001:db8::7", ""));
        assert!(matches!(typed("123 456 789"), Typed::Id { ref id, .. } if id == "123456789"));
        assert!(matches!(typed("sshremote://123456789@relay.example"), Typed::Id { ref address, .. } if address == "sshremote://123456789@relay.example"));
        assert_eq!(typed("my office pc"), Typed::Words);
        assert_eq!(typed("-oProxyCommand=x"), Typed::Words);
    }
}
