//! SSH helpers on the home screen: key generation and installation, and the
//! host entries of ~/.ssh/config.

use std::path::PathBuf;
use std::process::Command;

use eframe::egui::{self, Color32, RichText};

use crate::gui::{ACCENT, ic};

fn ssh_dir() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
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
        KeyTool { name, kind: 0, comment: format!("{user}@{host}").to_lowercase(), pass: String::new(), pass2: String::new(), status: None, keys, host: String::new() }
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
            if let Some((ok, text)) = &self.status {
                let color = if *ok { Color32::from_rgb(90, 200, 120) } else { Color32::from_rgb(240, 110, 100) };
                ui.label(RichText::new(text).color(color));
            }
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

pub fn read_config() -> Vec<HostEntry> {
    parse_config(&std::fs::read_to_string(ssh_dir().join("config")).unwrap_or_default())
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

pub struct ConfigTool {
    hosts: Vec<HostEntry>,
    new: HostEntry,
    status: Option<(bool, String)>,
}

impl ConfigTool {
    pub fn new() -> Self {
        ConfigTool { hosts: read_config(), new: HostEntry { port: "22".into(), ..Default::default() }, status: None }
    }

    fn add(&mut self) -> bool {
        let h = &self.new;
        if h.alias.trim().is_empty() || h.hostname.trim().is_empty() || h.alias.contains(char::is_whitespace) {
            self.status = Some((false, "An alias (one word) and a host name or address are needed.".into()));
            return false;
        }
        if self.hosts.iter().any(|e| e.alias == h.alias.trim()) {
            self.status = Some((false, format!("{} is in the file already.", h.alias.trim())));
            return false;
        }
        let mut block = format!("\nHost {}\n    HostName {}\n", h.alias.trim(), h.hostname.trim());
        if !h.user.trim().is_empty() {
            block.push_str(&format!("    User {}\n", h.user.trim()));
        }
        if !h.port.trim().is_empty() && h.port.trim() != "22" {
            block.push_str(&format!("    Port {}\n", h.port.trim()));
        }
        if !h.identity.trim().is_empty() {
            block.push_str(&format!("    IdentityFile {}\n", h.identity.trim()));
        }
        let dir = ssh_dir();
        let _ = std::fs::create_dir_all(&dir);
        use std::io::Write;
        let r = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("config")).and_then(|mut f| f.write_all(block.as_bytes()));
        match r {
            Ok(()) => {
                self.status = Some((true, format!("Added {} to ~/.ssh/config", h.alias.trim())));
                self.hosts = read_config();
                self.new = HostEntry { port: "22".into(), ..Default::default() };
                true
            }
            Err(e) => {
                self.status = Some((false, e.to_string()));
                false
            }
        }
    }

    /// The window; returns false once closed, and whether the host list changed.
    pub fn show(&mut self, ctx: &egui::Context, changed: &mut bool) -> bool {
        let mut open = true;
        egui::Window::new(format!("{}  SSH hosts", ic::GLOBE)).open(&mut open).default_width(600.0).collapsible(false).show(ctx, |ui| {
            ui.label(RichText::new("Hosts in ~/.ssh/config. ssh, this app and other tools use these names, users, ports and keys.").weak());
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                egui::Grid::new("hosts").striped(true).num_columns(5).spacing([14.0, 4.0]).show(ui, |ui| {
                    for t in ["Alias", "Host", "User", "Port", "Key"] {
                        ui.label(RichText::new(t).weak().small());
                    }
                    ui.end_row();
                    for h in &self.hosts {
                        ui.label(RichText::new(&h.alias).strong());
                        ui.label(&h.hostname);
                        ui.label(&h.user);
                        ui.label(if h.port.is_empty() { "22" } else { &h.port });
                        ui.label(RichText::new(&h.identity).small());
                        ui.end_row();
                    }
                });
            });
            ui.separator();
            ui.label(RichText::new("Add a host").strong());
            let keys: Vec<String> = list_keys().into_iter().map(|k| format!("~/.ssh/{}", k.name)).collect();
            egui::Grid::new("addhost").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Alias");
                ui.add(egui::TextEdit::singleline(&mut self.new.alias).hint_text("web1"));
                ui.end_row();
                ui.label("Host name");
                ui.add(egui::TextEdit::singleline(&mut self.new.hostname).hint_text("192.168.1.20 or server.example.com"));
                ui.end_row();
                ui.label("User");
                ui.add(egui::TextEdit::singleline(&mut self.new.user));
                ui.end_row();
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut self.new.port).desired_width(60.0));
                ui.end_row();
                ui.label("Key");
                let shown = if self.new.identity.is_empty() { "default".to_string() } else { self.new.identity.clone() };
                egui::ComboBox::from_id_salt("hostkey").selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.new.identity, String::new(), "default");
                    for k in &keys {
                        ui.selectable_value(&mut self.new.identity, k.clone(), k);
                    }
                });
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Add host").color(Color32::WHITE)).fill(ACCENT)).clicked() && self.add() {
                    *changed = true;
                }
                if ui.button("Edit the file").clicked() {
                    let path = ssh_dir().join("config");
                    if !path.exists() {
                        let _ = std::fs::create_dir_all(ssh_dir());
                        let _ = std::fs::write(&path, "");
                    }
                    open_path(&path);
                }
            });
            if let Some((ok, text)) = &self.status {
                let color = if *ok { Color32::from_rgb(90, 200, 120) } else { Color32::from_rgb(240, 110, 100) };
                ui.label(RichText::new(text).color(color));
            }
        });
        open
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
        assert_eq!((h.alias.as_str(), h.hostname.as_str(), h.user.as_str(), h.port.as_str()), ("web1", "10.0.0.5", "ana", "2222"));
    }
}
