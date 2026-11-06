//! The terminal window: shells on the remote host in tabs, drawn with
//! Alacritty's terminal emulator. Bytes travel over the session's connection.

use std::sync::mpsc;

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};
use eframe::egui::{self, Align2, Color32, CornerRadius, Event, FontId, Key, Pos2, Rect, RichText, Sense, Vec2};

use crate::gui::{ACCENT, ic};
use crate::proto::ToAgent;

const BACKGROUND: Color32 = Color32::from_rgb(22, 23, 28);
const TAB_STRIP: Color32 = Color32::from_rgb(12, 13, 16);
const TAB_HOVER: Color32 = Color32::from_rgb(34, 36, 43);
const TAB_DIVIDER: Color32 = Color32::from_rgb(52, 55, 64);
const FOREGROUND: Color32 = Color32::from_rgb(215, 218, 224);
const PALETTE: [Color32; 16] = [
    Color32::from_rgb(59, 63, 74),
    Color32::from_rgb(224, 108, 117),
    Color32::from_rgb(152, 195, 121),
    Color32::from_rgb(229, 192, 123),
    Color32::from_rgb(97, 175, 239),
    Color32::from_rgb(198, 120, 221),
    Color32::from_rgb(86, 182, 194),
    Color32::from_rgb(215, 218, 224),
    Color32::from_rgb(92, 99, 112),
    Color32::from_rgb(239, 123, 132),
    Color32::from_rgb(169, 212, 138),
    Color32::from_rgb(240, 207, 141),
    Color32::from_rgb(124, 192, 245),
    Color32::from_rgb(212, 143, 234),
    Color32::from_rgb(111, 203, 214),
    Color32::from_rgb(255, 255, 255),
];

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Clone)]
struct Listener(mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, e: TermEvent) {
        let _ = self.0.send(e);
    }
}

struct Tab {
    id: u32,
    term: Term<Listener>,
    parser: Processor,
    events: mpsc::Receiver<TermEvent>,
    title: String,
    exited: Option<String>,
    size: (u16, u16),
}

/// What the window asks the session to do.
pub enum Cmd {
    Send(ToAgent),
    Close,
}

pub struct TermWindow {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
    font_size: f32,
    /// A command that opens the same host in the system's own terminal, if there is one.
    system: Option<Vec<String>>,
}

fn color(c: Color, colors: &alacritty_terminal::term::color::Colors) -> Color32 {
    let rgb = |r: Rgb| Color32::from_rgb(r.r, r.g, r.b);
    match c {
        Color::Spec(r) => rgb(r),
        Color::Indexed(i) => colors[i as usize].map(rgb).unwrap_or_else(|| indexed(i)),
        Color::Named(n) => {
            if let Some(r) = colors[n] {
                return rgb(r);
            }
            match n {
                NamedColor::Foreground | NamedColor::Cursor => FOREGROUND,
                NamedColor::Background => BACKGROUND,
                NamedColor::BrightForeground => Color32::WHITE,
                NamedColor::DimForeground => FOREGROUND.gamma_multiply(0.66),
                n if (n as usize) < 16 => PALETTE[n as usize],
                // DimBlack.. DimWhite follow the eight base colours.
                n => PALETTE[(n as usize).saturating_sub(NamedColor::DimBlack as usize) % 8].gamma_multiply(0.66),
            }
        }
    }
}

/// The xterm 256-colour palette.
fn indexed(i: u8) -> Color32 {
    match i {
        0..16 => PALETTE[i as usize],
        16..232 => {
            let v = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            let n = i - 16;
            Color32::from_rgb(v(n / 36), v(n / 6 % 6), v(n % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

/// Bytes a key sends to the shell, honouring application cursor mode.
fn key_bytes(key: Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let csi = |code: &str, letter: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if app_cursor && code.is_empty() {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{code}{letter}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier > 1 { format!("\x1b[{n};{modifier}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let ss3 = |letter: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else {
            format!("\x1bO{letter}").into_bytes()
        }
    };
    Some(match key {
        Key::Enter => b"\r".to_vec(),
        Key::Backspace if m.ctrl => b"\x08".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab if m.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        Key::ArrowUp => csi("", 'A'),
        Key::ArrowDown => csi("", 'B'),
        Key::ArrowRight => csi("", 'C'),
        Key::ArrowLeft => csi("", 'D'),
        Key::Home => csi("", 'H'),
        Key::End => csi("", 'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => ss3('P'),
        Key::F2 => ss3('Q'),
        Key::F3 => ss3('R'),
        Key::F4 => ss3('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ => {
            // Ctrl or Alt with a letter, digit or symbol; plain text arrives as Event::Text.
            let name = key.symbol_or_name();
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            let c = c.to_ascii_lowercase();
            if m.ctrl && !m.alt {
                return match c {
                    'a'..='z' => Some(vec![c as u8 - b'a' + 1]),
                    ' ' | '2' | '@' => Some(vec![0]),
                    '[' | '3' => Some(vec![0x1b]),
                    '\\' | '4' => Some(vec![0x1c]),
                    ']' | '5' => Some(vec![0x1d]),
                    '6' | '^' => Some(vec![0x1e]),
                    '/' | '-' | '7' => Some(vec![0x1f]),
                    _ => None,
                };
            }
            if m.alt && !m.ctrl && c.is_ascii_graphic() {
                let c = if m.shift { c.to_ascii_uppercase() } else { c };
                return Some(vec![0x1b, c as u8]);
            }
            return None;
        }
    })
}

impl TermWindow {
    pub fn new(system: Option<Vec<String>>) -> Self {
        TermWindow { tabs: Vec::new(), active: 0, next_id: 1, font_size: 14.0, system }
    }

    fn open_tab(&mut self, out: &mut Vec<Cmd>) {
        let (tx, rx) = mpsc::channel();
        let size = (100u16, 30u16);
        let term = Term::new(Config::default(), &Size { cols: size.0 as usize, rows: size.1 as usize }, Listener(tx));
        let id = self.next_id;
        self.next_id += 1;
        out.push(Cmd::Send(ToAgent::TermOpen { id, cols: size.0, rows: size.1 }));
        self.tabs.push(Tab {
            id,
            term,
            parser: Processor::new(),
            events: rx,
            title: format!("Shell {id}"),
            exited: None,
            size,
        });
        self.active = self.tabs.len() - 1;
    }

    pub fn output(&mut self, id: u32, data: &[u8]) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.parser.advance(&mut t.term, data);
        }
    }

    pub fn exited(&mut self, id: u32, text: String) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.exited = Some(text);
        }
    }

    /// Ends every shell, for when the window closes.
    pub fn close_all(&self) -> Vec<Cmd> {
        self.tabs.iter().filter(|t| t.exited.is_none()).map(|t| Cmd::Send(ToAgent::TermClose { id: t.id })).collect()
    }

    pub fn show(&mut self, ctx: &egui::Context, host: &str) -> Vec<Cmd> {
        let mut out = Vec::new();
        if self.tabs.is_empty() {
            self.open_tab(&mut out);
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("Terminal - {host}"))
            .with_inner_size([900.0, 560.0])
            .with_min_inner_size([360.0, 200.0]);
        if let Some(icon) = crate::gui::app_icon() {
            builder = builder.with_icon(icon);
        }
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("sshremote-terminal"), builder, |ui, _| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                out.extend(self.close_all());
                out.push(Cmd::Close);
                return;
            }
            self.body(ui, &mut out);
        });
        // Replies the emulator owes the shell (cursor position, colours) and title changes.
        for t in &mut self.tabs {
            for e in t.events.try_iter() {
                match e {
                    TermEvent::PtyWrite(s) => {
                        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: s.into_bytes() }))
                    }
                    TermEvent::Title(s) => t.title = tab_title(&s),
                    TermEvent::ResetTitle => t.title = format!("Shell {}", t.id),
                    TermEvent::ClipboardStore(_, s) => ctx.copy_text(s),
                    _ => {}
                }
            }
        }
        out
    }

    fn body(&mut self, ui: &mut egui::Ui, out: &mut Vec<Cmd>) {
        // A tab strip as in Windows Terminal or a browser: the active tab joins the terminal below.
        let strip =
            egui::Frame::new().fill(TAB_STRIP).inner_margin(egui::Margin { left: 8, right: 6, top: 6, bottom: 0 });
        egui::Panel::top("term-tabs").frame(strip).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
            ui.horizontal(|ui| {
                let mut close = None;
                let n = self.tabs.len().max(1) as f32;
                let room = (ui.available_width() - 150.0).max(120.0);
                let tab_w = (room / n).clamp(110.0, 220.0);
                for i in 0..self.tabs.len() {
                    let active = i == self.active;
                    let (rect, r) = ui.allocate_exact_size(Vec2::new(tab_w, 34.0), Sense::click());
                    let hovered = r.hovered();
                    let p = ui.painter();
                    let top_round = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
                    if active {
                        p.rect_filled(rect, top_round, BACKGROUND);
                    } else if hovered {
                        p.rect_filled(rect.shrink2(Vec2::new(2.0, 3.0)), CornerRadius::same(6), TAB_HOVER);
                    } else if i + 1 != self.active && i + 1 < self.tabs.len() {
                        // A thin divider between two inactive tabs.
                        let x = rect.right() - 0.5;
                        p.vline(x, rect.y_range().shrink(9.0), egui::Stroke::new(1.0, TAB_DIVIDER));
                    }
                    let t = &self.tabs[i];
                    let ended = t.exited.is_some();
                    let fg = if active { Color32::WHITE } else { Color32::from_gray(170) };
                    let fg = if ended { fg.gamma_multiply(0.5) } else { fg };
                    p.text(
                        rect.left_center() + Vec2::new(12.0, 0.0),
                        Align2::LEFT_CENTER,
                        ic::TERMINAL,
                        FontId::proportional(14.0),
                        fg,
                    );
                    // The close button appears on the active tab and under the pointer, as in browsers.
                    let close_rect =
                        Rect::from_center_size(rect.right_center() - Vec2::new(16.0, 0.0), Vec2::splat(20.0));
                    let show_close = active || hovered;
                    let title_right = if show_close { close_rect.left() - 4.0 } else { rect.right() - 10.0 };
                    let mut title = t.title.clone();
                    if ended {
                        title.push_str(" (ended)");
                    }
                    let title_rect = Rect::from_min_max(
                        Pos2::new(rect.left() + 32.0, rect.top()),
                        Pos2::new(title_right, rect.bottom()),
                    );
                    let galley = p.layout_no_wrap(title, FontId::proportional(13.0), fg);
                    let clip = ui.painter_at(title_rect);
                    clip.galley(
                        Pos2::new(title_rect.left(), rect.center().y - galley.size().y / 2.0),
                        galley.clone(),
                        fg,
                    );
                    if galley.size().x > title_rect.width() {
                        // Fade the cut end instead of ending mid-letter.
                        let bg = if active {
                            BACKGROUND
                        } else if hovered {
                            TAB_HOVER
                        } else {
                            TAB_STRIP
                        };
                        let fade = Rect::from_min_max(
                            Pos2::new(title_rect.right() - 18.0, rect.top() + 6.0),
                            Pos2::new(title_rect.right(), rect.bottom() - 6.0),
                        );
                        for k in 0..6 {
                            let a = (k as f32 + 1.0) / 6.0;
                            let x0 = fade.left() + fade.width() * k as f32 / 6.0;
                            let slice =
                                Rect::from_min_max(Pos2::new(x0, fade.top()), Pos2::new(fade.right(), fade.bottom()));
                            ui.painter().rect_filled(slice, CornerRadius::ZERO, bg.gamma_multiply(a * 0.35));
                        }
                    }
                    if show_close {
                        let cr = ui.interact(close_rect, ui.make_persistent_id(("close-tab", t.id)), Sense::click());
                        if cr.hovered() {
                            ui.painter().circle_filled(close_rect.center(), 10.0, Color32::from_white_alpha(28));
                        }
                        ui.painter().text(
                            close_rect.center(),
                            Align2::CENTER_CENTER,
                            ic::X,
                            FontId::proportional(12.0),
                            fg,
                        );
                        if cr.clicked() {
                            close = Some(i);
                        }
                        cr.on_hover_text("Close (middle click on the tab also closes)");
                    }
                    if r.clicked() && close.is_none() {
                        self.active = i;
                    }
                    if r.middle_clicked() {
                        close = Some(i);
                    }
                    r.on_hover_text(&self.tabs[i].title);
                }
                ui.add_space(4.0);
                let (plus, pr) = ui.allocate_exact_size(Vec2::new(32.0, 30.0), Sense::click());
                if pr.hovered() {
                    ui.painter().rect_filled(plus.shrink(2.0), CornerRadius::same(6), TAB_HOVER);
                }
                ui.painter().text(
                    plus.center(),
                    Align2::CENTER_CENTER,
                    ic::PLUS,
                    FontId::proportional(15.0),
                    Color32::from_gray(200),
                );
                if pr.on_hover_text("New shell (Ctrl+Shift+T)").clicked() {
                    self.open_tab(out);
                }
                if let Some(i) = close {
                    let t = self.tabs.remove(i);
                    if t.exited.is_none() {
                        out.push(Cmd::Send(ToAgent::TermClose { id: t.id }));
                    }
                    if self.tabs.is_empty() {
                        out.push(Cmd::Close);
                    }
                    if self.active > i || self.active >= self.tabs.len() {
                        self.active = self.active.saturating_sub(1);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let small = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                        let b = egui::Button::new(RichText::new(glyph).size(14.0).color(Color32::from_gray(190)))
                            .frame_when_inactive(false)
                            .min_size(Vec2::splat(28.0));
                        ui.add(b).on_hover_text(tip).clicked()
                    };
                    if let Some(cmd) = &self.system
                        && small(ui, ic::ARROW_SQUARE_OUT, "Open this host in the system terminal (ssh)")
                    {
                        let _ = spawn_system(cmd);
                    }
                    if small(ui, ic::PLUS, "Larger text (Ctrl+=)") {
                        self.font_size = (self.font_size + 1.0).min(32.0);
                    }
                    ui.label(RichText::new(format!("{:.0}", self.font_size)).size(12.0).color(Color32::from_gray(140)));
                    if small(ui, ic::MINUS, "Smaller text (Ctrl+-)") {
                        self.font_size = (self.font_size - 1.0).max(8.0);
                    }
                });
            });
        });
        // Keyboard shortcuts of the window itself, as in other terminals.
        let (new_tab, close_tab, next, bigger, smaller) = ui.ctx().input(|i| {
            let cs = i.modifiers.ctrl && i.modifiers.shift;
            (
                cs && i.key_pressed(Key::T),
                cs && i.key_pressed(Key::W),
                i.modifiers.ctrl && i.key_pressed(Key::Tab),
                i.modifiers.ctrl && (i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus)),
                i.modifiers.ctrl && i.key_pressed(Key::Minus),
            )
        });
        if new_tab {
            self.open_tab(out);
        }
        if close_tab && !self.tabs.is_empty() {
            let t = self.tabs.remove(self.active);
            if t.exited.is_none() {
                out.push(Cmd::Send(ToAgent::TermClose { id: t.id }));
            }
            if self.tabs.is_empty() {
                out.push(Cmd::Close);
            }
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
        if next && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        if bigger {
            self.font_size = (self.font_size + 1.0).min(32.0);
        }
        if smaller {
            self.font_size = (self.font_size - 1.0).max(8.0);
        }
        let frame = egui::Frame::new().fill(BACKGROUND).inner_margin(6.0);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            let font_size = self.font_size;
            let Some(t) = self.tabs.get_mut(self.active) else { return };
            screen(ui, t, font_size, out);
        });
    }
}

/// Draws a shell and passes keys, paste and the mouse wheel to it.
fn screen(ui: &mut egui::Ui, t: &mut Tab, font_size: f32, out: &mut Vec<Cmd>) {
    let font = FontId::monospace(font_size);
    let (cw, ch) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let area = ui.available_rect_before_wrap();
    let cols = ((area.width() / cw).floor() as u16).max(2);
    let rows = ((area.height() / ch).floor() as u16).max(1);
    if (cols, rows) != t.size {
        t.size = (cols, rows);
        t.term.resize(Size { cols: cols as usize, rows: rows as usize });
        if t.exited.is_none() {
            out.push(Cmd::Send(ToAgent::TermResize { id: t.id, cols, rows }));
        }
    }
    let resp = ui.allocate_rect(area, Sense::click_and_drag());
    let origin = area.min;
    let offset = t.term.grid().display_offset() as i32;
    // Cell under a screen position, as a grid point.
    let cell_at = |p: Pos2| {
        let col = (((p.x - origin.x) / cw).floor().max(0.0) as usize).min(cols as usize - 1);
        let row = (((p.y - origin.y) / ch).floor().max(0.0) as i32).min(rows as i32 - 1);
        let side = if (p.x - origin.x) / cw - col as f32 > 0.5 { Side::Right } else { Side::Left };
        (Point::new(Line(row - offset), Column(col)), side)
    };

    // Mouse: drag selects, double click selects a word, the wheel scrolls back.
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (pt, side) = cell_at(p);
        t.term.selection = Some(Selection::new(SelectionType::Simple, pt, side));
    }
    if resp.dragged_by(egui::PointerButton::Primary)
        && let (Some(p), Some(sel)) = (resp.interact_pointer_pos(), t.term.selection.as_mut())
    {
        let (pt, side) = cell_at(p);
        sel.update(pt, side);
    }
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (pt, side) = cell_at(p);
        t.term.selection = Some(Selection::new(SelectionType::Semantic, pt, side));
    } else if resp.clicked() {
        t.term.selection = None;
    }
    let mut copy_now = false;
    let mut paste_now = false;
    resp.context_menu(|ui| {
        if ui.button("Copy").clicked() {
            copy_now = true;
            ui.close();
        }
        if ui.button("Paste").clicked() {
            paste_now = true;
            ui.close();
        }
    });
    if copy_now && let Some(s) = t.term.selection_to_string() {
        ui.ctx().copy_text(s);
    }
    if paste_now {
        // The paste arrives as an Event::Paste on the next frame.
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }
    let mode = *t.term.mode();
    let mut send: Vec<u8> = Vec::new();
    let events = ui.ctx().input(|i| i.events.clone());
    for e in events {
        match e {
            Event::Text(s) => send.extend_from_slice(s.as_bytes()),
            Event::Copy => match t.term.selection_to_string() {
                Some(s) if !s.is_empty() => {
                    ui.ctx().copy_text(s);
                    t.term.selection = None;
                }
                _ => send.push(0x03),
            },
            Event::Cut => send.push(0x18),
            Event::MouseWheel { unit, delta, .. } if resp.hovered() => {
                let lines = match unit {
                    egui::MouseWheelUnit::Point => delta.y / ch,
                    egui::MouseWheelUnit::Line => delta.y,
                    egui::MouseWheelUnit::Page => delta.y * rows as f32,
                };
                let lines = if lines.abs() < 1.0 { lines.signum() as i32 } else { lines.round() as i32 };
                if lines == 0 {
                    continue;
                }
                if mode.contains(TermMode::ALT_SCREEN) {
                    // Full-screen programs (less, vim) get arrow keys instead.
                    let key = if lines > 0 { b"OA" } else { b"OB" };
                    for _ in 0..lines.unsigned_abs() {
                        send.extend_from_slice(key);
                    }
                } else {
                    t.term.scroll_display(Scroll::Delta(lines));
                }
            }
            Event::Paste(s) => {
                let s = s.replace("\r\n", "\r").replace('\n', "\r");
                if mode.contains(TermMode::BRACKETED_PASTE) {
                    send.extend_from_slice(b"\x1b[200~");
                    send.extend_from_slice(s.as_bytes());
                    send.extend_from_slice(b"\x1b[201~");
                } else {
                    send.extend_from_slice(s.as_bytes());
                }
            }
            Event::Key { key, pressed: true, modifiers, .. } => {
                // The window's own shortcuts (new or close tab, next tab, text size) are not typed.
                let cs = modifiers.ctrl && modifiers.shift;
                if (cs && matches!(key, Key::T | Key::W))
                    || (modifiers.ctrl && matches!(key, Key::Tab | Key::Equals | Key::Plus | Key::Minus))
                {
                    continue;
                }
                if modifiers.shift && matches!(key, Key::PageUp | Key::PageDown) {
                    t.term.scroll_display(if key == Key::PageUp { Scroll::PageUp } else { Scroll::PageDown });
                    continue;
                }
                if let Some(b) = key_bytes(key, modifiers, mode.contains(TermMode::APP_CURSOR)) {
                    send.extend_from_slice(&b);
                }
            }
            _ => {}
        }
    }
    if !send.is_empty() && t.exited.is_none() {
        t.term.scroll_display(Scroll::Bottom);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    }

    // Paint the cells: backgrounds, then text in runs of the same colour.
    let painter = ui.painter_at(area);
    let content = t.term.renderable_content();
    let colors = content.colors;
    let sel = content.selection;
    let cursor = content.cursor.point;
    let offset = content.display_offset as i32;
    let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR) && offset == 0 && t.exited.is_none();
    let mut run = String::new();
    let mut run_start = Pos2::ZERO;
    let mut run_color = FOREGROUND;
    let flush = |run: &mut String, at: Pos2, color: Color32| {
        if !run.trim().is_empty() {
            painter.text(at, Align2::LEFT_TOP, run.as_str(), font.clone(), color);
        }
        run.clear();
    };
    let mut last_row = i32::MIN;
    for cell in content.display_iter {
        let row = cell.point.line.0 + offset;
        let col = cell.point.column.0;
        if row != last_row {
            flush(&mut run, run_start, run_color);
            last_row = row;
        }
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let pos = Pos2::new(origin.x + col as f32 * cw, origin.y + row as f32 * ch);
        let wide = flags.contains(Flags::WIDE_CHAR);
        let width = if wide { 2.0 * cw } else { cw };
        let mut fg = color(cell.fg, colors);
        let mut bg = color(cell.bg, colors);
        if flags.contains(Flags::BOLD)
            && let Color::Named(n) = cell.fg
            && (n as usize) < 8
        {
            fg = PALETTE[n as usize + 8];
        }
        if flags.contains(Flags::DIM) {
            fg = fg.gamma_multiply(0.66);
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let selected = sel.is_some_and(|s| s.contains(cell.point));
        if selected {
            bg = ACCENT.gamma_multiply(0.55);
        }
        let at_cursor = show_cursor && cell.point == cursor;
        if at_cursor {
            (fg, bg) = (BACKGROUND, FOREGROUND);
        }
        let cell_rect = Rect::from_min_size(pos, Vec2::new(width, ch));
        if bg != BACKGROUND {
            painter.rect_filled(cell_rect, CornerRadius::ZERO, bg);
        }
        if flags.contains(Flags::UNDERLINE) {
            painter.hline(cell_rect.x_range(), cell_rect.bottom() - 1.5, egui::Stroke::new(1.0, fg));
        }
        let c = if flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
        // ASCII shares the grid exactly; anything else is placed on its own cell.
        if c.is_ascii() && !wide && fg == run_color && !run.is_empty() {
            run.push(c);
            continue;
        }
        flush(&mut run, run_start, run_color);
        if c.is_ascii() && !wide {
            run_start = pos;
            run_color = fg;
            run.push(c);
        } else if c != ' ' {
            painter.text(cell_rect.center(), Align2::CENTER_CENTER, c, font.clone(), fg);
        }
    }
    flush(&mut run, run_start, run_color);
    if !show_cursor || t.exited.is_some() {
        // Nothing more to draw for the cursor.
    }
    if let Some(text) = &t.exited {
        let r = Rect::from_min_size(Pos2::new(area.left(), area.bottom() - 28.0), Vec2::new(area.width(), 28.0));
        painter.rect_filled(r, CornerRadius::same(6), Color32::from_rgba_unmultiplied(40, 42, 50, 235));
        painter.text(
            r.left_center() + Vec2::new(10.0, 0.0),
            Align2::LEFT_CENTER,
            format!("{text}. Close the tab or open a new one."),
            FontId::proportional(13.0),
            FOREGROUND,
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
}

/// A shell's title made short for a tab: "Administrator: C:\...\powershell.exe" becomes "powershell".
fn tab_title(raw: &str) -> String {
    let t = raw.trim().trim_start_matches("Administrator:").trim();
    if t.contains('\\') && !t.contains(' ') || t.ends_with(".exe") {
        let last = t.rsplit(['\\', '/']).next().unwrap_or(t);
        return last.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
    }
    t.to_string()
}

/// Runs `cmd` in a new window of the system's terminal.
fn spawn_system(cmd: &[String]) -> std::io::Result<()> {
    if cfg!(windows) {
        // "start" opens the default terminal (Windows Terminal or the console host).
        std::process::Command::new("cmd").arg("/c").arg("start").arg("").args(cmd).spawn()?;
        return Ok(());
    }
    for (term, flag) in [("x-terminal-emulator", "-e"), ("gnome-terminal", "--"), ("konsole", "-e"), ("xterm", "-e")] {
        if std::process::Command::new(term).arg(flag).args(cmd).spawn().is_ok() {
            return Ok(());
        }
    }
    Err(std::io::Error::other("no terminal program found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_titles_are_short() {
        assert_eq!(tab_title(r"Administrator: C:\WINDOWS\System32\WindowsPowerShell1.0\powershell.exe"), "powershell");
        assert_eq!(tab_title("a@server: ~"), "a@server: ~");
    }

    #[test]
    fn keys_become_terminal_bytes() {
        let none = egui::Modifiers::NONE;
        assert_eq!(key_bytes(Key::ArrowUp, none, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, none, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(Key::ArrowLeft, egui::Modifiers::CTRL, false).unwrap(), b"\x1b[1;5D");
        assert_eq!(key_bytes(Key::C, egui::Modifiers::CTRL, false).unwrap(), [3]);
        assert_eq!(key_bytes(Key::F5, none, false).unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes(Key::B, egui::Modifiers::ALT, false).unwrap(), b"\x1bb");
        assert!(key_bytes(Key::A, none, false).is_none());
    }

    #[test]
    fn the_emulator_answers_cursor_queries() {
        let (tx, rx) = mpsc::channel();
        let mut term = Term::new(Config::default(), &Size { cols: 20, rows: 5 }, Listener(tx));
        let mut p: Processor = Processor::new();
        p.advance(&mut term, b"ab\x1b[6n");
        let replies: Vec<String> =
            rx.try_iter().filter_map(|e| if let TermEvent::PtyWrite(s) = e { Some(s) } else { None }).collect();
        assert_eq!(replies, ["\x1b[1;3R"]);
    }
}
