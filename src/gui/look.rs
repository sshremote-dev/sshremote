//! The windows' look as each user wants it: the fonts and their sizes, the terminal's colours, a
//! picture behind the terminal panes (each pane's own, or one across the window), the logo's
//! motion, and an overlay with the system's load and the time. Set in the Appearance window, kept
//! with the other settings, and applied at once to every window.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Vec2};

use super::ic;

/// Where the picture goes: behind each pane on its own, or once across the whole window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    Pane,
    Window,
}

/// How the picture covers its place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fit {
    /// Covers it all, cut at the edges, its proportions kept.
    Fill,
    /// All of it shown, its proportions kept.
    Whole,
    /// Drawn to the place's size and shape.
    Stretch,
    /// Repeated at its own size.
    Tile,
    /// At its own size, in the middle.
    Center,
}

/// Where the logo shows: in the title bar's logo place, or over the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LogoPlace {
    TitleBar,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

/// Where the overlay goes: in the window's corner with this computer's figures, or in each
/// terminal with the figures of the computer it runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HudScope {
    Window,
    Terminals,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// One layer of the background: a built-in animation or a file of one's own, whether it shows,
/// how strongly, how it is placed, and whether each pane has its own or the window one.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct BgLayer {
    pub src: String,
    pub on: bool,
    pub strength: f32,
    pub fit: Fit,
    pub scope: Scope,
}

impl BgLayer {
    fn built_in(id: &str, strength: f32, fit: Fit) -> Self {
        BgLayer { src: id.to_string(), on: true, strength, fit, scope: Scope::Window }
    }
}

/// What a layer is called in the list: a built-in's name, a file's name.
fn layer_name(src: &str) -> String {
    super::lottie::built_in(src).map_or_else(
        || std::path::Path::new(src).file_name().map_or(src.to_string(), |n| n.to_string_lossy().into_owned()),
        |b| b.label.to_string(),
    )
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Look {
    /// Font files for the interface and for the terminal ("" for the system's own).
    pub ui_font: String,
    pub mono_font: String,
    /// The interface's size, 1.0 as designed.
    pub ui_scale: f32,
    /// The terminal's own text and background colours instead of a scheme.
    pub custom_colors: bool,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    /// What is behind the terminal panes: layers, the first in front.
    pub layers: Vec<BgLayer>,
    /// The "SSH REMOTE (thinkbigx)" badge in the window's bottom left corner, and how clearly.
    pub badge: bool,
    pub badge_opacity: f32,
    /// The logo moves: the built-in one breathes, a picture of one's own plays its animation.
    pub logo_motion: bool,
    /// A logo of one's own (a picture or an animation, transparency kept; "" for the built-in),
    /// where it goes, its size over the window, and how clearly it shows.
    pub logo_file: String,
    pub logo_place: LogoPlace,
    pub logo_size: f32,
    pub logo_opacity: f32,
    /// The overlay: whether it shows, what it shows, where, and how strongly.
    pub hud: bool,
    pub hud_cpu: bool,
    pub hud_ram: bool,
    pub hud_disk: bool,
    pub hud_clock: bool,
    pub hud_corner: Corner,
    pub hud_scope: HudScope,
    /// In the active terminal only.
    pub hud_active_only: bool,
    pub hud_text: f32,
    pub hud_bg: [u8; 3],
    pub hud_bg_alpha: f32,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            ui_font: String::new(),
            mono_font: String::new(),
            ui_scale: 1.0,
            custom_colors: false,
            fg: [215, 218, 224],
            bg: [22, 23, 28],
            // The 3D title in front, the night's shooting stars over its lake and mountains.
            layers: vec![
                BgLayer::built_in("builtin:brand", 0.45, Fit::Whole),
                BgLayer::built_in("builtin:night-motion", 0.6, Fit::Fill),
                BgLayer::built_in("builtin:night-still", 0.6, Fit::Fill),
            ],
            badge: false,
            badge_opacity: 0.6,
            logo_motion: false,
            logo_file: String::new(),
            logo_place: LogoPlace::TitleBar,
            logo_size: 96.0,
            logo_opacity: 0.8,
            hud: true,
            hud_cpu: true,
            hud_ram: true,
            hud_disk: true,
            hud_clock: true,
            hud_corner: Corner::TopRight,
            hud_scope: HudScope::Terminals,
            hud_active_only: true,
            hud_text: 0.75,
            hud_bg: [0, 0, 0],
            hud_bg_alpha: 0.6,
        }
    }
}

const SCOPES: [(Scope, &str); 2] = [(Scope::Pane, "pane"), (Scope::Window, "window")];
const FITS: [(Fit, &str); 5] = [(Fit::Fill, "fill"), (Fit::Whole, "fit"), (Fit::Stretch, "stretch"), (Fit::Tile, "tile"), (Fit::Center, "center")];
const LOGO_PLACES: [(LogoPlace, &str); 6] = [
    (LogoPlace::TitleBar, "title-bar"),
    (LogoPlace::TopLeft, "top-left"),
    (LogoPlace::TopRight, "top-right"),
    (LogoPlace::BottomLeft, "bottom-left"),
    (LogoPlace::BottomRight, "bottom-right"),
    (LogoPlace::Center, "center"),
];
const CORNERS: [(Corner, &str); 4] = [
    (Corner::TopLeft, "top-left"),
    (Corner::TopRight, "top-right"),
    (Corner::BottomLeft, "bottom-left"),
    (Corner::BottomRight, "bottom-right"),
];

fn name_of<T: PartialEq + Copy>(list: &[(T, &'static str)], v: T) -> &'static str {
    list.iter().find(|(x, _)| *x == v).map_or("", |x| x.1)
}

fn from_name<T: Copy>(list: &[(T, &'static str)], s: Option<&str>, default: T) -> T {
    list.iter().find(|(_, n)| Some(*n) == s).map_or(default, |x| x.0)
}

impl Look {
    fn to_json(&self) -> String {
        serde_json::json!({
            "ui_font": self.ui_font, "mono_font": self.mono_font, "ui_scale": self.ui_scale,
            "custom_colors": self.custom_colors, "fg": self.fg, "bg": self.bg,
            "layers": self.layers.iter().map(|b| serde_json::json!({"src": b.src, "on": b.on, "strength": b.strength,
                "fit": name_of(&FITS, b.fit), "scope": name_of(&SCOPES, b.scope)})).collect::<Vec<_>>(),
            "logo_motion": self.logo_motion,
            "badge": self.badge, "badge_opacity": self.badge_opacity,
            "logo_file": self.logo_file, "logo_place": name_of(&LOGO_PLACES, self.logo_place),
            "logo_size": self.logo_size, "logo_opacity": self.logo_opacity,
            "hud": self.hud, "hud_cpu": self.hud_cpu, "hud_ram": self.hud_ram, "hud_disk": self.hud_disk,
            "hud_clock": self.hud_clock, "hud_corner": name_of(&CORNERS, self.hud_corner),
            "hud_scope": if self.hud_scope == HudScope::Window { "window" } else { "terminals" },
            "hud_active_only": self.hud_active_only,
            "hud_text": self.hud_text, "hud_bg": self.hud_bg, "hud_bg_alpha": self.hud_bg_alpha,
        })
        .to_string()
    }

    fn from_json(s: &str) -> Look {
        let d = Look::default();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else { return d };
        let text = |k: &str, d: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or(d).to_string();
        let num = |k: &str, d: f32, lo: f32, hi: f32| v.get(k).and_then(|x| x.as_f64()).map_or(d, |x| (x as f32).clamp(lo, hi));
        let flag = |k: &str, d: bool| v.get(k).and_then(|x| x.as_bool()).unwrap_or(d);
        let rgb = |k: &str, d: [u8; 3]| {
            v.get(k)
                .and_then(|x| x.as_array())
                .filter(|a| a.len() == 3)
                .map_or(d, |a| [0, 1, 2].map(|i| a[i].as_u64().unwrap_or(0).min(255) as u8))
        };
        Look {
            ui_font: text("ui_font", ""),
            mono_font: text("mono_font", ""),
            ui_scale: num("ui_scale", d.ui_scale, 0.7, 1.6),
            custom_colors: flag("custom_colors", d.custom_colors),
            fg: rgb("fg", d.fg),
            bg: rgb("bg", d.bg),
            layers: match v.get("layers").and_then(|x| x.as_array()) {
                Some(list) => list
                    .iter()
                    .filter_map(|b| {
                        Some(BgLayer {
                            src: b.get("src")?.as_str()?.to_string(),
                            on: b.get("on").and_then(|x| x.as_bool()).unwrap_or(true),
                            strength: b.get("strength").and_then(|x| x.as_f64()).map_or(0.5, |x| (x as f32).clamp(0.0, 1.0)),
                            fit: from_name(&FITS, b.get("fit").and_then(|x| x.as_str()), Fit::Fill),
                            scope: from_name(&SCOPES, b.get("scope").and_then(|x| x.as_str()), Scope::Window),
                        })
                    })
                    .collect(),
                None => d.layers.clone(),
            },
            badge: flag("badge", d.badge),
            badge_opacity: num("badge_opacity", d.badge_opacity, 0.1, 1.0),
            logo_motion: flag("logo_motion", d.logo_motion),
            logo_file: text("logo_file", ""),
            logo_place: from_name(&LOGO_PLACES, v.get("logo_place").and_then(|x| x.as_str()), d.logo_place),
            logo_size: num("logo_size", d.logo_size, 16.0, 400.0),
            logo_opacity: num("logo_opacity", d.logo_opacity, 0.05, 1.0),
            hud: flag("hud", d.hud),
            hud_cpu: flag("hud_cpu", d.hud_cpu),
            hud_ram: flag("hud_ram", d.hud_ram),
            hud_disk: flag("hud_disk", d.hud_disk),
            hud_clock: flag("hud_clock", d.hud_clock),
            hud_corner: from_name(&CORNERS, v.get("hud_corner").and_then(|x| x.as_str()), d.hud_corner),
            hud_scope: match v.get("hud_scope").and_then(|x| x.as_str()) {
                Some("window") => HudScope::Window,
                Some("terminals") => HudScope::Terminals,
                _ => d.hud_scope,
            },
            hud_active_only: flag("hud_active_only", d.hud_active_only),
            hud_text: num("hud_text", d.hud_text, 0.1, 1.0),
            hud_bg: rgb("hud_bg", d.hud_bg),
            hud_bg_alpha: num("hud_bg_alpha", d.hud_bg_alpha, 0.0, 1.0),
        }
    }
}

static LOOK: RwLock<Option<Look>> = RwLock::new(None);
/// What the terminal reads for every cell, kept where it is quick to read: the custom colours
/// (on, then the text's and the background's RGB), the logo's motion, and whether a picture shows.
static CUSTOM: AtomicU64 = AtomicU64::new(0);
static MOTION: AtomicBool = AtomicBool::new(false);
static PICTURE: AtomicBool = AtomicBool::new(false);

/// The look now (read from the settings the first time).
pub(crate) fn get() -> Look {
    if let Some(l) = LOOK.read().ok().and_then(|g| g.clone()) {
        return l;
    }
    let l = crate::hosts::setting("look").map(|s| Look::from_json(&s)).unwrap_or_default();
    mirror(&l);
    if let Ok(mut g) = LOOK.write() {
        *g = Some(l.clone());
    }
    l
}

/// Takes a new look: kept, remembered, and shown by every window from the next frame.
pub(crate) fn set(l: Look) {
    crate::hosts::set_setting("look", &l.to_json());
    mirror(&l);
    if let Ok(mut g) = LOOK.write() {
        *g = Some(l);
    }
}

fn mirror(l: &Look) {
    let rgb = |c: [u8; 3]| (c[0] as u64) << 16 | (c[1] as u64) << 8 | c[2] as u64;
    CUSTOM.store(if l.custom_colors { 1 << 48 | rgb(l.fg) << 24 | rgb(l.bg) } else { 0 }, Ordering::Relaxed);
    MOTION.store(l.logo_motion, Ordering::Relaxed);
    PICTURE.store(l.layers.iter().any(|b| b.on && b.strength > 0.0), Ordering::Relaxed);
}

/// The terminal's own text and background colours, when chosen.
pub(crate) fn custom_colors() -> Option<(Color32, Color32)> {
    if LOOK.read().ok().is_none_or(|g| g.is_none()) {
        let _ = get();
    }
    let v = CUSTOM.load(Ordering::Relaxed);
    let c = |x: u64| Color32::from_rgb((x >> 16) as u8, (x >> 8) as u8, x as u8);
    (v >> 48 == 1).then(|| (c(v >> 24 & 0xFF_FFFF), c(v & 0xFF_FFFF)))
}

/// Whether the logo breathes.
pub(crate) fn logo_motion() -> bool {
    MOTION.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------------------------
// The picture behind the panes.

/// Pictures read from their files, by path: being read, read (each frame of an animation with
/// how long it shows, transparency kept, at most 4096 pixels a side), or why not.
enum Loaded {
    Reading,
    Ready(std::sync::Arc<Vec<(egui::ColorImage, f64)>>),
    Failed(String),
}

static PICTURES: Mutex<Vec<(String, Loaded)>> = Mutex::new(Vec::new());

/// The picture's frame for now as a texture, once read (in the background the first time), and
/// when the next frame is due for one that moves.
fn picture(ctx: &egui::Context, path: &str, play: bool, px: f32) -> Result<Option<(egui::TextureHandle, Option<f64>)>, String> {
    // A Lottie animation plays in a thread of its own, drawn at the size it shows.
    if super::lottie::is_lottie(path) {
        return super::lottie::frame(ctx, path, px * ctx.pixels_per_point(), play).map(|t| t.map(|t| (t, None)));
    }
    let id = egui::Id::unique(("look-picture", path));
    let textures = match ctx.data(|d| d.get_temp::<std::sync::Arc<Vec<(egui::TextureHandle, f64)>>>(id)) {
        Some(t) => t,
        None => {
            let mut list = PICTURES.lock().map_err(|e| e.to_string())?;
            match list.iter().find(|(p, _)| p == path).map(|x| &x.1) {
                Some(Loaded::Reading) => return Ok(None),
                Some(Loaded::Failed(e)) => return Err(e.clone()),
                Some(Loaded::Ready(frames)) => {
                    let opts = egui::TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..egui::TextureOptions::LINEAR };
                    let t: Vec<_> = frames.iter().map(|(img, d)| (ctx.load_texture("look-picture", img.clone(), opts), *d)).collect();
                    let t = std::sync::Arc::new(t);
                    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
                    t
                }
                None => {
                    list.push((path.to_string(), Loaded::Reading));
                    let (path, ctx) = (path.to_string(), ctx.clone());
                    std::thread::spawn(move || {
                        let got = read_frames(&path);
                        if let Ok(mut list) = PICTURES.lock()
                            && let Some(slot) = list.iter_mut().find(|(p, _)| *p == path)
                        {
                            slot.1 = match got {
                                Ok(frames) => Loaded::Ready(std::sync::Arc::new(frames)),
                                Err(e) => Loaded::Failed(e),
                            };
                        }
                        ctx.request_repaint();
                    });
                    return Ok(None);
                }
            }
        }
    };
    if textures.len() == 1 {
        return Ok(Some((textures[0].0.clone(), None)));
    }
    // An animation: the frame for this moment of its loop (still while the motion is off).
    let total: f64 = textures.iter().map(|f| f.1).sum();
    if !play || total <= 0.0 {
        return Ok(Some((textures[0].0.clone(), None)));
    }
    let mut t = ctx.input(|i| i.time) % total;
    for (tex, d) in textures.iter() {
        if t < *d {
            return Ok(Some((tex.clone(), Some(d - t))));
        }
        t -= d;
    }
    Ok(Some((textures[0].0.clone(), Some(textures[0].1))))
}

/// Every frame of a picture: an animated GIF, PNG (APNG) or WebP frame by frame, else the one
/// picture; with their transparency.
fn read_frames(path: &str) -> Result<Vec<(egui::ColorImage, f64)>, String> {
    use image::AnimationDecoder;
    let e = |x: image::ImageError| x.to_string();
    let bytes = std::fs::read(path).map_err(|x| x.to_string())?;
    let cursor = || std::io::Cursor::new(bytes.as_slice());
    let format = image::guess_format(&bytes).map_err(e)?;
    let frames: Option<Vec<image::Frame>> = match format {
        image::ImageFormat::Gif => Some(image::codecs::gif::GifDecoder::new(cursor()).map_err(e)?.into_frames().collect_frames().map_err(e)?),
        image::ImageFormat::Png => {
            let d = image::codecs::png::PngDecoder::new(cursor()).map_err(e)?;
            if d.is_apng().map_err(e)? { Some(d.apng().map_err(e)?.into_frames().collect_frames().map_err(e)?) } else { None }
        }
        image::ImageFormat::WebP => {
            let d = image::codecs::webp::WebPDecoder::new(cursor()).map_err(e)?;
            if d.has_animation() { Some(d.into_frames().collect_frames().map_err(e)?) } else { None }
        }
        _ => None,
    };
    let to_color = |img: image::DynamicImage| {
        let img = if img.width().max(img.height()) > 4096 { img.thumbnail(4096, 4096) } else { img };
        let rgba = img.to_rgba8();
        egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
    };
    match frames {
        Some(frames) if !frames.is_empty() => Ok(frames
            .into_iter()
            .take(600)
            .map(|f| {
                let (n, d) = f.delay().numer_denom_ms();
                // A frame with no time of its own shows for a tenth of a second, as browsers do.
                let secs = if n == 0 || d == 0 { 0.1 } else { (n as f64 / d as f64 / 1000.0).max(0.02) };
                (to_color(image::DynamicImage::ImageRgba8(f.into_buffer())), secs)
            })
            .collect()),
        _ => Ok(vec![(to_color(image::load_from_memory(&bytes).map_err(e)?), 0.0)]),
    }
}

/// Paints a terminal pane's background over `rect`: its colour, and the picture when one is set
/// (the pane's own, or its part of one across the window), seen through the colour as strongly
/// as chosen. Returns the colour to fill with from then on: clear over a picture, else `base`.
pub(crate) fn backdrop(ui: &egui::Ui, rect: Rect, base: Color32) -> Color32 {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, base);
    if !PICTURE.load(Ordering::Relaxed) {
        return base;
    }
    let l = get();
    // From the back of the list to its front.
    let mut shown = false;
    for b in l.layers.iter().rev().filter(|b| b.on && b.strength > 0.0) {
        let place = match b.scope {
            Scope::Pane => rect,
            Scope::Window => ui.ctx().content_rect(),
        };
        shown |= layer(ui, rect, place, &b.src, b.fit, b.strength);
    }
    if shown { Color32::TRANSPARENT } else { base }
}

/// One picture or animation of the background over `rect`, placed in `place` as `fit` says and
/// as strongly as `strength`; whether it could be drawn.
fn layer(ui: &egui::Ui, rect: Rect, place: Rect, path: &str, fit: Fit, strength: f32) -> bool {
    let painter = ui.painter();
    let Ok(Some((tex, next))) = picture(ui.ctx(), path, true, place.width().max(place.height())) else { return false };
    if let Some(next) = next {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(next));
    }
    let size = tex.size_vec2() / ui.ctx().pixels_per_point();
    let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let at = match fit {
        Fit::Stretch | Fit::Tile => place,
        Fit::Fill => Rect::from_center_size(place.center(), size * (place.width() / size.x).max(place.height() / size.y)),
        Fit::Whole => Rect::from_center_size(place.center(), size * (place.width() / size.x).min(place.height() / size.y)),
        Fit::Center => Rect::from_center_size(place.center(), size),
    };
    let tint = Color32::from_white_alpha((strength * 255.0) as u8);
    let clip = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    if fit == Fit::Tile {
        // One copy at its own size after another, from the place's corner, over what shows.
        let show = rect.intersect(place);
        let (x0, y0) = (((show.left() - place.left()) / size.x).floor(), ((show.top() - place.top()) / size.y).floor());
        let (x1, y1) = (((show.right() - place.left()) / size.x).ceil(), ((show.bottom() - place.top()) / size.y).ceil());
        if (x1 - x0) * (y1 - y0) <= 2000.0 {
            for j in y0 as i32..y1 as i32 {
                for i in x0 as i32..x1 as i32 {
                    let r = Rect::from_min_size(place.min + Vec2::new(i as f32 * size.x, j as f32 * size.y), size);
                    clip.image(tex.id(), r, full, tint);
                }
            }
        }
    } else {
        clip.image(tex.id(), at, full, tint);
    }
    true
}

/// The colour a terminal pane fills with after `backdrop`: clear when a picture shows.
pub(crate) fn fill(base: Color32) -> Color32 {
    if PICTURE.load(Ordering::Relaxed) { Color32::TRANSPARENT } else { base }
}

// ---------------------------------------------------------------------------------------------
// The system's load and the time.

#[derive(Clone, Default)]
struct Stats {
    cpu: Option<f32>,
    ram: Option<(f64, f64)>,
    disk: Option<(f64, f64)>,
    clock: String,
}

struct Sampler {
    at: Option<std::time::Instant>,
    cpu_times: Option<(u64, u64)>,
    stats: Stats,
}

static SAMPLER: Mutex<Sampler> = Mutex::new(Sampler { at: None, cpu_times: None, stats: Stats { cpu: None, ram: None, disk: None, clock: String::new() } });

/// The load and the time, measured at most once a second.
fn stats() -> Stats {
    let Ok(mut s) = SAMPLER.lock() else { return Stats::default() };
    if s.at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(1000)) {
        return s.stats.clone();
    }
    s.at = Some(std::time::Instant::now());
    let (busy_total, ram, disk, clock) = sys_now();
    let cpu = match (s.cpu_times, busy_total) {
        (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => Some(((b1 - b0) as f32 / (t1 - t0) as f32 * 100.0).clamp(0.0, 100.0)),
        _ => s.stats.cpu,
    };
    s.cpu_times = busy_total;
    s.stats = Stats { cpu, ram, disk, clock };
    s.stats.clone()
}

/// Busy and total CPU time, memory and disk (used and total), and the time of day.
type Now = (Option<(u64, u64)>, Option<(f64, f64)>, Option<(f64, f64)>, String);

/// Busy and total CPU time so far, memory and the system disk (used and total, bytes), and the
/// local time as hours and minutes.
#[cfg(windows)]
fn sys_now() -> Now {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    let ft = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    // SAFETY: the three out-pointers are valid FILETIMEs.
    let cpu = (unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0).then(|| {
        // Kernel time counts idle time too.
        let total = ft(kernel) + ft(user);
        (total - ft(idle), total)
    });
    // SAFETY: zeroed is a valid MEMORYSTATUSEX, its length set as the call asks.
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let ram = (unsafe { GlobalMemoryStatusEx(&mut m) } != 0).then(|| ((m.ullTotalPhys - m.ullAvailPhys) as f64, m.ullTotalPhys as f64));
    let root = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
    let (mut free, mut total, mut avail) = (0u64, 0u64, 0u64);
    // SAFETY: a zero-ended path and valid out-pointers.
    let disk = (unsafe { GetDiskFreeSpaceExW(crate::sys::wide(&root).as_ptr(), &mut avail, &mut total, &mut free) } != 0 && total > 0)
        .then(|| ((total - free) as f64, total as f64));
    // SAFETY: the out-pointer is a valid SYSTEMTIME.
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    (cpu, ram, disk, format!("{:02}:{:02}", t.wHour, t.wMinute))
}

#[cfg(unix)]
fn sys_now() -> Now {
    let cpu = std::fs::read_to_string("/proc/stat").ok().and_then(|s| {
        let v: Vec<u64> = s.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
        let total: u64 = v.iter().sum();
        let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
        Some((total - idle, total))
    });
    let ram = std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
        let kb = |k: &str| s.lines().find(|l| l.starts_with(k))?.split_whitespace().nth(1)?.parse::<f64>().ok();
        let (total, avail) = (kb("MemTotal:")?, kb("MemAvailable:")?);
        Some(((total - avail) * 1024.0, total * 1024.0))
    });
    // SAFETY: a zero-ended path and a zeroed statvfs to fill.
    let disk = unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(c"/".as_ptr(), &mut st) == 0).then(|| {
            let f = st.f_frsize as f64;
            ((st.f_blocks - st.f_bfree) as f64 * f, st.f_blocks as f64 * f)
        })
    };
    // SAFETY: localtime_r fills the tm it is given.
    let clock = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    };
    (cpu, ram, disk, clock)
}

/// The overlay's line: what is chosen of the load and the time.
fn hud_line(l: &Look, s: &Stats) -> String {
    let gb = |b: f64| b / (1u64 << 30) as f64;
    let mut parts = Vec::new();
    if l.hud_cpu && let Some(c) = s.cpu {
        parts.push(format!("CPU {c:.0}%"));
    }
    if l.hud_ram && let Some((u, t)) = s.ram {
        parts.push(format!("RAM {:.1}/{:.0} GB", gb(u), gb(t)));
    }
    if l.hud_disk && let Some((u, t)) = s.disk {
        parts.push(format!("Disk {:.0}%", u / t * 100.0));
    }
    if l.hud_clock {
        parts.push(s.clock.clone());
    }
    parts.join("  \u{B7}  ")
}

/// The overlay in a corner of the window, over everything and taking no clicks.
fn overlay(ctx: &egui::Context, l: &Look) {
    if !l.hud || l.hud_scope != HudScope::Window || !(l.hud_cpu || l.hud_ram || l.hud_disk || l.hud_clock) {
        return;
    }
    let line = hud_line(l, &stats());
    ctx.request_repaint_after(std::time::Duration::from_millis(1000));
    if line.is_empty() {
        return;
    }
    // Clear of the title bar above and of the edges.
    let (align, off) = match l.hud_corner {
        Corner::TopLeft => (egui::Align2::LEFT_TOP, Vec2::new(10.0, 44.0)),
        Corner::TopRight => (egui::Align2::RIGHT_TOP, Vec2::new(-10.0, 44.0)),
        Corner::BottomLeft => (egui::Align2::LEFT_BOTTOM, Vec2::new(10.0, -10.0)),
        Corner::BottomRight => (egui::Align2::RIGHT_BOTTOM, Vec2::new(-10.0, -10.0)),
    };
    let (back, ink) = hud_colours(l);
    egui::Area::new(egui::Id::unique("look-overlay"))
        .order(egui::Order::Foreground)
        .anchor(align, off)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new().fill(back).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(line).size(12.0).color(ink).monospace()).extend());
            });
        });
}


// ---------------------------------------------------------------------------------------------
// The load and the time of the computer a terminal runs on.

/// One line of `sshremote --stats`: busy and total CPU time, memory and disk used and total, and
/// the time of day ("-" for what could not be read).
pub(crate) fn stats_line() -> String {
    let (cpu, ram, disk, clock) = sys_now();
    let pair = |p: Option<(f64, f64)>| p.map_or("- -".to_string(), |(a, b)| format!("{a:.0} {b:.0}"));
    let cpu = cpu.map_or("- -".to_string(), |(a, b)| format!("{a} {b}"));
    format!("S {cpu} {} {} {clock}", pair(ram), pair(disk))
}

/// `sshremote --stats`: the line above every 2 s, until whoever reads it goes.
pub(crate) fn print_stats() -> Result<(), String> {
    use std::io::Write;
    loop {
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{}", stats_line()).and_then(|_| out.flush()).is_err() {
            return Ok(());
        }
        drop(out);
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

/// The same from a host without sshremote: Linux's own counters, read by its shell.
const SHELL_STATS: &str = "while :; do c=$(awk '/^cpu /{print $2+$3+$4+$7+$8+$9, $2+$3+$4+$5+$6+$7+$8+$9}' /proc/stat); \
m=$(awk '/MemTotal/{t=$2}/MemAvailable/{a=$2}END{print (t-a)*1024, t*1024}' /proc/meminfo); \
d=$(df -kP / | awk 'NR==2{print $3*1024, $2*1024}'); echo \"S $c $m $d $(date +%H:%M)\"; sleep 2; done";

/// A host's figures as they come, read from its `--stats` line by line.
struct Feed {
    key: Vec<String>,
    stats: Option<Stats>,
    cpu_times: Option<(u64, u64)>,
    used: std::time::Instant,
}

static FEEDS: Mutex<Vec<std::sync::Arc<Mutex<Feed>>>> = Mutex::new(Vec::new());

/// The figures of the computer a terminal runs on: this one with no SSH command, else its host's,
/// read over a connection of their own (one per host, shared by its panes, closed when no pane
/// shows them for a while); None until the first arrive.
fn host_stats(ctx: &egui::Context, ssh: Option<&[String]>) -> Option<Stats> {
    let Some(ssh) = ssh.filter(|s| !s.is_empty()) else { return Some(stats()) };
    let feed = {
        let mut feeds = FEEDS.lock().ok()?;
        match feeds.iter().find(|f| f.lock().is_ok_and(|f| f.key == ssh)) {
            Some(f) => f.clone(),
            None => {
                let f = std::sync::Arc::new(Mutex::new(Feed { key: ssh.to_vec(), stats: None, cpu_times: None, used: std::time::Instant::now() }));
                feeds.push(f.clone());
                let (me, ctx) = (f.clone(), ctx.clone());
                std::thread::spawn(move || follow(&me, &ctx));
                f
            }
        }
    };
    let mut f = feed.lock().ok()?;
    f.used = std::time::Instant::now();
    f.stats.clone()
}

/// Reads a host's figures while a pane shows them; tries again after a while when the
/// connection fails (the host asleep, or not yet signed in to).
fn follow(me: &std::sync::Arc<Mutex<Feed>>, ctx: &egui::Context) {
    use std::io::BufRead;
    let idle = || me.lock().map_or(true, |f| f.used.elapsed() > std::time::Duration::from_secs(15));
    while !idle() {
        let key = me.lock().map(|f| f.key.clone()).unwrap_or_default();
        // ssh [-p PORT] HOST, quietly, without asking for anything.
        let mut cmd = std::process::Command::new(&key[0]);
        cmd.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15"]);
        cmd.args(&key[1..]);
        // sshremote where the agent is looked for (on PATH, else ~/.local/bin), else the shell.
        cmd.arg(format!("sshremote --stats || ~/.local/bin/sshremote --stats || {SHELL_STATS}"));
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
        crate::sys::quiet(&mut cmd);
        if let Ok(mut child) = cmd.spawn() {
            let out = child.stdout.take();
            for line in out.map(std::io::BufReader::new).into_iter().flat_map(|r| r.lines()).map_while(Result::ok) {
                if let Some((times, s)) = parse_stats(&line)
                    && let Ok(mut f) = me.lock()
                {
                    let cpu = match (f.cpu_times, times) {
                        (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => Some(((b1 - b0) as f32 / (t1 - t0) as f32 * 100.0).clamp(0.0, 100.0)),
                        _ => f.stats.as_ref().and_then(|s| s.cpu),
                    };
                    f.cpu_times = times;
                    f.stats = Some(Stats { cpu, ..s });
                    ctx.request_repaint();
                }
                if idle() {
                    break;
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        for _ in 0..20 {
            if idle() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    if let Ok(mut feeds) = FEEDS.lock() {
        feeds.retain(|f| !std::sync::Arc::ptr_eq(f, me));
    }
}

/// A `--stats` line: the CPU times, and the rest as figures to show.
fn parse_stats(line: &str) -> Option<(Option<(u64, u64)>, Stats)> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() != 8 || f[0] != "S" {
        return None;
    }
    let n = |i: usize| f[i].parse::<f64>().ok();
    let cpu = f[1].parse::<u64>().ok().zip(f[2].parse::<u64>().ok());
    let pair = |i: usize| n(i).zip(n(i + 1)).filter(|p| p.1 > 0.0);
    Some((cpu, Stats { cpu: None, ram: pair(3), disk: pair(5), clock: f[7].to_string() }))
}

/// The overlay inside a terminal pane: what its computer is doing, with its name, in the corner
/// chosen; in every terminal, or the active one only.
pub(crate) fn pane_overlay(ui: &egui::Ui, rect: Rect, ssh: Option<&[String]>, label: &str, active: bool) {
    let l = get();
    if !l.hud || l.hud_scope != HudScope::Terminals || (l.hud_active_only && !active) || !(l.hud_cpu || l.hud_ram || l.hud_disk || l.hud_clock) {
        return;
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(1000));
    let line = match host_stats(ui.ctx(), ssh) {
        Some(s) => hud_line(&l, &s),
        None => "\u{2026}".to_string(),
    };
    let who = if ssh.is_some() { label.to_string() } else { "this computer".to_string() };
    let text = if line.is_empty() { who } else { format!("{who}  \u{B7}  {line}") };
    let (back, ink) = hud_colours(&l);
    let painter = ui.painter().with_clip_rect(rect);
    let galley = painter.layout_no_wrap(text, egui::FontId::monospace(11.5), ink);
    let size = galley.size() + Vec2::new(16.0, 8.0);
    let at = match l.hud_corner {
        Corner::TopLeft => Pos2::new(rect.left() + 8.0, rect.top() + 8.0),
        Corner::TopRight => Pos2::new(rect.right() - 8.0 - size.x, rect.top() + 8.0),
        Corner::BottomLeft => Pos2::new(rect.left() + 8.0, rect.bottom() - 8.0 - size.y),
        Corner::BottomRight => Pos2::new(rect.right() - 8.0 - size.x, rect.bottom() - 8.0 - size.y),
    };
    let r = Rect::from_min_size(at, size);
    painter.rect_filled(r, CornerRadius::same(6), back);
    painter.galley(r.min + Vec2::new(8.0, 4.0), galley, ink);
}

/// The overlay's background, and text that reads on it (on the window's own when it is clear).
fn hud_colours(l: &Look) -> (Color32, Color32) {
    let [r, g, b] = l.hud_bg;
    let back = Color32::from_rgba_unmultiplied(r, g, b, (l.hud_bg_alpha * 255.0) as u8);
    let light_back = if l.hud_bg_alpha > 0.4 { (r as u32 * 3 + g as u32 * 6 + b as u32) / 10 > 140 } else { !super::theme::dark() };
    let ink = if light_back { Color32::BLACK } else { Color32::WHITE };
    (back, ink.gamma_multiply(l.hud_text))
}

// ---------------------------------------------------------------------------------------------
// Fonts.

/// A font file found on the system: its name (from the file itself) and whether its letters are
/// all one width.
#[derive(Clone)]
pub(crate) struct FontFile {
    pub path: String,
    pub name: String,
    pub mono: bool,
}

static FONTS: Mutex<Option<Vec<FontFile>>> = Mutex::new(None);

/// The system's fonts, by name (read in the background the first time; empty until then).
fn fonts(ctx: &egui::Context) -> Option<Vec<FontFile>> {
    let mut g = FONTS.lock().ok()?;
    match &*g {
        Some(list) if list.is_empty() => None,
        Some(list) => Some(list.clone()),
        None => {
            *g = Some(Vec::new());
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let list = scan_fonts();
                if let Ok(mut g) = FONTS.lock() {
                    *g = Some(list);
                }
                ctx.request_repaint();
            });
            None
        }
    }
}

fn font_dirs() -> Vec<std::path::PathBuf> {
    let home = crate::sys::home_dir();
    if cfg!(windows) {
        let mut v = vec![std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts")];
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(std::path::PathBuf::from(l).join(r"Microsoft\Windows\Fonts"));
        }
        v
    } else if cfg!(target_os = "macos") {
        vec!["/System/Library/Fonts".into(), "/Library/Fonts".into(), home.join("Library/Fonts")]
    } else {
        vec!["/usr/share/fonts".into(), "/usr/local/share/fonts".into(), home.join(".local/share/fonts"), home.join(".fonts")]
    }
}

fn scan_fonts() -> Vec<FontFile> {
    let mut out: Vec<FontFile> = Vec::new();
    let mut dirs = font_dirs();
    while let Some(d) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
                continue;
            }
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
            if !matches!(ext.as_str(), "ttf" | "otf" | "ttc") {
                continue;
            }
            if let Some((name, mono)) = font_info(&p) {
                out.push(FontFile { path: p.to_string_lossy().into_owned(), name, mono });
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// A font file's full name and whether it is monospaced, read from its `name` and `post` tables
/// (the first face of a collection).
fn font_info(path: &std::path::Path) -> Option<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut read = |at: u64, n: usize| -> Option<Vec<u8>> {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut b = vec![0; n];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    let be16 = |b: &[u8], i: usize| u16::from_be_bytes([b[i], b[i + 1]]) as usize;
    let be32 = |b: &[u8], i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as u64;
    let head = read(0, 12)?;
    let start = if &head[..4] == b"ttcf" { be32(&read(12, 4)?, 0) } else { 0 };
    let dir = read(start, 12)?;
    let tables = be16(&dir, 4);
    let recs = read(start + 12, tables * 16)?;
    let find = |tag: &[u8]| (0..tables).map(|i| &recs[i * 16..i * 16 + 16]).find(|r| &r[..4] == tag).map(|r| (be32(r, 8), be32(r, 12) as usize));
    let mono = find(b"post").and_then(|(at, n)| (n >= 16).then_some(at)).and_then(|at| read(at + 12, 4)).is_some_and(|b| be32(&b, 0) != 0);
    let (at, n) = find(b"name")?;
    let t = read(at, n.min(1 << 20))?;
    let (count, strings) = (be16(&t, 2), be16(&t, 4));
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let r = 6 + i * 12;
        if r + 12 > t.len() {
            break;
        }
        let (platform, lang, id, len, off) = (be16(&t, r), be16(&t, r + 4), be16(&t, r + 6), be16(&t, r + 8), be16(&t, r + 10));
        let Some(raw) = t.get(strings + off..strings + off + len) else { continue };
        let text = match platform {
            0 | 3 => String::from_utf16_lossy(&raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect::<Vec<_>>()),
            1 => raw.iter().map(|&c| c as char).collect(),
            _ => continue,
        };
        // The full name in English first, then the family's.
        let rank = match (id, lang == 0x409 || platform != 3) {
            (4, true) => 4,
            (4, false) => 3,
            (1, true) => 2,
            (1, false) => 1,
            _ => continue,
        };
        if best.as_ref().is_none_or(|b| rank > b.0) && !text.trim().is_empty() {
            best = Some((rank, text.trim().to_string()));
        }
    }
    best.map(|b| (b.1, mono))
}

/// The interface and terminal fonts chosen, for the font setup: font files that still exist.
pub(crate) fn chosen_fonts() -> (Option<String>, Option<String>) {
    let l = get();
    let real = |p: String| (!p.is_empty() && std::path::Path::new(&p).is_file()).then_some(p);
    (real(l.ui_font), real(l.mono_font))
}

// ---------------------------------------------------------------------------------------------
// Every window, every frame.

/// The look applied to a window: its fonts and size, the overlay, and the Appearance window when
/// it was opened here.
pub(crate) fn frame(ctx: &egui::Context) {
    let l = get();
    let fonts_key = egui::Id::unique("look-fonts-applied");
    let want = (l.ui_font.clone(), l.mono_font.clone());
    match ctx.data(|d| d.get_temp::<(String, String)>(fonts_key)) {
        // The window's first frame: its fonts were just set up with the look.
        None => {
            ctx.data_mut(|d| d.insert_temp(fonts_key, want));
        }
        Some(had) if had != want => {
            ctx.data_mut(|d| d.insert_temp(fonts_key, want));
            ctx.set_fonts(egui::FontDefinitions::default());
            super::theme::install_fonts(ctx);
        }
        Some(_) => {}
    }
    if (ctx.zoom_factor() - l.ui_scale).abs() > 0.001 {
        ctx.set_zoom_factor(l.ui_scale);
    }
    overlay(ctx, &l);
    window_logo(ctx, &l);
    badge(ctx, &l);
    if l.logo_motion && l.logo_file.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(40));
    }
    editor_window(ctx);
}

/// A logo of one's own for the title bar, its frame for now and how clearly it shows.
pub(crate) fn title_logo(ctx: &egui::Context) -> Option<(egui::TextureHandle, f32)> {
    let l = get();
    if l.logo_file.is_empty() || l.logo_place != LogoPlace::TitleBar {
        return None;
    }
    let (tex, next) = picture(ctx, &l.logo_file, l.logo_motion, 22.0).ok()??;
    if let Some(next) = next {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(next));
    }
    Some((tex, l.logo_opacity))
}

/// A logo of one's own over the window, where it was put: over the panes, under menus and
/// dialogs, taking no clicks.
fn window_logo(ctx: &egui::Context, l: &Look) {
    if l.logo_file.is_empty() || l.logo_place == LogoPlace::TitleBar {
        return;
    }
    let Ok(Some((tex, next))) = picture(ctx, &l.logo_file, l.logo_motion, l.logo_size) else { return };
    if let Some(next) = next {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(next));
    }
    let s = tex.size_vec2();
    let size = Vec2::new(l.logo_size, l.logo_size * s.y / s.x.max(1.0));
    let area = ctx.content_rect();
    // Clear of the title bar above and of the edges.
    let (top, edge) = (area.top() + 44.0, 12.0);
    let at = match l.logo_place {
        LogoPlace::TopLeft => Pos2::new(area.left() + edge, top),
        LogoPlace::TopRight => Pos2::new(area.right() - edge - size.x, top),
        LogoPlace::BottomLeft => Pos2::new(area.left() + edge, area.bottom() - edge - size.y),
        LogoPlace::BottomRight => Pos2::new(area.right() - edge - size.x, area.bottom() - edge - size.y),
        LogoPlace::Center | LogoPlace::TitleBar => area.center() - size / 2.0,
    };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("look-logo")));
    let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    painter.image(tex.id(), Rect::from_min_size(at, size), full, Color32::from_white_alpha((l.logo_opacity * 255.0) as u8));
}

/// The badge in the window's bottom left corner: the logo and "SSH REMOTE (thinkbigx)", a light
/// passing over the name now and then and the logo breathing with it; over the panes, taking no
/// clicks.
fn badge(ctx: &egui::Context, l: &Look) {
    if !l.badge {
        return;
    }
    let area = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("look-badge")));
    let a = l.badge_opacity;
    let t = ctx.input(|i| i.time) as f32;
    // Every 7 s a light passes over it in 1.6 s.
    let cycle = t % 7.0;
    let sweep = (cycle < 1.6).then_some(cycle / 1.6);
    let glow = sweep.map_or(0.0, |s| (s * std::f32::consts::PI).sin());
    let dark = super::theme::dark();
    let ink = if dark { Color32::from_rgb(225, 232, 245) } else { Color32::from_rgb(30, 40, 60) };
    let at = Pos2::new(area.left() + 14.0, area.bottom() - 14.0);
    // The logo, a little larger at the light's height.
    let icon = 20.0 * (1.0 + 0.08 * glow);
    if let Some(tex) = super::logo_texture(ctx) {
        let r = Rect::from_center_size(Pos2::new(at.x + 10.0, at.y - 10.0), Vec2::splat(icon));
        if glow > 0.0 {
            painter.circle_filled(r.center(), 15.0, super::ACCENT.gamma_multiply(0.25 * glow * a));
        }
        painter.image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::from_white_alpha((a * 255.0) as u8));
    }
    let name = painter.layout_no_wrap("SSH REMOTE".into(), egui::FontId::proportional(13.0), ink.gamma_multiply(a));
    let who = painter.layout_no_wrap("(thinkbigx)".into(), egui::FontId::proportional(11.5), ink.gamma_multiply(a * 0.7));
    let x = at.x + 28.0;
    let name_at = Pos2::new(x, at.y - 10.0 - name.size().y / 2.0);
    let who_at = Pos2::new(x + name.size().x + 6.0, at.y - 10.0 - who.size().y / 2.0 + 1.0);
    let span = Rect::from_min_max(Pos2::new(x, name_at.y), Pos2::new(who_at.x + who.size().x, at.y));
    painter.galley(name_at, name.clone(), ink);
    painter.galley(who_at, who.clone(), ink);
    if let Some(s) = sweep {
        // The light: the text again, bright, in a band moving across it.
        let band_x = span.left() - 30.0 + (span.width() + 60.0) * s;
        let band = Rect::from_min_max(Pos2::new(band_x - 18.0, span.top() - 4.0), Pos2::new(band_x + 18.0, span.bottom() + 4.0));
        let lit = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("look-badge-lit"))).with_clip_rect(band);
        let bright = super::ACCENT.lerp_to_gamma(Color32::WHITE, 0.55).gamma_multiply(a.max(0.5));
        lit.galley_with_override_text_color(name_at, name, bright);
        lit.galley_with_override_text_color(who_at, who, bright);
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    } else {
        ctx.request_repaint_after(std::time::Duration::from_secs_f32(7.0 - cycle));
    }
}

/// The built-in logo's breathing now: its scale and the strength of the glow around it.
pub(crate) fn logo_pulse(ctx: &egui::Context) -> (f32, f32) {
    if !logo_motion() || !get().logo_file.is_empty() {
        return (1.0, 0.0);
    }
    let t = ctx.input(|i| i.time) as f32;
    let s = (t * 1.6).sin() * 0.5 + 0.5;
    (1.0 + 0.08 * s, 0.35 * s)
}

// ---------------------------------------------------------------------------------------------
// The Appearance window.

/// The window the Appearance settings are open in, and what is being edited.
static EDITOR: Mutex<Option<(egui::ViewportId, Editor)>> = Mutex::new(None);

#[derive(Default)]
struct Editor {
    ui_filter: String,
    mono_filter: String,
    image_error: Option<String>,
    /// Where the logo's and the animation's rows were: a file dropped there goes to that one,
    /// elsewhere to the background picture.
    logo_row: Option<Rect>,
    /// A file's path to add as a layer.
    new_file: String,
}

/// Opens the Appearance settings in this window.
pub(crate) fn open(ctx: &egui::Context) {
    if let Ok(mut g) = EDITOR.lock() {
        *g = Some((ctx.viewport_id(), Editor::default()));
    }
    ctx.request_repaint();
}

/// Closes the Appearance settings.
#[cfg(test)]
pub(crate) fn close() {
    if let Ok(mut g) = EDITOR.lock() {
        *g = None;
    }
}

fn editor_window(ctx: &egui::Context) {
    let Ok(mut g) = EDITOR.lock() else { return };
    let Some((vp, ed)) = g.as_mut() else { return };
    if *vp != ctx.viewport_id() {
        return;
    }
    let mut open = true;
    let mut l = get();
    let before = l.clone();
    egui::Window::new(RichText::new(format!("{}  Appearance", ic::EYE)).strong())
        .id(egui::Id::unique("look-editor"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(Vec2::new(460.0, 560.0))
        .max_height(ctx.content_rect().height() - 80.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| editor(ui, &mut l, ed));
        });
    // A picture dropped on the logo's row is the logo; anywhere else, the background.
    if let Some(p) = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| !p.as_os_str().is_empty())) {
        let over = |r: Option<Rect>| ctx.pointer_hover_pos().zip(r).is_some_and(|(at, r)| r.contains(at));
        let path = p.to_string_lossy().into_owned();
        if over(ed.logo_row) {
            l.logo_file = path;
        } else {
            l.layers.insert(0, BgLayer { src: path, on: true, strength: 0.5, fit: Fit::Fill, scope: Scope::Window });
        }
    }
    if l != before {
        set(l);
    }
    if !open {
        *g = None;
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).strong().size(15.0));
}

fn color_row(ui: &mut egui::Ui, label: &str, c: &mut [u8; 3]) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut col = Color32::from_rgb(c[0], c[1], c[2]);
        let r = ui.color_edit_button_srgba(&mut col);
        *c = [col.r(), col.g(), col.b()];
        r.changed()
    })
    .inner
}

/// A font chosen from the system's, found by typing part of its name.
fn font_picker(ui: &mut egui::Ui, id: &str, value: &mut String, filter: &mut String, mono_first: bool) {
    let list = fonts(ui.ctx());
    let current = if value.is_empty() {
        "The system's own".to_string()
    } else {
        list.as_ref().and_then(|l| l.iter().find(|f| f.path == *value).map(|f| f.name.clone())).unwrap_or_else(|| {
            std::path::Path::new(value.as_str()).file_stem().map_or(value.clone(), |s| s.to_string_lossy().into_owned())
        })
    };
    egui::ComboBox::from_id_salt(id).selected_text(current).width(300.0).show_ui(ui, |ui| {
        ui.add(egui::TextEdit::singleline(filter).hint_text("Search fonts").desired_width(280.0));
        if ui.selectable_label(value.is_empty(), "The system's own").clicked() {
            value.clear();
        }
        let Some(list) = list else {
            ui.label(RichText::new("Reading the fonts\u{2026}").weak());
            return;
        };
        let f = filter.to_lowercase();
        let mut shown: Vec<&FontFile> = list.iter().filter(|x| f.is_empty() || x.name.to_lowercase().contains(&f)).collect();
        if mono_first {
            shown.sort_by_key(|x| !x.mono);
        }
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            for x in shown.into_iter().take(400) {
                let label = if mono_first && x.mono { format!("{}  (monospaced)", x.name) } else { x.name.clone() };
                if ui.selectable_label(*value == x.path, label).clicked() {
                    *value = x.path.clone();
                }
            }
        });
    });
}

fn editor(ui: &mut egui::Ui, l: &mut Look, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 8.0;
    heading(ui, "Interface");
    ui.horizontal(|ui| {
        ui.label("Theme");
        let dark = super::theme::dark();
        if ui.selectable_label(dark, "Dark").clicked() && !dark {
            super::theme::set_theme(ui.ctx(), true);
        }
        if ui.selectable_label(!dark, "Light").clicked() && dark {
            super::theme::set_theme(ui.ctx(), false);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Font");
        font_picker(ui, "look-ui-font", &mut l.ui_font, &mut ed.ui_filter, false);
    });
    let mut pct = (l.ui_scale * 100.0).round();
    if ui.add(egui::Slider::new(&mut pct, 70.0..=160.0).step_by(5.0).fixed_decimals(0).suffix("%").text("Size")).changed() {
        l.ui_scale = pct / 100.0;
    }

    heading(ui, "Terminal");
    ui.horizontal(|ui| {
        ui.label("Font");
        font_picker(ui, "look-mono-font", &mut l.mono_font, &mut ed.mono_filter, true);
    });
    crate::termwin::size_picker(ui);
    ui.checkbox(&mut l.custom_colors, "My own colours (else the colour scheme)");
    if l.custom_colors {
        color_row(ui, "Text", &mut l.fg);
        color_row(ui, "Background", &mut l.bg);
    } else {
        crate::termwin::scheme_picker(ui);
    }

    heading(ui, "Background layers");
    ui.label(RichText::new("Behind the terminal panes, the top of the list in front. Add built-in ones or your own pictures and animations (PNG, JPEG, WebP, GIF, Lottie); drop a file on this window to add it.").weak().size(12.0));
    let mut act: Option<(usize, i8)> = None;
    for (i, ly) in l.layers.iter_mut().enumerate() {
        egui::Frame::new().fill(ui.visuals().faint_bg_color).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut ly.on, "");
                if let Ok(Some((t, _))) = picture(ui.ctx(), &ly.src, false, 64.0) {
                    let s = t.size_vec2();
                    ui.add(egui::Image::new(&t).fit_to_exact_size(Vec2::new(56.0, 56.0 * s.y / s.x.max(1.0))).corner_radius(3));
                }
                ui.label(RichText::new(layer_name(&ly.src)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(ic::X).on_hover_text("Remove").clicked() {
                        act = Some((i, 0));
                    }
                    if ui.small_button(ic::CARET_DOWN).on_hover_text("Further back").clicked() {
                        act = Some((i, 1));
                    }
                    if ui.small_button(ic::CARET_UP).on_hover_text("Further forward").clicked() {
                        act = Some((i, -1));
                    }
                });
            });
            if let Err(e) = picture(ui.ctx(), &ly.src, false, 64.0) {
                ui.colored_label(super::bad(), format!("Cannot use it: {e}"));
            }
            let mut pct = (ly.strength * 100.0).round();
            if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%").text("Strength")).changed() {
                ly.strength = pct / 100.0;
            }
            ui.horizontal_wrapped(|ui| {
                for (f, name) in [(Fit::Fill, "Fill"), (Fit::Whole, "Fit"), (Fit::Stretch, "Stretch"), (Fit::Tile, "Tile"), (Fit::Center, "Centre")] {
                    ui.selectable_value(&mut ly.fit, f, name);
                }
                ui.separator();
                ui.selectable_value(&mut ly.scope, Scope::Window, "Whole window");
                ui.selectable_value(&mut ly.scope, Scope::Pane, "Each pane");
            });
        });
    }
    match act {
        Some((i, 0)) => {
            l.layers.remove(i);
        }
        Some((i, -1)) if i > 0 => l.layers.swap(i, i - 1),
        Some((i, 1)) if i + 1 < l.layers.len() => l.layers.swap(i, i + 1),
        _ => {}
    }
    ui.horizontal(|ui| {
        ui.menu_button(format!("{}  Add built-in", ic::PLUS), |ui| {
            for b in super::lottie::BUILT_INS {
                let kind = if b.still { "picture" } else { "animation" };
                if ui.button(format!("{}  ({kind})", b.label)).clicked() {
                    let fit = if b.id == "builtin:brand" { Fit::Whole } else { Fit::Fill };
                    l.layers.insert(0, BgLayer { src: b.id.to_string(), on: true, strength: 0.5, fit, scope: Scope::Window });
                    ui.close();
                }
            }
        });
        ui.add(egui::TextEdit::singleline(&mut ed.new_file).hint_text("Your file's path").desired_width(220.0));
        if ui.add_enabled(!ed.new_file.trim().is_empty(), egui::Button::new("Add")).clicked() {
            l.layers.insert(0, BgLayer { src: ed.new_file.trim().to_string(), on: true, strength: 0.5, fit: Fit::Fill, scope: Scope::Window });
            ed.new_file.clear();
        }
        if ui.button("Defaults").on_hover_text("The default layers again").clicked() {
            l.layers = Look::default().layers;
        }
    });

    heading(ui, "Badge");
    ui.checkbox(&mut l.badge, "SSH REMOTE (thinkbigx) in the bottom left corner");
    if l.badge {
        let mut pct = (l.badge_opacity * 100.0).round();
        if ui.add(egui::Slider::new(&mut pct, 10.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%").text("How clearly it shows")).changed() {
            l.badge_opacity = pct / 100.0;
        }
    }

    heading(ui, "Logo");
    ui.label(RichText::new("The built-in one, or a picture or animation of your own (Lottie .json or .lottie, GIF, PNG, WebP; transparency kept): drop it here, or give its path.").weak().size(12.0));
    let row = ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut l.logo_file).hint_text("The built-in logo").desired_width(300.0));
        if !l.logo_file.is_empty() && ui.button("Built-in").clicked() {
            l.logo_file.clear();
        }
    });
    ed.logo_row = Some(row.response.rect.expand2(Vec2::new(0.0, 30.0)));
    ui.checkbox(&mut l.logo_motion, if l.logo_file.is_empty() { "Animate it (it breathes)" } else { "Play its animation" });
    if !l.logo_file.is_empty() {
        match picture(ui.ctx(), &l.logo_file, l.logo_motion, 64.0) {
            Err(e) => {
                ui.colored_label(super::bad(), format!("Cannot use it: {e}"));
            }
            Ok(None) => {
                ui.label(RichText::new("Reading it\u{2026}").weak());
            }
            Ok(Some((t, _))) => {
                let s = t.size_vec2();
                ui.add(egui::Image::new(&t).fit_to_exact_size(Vec2::new(64.0, 64.0 * s.y / s.x.max(1.0))));
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Where");
            for (p, name) in [
                (LogoPlace::TitleBar, "Title bar"),
                (LogoPlace::TopLeft, "Top left"),
                (LogoPlace::TopRight, "Top right"),
                (LogoPlace::BottomLeft, "Bottom left"),
                (LogoPlace::BottomRight, "Bottom right"),
                (LogoPlace::Center, "Centre"),
            ] {
                ui.selectable_value(&mut l.logo_place, p, name);
            }
        });
        if l.logo_place != LogoPlace::TitleBar {
            ui.add(egui::Slider::new(&mut l.logo_size, 16.0..=400.0).step_by(4.0).text("Size"));
        }
        let mut pct = (l.logo_opacity * 100.0).round();
        if ui.add(egui::Slider::new(&mut pct, 5.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%").text("How clearly it shows")).changed() {
            l.logo_opacity = pct / 100.0;
        }
    }

    heading(ui, "Overlay");
    ui.checkbox(&mut l.hud, "Show the load and the time");
    if l.hud {
        ui.horizontal_wrapped(|ui| {
            ui.label("Where");
            ui.selectable_value(&mut l.hud_scope, HudScope::Terminals, "In each terminal, from the computer it runs on");
            ui.selectable_value(&mut l.hud_scope, HudScope::Window, "In the window, from this computer");
        });
        if l.hud_scope == HudScope::Terminals {
            ui.checkbox(&mut l.hud_active_only, "Only in the active terminal");
        }
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut l.hud_cpu, "CPU");
            ui.checkbox(&mut l.hud_ram, "Memory");
            ui.checkbox(&mut l.hud_disk, "Disk");
            ui.checkbox(&mut l.hud_clock, "Time");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Corner");
            for (c, name) in [(Corner::TopLeft, "Top left"), (Corner::TopRight, "Top right"), (Corner::BottomLeft, "Bottom left"), (Corner::BottomRight, "Bottom right")] {
                ui.selectable_value(&mut l.hud_corner, c, name);
            }
        });
        let mut pct = (l.hud_text * 100.0).round();
        if ui.add(egui::Slider::new(&mut pct, 10.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%").text("Text: faint to strong")).changed() {
            l.hud_text = pct / 100.0;
        }
        color_row(ui, "Background colour", &mut l.hud_bg);
        let mut pct = (l.hud_bg_alpha * 100.0).round();
        if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%").text("Background: clear to solid")).changed() {
            l.hud_bg_alpha = pct / 100.0;
        }
    }
    ui.add_space(8.0);
    ui.separator();
    if ui.button(format!("{}  Back to the defaults", ic::ARROW_CLOCKWISE)).clicked() {
        *l = Look::default();
        crate::termwin::reset_look();
    }
    let _ = &ed.image_error;
    let _ = Fit::Center;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_look_survives_its_own_text() {
        let l = Look {
            ui_font: "a.ttf".into(),
            ui_scale: 1.25,
            custom_colors: true,
            fg: [1, 2, 3],
            layers: vec![BgLayer { src: "/pictures/p.jpg".into(), on: false, strength: 0.3, fit: Fit::Tile, scope: Scope::Pane }],
            hud: true,
            hud_corner: Corner::TopLeft,
            hud_bg_alpha: 0.5,
            ..Look::default()
        };
        assert_eq!(Look::from_json(&l.to_json()), l);
        assert_eq!(Look::from_json("not json"), Look::default());
        assert_eq!(Look::default().layers.first().map(|b| b.src.as_str()), Some("builtin:brand"), "the title first");
        assert!(Look::default().layers.iter().all(|b| super::super::lottie::built_in(&b.src).is_some()));
    }

    /// A logo animation (a GIF here) comes as its frames with their times, its transparent parts
    /// still clear; a still PNG with transparency keeps it too.
    #[test]
    fn pictures_keep_their_frames_and_transparency() {
        let dir = std::env::temp_dir().join(format!("sshremote-look-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gif = dir.join("logo.gif");
        let frame = |k: u8| {
            // A coloured square in the middle, clear around it.
            let img = image::RgbaImage::from_fn(32, 32, |x, y| {
                if (8..24).contains(&x) && (8..24).contains(&y) { image::Rgba([200, 40 * k, 60, 255]) } else { image::Rgba([0, 0, 0, 0]) }
            });
            image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(80, 1))
        };
        {
            let f = std::fs::File::create(&gif).unwrap();
            let mut enc = image::codecs::gif::GifEncoder::new(f);
            enc.encode_frames((0..3).map(frame)).unwrap();
        }
        let frames = read_frames(&gif.to_string_lossy()).unwrap();
        assert_eq!(frames.len(), 3, "every frame");
        assert!(frames.iter().all(|f| (f.1 - 0.08).abs() < 1e-6), "each with its time");
        let px = |img: &egui::ColorImage, x: usize, y: usize| img.pixels[y * img.size[0] + x];
        assert_eq!(px(&frames[0].0, 1, 1).a(), 0, "the edge stays clear");
        assert_eq!(px(&frames[0].0, 16, 16).a(), 255, "the middle is solid");
        let png = dir.join("still.png");
        image::RgbaImage::from_fn(8, 8, |x, _| image::Rgba([10, 20, 30, if x < 4 { 0 } else { 128 }])).save(&png).unwrap();
        let still = read_frames(&png.to_string_lossy()).unwrap();
        assert_eq!(still.len(), 1);
        assert_eq!(px(&still[0].0, 0, 0).a(), 0);
        assert!((px(&still[0].0, 6, 0).a() as i32 - 128).abs() <= 1, "half clear stays half clear");
    }

    #[test]
    fn the_overlay_line_has_what_is_chosen() {
        let l = Look { hud_cpu: true, hud_ram: true, hud_disk: true, hud_clock: true, ..Look::default() };
        let s = Stats { cpu: Some(12.4), ram: Some((8.0 * (1u64 << 30) as f64, 32.0 * (1u64 << 30) as f64)), disk: Some((60.0, 100.0)), clock: "14:05".into() };
        assert_eq!(hud_line(&l, &s), "CPU 12%  \u{B7}  RAM 8.0/32 GB  \u{B7}  Disk 60%  \u{B7}  14:05");
        let l = Look { hud_cpu: false, hud_ram: false, hud_disk: false, hud_clock: true, ..l };
        assert_eq!(hud_line(&l, &s), "14:05");
    }

    /// What `--stats` writes reads back as the figures, and a host's shell's line as well.
    #[test]
    fn stats_lines_read_back() {
        let (times, s) = parse_stats(&stats_line()).expect("its own line");
        assert!(times.is_some() && s.ram.is_some() && s.disk.is_some() && s.clock.len() == 5);
        let (times, s) = parse_stats("S 144600854 1303199213 18519367680 65371582464 93436006400 103976787968 22:14").unwrap();
        assert_eq!(times, Some((144600854, 1303199213)));
        assert_eq!(s.ram, Some((18519367680.0, 65371582464.0)));
        assert_eq!(s.clock, "22:14");
        assert!(parse_stats("S - - - - - - 22:14").is_some_and(|(t, s)| t.is_none() && s.ram.is_none()));
        assert!(parse_stats("bash: sshremote: command not found").is_none());
    }

    #[test]
    fn the_system_is_measured() {
        let (cpu, ram, disk, clock) = sys_now();
        assert!(cpu.is_some_and(|(b, t)| t > 0 && b <= t), "{cpu:?}");
        assert!(ram.is_some_and(|(u, t)| t > 0.0 && u <= t), "{ram:?}");
        assert!(disk.is_some_and(|(u, t)| t > 0.0 && u <= t), "{disk:?}");
        assert_eq!(clock.len(), 5);
    }

    #[test]
    fn font_files_tell_their_names() {
        // The fonts as the font list finds them (in folders under the font folders too).
        let all = scan_fonts();
        assert!(all.len() >= 10 && all.iter().any(|f| f.mono), "{} fonts found", all.len());
        let named: Vec<_> = all.iter().take(20).filter_map(|f| font_info(std::path::Path::new(&f.path))).collect();
        assert_eq!(named.len(), all.len().min(20), "each found font tells its name again");
        if cfg!(windows) {
            let (name, mono) = font_info(std::path::Path::new(r"C:\Windows\Fonts\consola.ttf")).unwrap();
            assert!(name.starts_with("Consolas") && mono, "{name} {mono}");
            let (name, mono) = font_info(std::path::Path::new(r"C:\Windows\Fonts\segoeui.ttf")).unwrap();
            assert!(name.starts_with("Segoe UI") && !mono, "{name} {mono}");
        }
    }
}
