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

pub(crate) const ACCENT: Color32 = Color32::from_rgb(64, 132, 245);

/// Phosphor icon codepoints (assets/fonts, MIT licensed).
pub(crate) mod ic {
    pub const ARROW_CLOCKWISE: &str = "\u{E036}";
    pub const ARROW_LEFT: &str = "\u{E058}";
    pub const ARROW_RIGHT: &str = "\u{E06C}";
    pub const ARROW_UP: &str = "\u{E08E}";
    pub const APP_WINDOW: &str = "\u{E5DA}";
    pub const SQUARES_FOUR: &str = "\u{E464}";
    pub const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
    pub const PLUS: &str = "\u{E3D4}";
    pub const ARROWS_CLOCKWISE: &str = "\u{E094}";
    pub const CARET_DOWN: &str = "\u{E136}";
    pub const CARET_UP: &str = "\u{E13C}";
    pub const ARTICLE: &str = "\u{E0A8}";
    pub const COPY: &str = "\u{E1CA}";
    pub const GLOBE: &str = "\u{E288}";
    pub const KEY: &str = "\u{E2D6}";
    pub const TEXT_T: &str = "\u{E48A}";
    pub const WRENCH: &str = "\u{E5D4}";
    pub const DOTS: &str = "\u{E1FE}";
    pub const CURSOR_CLICK: &str = "\u{E7C8}";
    pub const GRIP: &str = "\u{EAE2}";
    pub const DOWNLOAD: &str = "\u{E20C}";
    pub const EYE: &str = "\u{E220}";
    pub const EYE_SLASH: &str = "\u{E224}";
    pub const FOLDER_OPEN: &str = "\u{E256}";
    pub const FOLDER_PLUS: &str = "\u{E258}";
    pub const HARD_DRIVE: &str = "\u{E29E}";
    pub const HARD_DRIVES: &str = "\u{E2A0}";
    pub const HOUSE: &str = "\u{E2C2}";
    pub const PENCIL: &str = "\u{E3B4}";
    pub const TRASH: &str = "\u{E4A6}";
    pub const UPLOAD: &str = "\u{E4C0}";
    pub const X_CIRCLE: &str = "\u{E4F8}";
    pub const CAMERA: &str = "\u{E10E}";
    pub const CHAT: &str = "\u{E16C}";
    pub const CORNERS_IN: &str = "\u{E1CE}";
    pub const CORNERS_OUT: &str = "\u{E1D0}";
    pub const FILE: &str = "\u{E230}";
    pub const FOLDER: &str = "\u{E24A}";
    pub const GEAR: &str = "\u{E272}";
    pub const KEYBOARD: &str = "\u{E2D8}";
    pub const SEARCH: &str = "\u{E30C}";
    pub const MONITOR: &str = "\u{E32E}";
    pub const MOON: &str = "\u{E330}";
    pub const PAUSE: &str = "\u{E39E}";
    pub const PLAY: &str = "\u{E3D0}";
    pub const PUSH_PIN: &str = "\u{E3E2}";
    pub const PUSH_PIN_SLASH: &str = "\u{E3E4}";
    pub const STAR: &str = "\u{E46A}";
    pub const SUN: &str = "\u{E472}";
    pub const TERMINAL: &str = "\u{EAE8}";
    pub const X: &str = "\u{E4F6}";
}

/// The filled variant of the icon font, for a set star.
fn filled() -> egui::FontFamily {
    egui::FontFamily::Name("phosphor-fill".into())
}

fn install_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    let regular = egui::FontData::from_static(include_bytes!("../assets/fonts/Phosphor.ttf"));
    let fill = egui::FontData::from_static(include_bytes!("../assets/fonts/Phosphor-Fill.ttf"));
    let fallback = |f| InsertFontFamily { family: f, priority: FontPriority::Lowest };
    ctx.add_font(FontInsert {
        name: "phosphor".into(),
        data: regular,
        families: vec![fallback(egui::FontFamily::Proportional), fallback(egui::FontFamily::Monospace)],
    });
    ctx.add_font(FontInsert {
        name: "phosphor-fill".into(),
        data: fill,
        families: vec![InsertFontFamily { family: filled(), priority: FontPriority::Highest }],
    });
}

/// The logo, for the window and the task bar.
pub(crate) fn app_icon() -> Option<egui::IconData> {
    let mut reader =
        png::Decoder::new(std::io::Cursor::new(&include_bytes!("../assets/sshremote.png")[..])).read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).ok()?;
    rgba.truncate(info.buffer_size());
    Some(egui::IconData { rgba, width: info.width, height: info.height })
}
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
    /// The whole screen in the window, proportions kept.
    Window,
    /// The whole window filled, proportions ignored.
    Stretch,
    /// A fixed size: percent of the remote resolution, panned at the window edges when larger.
    Fixed(u16),
}

impl Fit {
    fn label(self) -> String {
        match self {
            Fit::Window => "Fit".into(),
            Fit::Stretch => "Stretch".into(),
            Fit::Fixed(p) => format!("{p}%"),
        }
    }
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
    /// How far a fixed-size picture is scrolled, in points.
    pan: Vec2,
    fullscreen: bool,
    pinned: bool,
    /// Where the toolbar sits along the top edge, 0 left to 1 right; dragged by its grip or handle.
    bar_x: f32,
    /// The toolbar's width last frame, to keep it on screen.
    bar_w: f32,
    /// The tucked-away toolbar stays out until then: after the handle or the toolbar was last under the pointer.
    bar_until: Instant,
    clip: Clip,
    share_clipboard: bool,
    /// Watching only: no key, click or wheel reaches the SSH host.
    view_only: bool,
    held: BTreeSet<u32>,
    mods: Modifiers,
    pointer: (u16, u16),
    view: (u16, u16),
    view_changed: Option<Instant>,
    win_next: bool,
    /// Notifications at the bottom right: text, whether it is a warning, when it came.
    toasts: Vec<(String, bool, Instant)>,
    grab_keys: bool,
    uploads: std::collections::VecDeque<Upload>,
    next_upload: u32,
    sound: Option<crate::audio::Player>,
    /// Open chat panel: messages (from the remote side?, text) and the line being typed.
    chat: Option<(Vec<(bool, String)>, String)>,
    /// Messages kept while the panel is closed, and how many are unread.
    chat_lines: Vec<(bool, String)>,
    unread: usize,
    paused: bool,
    monitors: Vec<crate::proto::MonitorInfo>,
    monitor: usize,
    inbox: crate::files::Inbox,
    /// Downloads in progress: id, name and the request tag (0 for the Downloads folder).
    downloads: Vec<(u32, String, u32)>,
    /// The file transfer window, while open.
    files: Option<crate::filewin::FileWindow>,
    /// The terminal window, while open.
    terms: Option<crate::termwin::TermWindow>,
    ssh_cmd: Option<Vec<String>>,
    /// The "what to share" chooser while open: the window list once it arrived, and the tab shown.
    picker: Option<(Option<Result<Vec<crate::capture::WindowInfo>, String>>, u8)>,
    /// What is shared now: kind as in `capture::Share` and a description.
    sharing: (u8, String),
    /// The next click on the picture chooses the window to share.
    click_pick: bool,
    /// Where the remote picture was drawn last, to tell when the pointer is over it.
    last_shown: Option<Rect>,
    /// That click has been pressed on the picture (the release of the click that opened the mode does not count).
    pick_pressed: bool,
    /// When to share the window that the choosing click brought to the front.
    pick_at: Option<Instant>,
    /// Local folder each tagged download request writes into.
    fetch_dirs: std::collections::HashMap<u32, std::path::PathBuf>,
    next_tag: u32,
}

/// A file being sent to the remote host.
struct Upload {
    id: u32,
    name: String,
    /// The remote folder chosen in the file window; `None` for the Downloads folder.
    dir: Option<String>,
    cancelled: bool,
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
    /// Host aliases from ~/.ssh/config; ssh applies their user, port and key itself.
    ssh_hosts: Vec<String>,
    favorites: Vec<String>,
    /// Host and port of the connection in progress.
    pending_host: Option<(String, String)>,
    /// The ssh command for the connection in progress, to open the host in a system terminal.
    pending_ssh: Option<Vec<String>>,
    seen: std::collections::HashMap<String, u64>,
    search: String,
    error: Option<String>,
    budget: f64,
    /// System keys taken from Windows while a session is in front.
    grabbed: Option<mpsc::Receiver<(u32, bool)>>,
    /// The stand-alone terminal window (local shells and ssh sessions).
    terms: Option<crate::termwin::TermWindow>,
    keys: Option<crate::tools::KeyTool>,
    config: Option<crate::tools::ConfigTool>,
    logo: Option<TextureHandle>,
    show_options: bool,
}

pub fn run(opt: &Options) -> Result<(), String> {
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
    let native = native_options("SSH Remote", [1280.0, 800.0]);
    eframe::run_native(
        "sshremote",
        native,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            #[cfg(windows)]
            let grabbed = crate::keygrab::install();
            #[cfg(not(windows))]
            let grabbed = None;
            let mut app = App {
                screen: Screen::Connect,
                form,
                recent: crate::hosts::load_recent(),
                ssh_hosts: crate::hosts::ssh_config_hosts(),
                favorites: crate::hosts::favorites(),
                pending_host: None,
                pending_ssh: None,
                seen: crate::hosts::last_seen(),
                search: String::new(),
                error: None,
                budget,
                grabbed,
                terms: None,
                keys: None,
                config: None,
                logo: None,
                show_options: false,
            };
            if auto_start {
                app.connect(&cc.egui_ctx, local);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

/// Window options shared by the viewer and the chat window.
pub fn native_options(title: &str, size: [f32; 2]) -> eframe::NativeOptions {
    #[cfg(windows)]
    if std::env::var_os("WGPU_BACKEND").is_none() {
        // Direct3D 12 works on every Windows 10/11 GPU, including drivers whose Vulkan or OpenGL is broken.
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
    }
    let mut native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(size)
            .with_min_inner_size([320.0, 240.0]),
        ..Default::default()
    };
    if let Some(icon) = app_icon() {
        native.viewport = native.viewport.with_icon(icon);
    }
    // Some drivers block a vsynced present for a quarter second; frames are paced by the agent anyway.
    native.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    native
}

pub fn style(ctx: &egui::Context) {
    install_fonts(ctx);
    // Ctrl+plus and Ctrl+minus belong to the remote host and the terminal, not to zooming this window.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    let dark = crate::hosts::setting("theme").is_none_or(|t| t != "light");
    set_theme(ctx, dark);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(10.0, 10.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
    });
}

/// Dark or light, remembered between runs.
pub fn set_theme(ctx: &egui::Context, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    if dark {
        v.panel_fill = Color32::from_rgb(18, 19, 23);
        v.window_fill = Color32::from_rgb(28, 30, 36);
        v.extreme_bg_color = Color32::from_rgb(12, 13, 16);
        v.widgets.hovered.bg_fill = Color32::from_rgb(48, 52, 62);
    } else {
        v.panel_fill = Color32::from_rgb(242, 243, 246);
        v.window_fill = Color32::WHITE;
        v.extreme_bg_color = Color32::from_rgb(234, 236, 241);
        v.widgets.hovered.bg_fill = Color32::from_rgb(226, 230, 238);
    }
    v.selection.bg_fill = ACCENT;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(8);
    }
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_visuals_of(theme, v);
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    crate::hosts::set_setting("theme", if dark { "dark" } else { "light" });
}

const GOOD: Color32 = Color32::from_rgb(70, 190, 110);
const WARN: Color32 = Color32::from_rgb(230, 170, 50);
const BAD: Color32 = Color32::from_rgb(225, 85, 75);

/// An icon button with a tooltip; `on` shows it pressed.
pub(crate) fn icon(ui: &mut egui::Ui, glyph: &str, tip: &str, on: bool) -> egui::Response {
    ui.add(tool_button(glyph, on)).on_hover_text(tip)
}

/// A square, borderless icon button that shows its frame on hover or while on.
fn tool_button(glyph: &str, on: bool) -> egui::Button<'_> {
    egui::Button::new(RichText::new(glyph).size(16.0))
        .min_size(Vec2::splat(28.0))
        .frame_when_inactive(false)
        .selected(on)
}

/// An icon button that opens a menu.
fn icon_menu<R>(ui: &mut egui::Ui, glyph: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::Response {
    let config =
        egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let (r, inner) =
        egui::containers::menu::MenuButton::from_button(tool_button(glyph, false)).config(config).ui(ui, add);
    if inner.is_some() {
        // Remembered for the next frame, so an auto-hidden toolbar stays while its menu is open.
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("sshremote-menu-open"), pass));
    }
    r
}

fn menu_open(ctx: &egui::Context) -> bool {
    let last = ctx.data(|d| d.get_temp::<u64>(egui::Id::unique("sshremote-menu-open")));
    last.is_some_and(|p| p + 2 >= ctx.cumulative_pass_nr())
}

/// A preset's real settings, e.g. "256 colours, 100%, 20 fps".
fn preset_text(p: &crate::proto::Preset) -> String {
    format!("{}, {}%, {} fps", p.mode.label(), p.scale, p.fps)
}

/// A circle with the host's initial, coloured by its name.
fn badge(ui: &mut egui::Ui, name: &str) {
    const COLORS: [Color32; 6] = [
        Color32::from_rgb(64, 132, 245),
        Color32::from_rgb(46, 160, 120),
        Color32::from_rgb(150, 100, 220),
        Color32::from_rgb(220, 120, 60),
        Color32::from_rgb(200, 80, 120),
        Color32::from_rgb(60, 150, 180),
    ];
    let bare = name.trim_start_matches("sshremote://");
    let bare = bare.rsplit('@').next().unwrap_or(bare);
    let hash = bare.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 18.0, COLORS[hash as usize % COLORS.len()]);
    let initial = bare.chars().next().unwrap_or('?').to_uppercase().to_string();
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, initial, FontId::proportional(17.0), Color32::WHITE);
}

/// The cause of a failed connection in plain words, with what to try.
fn friendly(e: &str) -> (String, &'static str) {
    let first = e.lines().find(|l| !l.trim().is_empty()).unwrap_or(e).trim().to_string();
    let l = e.to_lowercase();
    let hint = if l.contains("permission denied") {
        "The host refused the login. Enter the password, or install your key on the host with ssh-copy-id."
    } else if l.contains("could not resolve") || l.contains("name or service not known") || l.contains("no such host") {
        "The host name is unknown. Check the spelling, or use the IP address."
    } else if l.contains("connection refused") || l.contains("actively refused") {
        "Nothing answered on that port. Is the SSH server (or sshremote --serve) running there?"
    } else if l.contains("timed out") || l.contains("no route") || l.contains("unreachable") {
        "The host did not answer. Check the address, the network and any firewall."
    } else if l.contains("command not found") || l.contains("not recognized") || l.contains("no such file") {
        "sshremote is not installed on the host. Copy it to ~/.local/bin/sshremote (Windows: anywhere on PATH)."
    } else if l.contains("host key verification failed") {
        "The host's key changed since the last visit. If that is expected, remove its line from ~/.ssh/known_hosts."
    } else if l.contains("wrong password") {
        "The password does not match the one shown by sshremote --serve on the host."
    } else if l.contains("no agent with that id") {
        "No sshremote --serve with that ID is waiting at the relay."
    } else if l.contains("no frame") || l.contains("no interactive desktop") || l.contains("logged") {
        "Nobody is signed in to the SSH host's desktop, or it is locked."
    } else if l.contains("did not answer within") {
        "The remote agent did not start in time. Try again, or check that sshremote runs on the host."
    } else {
        ""
    };
    (first, hint)
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Favorite,
    Recent,
    SshConfig,
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
                    self.error = Some("Enter the one-time password shown by sshremote --serve".into());
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
        // Remembered once the connection works, so typos do not pile up in the list.
        self.pending_host = (!local).then(|| (f.host.trim().to_string(), f.port.trim().to_string()));
        self.pending_ssh = match &target {
            Target::Ssh { host, port, args, .. } => {
                let mut cmd = vec!["ssh".to_string()];
                if let Some(p) = port {
                    cmd.extend(["-p".into(), p.to_string()]);
                }
                cmd.extend(args.iter().cloned());
                cmd.push(host.clone());
                Some(cmd)
            }
            _ => None,
        };
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

    /// Favorites, then recent hosts, then ~/.ssh/config aliases, without repeats.
    fn host_list(&self) -> Vec<(String, String, Source)> {
        let mut out: Vec<(String, String, Source)> = Vec::new();
        let port_of = |h: &str| self.recent.iter().find(|r| r.0 == h).map(|r| r.1.clone()).unwrap_or_default();
        for h in &self.favorites {
            out.push((h.clone(), port_of(h), Source::Favorite));
        }
        for (h, p) in &self.recent {
            if !out.iter().any(|o| &o.0 == h) {
                out.push((h.clone(), p.clone(), Source::Recent));
            }
        }
        for h in &self.ssh_hosts {
            if !out.iter().any(|o| &o.0 == h) {
                out.push((h.clone(), String::new(), Source::SshConfig));
            }
        }
        let q = self.search.trim().to_lowercase();
        out.retain(|o| q.is_empty() || o.0.to_lowercase().contains(&q));
        out
    }

    /// Opens an ssh session to `host` in a tab of the stand-alone terminal.
    fn open_terminal(&mut self, host: &str, port: &str) {
        let host = host.trim();
        if host.is_empty() {
            self.error = Some("Enter a host, for example user@192.168.1.20".into());
            return;
        }
        if host.starts_with("sshremote://") {
            self.error = Some("sshremote:// addresses reach the remote screen only; choose Remote screen in the ... menu.".into());
            return;
        }
        self.error = None;
        let profile = crate::termwin::ssh_profile(host, port);
        self.open_profile(profile);
        self.recent.retain(|(r, _)| r != host);
        self.recent.insert(0, (host.to_string(), port.trim().to_string()));
        self.recent.truncate(12);
        crate::hosts::save_recent(&self.recent);
        crate::hosts::mark_seen(host);
        self.seen = crate::hosts::last_seen();
    }

    fn open_profile(&mut self, profile: crate::termwin::Profile) {
        match &mut self.terms {
            Some(t) => t.open(profile),
            None => self.terms = Some(crate::termwin::TermWindow::standalone(profile, Vec::new())),
        }
    }

    fn system_terminal(&mut self, host: &str, port: &str) {
        let mut cmd = vec!["ssh".to_string()];
        if !port.trim().is_empty() {
            cmd.extend(["-p".into(), port.trim().to_string()]);
        }
        cmd.push(host.trim().to_string());
        if let Err(e) = crate::termwin::spawn_system(&cmd) {
            self.error = Some(format!("cannot open the system terminal: {e}"));
        }
    }

    /// The windows that live beside every screen: the terminal and the SSH tools.
    fn side_windows(&mut self, ctx: &egui::Context) {
        if let Some(mut t) = self.terms.take() {
            let cmds = t.show(ctx, "");
            let closed = cmds.iter().any(|c| matches!(c, crate::termwin::Cmd::Close));
            if !closed {
                self.terms = Some(t);
            }
        }
        let mut actions = Vec::new();
        if let Some(k) = &mut self.keys
            && !k.show(ctx, &mut actions)
        {
            self.keys = None;
        }
        for a in actions {
            match a {
                crate::tools::ToolAction::Terminal { title, argv } => self.open_profile(crate::termwin::Profile {
                    name: title,
                    icon: ic::KEY,
                    launch: crate::termwin::Launch::Local(argv),
                }),
            }
        }
        let mut changed = false;
        if let Some(c) = &mut self.config
            && !c.show(ctx, &mut changed)
        {
            self.config = None;
        }
        if changed {
            self.ssh_hosts = crate::hosts::ssh_config_hosts();
        }
    }

    fn logo(&mut self, ctx: &egui::Context) -> Option<TextureHandle> {
        if self.logo.is_none() {
            let i = app_icon()?;
            let img = egui::ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba);
            self.logo = Some(ctx.load_texture("sshremote-logo", img, egui::TextureOptions::LINEAR));
        }
        self.logo.clone()
    }

    fn connect_screen(&mut self, ui: &mut egui::Ui) {
        // What the screen asks for: (host, port, how).
        #[derive(Clone, Copy, PartialEq)]
        enum How {
            Terminal,
            Screen,
            System,
            TestDesktop,
        }
        let mut go: Option<(String, String, How)> = None;
        let mut starred: Option<(String, bool)> = None;
        let mut forget: Option<String> = None;
        let ctx = ui.ctx().clone();
        let logo = self.logo(&ctx);
        let dark = ui.visuals().dark_mode;
        let card = ui.visuals().window_fill;
        let line = ui.visuals().widgets.noninteractive.bg_stroke.color;

        egui::Panel::top("home-header")
            .frame(egui::Frame::new().fill(card).inner_margin(egui::Margin::symmetric(18, 10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    match &logo {
                        Some(t) => {
                            ui.add(egui::Image::new(t).fit_to_exact_size(Vec2::splat(34.0)));
                        }
                        None => {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(34.0), egui::Sense::hover());
                            ui.painter().rect_filled(r, CornerRadius::same(9), ACCENT);
                            ui.painter().text(r.center(), Align2::CENTER_CENTER, ic::TERMINAL, FontId::proportional(20.0), Color32::WHITE);
                        }
                    }
                    ui.label(RichText::new("SSH Remote").size(21.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (glyph, tip) = if dark { (ic::SUN, "Light theme") } else { (ic::MOON, "Dark theme") };
                        if icon(ui, glyph, tip, false).clicked() {
                            set_theme(ui.ctx(), !dark);
                        }
                        icon_menu(ui, ic::WRENCH, |ui| {
                            ui.set_min_width(220.0);
                            if ui.button(format!("{}  SSH keys", ic::KEY)).clicked() {
                                self.keys = Some(crate::tools::KeyTool::new());
                                ui.close();
                            }
                            if ui.button(format!("{}  SSH hosts (~/.ssh/config)", ic::GLOBE)).clicked() {
                                self.config = Some(crate::tools::ConfigTool::new());
                                ui.close();
                            }
                            ui.separator();
                            ui.label(RichText::new("Local terminal").small().weak());
                            for p in crate::termwin::local_profiles() {
                                if ui.button(format!("{}  {}", p.icon, p.name)).clicked() {
                                    self.open_profile(p);
                                    ui.close();
                                }
                            }
                            ui.separator();
                            if ui.button(format!("{}  Built-in test desktop", ic::MONITOR)).clicked() {
                                go = Some((String::new(), String::new(), How::TestDesktop));
                                ui.close();
                            }
                        })
                        .on_hover_text("Tools: SSH keys, SSH hosts, local terminal");
                        let local = icon(ui, ic::TERMINAL, "Local terminal", false);
                        if local.clicked()
                            && let Some(p) = crate::termwin::local_profiles().into_iter().next()
                        {
                            self.open_profile(p);
                        }
                    });
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            let width = ui.available_width().min(760.0);
            let side = ((ui.available_width() - width) / 2.0).max(0.0);
            let full = ui.available_rect_before_wrap();
            let column = Rect::from_min_size(full.min + Vec2::new(side, 14.0), Vec2::new(width, full.height() - 14.0));
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(column).layout(egui::Layout::top_down(egui::Align::Min)));
            let ui = &mut ui;

            // Quick connect: the host, Terminal by default, everything else behind "...".
            egui::Frame::new().fill(card).stroke(Stroke::new(1.0, line)).corner_radius(CornerRadius::same(12)).inner_margin(14.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Connect to a host").strong());
                ui.horizontal(|ui| {
                    let field_w = ui.available_width() - 190.0;
                    let host = ui.add(
                        egui::TextEdit::singleline(&mut self.form.host)
                            .hint_text("user@host  or an ~/.ssh/config alias")
                            .desired_width(field_w)
                            .margin(egui::Margin::symmetric(8, 7)),
                    );
                    let enter = host.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let b = egui::Button::new(RichText::new(format!("{}  Terminal", ic::TERMINAL)).color(Color32::WHITE))
                        .fill(ACCENT)
                        .min_size(Vec2::new(112.0, 32.0));
                    if ui.add(b).on_hover_text("Open an SSH terminal (Enter)").clicked() || enter {
                        go = Some((self.form.host.clone(), self.form.port.clone(), How::Terminal));
                    }
                    let more = egui::containers::menu::MenuButton::new(RichText::new(ic::DOTS).size(16.0));
                    more.ui(ui, |ui| {
                        ui.set_min_width(230.0);
                        if ui.button(format!("{}  Remote screen", ic::MONITOR)).clicked() {
                            go = Some((self.form.host.clone(), self.form.port.clone(), How::Screen));
                            ui.close();
                        }
                        if ui.button(format!("{}  System terminal", ic::ARROW_SQUARE_OUT)).clicked() {
                            go = Some((self.form.host.clone(), self.form.port.clone(), How::System));
                            ui.close();
                        }
                    })
                    .0
                    .on_hover_text("Remote screen and more");
                    let opts = ui.add(tool_button(ic::GEAR, self.show_options)).on_hover_text("Connection options");
                    if opts.clicked() {
                        self.show_options = !self.show_options;
                    }
                });
                if self.show_options {
                    ui.add_space(4.0);
                    egui::Grid::new("connect-options").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new("Port").weak());
                        ui.add(egui::TextEdit::singleline(&mut self.form.port).hint_text("22").desired_width(70.0));
                        ui.end_row();
                        ui.label(RichText::new("Password").weak()).on_hover_text(
                            "For the remote screen. Leave empty to sign in with SSH keys; sshremote:// addresses need the password sshremote --serve shows. Terminals ask in the tab.",
                        );
                        ui.add(egui::TextEdit::singleline(&mut self.form.password).password(true).hint_text("optional with SSH keys").desired_width(260.0));
                        ui.end_row();
                        ui.label(RichText::new("Screen quality").weak());
                        let current = self.form.preset.map_or("Auto (adapts to the connection)".into(), |i| preset_text(&PRESETS[i]));
                        egui::ComboBox::from_id_salt("quality").selected_text(current).width(260.0).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.form.preset, None, "Auto (adapts to the connection)");
                            for (i, p) in PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut self.form.preset, Some(i), preset_text(p));
                            }
                        });
                        ui.end_row();
                        ui.label(RichText::new("Agent command").weak()).on_hover_text("How the remote screen starts sshremote on the host");
                        ui.add(egui::TextEdit::singleline(&mut self.form.remote).desired_width(260.0));
                        ui.end_row();
                        ui.label(RichText::new("Capture").weak());
                        ui.horizontal(|ui| {
                            for c in ["auto", "gnome", "windows", "synthetic"] {
                                ui.selectable_value(&mut self.form.capture, c.to_string(), c);
                            }
                        });
                        ui.end_row();
                    });
                }
                if let Some(e) = &self.error {
                    let (what, hint) = friendly(e);
                    ui.add_space(4.0);
                    egui::Frame::new()
                        .fill(BAD.gamma_multiply(0.15))
                        .stroke(Stroke::new(1.0, BAD.gamma_multiply(0.6)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("Could not connect").strong().color(BAD));
                            ui.label(&what).on_hover_text(e.as_str());
                            if !hint.is_empty() {
                                ui.label(RichText::new(hint).weak());
                            }
                        });
                }
            });

            ui.add_space(14.0);
            let hosts = self.host_list();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Connections").size(16.0).strong());
                ui.label(RichText::new(format!("{}", hosts.len())).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(format!("{}  Search", ic::SEARCH)).desired_width(200.0));
                });
            });
            ui.add_space(2.0);
            // Only the list scrolls; the header and the connect card stay put.
            egui::Frame::new().fill(card).stroke(Stroke::new(1.0, line)).corner_radius(CornerRadius::same(12)).inner_margin(4.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height((ui.available_height() - 12.0).max(80.0));
                if hosts.is_empty() {
                    let text = if self.search.is_empty() {
                        "Hosts you connect to, and the aliases in ~/.ssh/config, appear here."
                    } else {
                        "No host matches the search."
                    };
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new(text).weak()));
                    return;
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (i, (h, p, src)) in hosts.iter().enumerate() {
                        let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), egui::Sense::click());
                        if r.hovered() || self.form.host == *h {
                            let fill = if r.hovered() { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().faint_bg_color };
                            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
                        }
                        if i > 0 {
                            ui.painter().hline(rect.x_range().shrink(12.0), rect.top(), Stroke::new(1.0, line.gamma_multiply(0.5)));
                        }
                        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(10.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                        row.style_mut().interaction.selectable_labels = false;
                        badge(&mut row, h);
                        row.vertical(|ui| {
                            ui.add_space(9.0);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let name = if p.is_empty() { h.clone() } else { format!("{h}:{p}") };
                            ui.label(RichText::new(name).strong());
                            let when = match (src, self.seen.get(h)) {
                                (_, Some(t)) => crate::hosts::ago(*t),
                                (Source::SshConfig, None) => "from ~/.ssh/config".into(),
                                _ => "never connected".into(),
                            };
                            let how = if h.starts_with("sshremote://") { "direct" } else { "SSH" };
                            ui.label(RichText::new(format!("{how}  ·  {when}")).small().weak());
                        });
                        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let direct = h.starts_with("sshremote://");
                            egui::containers::menu::MenuButton::from_button(tool_button(ic::DOTS, false)).ui(ui, |ui| {
                                ui.set_min_width(210.0);
                                if !direct && ui.button(format!("{}  Terminal", ic::TERMINAL)).clicked() {
                                    go = Some((h.clone(), p.clone(), How::Terminal));
                                }
                                if ui.button(format!("{}  Remote screen", ic::MONITOR)).clicked() {
                                    go = Some((h.clone(), p.clone(), How::Screen));
                                }
                                if !direct && ui.button(format!("{}  System terminal", ic::ARROW_SQUARE_OUT)).clicked() {
                                    go = Some((h.clone(), p.clone(), How::System));
                                }
                                ui.separator();
                                if ui.button(format!("{}  Copy address", ic::COPY)).clicked() {
                                    ui.ctx().copy_text(h.clone());
                                }
                                if *src == Source::Recent && ui.button(format!("{}  Remove from the list", ic::TRASH)).clicked() {
                                    forget = Some(h.clone());
                                }
                            });
                            let fav = *src == Source::Favorite;
                            let mut star = RichText::new(ic::STAR).size(16.0);
                            star = if fav { star.family(filled()).color(WARN) } else { star.color(ui.visuals().weak_text_color()) };
                            let tip = if fav { "Remove from favorites" } else { "Add to favorites" };
                            if ui.add(egui::Button::new(star).frame_when_inactive(false).min_size(Vec2::splat(28.0))).on_hover_text(tip).clicked() {
                                starred = Some((h.clone(), !fav));
                            }
                            if icon(ui, ic::MONITOR, "Remote screen", false).clicked() {
                                go = Some((h.clone(), p.clone(), How::Screen));
                            }
                            if !direct {
                                let b = egui::Button::new(format!("{}  Terminal", ic::TERMINAL)).min_size(Vec2::new(0.0, 28.0));
                                if ui.add(b).on_hover_text("Open an SSH terminal").clicked() {
                                    go = Some((h.clone(), p.clone(), How::Terminal));
                                }
                            }
                        });
                        if r.clicked() {
                            (self.form.host, self.form.port) = (h.clone(), p.clone());
                        }
                        if r.double_clicked() {
                            let how = if h.starts_with("sshremote://") { How::Screen } else { How::Terminal };
                            go = Some((h.clone(), p.clone(), how));
                        }
                        r.on_hover_text("Double-click for a terminal");
                    }
                });
            });
        });

        if let Some((h, on)) = starred {
            crate::hosts::set_favorite(&h, on);
            self.favorites = crate::hosts::favorites();
        }
        if let Some(h) = forget {
            self.recent.retain(|(r, _)| *r != h);
            crate::hosts::save_recent(&self.recent);
        }
        if let Some((h, p, how)) = go {
            match how {
                How::Terminal => self.open_terminal(&h, &p),
                How::System => self.system_terminal(&h, &p),
                How::Screen => {
                    (self.form.host, self.form.port) = (h, p);
                    self.connect(&ctx, false);
                }
                How::TestDesktop => self.connect(&ctx, true),
            }
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
                    if let Some((h, p)) = self.pending_host.take() {
                        self.recent.retain(|(r, _)| *r != h);
                        self.recent.insert(0, (h.clone(), p));
                        self.recent.truncate(8);
                        crate::hosts::save_recent(&self.recent);
                        crate::hosts::mark_seen(&h);
                        self.seen = crate::hosts::last_seen();
                    }
                    let ctl = Control::new(&opt, Mode::True);
                    let waker = ctx.clone();
                    sess.on_message(move || waker.request_repaint());
                    let desk = sess.desk;
                    ctx.send_viewport_cmd(ViewportCommand::Title(format!("SSH Remote - {label}")));
                    let ssh_cmd = self.pending_ssh.take();
                    next = Some(Screen::Remote(Box::new(Remote {
                        ssh_cmd,
                        terms: None,
                        picker: None,
                        sharing: (0, String::new()),
                        click_pick: false,
                        last_shown: None,
                        pick_pressed: false,
                        pick_at: None,
                        label: format!("{label}  {}x{}  {}", desk.0, desk.1, sess.backend),
                        sess,
                        rx,
                        ctl,
                        fb: Framebuffer::default(),
                        tex: None,
                        fit: Fit::Window,
                        pan: Vec2::ZERO,
                        fullscreen: false,
                        pinned: true,
                        bar_x: crate::hosts::setting("toolbar_x").and_then(|v| v.parse().ok()).unwrap_or(0.5),
                        bar_w: 600.0,
                        bar_until: Instant::now(),
                        clip: Clip::new(),
                        share_clipboard: true,
                        view_only: false,
                        held: BTreeSet::new(),
                        mods: Modifiers::NONE,
                        pointer: (u16::MAX, u16::MAX),
                        view: (0, 0),
                        view_changed: None,
                        win_next: false,
                        toasts: Vec::new(),
                        grab_keys: true,
                        uploads: std::collections::VecDeque::new(),
                        next_upload: 1,
                        sound: None,
                        chat: None,
                        chat_lines: Vec::new(),
                        unread: 0,
                        paused: false,
                        monitors: Vec::new(),
                        monitor: 0,
                        inbox: crate::files::Inbox::default(),
                        downloads: Vec::new(),
                        files: None,
                        fetch_dirs: Default::default(),
                        next_tag: 1,
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
                            ui.add_space(ui.available_height() * 0.35);
                            ui.add(egui::Spinner::new().size(36.0));
                            ui.add_space(8.0);
                            ui.label(RichText::new(format!("Connecting to {label}")).size(20.0).strong());
                            let step = match secs {
                                0 => "Opening the connection",
                                1..=2 => "Signing in",
                                _ => "Starting the remote agent and the screen capture",
                            };
                            ui.label(RichText::new(format!("{step}...  {secs} s")).weak());
                            if secs >= 10 {
                                ui.label(
                                    RichText::new(
                                        "Still waiting. A host that needs a password gets it on the connect screen.",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            ui.add_space(12.0);
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
                {
                    // As in RustDesk: system keys go to the remote host while the pointer is over its picture.
                    let over = ctx.pointer_hover_pos().is_some_and(|p| r.last_shown.is_some_and(|s| s.contains(p)));
                    crate::keygrab::set_active(r.grab_keys && !r.view_only && ctx.input(|i| i.focused) && over);
                }
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
                    ctx.send_viewport_cmd(ViewportCommand::Title("SSH Remote".into()));
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
        self.side_windows(&ctx);
    }
}

impl Remote {
    fn send(&mut self, m: ToAgent) -> Result<(), String> {
        self.sess.send(m).map_err(|e| format!("connection lost: {e}"))
    }

    fn notify(&mut self, text: &str) {
        let l = text.to_lowercase();
        let warn =
            ["fail", "error", "unavailable", "cannot", "could not", "denied", "sound:"].iter().any(|w| l.contains(w));
        self.toasts.push((text.to_string(), warn, Instant::now()));
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
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
        let area = if self.pinned { screen.with_min_y(screen.top() + 42.0) } else { screen };
        ui.painter().rect_filled(screen, CornerRadius::ZERO, Color32::from_rgb(18, 19, 23));
        ui.painter().rect_filled(area, CornerRadius::ZERO, Color32::BLACK);
        self.resize(area.size() * ppp)?;
        self.edge_pan(&ctx, area);
        let shown = self.picture(ui, area, ppp);
        self.last_shown = shown;
        let toolbar_rect = self.toolbar(&ctx, screen)?;
        let chat_rect = self.chat_panel(&ctx)?;
        self.upload_overlay(&ctx, screen);
        self.draw_toasts(&ctx, screen);
        self.file_window(&ctx)?;
        self.term_window(&ctx)?;
        self.share_picker(&ctx)?;
        if self.pick_at.is_some_and(|t| Instant::now() >= t) {
            self.pick_at = None;
            self.send(ToAgent::Share { kind: 3, id: 0 })?;
        }
        if self.paused || self.view_only {
            // A badge on the picture: it is a still image until resumed, or input is off.
            let text = if self.paused {
                format!("{}  Paused   ·   {} resumes, {} fetches one frame", ic::PAUSE, ic::PLAY, ic::ARROW_CLOCKWISE)
            } else {
                format!("{}  View only   ·   turn it off in Options {}", ic::EYE, ic::GEAR)
            };
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("paused-badge")));
            let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), Color32::WHITE);
            let r = Rect::from_min_size(
                Pos2::new(screen.left() + 12.0, screen.bottom() - galley.size().y - 30.0),
                galley.size() + Vec2::new(24.0, 14.0),
            );
            painter.rect_filled(r, CornerRadius::same(8), Color32::from_rgba_unmultiplied(20, 22, 28, 220));
            painter.galley(r.min + Vec2::new(12.0, 7.0), galley, Color32::WHITE);
        }
        if self.click_pick {
            // A hint over the picture while the next click chooses the window.
            let text = format!("{}  Click the window to share   ·   Esc cancels", ic::CURSOR_CLICK);
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("click-pick")));
            let galley = painter.layout_no_wrap(text, FontId::proportional(15.0), Color32::WHITE);
            let r = Rect::from_center_size(
                Pos2::new(screen.center().x, screen.top() + 80.0),
                galley.size() + Vec2::new(32.0, 18.0),
            );
            painter.rect_filled(r, CornerRadius::same(10), ACCENT);
            painter.galley(r.min + Vec2::new(16.0, 9.0), galley, Color32::WHITE);
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let ui_rects: Vec<Rect> = toolbar_rect.into_iter().chain(chat_rect).collect();
        if self.view_only {
            self.release_all()?;
        } else {
            self.input(&ctx, shown, &ui_rects)?;
        }
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
                Ok(Ev::Net(ToClient::Snapshot(png))) => {
                    let path = crate::shot::save_path();
                    let msg = match std::fs::write(&path, &png) {
                        Ok(()) => format!("Screenshot saved to {}", path.display()),
                        Err(e) => format!("Screenshot: {e}"),
                    };
                    self.notify(&msg);
                }
                Ok(Ev::Net(ToClient::Chat(t))) => match self.chat.as_mut() {
                    Some((lines, _)) => lines.push((true, t)),
                    None => {
                        self.notify(&format!("Message: {t}"));
                        self.chat_lines.push((true, t));
                        self.unread += 1;
                    }
                },
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
                    if let Some(f) = self.files.as_mut() {
                        f.remote.set_list(path, entries, error);
                    }
                }
                Ok(Ev::Net(ToClient::WindowList { list, error })) => {
                    if let Some(p) = self.picker.as_mut() {
                        p.0 = Some(if error.is_empty() { Ok(list) } else { Err(error) });
                    }
                }
                Ok(Ev::Net(ToClient::Sharing { kind, text })) => {
                    if kind != self.sharing.0 || text != self.sharing.1 {
                        self.notify(&format!("Now sharing {text}"));
                    }
                    self.sharing = (kind, text);
                }
                Ok(Ev::Net(ToClient::TermOutput { id, data })) => {
                    if let Some(t) = self.terms.as_mut() {
                        t.output(id, &data);
                    }
                }
                Ok(Ev::Net(ToClient::TermExit { id, text })) => match self.terms.as_mut() {
                    Some(t) => t.exited(id, text),
                    None => self.notify(&format!("Terminal: {text}")),
                },
                Ok(Ev::Net(ToClient::OpDone { ok, text })) => match self.files.as_mut() {
                    Some(f) => {
                        let cmd = f.op_done(ok, &text);
                        self.file_cmd(cmd)?;
                    }
                    None => self.notify(&text),
                },
                Ok(Ev::Net(ToClient::FetchStart { id, size, name })) => {
                    if let Some(crate::files::Note::Done { text, .. }) = self.inbox.start(id, size, &name) {
                        self.notify(&format!("{name}: {text}"));
                    } else {
                        self.downloads.push((id, name, 0));
                    }
                }
                Ok(Ev::Net(ToClient::FetchBegin { id, size, tag, name })) => {
                    let dir = self.fetch_dirs.get(&tag).cloned().unwrap_or_else(crate::files::downloads);
                    if let Some(crate::files::Note::Done { text, .. }) = self.inbox.start_in(id, size, &dir, &name) {
                        self.notify(&format!("{name}: {text}"));
                    } else {
                        self.downloads.push((id, name, tag));
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
                    if id != 0 && !self.downloads.iter().any(|d| d.0 == id) {
                        // Already cancelled here.
                        continue;
                    }
                    let name = self.downloads.iter().find(|d| d.0 == id).map(|d| d.1.clone()).unwrap_or_default();
                    let tagged = self.downloads.iter().any(|d| d.0 == id && d.2 != 0);
                    self.downloads.retain(|d| d.0 != id);
                    let msg = match self.inbox.end(id, ok, &text) {
                        Some(crate::files::Note::Done { ok: true, text, .. }) => format!("Downloaded {name} to {text}"),
                        Some(crate::files::Note::Done { text, .. }) => format!("{name} failed: {text}"),
                        _ => format!("Download failed: {text}"),
                    };
                    // After the file is closed, so the listing shows its full size.
                    if tagged && let Some(f) = self.files.as_mut() {
                        f.refresh_local();
                    }
                    self.notify(&msg);
                }
                Ok(Ev::Net(ToClient::FileDone { id, ok, text })) => {
                    let name = self.uploads.iter().find(|u| u.id == id).map(|u| u.name.clone()).unwrap_or_default();
                    let cancelled = self.uploads.iter().any(|u| u.id == id && u.cancelled);
                    let into_folder = self.uploads.iter().any(|u| u.id == id && u.dir.is_some());
                    self.uploads.retain(|u| u.id != id);
                    if into_folder && let Some(f) = self.files.as_ref() {
                        let cmd = f.refresh_remote();
                        self.file_cmd(cmd)?;
                    }
                    let msg = if cancelled {
                        format!("{name}: upload cancelled")
                    } else if ok {
                        format!("{name} saved on the SSH host: {text}")
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
                self.queue_upload(&path, name, None)?;
            }
        }
        Ok(())
    }

    /// Uploads files or whole folders into `dir` on the remote host.
    fn upload_into(&mut self, paths: &[std::path::PathBuf], dir: &str) -> Result<(), String> {
        for path in paths {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let mut files = Vec::new();
            if path.is_dir() {
                crate::files::walk(path, &name, &mut files);
            } else {
                files.push((path.clone(), name));
            }
            for (path, name) in files {
                self.queue_upload(&path, name, Some(dir.to_string()))?;
            }
        }
        Ok(())
    }

    fn queue_upload(&mut self, path: &std::path::Path, name: String, dir: Option<String>) -> Result<(), String> {
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
        match &dir {
            Some(d) => self.send(ToAgent::FileStartIn { id, size, dir: d.clone(), name: name.clone() })?,
            None => self.send(ToAgent::FileStart { id, size, name: name.clone() })?,
        }
        self.uploads.push_back(Upload { id, name, dir, cancelled: false, file, size, sent: 0, acked: 0, ended: false });
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

    /// The chat panel; returns its rectangle while open.
    fn chat_panel(&mut self, ctx: &egui::Context) -> Result<Option<Rect>, String> {
        let Some((lines, input)) = self.chat.as_mut() else { return Ok(None) };
        let mut open = true;
        let mut sent = None;
        let resp = egui::Window::new("Chat")
            .open(&mut open)
            .default_pos(ctx.content_rect().left_top() + Vec2::new(20.0, 70.0))
            .default_size([340.0, 420.0])
            .show(ctx, |ui| {
                let h = ui.available_height().max(120.0) - 64.0;
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .max_height(h)
                    .show(ui, |ui| crate::chat::show_lines(ui, lines, "Remote", "You"));
                sent = crate::chat::input_row(ui, input, true);
            });
        let rect = resp.map(|r| r.response.rect);
        if let Some(text) = sent {
            if let Some((lines, _)) = self.chat.as_mut() {
                lines.push((false, text.clone()));
            }
            self.send(ToAgent::Chat(text))?;
        }
        if !open && let Some((lines, _)) = self.chat.take() {
            self.chat_lines = lines;
        }
        Ok(rect)
    }

    /// Shows the file transfer window and carries out what it asks for.
    fn file_window(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some(mut f) = self.files.take() else { return Ok(()) };
        let mut transfers: Vec<crate::filewin::Transfer> = self
            .uploads
            .iter()
            .filter(|u| !u.cancelled)
            .map(|u| crate::filewin::Transfer {
                upload: true,
                id: u.id,
                name: u.name.clone(),
                done: u.acked,
                size: u.size,
            })
            .collect();
        for (id, name, _) in &self.downloads {
            let (done, size) = self.inbox.received(*id).unwrap_or((0, 0));
            transfers.push(crate::filewin::Transfer { upload: false, id: *id, name: name.clone(), done, size });
        }
        let host = self.label.split("  ").next().unwrap_or(&self.label).to_string();
        let cmds = f.show(ctx, &host, &transfers);
        self.files = Some(f);
        for c in cmds {
            self.file_cmd(c)?;
        }
        Ok(())
    }

    /// Chooses what the remote host shares, as in a video call: a screen, a window or a program.
    fn share_picker(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some((list, tab)) = self.picker.as_mut() else { return Ok(()) };
        let mut pick: Option<ToAgent> = None;
        let mut close = false;
        let mut click_pick = false;
        let monitors = self.monitors.clone();
        let current = self.monitor;
        let modal = egui::Modal::new(egui::Id::unique("sshremote-share-picker")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.set_min_height(440.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Choose what to share").size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon(ui, ic::X, "Close", false).clicked() {
                        close = true;
                    }
                });
            });
            ui.label(RichText::new("The SSH host sends only what you choose; clicks and keys reach only that.").weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (i, (g, name)) in
                    [(ic::MONITOR, "Entire screen"), (ic::APP_WINDOW, "Window"), (ic::SQUARES_FOUR, "Application")]
                        .into_iter()
                        .enumerate()
                {
                    if ui.selectable_label(*tab == i as u8, format!("{g}  {name}")).clicked() {
                        *tab = i as u8;
                    }
                }
            });
            ui.separator();
            let row = |ui: &mut egui::Ui, glyph: &str, title: &str, sub: &str, on: bool| -> bool {
                let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), egui::Sense::click());
                let fill = if on {
                    ACCENT.gamma_multiply(0.3)
                } else if r.hovered() {
                    ui.visuals().widgets.hovered.bg_fill
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
                let text = ui.visuals().text_color();
                let weak = ui.visuals().weak_text_color();
                ui.painter().text(
                    rect.left_center() + Vec2::new(14.0, 0.0),
                    Align2::LEFT_CENTER,
                    glyph,
                    FontId::proportional(20.0),
                    text,
                );
                let galley =
                    ui.painter().layout(title.to_string(), FontId::proportional(14.0), text, rect.width() - 70.0);
                ui.painter().galley(rect.min + Vec2::new(46.0, 5.0), galley, text);
                ui.painter().text(
                    rect.min + Vec2::new(46.0, 25.0),
                    Align2::LEFT_TOP,
                    sub,
                    FontId::proportional(11.5),
                    weak,
                );
                r.clicked()
            };
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(330.0).show(ui, |ui| match *tab {
                0 => {
                    let screens: Vec<(String, String)> = if monitors.is_empty() {
                        vec![(String::new(), "The only screen".into())]
                    } else {
                        monitors
                            .iter()
                            .map(|m| {
                                (m.name.clone(), format!("{}x{}{}", m.w, m.h, if m.primary { ", primary" } else { "" }))
                            })
                            .collect()
                    };
                    for (i, (name, sub)) in screens.iter().enumerate() {
                        let on = self.sharing.0 == 0 && i == current;
                        if row(ui, ic::MONITOR, format!("Screen {}  {name}", i + 1).trim(), sub, on) {
                            pick = Some(if i == current || monitors.len() < 2 {
                                ToAgent::Share { kind: 0, id: 0 }
                            } else {
                                ToAgent::SelectMonitor(i as u8)
                            });
                        }
                    }
                }
                t => {
                    // The window in front, or its program: no list needed.
                    let no_helper = matches!(list, Some(Err(e)) if e.starts_with(crate::capture::NEEDS_HELPER));
                    if t == 1 {
                        let sub = "The window in front on the SSH host; click it there first";
                        if row(ui, ic::APP_WINDOW, "Active window", sub, false) {
                            pick = Some(ToAgent::Share { kind: 3, id: 0 });
                        }
                        // Pointing at the window itself: the click brings it to the front, then it is shared.
                        let sub = "Click the window you want in the picture of the SSH host";
                        if row(ui, ic::CURSOR_CLICK, "Click a window to share", sub, false) {
                            click_pick = true;
                        }
                    } else if no_helper {
                        ui.add_enabled_ui(false, |ui| {
                            row(
                                ui,
                                ic::SQUARES_FOUR,
                                "Active application",
                                "Needs the sshremote helper on GNOME",
                                false,
                            )
                        });
                    } else {
                        let sub = "Every window of the program in front on the SSH host";
                        if row(ui, ic::SQUARES_FOUR, "Active application", sub, false) {
                            pick = Some(ToAgent::Share { kind: 4, id: 0 });
                        }
                    }
                    ui.separator();
                    share_list(ui, list, t, &row, &mut pick);
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if click_pick {
            self.click_pick = true;
            self.pick_pressed = false;
            close = true;
            if self.sharing.0 != 0 {
                // The whole screen first, so every window can be seen and clicked.
                self.send(ToAgent::Share { kind: 0, id: 0 })?;
            }
        }
        if let Some(m) = pick {
            // The install answer comes back as the list's message, so the chooser stays open for it.
            let installing = m == ToAgent::InstallHelper;
            self.send(m)?;
            close = !installing;
            if installing && let Some(p) = self.picker.as_mut() {
                p.0 = None;
            }
        }
        if close {
            self.picker = None;
        }
        Ok(())
    }

    fn term_window(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some(mut t) = self.terms.take() else { return Ok(()) };
        let host = self.label.split("  ").next().unwrap_or(&self.label).to_string();
        let cmds = t.show(ctx, &host);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Close => self.terms = None,
            }
        }
        Ok(())
    }

    fn file_cmd(&mut self, c: crate::filewin::Cmd) -> Result<(), String> {
        use crate::filewin::Cmd;
        match c {
            Cmd::Send(m) => self.send(m)?,
            Cmd::Upload { paths, to } => self.upload_into(&paths, &to)?,
            Cmd::Download { paths, to } => {
                for path in paths {
                    let tag = self.next_tag;
                    self.next_tag += 1;
                    self.fetch_dirs.insert(tag, to.clone());
                    self.send(ToAgent::FetchTagged { tag, path })?;
                }
            }
            Cmd::Cancel(true, id) => {
                if let Some(u) = self.uploads.iter_mut().find(|u| u.id == id) {
                    u.cancelled = true;
                    if !u.ended {
                        u.ended = true;
                        self.send(ToAgent::FileEnd { id })?;
                    }
                }
            }
            Cmd::Cancel(false, id) => {
                if let Some(&(_, ref name, tag)) = self.downloads.iter().find(|d| d.0 == id) {
                    let name = name.clone();
                    self.send(ToAgent::FetchCancel { id, tag })?;
                    self.inbox.end(id, false, "cancelled");
                    self.downloads.retain(|d| d.0 != id);
                    self.notify(&format!("{name}: download cancelled"));
                }
            }
            Cmd::Close => self.files = None,
        }
        Ok(())
    }

    /// Stacked notifications at the bottom right that fade out after a few seconds.
    fn draw_toasts(&mut self, ctx: &egui::Context, screen: Rect) {
        self.toasts.retain(|t| t.2.elapsed() < Duration::from_secs(6));
        let mut y = screen.bottom() - 16.0;
        for (i, (text, warn, at)) in self.toasts.iter().enumerate().rev() {
            let fade = (1.0 - (at.elapsed().as_secs_f32() - 5.0).max(0.0)).clamp(0.0, 1.0);
            let id = egui::Id::unique(("toast", i));
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id));
            let galley = painter.layout(text.clone(), FontId::proportional(14.0), Color32::WHITE, 380.0);
            let size = galley.size() + Vec2::new(30.0, 20.0);
            let rect = Rect::from_min_size(Pos2::new(screen.right() - 16.0 - size.x, y - size.y), size);
            painter.rect_filled(
                rect,
                CornerRadius::same(10),
                Color32::from_rgba_unmultiplied(32, 34, 41, (235.0 * fade) as u8),
            );
            let bar = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
            painter.rect_filled(bar, CornerRadius::same(2), (if *warn { WARN } else { ACCENT }).gamma_multiply(fade));
            painter.galley(rect.min + Vec2::new(18.0, 10.0), galley, Color32::WHITE.gamma_multiply(fade));
            y = rect.top() - 8.0;
        }
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// Progress of uploads and the drop hint, bottom left over the remote screen.
    fn upload_overlay(&self, ctx: &egui::Context, screen: Rect) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let mut lines: Vec<String> = self
            .uploads
            .iter()
            .map(|u| format!("Sending {}  {}%", u.name, if u.size == 0 { 100 } else { u.acked * 100 / u.size }))
            .collect();
        for (id, name, _) in &self.downloads {
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
            Fit::Window | Fit::Stretch => (px.x.max(64.0) as u16, px.y.max(64.0) as u16),
            // Below 100% the agent sends fewer pixels; above, the full resolution is enlarged here.
            Fit::Fixed(p) => {
                let k = p.min(100) as f32 / 100.0;
                ((self.sess.desk.0 as f32 * k) as u16, (self.sess.desk.1 as f32 * k) as u16)
            }
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
            Fit::Stretch => area,
            Fit::Fixed(p) => {
                let full = Vec2::new(self.sess.desk.0 as f32, self.sess.desk.1 as f32) * (p as f32 / 100.0) / ppp;
                let over = (full - area.size()).max(Vec2::ZERO);
                self.pan = self.pan.clamp(Vec2::ZERO, over);
                // Centred along an axis where it fits, panned along one where it does not.
                let min = Pos2::new(
                    if over.x > 0.0 { area.min.x - self.pan.x } else { area.center().x - full.x / 2.0 },
                    if over.y > 0.0 { area.min.y - self.pan.y } else { area.center().y - full.y / 2.0 },
                );
                Rect::from_min_size(min, full)
            }
        };
        let painter = ui.painter().with_clip_rect(area);
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        // Where the visible part lies, when the picture is larger than the window.
        let bar = Color32::from_white_alpha(70);
        if rect.width() > area.width() + 0.5 {
            let (a, w) = ((area.min.x - rect.min.x) / rect.width(), area.width() / rect.width());
            let x0 = area.min.x + a * area.width();
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, area.max.y - 5.0), Pos2::new(x0 + w * area.width(), area.max.y - 1.0)),
                CornerRadius::same(2),
                bar,
            );
        }
        if rect.height() > area.height() + 0.5 {
            let (a, h) = ((area.min.y - rect.min.y) / rect.height(), area.height() / rect.height());
            let y0 = area.min.y + a * area.height();
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(area.max.x - 5.0, y0),
                    Pos2::new(area.max.x - 1.0, y0 + h * area.height()),
                ),
                CornerRadius::same(2),
                bar,
            );
        }
        Some(rect)
    }

    /// Moves a picture larger than the window when the pointer rests near an edge.
    fn edge_pan(&mut self, ctx: &egui::Context, area: Rect) {
        if !matches!(self.fit, Fit::Fixed(_)) {
            return;
        }
        let Some(p) = ctx.pointer_hover_pos().filter(|p| area.contains(*p)) else { return };
        const EDGE: f32 = 28.0;
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        let mut d = Vec2::ZERO;
        if p.x < area.min.x + EDGE {
            d.x = -1.0;
        } else if p.x > area.max.x - EDGE {
            d.x = 1.0;
        }
        if p.y < area.min.y + EDGE {
            d.y = -1.0;
        } else if p.y > area.max.y - EDGE {
            d.y = 1.0;
        }
        if d != Vec2::ZERO {
            self.pan += d * 900.0 * dt;
            ctx.request_repaint();
        }
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
        // Pointer events over any of this window's own controls (toolbar, menus, panels) stay here.
        let over_toolbar = |p: Pos2| {
            ui_rects.iter().any(|t| t.contains(p))
                || ctx.layer_id_at(p).is_some_and(|l| l.order != egui::Order::Background)
        };
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
            if self.click_pick && matches!(e, Event::Key { key: Key::Escape, .. }) {
                if matches!(e, Event::Key { pressed: true, .. }) {
                    self.click_pick = false;
                    self.notify("Choosing a window cancelled");
                }
                continue;
            }
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
                    if self.click_pick && b == 1 && pressed {
                        self.pick_pressed = true;
                    }
                    if self.click_pick && self.pick_pressed && b == 1 && !pressed {
                        self.pick_pressed = false;
                        // Once the window has come to the front, it becomes what is shared.
                        self.click_pick = false;
                        self.pick_at = Some(Instant::now() + Duration::from_millis(400));
                    }
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
    /// Moves the toolbar along the top edge by `dx` points; remembered when the drag ends.
    fn drag_bar(&mut self, ctx: &egui::Context, r: &egui::Response, screen: Rect) {
        if r.dragged() {
            self.bar_x = (self.bar_x + r.drag_delta().x / screen.width().max(1.0)).clamp(0.0, 1.0);
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if r.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
        if r.drag_stopped() {
            crate::hosts::set_setting("toolbar_x", &format!("{:.3}", self.bar_x));
        }
    }

    /// The toolbar's left edge for its remembered place, kept inside the window.
    fn bar_left(&self, screen: Rect, width: f32) -> f32 {
        let centre = screen.left() + self.bar_x * screen.width();
        (centre - width / 2.0).clamp(screen.left() + 4.0, (screen.right() - width - 4.0).max(screen.left() + 4.0))
    }

    /// The toolbar, or while it is tucked away a small tab hanging from the top edge (as in Jump
    /// Desktop) that brings it back when the pointer reaches it; returns the toolbar's rectangle.
    fn toolbar(&mut self, ctx: &egui::Context, screen: Rect) -> Result<Option<Rect>, String> {
        let id = egui::Id::unique("toolbar");
        let handle_id = egui::Id::unique("toolbar-handle");
        let shown_a_while = Instant::now() < self.bar_until;
        if !(self.pinned || shown_a_while || menu_open(ctx) || ctx.any_popup_open()) {
            let size = Vec2::new(64.0, 18.0);
            let left = self.bar_left(screen, size.x);
            let rect = Rect::from_min_size(Pos2::new(left, screen.top()), size);
            let dot = match self.ctl.rtt {
                r if r < 80.0 => GOOD,
                r if r < 250.0 => WARN,
                _ => BAD,
            };
            let resp = egui::Area::new(handle_id).fixed_pos(rect.min).order(egui::Order::Foreground).show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                let fill = ctx.global_style().visuals.window_fill;
                let radius = CornerRadius { nw: 0, ne: 0, sw: 9, se: 9 };
                ui.painter().rect_filled(r, radius, fill.gamma_multiply(if resp.hovered() { 1.0 } else { 0.9 }));
                ui.painter().rect_stroke(
                    r,
                    radius,
                    Stroke::new(1.0, Color32::from_white_alpha(30)),
                    egui::StrokeKind::Inside,
                );
                ui.painter().circle_filled(r.left_center() + Vec2::new(14.0, 0.0), 3.5, dot);
                let fg = ctx.global_style().visuals.text_color();
                ui.painter().text(
                    r.center() + Vec2::new(6.0, 0.0),
                    Align2::CENTER_CENTER,
                    ic::CARET_DOWN,
                    FontId::proportional(13.0),
                    fg,
                );
                resp.on_hover_text("Toolbar (not pinned). Point here to show it; drag to move it along the edge.")
            });
            let r = resp.inner;
            self.drag_bar(ctx, &r, screen);
            // Hovering opens the toolbar, which then stays while the pointer is on it.
            if r.hovered() && !r.dragged() && !r.drag_stopped() {
                self.bar_until = Instant::now() + Duration::from_millis(700);
                ctx.request_repaint();
            }
            return Ok(None);
        }
        let mut action: Option<Deferred> = None;
        let left = self.bar_left(screen, self.bar_w);
        let mut grip: Option<egui::Response> = None;
        let resp = egui::Area::new(id)
            .fixed_pos(Pos2::new(left, screen.top() + 4.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(ctx.global_style().visuals.window_fill.gamma_multiply(0.96))
                    .stroke(Stroke::new(1.0, Color32::from_white_alpha(25)))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(6, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let g = ui.add(
                                egui::Label::new(RichText::new(ic::GRIP).size(16.0).weak())
                                    .sense(egui::Sense::click_and_drag())
                                    .selectable(false),
                            );
                            grip = Some(g.on_hover_text("Drag to move the toolbar"));
                            self.toolbar_items(ui, &mut action)
                        })
                    });
            });
        if let Some(g) = grip {
            self.drag_bar(ctx, &g, screen);
        }
        let rect = resp.response.rect;
        self.bar_w = rect.width();
        if resp.response.contains_pointer() {
            self.bar_until = Instant::now() + Duration::from_millis(500);
        }
        if !self.pinned {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(f) = action {
            f(self, ctx)?;
        }
        Ok(Some(rect))
    }

    fn toolbar_items(&mut self, ui: &mut egui::Ui, action: &mut Option<Deferred>) {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
        // Connection quality at a glance; details on hover.
        let (dot, state) = match self.ctl.rtt {
            r if r < 80.0 => (GOOD, "good"),
            r if r < 250.0 => (WARN, "slow"),
            _ => (BAD, "poor"),
        };
        let details = format!(
            "Connection {state}\nRound trip {:.0} ms, {:.0} KB/s, {:.0} fps\n{}\n{}",
            self.ctl.rtt,
            self.ctl.kbps,
            self.ctl.fps_now,
            self.label,
            self.ctl.describe()
        );
        ui.label(RichText::new("●").size(14.0).color(dot)).on_hover_text(&details);
        let host = self.label.split("  ").next().unwrap_or(&self.label).to_string();
        ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(host).strong().size(13.0)).truncate())
            .on_hover_text(&details);
        // Fixed width, so a round trip going from 9 to 120 ms does not shift the buttons.
        let rtt = if self.ctl.rtt >= 999.5 { "999+ ms".to_string() } else { format!("{:.0} ms", self.ctl.rtt) };
        ui.add_sized([46.0, 20.0], egui::Label::new(RichText::new(rtt).small().weak().monospace()))
            .on_hover_text(&details);
        if self.sharing.0 != 0 {
            let name: String = self.sharing.1.chars().take(18).collect();
            let chip = egui::Button::new(
                RichText::new(format!("{}  {name}  {}", ic::APP_WINDOW, ic::X)).small().color(Color32::WHITE),
            )
            .fill(ACCENT)
            .corner_radius(CornerRadius::same(10));
            if ui
                .add(chip)
                .on_hover_text(format!("Sharing {}. Click to share the whole screen again.", self.sharing.1))
                .clicked()
            {
                *action = Some(Box::new(|r, _| r.send(ToAgent::Share { kind: 0, id: 0 })));
            }
        }
        ui.separator();

        let display = icon_menu(ui, ic::MONITOR, |ui| {
            ui.label(RichText::new("Quality").strong());
            let auto = self.ctl.auto.is_some();
            if ui.selectable_label(auto, "Auto (adapts to the connection)").clicked() {
                *action = Some(Box::new(|r, _| r.menu('a')));
                ui.close();
            }
            for (i, p) in PRESETS.iter().enumerate() {
                let on = !auto && self.ctl.preset == p.name;
                if ui.selectable_label(on, preset_text(p)).clicked() {
                    let c = char::from(b'0' + i as u8);
                    *action = Some(Box::new(move |r, _| r.menu(c)));
                    ui.close();
                }
            }
            ui.collapsing("Custom", |ui| {
                let mut mode = self.ctl.s.mode;
                ui.label(RichText::new("Colours").weak());
                ui.horizontal_wrapped(|ui| {
                    for m in Mode::ALL {
                        ui.selectable_value(&mut mode, m, m.label());
                    }
                });
                let mut scale = self.ctl.s.scale;
                ui.add(egui::Slider::new(&mut scale, 20..=100).text("Resolution %"));
                let mut fps = self.ctl.s.fps;
                ui.add(egui::Slider::new(&mut fps, 1..=60).text("Frames per second"));
                if (mode, scale, fps) != (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) {
                    *action = Some(Box::new(move |r, _| r.custom(mode, scale, fps)));
                }
            });
            ui.separator();
            ui.label(RichText::new("View").strong());
            let mut pick = |ui: &mut egui::Ui, fit: Fit, text: &str| {
                if ui.selectable_label(self.fit == fit, text).clicked() {
                    *action = Some(Box::new(move |r, _| {
                        (r.fit, r.pan) = (fit, Vec2::ZERO);
                        Ok(())
                    }));
                    ui.close();
                }
            };
            pick(ui, Fit::Window, "Fit to window");
            pick(ui, Fit::Stretch, "Stretch to fill");
            ui.horizontal_wrapped(|ui| {
                for p in [50, 75, 100, 125, 150, 200] {
                    pick(ui, Fit::Fixed(p), &format!("{p}%"));
                }
            });
            if self.monitors.len() > 1 {
                ui.separator();
                ui.label(RichText::new("Screen").strong());
                for (i, m) in self.monitors.iter().enumerate() {
                    let primary = if m.primary { ", primary" } else { "" };
                    let text = format!("{}  {} ({}x{}{primary})", i + 1, m.name, m.w, m.h);
                    if ui.selectable_label(i == self.monitor, text).clicked() {
                        *action = Some(Box::new(move |r, _| r.send(ToAgent::SelectMonitor(i as u8))));
                        ui.close();
                    }
                }
            }
        });
        display.on_hover_text(format!("Display: {}; view {}", self.ctl.describe(), self.fit.label()));

        if icon(ui, ic::APP_WINDOW, "Share: the entire screen, one window or one application", self.sharing.0 != 0)
            .clicked()
        {
            *action = Some(Box::new(|r, _| {
                r.picker = Some((None, if r.sharing.0 == 0 { 0 } else { 1 }));
                r.send(ToAgent::ListWindows)
            }));
        }
        let fs_tip = if self.fullscreen { "Leave full screen (F11)" } else { "Full screen (F11)" };
        let fs_glyph = if self.fullscreen { ic::CORNERS_IN } else { ic::CORNERS_OUT };
        if icon(ui, fs_glyph, fs_tip, self.fullscreen).clicked() {
            *action = Some(Box::new(|r, c| {
                r.set_fullscreen(c, !r.fullscreen);
                Ok(())
            }));
        }
        let keys = icon_menu(ui, ic::KEYBOARD, |ui| {
            let combos: [(&str, &str, Option<Vec<u32>>); 9] = [
                ("Secure attention", "Ctrl+Alt+Del", None),
                ("Lock screen", "Win+L", Some(vec![SUPER_L, 'l' as u32])),
                ("Start menu", "Win", Some(vec![SUPER_L])),
                ("Run", "Win+R", Some(vec![SUPER_L, 'r' as u32])),
                ("File explorer", "Win+E", Some(vec![SUPER_L, 'e' as u32])),
                ("Show desktop", "Win+D", Some(vec![SUPER_L, 'd' as u32])),
                ("Switch window", "Alt+Tab", Some(vec![ALT_L, 0xff09])),
                ("Close window", "Alt+F4", Some(vec![ALT_L, 0xffc1])),
                ("Print screen", "PrtSc", Some(vec![0xff61])),
            ];
            for (label, shortcut, keys) in combos {
                if ui.add(egui::Button::new(label).shortcut_text(shortcut)).clicked() {
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
        keys.on_hover_text("Send special keys");
        ui.separator();

        let files_tip = "File transfer: this computer and the SSH host side by side";
        if icon(ui, ic::FOLDER, files_tip, self.files.is_some()).clicked() {
            *action = Some(Box::new(|r, _| {
                if r.files.take().is_none() {
                    r.files = Some(crate::filewin::FileWindow::new());
                    r.send(ToAgent::ListDir { path: String::new() })?;
                }
                Ok(())
            }));
        }
        if icon(ui, ic::TERMINAL, "Terminal: a shell on the SSH host", self.terms.is_some()).clicked() {
            *action = Some(Box::new(|r, _| {
                match r.terms.take() {
                    Some(t) => {
                        for c in t.close_all() {
                            if let crate::termwin::Cmd::Send(m) = c {
                                r.send(m)?;
                            }
                        }
                    }
                    None => r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone())),
                }
                Ok(())
            }));
        }
        let chat_glyph = if self.unread > 0 { format!("{} {}", ic::CHAT, self.unread) } else { ic::CHAT.into() };
        if icon(ui, &chat_glyph, "Chat with the person at the SSH host's screen", self.chat.is_some()).clicked() {
            *action = Some(Box::new(|r, _| {
                match r.chat.take() {
                    Some((lines, _)) => r.chat_lines = lines,
                    None => {
                        r.chat = Some((std::mem::take(&mut r.chat_lines), String::new()));
                        r.unread = 0;
                    }
                }
                Ok(())
            }));
        }
        if icon(ui, ic::CAMERA, "Screenshot: save the SSH host's screen as a PNG in Pictures", false).clicked() {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Snapshot)));
        }
        let (glyph, tip) = if self.paused {
            (ic::PLAY, "Resume the picture")
        } else {
            (ic::PAUSE, "Pause the picture to save traffic; fetch single frames when needed")
        };
        // Paused: a filled blue play button, so the frozen state is plain to see.
        let b = if self.paused {
            egui::Button::new(RichText::new(glyph).size(16.0).color(Color32::WHITE))
                .fill(ACCENT)
                .min_size(Vec2::splat(28.0))
        } else {
            tool_button(glyph, false)
        };
        if ui.add(b).on_hover_text(tip).clicked() {
            let on = !self.paused;
            self.paused = on;
            *action = Some(Box::new(move |r, _| r.send(ToAgent::Pause(on))));
        }
        if self.paused && icon(ui, ic::ARROW_CLOCKWISE, "Fetch one frame", false).clicked() {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Refresh)));
        }

        let mut sound = self.sound.is_some();
        let mut sound_clicked = false;
        let mut dark = ui.visuals().dark_mode;
        let mut theme_clicked = false;
        let more = icon_menu(ui, ic::GEAR, |ui| {
            ui.checkbox(&mut self.view_only, "View only").on_hover_text("Watch without sending keys, clicks or the wheel");
            ui.checkbox(&mut self.share_clipboard, "Share clipboard").on_hover_text("Copied text goes both ways");
            sound_clicked = ui.checkbox(&mut sound, "Sound (about 8 KB/s)").clicked();
            if cfg!(windows) {
                let tip = "While the pointer is over the picture, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Print Screen go to the SSH host instead of this PC";
                ui.checkbox(&mut self.grab_keys, "Send system keys").on_hover_text(tip);
            }
            theme_clicked = ui.checkbox(&mut dark, "Dark theme").clicked();
            ui.separator();
            if ui.button("Repaint the whole screen").clicked() {
                *action = Some(Box::new(|r, _| r.menu('r')));
                ui.close();
            }
        });
        more.on_hover_text("Options");
        if theme_clicked {
            set_theme(ui.ctx(), dark);
        }
        if sound_clicked {
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
        // Pinned: a filled, coloured pin on a highlighted button; not pinned: a struck-through pin.
        let (pin, tip) = if self.pinned {
            (
                RichText::new(ic::PUSH_PIN).size(16.0).family(filled()).color(Color32::WHITE),
                "Pinned: the toolbar stays. Click to unpin; it then folds into a tab at the top edge.",
            )
        } else {
            (
                RichText::new(ic::PUSH_PIN_SLASH).size(16.0).color(ui.visuals().weak_text_color()),
                "Not pinned: the toolbar folds into a tab at the top edge. Click to pin it.",
            )
        };
        let b =
            egui::Button::new(pin).min_size(Vec2::splat(28.0)).frame_when_inactive(self.pinned).selected(self.pinned);
        if ui.add(b).on_hover_text(tip).clicked() {
            self.pinned = !self.pinned;
            self.notify(if self.pinned {
                "Toolbar pinned"
            } else {
                "Toolbar unpinned: it folds into the tab at the top"
            });
        }
        ui.separator();
        let end = egui::Button::new(RichText::new(ic::X).size(15.0).color(Color32::WHITE))
            .fill(BAD)
            .min_size(Vec2::splat(28.0));
        if ui.add(end).on_hover_text("Disconnect").clicked() {
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

pub(crate) fn human(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

/// The windows or programs to pick from, or why there is no list.
fn share_list(
    ui: &mut egui::Ui,
    list: &Option<Result<Vec<crate::capture::WindowInfo>, String>>,
    t: u8,
    row: &dyn Fn(&mut egui::Ui, &str, &str, &str, bool) -> bool,
    pick: &mut Option<ToAgent>,
) {
    match list {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Asking the SSH host for its windows...");
            });
        }
        Some(Err(e)) => {
            ui.add_space(8.0);
            ui.label(e.as_str());
            if e.starts_with(crate::capture::NEEDS_HELPER) && e.contains("Install it") {
                ui.add_space(6.0);
                let b = egui::Button::new(RichText::new("Install the helper on the SSH host").color(Color32::WHITE))
                    .fill(ACCENT);
                if ui.add(b).clicked() {
                    *pick = Some(ToAgent::InstallHelper);
                }
            }
        }
        Some(Ok(wins)) if wins.is_empty() => {
            ui.label(RichText::new("No windows are open on the SSH host.").weak());
        }
        Some(Ok(wins)) if t == 1 => {
            for w in wins {
                let sub = format!("{}  {}x{}", w.app, w.w, w.h);
                if row(ui, ic::APP_WINDOW, &w.title, &sub, false) {
                    *pick = Some(ToAgent::Share { kind: 1, id: w.id });
                }
            }
        }
        Some(Ok(wins)) => {
            let mut apps: Vec<(String, u64, usize)> = Vec::new();
            for w in wins {
                match apps.iter_mut().find(|a| a.0 == w.app) {
                    Some(a) => a.2 += 1,
                    None => apps.push((w.app.clone(), w.id, 1)),
                }
            }
            for (app, id, n) in apps {
                let sub = if n == 1 { "1 window".to_string() } else { format!("{n} windows") };
                if row(ui, ic::SQUARES_FOUR, &app, &sub, false) {
                    *pick = Some(ToAgent::Share { kind: 2, id });
                }
            }
        }
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
}
