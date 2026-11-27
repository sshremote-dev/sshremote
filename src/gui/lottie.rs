//! Lottie animations (`.json`, or `.lottie` packages) for the look: each plays in a thread of its
//! own that draws its frames at the size they are shown, transparency kept; the window takes the
//! newest frame when it draws, so drawing never waits for the animation. A player nobody looks at
//! for a few seconds stops.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

/// Whether a file is a Lottie animation, by its name.
pub(crate) fn is_lottie(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".json") || p.ends_with(".lottie") || built_in(path).is_some()
}

/// A built-in animation (assets/make_scenes.py and make_styles.py write them): its name in the
/// settings, what it is called, whether it never moves (drawn once), how often it is drawn, and
/// the animation.
pub(crate) struct BuiltIn {
    pub id: &'static str,
    pub label: &'static str,
    pub still: bool,
    fps: f32,
    bytes: &'static [u8],
}

macro_rules! built_in {
    ($id:literal, $label:expr, $still:literal, $fps:literal, $file:literal) => {
        BuiltIn { id: concat!("builtin:", $id), label: $label, still: $still, fps: $fps, bytes: include_bytes!(concat!("../../assets/", $file)) }
    };
}

pub(crate) const BUILT_INS: &[BuiltIn] = &[
    built_in!("brand", n!("SSH REMOTE in 3D"), false, 15.0, "brand_motion.json"),
    built_in!("night-motion", n!("Night lake: shooting stars"), false, 24.0, "night_motion.json"),
    built_in!("night-still", n!("Night lake: Milky Way, mountains, pines"), true, 1.0, "night_still.json"),
];

/// Styles that were built in and are now the registry's style packs: where an old setting's layer went.
pub(crate) fn moved(id: &str) -> Option<String> {
    let name = id.strip_prefix("builtin:")?;
    let (style, kind) = name.rsplit_once('-')?;
    let pack = match style {
        "meadow" | "aurora" | "snow" => "dev.sshremote.nature-scenes",
        "radar" | "matrix" | "synthwave" | "circuit" | "warp" | "network" => "dev.sshremote.tech-styles",
        _ => return None,
    };
    matches!(kind, "motion" | "still").then(|| format!("ext:{pack}/{style}_{kind}.json"))
}

pub(crate) fn built_in(id: &str) -> Option<&'static BuiltIn> {
    BUILT_INS.iter().find(|b| b.id == id)
}

/// Moving built-ins are drawn at most this large: soft shapes, they look the same scaled up.
const MOTION_SIDE: f32 = 1280.0;

struct Player {
    key: String,
    /// The newest frame and its number, or why there is none.
    frame: Option<egui::ColorImage>,
    count: u64,
    error: Option<String>,
    /// Last asked for, and whether to play (or stay on the first frame).
    used: Instant,
    play: bool,
}

static PLAYERS: Mutex<Vec<Arc<Mutex<Player>>>> = Mutex::new(Vec::new());

/// When a window of this program last had the keyboard, in ms from the first call: moving
/// backgrounds move only then, and hold their frame while the program is in the background.
static FOCUSED_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn since_epoch() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// Called each frame of each window: one with the keyboard keeps the animations going.
pub(super) fn note_focus(ctx: &egui::Context) {
    if ctx.input(|i| i.focused) {
        FOCUSED_AT.store(since_epoch(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// A window had the keyboard within the last second.
fn awake() -> bool {
    since_epoch().saturating_sub(FOCUSED_AT.load(std::sync::atomic::Ordering::Relaxed)) < 1000
}

/// The animation's frame for now, as a texture `px` pixels on its longer side (rounded up, so a
/// size that changes a little keeps its player).
pub(crate) fn frame(ctx: &egui::Context, path: &str, px: f32, play: bool) -> Result<Option<egui::TextureHandle>, String> {
    let px = if built_in(path).is_some_and(|b| !b.still) { px.min(MOTION_SIDE) } else { px };
    // What stays of the night is drawn once.
    let play = play && !built_in(path).is_some_and(|b| b.still);
    let side = ((px / 64.0).ceil() * 64.0).clamp(64.0, 2048.0) as u32;
    // The brand scene's logo follows the theme, so each theme has its own player.
    let dark = super::theme::dark();
    let key = if path == "builtin:brand" { format!("{path}@{side}@{dark}") } else { format!("{path}@{side}") };
    let player = {
        let mut list = PLAYERS.lock().map_err(|e| e.to_string())?;
        match list.iter().find(|p| p.lock().is_ok_and(|p| p.key == key)) {
            Some(p) => p.clone(),
            None => {
                let p = Arc::new(Mutex::new(Player { key: key.clone(), frame: None, count: 0, error: None, used: Instant::now(), play }));
                list.push(p.clone());
                let (path, ctx, me) = (path.to_string(), ctx.clone(), p.clone());
                std::thread::spawn(move || run(&path, side, dark, &ctx, &me));
                p
            }
        }
    };
    let mut p = player.lock().map_err(|e| e.to_string())?;
    p.used = Instant::now();
    p.play = play;
    if let Some(e) = &p.error {
        return Err(e.clone());
    }
    let id = egui::Id::unique(("lottie-texture", &key));
    let had = ctx.data(|d| d.get_temp::<(egui::TextureHandle, u64)>(id));
    match (had, p.frame.take()) {
        (Some((mut tex, _)), Some(img)) => {
            tex.set(img, egui::TextureOptions::LINEAR);
            ctx.data_mut(|d| d.insert_temp(id, (tex.clone(), p.count)));
            Ok(Some(tex))
        }
        (Some((tex, _)), None) => Ok(Some(tex)),
        (None, Some(img)) => {
            let tex = ctx.load_texture("lottie", img, egui::TextureOptions::LINEAR);
            ctx.data_mut(|d| d.insert_temp(id, (tex.clone(), p.count)));
            Ok(Some(tex))
        }
        (None, None) => Ok(None),
    }
}

/// Everything the renderer can draw is allowed: masks, mattes, parents, effects, text, images.
fn renderer() -> rasterlottie::Renderer {
    let mut p = rasterlottie::SupportProfile::target_corpus();
    p.allow_masks = true;
    p.allow_track_mattes = true;
    p.allow_parenting = true;
    p.allow_effects = true;
    p.allow_text_layers = true;
    p.allow_image_layers = true;
    p.allow_image_assets = true;
    p.allow_expressions = true;
    p.allow_animated_values = true;
    p.allow_unknown_shape_items = true;
    rasterlottie::Renderer::new(p)
}

fn load(path: &str, dark: bool) -> Result<rasterlottie::Animation, String> {
    let bytes = if let Some(b) = built_in(path) {
        b.bytes.to_vec()    } else {
        std::fs::read(path).map_err(|e| e.to_string())?
    };
    // A `.lottie` is a zip package with the animation (and its pictures) inside.
    let text = if bytes.starts_with(b"PK") { unpack(&bytes)? } else { String::from_utf8_lossy(&bytes).into_owned() };
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    normalize(&mut v);
    if path == "builtin:brand" {
        theme_logo(&mut v, dark);
    }
    rasterlottie::Animation::from_json_str(&v.to_string()).map_err(|e| e.to_string())
}

/// The animation in a `.lottie` package, its pictures put inside it as data.
fn unpack(bytes: &[u8]) -> Result<String, String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let names: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    let mut read = |name: &str| -> Option<Vec<u8>> {
        let mut f = zip.by_name(name).ok()?;
        let mut out = Vec::new();
        f.read_to_end(&mut out).ok()?;
        Some(out)
    };
    // The manifest names the animation; else the first one in the package.
    let id = read("manifest.json")
        .and_then(|m| serde_json::from_slice::<serde_json::Value>(&m).ok())
        .and_then(|m| m.get("animations")?.get(0)?.get("id")?.as_str().map(str::to_string));
    let json = id
        .and_then(|id| read(&format!("animations/{id}.json")).or_else(|| read(&format!("a/{id}.json"))))
        .or_else(|| names.iter().find(|n| n.ends_with(".json") && !n.ends_with("manifest.json")).and_then(|n| read(n)))
        .ok_or(n!("no animation in the package"))?;
    let mut v: serde_json::Value = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
    use base64::Engine;
    if let Some(assets) = v.get_mut("assets").and_then(|a| a.as_array_mut()) {
        for a in assets {
            let Some(file) = a.get("p").and_then(|p| p.as_str()).map(str::to_string) else { continue };
            if file.starts_with("data:") {
                continue;
            }
            let found = names.iter().find(|n| n.ends_with(&format!("/{file}")) || **n == file).and_then(|n| read(n));
            if let Some(data) = found {
                let kind = if file.ends_with(".png") { "png" } else if file.ends_with(".webp") { "webp" } else { "jpeg" };
                a["p"] = format!("data:image/{kind};base64,{}", base64::engine::general_purpose::STANDARD.encode(data)).into();
                a["u"] = "".into();
                a["e"] = 1.into();
            }
        }
    }
    Ok(v.to_string())
}

/// The logo layer in the colours of the app icon: the plate and its extruded sides in its blue,
/// the chevron and the frame in white.
fn theme_logo(v: &mut serde_json::Value, dark: bool) {
    use serde_json::Value;
    // The app icon's flat blue for the face, deeper on the sides; the mark in white. A dark
    // theme takes a slightly brighter blue.
    let (low, high): ([f64; 3], [f64; 3]) = if dark { ([0x1a as f64, 0x3f as f64, 0xa8 as f64], [0x3b as f64, 0x7b as f64, 0xf6 as f64]) } else { ([0x15 as f64, 0x37 as f64, 0x93 as f64], [0x25 as f64, 0x63 as f64, 0xeb as f64]) };
    let (chevron, portal) = ([255.0; 3], [255.0; 3]);
    let rgb = |c: [f64; 3]| c.map(|x| x / 255.0);
    // The exported plate runs from about 0.02 to 0.17 in brightness; keep each shade's place.
    let plate = |c: &[f64]| {
        let t = ((c[0] + c[1] + c[2]) / 3.0 - 0.02) / 0.15;
        let t = t.clamp(0.0, 1.0);
        [0, 1, 2].map(|i| (low[i] + (high[i] - low[i]) * t) / 255.0)
    };
    fn walk(v: &mut Value, f: &mut dyn FnMut(&str, &mut Value)) {
        match v {
            Value::Object(m) => {
                if let Some(ty) = m.get("ty").and_then(|t| t.as_str()).map(str::to_string)
                    && matches!(ty.as_str(), "fl" | "st" | "gf" | "gs")
                {
                    f(&ty, v);
                    return;
                }
                m.values_mut().for_each(|x| walk(x, f));
            }
            Value::Array(a) => a.iter_mut().for_each(|x| walk(x, f)),
            _ => {}
        }
    }
    let set = |arr: &mut Vec<Value>, at: usize, c: [f64; 3]| {
        for i in 0..3 {
            arr[at + i] = c[i].into();
        }
    };
    let Some(layers) = v.get_mut("layers").and_then(|l| l.as_array_mut()) else { return };
    for layer in layers.iter_mut().filter(|l| l.get("nm").and_then(|n| n.as_str()) == Some("logo")) {
        walk(layer, &mut |ty, item| match ty {
            "st" | "fl" => {
                let Some(arr) = item.pointer_mut("/c/k").and_then(|k| k.as_array_mut()).filter(|a| a.len() >= 3) else { return };
                let c: Vec<f64> = arr.iter().take(3).filter_map(|x| x.as_f64()).collect();
                if c.len() < 3 {
                    return;
                }
                let new = if ty == "st" { if c[0] + c[1] + c[2] > 1.9 { rgb(chevron) } else { rgb(portal) } } else { plate(&c) };
                set(arr, 0, new);
            }
            _ => {
                let stops = item.pointer("/g/p").and_then(|p| p.as_u64()).unwrap_or(0) as usize;
                let Some(arr) = item.pointer_mut("/g/k/k").and_then(|k| k.as_array_mut()) else { return };
                for s in 0..stops {
                    let at = s * 4 + 1;
                    let c: Vec<f64> = arr.get(at..at + 3).map(|x| x.iter().filter_map(|y| y.as_f64()).collect()).unwrap_or_default();
                    if c.len() == 3 {
                        set(arr, at, plate(&c));
                    }
                }
            }
        });
    }
}

/// What older exporters leave out, filled in as the format reads: a keyframe without its own
/// start value starts where the one before ended; a layer of one colour is a rectangle of it.
fn normalize(v: &mut serde_json::Value) {
    use serde_json::{Value, json};
    match v {
        Value::Object(map) => {
            if let Some(Value::Array(layers)) = map.get_mut("layers") {
                for l in layers.iter_mut() {
                    if l.get("ty").and_then(Value::as_i64) == Some(1) {
                        let (w, h) = (l.get("sw").and_then(Value::as_f64).unwrap_or(0.0), l.get("sh").and_then(Value::as_f64).unwrap_or(0.0));
                        let hex = l.get("sc").and_then(Value::as_str).unwrap_or("#000000").trim_start_matches('#').to_string();
                        let c = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0;
                        l["ty"] = 4.into();
                        l["shapes"] = json!([{ "ty": "gr", "it": [
                            { "ty": "rc", "d": 1, "s": { "a": 0, "k": [w, h] }, "p": { "a": 0, "k": [w / 2.0, h / 2.0] }, "r": { "a": 0, "k": 0 } },
                            { "ty": "fl", "c": { "a": 0, "k": [c(0), c(2), c(4), 1] }, "o": { "a": 0, "k": 100 }, "r": 1 },
                            { "ty": "tr", "p": { "a": 0, "k": [0, 0] }, "a": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [100, 100] }, "r": { "a": 0, "k": 0 }, "o": { "a": 0, "k": 100 } }
                        ] }]);
                    }
                }
            }
            if let Some(Value::Array(keys)) = map.get_mut("k")
                && keys.first().is_some_and(|k| k.get("t").is_some())
            {
                let mut ended: Option<Value> = None;
                for k in keys.iter_mut() {
                    if let Value::Object(o) = k {
                        if !o.contains_key("s")
                            && let Some(e) = ended.clone()
                        {
                            o.insert("s".into(), e);
                        }
                        ended = o.get("e").or_else(|| o.get("s")).cloned();
                    }
                }
                keys.retain(|k| k.get("s").is_some());
            }
            for x in map.values_mut() {
                normalize(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(normalize),
        _ => {}
    }
}

/// The player's thread: frames drawn at the animation's own pace (at most 30 a second) while it
/// plays, the first one while it does not; gone when nobody asks for it.
fn run(path: &str, side: u32, dark: bool, ctx: &egui::Context, me: &Arc<Mutex<Player>>) {
    let fail = |e: String| {
        if let Ok(mut p) = me.lock() {
            p.error = Some(e);
        }
        ctx.request_repaint();
    };
    let anim = match load(path, dark) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let prepared = match renderer().prepare(&anim) {
        Ok(p) => p,
        Err(e) => return fail(e.to_string()),
    };
    let longer = anim.width.max(anim.height).max(1) as f32;
    let config = rasterlottie::RenderConfig::new(rasterlottie::Rgba8::TRANSPARENT, side as f32 / longer);
    let (rate, length) = (anim.frame_rate.max(1.0), anim.duration_frames().max(1.0));
    let step = Duration::from_secs_f32(1.0 / rate.min(built_in(path).map_or(30.0, |b| b.fps)));
    let start = Instant::now();
    let mut drawn_still = false;
    let mut drawn_any = false;
    loop {
        let (play, idle) = match me.lock() {
            Ok(p) => (p.play, p.used.elapsed()),
            Err(_) => return,
        };
        // Longer than a still window goes undrawn (a still remote screen is drawn every ten
        // seconds), or each time it is drawn the animation would be read and prepared anew.
        if idle > Duration::from_secs(30) {
            if let Ok(mut list) = PLAYERS.lock() {
                list.retain(|p| !Arc::ptr_eq(p, me));
            }
            return;
        }
        if !play && drawn_still {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        // In the background the frame shown stays, and nothing is drawn or repainted for it.
        if play && drawn_any && !awake() {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        let at = if play { anim.in_point + (start.elapsed().as_secs_f32() * rate) % length } else { anim.in_point };
        match prepared.render_frame(at, config) {
            Ok(f) => {
                let img = egui::ColorImage::from_rgba_premultiplied([f.width as usize, f.height as usize], &f.pixels);
                if let Ok(mut p) = me.lock() {
                    p.frame = Some(img);
                    p.count += 1;
                }
                ctx.request_repaint();
            }
            Err(e) => return fail(e.to_string()),
        }
        drawn_still = !play;
        drawn_any = true;
        std::thread::sleep(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moving backgrounds move while a window has the keyboard and hold still a second after.
    #[test]
    fn animations_rest_in_the_background() {
        use std::sync::atomic::Ordering::Relaxed;
        let ctx = egui::Context::default();
        let input = |focused| egui::RawInput { focused, ..Default::default() };
        ctx.run_ui(input(true), |ui| note_focus(ui.ctx())).textures_delta.clear();
        assert!(awake(), "a window has the keyboard");
        // As if the last frame with the keyboard were two seconds back.
        FOCUSED_AT.store(since_epoch().saturating_sub(2000).max(1), Relaxed);
        ctx.run_ui(input(false), |ui| note_focus(ui.ctx())).textures_delta.clear();
        assert!(!awake() || since_epoch() < 2000, "in the background");
    }

    #[test]
    fn moved_styles_point_at_their_packs() {
        assert_eq!(moved("builtin:aurora-still").as_deref(), Some("ext:dev.sshremote.nature-scenes/aurora_still.json"));
        assert_eq!(moved("builtin:warp-motion").as_deref(), Some("ext:dev.sshremote.tech-styles/warp_motion.json"));
        assert_eq!(moved("builtin:night-still"), None, "still built in");
        assert!(built_in("builtin:night-still").is_some() && built_in("builtin:radar-still").is_none());
    }

    /// The built-in title scene on a light and a dark theme, into SSHREMOTE_SNAP_DIR.
    #[test]
    #[ignore = "writes pictures; set SSHREMOTE_SNAP_DIR"]
    fn brand_logo_follows_the_theme() {
        let Some(out) = std::env::var_os("SSHREMOTE_SNAP_DIR") else { return };
        for dark in [false, true] {
            let a = load("builtin:brand", dark).unwrap();
            let p = renderer().prepare(&a).unwrap();
            let f = p.render_frame(a.in_point + 40.0, rasterlottie::RenderConfig::new(rasterlottie::Rgba8::new(if dark { 14 } else { 245 }, if dark { 14 } else { 245 }, if dark { 16 } else { 247 }, 255), 0.5)).unwrap();
            let img = image::RgbaImage::from_raw(f.width, f.height, f.pixels).unwrap();
            img.save(std::path::Path::new(&out).join(format!("brand_{}.png", if dark { "dark" } else { "light" }))).unwrap();
        }
    }

    /// Lottie files from SSHREMOTE_LOTTIE_DIR drawn halfway through, on a checkered background so
    /// their transparency shows, into SSHREMOTE_SNAP_DIR; which could not be drawn, and why.
    #[test]
    #[ignore]
    fn lottie_files_render() {
        let (Some(src), Some(out)) = (std::env::var_os("SSHREMOTE_LOTTIE_DIR"), std::env::var_os("SSHREMOTE_SNAP_DIR")) else { return };
        let mut report = Vec::new();
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            let path = e.path().to_string_lossy().into_owned();
            if !is_lottie(&path) || path.ends_with("manifest.json") {
                continue;
            }
            let name = e.path().file_stem().unwrap().to_string_lossy().into_owned();
            let started = Instant::now();
            let got = load(&path, true).and_then(|a| {
                let p = renderer().prepare(&a).map_err(|e| e.to_string())?;
                let side: f32 = std::env::var("SSHREMOTE_LOTTIE_SIZE").ok().and_then(|v| v.parse().ok()).unwrap_or(200.0);
                let at: f32 = std::env::var("SSHREMOTE_LOTTIE_AT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.5);
                let scale = side / a.width.max(a.height).max(1) as f32;
                let mid = a.in_point + a.duration_frames() * at;
                p.render_frame(mid, rasterlottie::RenderConfig::new(rasterlottie::Rgba8::TRANSPARENT, scale)).map_err(|e| e.to_string())
            });
            match got {
                Ok(f) => {
                    let ms = started.elapsed().as_millis();
                    let clear = f.pixels.as_chunks::<4>().0.iter().filter(|p| p[3] == 0).count() * 100 / (f.pixels.len() / 4).max(1);
                    let img = image::RgbaImage::from_fn(f.width, f.height, |x, y| {
                        let i = ((y * f.width + x) * 4) as usize;
                        let a = f.pixels[i + 3] as u32;
                        let check = if (x / 10 + y / 10) % 2 == 0 { 200 } else { 255 };
                        let mix = |c: u8| (c as u32 + check * (255 - a) / 255).min(255) as u8;
                        image::Rgba([mix(f.pixels[i]), mix(f.pixels[i + 1]), mix(f.pixels[i + 2]), 255])
                    });
                    img.save(std::path::Path::new(&out).join(format!("lottie_{name}.png"))).unwrap();
                    report.push(format!("ok   {name}: {}x{} in {ms} ms, {clear}% clear", f.width, f.height));
                }
                Err(e) => report.push(format!("FAIL {name}: {}", e.chars().take(160).collect::<String>())),
            }
        }
        report.sort();
        println!("{}", report.join("\n"));
    }
}
