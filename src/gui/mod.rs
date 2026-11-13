//! Window viewer: a connect screen, then the remote desktop with a floating
//! toolbar, full screen, and every key and mouse event passed through.

use std::collections::BTreeSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, Event, FontId, Key, Modifiers, Pos2, Rect, RichText, Stroke,
    TextureHandle, TextureOptions, Vec2, ViewportCommand,
};

#[cfg(feature = "remote")]
use crate::clip::Clip;
use crate::color::Mode;
use crate::proto::{MOD_SUPER, PRESETS, SUPER_L, ToAgent, ToClient};
#[cfg(feature = "remote")]
use crate::session::{Action, Control, Ev, Framebuffer, Session};
use crate::options::{Options, REMOTE};
use workspace::{Launch, PaneMode, PaneStart, Place};
use crate::options::Target;

pub(crate) mod chrome;
mod home;
pub(crate) mod kit;
mod settings;
mod tray;
mod access_ui;
#[cfg(feature = "remote")]
mod remote;
#[cfg(not(feature = "remote"))]
#[path = "remote_off.rs"]
mod remote;
pub(crate) mod workspace;
pub(crate) mod theme;
pub(crate) mod look;
mod lottie;
#[cfg(test)]
pub(crate) mod snapshot;
mod signin;
mod extui;
mod dock;
mod toast;
#[cfg(feature = "drive")]
pub(crate) mod drive;

use remote::Remote;
#[cfg(not(feature = "remote"))]
use remote::{Ev, Session};
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
    pub const PHONE: &str = "\u{E3B8}";
    pub const PHONE_HANG_UP: &str = "\u{E3BC}";
    pub const RECORD: &str = "\u{E3EE}";
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
    pub const ERASER: &str = "\u{E21E}";
    pub const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
    pub const PLUS: &str = "\u{E3D4}";
    pub const PLUGS: &str = "\u{EB56}";
    pub const ARROWS_CLOCKWISE: &str = "\u{E094}";
    pub const CARET_DOWN: &str = "\u{E136}";
    pub const CARET_UP: &str = "\u{E13C}";
    pub const ARTICLE: &str = "\u{E0A8}";
    pub const COPY: &str = "\u{E1CA}";
    pub const CHECK: &str = "\u{E182}";
    pub const GLOBE: &str = "\u{E288}";
    pub const KEY: &str = "\u{E2D6}";
    pub const TEXT_T: &str = "\u{E48A}";
    pub const DOTS: &str = "\u{E1FE}";
    pub const CURSOR_CLICK: &str = "\u{E7C8}";
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
    pub const ARCHIVE: &str = "\u{E00C}";
    pub const LOCK_KEY: &str = "\u{E2FE}";
    pub const SHIELD: &str = "\u{E40A}";
    pub const SHIELD_CHECK: &str = "\u{E40C}";
    pub const GEAR: &str = "\u{E272}";
    pub const CHECK_CIRCLE: &str = "\u{E184}";
    pub const CLOCK_COUNTER_CLOCKWISE: &str = "\u{E1A0}";
    pub const CODE: &str = "\u{E1BC}";
    pub const INFO: &str = "\u{E2CE}";
    pub const LIGHTNING: &str = "\u{E2DE}";
    pub const PALETTE: &str = "\u{E6C8}";
    pub const PUZZLE_PIECE: &str = "\u{E596}";
    pub const SLIDERS_HORIZONTAL: &str = "\u{E434}";
    pub const STOREFRONT: &str = "\u{E470}";
    pub const KEYBOARD: &str = "\u{E2D8}";
    pub const SEARCH: &str = "\u{E30C}";
    pub const MONITOR: &str = "\u{E32E}";
    pub const MOON: &str = "\u{E330}";
    pub const PAUSE: &str = "\u{E39E}";
    pub const PLAY: &str = "\u{E3D0}";
    pub const PUSH_PIN: &str = "\u{E3E2}";
    pub const STAR: &str = "\u{E46A}";
    pub const SUN: &str = "\u{E472}";
    pub const TERMINAL: &str = "\u{EAE8}";
    pub const X: &str = "\u{E4F6}";
    pub const WARNING: &str = "\u{E4E0}";
}

/// The mark drawn as a line icon in `rect` and `color`, as `assets/logo.svg` draws it on its
/// 24-unit grid: a prompt chevron entering an open frame.
pub(crate) fn paint_logo(painter: &egui::Painter, rect: Rect, color: Color32) {
    let unit = rect.width().min(rect.height()) / 24.0;
    let at = |x: f32, y: f32| rect.center() + Vec2::new(x - 12.0, y - 12.0) * unit;
    let stroke = egui::Stroke::new((2.0 * unit).max(1.5), color);
    let line = |points: Vec<Pos2>| {
        for p in &points {
            painter.circle_filled(*p, stroke.width / 2.0, color);
        }
        painter.add(egui::Shape::line(points, stroke));
    };
    line(vec![at(5.0, 8.5), at(8.5, 12.0), at(5.0, 15.5)]);
    let mut frame = vec![at(12.0, 5.0)];
    for (cx, cy, from) in [(16.0, 8.0, -90.0f32), (16.0, 16.0, 0.0)] {
        frame.extend((0..=8).map(|i| {
            let a = (from + 90.0 * i as f32 / 8.0).to_radians();
            at(cx + 3.0 * a.cos(), cy + 3.0 * a.sin())
        }));
    }
    frame.push(at(12.0, 19.0));
    let (first, last) = (frame[0], frame[frame.len() - 1]);
    painter.add(egui::Shape::line(frame, stroke));
    painter.circle_filled(first, stroke.width / 2.0, color);
    painter.circle_filled(last, stroke.width / 2.0, color);
}

/// The logo, for the window and the task bar.
pub(crate) fn app_icon() -> Option<egui::IconData> {
    static ICON: std::sync::OnceLock<Option<egui::IconData>> = std::sync::OnceLock::new();
    ICON.get_or_init(decode_icon).clone()
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
    /// What Connect opens: a terminal, the screen or the files; remembered.
    mode: PaneMode,
}

/// A connection being made in the background, and what its agent said before it answered.
struct Pending {
    rx: mpsc::Receiver<Result<(Session, mpsc::Receiver<Ev>), String>>,
    note: std::sync::Arc<std::sync::Mutex<String>>,
}

/// Opens `target` on a thread of its own; the answer wakes the window.
fn connect_in_background(target: Target, ctx: &egui::Context) -> Pending {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    let note = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let said = note.clone();
    std::thread::spawn(move || {
        #[cfg(feature = "remote")]
        let r = {
            let wake = ctx.clone();
            let tell = move |t: &str| {
                *said.lock().unwrap_or_else(|e| e.into_inner()) = t.to_string();
                wake.request_repaint();
            };
            Session::open_noting(&target, true, &tell).map(|(s, _tx, rx)| (s, rx))
        };
        #[cfg(not(feature = "remote"))]
        let r = {
            let _ = target;
            Err(crate::not_built("screens and files"))
        };
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    Pending { rx, note }
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
    /// Looking for computers on this network.
    lan: home::connect::Lan,
    screen: Screen,
    form: Form,
    /// The computers as listed, and when they were read (None: read them again).
    computers: Vec<crate::computers::Computer>,
    computers_at: Option<Instant>,
    /// The row of the list the arrow keys are on, and whether they just moved it.
    picked: Option<usize>,
    picked_moved: bool,
    /// The row whose menu is open, by its computer's address (empty: this computer).
    row_menu: Option<String>,
    /// The address field asks for an SSH password, not only for an ID's.
    ssh_password: bool,
    /// This computer's names and addresses, looked up in the background, and when.
    here: std::sync::Arc<std::sync::Mutex<Option<crate::here::Here>>>,
    here_at: Option<Instant>,
    here_ssh_at: Option<Instant>,
    /// What opens over the home window: a computer's sheet, remote access's settings, Settings.
    sheet: Option<home::Sheet>,
    access_sheet_open: bool,
    settings: Option<settings::Settings>,
    /// The icon in the notification area, once the window was closed with something to keep
    /// going; what its menu shows; and whether the balloon said where the program went.
    #[cfg(windows)]
    tray: Option<tray::Tray>,
    tray_status: tray::Shared,
    #[cfg(windows)]
    told_tray: bool,
    error: Option<String>,
    budget: Option<f64>,
    /// System keys taken from Windows while a session is in front.
    grabbed: Option<mpsc::Receiver<(u32, bool)>>,
    /// The address field takes the keyboard on the next frame.
    focus_host: bool,
    /// What the next connection opens with: its screen, its terminal or its files.
    start_with: Start,
    /// The first connection opens in full screen (--fullscreen).
    start_fullscreen: bool,
    /// The terminal panel and the chat beside the screen from the start (--open panel, chat).
    start_panels: (bool, bool),
    /// `sshremote terminal`: the program ends with its last pane, as a terminal does.
    quit_when_empty: bool,
    /// The open window's close button was clicked; whether closing was asked about, and agreed.
    close_clicked: bool,
    quit_asked: bool,
    quitting: bool,
    /// Hosts being reached for the screen or files, with their ports: recent once reached.
    pending_ports: std::collections::HashMap<String, String>,
    /// Passwords to keep in the password store once their connection works, by host.
    pending_passwords: std::collections::HashMap<String, String>,
    /// The window whose active pane last asked for the system keys (None: the main one).
    grab_window: Option<egui::ViewportId>,
    /// Whether the window was last sized as the open window (true) or a working one.
    sized_for: Option<bool>,
    /// Workspaces in windows of their own (tabs dragged out), with where they first appear.
    windows: Vec<(egui::ViewportId, Box<workspace::Workspace>, Pos2)>,
    windows_made: u64,
    /// What to open next frame: the home window's choices, tools, the command line.
    queued: Vec<(workspace::Launch, workspace::Place, PaneStart)>,
    /// The open window growing into the working window: the size asked for and the frames waited.
    /// What is queued opens once it has its size, so a terminal starts at the size it keeps.
    growing: Option<(Vec2, u32)>,
    exts: Option<crate::ext::ExtTool>,
    /// The Remote access card's state.
    access: access_ui::AccessUi,
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
            // Given with the address (by the window that started this one, say): used for it.
            password: opt.password.clone().unwrap_or_default(),
            // On unless the user turned it off: a password typed once is not asked again.
            remember: crate::hosts::setting("remember_password").is_none_or(|v| v != "no"),
            // The command line's, else what Settings > Connections keeps.
            preset: opt.preset.or_else(|| crate::hosts::setting("preset").and_then(|p| p.parse().ok()).filter(|&i: &usize| i < PRESETS.len())),
            remote: if !opt.remote.is_empty() {
                opt.remote.clone()
            } else {
                crate::hosts::setting("agent_command").filter(|c| !c.trim().is_empty()).unwrap_or_else(|| REMOTE.into())
            },
            capture: if opt.capture.is_empty() || opt.capture == "auto" {
                crate::hosts::setting("capture").filter(|c| !c.is_empty()).unwrap_or_else(|| opt.capture.clone())
            } else {
                opt.capture.clone()
            },
            mode: crate::hosts::setting("connect_mode").and_then(|k| PaneMode::from_key(&k)).unwrap_or(PaneMode::Terminal),
        }
    }
}

pub fn run(opt: &Options) -> Result<(), String> {
    let form = Form::from(opt);
    let auto_start = opt.host.is_some() || opt.local;
    let local = opt.local;
    let budget = opt.budget;
    let open = opt.open.clone();
    let size = if auto_start { WORK_SIZE } else { OPEN_SIZE };
    let mut native = native_options("SSH Remote", size);
    native.centered = true;
    let r = boot(native, move |cc| {
        let mut app = App::boot(cc, form, budget, &open);
        std::thread::spawn(crate::backup::auto);
        std::thread::spawn(crate::ext::retire_built_in);
        // Remote access by ID while this window is open, when it is on and no service runs it.
        #[cfg(feature = "remote")]
        crate::access::start_in_app();
        // The tunnels set to open with the app, through the computers they belong to.
        // Where the computers by ID are on this network now, so opening one goes straight.
        std::thread::spawn(|| drop(crate::lan::refresh_known(std::time::Duration::from_millis(1500))));
        let refused = crate::tunnels::start_with_app();
        if !refused.is_empty() {
            app.error = Some(format!("Tunnels not opened: {}", refused.join("; ")));
        }
        if auto_start {
            app.connect(local);
        }
        app
    });
    // The tunnels end with the window.
    crate::tunnels::stop_all();
    r
}

/// Runs the window with the app `make` builds, then stops the extensions' helpers.
fn boot(native: eframe::NativeOptions, make: impl FnOnce(&eframe::CreationContext<'_>) -> App + 'static) -> Result<(), String> {
    #[cfg(feature = "drive")]
    drive::start();
    let r = eframe::run_native("sshremote", native, Box::new(move |cc| Ok(Box::new(make(cc)))))
        .map_err(|e| e.to_string());
    crate::exthost::stop_all();
    r
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
    good_in(theme::dark())
}

pub(crate) fn warn() -> Color32 {
    warn_in(theme::dark())
}

pub(crate) fn bad() -> Color32 {
    bad_in(theme::dark())
}

/// The status colours of the dark or the light theme.
pub(crate) fn good_in(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(70, 190, 110) } else { Color32::from_rgb(16, 112, 52) }
}

pub(crate) fn warn_in(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(230, 170, 50) } else { Color32::from_rgb(161, 98, 7) }
}

pub(crate) fn bad_in(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(240, 110, 100) } else { Color32::from_rgb(168, 24, 24) }
}


/// An icon button with a tooltip; `on` shows it pressed.
/// A one-line text field that selects characters `select(text)` of its text as it takes the
/// keyboard, as an address bar selects its whole text when clicked.
pub(crate) fn field_selecting(
    ui: &mut egui::Ui,
    text: &mut String,
    select: impl FnOnce(&str) -> std::ops::Range<usize>,
) -> egui::Response {
    let mut out = egui::TextEdit::singleline(text).desired_width(f32::INFINITY).show(ui);
    let response = out.response.response;
    if response.gained_focus() {
        let r = select(text);
        let range = egui::text::CCursorRange::two(egui::text::CCursor::new(r.start), egui::text::CCursor::new(r.end));
        out.state.cursor.set_char_range(Some(range));
        out.state.store(ui.ctx(), response.id);
    }
    response
}

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

/// The cause of a failed connection in plain words, with what to try.
fn friendly(e: &str) -> (String, &'static str) {
    let first = e.lines().find(|l| !l.trim().is_empty()).unwrap_or(e).trim().to_string();
    let l = e.to_lowercase();
    let hint = if l.contains("permission denied") {
        "The host refused the login. Enter the password, or install your key on the host with ssh-copy-id."
    } else if l.contains("could not resolve") || l.contains("name or service not known") || l.contains("no such host") {
        "The host name is unknown. Check the spelling, or use the IP address."
    } else if l.contains("connection refused") || l.contains("actively refused") {
        "Nothing answered on that port. Is the SSH server running there?"
    } else if l.contains("timed out") || l.contains("no route") || l.contains("unreachable") {
        "The host did not answer. Check the address, the network and any firewall."
    } else if l.contains("command not found") || l.contains("not recognized") || l.contains("no such file") {
        "sshremote is not installed on the host. Copy it to ~/.local/bin/sshremote (Windows: anywhere on PATH)."
    } else if l.contains("host key verification failed") {
        "The host's key changed since the last visit. If that is expected, remove its line from ~/.ssh/known_hosts."
    } else if l.contains("wrong password") {
        "The password does not match the one set on that computer (Remote access by ID, on the left of its window)."
    } else if l.contains("no agent with that id") || l.contains("relay: offline") {
        "That ID is not at the relay now: the computer is off, offline, or has remote access turned off."
    } else if l.contains("relay: the computer did not answer") {
        "The computer is at the relay but did not answer. Try again in a moment."
    } else if l.contains("too many attempts") {
        "Too many tries from here in a minute. Wait a minute, then try again."
    } else if l.contains(":7071") {
        "The relay could not be reached. Check the network; a firewall may block port 7071."
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

impl App {
    /// The app a new window starts with: the style, the system keys and the extensions' helpers set up.
    fn boot(cc: &eframe::CreationContext<'_>, form: Form, budget: Option<f64>, open: &str) -> App {
        style(&cc.egui_ctx);
        #[cfg(windows)]
        let grabbed = crate::keygrab::install(cc.egui_ctx.clone());
        #[cfg(not(windows))]
        let grabbed = None;
        let ctx = cc.egui_ctx.clone();
        crate::exthost::start(move || ctx.request_repaint());
        App::new(form, budget, open, grabbed)
    }

    /// The app on its home screen; `open` as `--open` gives it, `grabbed` the system keys caught.
    fn new(form: Form, budget: Option<f64>, open: &str, grabbed: Option<mpsc::Receiver<(u32, bool)>>) -> App {
        App {
            screen: Screen::Connect,
            form,
            computers: Vec::new(),
            computers_at: None,
            picked: None,
            picked_moved: false,
            row_menu: None,
            lan: Default::default(),
            ssh_password: false,
            here: Default::default(),
            here_at: None,
            here_ssh_at: None,
            sheet: None,
            access_sheet_open: false,
            settings: None,
            #[cfg(windows)]
            tray: None,
            tray_status: Default::default(),
            #[cfg(windows)]
            told_tray: false,
            error: None,
            budget,
            grabbed,
            focus_host: true,
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
            exts: None,
            access: Default::default(),
            service: None,
            windows: Vec::new(),
            windows_made: 0,
            quit_when_empty: false,
            close_clicked: false,
            quit_asked: false,
            quitting: false,
            pending_ports: Default::default(),
            pending_passwords: Default::default(),
            grab_window: None,
            sized_for: None,
            queued: Vec::new(),
            growing: None,
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
                let (port_n, cmd) = ssh_command(&host, &port);
                let target = Target::Ssh {
                    host: host.clone(),
                    port: port_n,
                    args: Vec::new(),
                    remote: REMOTE.into(),
                    capture: "auto".into(),
                    password: crate::vault::get(&host),
                    unattended: true,
                };
                let profile = crate::termwin::ssh_profile(&host, &port);
                Launch::Shell { label: host, profile, target, ssh_cmd: Some(cmd) }
            }
        }
    }
}

/// `ssh [-p PORT] HOST`, with the port only when it is one, and that port.
pub(super) fn ssh_command(host: &str, port: &str) -> (Option<u16>, Vec<String>) {
    let port_n: Option<u16> = port.trim().parse().ok();
    let mut cmd = vec!["ssh".to_string()];
    if let Some(p) = port_n {
        cmd.extend(["-p".into(), p.to_string()]);
    }
    cmd.push(host.trim().to_string());
    (port_n, cmd)
}

/// `sshremote terminal`: the workspace with `first` in a tab and `panes` beside (true) or below it.
pub fn run_terminal(first: TermStart, panes: Vec<(TermStart, bool)>, view: Option<String>) -> Result<(), String> {
    let opt = crate::parse_client(&["--gui".into()])?;
    let native = native_options("SSH Remote", [960.0, 600.0]);
    boot(native, move |cc| {
        // No backups or remote access here: the main window runs those, not each terminal window.
        let mut app = App::boot(cc, Form::from(&opt), opt.budget, "");
        let mut w = workspace::Workspace::new(app.options());
        w.add(first.launch(), Place::Tab, PaneStart::default());
        for (p, side) in panes {
            w.add(p.launch(), Place::Split { below: !side }, PaneStart::default());
        }
        if let Some(v) = view {
            w.start_view(&v);
        }
        app.screen = Screen::Work(Box::new(w));
        app.quit_when_empty = true;
        app
    })
}

/// The open window's size when it first shows, and its smallest.
const OPEN_SIZE: [f32; 2] = [980.0, 640.0];
const MIN_OPEN: [f32; 2] = [720.0, 500.0];
/// The working window's size when it first shows, as much of the monitor as fits.
const WORK_SIZE: [f32; 2] = [1280.0, 800.0];
/// The smallest working window that still shows its bar, a split and the files readably.
pub(crate) const MIN_WORK: [f32; 2] = [760.0, 480.0];

/// Resizes the window to `want`, no larger than most of the monitor, moved as little as needed to
/// stay whole on it. Returns the size asked for.
fn grow_on_screen(ctx: &egui::Context, want: Vec2) -> Vec2 {
    let (monitor, outer) = ctx.input(|i| (i.viewport().monitor_size, i.viewport().outer_rect));
    let size = monitor.map_or(want, |m| want.min(m * 0.92));
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
    // The monitor's own origin is not known here, so only a window on the primary one (at the
    // origin) is moved, to stay within it above the taskbar.
    if let (Some(m), Some(o)) = (monitor, outer)
        && Rect::from_min_size(Pos2::ZERO, m).contains(o.min)
    {
        let at = Pos2::new(o.min.x.min((m.x - size.x).max(0.0)), o.min.y.min((m.y - size.y - 48.0).max(0.0)));
        if at != o.min {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(at));
        }
    }
    size
}

impl eframe::App for App {
    #[cfg(feature = "drive")]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        drive::hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        lottie::note_focus(ui.ctx());
        let ctx = ui.ctx().clone();
        crate::exthost::pump();
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        // The open window is not dressed in the look: that is for the working windows.
        look::PLAIN.store(matches!(self.screen, Screen::Connect), std::sync::atomic::Ordering::Relaxed);
        if !fullscreen {
            chrome::behaviour(&ctx);
        } else {
            look::frame(&ctx);
        }
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        // What was chosen opens in the workspace, which takes a working window's size first.
        if !self.queued.is_empty() && matches!(self.screen, Screen::Connect) && !maximized && !fullscreen {
            match self.growing {
                None => {
                    self.growing = Some((grow_on_screen(&ctx, Vec2::from(WORK_SIZE)), 0));
                    ctx.request_repaint();
                }
                Some((size, waited)) => {
                    let now = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
                    let there = now.is_some_and(|n| (n - size).length() < 2.0);
                    self.growing = if there || waited >= 12 { None } else { Some((size, waited + 1)) };
                    ctx.request_repaint();
                }
            }
        }
        if self.growing.is_none() {
            for (launch, place, start) in std::mem::take(&mut self.queued) {
                if !matches!(self.screen, Screen::Work(_)) {
                    self.screen = Screen::Work(Box::new(workspace::Workspace::new(self.options())));
                }
                if let Screen::Work(w) = &mut self.screen {
                    w.set_preset(self.form.preset);
                    w.add(launch, place, start);
                }
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
            if let Some(e) = w.failed.take() {
                self.error = Some(e);
            }
            self.quick_open(&ctx, &mut w);
            self.spread_screens(&ctx, None, &mut w);
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
        // Back from the workspace, the window is the open window's size again; either way it
        // resizes down to where it still reads.
        let open = matches!(self.screen, Screen::Connect);
        if self.sized_for != Some(open) {
            let first = self.sized_for.is_none();
            self.sized_for = Some(open);
            if open {
                if !first {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::from(OPEN_SIZE)));
                }
                ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(Vec2::from(MIN_OPEN)));
                self.focus_host = true;
            } else {
                ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(Vec2::from(MIN_WORK)));
            }
        }
        if open {
            self.home(ui);
            self.ask_before_quitting(&ctx);
        }
        self.tray_sync();
        #[cfg(feature = "drive")]
        drive::end_of_frame(&ctx);
    }
}

impl App {
    /// Whether closing the window would end something that should go on: remote access by ID
    /// served by this program (not by the service), or open tunnels.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn keeps_going(&self) -> bool {
        #[cfg(feature = "remote")]
        {
            let svc = self.access.service.lock().map(|s| s.is_some_and(|s| s.runs)).unwrap_or(false);
            if !svc && self.access.cfg.as_ref().is_some_and(|c| c.enabled) {
                return true;
            }
        }
        crate::tunnels::any_open()
    }

    /// What the notification area's menu shows, and remote access read again when it changed it.
    fn tray_sync(&mut self) {
        if tray::CHANGED.swap(false, std::sync::atomic::Ordering::Relaxed) {
            #[cfg(feature = "remote")]
            {
                self.access.cfg = None;
                self.access.looked = None;
            }
        }
        let mut st = tray::Status::default();
        #[cfg(feature = "remote")]
        {
            st.access = self.access.cfg.as_ref().map(|c| (c.enabled, c.id.clone()));
            st.service = self.access.service.lock().ok().and_then(|s| s.filter(|s| s.known).map(|s| (s.installed, s.runs)));
        }
        if let Ok(mut s) = self.tray_status.lock() {
            *s = st;
        }
    }

    /// Closing the open window ends the other windows and anyone connected by this computer's ID:
    /// that is asked first. With neither, it just closes.
    fn ask_before_quitting(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested()) || std::mem::take(&mut self.close_clicked);
        // Closed while remote access by ID or tunnels are kept up here: the window goes to the
        // notification area, and they go on (Quit in its menu ends them).
        #[cfg(windows)]
        if requested && !self.quitting && !tray::QUIT.load(std::sync::atomic::Ordering::Relaxed) && self.keeps_going()
            && let Some(main) = tray::front_window()
        {
            if self.tray.is_none() {
                self.tray = tray::Tray::start(main, self.tray_status.clone(), app_icon().as_ref());
            }
            if let Some(t) = &self.tray {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                tray::hide(main);
                if !std::mem::replace(&mut self.told_tray, true) {
                    t.say("SSH Remote goes on here: remote access and tunnels stay up. Right-click the icon for its menu, or Quit.");
                }
                return;
            }
        }
        if requested && !self.quitting {
            let others = self.windows.len();
            #[cfg(feature = "remote")]
            let viewers = crate::access::viewers_here();
            #[cfg(not(feature = "remote"))]
            let viewers = 0;
            if others + viewers > 0 {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.quit_asked = true;
            } else {
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
        if !self.quit_asked {
            return;
        }
        let others = self.windows.len();
        #[cfg(feature = "remote")]
        let viewers = crate::access::viewers_here();
        #[cfg(not(feature = "remote"))]
        let viewers = 0;
        let mut body = Vec::new();
        if others > 0 {
            body.push(match others {
                1 => "The other window closes, and its connections and shells end.".to_string(),
                n => format!("The {n} other windows close, and their connections and shells end."),
            });
        }
        if viewers > 0 {
            body.push(match viewers {
                1 => "The person connected to this computer by its ID is disconnected.".to_string(),
                n => format!("The {n} people connected to this computer by its ID are disconnected."),
            });
        }
        if let Some(yes) = workspace::ask(ctx, "Close SSH Remote?", &body.join(" "), "Close") {
            self.quit_asked = false;
            if yes {
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// A window of its own for a tab dragged out, its tab under the screen point `at`.
    /// Opens the host's other screens workspace `w` asked for, each in a window of its own. With
    /// several monitors here, each window goes full screen on the monitor numbered as its screen
    /// is (the asking window too), as Remote Desktop's "use all my monitors"; with one, they open
    /// side by side.
    fn spread_screens(&mut self, ctx: &egui::Context, from: Option<egui::ViewportId>, w: &mut workspace::Workspace) {
        if w.spread.is_empty() {
            return;
        }
        let locals = crate::sys::monitors();
        let ppp = ctx.native_pixels_per_point().unwrap_or(1.0);
        // A window's top left for the middle of monitor `n` here, in points.
        let on = |n: usize| locals.get(n).map(|&(x, y, wd, h)| Pos2::new((x + wd / 2) as f32 / ppp - 550.0, (y + h / 2) as f32 / ppp - 360.0));
        let full = locals.len() > 1;
        if let Some(shown) = w.spread_from.take()
            && full
            && let Some(at) = on(shown)
        {
            ctx.send_viewport_cmd_to(from.unwrap_or(egui::ViewportId::ROOT), ViewportCommand::OuterPosition(at));
            w.full_screen_soon();
        }
        for (k, s) in w.spread.drain(..).enumerate() {
            let mut screen = workspace::Workspace::new(self.options());
            screen.set_preset(self.form.preset);
            let start = PaneStart { fullscreen: full && s.screen < locals.len(), ..Default::default() };
            screen.add_with(s.launch, s.prefs, start);
            let at = on(s.screen).unwrap_or(Pos2::new(140.0 + 48.0 * k as f32, 120.0 + 48.0 * k as f32));
            self.windows_made += 1;
            let id = egui::ViewportId::from_hash_of(("workspace-window", self.windows_made));
            self.windows.push((id, Box::new(screen), at));
        }
    }

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
                    if let Some(moved) = all[s].1.take_tab(t) {
                        let hint = at(all[o].1);
                        all[o].1.put_tab(moved, hint);
                        focus = Some(all[o].0.unwrap_or(egui::ViewportId::ROOT));
                    }
                }
                (Some((t, _)), None) => {
                    if all[s].1.tabs_len() > 1 {
                        tear = all[s].1.take_tab(t).map(|w| (w, point));
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
            // The password worked: kept now, so a wrong one is never saved.
            if let Some(pw) = self.pending_passwords.remove(&h)
                && let Err(e) = crate::vault::set(&h, &pw)
                && let Screen::Work(w) = &mut self.screen
            {
                w.failed = Some(format!("The password was not saved: {e}"));
            }
            if let Some(port) = self.pending_ports.remove(&h) {
                self.remember_host(&h, &port);
            } else if crate::computers::get(&h).is_some() {
                crate::computers::used(&h, None);
                self.computers_at = None;
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
                lottie::note_focus(ui.ctx());
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
                self.quick_open(&ctx, w);
                self.spread_screens(&ctx, Some(*id), w);
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

pub(crate) use crate::sys::human;

#[cfg(test)]
mod tests {
    use crate::gui::snapshot::RunTest;
    use super::*;

    /// Quick open over a workspace gives its address field the keyboard as it opens.
    #[test]
    fn quick_open_starts_in_its_address_field() {
        let ctx = egui::Context::default();
        theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let mut app = App::new(Form::from(&opt), None, "", None);
        // The start's focus long used up, as when "+" is pressed in a workspace.
        app.focus_host = false;
        let mut w = workspace::Workspace::new(app.options());
        w.picker = Some(Place::Tab);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        for _ in 0..3 {
            ctx.run_test(input(), |ui| app.quick_open(ui.ctx(), &mut w));
        }
        assert!(ctx.memory(|m| m.focused().is_some()), "a field has the keyboard");
    }

    /// The open window closes at once when nothing goes with it, and asks first when another
    /// window would.
    #[test]
    fn closing_the_open_window_asks_when_other_windows_would_go() {
        let ctx = egui::Context::default();
        theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        let mut alone = App::new(Form::from(&opt), None, "", None);
        alone.close_clicked = true;
        ctx.run_test(input(), |ui| alone.ask_before_quitting(ui.ctx()));
        assert!(alone.quitting && !alone.quit_asked, "nothing else open: it closes");
        let mut app = App::new(Form::from(&opt), None, "", None);
        let w = workspace::Workspace::new(app.options());
        app.windows.push((egui::ViewportId::from_hash_of("other"), Box::new(w), Pos2::ZERO));
        app.close_clicked = true;
        ctx.run_test(input(), |ui| app.ask_before_quitting(ui.ctx()));
        assert!(app.quit_asked && !app.quitting, "another window open: asked first");
        app.windows.clear();
    }

    /// A tab dragged out lives in a window of its own, which goes when its last tab closes.
    #[test]
    fn torn_tabs_live_in_windows_of_their_own() {
        let ctx = egui::Context::default();
        theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let mut app = App::new(Form::from(&opt), None, "", None);
        let mut w = workspace::Workspace::new(app.options());
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        w.add(
            Launch::Shell { label: "torn".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
            Place::Tab,
            PaneStart::default(),
        );
        app.tear_off(w, Pos2::new(200.0, 200.0));
        assert_eq!(app.windows.len(), 1);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run_test(input(), |ui| {
                app.other_windows(ui.ctx(), Vec::new(), &mut Vec::new());
            });
        }
        assert_eq!(app.windows.len(), 1, "the window stays while it has a tab");
        app.windows[0].1.close_all();
        let _ = ctx.run_test(input(), |ui| {
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
        let mut app = App::new(Form::from(&opt), None, "", None);
        let shell = |name: &str| {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        let mut first = workspace::Workspace::new(app.options());
        first.add(shell("one"), Place::Tab, PaneStart::default());
        first.add(shell("two"), Place::Tab, PaneStart::default());
        first.add(shell("three"), Place::Tab, PaneStart::default());
        app.screen = Screen::Work(Box::new(first));
        let mut other = workspace::Workspace::new(app.options());
        other.add(shell("four"), Place::Tab, PaneStart::default());
        app.tear_off(other, Pos2::new(300.0, 300.0));
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run_test(input(), |ui| {
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

    /// Pictures of the home window and of quick open, in a large and a small window, with an
    /// SSH address and an ID typed: SSHREMOTE_SNAP_DIR=folder cargo test --release home_pictures
    #[test]
    fn home_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        for (w_, h_) in [(1100.0, 700.0), (760.0, 520.0)] {
            for (typed, name) in [("", "empty"), ("dev@192.168.1.20:2222", "ssh"), ("123 456 789", "id")] {
                for quick in [false, true] {
                    let ctx = egui::Context::default();
                    theme::style(&ctx);
                    let opt = crate::parse_client(&["--gui".into()]).unwrap();
                    let mut app = App::new(Form::from(&opt), None, "", None);
                    app.form.host = typed.into();
                    let mut w = workspace::Workspace::new(app.options());
                    w.picker = Some(Place::Tab);
                    let size = Vec2::new(w_, h_);
                    let mut snap = snapshot::Snap::default();
                    for k in 0..10 {
                        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(k as f64 * 0.05), ..Default::default() };
                        snap.frame(&ctx, input, |ui| {
                            if quick {
                                egui::Panel::top("bar").exact_size(chrome::HEIGHT).show(ui, |_| {});
                                app.quick_open(ui.ctx(), &mut w);
                            } else {
                                app.home(ui);
                            }
                        });
                    }
                    let what = if quick { "quick" } else { "home" };
                    snap.save(&ctx, size, &dir.join(format!("{what}_{name}_{w_}.png")));
                }
            }
        }
    }

    /// The site's pictures of the windows, with made-up addresses for this computer:
    /// SSHREMOTE_SITE_DIR=site/img cargo test --release site_pictures (the computers and keys are
    /// those of the settings folder and ~/.ssh it runs with).
    #[test]
    fn site_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SITE_DIR").map(std::path::PathBuf::from) else { return };
        for dark in [true, false] {
            for name in ["new", "ssh", "local", "tools"] {
                let ctx = egui::Context::default();
                theme::style(&ctx);
                theme::apply_theme(&ctx, dark);
                let opt = crate::parse_client(&["--gui".into()]).unwrap();
                let mut app = App::new(Form::from(&opt), None, "", None);
                let ip = std::net::IpAddr::from([192, 168, 1, 40]);
                app.here = std::sync::Arc::new(std::sync::Mutex::new(Some(crate::here::Here {
                    name: "office-pc".into(),
                    dns: String::new(),
                    ips: vec![ip],
                    user: "alex".into(),
                    ssh_port: Some(22),
                })));
                app.here_at = Some(Instant::now());
                #[cfg(feature = "remote")]
                {
                    let cfg = crate::access::Config { id: "482913075".into(), token: String::new(), password: "k7Qp2mZ9".into(), relay: "sshremote.dev".into(), enabled: true, ask: false, allow: crate::grant::ALL, lan: true, direct: true };
                    *app.access.online.lock().unwrap() = Some((cfg.id.clone(), cfg.relay.clone(), Ok(true)));
                    *app.access.service.lock().unwrap() = Some(access_ui::Service { known: true, ..Default::default() });
                    app.access.relay = cfg.relay.clone();
                    app.access.cfg = Some(cfg);
                    app.access.looked = Some(Instant::now());
                    app.access.service_looked = Some(Instant::now());
                }
                match name {
                    "ssh" => app.sheet = crate::computers::get("web1").map(|c| home::Sheet::open(&c)),
                    "local" => app.access_sheet_open = true,
                    "tools" => app.settings = Some(settings::Settings::at("keys")),
                    _ => {}
                }
                let size = Vec2::new(900.0, 600.0);
                let mut snap = snapshot::Snap::default();
                for k in 0..12 {
                    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(k as f64 * 0.05), ..Default::default() };
                    snap.frame(&ctx, input, |ui| app.home(ui));
                }
                snap.save(&ctx, size, &dir.join(format!("{name}-{}.png", if dark { "dark" } else { "light" })));
            }
        }
    }

    /// Every state of the home window and what opens over it, to look over:
    /// SSHREMOTE_REVIEW_DIR=folder cargo test review_pictures
    #[test]
    fn review_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_REVIEW_DIR").map(std::path::PathBuf::from) else { return };
        let states = [
            "home", "home_off", "home_loading", "typed_ssh", "typed_id", "typed_words", "row_hover", "add_ssh", "add_id", "edit_config",
            "access_settings", "settings_appearance", "settings_connections", "settings_keys", "settings_passwords", "settings_backup",
            "settings_extensions", "settings_parts", "settings_about", "quick", "narrow",
        ];
        for dark in [true, false] {
            for name in states {
                let ctx = egui::Context::default();
                theme::style(&ctx);
                theme::apply_theme(&ctx, dark);
                let opt = crate::parse_client(&["--gui".into()]).unwrap();
                let mut app = App::new(Form::from(&opt), None, "", None);
                if name != "home_loading" {
                    app.here = std::sync::Arc::new(std::sync::Mutex::new(Some(crate::here::Here {
                        name: "office-pc".into(),
                        dns: "office-pc.corp.example".into(),
                        ips: vec![std::net::IpAddr::from([192, 168, 1, 40])],
                        user: "alex".into(),
                        ssh_port: Some(22),
                    })));
                }
                app.here_at = Some(Instant::now());
                app.here_ssh_at = Some(Instant::now());
                #[cfg(feature = "remote")]
                if name != "home_loading" {
                    let on = name != "home_off";
                    let cfg = crate::access::Config { id: "482913075".into(), token: String::new(), password: "k7Qp2mZ9".into(), relay: "sshremote.dev".into(), enabled: on, ask: false, allow: crate::grant::ALL, lan: true, direct: true };
                    *app.access.online.lock().unwrap() = Some((cfg.id.clone(), cfg.relay.clone(), Ok(true)));
                    *app.access.service.lock().unwrap() = Some(access_ui::Service { known: true, ..Default::default() });
                    app.access.relay = cfg.relay.clone();
                    app.access.cfg = Some(cfg);
                    app.access.looked = Some(Instant::now());
                    app.access.service_looked = Some(Instant::now());
                }
                let mut w = workspace::Workspace::new(app.options());
                match name {
                    "typed_ssh" => app.form.host = "dev@192.168.1.20:2222".into(),
                    "typed_id" => app.form.host = "123 456 789".into(),
                    "typed_words" => app.form.host = "my build".into(),
                    "add_ssh" => app.sheet = Some(home::Sheet::new(&home::connect::typed("dev@192.168.1.20"))),
                    "add_id" => app.sheet = Some(home::Sheet::new(&home::connect::typed("123456789"))),
                    "edit_config" => app.sheet = crate::computers::get("web1").map(|c| home::Sheet::open(&c)),
                    "access_settings" => app.access_sheet_open = true,
                    "quick" => w.picker = Some(Place::Tab),
                    n if n.starts_with("settings_") => app.settings = Some(settings::Settings::at(&n["settings_".len()..])),
                    _ => {}
                }
                let size = if name == "narrow" { Vec2::new(760.0, 520.0) } else { Vec2::new(1100.0, 700.0) };
                let mut snap = snapshot::Snap::default();
                for k in 0..12 {
                    let mut events = Vec::new();
                    if name == "row_hover" && k > 3 {
                        if let Some(r) = snap.find("Build box").first() {
                            events.push(egui::Event::PointerMoved(r.center()));
                        }
                    }
                    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(k as f64 * 0.05), events, ..Default::default() };
                    snap.frame(&ctx, input, |ui| {
                        if name == "quick" {
                            egui::Panel::top("bar").exact_size(chrome::HEIGHT).show(ui, |_| {});
                            app.quick_open(ui.ctx(), &mut w);
                        } else {
                            app.home(ui);
                        }
                    });
                }
                snap.save(&ctx, size, &dir.join(format!("{name}_{}.png", if dark { "dark" } else { "light" })));
            }
        }
    }

    /// The vector logo at title bar and larger sizes on both themes:
    /// SSHREMOTE_SNAP_DIR=folder cargo test --release logo_pictures
    #[test]
    fn logo_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        for dark in [false, true] {
            let ctx = egui::Context::default();
            theme::style(&ctx);
            theme::apply_theme(&ctx, dark);
            let size = Vec2::new(360.0, 140.0);
            let mut snap = snapshot::Snap::default();
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), ..Default::default() };
            snap.frame(&ctx, input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let mut x = 20.0;
                    for s in [16.0, 20.0, 32.0, 48.0, 96.0] {
                        paint_logo(ui.painter(), Rect::from_min_size(Pos2::new(x, 20.0), Vec2::splat(s)), ui.visuals().text_color());
                        x += s + 16.0;
                    }
                });
            });
            snap.save(&ctx, size, &dir.join(format!("logo_{}.png", if dark { "dark" } else { "light" })));
        }
    }

    /// Digits in the address field are a remote access ID: its password field shows at once, and
    /// it opens the screen and files only (an ID has no terminal); a relay that has not the ID
    /// says why plainly.
    #[test]
    fn an_id_opens_through_the_relay() {
        let ctx = egui::Context::default();
        theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let mut app = App::new(Form::from(&opt), None, "", None);
        let mut w = workspace::Workspace::new(app.options());
        w.picker = Some(Place::Tab);
        app.form.mode = PaneMode::Terminal;
        app.form.host = "123 456 789".into();
        let mut snap = snapshot::Snap::default();
        for k in 0..4 {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))), time: Some(k as f64 * 0.05), ..Default::default() };
            snap.frame(&ctx, input, |ui| {
                app.quick_open(ui.ctx(), &mut w);
            });
        }
        assert!(!snap.find("Password set on that computer").is_empty(), "the ID's password field");
        let button = |m: PaneMode| format!("{}  {}", m.glyph(), m.name());
        assert!(!snap.find(&button(PaneMode::Screen)).is_empty(), "the screen opens");
        assert!(snap.find(&button(PaneMode::Terminal)).is_empty(), "no terminal by ID");
        assert_eq!(app.default_mode(true), PaneMode::Screen, "Enter opens the screen");
        assert!(friendly("relay: offline").1.contains("not at the relay"));
    }

    /// A password typed on Connect goes to the host in its field, never to one opened from the list.
    #[test]
    fn a_typed_password_stays_with_its_host() {
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let mut app = App::new(Form::from(&opt), None, "", None);
        app.form.host = "alice@a".into();
        app.form.password = "secret".into();
        let password_of = |l: Launch| match l {
            Launch::Agent { target: Target::Ssh { password, .. }, .. } => password,
            _ => None,
        };
        let other = app.host_launch("bob@b", "", PaneMode::Screen).unwrap();
        assert_ne!(password_of(other), Some("secret".into()), "not given to another host");
        assert_eq!(app.form.password, "secret", "still there for its own");
        let own = app.host_launch("alice@a", "", PaneMode::Screen).unwrap();
        assert_eq!(password_of(own), Some("secret".into()));
        // Typed with its port, or as an IPv6 address in brackets, it is still the field's.
        for (field, host) in [("alice@a:2222", "alice@a"), ("bob@[fe80::1]:22", "bob@fe80::1")] {
            app.form.host = field.into();
            app.form.password = "secret".into();
            let l = app.host_launch(host, "2222", PaneMode::Screen).unwrap();
            assert_eq!(password_of(l), Some("secret".into()), "{field}");
        }
    }

    /// A computer's menu stays open while the pointer goes from its row over to it, and Edit
    /// there opens its sheet.
    #[test]
    fn a_rows_menu_can_be_reached() {
        let ctx = egui::Context::default();
        theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into()]).unwrap();
        let mut app = App::new(Form::from(&opt), None, "", None);
        app.computers = vec![crate::computers::Computer { address: "dev@build".into(), name: "Build box".into(), ..Default::default() }];
        app.computers_at = Some(Instant::now());
        app.here_at = Some(Instant::now());
        app.here_ssh_at = Some(Instant::now());
        let mut snap = snapshot::Snap::default();
        let mut t = 0.0;
        let mut frame = |app: &mut App, snap: &mut snapshot::Snap, events: Vec<egui::Event>| {
            t += 0.05;
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 640.0))), time: Some(t), events, ..Default::default() };
            snap.frame(&ctx, input, |ui| app.home(ui));
        };
        for _ in 0..3 {
            frame(&mut app, &mut snap, Vec::new());
        }
        let row = snap.find("Build box")[0].center();
        frame(&mut app, &mut snap, vec![egui::Event::PointerMoved(row)]);
        frame(&mut app, &mut snap, Vec::new());
        let dots = *snap.find(ic::DOTS).first().expect("the row's menu button under the pointer");
        for e in snapshot::click(dots.center(), egui::PointerButton::Primary) {
            frame(&mut app, &mut snap, e);
        }
        frame(&mut app, &mut snap, Vec::new());
        let edit = snap.find("Edit\u{2026}").first().expect("the menu open").center();
        // On the way over, the pointer leaves the row.
        for at in [Pos2::new(dots.center().x, edit.y - 30.0), edit] {
            frame(&mut app, &mut snap, vec![egui::Event::PointerMoved(at)]);
            frame(&mut app, &mut snap, Vec::new());
        }
        assert!(!snap.find("Edit\u{2026}").is_empty(), "still open over the menu");
        for e in snapshot::click(edit, egui::PointerButton::Primary) {
            frame(&mut app, &mut snap, e);
        }
        assert!(app.sheet.is_some(), "Edit opened the sheet");
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
