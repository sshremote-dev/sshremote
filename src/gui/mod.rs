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

pub(crate) mod chrome;
mod home;
mod remote;
pub(crate) mod theme;

use remote::Remote;
use theme::filled;
pub(crate) use theme::primary;
pub use theme::{set_theme, style};

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
    /// Terminal windows made from tabs dragged out of the terminal.
    torn_terms: Vec<crate::termwin::TermWindow>,
    keys: Option<crate::tools::KeyTool>,
    config: Option<crate::tools::ConfigTool>,
    show_options: bool,
    /// What the next connection opens with: its screen, its terminal or its files.
    start_with: Start,
    /// Why the password typed for this connection was not saved, shown once it starts.
    save_error: Option<String>,
    /// The first connection opens in full screen (--fullscreen).
    start_fullscreen: bool,
    /// The terminal panel and the chat beside the screen from the start (--open panel, chat).
    start_panels: (bool, bool),
    vault: Option<crate::tools::VaultTool>,
    backup: Option<crate::tools::BackupTool>,
    /// Whether the sshremote service is installed here, looked up when the tools menu opens.
    #[cfg_attr(not(windows), allow(dead_code))]
    service: Option<bool>,
}

pub fn run(opt: &Options) -> Result<(), String> {
    let form = Form {
        host: opt.host.clone().unwrap_or_default(),
        port: String::new(),
        password: String::new(),
        remember: false,
        preset: opt.preset,
        remote: if opt.remote.is_empty() { REMOTE.into() } else { opt.remote.clone() },
        capture: opt.capture.clone(),
    };
    let auto_start = opt.host.is_some() || opt.local;
    let local = opt.local;
    let budget = opt.budget;
    let open = opt.open.clone();
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
                torn_terms: Vec::new(),
                keys: None,
                config: None,
                show_options: false,
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
            };
            std::thread::spawn(crate::backup::auto);
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

const GOOD: Color32 = Color32::from_rgb(70, 190, 110);
const WARN: Color32 = Color32::from_rgb(230, 170, 50);
const BAD: Color32 = Color32::from_rgb(225, 85, 75);

/// An icon button with a tooltip; `on` shows it pressed.
pub(crate) fn icon(ui: &mut egui::Ui, glyph: &str, tip: &str, on: bool) -> egui::Response {
    ui.add(tool_button(glyph, on)).on_hover_text(tip)
}

/// A square, borderless icon button that shows its frame on hover or while on.
pub(crate) fn tool_button(glyph: &str, on: bool) -> egui::Button<'_> {
    let text = RichText::new(glyph).size(16.0);
    let b = egui::Button::new(if on { text.color(ACCENT_TEXT) } else { text })
        .min_size(Vec2::splat(30.0))
        .corner_radius(crate::gui::theme::R_SM)
        .frame_when_inactive(on);
    if on { b.fill(ACCENT.gamma_multiply(0.22)) } else { b }
}

/// Accent-coloured text and icons that read on dark and light backgrounds.
pub(crate) const ACCENT_TEXT: Color32 = Color32::from_rgb(96, 160, 255);

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
    ui.painter().rect_filled(rect, theme::R_LG, COLORS[hash as usize % COLORS.len()]);
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if !fullscreen {
            chrome::behaviour(&ctx);
        }
        let mut next = None;
        let opt = self.options();
        match &mut self.screen {
            Screen::Connect => {}
            Screen::Connecting { started, rx, label } => match rx.try_recv() {
                Ok(Ok((sess, rx))) => {
                    if let Some((h, p)) = self.pending_host.take() {
                        self.recent.retain(|(r, _)| *r != h);
                        self.recent.insert(0, (h.clone(), p));
                        self.recent.truncate(crate::hosts::RECENT_MAX);
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
                    let start = std::mem::replace(&mut self.start_with, Start::Screen);
                    let label = format!("{label}  {}x{}  {}", desk.0, desk.1, sess.backend);
                    let mut remote = Remote::new(sess, rx, ctl, label, ssh_cmd, start == Start::Terminal);
                    if std::mem::take(&mut self.start_fullscreen) {
                        remote.set_fullscreen(&ctx, true);
                    }
                    let (panel, chat) = std::mem::take(&mut self.start_panels);
                    remote.open_panels(panel, chat);
                    if start == Start::Files
                        && let Err(e) = remote.open_files_only()
                    {
                        self.error = Some(e);
                    }
                    next = Some(Screen::Remote(Box::new(remote)));
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    next = Some(Screen::Connect);
                    self.start_with = Start::Screen;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.error = Some("connection failed".into());
                    next = Some(Screen::Connect);
                    self.start_with = Start::Screen;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    let secs = started.elapsed().as_secs();
                    let label = label.clone();
                    if chrome::title_bar(ui, |ui| {
                        ui.label(RichText::new("SSH Remote").size(14.0).strong());
                    })
                    .close
                    {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
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
                                self.start_with = Start::Screen;
                            }
                        });
                    });
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
            },
            Screen::Remote(r) => {
                let grabbed: Vec<(u32, bool)> =
                    self.grabbed.as_ref().map(|g| g.try_iter().collect()).unwrap_or_default();
                if let Err(e) = r.show(ui, grabbed) {
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
