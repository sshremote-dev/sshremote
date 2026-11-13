//! A computer's sheet, over the home window: its name, how it is reached (an address, an alias of
//! ~/.ssh/config with its host, user, port and key, or a remote access ID), its saved password and
//! its tunnels. A new computer starts from what the address field holds.

use super::connect::Typed;
use super::*;
use crate::gui::kit;
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
    /// It was in ~/.ssh/config when the sheet opened, with this block.
    was_in_config: bool,
    entry_was: HostEntry,
    /// Its block can be changed here: one of its own in ~/.ssh/config itself (not in a file it
    /// includes, nor one named with other computers).
    editable: bool,
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
            was_in_config: false,
            entry_was: HostEntry::default(),
            editable: true,
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
                // The relay this computer uses unless another was typed with the ID.
                let typed = relay_of(address);
                s.relay = if typed == crate::computers::relay() { String::new() } else { typed };
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
            (s.in_config, s.was_in_config) = (true, true);
            let found = crate::tools::config_entry(&c.address);
            s.editable = found.is_some();
            s.entry = found.unwrap_or_else(|| HostEntry { alias: c.address.clone(), ..Default::default() });
            s.entry_was = s.entry.clone();
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
        // A port typed after the address goes in its own field.
        if !self.by_id
            && !self.in_config
            && let Typed::Ssh { host, port } = super::connect::typed(&self.address)
            && !port.is_empty()
        {
            if !self.port.trim().is_empty() && self.port.trim() != port {
                return Err("The port is given twice, after the address and as Port.".into());
            }
            (self.address, self.port) = (host, port);
        }
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
        let tunnels = self.tunnels.iter().map(TunnelForm::tunnel).collect::<Result<Vec<_>, _>>()?;
        if self.by_id && tunnels.iter().any(|t| t.kind != Kind::Local) {
            return Err("Through a computer reached by its ID, a tunnel goes from here to there only.".into());
        }
        // Another computer's address, or another block of ~/.ssh/config, is never taken over.
        let in_file = self.in_config && !self.by_id;
        if address != self.was && (crate::computers::get(&address).is_some() || (in_file && crate::tools::config_entry(&address).is_some())) {
            return Err(format!("{address} is a computer of its own already: change that one instead."));
        }
        if in_file && self.editable && self.entry != self.entry_was {
            let was_alias = if self.was_in_config { self.was.clone() } else { String::new() };
            crate::tools::save_config_entry(&was_alias, &self.entry)?;
        }
        // Tunnels taken out, or of an address it no longer has, close.
        crate::tunnels::stop_others(&self.was, if self.was == address { &tunnels } else { &[] });
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
        if self.editable && crate::tools::config_entry(&self.was).is_some() {
            crate::tools::remove_config_entry(&self.was)?;
        }
        crate::computers::forget(&self.was);
        if crate::vault::has(&self.was) {
            crate::vault::delete(&self.was)?;
        }
        Ok(())
    }
}

impl Sheet {
    /// Closed without saving: tunnels started from it that its computer does not keep close.
    fn drop_unsaved(&self) {
        let address = self.address_now();
        let kept = crate::computers::get(&address).map(|c| c.tunnels).unwrap_or_default();
        crate::tunnels::stop_others(&address, &kept);
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
        let (modal, fit) = super::super::modal(ctx, "computer-sheet", Vec2::new(640.0, 0.0));
        let r = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            let title = if new { "Add a computer".to_string() } else if s.name.trim().is_empty() { s.was.clone() } else { s.name.trim().chars().take(48).collect() };
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(title).size(18.0).strong()).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut star = RichText::new(ic::STAR).size(18.0);
                    star = if s.favorite { star.family(filled()).color(warn()) } else { star.color(ui.visuals().weak_text_color()) };
                    let tip = if s.favorite { "A favorite: first in the list" } else { "Make it a favorite: first in the list" };
                    if ui.add(egui::Button::new(star).frame_when_inactive(false).min_size(Vec2::splat(30.0))).on_hover_text(tip).clicked() {
                        s.favorite = !s.favorite;
                    }
                });
            });
            ui.add_space(10.0);
            if new {
                let mut way = usize::from(s.by_id);
                if kit::segmented(ui, &mut way, &[&format!("{}  By SSH", ic::TERMINAL), &format!("{}  By its remote access ID", ic::GLOBE)]) {
                    s.by_id = way == 1;
                }
                ui.add_space(10.0);
            }
            // All the room the window leaves under the title and above the buttons.
            let room = (ctx.content_rect().height() - chrome::HEIGHT - if new { 260.0 } else { 220.0 }).max(160.0);
            egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, |ui| {
                kit::card(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    kit::row(ui, "Name", |ui| ui.add(kit::field(&mut s.name, "as the list shows it (optional)")));
                    if s.by_id {
                        kit::row(ui, "ID", |ui| ui.add(kit::field(&mut s.id, "123 456 789")));
                        // Reached through the relay this computer uses, set in remote access's
                        // settings; one added through another keeps it until told otherwise.
                        let mine = crate::computers::relay();
                        let other = !s.relay.trim().is_empty() && s.relay.trim() != mine;
                        kit::row(ui, "", |ui| {
                            let through = if other { s.relay.trim().to_string() } else { mine.clone() };
                            ui.label(RichText::new(format!("{}  Through {through}", ic::GLOBE)).weak().size(12.5));
                            if other && ui.add(kit::ghost(format!("Use {mine}"))).on_hover_text("The relay this computer uses").clicked() {
                                s.relay.clear();
                            }
                        });
                    } else if s.in_config {
                        let on = s.editable;
                        kit::row(ui, "Alias", |ui| ui.add_enabled(on, kit::field(&mut s.entry.alias, "web1"))).on_hover_text("The name ssh and other programs know it by: ssh ALIAS");
                        if !on {
                            kit::notice(ui, kit::Tone::Info, "", "Its block of ~/.ssh/config is in a file it includes, or names other computers too: change it in the file.");
                        } else {
                            kit::row(ui, "Host", |ui| {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.add(kit::field(&mut s.entry.port, "22").desired_width(64.0));
                                    ui.label(RichText::new("Port").weak());
                                    ui.add(kit::field(&mut s.entry.hostname, "192.168.1.20 or web1.example.com"));
                                });
                            });
                            kit::row(ui, "User", |ui| ui.add(kit::field(&mut s.entry.user, "your name there")));
                            kit::row(ui, "Key file", |ui| ui.add(kit::field(&mut s.entry.identity, "~/.ssh/id_ed25519 (optional)")));
                        }
                    } else {
                        kit::row(ui, "Address", |ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(kit::field(&mut s.port, "22").desired_width(64.0));
                                ui.label(RichText::new("Port").weak());
                                ui.add(kit::field(&mut s.address, "user@host"));
                            });
                        });
                    }
                    if !s.by_id && !s.was_in_config {
                        let r = kit::row(ui, "", |ui| {
                            ui.checkbox(&mut s.in_config, "Keep it in ~/.ssh/config").on_hover_text("As an alias, with its user, port and key: ssh and other programs know it too")
                        });
                        if r.changed() && s.in_config {
                            // The address typed becomes the alias's host and user.
                            let (user, host) = s.address.trim().rsplit_once('@').map_or((String::new(), s.address.trim().to_string()), |(u, h)| (u.to_string(), h.to_string()));
                            if s.entry.hostname.is_empty() {
                                // A name's first part is a fair alias; an IP's is not.
                                let alias = if host.parse::<std::net::IpAddr>().is_ok() { String::new() } else { host.split('.').next().unwrap_or("").to_string() };
                                s.entry = HostEntry { alias, hostname: host, user, port: s.port.clone(), identity: String::new() };
                            }
                        } else if r.changed() && s.address.is_empty() {
                            let user = if s.entry.user.is_empty() { String::new() } else { format!("{}@", s.entry.user) };
                            s.address = format!("{user}{}", s.entry.hostname);
                            s.port = s.entry.port.clone();
                        }
                    }
                    let saved_pw = !new && crate::vault::has(&s.was);
                    kit::row(ui, "Password", |ui| {
                        let hint = if saved_pw { "saved: type to replace it" } else if s.by_id { "the one set on that computer" } else { "none: your SSH keys sign in" };
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if saved_pw
                                && ui.add(kit::secondary("Forget")).on_hover_text("Take the saved password out of the password store").clicked()
                                && let Err(e) = crate::vault::delete(&s.was)
                            {
                                s.problem = Some(e);
                            }
                            ui.add_enabled(crate::vault::backend().is_some(), kit::field(&mut s.password, hint).password(true))
                                .on_disabled_hover_text("This system has no password store sshremote can use");
                        });
                    });
                });
                ui.add_space(kit::GAP);
                self.tunnel_rows(ui, &mut s);
            });
            if let Some(p) = s.problem.clone() {
                ui.add_space(10.0);
                kit::notice(ui, kit::Tone::Bad, &p, "");
            }
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if !new {
                    if s.sure_to_remove {
                        let what = match (s.was_in_config, s.editable) {
                            (true, true) => "Remove it and its ~/.ssh/config block",
                            (true, false) => "Forget its name, password and tunnels",
                            _ => "Remove it, its password and tunnels",
                        };
                        if ui.add(kit::danger(what, true)).clicked() {
                            match s.remove() {
                                Ok(()) => close = true,
                                Err(e) => s.problem = Some(e),
                            }
                            saved = true;
                        }
                        if ui.add(kit::secondary("Keep it")).clicked() {
                            s.sure_to_remove = false;
                        }
                    } else if ui.add(kit::danger(format!("{}  Remove", ic::TRASH), false)).clicked() {
                        s.sure_to_remove = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(theme::primary(if new { "Add" } else { "Save" }).min_size(Vec2::new(88.0, kit::CONTROL_H))).clicked() {
                        match s.save() {
                            Ok(_) => {
                                close = true;
                                saved = true;
                            }
                            Err(e) => s.problem = Some(e),
                        }
                    }
                    if ui.add(kit::secondary("Cancel").min_size(Vec2::new(88.0, kit::CONTROL_H))).clicked() {
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
        } else if !saved {
            s.drop_unsaved();
        }
    }

    /// The tunnels of the computer: each one's way, ports and state, with start and stop; then a
    /// way to add one.
    fn tunnel_rows(&mut self, ui: &mut egui::Ui, s: &mut Sheet) {
        ui.horizontal(|ui| {
            kit::heading(ui, "Tunnels");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(kit::ghost(format!("{}  Add a tunnel", ic::PLUS))).clicked() {
                    s.tunnels.push(TunnelForm { kind: Kind::Local, port: String::new(), dest: "localhost:".into(), auto: false });
                }
            });
        });
        let address = s.address_now();
        let port = s.port.trim().parse().ok();
        let by_id = s.by_id;
        let mut gone = None;
        kit::card(ui, |ui| {
            if s.tunnels.is_empty() {
                let about = if s.by_id {
                    "Ports forwarded through this computer while SSH Remote runs, with its saved password: a port here that leads somewhere it reaches."
                } else {
                    "Ports forwarded through this computer while SSH Remote runs, signing in with your SSH keys: a port here that leads somewhere it reaches, one there that leads here, or a SOCKS proxy."
                };
                ui.add(egui::Label::new(RichText::new(about).weak().size(12.5)).wrap());
                return;
            }
            for (i, t) in s.tunnels.iter_mut().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                ui.push_id(i, |ui| {
                    ui.horizontal(|ui| {
                        combo("kind", t.kind.name()).width(130.0).show_ui(ui, |ui| {
                            crate::gui::menu_look(ui);
                            // Through a computer by its ID only a port here leads there.
                            for k in Kind::ALL.into_iter().filter(|k| !by_id || *k == Kind::Local) {
                                ui.selectable_value(&mut t.kind, k, k.name());
                            }
                        });
                        let port_hint = if t.kind == Kind::Remote { "port there" } else { "port here" };
                        ui.add(kit::field(&mut t.port, port_hint).desired_width(84.0));
                        if t.kind != Kind::Socks {
                            ui.label(RichText::new(ic::ARROW_RIGHT).weak());
                            ui.add(kit::field(&mut t.dest, "localhost:5432"));
                        }
                    });
                    let state = t.tunnel().ok().map(|k| (crate::tunnels::state(&address, &k), k));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut t.auto, "Open when SSH Remote starts");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon(ui, ic::TRASH, "Remove this tunnel", false).clicked() {
                                gone = Some(i);
                            }
                            match &state {
                                Some((State::On | State::Starting, k)) => {
                                    if ui.add(kit::secondary("Stop")).clicked() {
                                        crate::tunnels::stop(&address, k);
                                    }
                                }
                                Some((_, k)) => {
                                    if ui.add_enabled(!address.is_empty(), kit::secondary("Start")).clicked()
                                        && let Err(e) = crate::tunnels::start(&address, port, k)
                                    {
                                        s.problem = Some(e);
                                    }
                                }
                                None => {}
                            }
                            match &state {
                                Some((State::On, _)) => {
                                    ui.label(RichText::new("Open").color(good()).size(12.5));
                                    kit::dot(ui, good());
                                }
                                Some((State::Starting, _)) => {
                                    ui.label(RichText::new("Opening\u{2026}").color(warn()).size(12.5));
                                    ui.add(egui::Spinner::new().size(12.0).color(warn()));
                                }
                                Some((State::Failed(why), _)) => {
                                    ui.add(egui::Label::new(RichText::new(why).color(bad()).size(12.5)).truncate()).on_hover_text(why);
                                }
                                Some((State::Off, _)) => {
                                    ui.label(RichText::new("Closed").weak().size(12.5));
                                }
                                None => {
                                    ui.label(RichText::new("Fill in the ports").weak().size(12.5));
                                }
                            }
                            if matches!(state, Some((State::Starting | State::On, _))) {
                                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                            }
                        });
                    });
                });
            }
        });
        if let Some(i) = gone {
            let t = s.tunnels.remove(i);
            if let Ok(k) = t.tunnel() {
                crate::tunnels::stop(&address, &k);
            }
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
