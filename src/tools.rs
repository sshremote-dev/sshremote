//! SSH helpers on the home screen: key generation and installation, the host entries of
//! ~/.ssh/config, saved passwords, and backups.

use std::path::PathBuf;
use std::process::Command;

use eframe::egui::{self, RichText};

use crate::gui::{bad, choice, combo, good, ic, icon, primary};

use crate::sys::{quiet, ssh_dir};

/// A key pair in ~/.ssh: file stem and the public key line.
pub struct KeyPair {
    pub name: String,
    pub public: String,
}

pub fn list_keys() -> Vec<KeyPair> {
    let mut out: Vec<KeyPair> = std::fs::read_dir(ssh_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().is_some_and(|x| x == "pub"))
                .then(|| KeyPair {
                    name: p.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                    public: std::fs::read_to_string(&p).unwrap_or_default().trim().to_string(),
                })
                .filter(|k| !k.public.is_empty())
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// What a key tool asks the home screen to do.
pub enum ToolAction {
    /// Run this in a terminal tab (password prompts appear there).
    Terminal { title: String, argv: Vec<String> },
}

pub struct KeyTool {
    name: String,
    kind: usize,
    comment: String,
    /// Protect the key with a passphrase, typed into ssh-keygen in a terminal tab.
    protect: bool,
    status: Option<(bool, String)>,
    keys: Vec<KeyPair>,
    host: String,
    /// The dialog open: creating a key, or installing the key at this index on a host.
    creating: bool,
    installing: Option<usize>,
}

const KINDS: [(&str, &[&str]); 3] = [
    ("Ed25519 (recommended)", &["-t", "ed25519"]),
    ("RSA 4096", &["-t", "rsa", "-b", "4096"]),
    ("ECDSA 521", &["-t", "ecdsa", "-b", "521"]),
];

impl KeyTool {
    pub fn new() -> Self {
        let user = std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default();
        let host = std::env::var(if cfg!(windows) { "COMPUTERNAME" } else { "HOSTNAME" }).unwrap_or_default();
        let keys = list_keys();
        let name = if keys.iter().any(|k| k.name == "id_ed25519") { String::new() } else { "id_ed25519".into() };
        KeyTool {
            name,
            kind: 0,
            comment: format!("{user}@{host}").to_lowercase(),
            protect: false,
            status: None,
            keys,
            host: String::new(),
            creating: false,
            installing: None,
        }
    }

    fn generate(&mut self, actions: &mut Vec<ToolAction>) {
        let name = self.name.trim();
        if name.is_empty() || name.contains(['/', '\\']) {
            self.status = Some((false, "Give the key a file name, such as id_ed25519_work.".into()));
            return;
        }
        let dir = ssh_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        if path.exists() {
            self.status = Some((false, format!("{} exists already; choose another name.", path.display())));
            return;
        }
        let mut argv: Vec<String> = ["ssh-keygen"].iter().chain(KINDS[self.kind].1).map(|s| s.to_string()).collect();
        argv.extend(["-C".into(), self.comment.clone(), "-f".into(), path.to_string_lossy().into_owned()]);
        // A passphrase given with -N shows in the process list: ssh-keygen asks for it in a terminal instead.
        if self.protect {
            actions.push(ToolAction::Terminal { title: format!("Create key {name}"), argv });
            self.status = Some((true, format!("ssh-keygen asks for the passphrase of {} in the terminal.", path.display())));
            return;
        }
        let out = quiet(Command::new(&argv[0]).args(&argv[1..]).arg("-N").arg("")).output();
        self.status = Some(match out {
            Ok(o) if o.status.success() => (true, format!("Created {} and {}.pub", path.display(), path.display())),
            Ok(o) => (false, String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => (false, format!("ssh-keygen: {e} (is OpenSSH installed?)")),
        });
        self.keys = list_keys();
    }

    /// The page, in the open view's tools: the keys, or the form creating one or installing one
    /// on a host.
    pub fn ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<ToolAction>) {
        if self.creating || self.installing.is_some() {
            return self.forms(ui, actions);
        }
        ui.label(RichText::new("Your key pairs in ~/.ssh. Copy a public key, or install it on a host so ssh signs in without a password.").weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add(primary(format!("{}  Create a key", ic::PLUS))).clicked() {
                self.creating = true;
                self.status = None;
            }
            if ui.button(format!("{}  Open the .ssh folder", ic::FOLDER_OPEN)).clicked() {
                open_path(&ssh_dir());
            }
        });
        ui.add_space(8.0);
        if self.keys.is_empty() {
            ui.label(RichText::new("No keys yet.").weak());
        }
        for (i, k) in self.keys.iter().enumerate() {
            egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(crate::gui::theme::R_MD).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}  {}", ic::KEY, k.name)).strong());
                    let kind = k.public.split_whitespace().next().unwrap_or("").trim_start_matches("ssh-");
                    ui.label(RichText::new(kind).weak().small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon(ui, ic::UPLOAD, "Install on a host: adds the public key to its ~/.ssh/authorized_keys", false).clicked() {
                            self.installing = Some(i);
                        }
                        if icon(ui, ic::COPY, "Copy the public key", false).clicked() {
                            ui.ctx().copy_text(k.public.clone());
                            self.status = Some((true, format!("Copied the public key of {}", k.name)));
                        }
                    });
                });
                let comment = k.public.split_whitespace().nth(2).unwrap_or("");
                ui.label(RichText::new(comment).weak().small());
            });
            ui.add_space(4.0);
        }
        status(ui, &self.status);
    }

    /// What the back arrow leads back from, if a form is open.
    pub fn sub_page(&self) -> bool {
        self.creating || self.installing.is_some()
    }

    pub fn back(&mut self) {
        (self.creating, self.installing) = (false, None);
        if self.status.as_ref().is_some_and(|s| !s.0) {
            self.status = None;
        }
    }

    fn forms(&mut self, ui: &mut egui::Ui, actions: &mut Vec<ToolAction>) {
        if self.creating {
            let ready = !self.name.trim().is_empty();
            let answer = form_page(ui, "Create a key", "Create key", ready, &self.status, |ui| {
                egui::Grid::new("keygen").striped(false).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Type");
                    combo("keytype", KINDS[self.kind].0).width(FIELD_W).show_ui(ui, |ui| {
                        crate::gui::menu_look(ui);
                        for (i, k) in KINDS.iter().enumerate() {
                            ui.selectable_value(&mut self.kind, i, k.0);
                        }
                    });
                    ui.end_row();
                    ui.label("File name");
                    ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("id_ed25519_work").desired_width(FIELD_W));
                    ui.end_row();
                    ui.label("Comment");
                    ui.add(egui::TextEdit::singleline(&mut self.comment).desired_width(FIELD_W));
                    ui.end_row();
                    ui.label("Passphrase");
                    ui.checkbox(&mut self.protect, "Ask for one in a terminal");
                    ui.end_row();
                });
            });
            match answer {
                Some(true) => {
                    self.generate(actions);
                    if self.status.as_ref().is_some_and(|s| s.0) {
                        self.creating = false;
                        // The next key needs a name of its own.
                        self.name.clear();
                    }
                }
                Some(false) => self.back(),
                None => {}
            }
        }
        if let Some(i) = self.installing {
            let name = self.keys.get(i).map_or(String::new(), |k| k.name.clone());
            let ready = !self.host.trim().is_empty();
            let answer = form_page(ui, &format!("Install {name} on a host"), "Install", ready, &None, |ui| {
                ui.label(RichText::new("A terminal opens to sign in once with the password; after that ssh uses the key.").weak());
                ui.add_space(6.0);
                ui.add(egui::TextEdit::singleline(&mut self.host).hint_text("user@host or alias").desired_width(FIELD_W + 80.0));
            });
            match answer {
                Some(true) => {
                    if let Some(k) = self.keys.get(i) {
                        let host = self.host.trim().to_string();
                        let key = k.public.replace('\'', "");
                        let script = format!(
                            "umask 077; mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && (grep -qxF '{key}' ~/.ssh/authorized_keys || echo '{key}' >> ~/.ssh/authorized_keys) && echo 'Key {} installed on' $(hostname)",
                            k.name
                        );
                        actions.push(ToolAction::Terminal { title: format!("Install key on {host}"), argv: vec!["ssh".into(), host, script] });
                    }
                    self.installing = None;
                }
                Some(false) => self.back(),
                None => {}
            }
        }
    }
}

/// One `Host` block of ~/.ssh/config.
#[derive(Default, Clone)]
pub struct HostEntry {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
    pub identity: String,
}

fn parse_config(text: &str) -> Vec<HostEntry> {
    let mut out: Vec<HostEntry> = Vec::new();
    // The block the lines belong to: a Host block, or none under a Match.
    let mut current: Option<usize> = None;
    for line in text.lines() {
        let l = line.trim();
        let Some((k, v)) = l.split_once(|c: char| c.is_whitespace() || c == '=') else { continue };
        let v = v.trim().trim_start_matches('=').trim().to_string();
        let key = k.to_lowercase();
        match key.as_str() {
            "host" => {
                // Several names on one line stay together, as the line has them.
                out.push(HostEntry { alias: v.split_whitespace().collect::<Vec<_>>().join(" "), ..Default::default() });
                current = Some(out.len() - 1);
                continue;
            }
            "match" => {
                current = None;
                continue;
            }
            _ => {}
        }
        let Some(h) = current.map(|i| &mut out[i]) else { continue };
        match key.as_str() {
            "hostname" => h.hostname = v,
            "user" => h.user = v,
            "port" => h.port = v,
            // ssh tries them in order; the form shows the first.
            "identityfile" if h.identity.is_empty() => h.identity = v,
            _ => {}
        }
    }
    out.retain(|h| !h.alias.contains(['*', '?']));
    out
}

/// The keys of a host the form edits; other lines of its block are left alone.
const MANAGED: [&str; 4] = ["hostname", "user", "port", "identityfile"];

fn key_of(line: &str) -> String {
    line.trim().split(|c: char| c.is_whitespace() || c == '=').next().unwrap_or("").to_lowercase()
}

/// The lines of the `Host` block named exactly `alias`: from its Host line to before the next
/// Host or Match line.
fn block(lines: &[&str], alias: &str) -> Option<(usize, usize)> {
    let want: Vec<&str> = alias.split_whitespace().collect();
    let start = lines.iter().position(|l| {
        key_of(l) == "host" && l.trim()[4..].trim().trim_start_matches('=').split_whitespace().eq(want.iter().copied())
    })?;
    let end = (start + 1..lines.len())
        .find(|&i| matches!(key_of(lines[i]).as_str(), "host" | "match"))
        .unwrap_or(lines.len());
    Some((start, end))
}

/// The config text with `h` added, or the block of `was` updated to it.
pub fn upsert_host(text: &str, was: &str, h: &HostEntry) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let values =
        [h.hostname.trim(), h.user.trim(), if h.port.trim() == "22" { "" } else { h.port.trim() }, h.identity.trim()];
    let names = ["HostName", "User", "Port", "IdentityFile"];
    let Some((start, end)) = block(&lines, was).or_else(|| block(&lines, h.alias.trim())) else {
        let mut out = text.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("Host {}\n", h.alias.trim()));
        for (n, v) in names.iter().zip(values) {
            if !v.is_empty() {
                out.push_str(&format!("    {n} {v}\n"));
            }
        }
        return out;
    };
    let indent = lines[start + 1..end]
        .iter()
        .find(|l| !l.trim().is_empty())
        .map_or("    ".to_string(), |l| l[..l.len() - l.trim_start().len()].to_string());
    let mut body: Vec<String> = Vec::new();
    let mut done = [false; 4];
    for l in &lines[start + 1..end] {
        match MANAGED.iter().position(|k| *k == key_of(l)) {
            Some(i) if !done[i] && !values[i].is_empty() => {
                body.push(format!("{indent}{} {}", names[i], values[i]));
                done[i] = true;
            }
            // Further keys ssh also tries stay as they were.
            Some(3) if done[3] => body.push(l.to_string()),
            Some(_) => {}
            None => body.push(l.to_string()),
        }
    }
    // Keys that were not there yet go right under the Host line.
    let new: Vec<String> = (0..4)
        .filter(|&i| !done[i] && !values[i].is_empty())
        .map(|i| format!("{indent}{} {}", names[i], values[i]))
        .collect();
    let mut out: Vec<String> = lines[..start].iter().map(|l| l.to_string()).collect();
    out.push(format!("Host {}", h.alias.trim()));
    out.extend(new);
    out.extend(body);
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    out.join("\n") + "\n"
}

/// The config text without the block of `alias`.
pub fn remove_host(text: &str, alias: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some((start, end)) = block(&lines, alias) else { return text.to_string() };
    let mut out: Vec<&str> = lines[..start].to_vec();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let rest = &lines[end..];
    if !out.is_empty() && !rest.is_empty() {
        out.push("");
    }
    out.extend(rest);
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

#[derive(PartialEq)]
enum ConfigView {
    Hosts,
    File,
}

/// `~/.ssh/config`: its hosts as a list with a form to add, change or remove one, and the whole
/// file in an editor.
pub struct ConfigTool {
    view: ConfigView,
    hosts: Vec<HostEntry>,
    form: HostEntry,
    /// The alias of the host being changed; empty while adding one.
    editing: String,
    /// The dialog adding or changing a host is open.
    form_open: bool,
    text: String,
    saved: String,
    status: Option<(bool, String)>,
}

fn config_path() -> PathBuf {
    ssh_dir().join("config")
}

impl ConfigTool {
    pub fn new() -> Self {
        let text = std::fs::read_to_string(config_path()).unwrap_or_default();
        ConfigTool {
            view: ConfigView::Hosts,
            hosts: parse_config(&text),
            form: HostEntry { port: "22".into(), ..Default::default() },
            editing: String::new(),
            form_open: false,
            saved: text.clone(),
            text,
            status: None,
        }
    }

    /// Writes the file, keeping the previous one as config.bak.
    fn write(&mut self, text: String, done: String) -> bool {
        let path = config_path();
        let _ = std::fs::create_dir_all(ssh_dir());
        if path.exists() {
            let _ = std::fs::copy(&path, ssh_dir().join("config.bak"));
        }
        match std::fs::write(&path, &text) {
            Ok(()) => {
                self.hosts = parse_config(&text);
                self.saved = text.clone();
                self.text = text;
                self.status = Some((true, done));
                true
            }
            Err(e) => {
                self.status = Some((false, format!("{}: {e}", path.display())));
                false
            }
        }
    }

    fn save_form(&mut self) -> bool {
        let h = &self.form;
        let alias = h.alias.trim().to_string();
        if alias.is_empty() || h.hostname.trim().is_empty() || alias.contains(char::is_whitespace) {
            self.status = Some((false, "An alias (one word) and a host name or address are needed.".into()));
            return false;
        }
        if alias != self.editing && self.hosts.iter().any(|e| e.alias == alias) {
            self.status = Some((false, format!("{alias} is in the file already.")));
            return false;
        }
        if self.text != self.saved {
            self.status = Some((false, "The file has unsaved edits; save or revert them first.".into()));
            return false;
        }
        let text = upsert_host(&self.saved, &self.editing, h);
        let done = if self.editing.is_empty() { format!("Added {alias}") } else { format!("Saved {alias}") };
        let ok = self.write(text, done);
        if ok {
            self.form = HostEntry { port: "22".into(), ..Default::default() };
            self.editing.clear();
        }
        ok
    }

    /// The page, in the open view's tools; sets `changed` when the host list changed.
    pub fn ui(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        if self.form_open {
            return self.host_form(ui, changed);
        }
        ui.horizontal(|ui| {
            if ui.add(choice("Hosts", self.view == ConfigView::Hosts)).clicked() {
                self.view = ConfigView::Hosts;
            }
            let dirty = if self.text != self.saved { " *" } else { "" };
            if ui.add(choice(format!("Edit the file{dirty}"), self.view == ConfigView::File)).clicked() {
                self.view = ConfigView::File;
            }
        });
        ui.add_space(6.0);
        match self.view {
            ConfigView::Hosts => self.hosts_view(ui, changed),
            ConfigView::File => self.file_view(ui, changed),
        }
        status(ui, &self.status);
    }

    fn hosts_view(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        ui.label(RichText::new("ssh, this app and other tools use these names, users, ports and keys.").weak());
        ui.add_space(8.0);
        if ui.add(primary(format!("{}  Add a host", ic::PLUS))).clicked() {
            self.editing.clear();
            self.form = HostEntry { port: "22".into(), ..Default::default() };
            self.form_open = true;
            self.status = None;
        }
        let mut edit = None;
        let mut remove = None;
        ui.add_space(4.0);
        {
            egui::Grid::new("hosts").striped(true).num_columns(6).spacing([14.0, 2.0]).min_row_height(30.0).show(ui, |ui| {
                for t in ["Alias", "Host", "User", "Port", "Key", ""] {
                    ui.label(RichText::new(t).weak().small());
                }
                ui.end_row();
                for h in &self.hosts {
                    ui.label(RichText::new(&h.alias).strong());
                    ui.label(&h.hostname);
                    ui.label(&h.user);
                    ui.label(if h.port.is_empty() { "22" } else { &h.port });
                    ui.label(RichText::new(&h.identity).small());
                    ui.horizontal(|ui| {
                        if icon(ui, ic::PENCIL, &format!("Change {}", h.alias), self.editing == h.alias).clicked() {
                            edit = Some(h.clone());
                        }
                        if icon(ui, ic::TRASH, &format!("Remove {} from the file (the old file is kept as config.bak)", h.alias), false).clicked() {
                            remove = Some(h.alias.clone());
                        }
                    });
                    ui.end_row();
                }
            });
        }
        if let Some(h) = edit {
            self.editing = h.alias.clone();
            self.form = HostEntry { port: if h.port.is_empty() { "22".into() } else { h.port.clone() }, ..h };
            self.form_open = true;
            self.status = None;
        }
        if let Some(alias) = remove {
            if self.text != self.saved {
                self.status = Some((false, "The file has unsaved edits; save or revert them first.".into()));
            } else if self
                .write(remove_host(&self.saved, &alias), format!("Removed {alias} (the old file is config.bak)"))
            {
                *changed = true;
            }
        }
    }

    pub fn sub_page(&self) -> bool {
        self.form_open
    }

    /// Edits of the file not saved yet: leaving would lose them, so it asks for a save first.
    pub fn keep_open(&mut self) -> bool {
        let unsaved = self.text != self.saved;
        if unsaved {
            self.view = ConfigView::File;
            self.status = Some((false, "The file has unsaved edits: save them (Ctrl+S) or revert them first.".into()));
        }
        unsaved
    }

    pub fn back(&mut self) {
        self.form_open = false;
        self.editing.clear();
        self.status = None;
    }

    /// The form adding or changing a host, in place of the list.
    fn host_form(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        let title = if self.editing.is_empty() { "Add a host".to_string() } else { format!("Change {}", self.editing) };
        let ok = if self.editing.is_empty() { "Add host" } else { "Save" };
        let ready = !self.form.alias.trim().is_empty() && !self.form.hostname.trim().is_empty();
        let keys: Vec<String> = list_keys().into_iter().map(|k| format!("~/.ssh/{}", k.name)).collect();
        let status = self.status.clone();
        let answer = form_page(ui, &title, ok, ready, &status, |ui| {
            egui::Grid::new("addhost").striped(false).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Alias");
                ui.add(egui::TextEdit::singleline(&mut self.form.alias).hint_text("web1").desired_width(FIELD_W));
                ui.end_row();
                ui.label("Host name");
                ui.add(egui::TextEdit::singleline(&mut self.form.hostname).hint_text("192.168.1.20 or server.example.com").desired_width(FIELD_W));
                ui.end_row();
                ui.label("User");
                ui.add(egui::TextEdit::singleline(&mut self.form.user).desired_width(FIELD_W));
                ui.end_row();
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut self.form.port).desired_width(80.0));
                ui.end_row();
                ui.label("Key");
                let shown = if self.form.identity.is_empty() { "default".to_string() } else { self.form.identity.clone() };
                combo("hostkey", shown).width(FIELD_W).show_ui(ui, |ui| {
                    crate::gui::menu_look(ui);
                    ui.selectable_value(&mut self.form.identity, String::new(), "default");
                    for k in &keys {
                        ui.selectable_value(&mut self.form.identity, k.clone(), k);
                    }
                });
                ui.end_row();
            });
        });
        match answer {
            Some(true) => {
                if self.save_form() {
                    *changed = true;
                    self.form_open = false;
                }
            }
            Some(false) => {
                self.form_open = false;
                self.editing.clear();
                self.status = None;
            }
            None => {}
        }
    }

    fn file_view(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        ui.horizontal(|ui| {
            let dirty = self.text != self.saved;
            let save = primary(format!("{}  Save", ic::CHECK));
            if ui.add_enabled(dirty, save).on_hover_text("Ctrl+S; the previous file is kept as config.bak").clicked()
                || (dirty && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)))
            {
                let text = self.text.clone();
                if self.write(text, "Saved ~/.ssh/config (the old file is config.bak)".into()) {
                    *changed = true;
                }
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                self.text = self.saved.clone();
            }
            if ui.button("Reload").on_hover_text("Read the file again from disk").clicked() {
                *self = ConfigTool { view: ConfigView::File, ..ConfigTool::new() };
            }
            if ui.button("Open in the system editor").clicked() {
                let path = config_path();
                if !path.exists() {
                    let _ = std::fs::create_dir_all(ssh_dir());
                    let _ = std::fs::write(&path, "");
                }
                open_path(&path);
            }
        });
        let th = crate::markdown::Theme::of(ui);
        egui::Frame::new().fill(th.bg).corner_radius(crate::gui::theme::R_SM).inner_margin(6.0).show(ui, |ui| {
            egui::ScrollArea::both().max_height(440.0).auto_shrink([false, false]).show(
                ui,
                |ui| {
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _: f32| {
                        let mut job = crate::markdown::highlight(buf.as_str(), "ssh-config", &th, 13.0);
                        job.wrap.max_width = f32::INFINITY;
                        ui.ctx().fonts_mut(|f| f.layout_job(job))
                    };
                    ui.add(
                        egui::TextEdit::multiline(&mut self.text)
                            .code_editor()
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .desired_rows(18)
                            .layouter(&mut layouter),
                    );
                },
            );
        });
    }
}

/// Saved SSH passwords, kept in the system's password store.
pub struct VaultTool {
    hosts: Vec<String>,
    host: String,
    password: String,
    status: Option<(bool, String)>,
    adding: bool,
}

impl VaultTool {
    pub fn new(host: &str) -> Self {
        VaultTool { hosts: crate::vault::list(), host: host.to_string(), password: String::new(), status: None, adding: false }
    }

    /// The page, in the open view's tools. `known` are hosts to pick from.
    pub fn ui(&mut self, ui: &mut egui::Ui, known: &[String]) {
        if self.adding {
            return self.form(ui, known);
        }
        {
            let Some(store) = crate::vault::backend() else {
                ui.label(
                    RichText::new("This system has no password store sshremote can use. On Linux, install secret-tool (libsecret-tools).")
                        .color(bad()),
                );
                return;
            };
            ui.label(
                RichText::new(format!(
                    "Passwords for SSH logins, kept in {store}. The remote screen and terminal tabs answer the password prompt with them; SSH keys are still the better choice."
                ))
                .weak(),
            );
            ui.add_space(8.0);
            if ui.add(primary(format!("{}  Save a password", ic::PLUS))).clicked() {
                self.adding = true;
                self.status = None;
            }
            ui.add_space(4.0);
            let mut remove = None;
            if self.hosts.is_empty() {
                ui.label(RichText::new("No saved passwords yet.").weak().italics());
            }
            egui::Grid::new("vault").striped(true).num_columns(2).spacing([14.0, 2.0]).min_row_height(30.0).show(ui, |ui| {
                for h in &self.hosts {
                    ui.label(RichText::new(h).strong());
                    if icon(ui, ic::TRASH, &format!("Forget the password of {h}"), false).clicked() {
                        remove = Some(h.clone());
                    }
                    ui.end_row();
                }
            });
            if let Some(h) = remove {
                self.status = Some(match crate::vault::delete(&h) {
                    Ok(()) => (true, format!("Removed the password of {h}")),
                    Err(e) => (false, e),
                });
                self.hosts = crate::vault::list();
            }
            status(ui, &self.status);
        }
    }

    pub fn sub_page(&self) -> bool {
        self.adding
    }

    pub fn back(&mut self) {
        self.adding = false;
        self.password.clear();
        if self.status.as_ref().is_some_and(|s| !s.0) {
            self.status = None;
        }
    }

    /// The form saving a password, in place of the list.
    fn form(&mut self, ui: &mut egui::Ui, known: &[String]) {
        {
            let ready = !self.host.trim().is_empty() && !self.password.is_empty();
            let status = self.status.clone();
            let answer = form_page(ui, "Save a password", "Save", ready, &status, |ui| {
                egui::Grid::new("vault-add").striped(false).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Host");
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.add(egui::TextEdit::singleline(&mut self.host).hint_text("user@host or alias").desired_width(FIELD_W - 34.0));
                        let r = crate::gui::icon_menu(ui, ic::CARET_DOWN, |ui| {
                            for h in known {
                                if ui.button(h).clicked() {
                                    self.host = h.clone();
                                    ui.close();
                                }
                            }
                        });
                        r.on_hover_text("A known host");
                    });
                    ui.end_row();
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(FIELD_W));
                    ui.end_row();
                });
            });
            match answer {
                Some(true) => {
                    let host = self.host.trim().to_string();
                    self.status = Some(match crate::vault::set(&host, &self.password) {
                        Ok(()) => (true, format!("Saved the password of {host}")),
                        Err(e) => (false, e),
                    });
                    self.password.clear();
                    self.hosts = crate::vault::list();
                    if self.status.as_ref().is_some_and(|s| s.0) {
                        self.adding = false;
                    }
                }
                Some(false) => self.back(),
                None => {}
            }
        }
    }
}

/// Backups of hosts, settings and ~/.ssh, made by hand or automatically once a day.
pub struct BackupTool {
    passphrase: String,
    restore_pass: String,
    ssh_config: bool,
    ssh_files: bool,
    chosen: Option<PathBuf>,
    list: Vec<(PathBuf, String, bool)>,
    status: Option<(bool, String)>,
    /// The dialog open: backing up (false) or restoring the chosen backup (true).
    dialog: Option<bool>,
}

impl BackupTool {
    pub fn new() -> Self {
        let mut t = BackupTool {
            passphrase: String::new(),
            restore_pass: String::new(),
            ssh_config: false,
            ssh_files: true,
            chosen: None,
            list: Vec::new(),
            status: None,
            dialog: None,
        };
        t.refresh();
        t
    }

    fn refresh(&mut self) {
        self.list = crate::backup::list()
            .into_iter()
            .filter_map(|p| crate::backup::describe(&p).map(|(when, pw)| (p, when, pw)))
            .collect();
    }

    /// The page, in the open view's tools: the backups; backing up and restoring ask what they
    /// need in dialogs. Sets `changed` after a restore.
    pub fn ui(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        if self.dialog.is_some() {
            return self.forms(ui, changed);
        }
        ui.label(
            RichText::new(
                "Recent and favorite hosts, settings, ~/.ssh/config and known_hosts. With a passphrase, also the saved passwords and every file of ~/.ssh, private keys included, all encrypted with it. A copy without those is made automatically each day.",
            )
            .weak(),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add(primary(format!("{}  Back up now", ic::DOWNLOAD))).clicked() {
                self.dialog = Some(false);
                self.status = None;
            }
            if ui.add_enabled(self.chosen.is_some(), egui::Button::new(format!("{}  Restore", ic::UPLOAD)).min_size(egui::vec2(0.0, 32.0)))
                .on_disabled_hover_text("Choose a backup below first")
                .clicked()
            {
                self.dialog = Some(true);
                self.status = None;
            }
            if ui.button(format!("{}  Folder", ic::FOLDER)).clicked() {
                let dir = crate::backup::folder();
                let _ = std::fs::create_dir_all(&dir);
                open_path(&dir);
            }
        });
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        if self.list.is_empty() {
            ui.label(RichText::new("No backups yet.").weak().italics());
        }
        for (p, when, pw) in &self.list {
            let auto = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("auto-"));
            let label = format!("{}  {when}{}{}", ic::ARCHIVE, if auto { "   automatic" } else { "" }, if *pw { "   with passwords" } else { "" });
            let on = self.chosen.as_ref() == Some(p);
            let b = choice(label, on).min_size(egui::vec2(ui.available_width(), 30.0));
            if ui.add(b).on_hover_text(p.display().to_string()).clicked() {
                self.chosen = Some(p.clone());
            }
        }
        ui.add_space(6.0);
        status(ui, &self.status);
    }

    pub fn sub_page(&self) -> bool {
        self.dialog.is_some()
    }

    pub fn back(&mut self) {
        self.dialog = None;
        self.passphrase.clear();
        self.restore_pass.clear();
        if self.status.as_ref().is_some_and(|s| !s.0) {
            self.status = None;
        }
    }

    /// Backing up or restoring, in place of the list.
    fn forms(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        let status = self.status.clone();
        match self.dialog {
            Some(false) => {
                let min = crate::backup::MIN_PASSPHRASE;
                let answer = form_page(ui, "Back up now", "Back up", true, &status, |ui| {
                    ui.label(RichText::new(format!(
                        "With a passphrase (at least {min} characters), saved passwords and all of ~/.ssh, private keys too, are included, encrypted with it. Keep it: without it they cannot be restored."
                    )).weak());
                    ui.add_space(6.0);
                    ui.add(egui::TextEdit::singleline(&mut self.passphrase).password(true).hint_text("passphrase (optional)").desired_width(FIELD_W + 80.0));
                });
                match answer {
                    Some(true) => {
                        if !self.passphrase.is_empty() && self.passphrase.chars().count() < min {
                            self.status = Some((false, format!("Use a passphrase of at least {min} characters, or none.")));
                        } else {
                            let pass = Some(self.passphrase.as_str()).filter(|p| !p.is_empty());
                            self.status = Some(match crate::backup::save(pass) {
                                Ok(p) => (true, format!("Saved {}", p.display())),
                                Err(e) => (false, e),
                            });
                            self.passphrase.clear();
                            self.refresh();
                            self.dialog = None;
                        }
                    }
                    Some(false) => self.back(),
                    None => {}
                }
            }
            Some(true) => {
                let answer = form_page(ui, "Restore this backup", "Restore", self.chosen.is_some(), &status, |ui| {
                    ui.checkbox(&mut self.ssh_config, "Replace ~/.ssh/config with the backup's (the current one is kept as config.bak)");
                    ui.checkbox(&mut self.ssh_files, "Restore the files of ~/.ssh, keys included (needs the passphrase; changed files are kept as .bak)");
                    ui.add_space(6.0);
                    ui.add(egui::TextEdit::singleline(&mut self.restore_pass).password(true).hint_text("passphrase, for passwords and keys").desired_width(FIELD_W + 80.0));
                });
                match answer {
                    Some(true) => {
                        if let Some(p) = &self.chosen {
                            let pass = Some(self.restore_pass.as_str()).filter(|p| !p.is_empty());
                            self.status = Some(match crate::backup::restore(p, pass, self.ssh_config, self.ssh_files) {
                                Ok(s) => {
                                    *changed = true;
                                    (true, s)
                                }
                                Err(e) => (false, e),
                            });
                        }
                        if self.status.as_ref().is_some_and(|s| s.0) {
                            self.restore_pass.clear();
                            self.dialog = None;
                        }
                    }
                    Some(false) => self.back(),
                    None => {}
                }
            }
            None => {}
        }
    }
}

/// A form shown in place of the tool's list, as a web page opens an "add" page: its title,
/// the fields, a status line, then the main button and Cancel. Some(true) for the main button
/// (or Enter once `ready`), Some(false) for Cancel, None while it is being filled in; the tool
/// page's back arrow and Escape lead back to the list too.
fn form_page(
    ui: &mut egui::Ui,
    title: &str,
    ok: &str,
    ready: bool,
    status_line: &Option<(bool, String)>,
    fields: impl FnOnce(&mut egui::Ui),
) -> Option<bool> {
    let mut answer = None;
    ui.label(RichText::new(title).size(15.0).strong());
    ui.add_space(10.0);
    fields(ui);
    if let Some((ok, text)) = status_line.as_ref().filter(|s| !s.0) {
        ui.add_space(6.0);
        ui.label(RichText::new(text).color(if *ok { good() } else { bad() }));
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(ready, primary(ok)).clicked() {
            answer = Some(true);
        }
        if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(0.0, 32.0))).clicked() {
            answer = Some(false);
        }
    });
    // Enter in a text field submits; on a focused button or check box it presses that instead.
    let focus = ui.memory(|m| m.focused());
    let in_text = focus.is_none_or(|id| egui::text_edit::TextEditState::load(ui.ctx(), id).is_some());
    if ready && in_text && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        answer = Some(true);
    }
    answer
}

/// The width of the tools' text fields and drop-downs.
const FIELD_W: f32 = 300.0;

fn status(ui: &mut egui::Ui, s: &Option<(bool, String)>) {
    if let Some((ok, text)) = s {
        ui.label(RichText::new(text).color(if *ok { good() } else { bad() }));
    }
}

/// Opens a file or folder with the system's default program.
pub fn open_path(p: &std::path::Path) {
    if cfg!(windows) {
        if p.is_file() {
            let _ = quiet(Command::new("notepad.exe").arg(p)).spawn();
        } else {
            let _ = Command::new("explorer.exe").arg(p).spawn();
        }
    } else {
        let _ = Command::new("xdg-open").arg(p).spawn();
    }
}

#[cfg(test)]
mod tests {
    use crate::gui::snapshot::RunTest;
    #[test]
    fn config_blocks_are_read() {
        let text = "Host web1
  HostName 10.0.0.5
  User ana
  Port=2222
Host *
  ServerAliveInterval 30
";
        let hosts = super::parse_config(text);
        assert_eq!(hosts.len(), 1);
        let h = &hosts[0];
        assert_eq!(
            (h.alias.as_str(), h.hostname.as_str(), h.user.as_str(), h.port.as_str()),
            ("web1", "10.0.0.5", "ana", "2222")
        );
    }

    #[test]
    fn hosts_are_added_changed_and_removed_in_place() {
        use super::{HostEntry, remove_host, upsert_host};
        let text = "# mine\nHost web1\n  HostName 10.0.0.5\n  User ana\n  ForwardAgent yes\n\nHost *\n  ServerAliveInterval 30\n";
        let h = |alias: &str, host: &str, user: &str, port: &str| HostEntry {
            alias: alias.into(),
            hostname: host.into(),
            user: user.into(),
            port: port.into(),
            identity: String::new(),
        };
        let changed = upsert_host(text, "web1", &h("web2", "10.0.0.6", "", "2222"));
        assert_eq!(
            changed,
            "# mine\nHost web2\n  Port 2222\n  HostName 10.0.0.6\n  ForwardAgent yes\n\nHost *\n  ServerAliveInterval 30\n"
        );
        let added = upsert_host(text, "", &h("db", "10.0.0.9", "root", "22"));
        assert!(
            added.ends_with("ServerAliveInterval 30\n\nHost db\n    HostName 10.0.0.9\n    User root\n"),
            "{added}"
        );
        assert_eq!(remove_host(text, "web1"), "# mine\n\nHost *\n  ServerAliveInterval 30\n");
        assert_eq!(remove_host(text, "nothere"), text);
    }

    /// A Host line with several names is one entry, changed and removed whole; lines under a
    /// Match belong to no host; a second IdentityFile stays when the host is saved.
    #[test]
    fn several_names_match_blocks_and_keys_are_kept() {
        use super::{parse_config, remove_host, upsert_host};
        let text = "Host a b
  HostName 10.0.0.1
  IdentityFile ~/.ssh/one
  IdentityFile ~/.ssh/two
Match host x
  User other
Host c
  User me
";
        let hosts = parse_config(text);
        assert_eq!(hosts.len(), 2);
        assert_eq!((hosts[0].alias.as_str(), hosts[0].user.as_str(), hosts[0].identity.as_str()), ("a b", "", "~/.ssh/one"));
        assert_eq!(hosts[1].user, "me", "the Match's User is not c's or a's");
        let saved = upsert_host(text, "a b", &hosts[0]);
        assert!(saved.contains("Host a b
") && saved.contains("IdentityFile ~/.ssh/two"), "{saved}");
        assert_eq!(saved.matches("Host a b").count(), 1, "changed in place, not added again");
        assert!(!remove_host(text, "a b").contains("10.0.0.1"));
    }

    /// Every tool window draws for a few frames, in both views of the hosts window, without
    /// panicking (it reads ~/.ssh, the password store and the backup folders, and writes nothing).
    #[test]
    fn tool_windows_draw() {
        let ctx = eframe::egui::Context::default();
        crate::gui::style(&ctx);
        let mut keys = super::KeyTool::new();
        let mut config = super::ConfigTool::new();
        let mut vault = super::VaultTool::new("user@host");
        let mut backup = super::BackupTool::new();
        for frame in 0..4 {
            if frame == 2 {
                config.view = super::ConfigView::File;
            }
            let input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    Default::default(),
                    eframe::egui::vec2(1200.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run_test(input, |ui| {
                let mut actions = Vec::new();
                let mut changed = false;
                keys.ui(ui, &mut actions);
                config.ui(ui, &mut changed);
                vault.ui(ui, &["a".into(), "b".into()]);
                backup.ui(ui, &mut changed);
                assert!(!changed, "drawing changes nothing");
            });
        }
    }
}
