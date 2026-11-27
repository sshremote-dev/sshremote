//! The team screens (docs/team.md, In the app): Settings > Team, where this computer joins a team
//! and the person signs in to one; the team's computers in the home window's list; the team's
//! name over This computer. What the window keeps of the team is on the egui context, so the
//! page, the home window and their pictures share it; every call to the server runs off the
//! window's thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;
use crate::computers::Computer;
use crate::gui::kit;
use crate::team::client::{self, Device, State};

/// Phosphor's users-three: a team.
pub(super) const ICON: &str = "\u{E68E}";

/// How often the team's computers are asked for again while they are shown.
const DEVICES_EVERY: Duration = Duration::from_secs(60);
/// How long a sign-in in the browser is waited for (the server keeps its nonce as long).
const SIGN_IN_FOR: Duration = Duration::from_secs(600);
/// What the server says of an account with an authenticator when no code was given.
const CODE_NEEDED: &str = n!("This account wants the code of its authenticator app too.");

type Slot<T> = Arc<Mutex<Option<T>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `work` on a thread of its own; its answer lands in the slot (an error if it panicked).
fn spawn<T: Send + 'static>(ctx: &egui::Context, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Slot<Result<T, String>> {
    let slot: Slot<Result<T, String>> = Arc::default();
    let (out, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err(n!("it stopped on an error").into()));
        *lock(&out) = Some(r);
        ctx.request_repaint();
    });
    slot
}

/// What is being asked of the team server.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Doing {
    Join,
    Leave,
    Browser,
    Password,
    SignOut,
    Upload,
    Download,
}

/// A call running, and its answer: words to show, or why it did not go.
struct Job {
    doing: Doing,
    done: Slot<Result<String, String>>,
    /// Set to let the browser sign-in go.
    stop: Arc<AtomicBool>,
}

impl Job {
    fn start(ctx: &egui::Context, doing: Doing, work: impl FnOnce(&AtomicBool) -> Result<String, String> + Send + 'static) -> Job {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        Job { doing, done: spawn(ctx, move || work(&s)), stop }
    }

    /// Its answer, once it has one.
    fn finished(job: &mut Option<Job>) -> Option<(Doing, Result<String, String>)> {
        let r = job.as_ref().and_then(|j| lock(&j.done).take())?;
        job.take().map(|j| (j.doing, r))
    }
}

/// What the window keeps of the team while it runs (the passphrase too: never written down).
#[derive(Default)]
pub(super) struct Shared {
    state: State,
    read: Option<Instant>,
    /// The state as a test set it, not read from the file (nor the computers asked for).
    held: bool,
    devices: Vec<Device>,
    /// Whose list it is (the server and the person), when it was asked for, and the answer coming.
    devices_of: String,
    devices_at: Option<Instant>,
    devices_coming: Option<(String, Slot<Result<Vec<Device>, String>>)>,
    devices_problem: Option<String>,
    passphrase: String,
    /// The team's logo: its data URL and what it made (None: it does not read).
    logo: Option<(String, Option<egui::TextureHandle>)>,
    /// What runs for this computer's card and for the person's, and what the last one said.
    computer: Option<Job>,
    person: Option<Job>,
    computer_said: Option<Result<String, String>>,
    person_said: Option<Result<String, String>>,
    /// The page the browser sign-in opened, to open again.
    sign_in_page: String,
    /// The calls ended since the settings page last looked, and whether each went.
    ended: Vec<(Doing, bool)>,
}

fn shared(ctx: &egui::Context) -> Arc<Mutex<Shared>> {
    ctx.data_mut(|d| d.get_temp_mut_or_insert_with(egui::Id::unique("team-shared"), || Arc::new(Mutex::new(Shared::default()))).clone())
}

impl Shared {
    /// The answers that came, and the state read again every two seconds (the service writes
    /// the policy, and the command line can join or sign in).
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some((doing, r)) = Job::finished(&mut self.computer) {
            self.ended.push((doing, r.is_ok()));
            self.computer_said = Some(r);
            self.read = None;
        }
        if let Some((doing, r)) = Job::finished(&mut self.person) {
            self.ended.push((doing, r.is_ok()));
            self.person_said = Some(r);
            self.read = None;
        }
        self.ended.truncate(8);
        if !self.held && self.read.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            self.state = client::state();
            self.read = Some(Instant::now());
        }
        if self.computer.is_some() || self.person.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    /// The team's computers the person may reach: asked for again every minute, and on demand.
    fn devices(&mut self, ctx: &egui::Context) {
        let Some(p) = self.state.person.as_ref() else {
            (self.devices, self.devices_of, self.devices_problem, self.devices_coming) = (Vec::new(), String::new(), None, None);
            return;
        };
        let whose = format!("{} {}", p.url, p.user["name"].as_str().unwrap_or(""));
        if whose != self.devices_of {
            (self.devices, self.devices_at, self.devices_problem) = (Vec::new(), None, None);
            self.devices_of = whose.clone();
        }
        if let Some((of, r)) = self.devices_coming.as_ref().and_then(|(of, s)| lock(s).take().map(|r| (of.clone(), r))) {
            self.devices_coming = None;
            if of == whose {
                match r {
                    Ok(d) => (self.devices, self.devices_problem) = (d, None),
                    Err(e) => self.devices_problem = Some(e),
                }
            }
            // A session the server ended signs the person out: read so at once.
            self.read = None;
        }
        if !self.held && self.devices_coming.is_none() && self.devices_at.is_none_or(|t| t.elapsed() >= DEVICES_EVERY) {
            self.devices_at = Some(Instant::now());
            self.devices_coming = Some((whose, spawn(ctx, client::devices)));
        }
        ctx.request_repaint_after(DEVICES_EVERY);
    }

    /// The team's logo, `height` high, if it has one that reads.
    fn logo(&mut self, ui: &mut egui::Ui, team: &Value, height: f32) -> bool {
        let Some(url) = team["logo"].as_str().filter(|u| u.starts_with("data:image/")) else { return false };
        if self.logo.as_ref().is_none_or(|(u, _)| u != url) {
            let tex = decode_logo(url).map(|img| ui.ctx().load_texture("team-logo", img, egui::TextureOptions::LINEAR));
            self.logo = Some((url.to_string(), tex));
        }
        let Some((_, Some(tex))) = &self.logo else { return false };
        let s = tex.size_vec2();
        let size = Vec2::new((s.x / s.y.max(1.0) * height).min(height * 4.0), height);
        ui.add(egui::Image::new(tex).fit_to_exact_size(size).corner_radius(3));
        true
    }
}

/// A logo as the team's settings keep it: a PNG or JPEG in a data URL, made small.
fn decode_logo(url: &str) -> Option<egui::ColorImage> {
    use base64::Engine;
    let (head, data) = url.split_once(',')?;
    if !head.ends_with(";base64") || data.len() > 2 << 20 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = if img.width() > 128 || img.height() > 128 { img.thumbnail(128, 128) } else { img }.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}

/// The team's name and logo (a data URL) as the app's own, when this computer is in a team that
/// asks for it (`brand` in its settings).
pub(super) fn team_brand(ctx: &egui::Context) -> Option<(String, String)> {
    let arc = shared(ctx);
    let s = lock(&arc);
    let j = s.state.joined.as_ref().filter(|j| j.team["brand"].as_bool() == Some(true))?;
    Some((team_name(&j.team, &j.url), j.team["logo"].as_str().unwrap_or("").to_string()))
}

/// The window's title and icon as the team's brand says (or the app's own again), sent when they
/// change.
pub(super) fn apply_brand(ctx: &egui::Context) {
    let now = team_brand(ctx);
    let id = egui::Id::unique("team-brand-applied");
    if ctx.data(|d| d.get_temp::<Option<(String, String)>>(id)) == Some(now.clone()) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(id, now.clone()));
    let (title, icon) = match &now {
        Some((name, logo)) => (name.clone(), decode_logo(logo).map(|img| egui::IconData { rgba: img.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect(), width: img.size[0] as u32, height: img.size[1] as u32 }).or_else(super::app_icon)),
        None => ("SSH Remote".to_string(), super::app_icon()),
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    if let Some(icon) = icon {
        ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(icon))));
    }
}

/// A team's name as shown: its own, else its server's.
fn team_name(team: &Value, url: &str) -> String {
    match team["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => url.split("://").nth(1).unwrap_or(url).to_string(),
    }
}

/// A person as shown: their full name, else their sign-in name.
fn person_name(user: &Value) -> String {
    [&user["display"], &user["name"]].iter().filter_map(|v| v.as_str()).map(str::trim).find(|n| !n.is_empty()).unwrap_or("").to_string()
}

/// A policy in one line: who comes in, whether the person here is asked, what anyone may do.
fn policy_words(p: &crate::team::Policy) -> String {
    let mut out: Vec<String> = vec![if p.password { t!("Passwords allowed").into() } else { t!("Team members only").into() }];
    match p.ask {
        Some(true) => out.push(t!("The person here is asked first").into()),
        Some(false) => out.push(t!("Nobody is asked").into()),
        None => {}
    }
    out.push(if p.rights & crate::grant::ALL == crate::grant::ALL { t!("Every right").into() } else { t!("At most: {}", crate::grant::shown(p.rights)) });
    if p.record == Some(true) {
        out.push(t!("Sessions are recorded").into());
    }
    if let Some(m) = p.idle.filter(|m| *m > 0) {
        out.push(t!("Idle sessions end after {} min", m));
    }
    if p.incoming_only {
        out.push(t!("Incoming only").into());
    }
    out.join("  \u{00b7}  ")
}

/// What a call said, under its card.
fn said(ui: &mut egui::Ui, said: &Option<Result<String, String>>) {
    let Some(r) = said else { return };
    let (glyph, text, color) = match r {
        Ok(t) => (ic::CHECK_CIRCLE, crate::lang::tr_msg(t), good()),
        Err(e) => (ic::WARNING, crate::lang::tr_msg(e), bad()),
    };
    // The client's messages start in lower case, to go inside others.
    let mut chars = text.chars();
    let text: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(glyph).color(color));
        ui.add(egui::Label::new(RichText::new(text).size(12.5).color(color)).wrap());
    });
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(ui.visuals().weak_text_color())).wrap());
}

/// A field's row of a form, as tall as the buttons.
fn field_row(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str, secret: bool) -> egui::Response {
    kit::row(ui, label, |ui| ui.add(kit::field(text, hint).password(secret)))
}

/// Settings > Team: what is typed there (the rest is kept while the window runs).
#[derive(Default)]
pub(super) struct Page {
    url: String,
    token: String,
    person_url: String,
    name: String,
    password: String,
    code: String,
    wants_code: bool,
    sure_to_leave: bool,
    /// The addresses were filled in from what is kept, once.
    filled: bool,
}

impl App {
    /// Settings > Team: this computer in a team, and the person signed in to one.
    pub(super) fn team_page(&mut self, ui: &mut egui::Ui, page: &mut Page) {
        let ctx = ui.ctx().clone();
        let access_on = self.access_status(&ctx).map(|(c, _)| c.enabled);
        let mut turn_on = false;
        // The team is kept with remote access, whose settings open to administrators on Windows.
        let mut restart = cfg!(windows) && !crate::admin::elevated();
        let admin_needed = restart;
        {
            let arc = shared(&ctx);
            let mut s = lock(&arc);
            s.poll(&ctx);
            for (doing, ok) in std::mem::take(&mut s.ended) {
                match doing {
                    Doing::Password if ok => (page.password, page.code, page.wants_code) = (String::new(), String::new(), false),
                    Doing::Password => page.wants_code |= s.person_said.as_ref().is_some_and(|r| r.as_ref().is_err_and(|e| e == CODE_NEEDED)),
                    Doing::Join if ok => page.token.clear(),
                    Doing::Leave => page.sure_to_leave = false,
                    _ => {}
                }
            }
            if !page.filled {
                page.filled = true;
                let (joined, person) = (s.state.joined.as_ref().map(|j| j.url.clone()), s.state.person.as_ref().map(|p| p.url.clone()));
                page.url = joined.clone().or(person.clone()).unwrap_or_default();
                page.person_url = person.or(joined).unwrap_or_default();
            }
            kit::heading(ui, t!("This computer"));
            computer_card(ui, &mut s, page, access_on, &mut turn_on, &mut restart);
            ui.add_space(kit::GAP);
            kit::heading(ui, t!("You"));
            person_card(ui, &mut s, page);
        }
        if admin_needed && !restart {
            self.restart_elevated(&ctx);
        }
        if turn_on && let Some((mut cfg, _)) = self.access_status(&ctx) {
            cfg.enabled = true;
            // Said here: the home window's banner is under the settings.
            let before = self.error.take();
            self.save_access(cfg);
            match self.error.take() {
                Some(e) => lock(&shared(&ctx)).computer_said = Some(Err(e)),
                None => self.error = before,
            }
        }
    }
}

/// This computer: joined with an enrollment token, or in a team with its policy, and leaving it.
/// `admin`: true while the window lacks the rights joining needs (Windows); set to false to
/// open it again as administrator.
fn computer_card(ui: &mut egui::Ui, s: &mut Shared, p: &mut Page, access_on: Option<bool>, turn_on: &mut bool, admin: &mut bool) {
    let ctx = ui.ctx().clone();
    kit::card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        if *admin {
            kit::notice(ui, kit::Tone::Info, t!("Its settings open to administrators"), t!("Joining a team or leaving it changes remote access, whose settings open to administrators."));
            if ui.add(theme::primary(format!("{}  {}", ic::SHIELD_CHECK, t!("Open as administrator")))).clicked() {
                *admin = false;
            }
        }
        let locked = *admin;
        let busy = s.computer.as_ref().map(|j| j.doing);
        match s.state.joined.clone() {
            None => {
                note(ui, t!("Join this computer to your organization's team with an enrollment token from the team's console. The team then decides who may reach it and what they may do there."));
                field_row(ui, t!("Team address"), &mut p.url, "team.example.com", false);
                field_row(ui, t!("Enrollment token"), &mut p.token, t!("From the team's console"), false);
                ui.horizontal(|ui| {
                    ui.add_space(kit::LABEL_W + ui.spacing().item_spacing.x);
                    if busy.is_some() {
                        kit::loading(ui, t!("Joining the team\u{2026}"));
                    } else if ui.add_enabled(!locked && !p.url.trim().is_empty() && !p.token.trim().is_empty(), theme::primary(t!("Join"))).clicked() {
                        let (url, token) = (p.url.trim().to_string(), p.token.trim().to_string());
                        s.computer_said = None;
                        s.computer = Some(Job::start(&ctx, Doing::Join, move |_| {
                            let team = client::join(&url, &token)?;
                            Ok(t!("This computer is in {} now.", team_name(&team, &client::tidy_url(&url))))
                        }));
                    }
                });
            }
            Some(j) => {
                ui.horizontal(|ui| {
                    if !s.logo(ui, &j.team, 32.0) {
                        kit::mark(ui, ICON, brand());
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add(egui::Label::new(RichText::new(team_name(&j.team, &j.url)).size(16.0).strong()).truncate());
                        ui.add(egui::Label::new(RichText::new(&j.url).size(12.0).weak()).truncate());
                    });
                });
                kit::row(ui, t!("ID"), |ui| ui.label(RichText::new(crate::computers::spaced(&j.id)).monospace()));
                kit::row(ui, t!("Policy"), |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add(egui::Label::new(RichText::new(policy_words(&j.policy)).size(12.5)).wrap());
                        let when = if j.policy_at == 0 { t!("Not read from the team server yet.").to_string() } else { t!("Read from the team server {}.", crate::hosts::ago(j.policy_at)) };
                        ui.label(RichText::new(when).size(11.5).weak());
                    })
                });
                ui.horizontal(|ui| {
                    if busy.is_some() {
                        kit::loading(ui, t!("Leaving the team\u{2026}"));
                    } else if p.sure_to_leave {
                        if ui.add(kit::danger(t!("Leave the team"), true)).clicked() {
                            s.computer_said = None;
                            s.computer = Some(Job::start(&ctx, Doing::Leave, |_| client::leave().map(|()| t!("This computer left the team.").to_string())));
                        }
                        if ui.add(kit::secondary(t!("Stay in it"))).clicked() {
                            p.sure_to_leave = false;
                        }
                    } else if ui.add_enabled(!locked, kit::danger(t!("Leave the team\u{2026}"), false)).clicked() {
                        p.sure_to_leave = true;
                    }
                });
                if p.sure_to_leave && busy.is_none() {
                    note(ui, t!("The team's people will not reach this computer through the team any more, and its policy no longer holds here. Joining again needs a new enrollment token."));
                }
            }
        }
        // The team reaches this computer by its ID.
        if access_on == Some(false) {
            kit::notice(ui, kit::Tone::Warn, t!("Remote access is off"), t!("The team reaches this computer by its remote access ID: remote access needs to be on for its people to come in."));
            if ui.add(kit::secondary(format!("{}  {}", ic::PLUGS, t!("Turn remote access on")))).clicked() {
                *turn_on = true;
            }
        }
        said(ui, &s.computer_said);
    });
}

/// The person: signed in (in the browser, or with a password here) or not, and their address book.
fn person_card(ui: &mut egui::Ui, s: &mut Shared, p: &mut Page) {
    let ctx = ui.ctx().clone();
    kit::card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        let busy = s.person.as_ref().map(|j| j.doing);
        let Some(person) = s.state.person.clone() else {
            note(ui, t!("Sign in to your organization's team to open the computers it lets you reach, with no password of theirs, and to keep your list of computers on its server."));
            field_row(ui, t!("Team address"), &mut p.person_url, "team.example.com", false);
            let url = p.person_url.trim().to_string();
            ui.horizontal(|ui| {
                ui.add_space(kit::LABEL_W + ui.spacing().item_spacing.x);
                match busy {
                    Some(Doing::Browser) => {
                        ui.add(egui::Spinner::new().size(14.0).color(ui.visuals().weak_text_color()));
                        ui.add(egui::Label::new(RichText::new(t!("Finish signing in in your browser\u{2026}")).size(12.5).weak()).wrap());
                    }
                    Some(_) => kit::loading(ui, t!("Signing in\u{2026}")),
                    None => {
                        let b = ui
                            .add_enabled(!url.is_empty(), theme::primary(format!("{}  {}", ic::ARROW_SQUARE_OUT, t!("Sign in"))))
                            .on_hover_text(t!("Opens the team's sign-in page in your browser, where your organization's single sign-on works"));
                        if b.clicked() {
                            let (page, nonce) = client::start_sign_in(&url);
                            crate::team::open_url(&page);
                            s.sign_in_page = page;
                            s.person_said = None;
                            let url = url.clone();
                            s.person = Some(Job::start(&ctx, Doing::Browser, move |stop| wait_for_sign_in(&url, &nonce, stop)));
                        }
                    }
                }
            });
            if busy == Some(Doing::Browser) {
                ui.horizontal(|ui| {
                    ui.add_space(kit::LABEL_W + ui.spacing().item_spacing.x);
                    if ui.add(kit::ghost(t!("Open the page again"))).on_hover_text(s.sign_in_page.clone()).clicked() {
                        crate::team::open_url(&s.sign_in_page);
                    }
                    if ui.add(kit::secondary(t!("Cancel"))).clicked()
                        && let Some(j) = s.person.take()
                    {
                        j.stop.store(true, Ordering::Release);
                    }
                });
            }
            ui.collapsing(t!("Sign in with a password here"), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                field_row(ui, t!("Name"), &mut p.name, t!("Your name on the team"), false);
                let mut enter = field_row(ui, t!("Password"), &mut p.password, "", true).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if p.wants_code {
                    enter |= field_row(ui, t!("Code"), &mut p.code, t!("From your authenticator app"), false).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                }
                ui.horizontal(|ui| {
                    ui.add_space(kit::LABEL_W + ui.spacing().item_spacing.x);
                    let ready = !url.is_empty() && !p.name.trim().is_empty() && !p.password.is_empty() && busy.is_none();
                    if ui.add_enabled(ready, kit::secondary(t!("Sign in"))).clicked() || (enter && ready) {
                        let (url, name, password, code) = (url.clone(), p.name.trim().to_string(), p.password.clone(), p.code.trim().to_string());
                        s.person_said = None;
                        s.person = Some(Job::start(&ctx, Doing::Password, move |_| {
                            match client::sign_in_password(&url, &name, &password, Some(code.as_str()).filter(|c| !c.is_empty())) {
                                Ok(user) => Ok(t!("Signed in as {}.", person_name(&user))),
                                Err(e) if e == "code needed" => Err(CODE_NEEDED.into()),
                                Err(e) => Err(e),
                            }
                        }));
                    }
                });
            });
            said(ui, &s.person_said);
            return;
        };
        ui.horizontal(|ui| {
            if !s.logo(ui, &person.team, 32.0) {
                kit::mark(ui, ic::USER, brand());
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add(egui::Label::new(RichText::new(person_name(&person.user)).size(16.0).strong()).truncate());
                let line = t!("Signed in to {} at {}", team_name(&person.team, &person.url), person.url);
                ui.add(egui::Label::new(RichText::new(line).size(12.0).weak()).truncate());
            });
        });
        note(ui, t!("The team's computers you may reach are in the home window's list."));
        ui.horizontal(|ui| {
            if busy == Some(Doing::SignOut) {
                kit::loading(ui, t!("Signing out\u{2026}"));
            } else if ui.add_enabled(busy.is_none(), kit::secondary(t!("Sign out"))).clicked() {
                s.person_said = None;
                s.person = Some(Job::start(&ctx, Doing::SignOut, |_| client::sign_out().map(|()| t!("Signed out of the team.").to_string())));
            }
            if person.user["admin"].as_bool() == Some(true) && ui.add(kit::ghost(format!("{}  {}", ic::ARROW_SQUARE_OUT, t!("Open the console")))).on_hover_text(person.url.clone()).clicked() {
                crate::team::open_url(&person.url);
            }
        });
        ui.separator();
        ui.label(RichText::new(t!("Address book on the team server")).strong());
        note(ui, t!("Your list of computers, sealed on this device with a passphrase only you know: the team server keeps what it cannot read. Saved passwords never leave this device."));
        let pass = kit::row(ui, t!("Passphrase"), |ui| ui.add(kit::field(&mut s.passphrase, t!("Your address book passphrase")).password(true)));
        pass.on_hover_text(t!("Kept only until SSH Remote closes, never saved. The same passphrase opens the address book on your other devices."));
        ui.horizontal(|ui| {
            ui.add_space(kit::LABEL_W + ui.spacing().item_spacing.x);
            let ready = !s.passphrase.is_empty() && busy.is_none();
            let up = ui
                .add_enabled(ready, kit::secondary(format!("{}  {}", ic::UPLOAD, t!("Upload my list"))))
                .on_hover_text(t!("Puts your computers in the address book: each one's address, name and tags. What the book has already stays."));
            let down = ui
                .add_enabled(ready, kit::secondary(format!("{}  {}", ic::DOWNLOAD, t!("Download to my list"))))
                .on_hover_text(t!("Adds the address book's computers to your list, and their names and tags where yours has none. Nothing in your list is removed."));
            if up.clicked() || down.clicked() {
                let pass = s.passphrase.clone();
                s.person_said = None;
                s.person = Some(if up.clicked() { Job::start(&ctx, Doing::Upload, move |_| upload(&pass)) } else { Job::start(&ctx, Doing::Download, move |_| download(&pass)) });
            }
        });
        match busy {
            Some(Doing::Upload) => kit::loading(ui, t!("Sealing and uploading your list\u{2026}")),
            Some(Doing::Download) => kit::loading(ui, t!("Downloading and opening the address book\u{2026}")),
            _ => {}
        }
        said(ui, &s.person_said);
    });
}

/// The browser sign-in, asked after every two seconds until it is done, given up, or ten minutes
/// have gone by. A blip of the network is tried past; the server's refusal is not.
fn wait_for_sign_in(url: &str, nonce: &str, stop: &AtomicBool) -> Result<String, String> {
    let until = Instant::now() + SIGN_IN_FOR;
    let mut failed = 0;
    while Instant::now() < until {
        for _ in 0..20 {
            if stop.load(Ordering::Acquire) {
                return Err(n!("The sign-in was cancelled.").into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        match client::poll_sign_in(url, nonce) {
            Ok(Some(user)) => return Ok(t!("Signed in as {}.", person_name(&user))),
            Ok(None) => failed = 0,
            Err(e) => {
                failed += 1;
                if failed >= 3 {
                    return Err(e);
                }
            }
        }
    }
    Err(n!("The sign-in was not finished in 10 minutes.").into())
}

/// The computers of an address book, as the list keeps them (address, name and tags only).
fn book_computers(book: &Value) -> Vec<Computer> {
    let mut out: Vec<Computer> = Vec::new();
    for c in book["computers"].as_array().into_iter().flatten().take(10_000) {
        let Some(address) = c["address"].as_str().map(|a| crate::computers::canonical(a.trim())).filter(|a| !a.is_empty() && a.len() <= 512) else { continue };
        if out.iter().any(|o| o.address == address) {
            continue;
        }
        let name: String = c["name"].as_str().unwrap_or("").trim().chars().take(200).collect();
        let tags: Vec<&str> = c["tags"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        out.push(Computer { address, name, tags: crate::computers::tags(&tags.join(",")), ..Default::default() });
    }
    out
}

/// Tags of both, each once.
fn both_tags(a: &[String], b: &[String]) -> Vec<String> {
    crate::computers::tags(&a.iter().chain(b).cloned().collect::<Vec<_>>().join(","))
}

/// The address book with `list` put in it: each of those by its address, its name and tags as
/// the list has them (a name it lacks kept from the book); the book's others stay.
fn book_with(book: Option<&Value>, list: &[Computer]) -> Value {
    let mut out = book.map(book_computers).unwrap_or_default();
    for c in list {
        match out.iter_mut().find(|o| o.address == c.address) {
            Some(o) => {
                if !c.name.is_empty() {
                    o.name = c.name.clone();
                }
                o.tags = both_tags(&c.tags, &o.tags);
            }
            None => out.push(Computer { address: c.address.clone(), name: c.name.clone(), tags: c.tags.clone(), ..Default::default() }),
        }
    }
    json!({"computers": out.iter().map(|c| json!({"address": c.address, "name": c.name, "tags": c.tags})).collect::<Vec<_>>()})
}

/// What taking in a book changes in `list`: the computers it adds, and those of the list it gives
/// a name or tags to (whole, as kept, for the list to take). Nothing is removed; no password moves.
fn taken_in(book: &Value, list: &[Computer]) -> (Vec<Computer>, usize) {
    let mut changed = Vec::new();
    let mut added = 0;
    for b in book_computers(book) {
        match list.iter().find(|c| c.address == b.address) {
            Some(c) => {
                let mut k = c.clone();
                if k.name.is_empty() {
                    k.name = b.name.clone();
                }
                k.tags = both_tags(&k.tags, &b.tags);
                if k != *c {
                    changed.push(k);
                }
            }
            None => {
                added += 1;
                changed.push(b);
            }
        }
    }
    (changed, added)
}

/// Puts this list in the person's address book on the server; the book is opened first, so a
/// mistyped passphrase does not seal a second book over the first.
fn upload(passphrase: &str) -> Result<String, String> {
    let book = client::book_get(passphrase)?;
    let merged = book_with(book.as_ref(), &crate::computers::all());
    client::book_put(&merged, passphrase)?;
    let n = merged["computers"].as_array().map_or(0, Vec::len);
    Ok(if n == 1 { t!("The address book on the team server has 1 computer now.").into() } else { t!("The address book on the team server has {} computers now.", n) })
}

/// Takes the person's address book into this list.
fn download(passphrase: &str) -> Result<String, String> {
    let Some(book) = client::book_get(passphrase)? else { return Ok(t!("The address book on the team server is empty: upload your list first.").into()) };
    let (changed, added) = taken_in(&book, &crate::computers::all());
    crate::computers::merge(changed, true);
    Ok(match added {
        0 => t!("Your list already had every computer of the address book.").into(),
        1 => t!("1 computer was added to your list from the address book.").into(),
        n => t!("{} computers were added to your list from the address book.", n),
    })
}

// ---------------------------------------------------------------------------------------------
// The home window.

/// The team's computers as the home list shows them, while the person is signed in.
pub(super) struct Listed {
    pub(super) devices: Vec<Device>,
    /// Asked for and not answered yet, with nothing to show meanwhile.
    pub(super) coming: bool,
    pub(super) problem: Option<String>,
}

pub(super) fn listed(ctx: &egui::Context) -> Option<Listed> {
    let arc = shared(ctx);
    let mut s = lock(&arc);
    s.poll(ctx);
    s.devices(ctx);
    s.state.person.as_ref()?;
    Some(Listed { devices: s.devices.clone(), coming: s.devices_coming.is_some() && s.devices.is_empty(), problem: s.devices_problem.clone() })
}

/// The team's computers asked for again now.
pub(super) fn refresh(ctx: &egui::Context) {
    lock(&shared(ctx)).devices_at = None;
}

/// Whether `d` is found by the words `q` (lower case): its name, its group, its ID's digits.
pub(super) fn finds(d: &Device, q: &str) -> bool {
    let digits: String = q.chars().filter(|c| !c.is_whitespace()).collect();
    d.name.to_lowercase().contains(q) || d.group.to_lowercase().contains(q) || (!digits.is_empty() && d.id.contains(&digits))
}

/// Over This computer: the team that manages it, or the person's.
pub(super) fn branding(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let arc = shared(&ctx);
    let mut s = lock(&arc);
    s.poll(&ctx);
    let (team, text) = match (s.state.joined.clone(), s.state.person.clone()) {
        (Some(j), _) => (j.team.clone(), t!("Managed by {}", team_name(&j.team, &j.url))),
        (None, Some(p)) => (p.team.clone(), t!("Team: {}", team_name(&p.team, &p.url))),
        (None, None) => return,
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if !s.logo(ui, &team, 18.0) {
            ui.label(RichText::new(ICON).size(15.0).color(brand()));
        }
        ui.add(egui::Label::new(RichText::new(text).size(12.5).strong().color(brand())).truncate());
    });
    ui.add_space(4.0);
}

/// What opens a team's computer: by its ID through the team's relay (else this computer's), a
/// fresh ticket from the team server in place of a password at each connection.
pub(super) fn launch(id: &str, name: &str, mode: PaneMode) -> Result<Launch, String> {
    let relay = client::state().person.and_then(|p| p.team["relay"].as_str().map(str::trim).filter(|r| !r.is_empty()).map(str::to_string)).unwrap_or_else(crate::access::relay);
    let addr = crate::net::Address::parse(&crate::access::address(id, &relay)).ok_or_else(|| t!("The team gave an ID that cannot be reached: {}", id))?;
    let label = if name.trim().is_empty() { t!("ID {}", crate::computers::spaced(id)) } else { name.trim().to_string() };
    Ok(Launch::Agent { target: Target::Direct { addr, password: crate::net::TEAM_MARK.to_string() }, label, ssh_cmd: None, mode })
}

/// For pictures: the team as given, not read from the settings folder or asked of a server.
#[cfg(test)]
pub(super) fn hold(ctx: &egui::Context, state: State, devices: Vec<Device>) {
    let arc = shared(ctx);
    let mut s = lock(&arc);
    s.devices_of = state.person.as_ref().map(|p| format!("{} {}", p.url, p.user["name"].as_str().unwrap_or(""))).unwrap_or_default();
    (s.state, s.held, s.read) = (state, true, Some(Instant::now()));
    (s.devices, s.devices_at) = (devices, Some(Instant::now()));
}

/// A made-up team for pictures: this computer in it, a person signed in, three of its computers.
#[cfg(test)]
pub(super) fn sample() -> (State, Vec<Device>) {
    use base64::Engine;
    let logo = image::RgbaImage::from_fn(64, 64, |x, y| if (x / 16 + y / 16) % 2 == 0 { image::Rgba([37, 99, 235, 255]) } else { image::Rgba([16, 185, 129, 255]) });
    let mut png = Vec::new();
    logo.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let team = json!({"name": "Northwind Traders", "logo": format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png)), "relay": "", "url": "https://team.northwind.example"});
    let policy = crate::team::Policy { password: false, ask: Some(true), rights: crate::grant::CONTROL | crate::grant::CLIPBOARD | crate::grant::FILES, ..Default::default() };
    let state = State {
        joined: Some(client::Joined { url: "https://team.northwind.example".into(), id: "482913075".into(), key: vec![0; 32], team: team.clone(), policy, policy_at: crate::sys::now_secs() - 240 }),
        person: Some(client::SignedIn {
            url: "https://team.northwind.example".into(),
            token: String::new(),
            user: json!({"name": "alex", "display": "Alex Morgan", "admin": true}),
            team,
            expires: 0,
        }),
    };
    let device = |id: &str, name: &str, group: &str, online: bool| Device { id: id.into(), name: name.into(), os: "linux".into(), group: group.into(), online, rights: crate::grant::ALL, ask: false };
    let devices = vec![device("731004522", "Reception PC", "Front desk", true), device("118204773", "Build server", "Servers", true), device("905561208", "Warehouse laptop", "", false)];
    (state, devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_book_takes_the_list_in_and_keeps_its_others() {
        let c = |a: &str, n: &str, t: &[&str]| Computer { address: a.into(), name: n.into(), tags: t.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        let book = json!({"computers": [{"address": "dev@build", "name": "Build", "tags": ["work"]}, {"address": "web1", "name": "", "tags": []}]});
        let list = vec![c("dev@build", "", &["servers"]), c("sshremote://123456789@relay.sshremote.dev", "Mum", &[])];
        let up = book_with(Some(&book), &list);
        let got = book_computers(&up);
        assert_eq!(got.len(), 3, "the book's others stay: {up}");
        assert_eq!(got[0].name, "Build", "a name the list lacks is kept");
        assert_eq!(got[0].tags, vec!["servers", "work"]);
        assert_eq!(got[2].name, "Mum");
        // Back down: added, named where the list had no name, never removed.
        let (changed, added) = taken_in(&book, &list);
        assert_eq!(added, 1);
        assert!(changed.iter().any(|k| k.address == "dev@build" && k.name == "Build" && k.tags == vec!["servers", "work"]));
        assert!(changed.iter().any(|k| k.address == "web1"));
        assert!(!changed.iter().any(|k| k.address.starts_with("sshremote://")), "a computer the book has not is left alone");
        // Only addresses, names and tags go up.
        let mut with_more = list.clone();
        with_more[0].mac = "aa:bb:cc:dd:ee:ff".into();
        assert!(!book_with(None, &with_more).to_string().contains("aa:bb"));
    }

    #[test]
    fn a_logo_reads_from_its_data_url() {
        let (state, _) = sample();
        let url = state.joined.unwrap().team["logo"].as_str().unwrap().to_string();
        let img = decode_logo(&url).expect("the sample logo reads");
        assert_eq!(img.size, [64, 64]);
        assert!(decode_logo("data:image/png;base64,AAAA").is_none());
    }

    #[test]
    fn a_policy_reads_in_one_line() {
        let p = crate::team::Policy { password: false, ask: Some(true), rights: crate::grant::FILES, idle: Some(30), ..Default::default() };
        let w = policy_words(&p);
        assert!(w.starts_with("Team members only") && w.contains("asked first") && w.contains("files") && w.contains("30 min"), "{w}");
        assert!(policy_words(&crate::team::Policy::default()).contains("Every right"));
    }
}

