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
use workspace::{Launch, PaneMode, Place};
use crate::transport::Target;

pub(crate) mod chrome;
mod home;
mod remote;
pub(crate) mod workspace;
pub(crate) mod theme;
#[cfg(test)]
mod snapshot;

use remote::Remote;
use theme::filled;
pub(crate) use theme::primary;
pub use theme::{set_theme, style};

pub(crate) const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);

/// Phosphor icon codepoints (assets/fonts, MIT licensed).
pub(crate) mod ic {
    pub const ARROWS_OUT: &str = "\u{E0A2}";
    pub const ARROWS_IN: &str = "\u{E09A}";
    pub const SPLIT_SIDE: &str = "\u{E870}";
    pub const SPLIT_BELOW: &str = "\u{E874}";
    pub const CARET_LEFT: &str = "\u{E138}";
    pub const CARET_RIGHT: &str = "\u{E13A}";
    pub const DESKTOP: &str = "\u{E560}";
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
    pub const CHECK: &str = "\u{E182}";
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
    pub const SEND: &str = "\u{E396}";
    pub const CORNERS_IN: &str = "\u{E1CE}";
    pub const CORNERS_OUT: &str = "\u{E1D0}";
    pub const FILE: &str = "\u{E230}";
    pub const FOLDER: &str = "\u{E24A}";
    pub const LINK_BREAK: &str = "\u{E2E4}";
    pub const ARCHIVE: &str = "\u{E00C}";
    pub const LOCK_KEY: &str = "\u{E2FE}";
    pub const SHIELD: &str = "\u{E40A}";
    pub const SHIELD_CHECK: &str = "\u{E40C}";
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

/// The logo as a texture, loaded once per window context, for title bars.
pub(crate) fn logo_texture(ctx: &egui::Context) -> Option<TextureHandle> {
    let id = egui::Id::unique("sshremote-logo-texture");
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return Some(t);
    }
    let i = app_icon()?;
    let img = egui::ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba);
    let t = ctx.load_texture("sshremote-logo", img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}

/// The logo, for the window and the task bar.
pub(crate) fn app_icon() -> Option<egui::IconData> {
    static ICON: std::sync::OnceLock<Option<egui::IconData>> = std::sync::OnceLock::new();
    ICON.get_or_init(decode_icon).clone()
}

fn decode_icon() -> Option<egui::IconData> {
    let mut reader =
        png::Decoder::new(std::io::Cursor::new(&include_bytes!("../../assets/sshremote.png")[..])).read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).ok()?;
    rgba.truncate(info.buffer_size());
    Some(egui::IconData { rgba, width: info.width, height: info.height })
}
use crate::xk::{self, ALT_L, CONTROL_L, SHIFT_L};

/// Keysym for a key by its position (US layout names), so the remote layout decides the character.
fn keysym(k: Key) -> Option<u32> {
    use Key::*;
    let ch = |c: char| Some(c as u32);
    match k {
        ArrowLeft => Some(xk::LEFT),
        ArrowUp => Some(xk::UP),
        ArrowRight => Some(xk::RIGHT),
        ArrowDown => Some(xk::DOWN),
        Escape => Some(xk::ESCAPE),
        Tab => Some(xk::TAB),
        Backspace => Some(xk::BACKSPACE),
        Enter => Some(xk::RETURN),
        Space => ch(' '),
        Insert => Some(xk::INSERT),
        Delete => Some(xk::DELETE),
        Home => Some(xk::HOME),
        End => Some(xk::END),
        PageUp => Some(xk::PAGE_UP),
        PageDown => Some(xk::PAGE_DOWN),
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
                    name[1..].parse::<u32>().ok().and_then(xk::f)
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
    /// Keep the password in the system's password store once used.
    remember: bool,
    preset: Option<usize>,
    remote: String,
    capture: String,
}

type Pending = mpsc::Receiver<Result<(Session, mpsc::Receiver<Ev>), String>>;

/// Opens `target` on a thread of its own; the answer wakes the window.
fn connect_in_background(target: Target, ctx: &egui::Context) -> Pending {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = Session::open(&target, true).map(|(s, _tx, rx)| (s, rx));
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    rx
}

enum Screen {
    /// The open window: what to connect to.
    Connect,
    /// The workspace: tabs of panes, each a terminal, a screen or files.
    Work(Box<workspace::Workspace>),
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

pub struct App {
    screen: Screen,
    form: Form,
    recent: Vec<(String, String)>,
    /// Host aliases from ~/.ssh/config; ssh applies their user, port and key itself.
    ssh_hosts: Vec<String>,
    favorites: Vec<String>,
    seen: std::collections::HashMap<String, u64>,
    search: String,
    error: Option<String>,
    budget: f64,
    /// System keys taken from Windows while a session is in front.
    grabbed: Option<mpsc::Receiver<(u32, bool)>>,
    /// The stand-alone terminal window (local shells and ssh sessions).
    terms: Option<crate::termwin::TermWindow>,
    /// Terminal windows made from tabs dragged out of the terminal.
    torn_terms: Vec<crate::termwin::TermWindow>,
    keys: Option<crate::tools::KeyTool>,
    config: Option<crate::tools::ConfigTool>,
    /// What the next connection opens with: its screen, its terminal or its files.
    start_with: Start,
    /// Why the password typed for this connection was not saved, shown once it starts.
    save_error: Option<String>,
    /// The first connection opens in full screen (--fullscreen).
    start_fullscreen: bool,
    /// The terminal panel and the chat beside the screen from the start (--open panel, chat).
    start_panels: (bool, bool),
    /// The open view's place shown.
    section: home::Section,
    /// Workspaces in windows of their own (tabs dragged out), with where they first appear.
    windows: Vec<(egui::ViewportId, Box<workspace::Workspace>, Pos2)>,
    windows_made: u64,
    /// What to open next frame: the open view's choices, tools, the command line.
    queued: Vec<(workspace::Launch, workspace::Place, (bool, bool, bool))>,
    vault: Option<crate::tools::VaultTool>,
    backup: Option<crate::tools::BackupTool>,
    /// Whether the sshremote service is installed here, looked up when the tools menu opens.
    #[cfg_attr(not(windows), allow(dead_code))]
    service: Option<bool>,
}

impl Form {
    /// The connect form as the command line fills it.
    fn from(opt: &Options) -> Form {
        Form {
            host: opt.host.clone().unwrap_or_default(),
            port: String::new(),
            password: String::new(),
            remember: false,
            preset: opt.preset,
            remote: if opt.remote.is_empty() { REMOTE.into() } else { opt.remote.clone() },
            capture: opt.capture.clone(),
        }
    }
}

pub fn run(opt: &Options) -> Result<(), String> {
    let form = Form::from(opt);
    let auto_start = opt.host.is_some() || opt.local;
    let local = opt.local;
    let budget = opt.budget;
    let open = opt.open.clone();
    let size = if auto_start { [1280.0, 800.0] } else { OPEN_SIZE };
    let native = native_options("SSH Remote", size);
    eframe::run_native(
        "sshremote",
        native,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            #[cfg(windows)]
            let grabbed = crate::keygrab::install();
            #[cfg(not(windows))]
            let grabbed = None;
            let mut app = App::new(form, budget, &open, grabbed);
            std::thread::spawn(crate::backup::auto);
            if auto_start {
                app.connect(local);
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
            .with_min_inner_size([320.0, 240.0])
            // Every window draws its own title bar (see chrome).
            .with_decorations(false),
        ..Default::default()
    };
    if let Some(icon) = app_icon() {
        native.viewport = native.viewport.with_icon(icon);
    }
    // Some drivers block a vsynced present for a quarter second; frames are paced by the agent anyway.
    native.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    // As large a window as the GPU can draw (eframe asks for 8192 pixels; most GPUs do 16384), so
    // a window stretched over several monitors still fits.
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut native.wgpu_options.wgpu_setup {
        let base = setup.device_descriptor.clone();
        setup.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut d = base(adapter);
            let most = adapter.limits().max_texture_dimension_2d.max(d.required_limits.max_texture_dimension_2d);
            d.required_limits.max_texture_dimension_2d = most;
            chrome::MAX_SIDE.store(most, std::sync::atomic::Ordering::Relaxed);
            d
        });
    }
    native
}

/// Status colours for text and icons, a shade for each theme so they read on its background.
pub(crate) fn good() -> Color32 {
    if theme::dark() { Color32::from_rgb(70, 190, 110) } else { Color32::from_rgb(16, 112, 52) }
}

pub(crate) fn warn() -> Color32 {
    if theme::dark() { Color32::from_rgb(230, 170, 50) } else { Color32::from_rgb(161, 98, 7) }
}

pub(crate) fn bad() -> Color32 {
    if theme::dark() { Color32::from_rgb(240, 110, 100) } else { Color32::from_rgb(168, 24, 24) }
}


/// An icon button with a tooltip; `on` shows it pressed.
pub(crate) fn icon(ui: &mut egui::Ui, glyph: &str, tip: &str, on: bool) -> egui::Response {
    ui.add(tool_button(glyph, on)).on_hover_text(tip)
}

/// A square icon button: the icon alone, a tint when hovered, and when on an outline over a tint
/// close to the background, in the manner of VS Code and JetBrains.
pub(crate) fn tool_button(glyph: &str, on: bool) -> egui::Button<'_> {
    let b = egui::Button::new(RichText::new(glyph).size(16.0))
        .min_size(Vec2::splat(30.0))
        .corner_radius(crate::gui::theme::R_SM)
        .frame_when_inactive(on);
    if on { b.fill(selected_fill()).stroke(egui::Stroke::new(1.0, selected_stroke())) } else { b }
}

/// The tint behind something selected: barely off the background.
pub(crate) fn selected_fill() -> Color32 {
    if theme::dark() { Color32::from_white_alpha(14) } else { Color32::from_black_alpha(10) }
}

/// The outline of something selected: neutral, plain against the tint.
pub(crate) fn selected_stroke() -> Color32 {
    if theme::dark() { Color32::from_white_alpha(90) } else { Color32::from_black_alpha(80) }
}

/// The outline of a button that stands out a little (the main action of a form).
pub(crate) fn outline() -> Color32 {
    if theme::dark() { Color32::from_white_alpha(40) } else { Color32::from_black_alpha(40) }
}


/// An icon button that opens a menu.
fn icon_menu<R>(ui: &mut egui::Ui, glyph: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::Response {
    let config =
        egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let (r, inner) = egui::containers::menu::MenuButton::from_button(tool_button(glyph, false))
        .config(config)
        .ui(ui, |ui| menu(ui, add));
    if inner.is_some() {
        // Remembered for the next frame, so an auto-hidden toolbar stays while its menu is open.
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("sshremote-menu-open"), pass));
    }
    r
}

/// The inside of a menu: compact rows, a quiet mark on the chosen item, and scrolling when it is
/// taller than the window, so all of it stays within reach.
pub(crate) fn menu<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    menu_look(ui);
    let room = (ui.ctx().content_rect().height() - 48.0).max(120.0);
    egui::ScrollArea::vertical().max_height(room).auto_shrink([true, true]).show(ui, add).inner
}

/// The look of every menu, popup list and drop-down: rows about 28 points tall, as in Windows 11
/// and VS Code, and the chosen item on a tint near the background instead of a colour.
pub(crate) fn menu_look(ui: &mut egui::Ui) {
    let dark = theme::dark();
    let s = ui.style_mut();
    s.spacing.item_spacing.y = 2.0;
    s.spacing.interact_size.y = 28.0;
    s.spacing.button_padding = Vec2::new(8.0, 4.0);
    let text = s.visuals.widgets.inactive.fg_stroke.color;
    s.visuals.selection.bg_fill = if dark { Color32::from_white_alpha(26) } else { Color32::from_black_alpha(18) };
    s.visuals.selection.stroke = egui::Stroke::new(1.0, text);
}

fn menu_open(ctx: &egui::Context) -> bool {
    let last = ctx.data(|d| d.get_temp::<u64>(egui::Id::unique("sshremote-menu-open")));
    last.is_some_and(|p| p + 2 >= ctx.cumulative_pass_nr())
}

/// A preset's real settings, e.g. "256 colours, 100%, 20 fps".
fn preset_text(p: &crate::proto::Preset) -> String {
    format!("{}, {}%, {} fps", p.mode.label(), p.scale, p.fps)
}

/// The colours of the hosts' initials, deep enough for their white letter.
pub(crate) const AVATAR_COLORS: [Color32; 6] = [
    Color32::from_rgb(37, 99, 235),
    Color32::from_rgb(21, 128, 90),
    Color32::from_rgb(124, 72, 200),
    Color32::from_rgb(194, 85, 20),
    Color32::from_rgb(190, 50, 100),
    Color32::from_rgb(20, 113, 150),
];

/// A square with the host's initial, coloured by its name.
fn badge(ui: &mut egui::Ui, name: &str) {
    let bare = name.trim_start_matches("sshremote://");
    let bare = bare.rsplit('@').next().unwrap_or(bare);
    let hash = bare.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), egui::Sense::hover());
    // A quiet square tinted by the host's colour, so hosts stay apart without loud colour.
    let tint = AVATAR_COLORS[hash as usize % AVATAR_COLORS.len()].gamma_multiply(if theme::dark() { 0.30 } else { 0.16 });
    ui.painter().rect_filled(rect, theme::R_LG, tint);
    ui.painter().rect_stroke(rect.shrink(0.5), theme::R_LG, egui::Stroke::new(1.0, outline()), egui::StrokeKind::Inside);
    let initial = bare.chars().next().unwrap_or('?').to_uppercase().to_string();
    let fg = ui.visuals().strong_text_color();
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, initial, FontId::proportional(17.0), fg);
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

/// What a connection opens with.
#[derive(Clone, Copy, PartialEq)]
enum Start {
    Screen,
    Terminal,
    Files,
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Favorite,
    Recent,
    SshConfig,
}

impl App {
    /// The app on its home screen; `open` as `--open` gives it, `grabbed` the system keys caught.
    fn new(form: Form, budget: f64, open: &str, grabbed: Option<mpsc::Receiver<(u32, bool)>>) -> App {
        App {
            screen: Screen::Connect,
            form,
            recent: crate::hosts::load_recent(),
            ssh_hosts: crate::hosts::ssh_config_hosts(),
            favorites: crate::hosts::favorites(),
            seen: crate::hosts::last_seen(),
            search: String::new(),
            error: None,
            budget,
            grabbed,
            terms: None,
            torn_terms: Vec::new(),
            keys: None,
            config: None,
            save_error: None,
            start_with: if open.split_whitespace().any(|w| w == "terminal") {
                Start::Terminal
            } else if open.split_whitespace().any(|w| w == "files") {
                Start::Files
            } else {
                Start::Screen
            },
            start_fullscreen: open.split_whitespace().any(|w| w == "fullscreen"),
            start_panels: (
                open.split_whitespace().any(|w| w == "panel"),
                open.split_whitespace().any(|w| w == "chat"),
            ),
            vault: None,
            backup: None,
            service: None,
            section: home::Section::Ssh,
            windows: Vec::new(),
            windows_made: 0,
            queued: Vec::new(),
        }
    }
}

/// What `sshremote terminal` opens in a pane: a shell here, ssh to a host, or a command.
pub enum TermStart {
    Local,
    Host(String, String),
    Command(Vec<String>),
}

impl TermStart {
    /// The pane: the shell, and the agent of the same machine for its screen and files.
    fn launch(self) -> Launch {
        let local = Target::Local { capture: "auto".into() };
        match self {
            TermStart::Local => {
                let profile = crate::termwin::local_profiles().into_iter().next().expect("a local shell");
                Launch::Shell { label: profile.name.clone(), profile, target: local, ssh_cmd: None }
            }
            TermStart::Command(argv) => {
                let name = argv.first().cloned().unwrap_or_default();
                let profile = crate::termwin::Profile { name: name.clone(), icon: ic::TERMINAL, launch: crate::termwin::Launch::Local(argv) };
                Launch::Shell { label: name, profile, target: local, ssh_cmd: None }
            }
            TermStart::Host(host, port) => {
                let port_n: Option<u16> = port.trim().parse().ok();
                let target = Target::Ssh {
                    host: host.clone(),
                    port: port_n,
                    args: Vec::new(),
                    remote: REMOTE.into(),
                    capture: "auto".into(),
                    password: crate::vault::get(&host),
                    unattended: true,
                };
                let mut cmd = vec!["ssh".to_string()];
                if let Some(p) = port_n {
                    cmd.extend(["-p".into(), p.to_string()]);
                }
                cmd.push(host.clone());
                let profile = crate::termwin::ssh_profile(&host, &port);
                Launch::Shell { label: host, profile, target, ssh_cmd: Some(cmd) }
            }
        }
    }
}

/// `sshremote terminal`: the workspace with `first` in a tab and `panes` beside (true) or below it.
pub fn run_terminal(first: TermStart, panes: Vec<(TermStart, bool)>, view: Option<String>) -> Result<(), String> {
    let opt = crate::parse_client(&["--gui".into()])?;
    let native = native_options("SSH Remote", [960.0, 600.0]);
    eframe::run_native(
        "sshremote",
        native,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            #[cfg(windows)]
            let grabbed = crate::keygrab::install();
            #[cfg(not(windows))]
            let grabbed = None;
            let mut app = App::new(Form::from(&opt), opt.budget, "", grabbed);
            let mut w = workspace::Workspace::new(app.options());
            w.add(first.launch(), Place::Tab, (false, false, false));
            for (p, side) in panes {
                w.add(p.launch(), Place::Split { below: !side }, (false, false, false));
            }
            if let Some(v) = view {
                w.start_view(&v);
            }
            app.screen = Screen::Work(Box::new(w));
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

/// The open window's size, about a file dialog's.
const OPEN_SIZE: [f32; 2] = [900.0, 600.0];

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if !fullscreen {
            chrome::behaviour(&ctx);
        }
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        // What was chosen opens in the workspace, which takes a working window's size.
        for (launch, place, start) in std::mem::take(&mut self.queued) {
            if !matches!(self.screen, Screen::Work(_)) {
                self.screen = Screen::Work(Box::new(workspace::Workspace::new(self.options())));
                if !maximized && !fullscreen {
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(1280.0, 800.0)));
                }
            }
            if let Screen::Work(w) = &mut self.screen {
                w.add(launch, place, start);
            }
        }
        let grabbed: Vec<(u32, bool)> = self.grabbed.as_ref().map(|g| g.try_iter().collect()).unwrap_or_default();
        let mut back = false;
        let mut reached = Vec::new();
        let mut torn = Vec::new();
        if let Screen::Work(mut w) = std::mem::replace(&mut self.screen, Screen::Connect) {
            back = w.ui(ui, grabbed).empty;
            reached = std::mem::take(&mut w.reached);
            self.open_dialog(&ctx, &mut w);
            torn = std::mem::take(&mut w.torn_out);
            self.screen = Screen::Work(w);
        }
        self.tear_off(&ctx, torn);
        self.other_windows(&ctx);
        // Hosts reached are the recent ones; a host typed for a terminal is recorded when opened.
        for h in reached {
            if self.recent.iter().any(|(r, _)| *r == h) || self.ssh_hosts.contains(&h) {
                crate::hosts::mark_seen(&h);
                self.seen = crate::hosts::last_seen();
            }
        }
        if back {
            self.screen = Screen::Connect;
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(ViewportCommand::Title("SSH Remote".into()));
            if !maximized {
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::from(OPEN_SIZE)));
            }
        }
        #[cfg(windows)]
        if !matches!(self.screen, Screen::Work(_)) {
            crate::keygrab::set_active(false);
        }
        if matches!(self.screen, Screen::Connect) {
            self.connect_screen(ui);
        }
        self.side_windows(&ctx);
    }
}

impl App {
    /// The open dialog over workspace `w`, when it asks for one: the choice opens in it, where
    /// it was asked for (a new tab, or beside the active pane).
    fn open_dialog(&mut self, ctx: &egui::Context, w: &mut workspace::Workspace) {
        let Some(place) = w.picker else { return };
        let modal = egui::Modal::new(egui::Id::unique("open-dialog")).show(ctx, |ui| {
            ui.set_width(820.0_f32.min(ctx.content_rect().width() - 40.0));
            ui.set_height(520.0_f32.min(ctx.content_rect().height() - 60.0));
            let close = ui
                .horizontal(|ui| {
                    ui.label(RichText::new("Open").size(16.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(tool_button(ic::X, false)).on_hover_text("Close (Esc)").clicked()
                    })
                    .inner
                })
                .inner;
            ui.separator();
            (close, self.open_view(ui))
        });
        let shut = modal.should_close() || modal.inner.0 || ctx.input(|i| i.key_pressed(Key::Escape));
        if let Some(c) = modal.inner.1 {
            let before = self.queued.len();
            self.choose(c, place);
            // Opened here, not in the main window: a host typed wrong keeps the dialog with its error.
            for (launch, place, start) in self.queued.drain(before..) {
                w.add(launch, place, start);
                w.picker = None;
            }
        }
        if shut {
            w.picker = None;
        }
    }

    /// Tabs dragged out of a workspace: each in a window of its own, where the pointer let go.
    fn tear_off(&mut self, ctx: &egui::Context, torn: Vec<workspace::Workspace>) {
        let at = ctx.input(|i| {
            let origin = i.viewport().outer_rect.map_or(Pos2::ZERO, |r| r.min);
            origin + i.pointer.latest_pos().unwrap_or(Pos2::new(100.0, 100.0)).to_vec2() - Vec2::new(60.0, 16.0)
        });
        for w in torn {
            self.windows_made += 1;
            let id = egui::ViewportId::from_hash_of(("workspace-window", self.windows_made));
            self.windows.push((id, Box::new(w), at));
        }
    }

    /// The windows of torn-off tabs, each a workspace with its own title bar, tabs and panes.
    fn other_windows(&mut self, ctx: &egui::Context) {
        let mut windows = std::mem::take(&mut self.windows);
        let mut torn = Vec::new();
        windows.retain_mut(|(id, w, at)| {
            let mut keep = true;
            let mut builder = egui::ViewportBuilder::default()
                .with_title("SSH Remote")
                .with_inner_size([1100.0, 720.0])
                .with_min_inner_size([480.0, 320.0])
                .with_position(*at)
                .with_decorations(false);
            if let Some(icon) = app_icon() {
                builder = builder.with_icon(icon);
            }
            ctx.show_viewport_immediate(*id, builder, |ui, _| {
                let ctx = ui.ctx().clone();
                if ctx.input(|i| i.viewport().close_requested()) {
                    w.close_all();
                    keep = false;
                    return;
                }
                chrome::behaviour(&ctx);
                if w.ui(ui, Vec::new()).empty {
                    keep = false;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    return;
                }
                self.open_dialog(&ctx, w);
                torn.append(&mut w.torn_out);
            });
            keep
        });
        self.windows.append(&mut windows);
        self.tear_off(ctx, torn);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab dragged out lives in a window of its own, which goes when its last tab closes.
    #[test]
    fn torn_tabs_live_in_windows_of_their_own() {
        let ctx = egui::Context::default();
        theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let mut app = App::new(Form::from(&opt), 100.0, "", None);
        let mut w = workspace::Workspace::new(app.options());
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        w.add(
            Launch::Shell { label: "torn".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
            Place::Tab,
            (false, false, false),
        );
        app.tear_off(&ctx, vec![w]);
        assert_eq!(app.windows.len(), 1);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run_ui(input(), |ui| app.other_windows(ui.ctx()));
        }
        assert_eq!(app.windows.len(), 1, "the window stays while it has a tab");
        app.windows[0].1.close_all();
        let _ = ctx.run_ui(input(), |ui| app.other_windows(ui.ctx()));
        assert!(app.windows.is_empty(), "and goes with its last tab");
    }

    #[test]
    fn keys_map_to_keysyms() {
        assert_eq!(keysym(Key::A), Some('a' as u32));
        assert_eq!(keysym(Key::Num7), Some('7' as u32));
        assert_eq!(keysym(Key::F1), Some(xk::F1));
        assert_eq!(keysym(Key::F12), Some(xk::F12));
        assert_eq!(keysym(Key::Enter), Some(xk::RETURN));
        assert_eq!(keysym(Key::Quote), Some('\'' as u32));
    }
}
