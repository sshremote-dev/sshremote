//! Pictures of the UI without a window, for tests: egui's shapes tessellated and filled in on
//! the CPU, so menus, popups and both themes can be looked at and checked.

use std::collections::HashMap;

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, TextureId, Vec2};

/// The textures egui has handed out so far (the font atlas among them).
#[derive(Default)]
pub struct Snap {
    textures: HashMap<TextureId, ColorImage>,
    /// The shapes of the last frame, to find what was drawn where.
    pub shapes: Vec<egui::epaint::ClippedShape>,
    last: Option<egui::FullOutput>,
}

impl Snap {
    /// One frame of `run`, its textures kept and its shapes remembered.
    pub fn frame(&mut self, ctx: &egui::Context, input: egui::RawInput, run: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let out = ctx.run_ui(input, run);
        for (id, d) in out.textures_delta.set.iter().flat_map(|(id, ds)| ds.iter().map(move |d| (id, d))) {
            let egui::ImageData::Color(img) = &d.image;
            match d.pos {
                None => {
                    self.textures.insert(*id, (**img).clone());
                }
                Some([x, y]) => {
                    if let Some(t) = self.textures.get_mut(id) {
                        for row in 0..img.size[1] {
                            for col in 0..img.size[0] {
                                t.pixels[(y + row) * t.size[0] + x + col] = img.pixels[row * img.size[0] + col];
                            }
                        }
                    }
                }
            }
        }
        for id in &out.textures_delta.free {
            self.textures.remove(id);
        }
        self.shapes = out.shapes.clone();
        self.last = Some(out.clone());
        out
    }

    /// Where text containing `what` was drawn last frame; the last drawn (topmost) first.
    pub fn find(&self, what: &str) -> Vec<Rect> {
        fn walk(s: &egui::Shape, what: &str, out: &mut Vec<Rect>) {
            match s {
                egui::Shape::Text(t) if t.galley.job.text.contains(what) => {
                    out.push(t.galley.rect.translate(t.pos.to_vec2()))
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, what, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in &self.shapes {
            walk(&c.shape, what, &mut out);
        }
        out.reverse();
        out
    }

    /// The last frame as a picture of `size` points, written to `path` as a PNG.
    pub fn save(&self, ctx: &egui::Context, size: Vec2, path: &std::path::Path) {
        let out = self.last.as_ref().expect("a frame first");
        let ppp = out.pixels_per_point;
        let (w, h) = ((size.x * ppp) as usize, (size.y * ppp) as usize);
        let mut px = vec![Color32::BLACK; w * h];
        for prim in ctx.tessellate(out.shapes.clone(), ppp) {
            let egui::epaint::Primitive::Mesh(mesh) = prim.primitive else { continue };
            let clip = Rect::from_min_max(prim.clip_rect.min * ppp, prim.clip_rect.max * ppp)
                .intersect(Rect::from_min_size(Pos2::ZERO, Vec2::new(w as f32, h as f32)));
            let tex = self.textures.get(&mesh.texture_id);
            for tri in mesh.indices.as_chunks::<3>().0 {
                let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
                fill(&mut px, w, clip, ppp, v, tex);
            }
        }
        let file = std::fs::File::create(path).expect("picture file");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let bytes: Vec<u8> = px.iter().flat_map(|c| [c.r(), c.g(), c.b(), 255]).collect();
        enc.write_header().unwrap().write_image_data(&bytes).unwrap();
    }
}

/// One triangle, its colours times its texture, blended over what is there (premultiplied).
fn fill(px: &mut [Color32], w: usize, clip: Rect, ppp: f32, v: [&egui::epaint::Vertex; 3], tex: Option<&ColorImage>) {
    let p: Vec<Pos2> = v.iter().map(|v| v.pos * ppp).collect();
    let area = (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[2].x - p[0].x) * (p[1].y - p[0].y);
    if area.abs() < 1e-6 {
        return;
    }
    let min = Pos2::new(p.iter().map(|q| q.x).fold(f32::MAX, f32::min), p.iter().map(|q| q.y).fold(f32::MAX, f32::min));
    let max = Pos2::new(p.iter().map(|q| q.x).fold(f32::MIN, f32::max), p.iter().map(|q| q.y).fold(f32::MIN, f32::max));
    let (x0, y0) = (min.x.max(clip.min.x).floor() as i32, min.y.max(clip.min.y).floor() as i32);
    let (x1, y1) = (max.x.min(clip.max.x).ceil() as i32, max.y.min(clip.max.y).ceil() as i32);
    for y in y0.max(0)..y1 {
        for x in x0.max(0)..x1 {
            let c = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
            if !clip.contains(c) {
                continue;
            }
            let w0 = ((p[1].x - c.x) * (p[2].y - c.y) - (p[2].x - c.x) * (p[1].y - c.y)) / area;
            let w1 = ((p[2].x - c.x) * (p[0].y - c.y) - (p[0].x - c.x) * (p[2].y - c.y)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                continue;
            }
            let mix = |f: &dyn Fn(&egui::epaint::Vertex) -> f32| w0 * f(v[0]) + w1 * f(v[1]) + w2 * f(v[2]);
            let col = [
                mix(&|v| v.color.r() as f32),
                mix(&|v| v.color.g() as f32),
                mix(&|v| v.color.b() as f32),
                mix(&|v| v.color.a() as f32),
            ];
            // A flat fill points all three corners at the atlas's one white texel: read exactly, as
            // blending it with its neighbours would make the fill see-through.
            let flat = v[0].uv == v[1].uv && v[1].uv == v[2].uv;
            let t = match tex {
                None => [255.0; 4],
                Some(t) if flat => {
                    let (x, y) = ((v[0].uv.x * t.size[0] as f32) as usize, (v[0].uv.y * t.size[1] as f32) as usize);
                    let c = t.pixels[y.min(t.size[1] - 1) * t.size[0] + x.min(t.size[0] - 1)];
                    [c.r() as f32, c.g() as f32, c.b() as f32, c.a() as f32]
                }
                Some(t) => sample(t, mix(&|v| v.uv.x), mix(&|v| v.uv.y)),
            };
            let src = [col[0] * t[0] / 255.0, col[1] * t[1] / 255.0, col[2] * t[2] / 255.0, col[3] * t[3] / 255.0];
            let i = y as usize * w + x as usize;
            if i >= px.len() {
                continue;
            }
            let d = px[i];
            let k = 1.0 - src[3] / 255.0;
            let blend = |s: f32, d: u8| (s + d as f32 * k).round().clamp(0.0, 255.0) as u8;
            px[i] = Color32::from_rgb(blend(src[0], d.r()), blend(src[1], d.g()), blend(src[2], d.b()));
        }
    }
}

/// The texture at `u`, `v`, bilinear, premultiplied.
fn sample(t: &ColorImage, u: f32, v: f32) -> [f32; 4] {
    let (tw, th) = (t.size[0] as f32, t.size[1] as f32);
    let (x, y) = (u * tw - 0.5, v * th - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |x: f32, y: f32| {
        let (x, y) = (x.clamp(0.0, tw - 1.0) as usize, y.clamp(0.0, th - 1.0) as usize);
        let c = t.pixels[y * t.size[0] + x];
        [c.r() as f32, c.g() as f32, c.b() as f32, c.a() as f32]
    };
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
    std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
}

/// A pointer press and release at `at`, as a click.
pub fn click(at: Pos2, button: egui::PointerButton) -> [Vec<egui::Event>; 3] {
    let b = |pressed| egui::Event::PointerButton { pos: at, button, pressed, modifiers: Default::default() };
    [vec![egui::Event::PointerMoved(at)], vec![b(true)], vec![b(false)]]
}

/// Pictures of every menu in both themes, to look at:
/// SSHREMOTE_SNAP_DIR=folder cargo test --release -- --ignored --test-threads=1 menus_
#[cfg(test)]
mod pictures {
    use super::*;
    use crate::gui::ic;
    use egui::{Event, Key, PointerButton};

    fn dir() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var_os("SSHREMOTE_SNAP_DIR").expect("SSHREMOTE_SNAP_DIR"))
    }

    fn input(size: Vec2, events: Vec<Event>, t: f64) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(t),
            focused: true,
            events,
            ..Default::default()
        }
    }

    /// Drives one window: frames, clicks on what was drawn, and pictures.
    struct Shot<'a> {
        ctx: egui::Context,
        snap: Snap,
        size: Vec2,
        t: f64,
        draw: Box<dyn FnMut(&mut egui::Ui) + 'a>,
        name: String,
    }

    impl<'a> Shot<'a> {
        fn new(name: &str, size: Vec2, dark: bool, draw: impl FnMut(&mut egui::Ui) + 'a) -> Self {
            let ctx = egui::Context::default();
            crate::gui::theme::style(&ctx);
            crate::gui::theme::apply_theme(&ctx, dark);
            let name = format!("{name}_{}", if dark { "dark" } else { "light" });
            let mut s = Shot { ctx, snap: Snap::default(), size, t: 0.0, draw: Box::new(draw), name };
            for _ in 0..4 {
                s.frame(Vec::new());
            }
            s
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.t += 0.05;
            let draw = &mut self.draw;
            self.snap.frame(&self.ctx, input(self.size, events, self.t), |ui| draw(ui));
        }

        fn click(&mut self, what: &str, nth: usize, button: PointerButton) {
            let r = *self.snap.find(what).get(nth).unwrap_or_else(|| panic!("{}: no {what:?} drawn", self.name));
            for e in click(r.center(), button) {
                self.frame(e);
            }
            for _ in 0..3 {
                self.frame(Vec::new());
            }
        }

        fn escape(&mut self) {
            self.frame(vec![Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
            self.frame(Vec::new());
        }

        fn save(&mut self, what: &str) {
            // Popups fade in: the picture once they are fully there.
            for _ in 0..8 {
                self.frame(Vec::new());
            }
            self.snap.save(&self.ctx, self.size, &dir().join(format!("{}_{what}.png", self.name)));
        }
    }

    #[test]
    #[ignore]
    fn menus_terminal() {
        for dark in [true, false] {
            let mut w = crate::termwin::TermWindow::new(None);
            let mut s = Shot::new("terminal", Vec2::new(900.0, 560.0), dark, |ui| {
                w.embedded(ui);
            });
            s.save("plain");
            s.click(ic::GEAR, 0, PointerButton::Primary);
            s.save("settings");
            s.escape();
            s.click(ic::CARET_DOWN, 0, PointerButton::Primary);
            s.save("profiles");
            s.escape();
            // The tab's own menu: the last terminal icon drawn first is the view switch; the tab's
            // icon is the leftmost.
            let tab = s.snap.find(ic::TERMINAL).into_iter().min_by(|a, b| a.left().total_cmp(&b.left())).unwrap();
            for e in click(tab.center() + Vec2::new(40.0, 0.0), PointerButton::Secondary) {
                s.frame(e);
            }
            s.frame(Vec::new());
            s.save("tab_menu");
        }
    }

    #[test]
    #[ignore]
    fn menus_chat() {
        let lines: Vec<(bool, String)> = vec![
            (true, "Hi, can you look at the build server?".into()),
            (false, "Sure, connecting now.".into()),
            (false, "I see the disk is almost full. Shall I clear the old logs in /var/log/app? They go back two years.".into()),
            (true, "Yes please.".into()),
            (true, "And restart the service after.".into()),
        ];
        for dark in [true, false] {
            let mut input = String::new();
            let mut s = Shot::new("chat", Vec2::new(380.0, 460.0), dark, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    crate::chat::show_lines_since(ui, &lines, 1, "Remote", "You");
                    ui.add_space(8.0);
                    crate::chat::input_row(ui, &mut input, true);
                });
            });
            s.save("lines");
        }
    }

    #[test]
    #[ignore]
    fn menus_home() {
        for dark in [true, false] {
            let opt = crate::parse_client(&["--gui".into()]).unwrap();
            let mut app = crate::gui::App::new(crate::gui::Form::from(&opt), 100.0, "", None);
            let mut s = Shot::new("open", Vec2::new(900.0, 600.0), dark, |ui| app.connect_screen(ui));
            s.save("ssh");
            s.click(ic::DOTS, 0, PointerButton::Primary);
            s.save("row_menu");
            s.escape();
            for (place, what) in [("New connection", "new"), ("This computer", "local"), ("Tools", "tools")] {
                s.click(place, 0, PointerButton::Primary);
                s.save(what);
            }
        }
    }
}
