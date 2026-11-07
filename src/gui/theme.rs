//! The look shared by every window: the system's own UI and code fonts, text sizes, colours,
//! slightly squared widgets and soft shadows, in a dark and a light variant.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Shadow, Stroke, TextStyle, Vec2,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};


/// The filled variant of the icon font, for a set star.
pub(super) fn filled() -> FontFamily {
    FontFamily::Name("phosphor-fill".into())
}

/// Font files to try, best first, with the face to use in a .ttc collection.
type FontFiles = Vec<(&'static str, u32)>;

/// Corners, slightly squared rather than round: controls and highlights, cards and menus, and
/// dialogs and floating bars.
pub const R_SM: u8 = 3;
pub const R_MD: u8 = 4;
pub const R_LG: u8 = 6;

/// Fonts that may be on this system: for text, then for code.
fn system_fonts() -> (FontFiles, FontFiles) {
    if cfg!(windows) {
        (
            vec![(r"C:\Windows\Fonts\segoeui.ttf", 0), (r"C:\Windows\Fonts\malgun.ttf", 0)],
            vec![(r"C:\Windows\Fonts\CascadiaMono.ttf", 0), (r"C:\Windows\Fonts\consola.ttf", 0)],
        )
    } else if cfg!(target_os = "macos") {
        (
            vec![("/System/Library/Fonts/SFNS.ttf", 0), ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0)],
            vec![("/System/Library/Fonts/SFNSMono.ttf", 0)],
        )
    } else {
        (
            vec![
                ("/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf", 0),
                ("/usr/share/fonts/opentype/cantarell/Cantarell-Regular.otf", 0),
                ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
                ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 1),
            ],
            vec![
                ("/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf", 0),
                ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0),
            ],
        )
    }
}

fn install_fonts(ctx: &egui::Context) {
    crate::markdown::install_bold(ctx);
    // The system's UI font first (one Latin face, then its Korean, Japanese and Chinese fallback),
    // egui's own fonts after it for what they lack. Missing files are skipped.
    let (ui_fonts, mono_fonts) = system_fonts();
    for (family, list) in [(FontFamily::Proportional, ui_fonts), (FontFamily::Monospace, mono_fonts)] {
        let mut latin_done = false;
        // Inserted in reverse at the front, so the list's order is kept.
        let found: Vec<_> = list.into_iter().filter(|(p, _)| std::path::Path::new(p).is_file()).collect();
        let mut picked = Vec::new();
        for (path, index) in found {
            let cjk = path.contains("malgun") || path.contains("CJK") || path.contains("Gothic");
            if cjk || !latin_done {
                latin_done |= !cjk;
                picked.push((path, index));
            }
        }
        for (path, index) in picked.into_iter().rev() {
            let Ok(bytes) = std::fs::read(path) else { continue };
            let mut data = egui::FontData::from_owned(bytes);
            data.index = index;
            let families = vec![InsertFontFamily { family: family.clone(), priority: FontPriority::Highest }];
            ctx.add_font(FontInsert { name: format!("system:{path}"), data, families });
        }
    }
    let regular = egui::FontData::from_static(include_bytes!("../../assets/fonts/Phosphor.ttf"));
    let fill = egui::FontData::from_static(include_bytes!("../../assets/fonts/Phosphor-Fill.ttf"));
    let fallback = |f| InsertFontFamily { family: f, priority: FontPriority::Lowest };
    ctx.add_font(FontInsert {
        name: "phosphor".into(),
        data: regular,
        families: vec![fallback(FontFamily::Proportional), fallback(FontFamily::Monospace)],
    });
    ctx.add_font(FontInsert {
        name: "phosphor-fill".into(),
        data: fill,
        families: vec![InsertFontFamily { family: filled(), priority: FontPriority::Highest }],
    });
    // After the icon font, so its private-use glyphs keep the icons.
    // Symbols that agents and shells draw (prompts, bullets, arrows, spinners) from the system's
    // symbol font.
    let symbols: &[&str] = if cfg!(windows) {
        &[r"C:\Windows\Fonts\seguisym.ttf"]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        ]
    };
    for path in symbols {
        if let Ok(bytes) = std::fs::read(path) {
            let fallback = |f| InsertFontFamily { family: f, priority: FontPriority::Lowest };
            ctx.add_font(FontInsert {
                name: format!("symbols:{path}"),
                data: egui::FontData::from_owned(bytes),
                families: vec![fallback(FontFamily::Proportional), fallback(FontFamily::Monospace)],
            });
        }
    }
}

/// Fonts, sizes, spacing and the remembered theme, for any window of the program.
pub fn style(ctx: &egui::Context) {
    install_fonts(ctx);
    // Ctrl+plus and Ctrl+minus belong to the remote host and the terminal, not to zooming this window.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    // SSHREMOTE_THEME (dark or light) picks one for this run without changing the remembered one.
    let theme = std::env::var("SSHREMOTE_THEME").ok().or_else(|| crate::hosts::setting("theme"));
    apply_theme(ctx, theme.is_none_or(|t| t != "light"));
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, FontId::proportional(20.0)),
            (TextStyle::Monospace, FontId::monospace(13.5)),
        ]
        .into();
        s.spacing.item_spacing = Vec2::new(10.0, 10.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.window_margin = egui::Margin::same(16);
        s.spacing.menu_margin = egui::Margin::same(8);
        s.spacing.icon_width = 18.0;
        s.spacing.icon_width_inner = 10.0;
        s.spacing.scroll.bar_width = 8.0;
        s.spacing.scroll.floating = true;
    });
}

/// Dark or light, remembered between runs.
pub fn set_theme(ctx: &egui::Context, dark: bool) {
    apply_theme(ctx, dark);
    crate::hosts::set_setting("theme", if dark { "dark" } else { "light" });
}

/// Whether the windows are dark now, for colours chosen away from a `Context`.
pub static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn dark() -> bool {
    DARK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Dark or light for the windows now.
fn apply_theme(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, std::sync::atomic::Ordering::Relaxed);
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let (text, weak_line) = if dark {
        v.panel_fill = Color32::from_rgb(20, 21, 25);
        v.window_fill = Color32::from_rgb(30, 32, 38);
        v.extreme_bg_color = Color32::from_rgb(14, 15, 18);
        v.faint_bg_color = Color32::from_rgb(26, 28, 33);
        v.code_bg_color = Color32::from_rgb(36, 38, 46);
        for (w, fill) in [
            (&mut v.widgets.inactive, Color32::from_rgb(40, 43, 51)),
            (&mut v.widgets.hovered, Color32::from_rgb(52, 56, 66)),
            (&mut v.widgets.active, Color32::from_rgb(60, 65, 77)),
            (&mut v.widgets.open, Color32::from_rgb(46, 50, 59)),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
        }
        (Color32::from_rgb(230, 232, 237), Color32::from_white_alpha(18))
    } else {
        v.panel_fill = Color32::from_rgb(244, 245, 248);
        v.window_fill = Color32::WHITE;
        v.extreme_bg_color = Color32::from_rgb(236, 238, 243);
        v.faint_bg_color = Color32::from_rgb(248, 249, 251);
        v.code_bg_color = Color32::from_rgb(236, 238, 243);
        for (w, fill) in [
            (&mut v.widgets.inactive, Color32::from_rgb(233, 235, 240)),
            (&mut v.widgets.hovered, Color32::from_rgb(222, 226, 234)),
            (&mut v.widgets.active, Color32::from_rgb(210, 216, 226)),
            (&mut v.widgets.open, Color32::from_rgb(226, 230, 238)),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
        }
        (Color32::from_rgb(28, 30, 36), Color32::from_black_alpha(22))
    };
    // Frameless buttons (icons, tabs, menu items) show a translucent tint when hovered or pressed,
    // as Windows 11 and VS Code do, so they sit well on any background.
    let tint = |a: u8| if dark { Color32::from_white_alpha(a) } else { Color32::from_black_alpha(a) };
    v.widgets.hovered.weak_bg_fill = tint(18);
    v.widgets.active.weak_bg_fill = tint(10);
    v.widgets.open.weak_bg_fill = tint(14);
    v.override_text_color = None;
    v.widgets.noninteractive.fg_stroke.color = text;
    v.widgets.inactive.fg_stroke.color = text;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, weak_line);
    // Flat buttons: no outline until hovered.
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, weak_line);
    // Selected text and items in VS Code's selection colours, readable in both themes.
    v.selection.bg_fill = if dark { Color32::from_rgb(38, 79, 120) } else { Color32::from_rgb(173, 214, 255) };
    v.selection.stroke = Stroke::new(1.0, text);
    v.hyperlink_color = if dark { Color32::from_rgb(110, 168, 255) } else { Color32::from_rgb(29, 78, 216) };
    // Secondary text (hints, times, captions) dimmer than text but still easy to read.
    v.weak_text_color = Some(if dark { Color32::from_rgb(150, 155, 166) } else { Color32::from_rgb(92, 97, 110) });
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(R_SM);
        w.expansion = 0.0;
    }
    v.window_corner_radius = CornerRadius::same(R_LG);
    v.menu_corner_radius = CornerRadius::same(R_MD);
    v.window_stroke = Stroke::new(1.0, weak_line);
    let shade = if dark { 110 } else { 40 };
    v.window_shadow = Shadow { offset: [0, 10], blur: 32, spread: 0, color: Color32::from_black_alpha(shade) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(shade) };
    v.window_highlight_topmost = false;
    v.striped = true;
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_visuals_of(theme, v);
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
}

/// The main action of a window or form: a button close to the background with an outline, in
/// the text colour, so it stands out without loud colour.
pub(crate) fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).strong())
        .stroke(Stroke::new(1.0, crate::gui::outline()))
        .min_size(Vec2::new(0.0, 32.0))
}

#[cfg(test)]
mod contrast {
    use super::*;
    use crate::gui::{ACCENT, bad, good, selected_fill, warn};

    /// A colour over `bg`, its alpha blended in as egui paints it.
    fn over(c: Color32, bg: Color32) -> Color32 {
        let a = c.a() as f32 / 255.0;
        let mix = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
        if c.a() == 255 { c } else { Color32::from_rgb(mix(c.r(), bg.r()), mix(c.g(), bg.g()), mix(c.b(), bg.b())) }
    }

    fn luminance(c: Color32) -> f32 {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    /// WCAG contrast ratio of `fg` on `bg`.
    fn ratio(fg: Color32, bg: Color32) -> f32 {
        let (a, b) = (luminance(over(fg, bg)), luminance(bg));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Every pairing of text or icon and background the windows use, in both themes: text at
    /// least 4.5 (WCAG AA), large icons and outlines at least 3.
    #[test]
    fn every_pairing_reads_in_both_themes() {
        let mut failures = Vec::new();
        for dark in [true, false] {
            let ctx = egui::Context::default();
            apply_theme(&ctx, dark);
            let v = ctx.style_of(if dark { egui::Theme::Dark } else { egui::Theme::Light }).visuals.clone();
            let text = v.widgets.inactive.fg_stroke.color;
            let weak = v.weak_text_color();
            let bar = crate::gui::chrome::bar_fill(&v);
            let on_tint = over(selected_fill(), bar);
            let hover = over(v.widgets.hovered.weak_bg_fill, bar);
            let pairs: Vec<(&str, Color32, Color32, f32)> = vec![
                ("text on panel", text, v.panel_fill, 4.5),
                ("text on window", text, v.window_fill, 4.5),
                ("text on field", text, v.extreme_bg_color, 4.5),
                ("text on code", text, v.code_bg_color, 4.5),
                ("text on title bar", text, bar, 4.5),
                ("text on button", text, v.widgets.inactive.bg_fill, 4.5),
                ("text on hovered button", text, v.widgets.hovered.bg_fill, 4.5),
                ("text on hovered icon", text, hover, 4.5),
                ("text on selection", text, v.selection.bg_fill, 4.5),
                ("weak text on panel", weak, v.panel_fill, 4.5),
                ("weak text on title bar", weak, bar, 4.5),
                ("weak text on field", weak, v.extreme_bg_color, 4.5),
                ("selected icon on its tint", text, on_tint, 4.5),
                ("red cross on the dock", bad(), bar, 4.5),
                ("link on panel", v.hyperlink_color, v.panel_fill, 4.5),
                ("good text on panel", good(), v.panel_fill, 4.5),
                ("warning text on panel", warn(), v.panel_fill, 4.5),
                ("error text on panel", bad(), v.panel_fill, 4.5),
                ("good on title bar", good(), bar, 4.5),
                ("error text on its notice", bad(), over(bad().gamma_multiply(0.15), v.panel_fill), 4.5),
                ("focus outline on panel", ACCENT, v.panel_fill, 3.0),
                ("own chat bubble", text, if dark { Color32::from_rgb(52, 56, 66) } else { Color32::from_rgb(220, 225, 233) }, 4.5),
            ];
            let mut pairs = pairs;
            for c in crate::gui::AVATAR_COLORS {
                let square = over(c.gamma_multiply(if dark { 0.30 } else { 0.16 }), v.panel_fill);
                pairs.push(("initial on its square", v.strong_text_color(), square, 4.5));
            }
            for (name, fg, bg, need) in pairs {
                let r = ratio(fg, bg);
                eprintln!("{} {name:28} {r:5.2} (needs {need})", if dark { "dark " } else { "light" });
                if r < need {
                    failures.push(format!("{} {name}: {r:.2} < {need}", if dark { "dark" } else { "light" }));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
