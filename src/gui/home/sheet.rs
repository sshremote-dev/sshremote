//! A computer's sheet, over the home window: its name, how it is reached (an address, an alias of
//! ~/.ssh/config with its host, user, port and key, or a remote access ID), its saved password and
//! its tunnels. A new computer starts from what the address field holds.

use super::connect::Typed;
use super::*;
use crate::tools::HostEntry;
use crate::tunnels::{Kind, State, Tunnel};

/// A tunnel as its row in the sheet edits it.
struct TunnelForm {
    kind: Kind,
    port: String,
    dest: String,
    auto: bool,
}

impl TunnelForm {
    fn tunnel(&self) -> Result<Tunnel, String> {
        let port = self.port.trim().parse::<u16>().unwrap_or(0);
        let t = Tunnel { kind: self.kind, port, dest: self.dest.trim().to_string(), start_with_app: self.auto };
        t.problem().map_or(Ok(t), |p| Err(p.to_string()))
    }
}

pub(in crate::gui) struct Sheet {
    /// Its address when the sheet opened; empty for a new computer.
    was: String,
    by_id: bool,
    name: String,
    favorite: bool,
    /// SSH, kept in the list only: `user@host` and a port.
    address: String,
    port: String,
    /// SSH, kept in ~/.ssh/config as an alias.
    in_config: bool,
    entry: HostEntry,
    /// By ID: the ID and the relay it is at.
    id: String,
    relay: String,
    password: String,
    tunnels: Vec<TunnelForm>,
    problem: Option<String>,
    sure_to_remove: bool,
}

impl Sheet {
    /// A new computer, from what the address field holds.
    pub(in crate::gui) fn new(what: &Typed) -> Sheet {
        let mut s = Sheet {
            was: String::new(),
            by_id: false,
            name: String::new(),
            favorite: false,
            address: String::new(),
            port: String::new(),
            in_config: false,
            entry: HostEntry::default(),
            id: String::new(),
            relay: String::new(),
            password: String::new(),
            tunnels: Vec::new(),
            problem: None,
            sure_to_remove: false,
        };
        match what {
            Typed::Id { address, id } => {
                s.by_id = true;
                s.id = id.clone();
                s.relay = relay_of(address);
            }
            Typed::Ssh { host, port } => {
                s.address = host.clone();
                s.port = port.clone();
            }
            _ => {}
        }
        s
    }

    /// The sheet of a computer in the list.
    pub(in crate::gui) fn open(c: &crate::computers::Computer) -> Sheet {
        let mut s = Sheet::new(&Typed::Empty);
        s.was = c.address.clone();
        s.name = c.name.clone();
        s.favorite = c.favorite;
        if let Some(id) = c.id() {
            s.by_id = true;
            s.id = id.to_string();
            s.relay = relay_of(&c.address);
        } else if c.in_ssh_config {
            s.in_config = true;
            s.entry = crate::tools::config_entry(&c.address).unwrap_or_else(|| HostEntry { alias: c.address.clone(), ..Default::default() });
        } else {
            s.address = c.address.clone();
            s.port = c.port.map(|p| p.to_string()).unwrap_or_default();
        }
        s.tunnels = c.tunnels.iter().map(|t| TunnelForm { kind: t.kind, port: t.port.to_string(), dest: t.dest.clone(), auto: t.start_with_app }).collect();
        s
    }

    /// The address it is kept and reached at, as the sheet has it now.
    fn address_now(&self) -> String {
        if self.by_id {
            let relay = if self.relay.trim().is_empty() { crate::computers::relay() } else { self.relay.trim().to_string() };
            format!("sshremote://{}@{relay}", self.id.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        } else if self.in_config {
            self.entry.alias.trim().to_string()
        } else {
            self.address.trim().to_string()
        }
    }

    /// Keeps it all: the config block, the list's entry (moved when its address changed), the
    /// password and the tunnels. Returns why not, if not.
    fn save(&mut self) -> Result<String, String> {
        let address = self.address_now();
        // All of it is checked before any of it is written.
        if self.by_id {
            let id: String = self.id.chars().filter(|c| !c.is_whitespace()).collect();
            if !(6..=12).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
                return Err("An ID is 6 to 12 digits, as that computer shows it.".into());
            }
        } else if !self.in_config && !matches!(super::connect::typed(&address), Typed::Ssh { .. }) {
            return Err("The address is user@host, a host name or an IP, such as dev@192.168.1.20.".into());
        }
        let port = if self.by_id || self.in_config || self.port.trim().is_empty() {
            None
        } else {
            Some(self.port.trim().parse::<u16>().ok().filter(|p| *p > 0).ok_or("The port is a number from 1 to 65535.")?)
        };
        let tunnels = if self.by_id { Vec::new() } else { self.tunnels.iter().map(TunnelForm::tunnel).collect::<Result<Vec<_>, _>>()? };
        if self.in_config && !self.by_id {
            let was_alias = if crate::tools::config_entry(&self.was).is_some() { self.was.clone() } else { String::new() };
            crate::tools::save_config_entry(&was_alias, &self.entry)?;
        }
        if !self.was.is_empty() && self.was != address {
            crate::computers::rename(&self.was, &address);
            if let Some(pw) = crate::vault::get(&self.was) {
                let _ = crate::vault::set(&address, &pw).and_then(|_| crate::vault::delete(&self.was));
            }
            if crate::tools::config_entry(&self.was).is_some() && !self.in_config {
                crate::tools::remove_config_entry(&self.was)?;
            }
        }
        let (name, favorite) = (self.name.trim().to_string(), self.favorite);
        crate::computers::update(&address, |c| {
            c.name = name;
            c.favorite = favorite;
            c.port = port;
            c.tunnels = tunnels;
        });
        if !self.password.is_empty() {
            crate::vault::set(&address, &std::mem::take(&mut self.password))?;
        }
        Ok(address)
    }

    /// Takes it away: from the list, its password, its tunnels, and its block of ~/.ssh/config.
    fn remove(&self) -> Result<(), String> {
        if let Some(c) = crate::computers::get(&self.was) {
            for t in &c.tunnels {
                crate::tunnels::stop(&c.address, t);
            }
        }
        if crate::tools::config_entry(&self.was).is_some() {
            crate::tools::remove_config_entry(&self.was)?;
        }
        crate::computers::forget(&self.was);
        if crate::vault::get(&self.was).is_some() {
            crate::vault::delete(&self.was)?;
        }
        Ok(())
    }
}

/// The relay in an ID's address; empty for the one this computer uses.
fn relay_of(address: &str) -> String {
    address.strip_prefix("sshremote://").and_then(|r| r.split_once('@')).map(|(_, r)| r.trim_end_matches('/').to_string()).unwrap_or_default()
}

impl App {
    /// The sheet of the computer being added or changed, while one is.
    pub(super) fn computer_sheet(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.sheet.take() else { return };
        let new = s.was.is_empty();
        let mut close = false;
        let mut saved = false;
        let (modal, fit) = super::super::modal(ctx, "computer-sheet", Vec2::new(600.0, 0.0));
        let r = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            let title = if new { "Add a computer".to_string() } else { s.name.clone().chars().take(40).collect::<String>() };
            let title = if title.is_empty() { s.was.clone() } else { title };
            ui.label(RichText::new(title).size(17.0).strong());
            ui.add_space(10.0);
            if new {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Reached").weak());
                    if ui.add(choice(format!("{}  By SSH", ic::TERMINAL), !s.by_id)).clicked() {
                        s.by_id = false;
                    }
                    if ui.add(choice(format!("{}  By its ID", ic::GLOBE), s.by_id)).clicked() {
                        s.by_id = true;
                    }
                });
                ui.add_space(8.0);
            }
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.62).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("sheet-grid").num_columns(2).spacing([14.0, 8.0]).min_col_width(96.0).show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut s.name).hint_text("as the list shows it (optional)").desired_width(300.0));
                    ui.end_row();
                    if s.by_id {
                        ui.label("ID");
                        ui.add(egui::TextEdit::singleline(&mut s.id).hint_text("123 456 789").desired_width(160.0));
                        ui.end_row();
                        ui.label("Relay").on_hover_text("Empty: the relay this computer uses");
                        ui.add(egui::TextEdit::singleline(&mut s.relay).hint_text(crate::computers::relay()).desired_width(220.0));
                        ui.end_row();
                    } else if s.in_config {
                        ui.label("Alias").on_hover_text("The name ssh and other programs know it by: ssh ALIAS");
                        ui.add(egui::TextEdit::singleline(&mut s.entry.alias).hint_text("web1").desired_width(160.0));
                        ui.end_row();
                        ui.label("Host");
                        ui.add(egui::TextEdit::singleline(&mut s.entry.hostname).hint_text("192.168.1.20 or web1.example.com").desired_width(300.0));
                        ui.end_row();
                        ui.label("User");
                        ui.add(egui::TextEdit::singleline(&mut s.entry.user).hint_text("your name there").desired_width(160.0));
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::TextEdit::singleline(&mut s.entry.port).hint_text("22").desired_width(70.0));
                        ui.end_row();
                        ui.label("Key file");
                        ui.add(egui::TextEdit::singleline(&mut s.entry.identity).hint_text("~/.ssh/id_ed25519 (optional)").desired_width(300.0));
                        ui.end_row();
                    } else {
                        ui.label("Address");
                        ui.add(egui::TextEdit::singleline(&mut s.address).hint_text("user@host").desired_width(300.0));
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::TextEdit::singleline(&mut s.port).hint_text("22").desired_width(70.0));
                        ui.end_row();
                    }
                    if !s.by_id && (new || !s.in_config) {
                        ui.label("");
                        let r = ui.checkbox(&mut s.in_config, "Keep it in ~/.ssh/config").on_hover_text("As an alias, with its user, port and key: ssh and other programs know it too");
                        if r.changed() && s.in_config {
                            // The address typed becomes the alias's host and user.
                            let (user, host) = s.address.trim().rsplit_once('@').map_or((String::new(), s.address.trim().to_string()), |(u, h)| (u.to_string(), h.to_string()));
                            if s.entry.hostname.is_empty() {
                                s.entry = HostEntry { alias: host.split('.').next().unwrap_or("").to_string(), hostname: host, user, port: s.port.clone(), identity: String::new() };
                            }
                        } else if r.changed() && s.address.is_empty() {
                            let user = if s.entry.user.is_empty() { String::new() } else { format!("{}@", s.entry.user) };
                            s.address = format!("{user}{}", s.entry.hostname);
                            s.port = s.entry.port.clone();
                        }
                        ui.end_row();
                    }
                    ui.label("");
                    ui.checkbox(&mut s.favorite, "Favorite: first in the list");
                    ui.end_row();
                    ui.label("Password");
                    ui.horizontal(|ui| {
                        let saved_pw = !new && crate::vault::get(&s.was).is_some();
                        let hint = if saved_pw { "saved; type to replace it" } else if s.by_id { "set on that computer" } else { "none: keys sign in" };
                        ui.add_enabled(crate::vault::backend().is_some(), egui::TextEdit::singleline(&mut s.password).password(true).hint_text(hint).desired_width(220.0));
                        if saved_pw
                            && ui.button("Forget").clicked()
                            && let Err(e) = crate::vault::delete(&s.was)
                        {
                            s.problem = Some(e);
                        }
                    });
                    ui.end_row();
                });
                if !s.by_id {
                    ui.add_space(12.0);
                    self.tunnel_rows(ui, &mut s);
                }
            });
            if let Some(p) = &s.problem {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(p).color(bad())).wrap());
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if !new {
                    if s.sure_to_remove {
                        let what = if s.in_config { "Remove it, its password, its tunnels and its block of ~/.ssh/config" } else { "Remove it, its password and its tunnels" };
                        if ui.add(egui::Button::new(RichText::new(what).color(Color32::WHITE)).fill(bad())).clicked() {
                            match s.remove() {
                                Ok(()) => close = true,
                                Err(e) => s.problem = Some(e),
                            }
                            saved = true;
                        }
                        if ui.button("Keep it").clicked() {
                            s.sure_to_remove = false;
                        }
                    } else if ui.button(format!("{}  Remove", ic::TRASH)).clicked() {
                        s.sure_to_remove = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(theme::primary(if new { "Add" } else { "Save" })).clicked() {
                        match s.save() {
                            Ok(_) => {
                                close = true;
                                saved = true;
                            }
                            Err(e) => s.problem = Some(e),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        });
        if saved {
            self.computers_at = None;
        }
        if !(close || r.should_close()) {
            self.sheet = Some(s);
        }
    }

    /// The tunnels of the computer: each one's way, ports and state, with start and stop; then a
    /// way to add one.
    fn tunnel_rows(&mut self, ui: &mut egui::Ui, s: &mut Sheet) {
        heading(ui, "Tunnels");
        ui.add(egui::Label::new(RichText::new("Ports forwarded through this computer while SSH Remote runs. They sign in with your SSH keys.").weak().size(12.0)).wrap());
        ui.add_space(4.0);
        let address = s.address_now();
        let port = s.port.trim().parse().ok();
        let mut gone = None;
        for (i, t) in s.tunnels.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.push_id(i, |ui| {
                    combo("kind", t.kind.name()).width(120.0).show_ui(ui, |ui| {
                        crate::gui::menu_look(ui);
                        for k in Kind::ALL {
                            ui.selectable_value(&mut t.kind, k, k.name());
                        }
                    });
                    let port_hint = if t.kind == Kind::Remote { "port there" } else { "port here" };
                    ui.add(egui::TextEdit::singleline(&mut t.port).hint_text(port_hint).desired_width(70.0));
                    if t.kind != Kind::Socks {
                        ui.label("\u{2192}");
                        ui.add(egui::TextEdit::singleline(&mut t.dest).hint_text("localhost:5432").desired_width(140.0));
                    }
                    ui.checkbox(&mut t.auto, "At start").on_hover_text("Open it when SSH Remote starts");
                    let state = t.tunnel().ok().map(|k| (crate::tunnels::state(&address, &k), k));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon(ui, ic::TRASH, "Remove this tunnel", false).clicked() {
                            gone = Some(i);
                        }
                        match &state {
                            Some((State::On | State::Starting, k)) => {
                                if ui.button("Stop").clicked() {
                                    crate::tunnels::stop(&address, k);
                                }
                            }
                            Some((_, k)) => {
                                if ui.add_enabled(!s.was.is_empty() || !address.is_empty(), egui::Button::new("Start")).clicked()
                                    && let Err(e) = crate::tunnels::start(&address, port, k)
                                {
                                    s.problem = Some(e);
                                }
                            }
                            None => {}
                        }
                        let (color, text) = match &state {
                            Some((State::On, _)) => (good(), "open".to_string()),
                            Some((State::Starting, _)) => (warn(), "opening".to_string()),
                            Some((State::Failed(why), _)) => (bad(), why.clone()),
                            _ => (ui.visuals().weak_text_color(), String::new()),
                        };
                        if !text.is_empty() {
                            ui.add(egui::Label::new(RichText::new(text).color(color).size(12.0)).truncate());
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                        }
                    });
                });
            });
        }
        if let Some(i) = gone {
            let t = s.tunnels.remove(i);
            if let Ok(k) = t.tunnel() {
                crate::tunnels::stop(&address, &k);
            }
        }
        if ui.add(egui::Button::new(RichText::new(format!("{}  Add a tunnel", ic::PLUS)).size(12.5)).frame_when_inactive(false)).clicked() {
            s.tunnels.push(TunnelForm { kind: Kind::Local, port: String::new(), dest: "localhost:".into(), auto: false });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sheet with something wrong in it says what, and keeps nothing: no block is written
    /// to ~/.ssh/config for a computer whose tunnel cannot be made.
    #[test]
    fn a_sheet_keeps_nothing_until_all_of_it_is_right() {
        let mut s = Sheet::new(&Typed::Ssh { host: "dev@10.9.8.7".into(), port: "70000".into() });
        assert!(s.save().unwrap_err().contains("port"));
        s.port.clear();
        s.in_config = true;
        s.entry = HostEntry { alias: "sheet-test-alias".into(), hostname: "10.9.8.7".into(), user: "dev".into(), ..Default::default() };
        s.tunnels.push(TunnelForm { kind: Kind::Local, port: "8080".into(), dest: "-oProxyCommand=x:1".into(), auto: false });
        assert!(s.save().unwrap_err().contains("HOST:PORT"));
        assert!(crate::tools::config_entry("sheet-test-alias").is_none(), "nothing written");
        let mut id = Sheet::new(&Typed::Id { address: "sshremote://12@relay.example".into(), id: "12".into() });
        assert!(id.save().unwrap_err().contains("6 to 12 digits"));
        assert_eq!(relay_of("sshremote://123456789@relay.example:7070/"), "relay.example:7070");
    }
}
