//! Signing in when a host refused the login: which host, user, port and for what, then a
//! password, or a key file and its passphrase. What is typed is used for this connection only,
//! unless it is to be remembered in the system's password store.

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
            why: why.lines().find(|l| !l.trim().is_empty()).unwrap_or(why).trim().to_string(),
            password: String::new(),
            key,
            remember: false,
        })
    }

    /// The dialog; an answer once one of its buttons is used.
    pub fn show(&mut self, ctx: &egui::Context, target: &Target) -> Option<Answer> {
        let (modal, fit) = super::modal(ctx, "sign-in", egui::Vec2::new(460.0, 330.0));
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
                row(ui, "Host", &self.host);
                row(ui, "User", if self.user.is_empty() { "from ~/.ssh/config, or yours here" } else { &self.user });
                row(ui, "Port", if self.port.is_empty() { "22, or from ~/.ssh/config" } else { &self.port });
                row(ui, "For", &self.purpose);
            });
            ui.add_space(4.0);
            ui.label(RichText::new(&self.why).small().weak());
            ui.add_space(8.0);
            ui.label("Password, or the key's passphrase");
            let pw = ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(fit.x));
            if self.password.is_empty() && self.key.is_empty() {
                pw.request_focus();
            }
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
            ui.add_space(4.0);
            ui.checkbox(&mut self.remember, "Remember the password in the system's password store")
                .on_hover_text("Otherwise it is used for this connection only and never written anywhere");
            ui.add_space(8.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
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
        if let Some(Answer::Connect(Target::Ssh { host, .. })) = &answer
            && self.remember
            && !self.password.is_empty()
            && let Err(e) = crate::vault::set(host, &self.password)
        {
            self.why = format!("The password was not saved: {e}");
        }
        answer
    }

    /// The target with what was typed: the password (or passphrase) and the key.
    fn target(&self, t: &Target) -> Target {
        let mut t = t.clone();
        if let Target::Ssh { args, password, .. } = &mut t {
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
        assert!(SignIn::new(1, &Target::Local { capture: String::new() }, "x", "").is_none());
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
