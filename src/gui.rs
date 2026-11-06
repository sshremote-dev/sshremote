//! Window viewer: a connect screen, then the remote desktop with a floating
//! toolbar, full screen, and every key and mouse event passed through.

use std::collections::BTreeSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, Event, FontId, Key, Modifiers, Pos2, Rect, RichText, Stroke,
    TextureHandle, TextureOptions, Vec2, ViewportCommand,
};

use crate::clip::Clip;
use crate::color::Mode;
use crate::proto::{MOD_SUPER, PRESETS, SUPER_L, ToAgent, ToClient};
use crate::session::{Action, Control, Ev, Framebuffer, Options, REMOTE, Session};
use crate::transport::Target;

const ACCENT: Color32 = Color32::from_rgb(64, 132, 245);
const PANEL: Color32 = Color32::from_rgb(28, 30, 36);
const SHIFT_L: u32 = 0xffe1;
const CONTROL_L: u32 = 0xffe3;
const ALT_L: u32 = 0xffe9;

/// Keysym for a key by its position (US layout names), so the remote layout decides the character.
fn keysym(k: Key) -> Option<u32> {
    use Key::*;
    let ch = |c: char| Some(c as u32);
    match k {
        ArrowLeft => Some(0xff51),
        ArrowUp => Some(0xff52),
        ArrowRight => Some(0xff53),
        ArrowDown => Some(0xff54),
        Escape => Some(0xff1b),
        Tab => Some(0xff09),
        Backspace => Some(0xff08),
        Enter => Some(0xff0d),
        Space => ch(' '),
        Insert => Some(0xff63),
        Delete => Some(0xffff),
        Home => Some(0xff50),
        End => Some(0xff57),
        PageUp => Some(0xff55),
        PageDown => Some(0xff56),
        Comma => ch(','),
        Backslash => ch('\\'),
        Slash => ch('/'),
        OpenBracket => ch('['),
        CloseBracket => ch(']'),
        Backtick => ch('`'),
        Minus => ch('-'),
        Period => ch('.'),
        Equals => ch('='),
        Semicolon => ch(';'),
        Quote => ch('\''),
        _ => {
            let name = k.name();
            let mut it = name.chars();
            match (it.next(), it.next()) {
                (Some(c @ ('A'..='Z' | '0'..='9')), None) => ch(c.to_ascii_lowercase()),
                (Some('F'), Some(_)) => {
                    name[1..].parse::<u32>().ok().filter(|n| (1..=12).contains(n)).map(|n| 0xffbd + n)
                }
                _ => None,
            }
        }
    }
}

fn config_dir() -> Option<std::path::PathBuf> {
    let base: Option<std::path::PathBuf> = if cfg!(windows) {
        std::env::var_os("APPDATA").map(Into::into)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(Into::into)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
    };
    base.map(|b| b.join("desk"))
}

/// Recent connections as `host port` lines; passwords are never stored.
fn load_recent() -> Vec<(String, String)> {
    let Some(p) = config_dir().map(|d| d.join("recent.txt")) else { return Vec::new() };
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
        })
        .collect()
}

fn save_recent(list: &[(String, String)]) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let text: String = list.iter().map(|(h, p)| format!("{h} {p}\n")).collect();
        let _ = std::fs::write(d.join("recent.txt"), text);
    }
}

#[derive(Clone)]
struct Form {
    host: String,
    port: String,
    password: String,
    preset: Option<usize>,
    remote: String,
    capture: String,
}

type Pending = mpsc::Receiver<Result<(Session, mpsc::Receiver<Ev>), String>>;

enum Screen {
    Connect,
    Connecting { started: Instant, rx: Pending, label: String },
    Remote(Box<Remote>),
}

#[derive(PartialEq, Clone, Copy)]
enum Fit {
    /// Remote capture sized to the window.
    Window,
    /// One remote pixel per screen pixel.
    Original,
}

type Deferred = Box<dyn FnOnce(&mut Remote, &egui::Context) -> Result<(), String>>;

struct Remote {
    sess: Session,
    rx: mpsc::Receiver<Ev>,
    ctl: Control,
    fb: Framebuffer,
    tex: Option<TextureHandle>,
    label: String,
    fit: Fit,
    fullscreen: bool,
    pinned: bool,
    clip: Clip,
    share_clipboard: bool,
    held: BTreeSet<u32>,
    mods: Modifiers,
    pointer: (u16, u16),
    view: (u16, u16),
    view_changed: Option<Instant>,
    win_next: bool,
    note: Option<(String, Instant)>,
    grab_keys: bool,
    uploads: std::collections::VecDeque<Upload>,
    next_upload: u32,
    sound: Option<crate::audio::Player>,
    monitors: Vec<crate::proto::MonitorInfo>,
    monitor: usize,
    inbox: crate::files::Inbox,
    /// Downloads in progress: id and name.
    downloads: Vec<(u32, String)>,
    panel: Option<Panel>,
}

/// The remote file browser.
struct Panel {
    path: String,
    typed: String,
    entries: Vec<crate::proto::DirEntry>,
    error: String,
    selected: Option<usize>,
    hidden: bool,
}

impl Panel {
    fn sep(&self) -> char {
        if self.path.contains('\\') && !self.path.contains('/') { '\\' } else { '/' }
    }

    fn child(&self, name: &str) -> String {
        let sep = self.sep();
        if self.path.ends_with(sep) { format!("{}{name}", self.path) } else { format!("{}{sep}{name}", self.path) }
    }

    fn parent(&self) -> String {
        let sep = self.sep();
        let trimmed = self.path.trim_end_matches(sep);
        match trimmed.rfind(sep) {
            Some(0) => sep.to_string(),
            // "C:" needs its separator to mean the drive root.
            Some(i) if trimmed[..i].ends_with(':') => format!("{}{sep}", &trimmed[..i]),
            Some(i) => trimmed[..i].to_string(),
            None => self.path.clone(),
        }
    }
}

/// A file being sent to the remote Downloads folder.
struct Upload {
    id: u32,
    name: String,
    file: std::fs::File,
    size: u64,
    sent: u64,
    acked: u64,
    ended: bool,
}

/// Unacknowledged upload bytes; more would only queue in ssh and delay the screen.
const UPLOAD_WINDOW: u64 = 1 << 20;
const UPLOAD_CHUNK: usize = 64 << 10;

pub struct App {
    screen: Screen,
    form: Form,
    recent: Vec<(String, String)>,
    error: Option<String>,
    budget: f64,
    /// System keys taken from Windows while a session is in front.
    grabbed: Option<mpsc::Receiver<(u32, bool)>>,
}

pub fn run(opt: &Options) -> Result<(), String> {
    #[cfg(windows)]
    if std::env::var_os("WGPU_BACKEND").is_none() {
        // Direct3D 12 works on every Windows 10/11 GPU, including drivers whose Vulkan or OpenGL is broken.
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
    }
    let form = Form {
        host: opt.host.clone().unwrap_or_default(),
        port: String::new(),
        password: String::new(),
        preset: opt.preset,
        remote: if opt.remote.is_empty() { REMOTE.into() } else { opt.remote.clone() },
        capture: opt.capture.clone(),
    };
    let auto_start = opt.host.is_some() || opt.local;
    let local = opt.local;
    let budget = opt.budget;
    let mut native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("desk")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 360.0]),
        ..Default::default()
    };
    // Some drivers block a vsynced present for a quarter second; frames are paced by the agent anyway.
    native.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    eframe::run_native(
        "desk",
        native,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            #[cfg(windows)]
            let grabbed = crate::keygrab::install();
            #[cfg(not(windows))]
            let grabbed = None;
            let mut app = App { screen: Screen::Connect, form, recent: load_recent(), error: None, budget, grabbed };
            if auto_start {
                app.connect(&cc.egui_ctx, local);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(18, 19, 23);
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(12, 13, 16);
    v.selection.bg_fill = ACCENT;
    v.widgets.hovered.bg_fill = Color32::from_rgb(48, 52, 62);
    v.widgets.inactive.corner_radius = CornerRadius::same(8);
    v.widgets.hovered.corner_radius = CornerRadius::same(8);
    v.widgets.active.corner_radius = CornerRadius::same(8);
    v.window_corner_radius = CornerRadius::same(12);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(10.0, 10.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
    });
}

impl App {
    fn connect(&mut self, ctx: &egui::Context, local: bool) {
        let f = self.form.clone();
        let target = if local {
            Target::Local { capture: if f.capture == "auto" { "synthetic".into() } else { f.capture.clone() } }
        } else {
            let host = f.host.trim().to_string();
            if host.is_empty() {
                self.error = Some("Enter a host, for example user@192.168.1.20".into());
                return;
            }
            if let Some(addr) = crate::net::Address::parse(&host) {
                if f.password.is_empty() {
                    self.error = Some("Enter the one-time password shown by desk --serve".into());
                    return;
                }
                let target = Target::Direct { addr, password: f.password.clone() };
                return self.start(ctx, target, host);
            }
            Target::Ssh {
                host,
                port: f.port.trim().parse().ok(),
                args: Vec::new(),
                remote: f.remote.clone(),
                capture: f.capture.clone(),
                password: (!f.password.is_empty()).then(|| f.password.clone()),
                unattended: true,
            }
        };
        let label = if local { "local test desktop".to_string() } else { f.host.trim().to_string() };
        self.start(ctx, target, label);
    }

    fn start(&mut self, ctx: &egui::Context, target: Target, label: String) {
        let f = self.form.clone();
        let local = matches!(target, Target::Local { .. });
        if !local {
            self.recent.retain(|(h, _)| h != f.host.trim());
            self.recent.insert(0, (f.host.trim().to_string(), f.port.trim().to_string()));
            self.recent.truncate(8);
            save_recent(&self.recent);
        }
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = Session::open(&target, true).map(|(s, _tx, rx)| (s, rx));
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.error = None;
        self.form.password.clear();
        self.screen = Screen::Connecting { started: Instant::now(), rx, label };
    }

    fn options(&self) -> Options {
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
        }
    }

    fn connect_screen(&mut self, ui: &mut egui::Ui) {
        let mut go: Option<bool> = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.10).max(16.0));
                ui.label(RichText::new("desk").size(40.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("Remote desktop over SSH").size(15.0).color(Color32::GRAY));
                ui.add_space(20.0);
                egui::Frame::new().fill(PANEL).corner_radius(CornerRadius::same(14)).inner_margin(24.0).show(
                    ui,
                    |ui| {
                        ui.set_width(420.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.label(RichText::new("Host").color(Color32::GRAY));
                            let host = ui.add(
                                egui::TextEdit::singleline(&mut self.form.host)
                                    .hint_text("user@host  or  desk://host  or  desk://ID@relay")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new("Port").color(Color32::GRAY));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.form.port)
                                            .hint_text("22")
                                            .desired_width(80.0),
                                    );
                                });
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new("Password (blank for SSH keys; desk:// needs one)")
                                            .color(Color32::GRAY),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.form.password)
                                            .password(true)
                                            .desired_width(f32::INFINITY),
                                    );
                                });
                            });
                            ui.label(RichText::new("Quality").color(Color32::GRAY));
                            ui.horizontal_wrapped(|ui| {
                                ui.selectable_value(&mut self.form.preset, None, "Auto");
                                for (i, p) in PRESETS.iter().enumerate() {
                                    let name = p.name.split_once(' ').map_or(p.name, |(_, n)| n);
                                    ui.selectable_value(&mut self.form.preset, Some(i), name);
                                }
                            });
                            ui.collapsing("Advanced", |ui| {
                                ui.label(RichText::new("Agent command on the host").color(Color32::GRAY));
                                ui.add(egui::TextEdit::singleline(&mut self.form.remote).desired_width(f32::INFINITY));
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Capture").color(Color32::GRAY));
                                    for c in ["auto", "gnome", "windows", "synthetic"] {
                                        ui.selectable_value(&mut self.form.capture, c.to_string(), c);
                                    }
                                });
                            });
                            ui.add_space(6.0);
                            let enter = host.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            let button = egui::Button::new(RichText::new("Connect").size(16.0).color(Color32::WHITE))
                                .fill(ACCENT)
                                .min_size(Vec2::new(ui.available_width(), 38.0));
                            if ui.add(button).clicked() || enter {
                                go = Some(false);
                            }
                            if ui.link("Try the built-in test desktop").clicked() {
                                go = Some(true);
                            }
                            if let Some(e) = &self.error {
                                ui.colored_label(Color32::from_rgb(240, 110, 100), e);
                            }
                        });
                    },
                );
                if !self.recent.is_empty() {
                    ui.add_space(16.0);
                    ui.label(RichText::new("Recent").color(Color32::GRAY));
                    let mut pick = None;
                    for (h, p) in &self.recent {
                        let text = if p.is_empty() { h.clone() } else { format!("{h}  :{p}") };
                        let r = ui.add(egui::Button::new(text).min_size(Vec2::new(468.0, 30.0)));
                        if r.clicked() {
                            pick = Some((h.clone(), p.clone(), false));
                        }
                        if r.double_clicked() {
                            pick = Some((h.clone(), p.clone(), true));
                        }
                    }
                    if let Some((h, p, now)) = pick {
                        self.form.host = h;
                        self.form.port = p;
                        if now {
                            go = Some(false);
                        }
                    }
                }
            });
        });
        if let Some(local) = go {
            let ctx = ui.ctx().clone();
            self.connect(&ctx, local);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut next = None;
        let opt = self.options();
        match &mut self.screen {
            Screen::Connect => {}
            Screen::Connecting { started, rx, label } => match rx.try_recv() {
                Ok(Ok((sess, rx))) => {
                    let ctl = Control::new(&opt, Mode::True);
                    let waker = ctx.clone();
                    sess.on_message(move || waker.request_repaint());
                    let desk = sess.desk;
                    ctx.send_viewport_cmd(ViewportCommand::Title(format!("desk - {label}")));
                    next = Some(Screen::Remote(Box::new(Remote {
                        label: format!("{label}  {}x{}  {}", desk.0, desk.1, sess.backend),
                        sess,
                        rx,
                        ctl,
                        fb: Framebuffer::default(),
                        tex: None,
                        fit: Fit::Window,
                        fullscreen: false,
                        pinned: true,
                        clip: Clip::new(),
                        share_clipboard: true,
                        held: BTreeSet::new(),
                        mods: Modifiers::NONE,
                        pointer: (u16::MAX, u16::MAX),
                        view: (0, 0),
                        view_changed: None,
                        win_next: false,
                        note: None,
                        grab_keys: true,
                        uploads: std::collections::VecDeque::new(),
                        next_upload: 1,
                        sound: None,
                        monitors: Vec::new(),
                        monitor: 0,
                        inbox: crate::files::Inbox::default(),
                        downloads: Vec::new(),
                        panel: None,
                    })));
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    next = Some(Screen::Connect);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.error = Some("connection failed".into());
                    next = Some(Screen::Connect);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    let secs = started.elapsed().as_secs();
                    let label = label.clone();
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.4);
                            ui.spinner();
                            ui.label(RichText::new(format!("Connecting to {label}...  {secs} s")).size(16.0));
                            if ui.button("Cancel").clicked() {
                                next = Some(Screen::Connect);
                            }
                        });
                    });
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
            },
            Screen::Remote(r) => {
                #[cfg(windows)]
                crate::keygrab::set_active(r.grab_keys && ctx.input(|i| i.focused));
                let grabbed: Vec<(u32, bool)> =
                    self.grabbed.as_ref().map(|g| g.try_iter().collect()).unwrap_or_default();
                let result = grabbed
                    .into_iter()
                    .try_for_each(|(keysym, down)| r.send(ToAgent::KeyState { keysym, down }))
                    .and_then(|_| r.frame(ui));
                if let Err(e) = result {
                    r.sess.close();
                    if r.fullscreen {
                        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Title("desk".into()));
                    self.error = (e != "disconnected").then_some(e);
                    next = Some(Screen::Connect);
                }
            }
        }
        if let Some(s) = next {
            self.screen = s;
        }
        #[cfg(windows)]
        if !matches!(self.screen, Screen::Remote(_)) {
            crate::keygrab::set_active(false);
        }
        if matches!(self.screen, Screen::Connect) {
            self.connect_screen(ui);
        }
    }
}

impl Remote {
    fn send(&mut self, m: ToAgent) -> Result<(), String> {
        self.sess.send(m).map_err(|e| format!("connection lost: {e}"))
    }

    fn notify(&mut self, text: &str) {
        self.note = Some((text.to_string(), Instant::now()));
    }

    /// One UI pass: network, input, picture and toolbar. An error ends the session.
    fn frame(&mut self, ui: &mut egui::Ui) -> Result<(), String> {
        let ctx = ui.ctx().clone();
        self.pump(&ctx)?;
        if self.share_clipboard
            && let Some(text) = self.clip.poll()
        {
            self.send(ToAgent::Clipboard(text))?;
        }
        self.ctl.tick(&mut self.sess).map_err(|e| e.to_string())?;
        self.take_drops(&ctx)?;
        self.feed_uploads()?;
        let screen = ui.max_rect();
        let ppp = ctx.pixels_per_point();
        // A pinned toolbar gets its own strip instead of covering the top of the remote screen.
        let area = if self.pinned { screen.with_min_y(screen.top() + 52.0) } else { screen };
        ui.painter().rect_filled(screen, CornerRadius::ZERO, Color32::from_rgb(18, 19, 23));
        ui.painter().rect_filled(area, CornerRadius::ZERO, Color32::BLACK);
        self.resize(area.size() * ppp)?;
        let shown = self.picture(ui, area, ppp);
        let toolbar_rect = self.toolbar(&ctx, screen)?;
        let panel_rect = self.file_panel(&ctx)?;
        self.upload_overlay(&ctx, screen);
        let ui_rects: Vec<Rect> = toolbar_rect.into_iter().chain(panel_rect).collect();
        self.input(&ctx, shown, &ui_rects)?;
        if ctx.input(|i| i.key_pressed(Key::F11)) {
            self.set_fullscreen(&ctx, !self.fullscreen);
        }
        // Network messages wake the window themselves; this only keeps statistics and pings going.
        ctx.request_repaint_after(Duration::from_millis(100));
        Ok(())
    }

    fn pump(&mut self, ctx: &egui::Context) -> Result<(), String> {
        loop {
            match self.rx.try_recv() {
                Ok(Ev::Net(ToClient::Frame(f))) => {
                    let resized = self.fb.apply(&f).map_err(|e| e.to_string())?;
                    self.upload(ctx, resized, &f);
                    self.ctl.frame_done(f.covered() * 2 >= self.fb.w * self.fb.h);
                    self.send(ToAgent::Ack(f.id))?;
                }
                Ok(Ev::Net(ToClient::Pong(t))) => self.ctl.pong(t),
                Ok(Ev::Net(ToClient::Audio(p))) => {
                    if let Some(pl) = self.sound.as_mut() {
                        pl.push(&p);
                    }
                }
                Ok(Ev::Net(ToClient::Notice(t))) => self.notify(&t),
                Ok(Ev::Net(ToClient::Monitors { list, current })) => {
                    (self.monitors, self.monitor) = (list, current as usize)
                }
                Ok(Ev::Net(ToClient::Hello { w, h, backend })) => {
                    // Another monitor: new size, and the capture must be fitted again.
                    self.sess.desk = (w, h);
                    let host = self.label.split("  ").next().unwrap_or("").to_string();
                    self.label = format!("{host}  {w}x{h}  {backend}");
                    self.view = (0, 0);
                }
                Ok(Ev::Net(ToClient::Clipboard(t))) => {
                    if self.share_clipboard {
                        self.clip.set(t);
                    }
                }
                Ok(Ev::Net(ToClient::FileProgress { id, received })) => {
                    if let Some(u) = self.uploads.iter_mut().find(|u| u.id == id) {
                        u.acked = received;
                    }
                }
                Ok(Ev::Net(ToClient::DirList { path, entries, error })) => {
                    let p = self.panel.get_or_insert_with(|| Panel {
                        path: String::new(),
                        typed: String::new(),
                        entries: Vec::new(),
                        error: String::new(),
                        selected: None,
                        hidden: false,
                    });
                    (p.typed, p.path, p.entries, p.error, p.selected) = (path.clone(), path, entries, error, None);
                }
                Ok(Ev::Net(ToClient::FetchStart { id, size, name })) => {
                    if let Some(crate::files::Note::Done { text, .. }) = self.inbox.start(id, size, &name) {
                        self.notify(&format!("{name}: {text}"));
                    } else {
                        self.downloads.push((id, name));
                    }
                }
                Ok(Ev::Net(ToClient::FetchData { id, data })) => match self.inbox.data(id, &data) {
                    Some(crate::files::Note::Progress { id, received }) => {
                        self.send(ToAgent::FetchAck { id, received })?
                    }
                    Some(crate::files::Note::Done { text, .. }) => {
                        self.downloads.retain(|d| d.0 != id);
                        self.notify(&text);
                    }
                    None => {}
                },
                Ok(Ev::Net(ToClient::FetchEnd { id, ok, text })) => {
                    let name = self.downloads.iter().find(|d| d.0 == id).map(|d| d.1.clone()).unwrap_or_default();
                    self.downloads.retain(|d| d.0 != id);
                    let msg = match self.inbox.end(id, ok, &text) {
                        Some(crate::files::Note::Done { ok: true, text, .. }) => format!("Downloaded {name} to {text}"),
                        Some(crate::files::Note::Done { text, .. }) => format!("{name} failed: {text}"),
                        _ => format!("Download failed: {text}"),
                    };
                    self.notify(&msg);
                }
                Ok(Ev::Net(ToClient::FileDone { id, ok, text })) => {
                    let name = self.uploads.iter().find(|u| u.id == id).map(|u| u.name.clone()).unwrap_or_default();
                    self.uploads.retain(|u| u.id != id);
                    let msg = if ok {
                        format!("{name} saved on the remote host: {text}")
                    } else {
                        format!("{name} failed: {text}")
                    };
                    self.notify(&msg);
                }
                Ok(Ev::Net(ToClient::Error(e)) | Ev::Closed(e)) => {
                    let log = self.sess.log();
                    return Err(if log.is_empty() { e } else { format!("{e}\n{log}") });
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => return Err("connection closed".into()),
            }
        }
    }

    /// Starts uploading files dropped on the window.
    fn take_drops(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let path = f.path().to_path_buf();
            if path.as_os_str().is_empty() {
                continue;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let mut files = Vec::new();
            if path.is_dir() {
                // The folder is recreated on the remote host, "name/sub/file".
                crate::files::walk(&path, &name, &mut files);
            } else {
                files.push((path, name));
            }
            for (path, name) in files {
                self.queue_upload(&path, name)?;
            }
        }
        Ok(())
    }

    fn queue_upload(&mut self, path: &std::path::Path, name: String) -> Result<(), String> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                self.notify(&format!("{name}: {e}"));
                return Ok(());
            }
        };
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let id = self.next_upload;
        self.next_upload += 1;
        self.send(ToAgent::FileStart { id, size, name: name.clone() })?;
        self.uploads.push_back(Upload { id, name, file, size, sent: 0, acked: 0, ended: false });
        Ok(())
    }

    /// Sends the next chunks of the first unfinished upload, keeping a bounded amount unacknowledged.
    fn feed_uploads(&mut self) -> Result<(), String> {
        use std::io::Read;
        let Some(i) = self.uploads.iter().position(|u| !u.ended) else { return Ok(()) };
        let mut buf = vec![0u8; UPLOAD_CHUNK];
        loop {
            let u = &mut self.uploads[i];
            if u.sent - u.acked.min(u.sent) >= UPLOAD_WINDOW {
                return Ok(());
            }
            let n = u.file.read(&mut buf).map_err(|e| format!("{}: {e}", u.name))?;
            let id = u.id;
            if n == 0 {
                u.ended = true;
                return self.send(ToAgent::FileEnd { id });
            }
            u.sent += n as u64;
            self.send(ToAgent::FileData { id, data: buf[..n].to_vec() })?;
        }
    }

    /// The remote file browser; returns its rectangle while open.
    fn file_panel(&mut self, ctx: &egui::Context) -> Result<Option<Rect>, String> {
        let Some(p) = self.panel.as_mut() else { return Ok(None) };
        let mut request: Option<ToAgent> = None;
        let mut open = true;
        let resp = egui::Window::new("Remote files")
            .open(&mut open)
            .default_pos(ctx.content_rect().right_top() + Vec2::new(-440.0, 70.0))
            .default_size([420.0, 480.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Up").clicked() {
                        request = Some(ToAgent::ListDir { path: p.parent() });
                    }
                    if ui.button("Home").clicked() {
                        request = Some(ToAgent::ListDir { path: String::new() });
                    }
                    let edit = ui.add(egui::TextEdit::singleline(&mut p.typed).desired_width(f32::INFINITY));
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        request = Some(ToAgent::ListDir { path: p.typed.clone() });
                    }
                });
                if !p.error.is_empty() {
                    ui.colored_label(Color32::from_rgb(240, 110, 100), &p.error);
                }
                let selected = p.selected.and_then(|i| p.entries.get(i));
                let label = match selected {
                    Some(e) if e.dir => format!("Download folder {}", e.name),
                    Some(e) => format!("Download {}", e.name),
                    None => "Select a file or folder".into(),
                };
                if ui.add_enabled(selected.is_some(), egui::Button::new(label).fill(ACCENT)).clicked()
                    && let Some(e) = selected
                {
                    request = Some(ToAgent::Fetch { path: p.child(&e.name) });
                }
                let hint = "Downloads go to this PC's Downloads folder. Drop files or folders on the window to upload.";
                ui.label(RichText::new(hint).small().color(Color32::GRAY));
                ui.separator();
                ui.checkbox(&mut p.hidden, "Show hidden files");
                let mut clicked = None;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (i, e) in p.entries.iter().enumerate() {
                        if !p.hidden && e.name.starts_with('.') {
                            continue;
                        }
                        let icon = if e.dir { "📁" } else { "📄" };
                        let size = if e.dir { String::new() } else { human(e.size) };
                        ui.horizontal(|ui| {
                            let r = ui.selectable_label(p.selected == Some(i), format!("{icon} {}", e.name));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(size).color(Color32::GRAY));
                            });
                            if r.clicked() {
                                clicked = Some(i);
                            }
                            if r.double_clicked() {
                                request = Some(if e.dir {
                                    ToAgent::ListDir { path: p.child(&e.name) }
                                } else {
                                    ToAgent::Fetch { path: p.child(&e.name) }
                                });
                            }
                        });
                    }
                });
                if clicked.is_some() {
                    p.selected = clicked;
                }
            });
        let rect = resp.map(|r| r.response.rect);
        if !open {
            self.panel = None;
        }
        if let Some(m) = request {
            self.send(m)?;
        }
        Ok(rect)
    }

    /// Progress of uploads and the drop hint, bottom left over the remote screen.
    fn upload_overlay(&self, ctx: &egui::Context, screen: Rect) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let mut lines: Vec<String> = self
            .uploads
            .iter()
            .map(|u| format!("Sending {}  {}%", u.name, if u.size == 0 { 100 } else { u.acked * 100 / u.size }))
            .collect();
        for (id, name) in &self.downloads {
            if let Some((got, size)) = self.inbox.received(*id) {
                lines.push(format!("Receiving {name}  {}%", if size == 0 { 100 } else { got * 100 / size }));
            }
        }
        if hovering {
            lines.push("Drop to send to the remote Downloads folder".into());
        }
        if lines.is_empty() {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("uploads")));
        let text = lines.join(
            "
",
        );
        let galley = painter.layout_no_wrap(text, FontId::proportional(14.0), Color32::WHITE);
        let pos = Pos2::new(screen.left() + 16.0, screen.bottom() - 16.0 - galley.size().y);
        let bg = Rect::from_min_size(pos, galley.size()).expand(8.0);
        painter.rect_filled(bg, CornerRadius::same(8), Color32::from_rgba_unmultiplied(28, 30, 36, 230));
        painter.galley(pos, galley, Color32::WHITE);
    }

    /// Copies changed pixels into the texture.
    fn upload(&mut self, ctx: &egui::Context, resized: bool, f: &crate::proto::Frame) {
        let (w, h) = (self.fb.w, self.fb.h);
        let opts = TextureOptions::LINEAR;
        match self.tex.as_mut() {
            Some(t) if !resized && t.size() == [w, h] => {
                for (x, y, rw, rh) in f.damage() {
                    let mut px = Vec::with_capacity(rw * rh * 3);
                    for row in y..y + rh {
                        px.extend_from_slice(&self.fb.rgb[(row * w + x) * 3..(row * w + x + rw) * 3]);
                    }
                    t.set_partial([x, y], ColorImage::from_rgb([rw, rh], &px), opts);
                }
            }
            _ => self.tex = Some(ctx.load_texture("remote", ColorImage::from_rgb([w, h], &self.fb.rgb), opts)),
        }
    }

    /// Asks the agent for a capture that fits the window, once resizing has settled.
    fn resize(&mut self, px: Vec2) -> Result<(), String> {
        let want = match self.fit {
            Fit::Window => (px.x.max(64.0) as u16, px.y.max(64.0) as u16),
            Fit::Original => self.sess.desk,
        };
        if want != self.view {
            self.view = want;
            self.view_changed = Some(Instant::now());
        }
        if let Some(t) = self.view_changed
            && t.elapsed() > Duration::from_millis(250)
        {
            self.view_changed = None;
            (self.ctl.s.view_w, self.ctl.s.view_h) = self.view;
            let s = self.ctl.s;
            self.send(ToAgent::Settings(s))?;
        }
        Ok(())
    }

    /// Draws the remote screen; returns where it is, in points.
    fn picture(&mut self, ui: &mut egui::Ui, area: Rect, ppp: f32) -> Option<Rect> {
        let tex = self.tex.as_ref()?;
        let size = Vec2::new(self.fb.w as f32, self.fb.h as f32) / ppp;
        let rect = match self.fit {
            Fit::Window => {
                let k = (area.width() / size.x).min(area.height() / size.y);
                Rect::from_center_size(area.center(), size * k)
            }
            Fit::Original => Rect::from_center_size(area.center(), size),
        };
        ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        Some(rect)
    }

    fn remote_point(&self, rect: Rect, pos: Pos2) -> Option<(u16, u16)> {
        if !rect.contains(pos) || self.fb.w == 0 {
            return None;
        }
        let x = ((pos.x - rect.min.x) / rect.width() * self.fb.w as f32) as usize;
        let y = ((pos.y - rect.min.y) / rect.height() * self.fb.h as f32) as usize;
        Some((x.min(self.fb.w - 1) as u16, y.min(self.fb.h - 1) as u16))
    }

    fn key(&mut self, keysym: u32, down: bool) -> Result<(), String> {
        if down {
            self.held.insert(keysym);
        } else if !self.held.remove(&keysym) {
            return Ok(());
        }
        self.send(ToAgent::KeyState { keysym, down })
    }

    /// Taps a key, for the Copy/Cut/Paste shortcuts the window system turns into events.
    fn tap(&mut self, keysym: u32) -> Result<(), String> {
        self.key(keysym, true)?;
        self.key(keysym, false)
    }

    fn sync_mods(&mut self, m: Modifiers) -> Result<(), String> {
        let pairs =
            [(self.mods.shift, m.shift, SHIFT_L), (self.mods.ctrl, m.ctrl, CONTROL_L), (self.mods.alt, m.alt, ALT_L)];
        for (was, now, sym) in pairs {
            if was != now {
                self.key(sym, now)?;
            }
        }
        self.mods = m;
        Ok(())
    }

    /// Releases everything held, as when the window loses focus mid-shortcut.
    fn release_all(&mut self) -> Result<(), String> {
        for k in std::mem::take(&mut self.held) {
            self.send(ToAgent::KeyState { keysym: k, down: false })?;
        }
        self.mods = Modifiers::NONE;
        Ok(())
    }

    fn input(&mut self, ctx: &egui::Context, shown: Option<Rect>, ui_rects: &[Rect]) -> Result<(), String> {
        let (events, focused) = ctx.input(|i| (i.events.clone(), i.focused));
        if !focused {
            return self.release_all();
        }
        let over_toolbar = |p: Pos2| ui_rects.iter().any(|t| t.contains(p));
        // Keys go to the remote unless a toolbar widget (a slider, a menu) has focus.
        let local = ctx.memory(|m| m.focused().is_some()) || ctx.any_popup_open();
        if let Some(rect) = shown {
            // Composed text (Korean, Japanese, Chinese input methods) arrives as IME commits.
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Terminal,
                    rect,
                    cursor_rect: Rect::from_min_size(rect.min, Vec2::new(1.0, 16.0)),
                    should_interrupt_composition: false,
                })
            });
        }
        for e in events {
            match e {
                Event::Key { key: Key::F11, .. } => {}
                Event::Key { key, physical_key, pressed, modifiers, .. } if !local => {
                    self.sync_mods(modifiers)?;
                    let Some(sym) = keysym(physical_key.unwrap_or(key)) else { continue };
                    if pressed && std::mem::take(&mut self.win_next) {
                        let (sym, mods) = if sym == ' ' as u32 { (SUPER_L, 0) } else { (sym, MOD_SUPER) };
                        self.send(ToAgent::Key { keysym: sym, mods })?;
                        continue;
                    }
                    self.key(sym, pressed)?;
                }
                Event::ModifiersChanged(m) if !local => self.sync_mods(m)?,
                Event::Copy if !local => self.tap('c' as u32)?,
                Event::Cut if !local => self.tap('x' as u32)?,
                Event::Paste(_) if !local => self.tap('v' as u32)?,
                Event::Ime(egui::ImeEvent::Commit(text)) if !local => {
                    for c in text.chars() {
                        let sym = if (c as u32) < 0x100 { c as u32 } else { 0x0100_0000 | c as u32 };
                        self.send(ToAgent::Key { keysym: sym, mods: 0 })?;
                    }
                }
                Event::PointerMoved(pos) if !over_toolbar(pos) => {
                    if let Some(p) = shown.and_then(|r| self.remote_point(r, pos))
                        && p != self.pointer
                    {
                        self.pointer = p;
                        self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                    }
                }
                Event::PointerButton { pos, button, pressed, .. } => {
                    if pressed && (over_toolbar(pos) || local) {
                        continue;
                    }
                    let Some(p) = shown.and_then(|r| self.remote_point(r, pos)) else { continue };
                    if p != self.pointer {
                        self.pointer = p;
                        self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                    }
                    let b = match button {
                        egui::PointerButton::Primary => 1,
                        egui::PointerButton::Middle => 2,
                        egui::PointerButton::Secondary => 3,
                        egui::PointerButton::Extra1 => 8,
                        egui::PointerButton::Extra2 => 9,
                    };
                    self.send(ToAgent::Button { button: b, pressed })?;
                }
                Event::MouseWheel { unit, delta, .. } => {
                    let Some(pos) = ctx.pointer_hover_pos() else { continue };
                    if over_toolbar(pos) || !shown.is_some_and(|r| r.contains(pos)) {
                        continue;
                    }
                    let lines = match unit {
                        egui::MouseWheelUnit::Point => delta / 40.0,
                        egui::MouseWheelUnit::Line => delta,
                        egui::MouseWheelUnit::Page => delta * 10.0,
                    };
                    let (sy, sx) = (-lines.y.round() as i32, -lines.x.round() as i32);
                    if sy != 0 {
                        self.send(ToAgent::Scroll { steps: sy.clamp(-10, 10) as i8 })?;
                    }
                    if sx != 0 {
                        self.send(ToAgent::HScroll { steps: sx.clamp(-10, 10) as i8 })?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        self.pinned = !on;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(on));
    }

    /// The floating toolbar; returns its rectangle while shown.
    fn toolbar(&mut self, ctx: &egui::Context, screen: Rect) -> Result<Option<Rect>, String> {
        let id = egui::Id::unique("toolbar");
        let near_top = ctx.pointer_hover_pos().is_some_and(|p| p.y < screen.top() + 56.0);
        let was_hovered = ctx.memory(|m| m.data.get_temp::<bool>(id).unwrap_or(false));
        if !(self.pinned || near_top || was_hovered || ctx.any_popup_open()) {
            let r = Rect::from_center_size(Pos2::new(screen.center().x, screen.top() + 4.0), Vec2::new(90.0, 4.0));
            ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id)).rect_filled(
                r,
                CornerRadius::same(2),
                Color32::from_white_alpha(90),
            );
            return Ok(None);
        }
        let mut action: Option<Deferred> = None;
        let resp = egui::Area::new(id)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(28, 30, 36, 240))
                    .stroke(Stroke::new(1.0, Color32::from_white_alpha(25)))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| ui.horizontal(|ui| self.toolbar_items(ui, &mut action)));
            });
        let rect = resp.response.rect;
        ctx.memory_mut(|m| m.data.insert_temp(id, resp.response.contains_pointer()));
        if let Some((text, at)) = &self.note {
            if at.elapsed() < Duration::from_secs(4) {
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id.with("note"))).text(
                    Pos2::new(screen.center().x, rect.bottom() + 20.0),
                    Align2::CENTER_TOP,
                    text,
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
            } else {
                self.note = None;
            }
        }
        if let Some(f) = action {
            f(self, ctx)?;
        }
        Ok(Some(rect))
    }

    fn toolbar_items(&mut self, ui: &mut egui::Ui, action: &mut Option<Deferred>) {
        ui.label(RichText::new("●").color(Color32::from_rgb(80, 200, 120)));
        let host = self.label.split("  ").next().unwrap_or(&self.label).to_string();
        ui.label(RichText::new(host).strong()).on_hover_text(&self.label);
        ui.separator();
        let quality = self.ctl.preset_label().replace(" crisp", "").replace(" exact", "").replace(" balanced", "");
        ui.menu_button(format!("Quality: {quality}"), |ui| {
            if ui.button("Auto").clicked() {
                *action = Some(Box::new(|r, _| r.menu('a')));
                ui.close();
            }
            for (i, p) in PRESETS.iter().enumerate() {
                if ui.button(p.name).clicked() {
                    let c = char::from(b'0' + i as u8);
                    *action = Some(Box::new(move |r, _| r.menu(c)));
                    ui.close();
                }
            }
            ui.separator();
            let mut mode = self.ctl.s.mode;
            egui::ComboBox::from_label("Colours").selected_text(mode.name()).show_ui(ui, |ui| {
                for m in Mode::ALL {
                    ui.selectable_value(&mut mode, m, m.name());
                }
            });
            let mut scale = self.ctl.s.scale;
            ui.add(egui::Slider::new(&mut scale, 20..=100).text("Scale %"));
            let mut fps = self.ctl.s.fps;
            ui.add(egui::Slider::new(&mut fps, 1..=60).text("FPS"));
            if (mode, scale, fps) != (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) {
                *action = Some(Box::new(move |r, _| r.custom(mode, scale, fps)));
            }
        });
        if self.monitors.len() > 1 {
            ui.menu_button(format!("Screen {}", self.monitor + 1), |ui| {
                for (i, m) in self.monitors.iter().enumerate() {
                    let primary = if m.primary { "  (primary)" } else { "" };
                    let text = format!("{}: {}  {}x{}{primary}", i + 1, m.name, m.w, m.h);
                    if ui.selectable_label(i == self.monitor, text).clicked() {
                        *action = Some(Box::new(move |r, _| r.send(ToAgent::SelectMonitor(i as u8))));
                        ui.close();
                    }
                }
            });
        }
        let fit_label = if self.fit == Fit::Window { "Fit" } else { "1:1" };
        if ui.button(fit_label).on_hover_text("Fit to the window or show at original size").clicked() {
            *action = Some(Box::new(|r, _| {
                r.fit = if r.fit == Fit::Window { Fit::Original } else { Fit::Window };
                Ok(())
            }));
        }
        let fs = if self.fullscreen { "Exit full screen" } else { "Full screen" };
        if ui.button(fs).on_hover_text("F11").clicked() {
            *action = Some(Box::new(|r, c| {
                r.set_fullscreen(c, !r.fullscreen);
                Ok(())
            }));
        }
        ui.menu_button("Keys", |ui| {
            let combos: [(&str, Option<Vec<u32>>); 8] = [
                ("Ctrl+Alt+Del", None),
                ("Win", Some(vec![SUPER_L])),
                ("Win+R   Run", Some(vec![SUPER_L, 'r' as u32])),
                ("Win+E   Files", Some(vec![SUPER_L, 'e' as u32])),
                ("Win+D   Desktop", Some(vec![SUPER_L, 'd' as u32])),
                ("Alt+Tab", Some(vec![ALT_L, 0xff09])),
                ("Alt+F4", Some(vec![ALT_L, 0xffc1])),
                ("Print Screen", Some(vec![0xff61])),
            ];
            for (label, keys) in combos {
                if ui.button(label).clicked() {
                    *action = Some(Box::new(move |r, _| match keys {
                        None => r.send(ToAgent::SecureAttention),
                        Some(k) => r.combo(&k),
                    }));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Win + next key").clicked() {
                *action = Some(Box::new(|r, _| {
                    r.win_next = true;
                    r.notify("Press a key to send with Win (Space: Win alone)");
                    Ok(())
                }));
                ui.close();
            }
        });
        ui.toggle_value(&mut self.share_clipboard, "Clipboard").on_hover_text("Share copied text both ways");
        let mut sound = self.sound.is_some();
        if ui.toggle_value(&mut sound, "Sound").on_hover_text("Play the remote host's sound (about 8 KB/s)").clicked() {
            *action = Some(Box::new(move |r, _| {
                if sound {
                    match crate::audio::Player::new() {
                        Ok(p) => r.sound = Some(p),
                        Err(e) => {
                            r.notify(&format!("Sound: {e}"));
                            return Ok(());
                        }
                    }
                } else {
                    r.sound = None;
                }
                r.send(ToAgent::Audio(sound))
            }));
        }
        if cfg!(windows) {
            let tip = "Send Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Print Screen to the remote host instead of this PC";
            ui.toggle_value(&mut self.grab_keys, "System keys").on_hover_text(tip);
        }
        let files_tip = "Browse and download remote files; drop files or folders on the window to upload";
        if ui.button("Files").on_hover_text(files_tip).clicked() {
            *action = Some(Box::new(|r, _| {
                if r.panel.take().is_none() {
                    r.send(ToAgent::ListDir { path: String::new() })?;
                }
                Ok(())
            }));
        }
        if ui.button("Refresh").clicked() {
            *action = Some(Box::new(|r, _| r.menu('r')));
        }
        ui.separator();
        ui.label(
            RichText::new(format!("{:.0} KB/s  {:.0} ms", self.ctl.kbps, self.ctl.rtt))
                .font(FontId::monospace(12.0))
                .color(Color32::GRAY),
        );
        let pin = if self.pinned { "Auto-hide" } else { "Pin" };
        if ui
            .small_button(pin)
            .on_hover_text("Keep the toolbar shown or hide it until the pointer nears the top")
            .clicked()
        {
            *action = Some(Box::new(|r, _| {
                r.pinned = !r.pinned;
                Ok(())
            }));
        }
        let close =
            egui::Button::new(RichText::new("Disconnect").color(Color32::WHITE)).fill(Color32::from_rgb(190, 60, 60));
        if ui.add(close).clicked() {
            *action = Some(Box::new(|_, _| Err("disconnected".into())));
        }
    }

    /// Presses keys in order and releases them in reverse.
    fn combo(&mut self, keys: &[u32]) -> Result<(), String> {
        for &k in keys {
            self.send(ToAgent::KeyState { keysym: k, down: true })?;
        }
        for &k in keys.iter().rev() {
            self.send(ToAgent::KeyState { keysym: k, down: false })?;
        }
        Ok(())
    }

    fn menu(&mut self, c: char) -> Result<(), String> {
        match self.ctl.menu(c) {
            Action::Refresh => {
                self.fb = Framebuffer::default();
                self.tex = None;
                self.send(ToAgent::Refresh)
            }
            Action::Settings => {
                let s = self.ctl.s;
                self.send(ToAgent::Settings(s))
            }
            _ => Ok(()),
        }
    }

    fn custom(&mut self, mode: Mode, scale: u8, fps: u8) -> Result<(), String> {
        (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) = (mode, scale, fps);
        self.ctl.preset = "custom".into();
        self.ctl.auto = None;
        let s = self.ctl.s;
        self.send(ToAgent::Settings(s))
    }
}

fn human(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_keysyms() {
        assert_eq!(keysym(Key::A), Some('a' as u32));
        assert_eq!(keysym(Key::Num7), Some('7' as u32));
        assert_eq!(keysym(Key::F1), Some(0xffbe));
        assert_eq!(keysym(Key::F12), Some(0xffc9));
        assert_eq!(keysym(Key::Enter), Some(0xff0d));
        assert_eq!(keysym(Key::Quote), Some('\'' as u32));
    }

    fn panel(path: &str) -> Panel {
        Panel {
            path: path.into(),
            typed: String::new(),
            entries: Vec::new(),
            error: String::new(),
            selected: None,
            hidden: false,
        }
    }

    #[test]
    fn panel_paths_follow_the_remote_os() {
        assert_eq!(panel("/home/a").child("x"), "/home/a/x");
        assert_eq!(panel("/home/a").parent(), "/home");
        assert_eq!(panel("/home").parent(), "/");
        assert_eq!(panel(r"C:\Users\a").child("x"), r"C:\Users\a\x");
        assert_eq!(panel(r"C:\Users").parent(), r"C:\");
        assert_eq!(panel(r"C:\").child("x"), r"C:\x");
    }
}
