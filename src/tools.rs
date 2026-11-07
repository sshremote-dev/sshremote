//! SSH helpers on the home screen: key generation and installation, the host entries of
//! ~/.ssh/config, saved passwords, and backups.

use std::path::PathBuf;
use std::process::Command;

use eframe::egui::{self, Color32, RichText};

use crate::gui::{ACCENT, ic};

fn ssh_dir() -> PathBuf {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
    home.join(".ssh")
}

/// Runs a program without a console window flashing up.
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

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
    pass: String,
    pass2: String,
    status: Option<(bool, String)>,
    keys: Vec<KeyPair>,
    host: String,
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
            pass: String::new(),
            pass2: String::new(),
            status: None,
            keys,
            host: String::new(),
        }
    }

    fn generate(&mut self) {
        let name = self.name.trim();
        if name.is_empty() || name.contains(['/', '\\']) {
            self.status = Some((false, "Give the key a file name, such as id_ed25519_work.".into()));
            return;
        }
        if self.pass != self.pass2 {
            self.status = Some((false, "The two passphrases differ.".into()));
            return;
        }
        let dir = ssh_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        if path.exists() {
            self.status = Some((false, format!("{} exists already; choose another name.", path.display())));
            return;
        }
        let out = quiet(
            Command::new("ssh-keygen")
                .args(KINDS[self.kind].1)
                .arg("-C")
                .arg(&self.comment)
                .arg("-f")
                .arg(&path)
                .arg("-N")
                .arg(&self.pass),
        )
        .output();
        self.status = Some(match out {
            Ok(o) if o.status.success() => (true, format!("Created {} and {}.pub", path.display(), path.display())),
            Ok(o) => (false, String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => (false, format!("ssh-keygen: {e} (is OpenSSH installed?)")),
        });
        self.pass.clear();
        self.pass2.clear();
        self.keys = list_keys();
    }

    /// The window; returns false once closed.
    pub fn show(&mut self, ctx: &egui::Context, actions: &mut Vec<ToolAction>) -> bool {
        let mut open = true;
        egui::Window::new(format!("{}  SSH keys", ic::KEY)).open(&mut open).default_width(560.0).collapsible(false).show(ctx, |ui| {
            ui.label(RichText::new("Your key pairs in ~/.ssh. Copy a public key, or install it on a host so ssh signs in without a password.").weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Host to install on");
                ui.add(egui::TextEdit::singleline(&mut self.host).hint_text("user@host or alias").desired_width(220.0));
            });
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                if self.keys.is_empty() {
                    ui.label(RichText::new("No keys yet. Create one below.").weak());
                }
                for k in &self.keys {
                    egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(8).inner_margin(8.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}  {}", ic::KEY, k.name)).strong());
                            let kind = k.public.split_whitespace().next().unwrap_or("").trim_start_matches("ssh-");
                            ui.label(RichText::new(kind).weak().small());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let host = self.host.trim().to_string();
                                let b = ui.add_enabled(!host.is_empty(), egui::Button::new("Install on host"));
                                if b.on_hover_text("Adds the public key to the host's ~/.ssh/authorized_keys; you sign in once with the password").clicked() {
                                    let key = k.public.replace('\'', "");
                                    let script = format!(
                                        "umask 077; mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && (grep -qxF '{key}' ~/.ssh/authorized_keys || echo '{key}' >> ~/.ssh/authorized_keys) && echo 'Key {} installed on' $(hostname)",
                                        k.name
                                    );
                                    actions.push(ToolAction::Terminal {
                                        title: format!("Install key on {host}"),
                                        argv: vec!["ssh".into(), host, script],
                                    });
                                }
                                if ui.button(format!("{}  Copy public key", ic::COPY)).clicked() {
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
            });
            ui.separator();
            ui.label(RichText::new("Create a key").strong());
            egui::Grid::new("keygen").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Type");
                egui::ComboBox::from_id_salt("keytype").selected_text(KINDS[self.kind].0).show_ui(ui, |ui| {
                    for (i, k) in KINDS.iter().enumerate() {
                        ui.selectable_value(&mut self.kind, i, k.0);
                    }
                });
                ui.end_row();
                ui.label("File name");
                ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("id_ed25519_work"));
                ui.end_row();
                ui.label("Comment");
                ui.add(egui::TextEdit::singleline(&mut self.comment));
                ui.end_row();
                ui.label("Passphrase");
                ui.add(egui::TextEdit::singleline(&mut self.pass).password(true).hint_text("optional"));
                ui.end_row();
                ui.label("Again");
                ui.add(egui::TextEdit::singleline(&mut self.pass2).password(true));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Create key").color(Color32::WHITE)).fill(ACCENT)).clicked() {
                    self.generate();
                }
                if ui.button("Open the .ssh folder").clicked() {
                    open_path(&ssh_dir());
                }
            });
            status(ui, &self.status);
        });
        open
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
    for line in text.lines() {
        let l = line.trim();
        let Some((k, v)) = l.split_once(|c: char| c.is_whitespace() || c == '=') else { continue };
        let v = v.trim().trim_start_matches('=').trim().to_string();
        match k.to_lowercase().as_str() {
            "host" => out.push(HostEntry { alias: v, ..Default::default() }),
            "hostname" => out.last_mut().into_iter().for_each(|h| h.hostname = v.clone()),
            "user" => out.last_mut().into_iter().for_each(|h| h.user = v.clone()),
            "port" => out.last_mut().into_iter().for_each(|h| h.port = v.clone()),
            "identityfile" => out.last_mut().into_iter().for_each(|h| h.identity = v.clone()),
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
    let start = lines.iter().position(|l| {
        key_of(l) == "host" && l.trim()[4..].trim().trim_start_matches('=').split_whitespace().eq([alias])
    })?;
    let end = (start + 1..lines.len()).find(|&i| matches!(key_of(lines[i]).as_str(), "host" | "match")).unwrap_or(lines.len());
    Some((start, end))
}

/// The config text with `h` added, or the block of `was` updated to it.
pub fn upsert_host(text: &str, was: &str, h: &HostEntry) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let values = [h.hostname.trim(), h.user.trim(), if h.port.trim() == "22" { "" } else { h.port.trim() }, h.identity.trim()];
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
            Some(_) => {}
            None => body.push(l.to_string()),
        }
    }
    // Keys that were not there yet go right under the Host line.
    let new: Vec<String> = (0..4).filter(|&i| !done[i] && !values[i].is_empty()).map(|i| format!("{indent}{} {}", names[i], values[i])).collect();
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

    /// The window; returns false once closed, and whether the host list changed.
    pub fn show(&mut self, ctx: &egui::Context, changed: &mut bool) -> bool {
        let mut open = true;
        let title = format!("{}  SSH hosts (~/.ssh/config)", ic::GLOBE);
        egui::Window::new(title).open(&mut open).default_size([680.0, 520.0]).collapsible(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.view, ConfigView::Hosts, "Hosts");
                let dirty = if self.text != self.saved { " *" } else { "" };
                ui.selectable_value(&mut self.view, ConfigView::File, format!("Edit the file{dirty}"));
            });
            ui.separator();
            match self.view {
                ConfigView::Hosts => self.hosts_view(ui, changed),
                ConfigView::File => self.file_view(ui, changed),
            }
            status(ui, &self.status);
        });
        open
    }

    fn hosts_view(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        ui.label(RichText::new("ssh, this app and other tools use these names, users, ports and keys.").weak());
        let mut edit = None;
        let mut remove = None;
        egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
            egui::Grid::new("hosts").striped(true).num_columns(6).spacing([14.0, 4.0]).show(ui, |ui| {
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
                        if ui.small_button("Edit").clicked() {
                            edit = Some(h.clone());
                        }
                        if ui.small_button("Remove").clicked() {
                            remove = Some(h.alias.clone());
                        }
                    });
                    ui.end_row();
                }
            });
        });
        if let Some(h) = edit {
            self.editing = h.alias.clone();
            self.form = HostEntry { port: if h.port.is_empty() { "22".into() } else { h.port.clone() }, ..h };
        }
        if let Some(alias) = remove {
            if self.text != self.saved {
                self.status = Some((false, "The file has unsaved edits; save or revert them first.".into()));
            } else if self.write(remove_host(&self.saved, &alias), format!("Removed {alias} (the old file is config.bak)")) {
                *changed = true;
            }
        }
        ui.separator();
        let heading = if self.editing.is_empty() { "Add a host".to_string() } else { format!("Change {}", self.editing) };
        ui.label(RichText::new(heading).strong());
        let keys: Vec<String> = list_keys().into_iter().map(|k| format!("~/.ssh/{}", k.name)).collect();
        egui::Grid::new("addhost").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Alias");
            ui.add(egui::TextEdit::singleline(&mut self.form.alias).hint_text("web1"));
            ui.end_row();
            ui.label("Host name");
            ui.add(egui::TextEdit::singleline(&mut self.form.hostname).hint_text("192.168.1.20 or server.example.com"));
            ui.end_row();
            ui.label("User");
            ui.add(egui::TextEdit::singleline(&mut self.form.user));
            ui.end_row();
            ui.label("Port");
            ui.add(egui::TextEdit::singleline(&mut self.form.port).desired_width(60.0));
            ui.end_row();
            ui.label("Key");
            let shown = if self.form.identity.is_empty() { "default".to_string() } else { self.form.identity.clone() };
            egui::ComboBox::from_id_salt("hostkey").selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.form.identity, String::new(), "default");
                for k in &keys {
                    ui.selectable_value(&mut self.form.identity, k.clone(), k);
                }
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let label = if self.editing.is_empty() { "Add host" } else { "Save host" };
            if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(ACCENT)).clicked() && self.save_form() {
                *changed = true;
            }
            if !self.editing.is_empty() && ui.button("Cancel").clicked() {
                self.editing.clear();
                self.form = HostEntry { port: "22".into(), ..Default::default() };
            }
        });
    }

    fn file_view(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        ui.horizontal(|ui| {
            let dirty = self.text != self.saved;
            let save = egui::Button::new(RichText::new(format!("{}  Save", ic::CHECK)).color(Color32::WHITE)).fill(ACCENT);
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
        egui::Frame::new().fill(th.bg).corner_radius(6).inner_margin(6.0).show(ui, |ui| {
            egui::ScrollArea::both().max_height(ui.available_height() - 30.0).auto_shrink([false, false]).show(ui, |ui| {
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
            });
        });
    }
}

/// Saved SSH passwords, kept in the system's password store.
pub struct VaultTool {
    hosts: Vec<String>,
    host: String,
    password: String,
    status: Option<(bool, String)>,
}

impl VaultTool {
    pub fn new(host: &str) -> Self {
        VaultTool { hosts: crate::vault::list(), host: host.to_string(), password: String::new(), status: None }
    }

    /// The window; returns false once closed. `known` are hosts to pick from.
    pub fn show(&mut self, ctx: &egui::Context, known: &[String]) -> bool {
        let mut open = true;
        let title = format!("{}  Saved passwords", ic::LOCK_KEY);
        egui::Window::new(title).open(&mut open).default_width(480.0).collapsible(false).show(ctx, |ui| {
            let Some(store) = crate::vault::backend() else {
                ui.label(
                    RichText::new("This system has no password store sshremote can use. On Linux, install secret-tool (libsecret-tools).")
                        .color(BAD),
                );
                return;
            };
            ui.label(
                RichText::new(format!(
                    "Passwords for SSH logins, kept in {store}. The remote screen and terminal tabs answer the password prompt with them; SSH keys are still the better choice."
                ))
                .weak(),
            );
            ui.add_space(4.0);
            let mut remove = None;
            if self.hosts.is_empty() {
                ui.label(RichText::new("No saved passwords yet.").weak().italics());
            }
            egui::Grid::new("vault").striped(true).num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                for h in &self.hosts {
                    ui.label(RichText::new(h).strong());
                    if ui.small_button("Remove").clicked() {
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
            ui.separator();
            egui::Grid::new("vault-add").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Host");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.host).hint_text("user@host or alias").desired_width(220.0));
                    egui::ComboBox::from_id_salt("vault-hosts").selected_text("").width(24.0).show_ui(ui, |ui| {
                        for h in known {
                            if ui.selectable_label(false, h).clicked() {
                                self.host = h.clone();
                            }
                        }
                    });
                });
                ui.end_row();
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(220.0));
                ui.end_row();
            });
            let ready = !self.host.trim().is_empty() && !self.password.is_empty();
            let save = egui::Button::new(RichText::new("Save password").color(Color32::WHITE)).fill(ACCENT);
            if ui.add_enabled(ready, save).clicked() {
                let host = self.host.trim().to_string();
                self.status = Some(match crate::vault::set(&host, &self.password) {
                    Ok(()) => (true, format!("Saved the password of {host}")),
                    Err(e) => (false, e),
                });
                self.password.clear();
                self.hosts = crate::vault::list();
            }
            status(ui, &self.status);
        });
        open
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

    /// The window; returns false once closed, and sets `changed` after a restore.
    pub fn show(&mut self, ctx: &egui::Context, changed: &mut bool) -> bool {
        let mut open = true;
        let title = format!("{}  Backup and restore", ic::ARCHIVE);
        egui::Window::new(title).open(&mut open).default_width(560.0).collapsible(false).show(ctx, |ui| {
            ui.label(
                RichText::new(
                    "Recent and favorite hosts, settings, ~/.ssh/config and known_hosts. With a passphrase, also the saved passwords and every file of ~/.ssh, private keys included, all encrypted with it. A copy without those is made automatically each day.",
                )
                .weak(),
            );
            ui.add_space(4.0);
            ui.label(RichText::new("Back up").strong());
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.passphrase).password(true).hint_text("passphrase (optional)").desired_width(200.0))
                    .on_hover_text(format!(
                        "With a passphrase (at least {} characters), saved passwords and all of ~/.ssh, private keys too, are included, encrypted with it. Keep it: without it they cannot be restored.",
                        crate::backup::MIN_PASSPHRASE
                    ));
                let now = egui::Button::new(RichText::new(format!("{}  Back up now", ic::DOWNLOAD)).color(Color32::WHITE)).fill(ACCENT);
                if ui.add(now).clicked() {
                    let min = crate::backup::MIN_PASSPHRASE;
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
                    }
                }
                if ui.button(format!("{}  Folder", ic::FOLDER)).clicked() {
                    let dir = crate::backup::folder();
                    let _ = std::fs::create_dir_all(&dir);
                    open_path(&dir);
                }
            });
            ui.separator();
            ui.label(RichText::new("Restore").strong());
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                if self.list.is_empty() {
                    ui.label(RichText::new("No backups yet.").weak().italics());
                }
                for (p, when, pw) in &self.list {
                    let auto = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("auto-"));
                    let label =
                        format!("{when}{}{}", if auto { "   automatic" } else { "" }, if *pw { "   with passwords" } else { "" });
                    let r = ui.selectable_label(self.chosen.as_ref() == Some(p), label).on_hover_text(p.display().to_string());
                    if r.clicked() {
                        self.chosen = Some(p.clone());
                    }
                }
            });
            ui.checkbox(&mut self.ssh_config, "Replace ~/.ssh/config with the backup's (the current one is kept as config.bak)");
            ui.checkbox(&mut self.ssh_files, "Restore the files of ~/.ssh, keys included (needs the passphrase; changed files are kept as .bak)");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.restore_pass).password(true).hint_text("passphrase, for passwords").desired_width(200.0),
                );
                if ui.add_enabled(self.chosen.is_some(), egui::Button::new(format!("{}  Restore", ic::UPLOAD))).clicked()
                    && let Some(p) = &self.chosen
                {
                    let pass = Some(self.restore_pass.as_str()).filter(|p| !p.is_empty());
                    self.status = Some(match crate::backup::restore(p, pass, self.ssh_config, self.ssh_files) {
                        Ok(s) => {
                            *changed = true;
                            (true, s)
                        }
                        Err(e) => (false, e),
                    });
                }
            });
            status(ui, &self.status);
        });
        open
    }
}

const GOOD: Color32 = Color32::from_rgb(90, 200, 120);
const BAD: Color32 = Color32::from_rgb(240, 110, 100);

fn status(ui: &mut egui::Ui, s: &Option<(bool, String)>) {
    if let Some((ok, text)) = s {
        ui.label(RichText::new(text).color(if *ok { GOOD } else { BAD }));
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
        assert!(added.ends_with("ServerAliveInterval 30\n\nHost db\n    HostName 10.0.0.9\n    User root\n"), "{added}");
        assert_eq!(remove_host(text, "web1"), "# mine\n\nHost *\n  ServerAliveInterval 30\n");
        assert_eq!(remove_host(text, "nothere"), text);
    }
}
