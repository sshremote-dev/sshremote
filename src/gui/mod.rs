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
pub(crate) mod snapshot;

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
    pub const LAYOUT: &str = "\u{E6D6}";
    pub const USER: &str = "\u{E4C2}";
    pub const FILE_TEXT: &str = "\u{E23A}";
    pub const ROWS: &str = "\u{E5A2}";
    pub const EQUALS: &str = "\u{E21C}";
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

/// The logo as a texture for title bars, in the variant of the theme shown, loaded once each.
pub(crate) fn logo_texture(ctx: &egui::Context) -> Option<TextureHandle> {
    let dark = theme::dark();
    let id = egui::Id::unique(("sshremote-logo-texture", dark));
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return Some(t);
    }
    let i = themed_icon(dark)?;
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

/// The logo for a light or a dark theme: a light plate, or a dark one with brighter blues.
pub(crate) fn themed_icon(dark: bool) -> Option<egui::IconData> {
    static DARK: std::sync::OnceLock<Option<egui::IconData>> = std::sync::OnceLock::new();
    if dark { DARK.get_or_init(|| decode_png(include_bytes!("../../assets/sshremote-dark.png"))).clone() } else { app_icon() }
}

fn decode_icon() -> Option<egui::IconData> {
    decode_png(include_bytes!("../../assets/sshremote.png"))
}

fn decode_png(bytes: &[u8]) -> Option<egui::IconData> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
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
    /// `sshremote terminal`: the program ends with its last pane, as a terminal does.
    quit_when_empty: bool,
    /// Hosts being reached for the screen or files, with their ports: recent once reached.
    pending_ports: std::collections::HashMap<String, String>,
    /// The window whose active pane last asked for the system keys (None: the main one).
    grab_window: Option<egui::ViewportId>,
    /// Whether the window was last sized as the open window (true) or a working one.
    sized_for: Option<bool>,
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
            let grabbed = crate::keygrab::install(cc.egui_ctx.clone());
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

/// The app's own blue in each theme (the icon's): folders, and what belongs to the app.
pub(crate) fn brand() -> Color32 {
    if theme::dark() { Color32::from_rgb(0x7C, 0xB4, 0xFF) } else { Color32::from_rgb(0x36, 0x59, 0xD9) }
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


/// A dialog over the window, below its title bar so that stays in sight, centred in the room
/// left; `fit` is the content size it would like, made smaller to fit that room whole.
pub(crate) fn modal(ctx: &egui::Context, id: &str, want: Vec2) -> (egui::Modal, Vec2) {
    let id = egui::Id::unique(id);
    let room = ctx.content_rect().size() - Vec2::new(0.0, chrome::HEIGHT);
    // The modal's frame and a gap around it.
    let edge = Vec2::new(24.0 + 24.0, 24.0 + 24.0);
    let fit = want.min(room - edge).max(Vec2::new(200.0, 120.0));
    let area = egui::Modal::default_area(id).anchor(Align2::CENTER_CENTER, Vec2::new(0.0, chrome::HEIGHT / 2.0));
    (egui::Modal::new(id).area(area), fit)
}

/// A choice among a few, as a quiet button: outlined over a faint tint when it is the chosen one.
pub(crate) fn choice(text: impl Into<String>, on: bool) -> egui::Button<'static> {
    let b = egui::Button::new(text.into()).corner_radius(theme::R_SM).min_size(Vec2::new(0.0, 28.0)).frame_when_inactive(on);
    if on { b.fill(selected_fill()).stroke(egui::Stroke::new(1.0, selected_stroke())) } else { b }
}

/// A drop-down with the app's caret rather than egui's triangle.
pub(crate) fn combo(id: &str, text: impl Into<egui::WidgetText>) -> egui::ComboBox<'_> {
    egui::ComboBox::from_id_salt(id).selected_text(text).icon(|ui, rect, visuals, _open| {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, ic::CARET_DOWN, FontId::proportional(12.0), visuals.fg_stroke.color);
    })
}

/// An icon button that opens a menu.
pub(crate) fn icon_menu<R>(ui: &mut egui::Ui, glyph: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::Response {
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

/// A host's mark in a list: a plain line icon, as the toolbar's are (a direct sshremote://
/// address a link, a host over ssh a server).
fn badge(ui: &mut egui::Ui, name: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), egui::Sense::hover());
    let glyph = if name.starts_with("sshremote://") { ic::GLOBE } else { ic::HARD_DRIVES };
    let fg = ui.visuals().text_color();
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(20.0), fg);
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
            quit_when_empty: false,
            pending_ports: Default::default(),
            grab_window: None,
            sized_for: None,
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
            let grabbed = crate::keygrab::install(cc.egui_ctx.clone());
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
            app.quit_when_empty = true;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

/// The open window's size, about a file dialog's.
const OPEN_SIZE: [f32; 2] = [900.0, 600.0];
/// The smallest working window that still shows its bar, a split and the files readably.
pub(crate) const MIN_WORK: [f32; 2] = [760.0, 480.0];

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
                    ctx.send_viewport_cmd(ViewportCommand::Resizable(true));
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(1280.0, 800.0)));
                }
            }
            if let Screen::Work(w) = &mut self.screen {
                w.add(launch, place, start);
            }
        }
        // System keys caught for a host go to the window whose pane asked for them last frame.
        let grabbed: Vec<(u32, bool)> = self.grabbed.as_ref().map(|g| g.try_iter().collect()).unwrap_or_default();
        let (main_keys, window_keys) = if self.grab_window.is_some() { (Vec::new(), grabbed) } else { (grabbed, Vec::new()) };
        let mut back = false;
        let mut reached = Vec::new();
        let mut main_grab = false;
        let mut closing = false;
        if let Screen::Work(mut w) = std::mem::replace(&mut self.screen, Screen::Connect) {
            let out = w.ui(ui, main_keys);
            back = out.empty;
            closing = w.closing_window;
            main_grab = out.grab;
            reached = std::mem::take(&mut w.reached);
            self.open_dialog(&ctx, &mut w);
            self.screen = Screen::Work(w);
        }
        let window_grab = self.other_windows(&ctx, window_keys, &mut reached);
        self.tab_drags(&ctx);
        // The hook catches the keys while any active pane wants them, for that window.
        self.grab_window = if main_grab { None } else { window_grab };
        #[cfg(windows)]
        crate::keygrab::set_active(main_grab || window_grab.is_some());
        self.reached(reached);
        // `sshremote terminal` ends with its last pane, unless tabs live on in other windows.
        if closing {
            // Closing after a yes: the open window is not shown on the way out.
        } else if back && self.quit_when_empty && self.windows.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else if back {
            self.screen = Screen::Connect;
            ctx.send_viewport_cmd(ViewportCommand::Title("SSH Remote".into()));
        }
        // The open window has one size, whatever the working window was; the working window
        // resizes, down to where it still reads.
        let open = matches!(self.screen, Screen::Connect);
        if self.sized_for != Some(open) {
            self.sized_for = Some(open);
            chrome::set_fixed(&ctx, open);
            if open {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(Vec2::from(OPEN_SIZE)));
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::from(OPEN_SIZE)));
                ctx.send_viewport_cmd(ViewportCommand::Resizable(false));
                ctx.send_viewport_cmd(ViewportCommand::EnableButtons { close: true, minimized: true, maximize: false });
            } else {
                ctx.send_viewport_cmd(ViewportCommand::Resizable(true));
                ctx.send_viewport_cmd(ViewportCommand::EnableButtons { close: true, minimized: true, maximize: true });
                ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(Vec2::from(MIN_WORK)));
            }
        }
        if open {
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
        // What is opened from the dialog, a tool's terminal too, opens in this workspace.
        let before = self.queued.len();
        let (modal, fit) = modal(ctx, "open-dialog", Vec2::new(820.0, 560.0));
        let modal = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.set_height(fit.y);
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
            self.choose(c, place);
        }
        // Opened here, not in the main window: a host typed wrong keeps the dialog with its error.
        for (launch, place, start) in self.queued.drain(before.min(self.queued.len())..) {
            w.add(launch, place, start);
            w.picker = None;
        }
        if shut {
            w.picker = None;
        }
    }

    /// A window of its own for a tab dragged out, its tab under the screen point `at`.
    fn tear_off(&mut self, w: workspace::Workspace, at: Pos2) {
        self.windows_made += 1;
        let id = egui::ViewportId::from_hash_of(("workspace-window", self.windows_made));
        self.windows.push((id, Box::new(w), at - Vec2::new(90.0, 18.0)));
    }

    /// Every window's workspace, with its window (None: the main one).
    fn workspaces(&mut self) -> Vec<(Option<egui::ViewportId>, &mut workspace::Workspace)> {
        let mut all = Vec::new();
        if let Screen::Work(w) = &mut self.screen {
            all.push((None, &mut **w));
        }
        for (id, w, _) in &mut self.windows {
            all.push((Some(*id), &mut **w));
        }
        all
    }

    /// A tab dragged out of its bar, as Windows Terminal and browsers do it: over another
    /// window's tabs that window shows where it would go in, and takes it when let go there;
    /// anywhere else a picture of it follows the pointer, and let go it opens in a window of its
    /// own there (the only tab of a window stays: there is nothing to tear it from).
    fn tab_drags(&mut self, ctx: &egui::Context) {
        let mut ghost = None;
        let mut tear = None;
        let mut focus = None;
        {
            let mut all = self.workspaces();
            for (_, w) in all.iter_mut() {
                w.drop_hint = None;
            }
            let Some(s) = all.iter().position(|(_, w)| w.drag_out.is_some() || w.dropped.is_some()) else { return };
            let dropped = all[s].1.dropped.take();
            let Some(point) = dropped.map(|d| d.1).or(all[s].1.drag_out) else { return };
            let over = all.iter().enumerate().position(|(i, (_, w))| i != s && w.strip_screen.expand2(Vec2::new(8.0, 10.0)).contains(point));
            let at = |w: &workspace::Workspace| {
                let shown: Vec<(usize, &Rect)> = w.tabs_screen.iter().enumerate().filter(|(_, r)| r.is_positive()).collect();
                match shown.iter().find(|(_, r)| r.center().x > point.x) {
                    Some((i, _)) => *i,
                    None => shown.last().map_or(w.tabs_screen.len(), |(i, _)| i + 1),
                }
            };
            match (dropped, over) {
                (None, Some(o)) => {
                    let hint = at(all[o].1);
                    all[o].1.drop_hint = Some(hint);
                }
                (None, None) => {
                    let t = all[s].1.dragging_tab().unwrap_or(0);
                    ghost = Some((all[s].1.tab_label(t), point));
                }
                (Some((t, _)), Some(o)) => {
                    let moved = all[s].1.take_tab(t);
                    let hint = at(all[o].1);
                    all[o].1.put_tab(moved, hint);
                    focus = Some(all[o].0.unwrap_or(egui::ViewportId::ROOT));
                }
                (Some((t, _)), None) => {
                    if all[s].1.tabs_len() > 1 {
                        tear = Some((all[s].1.take_tab(t), point));
                    }
                }
            }
        }
        ctx.request_repaint();
        // Back from screen pixels to this window's points, for placing windows.
        let scale = ctx.input(|i| i.viewport().native_pixels_per_point.unwrap_or(1.0));
        let points = |p: Pos2| Pos2::new(p.x / scale, p.y / scale);
        if let Some((w, at)) = tear {
            self.tear_off(w, points(at));
        }
        if let Some(id) = focus {
            ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
        }
        if let Some(((glyph, name, panes), at)) = ghost {
            tab_ghost(ctx, glyph, &name, panes, points(at));
        }
    }

    /// Hosts reached: the recent list (screen and files are recorded once reached) and when.
    fn reached(&mut self, reached: Vec<String>) {
        for h in reached {
            if let Some(port) = self.pending_ports.remove(&h) {
                self.remember_host(&h, &port);
            } else if self.recent.iter().any(|(r, _)| *r == h) || self.ssh_hosts.contains(&h) {
                crate::hosts::mark_seen(&h);
                self.seen = crate::hosts::last_seen();
            }
        }
    }

    /// The windows of torn-off tabs, each a workspace with its own title bar, tabs and panes.
    /// `keys` are the system keys caught for the one that asked; returns the one asking now.
    fn other_windows(&mut self, ctx: &egui::Context, keys: Vec<(u32, bool)>, reached: &mut Vec<String>) -> Option<egui::ViewportId> {
        let mut windows = std::mem::take(&mut self.windows);
        let mut keys = Some(keys);
        let owner = self.grab_window;
        let mut grab = None;
        windows.retain_mut(|(id, w, at)| {
            let mut keep = true;
            let mut builder = egui::ViewportBuilder::default()
                .with_title("SSH Remote")
                .with_inner_size([1100.0, 720.0])
                .with_min_inner_size(MIN_WORK)
                .with_position(*at)
                .with_decorations(false);
            if let Some(icon) = app_icon() {
                builder = builder.with_icon(icon);
            }
            ctx.show_viewport_immediate(*id, builder, |ui, _| {
                let ctx = ui.ctx().clone();
                // Closing asks first (the workspace does that); once empty the window goes.
                chrome::behaviour(&ctx);
                let mine = if owner == Some(*id) { keys.take().unwrap_or_default() } else { Vec::new() };
                let out = w.ui(ui, mine);
                reached.append(&mut w.reached);
                if out.empty {
                    keep = false;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    return;
                }
                if out.grab {
                    grab = Some(*id);
                }
                self.open_dialog(&ctx, w);
            });
            keep
        });
        self.windows.append(&mut windows);
        grab
    }
}

/// The picture of a tab being dragged where it cannot go in: a small window under the pointer,
/// its tab on top and what it shows below, that lets the pointer through and takes no focus.
fn tab_ghost(ctx: &egui::Context, glyph: &str, name: &str, panes: usize, at: Pos2) {
    let size = Vec2::new(260.0, 150.0);
    let builder = egui::ViewportBuilder::default()
        .with_title("SSH Remote")
        .with_decorations(false)
        .with_inner_size(size)
        .with_position(at - Vec2::new(40.0, 14.0))
        .with_resizable(false)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_taskbar(false)
        .with_active(false);
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("tab-ghost"), builder, |ui, class| {
        let v = ui.visuals().clone();
        // Embedded (where windows of their own are not possible): drawn at the pointer.
        let rect = if class == egui::ViewportClass::EmbeddedWindow {
            Rect::from_min_size(at - Vec2::new(40.0, 14.0), size)
        } else {
            ui.max_rect()
        };
        let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("tab-ghost")));
        p.rect_filled(rect, theme::R_MD, chrome::bar_fill(&v));
        p.rect_stroke(rect, theme::R_MD, v.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
        let tab = Rect::from_min_size(rect.min + Vec2::new(8.0, 6.0), Vec2::new(rect.width() - 16.0, 28.0));
        let top = egui::CornerRadius { nw: theme::R_LG, ne: theme::R_LG, sw: 0, se: 0 };
        p.rect_filled(tab, top, v.panel_fill);
        p.text(tab.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER, glyph, FontId::proportional(14.0), v.weak_text_color());
        let galley = ui.ctx().fonts_mut(|f| {
            let mut job = egui::text::LayoutJob::simple_singleline(name.to_string(), FontId::proportional(13.0), v.strong_text_color());
            job.wrap = egui::text::TextWrapping::truncate_at_width(tab.width() - 44.0);
            f.layout_job(job)
        });
        p.galley(tab.left_center() + Vec2::new(32.0, -galley.size().y / 2.0), galley, v.strong_text_color());
        let body = Rect::from_min_max(Pos2::new(rect.left() + 8.0, tab.bottom()), rect.max - Vec2::new(8.0, 8.0));
        p.rect_filled(body, egui::CornerRadius { nw: 0, ne: 0, sw: theme::R_SM, se: theme::R_SM }, v.panel_fill);
        p.text(body.center() - Vec2::new(0.0, 10.0), Align2::CENTER_CENTER, glyph, FontId::proportional(30.0), v.weak_text_color());
        let what = match panes {
            0 | 1 => "A window of its own here, or onto other tabs".to_string(),
            n => format!("{n} panes: a window of their own here, or onto other tabs"),
        };
        p.text(body.center() + Vec2::new(0.0, 26.0), Align2::CENTER_CENTER, what, FontId::proportional(11.5), v.weak_text_color());
    });
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
        app.tear_off(w, Pos2::new(200.0, 200.0));
        assert_eq!(app.windows.len(), 1);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run_ui(input(), |ui| {
                app.other_windows(ui.ctx(), Vec::new(), &mut Vec::new());
            });
        }
        assert_eq!(app.windows.len(), 1, "the window stays while it has a tab");
        app.windows[0].1.close_all();
        let _ = ctx.run_ui(input(), |ui| {
            app.other_windows(ui.ctx(), Vec::new(), &mut Vec::new());
        });
        assert!(app.windows.is_empty(), "and goes with its last tab");
    }

    /// A tab dragged over another window's tabs is shown going in there, and goes in when let
    /// go; let go anywhere else, it opens in a window of its own.
    #[test]
    fn tabs_move_between_windows() {
        let ctx = egui::Context::default();
        theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let mut app = App::new(Form::from(&opt), 100.0, "", None);
        let shell = |name: &str| {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        let mut first = workspace::Workspace::new(app.options());
        first.add(shell("one"), Place::Tab, (false, false, false));
        first.add(shell("two"), Place::Tab, (false, false, false));
        first.add(shell("three"), Place::Tab, (false, false, false));
        app.screen = Screen::Work(Box::new(first));
        let mut other = workspace::Workspace::new(app.options());
        other.add(shell("four"), Place::Tab, (false, false, false));
        app.tear_off(other, Pos2::new(300.0, 300.0));
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run_ui(input(), |ui| {
                app.other_windows(ui.ctx(), Vec::new(), &mut Vec::new());
            });
        }
        let strip = app.windows[0].1.strip_screen;
        assert!(strip.is_positive(), "the other window's tabs are known");
        main(&mut app).drag_out = Some(strip.left_center() + Vec2::new(4.0, 0.0));
        app.tab_drags(&ctx);
        assert_eq!(app.windows[0].1.drop_hint, Some(0), "shown going in before its tab");
        main(&mut app).drag_out = None;
        main(&mut app).dropped = Some((0, strip.left_center() + Vec2::new(4.0, 0.0)));
        app.tab_drags(&ctx);
        assert_eq!(main(&mut app).tabs_len(), 2);
        assert_eq!(app.windows[0].1.tabs_len(), 2, "it went in");
        assert_eq!(app.windows[0].1.tab_label(0).1, "one");
        main(&mut app).dropped = Some((0, Pos2::new(900.0, 700.0)));
        app.tab_drags(&ctx);
        assert_eq!(main(&mut app).tabs_len(), 1);
        assert_eq!(app.windows.len(), 2, "a window of its own");
        main(&mut app).dropped = Some((0, Pos2::new(900.0, 700.0)));
        app.tab_drags(&ctx);
        assert_eq!(main(&mut app).tabs_len(), 1, "the last tab stays");
        main(&mut app).close_all();
        for (_, w, _) in &mut app.windows {
            w.close_all();
        }
    }

    /// Pictures of the open dialog over a workspace, in a large and a small window:
    /// SSHREMOTE_SNAP_DIR=folder cargo test --release open_dialog_pictures
    #[test]
    fn open_dialog_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        for (w_, h_) in [(1280.0, 800.0), (760.0, 480.0)] {
            let ctx = egui::Context::default();
            theme::style(&ctx);
            let opt = crate::parse_client(&["--gui".into()]).unwrap();
            let mut app = App::new(Form::from(&opt), 100.0, "", None);
            let mut w = workspace::Workspace::new(app.options());
            w.picker = Some(Place::Tab);
            let mut snap = snapshot::Snap::default();
            let size = Vec2::new(w_, h_);
            for (section, name) in [(home::Section::Ssh, "ssh"), (home::Section::Tools, "tools")] {
                app.section = section;
                if section == home::Section::Tools {
                    app.config = Some(crate::tools::ConfigTool::new());
                }
                for k in 0..10 {
                    let input = egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        time: Some(k as f64 * 0.05),
                        ..Default::default()
                    };
                    snap.frame(&ctx, input, |ui| {
                        egui::Panel::top("bar").exact_size(chrome::HEIGHT).show(ui, |_| {});
                        app.open_dialog(ui.ctx(), &mut w);
                    });
                }
                snap.save(&ctx, size, &dir.join(format!("open_dialog_{name}_{w_}.png")));
            }
        }
    }

    fn main(app: &mut App) -> &mut workspace::Workspace {
        match &mut app.screen {
            Screen::Work(w) => w,
            _ => unreachable!(),
        }
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
