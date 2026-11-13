//! Signing in when a host refused the login: which host, user, port and for what, then a
//! password, or a key file and its passphrase; for a computer reached by its ID, its password.
//! What is typed is remembered in the system's password store unless that is unticked.

use eframe::egui::{self, RichText};

use super::ic;
use crate::options::Target;

pub(super) struct SignIn {
    /// The pane it is for.
    pub pane: u64,
    host: String,
    user: String,
    port: String,
    purpose: String,
    why: String,
    password: String,
    key: String,
    remember: bool,
    /// A computer reached by its remote access ID: a password and nothing else; the name its
    /// password is kept under.
    by_id: Option<String>,
    /// The password field has had the keyboard once (later the user puts it where they like).
    focused: bool,
    /// The computer wants its authenticator app's code too, typed here.
    wants_code: bool,
    code: String,
}

pub(super) enum Answer {
    /// Try again with this target.
    Connect(Target),
    Cancel,
}

/// Whether a failed connection is the host refusing the login (as opposed to not answering).
pub(super) fn refused(why: &str) -> bool {
    let l = why.to_lowercase();
    ["permission denied", "authentication failed", "too many authentication failures", "passphrase", "password"].iter().any(|w| l.contains(w))
}

impl SignIn {
    pub fn new(pane: u64, target: &Target, purpose: &str, why: &str) -> Option<SignIn> {
        let why = why.lines().find(|l| !l.trim().is_empty()).unwrap_or(why).trim().to_string();
        let remember = crate::vault::backend().is_some();
        #[cfg(feature = "remote")]
        if let Target::Direct { addr, password } = target {
            let address = crate::access::address(&addr.id, &addr.relay);
            // A computer with two-step sign-in took the password: only the code is missing (or wrong).
            let wants_code = why.contains("code");
            let why = if wants_code {
                "The password was right; this computer wants the code of its authenticator app too.".to_string()
            } else if why.contains("wrong password") {
                "That password was not accepted.".to_string()
            } else {
                why
            };
            let known = password.split(crate::net::CODE_SEP).next().unwrap_or("").to_string();
            let password = if wants_code && !known.is_empty() { known } else if wants_code { crate::vault::get(&address).unwrap_or_default() } else { String::new() };
            return Some(SignIn {
                pane,
                host: format!("ID {}", crate::computers::spaced(&addr.id)),
                user: String::new(),
                port: String::new(),
                purpose: purpose.to_string(),
                why,
                password,
                key: String::new(),
                remember,
                by_id: Some(address),
                focused: false,
                wants_code,
                code: String::new(),
            });
        }
        let Target::Ssh { host, port, args, .. } = target else { return None };
        let (user, name) = match host.split_once('@') {
            Some((u, h)) => (u.to_string(), h.to_string()),
            None => (String::new(), host.clone()),
        };
        // The key named with -i, if any.
        let key = args.iter().position(|a| a == "-i").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
        Some(SignIn {
            pane,
            host: name,
            user,
            port: port.map_or_else(String::new, |p| p.to_string()),
            purpose: purpose.to_string(),
            why,
            password: String::new(),
            key,
            remember,
            by_id: None,
            focused: false,
            wants_code: false,
            code: String::new(),
        })
    }

    /// The dialog; an answer once one of its buttons is used.
    pub fn show(&mut self, ctx: &egui::Context, target: &Target) -> Option<Answer> {
        let by_id = self.by_id.is_some();
        let (modal, fit) = super::modal(ctx, "sign-in", egui::Vec2::new(460.0, if self.wants_code { 300.0 } else if by_id { 240.0 } else { 330.0 }));
        let mut answer = None;
        let r = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.label(RichText::new(format!("{}  Sign in to {}", ic::LOCK_KEY, self.host)).size(16.0).strong());
            ui.add_space(6.0);
            egui::Grid::new("sign-in-host").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                    ui.label(RichText::new(k).weak());
                    ui.label(v);
                    ui.end_row();
                };
                if by_id {
                    row(ui, "Computer", &self.host);
                } else {
                    row(ui, "Host", &self.host);
                    // A login refused for the wrong user (or a port that is not ssh's) is put
                    // right here.
                    ui.label(RichText::new("User").weak());
                    ui.add(egui::TextEdit::singleline(&mut self.user).hint_text("from ~/.ssh/config, or yours here").desired_width(220.0));
                    ui.end_row();
                    ui.label(RichText::new("Port").weak());
                    ui.add(egui::TextEdit::singleline(&mut self.port).hint_text("22, or from ~/.ssh/config").desired_width(80.0));
                    ui.end_row();
                }
                row(ui, "For", &self.purpose);
            });
            ui.add_space(4.0);
            ui.label(RichText::new(&self.why).small().weak());
            ui.add_space(8.0);
            ui.label(if by_id { "The password set on that computer" } else { "Password, or the key's passphrase" });
            let pw = ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(fit.x));
            let code = self.wants_code.then(|| {
                ui.add_space(4.0);
                ui.label("The code your authenticator app shows now");
                ui.add(egui::TextEdit::singleline(&mut self.code).hint_text("six digits").desired_width(140.0))
            });
            // The keyboard starts here, once; after that it stays where the user puts it.
            if !self.focused {
                match &code {
                    Some(c) if !self.password.is_empty() => c.request_focus(),
                    _ => pw.request_focus(),
                }
                self.focused = true;
            }
            if !by_id {
            ui.add_space(4.0);
            ui.label("Key file (optional)");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.key).hint_text("~/.ssh/id_ed25519").desired_width(fit.x - 110.0));
                ui.menu_button("Your keys", |ui| {
                    let keys = crate::tools::list_keys();
                    if keys.is_empty() {
                        ui.label(RichText::new("No keys in ~/.ssh").weak());
                    }
                    for k in keys {
                        if ui.button(&k.name).clicked() {
                            self.key = crate::sys::ssh_dir().join(&k.name).to_string_lossy().into_owned();
                            ui.close();
                        }
                    }
                });
            });
            }
            ui.add_space(4.0);
            ui.add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.remember, "Remember the password in the system's password store"))
                .on_hover_text("So it is not asked again; unticked, it is used for this connection only and never written anywhere");
            ui.add_space(8.0);
            // Taken, so it does not also press whatever else is on the screen.
            let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            ui.horizontal(|ui| {
                let ready = !self.password.is_empty() || !self.key.trim().is_empty();
                if ui.add_enabled(ready, super::theme::primary(format!("{}  Connect", ic::ARROW_CLOCKWISE))).clicked() || (enter && ready) {
                    answer = Some(Answer::Connect(self.target(target)));
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::Cancel);
                }
            });
        });
        if r.should_close() && answer.is_none() {
            answer = Some(Answer::Cancel);
        }
        answer
    }

    /// What to keep in the password store once the connection made with `target` works: the
    /// name the open window looks it up by (the host, or the ID's address) and the password.
    pub fn keep(&self, target: &Target) -> Option<(String, String)> {
        if !self.remember || self.password.is_empty() {
            return None;
        }
        let name = match target {
            Target::Ssh { host, .. } => host.clone(),
            _ => self.by_id.clone()?,
        };
        Some((name, self.password.clone()))
    }

    /// The target with what was typed: the password (or passphrase) and the key.
    fn target(&self, t: &Target) -> Target {
        let mut t = t.clone();
        #[cfg(feature = "remote")]
        if let Target::Direct { password, .. } = &mut t {
            *password = self.password.clone();
            let code = self.code.trim();
            if !code.is_empty() {
                password.push(crate::net::CODE_SEP);
                password.push_str(code);
            }
        }
        if let Target::Ssh { host, port, args, password, .. } = &mut t {
            let user = self.user.trim();
            *host = if user.is_empty() { self.host.clone() } else { format!("{user}@{}", self.host) };
            *port = self.port.trim().parse().ok().filter(|&p: &u16| p > 0);
            if !self.password.is_empty() {
                *password = Some(self.password.clone());
            }
            let key = self.key.trim();
            if !key.is_empty() {
                if let Some(i) = args.iter().position(|a| a == "-i") {
                    args.drain(i..(i + 2).min(args.len()));
                }
                args.extend(["-i".to_string(), key.to_string()]);
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_typed_reaches_the_connection_only() {
        assert!(refused("dev@build: Permission denied (publickey,password).") && !refused("Connection refused"));
        let t = Target::Ssh { host: "dev@build".into(), port: Some(2222), args: vec!["-i".into(), "old".into(), "-A".into()], remote: String::new(), capture: String::new(), password: None, unattended: false };
        let mut s = SignIn::new(7, &t, "Files", "Permission denied").unwrap();
        assert_eq!((s.host.as_str(), s.user.as_str(), s.port.as_str(), s.key.as_str()), ("build", "dev", "2222", "old"));
        s.password = "pw".into();
        s.key = "/k/new".into();
        let Target::Ssh { args, password, .. } = s.target(&t) else { panic!() };
        assert_eq!((args, password), (vec!["-A".to_string(), "-i".into(), "/k/new".into()], Some("pw".to_string())));
        // The user and port put right in the dialog go to the next try; left empty, ssh's own.
        s.user = "ops".into();
        s.port = "".into();
        let Target::Ssh { host, port, .. } = s.target(&t) else { panic!() };
        assert_eq!((host.as_str(), port), ("ops@build", None));
        s.user = " ".into();
        s.port = "2200".into();
        let Target::Ssh { host, port, .. } = s.target(&t) else { panic!() };
        assert_eq!((host.as_str(), port), ("build", Some(2200)));
        assert!(SignIn::new(1, &Target::Local { capture: String::new() }, "x", "").is_none());
    }

    /// A computer reached by its ID that refused the password is asked for it again, and the
    /// answer goes to that connection.
    #[cfg(feature = "remote")]
    #[test]
    fn a_wrong_id_password_is_asked_again() {
        let addr = crate::net::Address { id: "123456789".into(), relay: "relay.example".into() };
        let t = Target::Direct { addr, password: "old".into() };
        let mut s = SignIn::new(3, &t, "Screen sharing", "wrong password").unwrap();
        assert_eq!(s.host, "ID 123 456 789");
        assert_eq!(s.by_id.as_deref(), Some("sshremote://123456789@relay.example"));
        assert!(refused("wrong password"));
        s.password = "new".into();
        let Target::Direct { password, .. } = s.target(&t) else { panic!() };
        assert_eq!(password, "new");
    }

    /// The dialog shows the host, user, port and purpose before anything is typed.
    #[test]
    fn the_dialog_says_where_and_what_for() {
        use eframe::egui::{Pos2, Rect, Vec2};
        let t = Target::Ssh { host: "dev@build.example".into(), port: Some(2222), args: vec![], remote: String::new(), capture: String::new(), password: None, unattended: false };
        let mut s = SignIn::new(1, &t, "Screen sharing", "dev@build.example: Permission denied (publickey,password).").unwrap();
        let ctx = egui::Context::default();
        super::super::theme::style(&ctx);
        let size = Vec2::new(900.0, 600.0);
        let mut snap = super::super::snapshot::Snap::default();
        for k in 0..3 {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(k as f64 * 0.1), ..Default::default() };
            snap.frame(&ctx, input, |ui| {
                assert!(s.show(ui.ctx(), &t).is_none());
            });
        }
        for text in ["Sign in to build.example", "dev", "2222", "Screen sharing", "Permission denied", "Key file", "Remember the password"] {
            assert!(!snap.find(text).is_empty(), "{text} is shown");
        }
        if let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR") {
            snap.save(&ctx, size, &std::path::Path::new(&dir).join("sign_in.png"));
        }
    }
}
