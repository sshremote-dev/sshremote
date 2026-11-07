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

const TAB_STRIP: Color32 = Color32::from_rgb(12, 13, 16);
const TAB_HOVER: Color32 = Color32::from_rgb(34, 36, 43);
const TAB_DIVIDER: Color32 = Color32::from_rgb(52, 55, 64);

/// A colour scheme: name, background, foreground, the sixteen ANSI colours.
struct Scheme(&'static str, [u8; 3], [u8; 3], [[u8; 3]; 16]);

const SCHEMES: [Scheme; 6] = [
    Scheme("One Half Dark", [22, 23, 28], [215, 218, 224], [
        [59, 63, 74], [224, 108, 117], [152, 195, 121], [229, 192, 123], [97, 175, 239], [198, 120, 221], [86, 182, 194], [215, 218, 224],
        [92, 99, 112], [239, 123, 132], [169, 212, 138], [240, 207, 141], [124, 192, 245], [212, 143, 234], [111, 203, 214], [255, 255, 255],
    ]),
    Scheme("Campbell", [12, 12, 12], [204, 204, 204], [
        [12, 12, 12], [197, 15, 31], [19, 161, 14], [193, 156, 0], [0, 55, 218], [136, 23, 152], [58, 150, 221], [204, 204, 204],
        [118, 118, 118], [231, 72, 86], [22, 198, 12], [249, 241, 165], [59, 120, 255], [180, 0, 158], [97, 214, 214], [242, 242, 242],
    ]),
    Scheme("Dracula", [40, 42, 54], [248, 248, 242], [
        [33, 34, 44], [255, 85, 85], [80, 250, 123], [241, 250, 140], [189, 147, 249], [255, 121, 198], [139, 233, 253], [248, 248, 242],
        [98, 114, 164], [255, 110, 110], [105, 255, 148], [255, 255, 165], [214, 172, 255], [255, 146, 223], [164, 255, 255], [255, 255, 255],
    ]),
    Scheme("Solarized Dark", [0, 43, 54], [131, 148, 150], [
        [7, 54, 66], [220, 50, 47], [133, 153, 0], [181, 137, 0], [38, 139, 210], [211, 54, 130], [42, 161, 152], [238, 232, 213],
        [0, 43, 54], [203, 75, 22], [88, 110, 117], [101, 123, 131], [131, 148, 150], [108, 113, 196], [147, 161, 161], [253, 246, 227],
    ]),
    Scheme("Tango Dark", [0, 0, 0], [211, 215, 207], [
        [0, 0, 0], [204, 0, 0], [78, 154, 6], [196, 160, 0], [52, 101, 164], [117, 80, 123], [6, 152, 154], [211, 215, 207],
        [85, 87, 83], [239, 41, 41], [138, 226, 52], [252, 233, 79], [114, 159, 207], [173, 127, 168], [52, 226, 226], [238, 238, 236],
    ]),
    Scheme("One Half Light", [250, 250, 250], [56, 58, 66], [
        [56, 58, 66], [228, 86, 73], [80, 161, 79], [193, 132, 1], [1, 132, 188], [166, 38, 164], [9, 151, 179], [250, 250, 250],
        [79, 82, 93], [223, 108, 117], [152, 195, 121], [228, 192, 122], [97, 175, 239], [197, 119, 221], [86, 181, 193], [255, 255, 255],
    ]),
];

/// The scheme in use, shared by every terminal window.
static SCHEME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn scheme_bg() -> Color32 {
    rgb(SCHEMES[SCHEME.load(std::sync::atomic::Ordering::Relaxed) % SCHEMES.len()].1)
}

fn scheme_fg() -> Color32 {
    rgb(SCHEMES[SCHEME.load(std::sync::atomic::Ordering::Relaxed) % SCHEMES.len()].2)
}

fn pal(i: usize) -> Color32 {
    rgb(SCHEMES[SCHEME.load(std::sync::atomic::Ordering::Relaxed) % SCHEMES.len()].3[i % 16])
}

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

/// Where a tab's program runs.
enum Backend {
    /// A shell on the SSH host, through the remote-screen session's agent.
    Agent,
    /// A program on this computer in a pseudo-terminal: a local shell or `ssh HOST`.
    Local(crate::pty::Local),
}

/// How a tab is shown: the live terminal, its text, or its text read as Markdown.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Terminal,
    Text,
    Markdown,
}

/// Find in the scrollback, as Ctrl+Shift+F in Windows Terminal.
#[derive(Default)]
struct Search {
    query: String,
    found: Option<alacritty_terminal::term::search::Match>,
    missing: bool,
}

struct Tab {
    id: u32,
    term: Term<Listener>,
    parser: Processor,
    events: mpsc::Receiver<TermEvent>,
    title: String,
    /// A name the user gave the tab, which the program's title does not replace.
    custom: Option<String>,
    exited: Option<String>,
    size: (u16, u16),
    backend: Backend,
    view: View,
    search: Option<Search>,
    /// Agent markers and frames removed in the Markdown view.
    clean: bool,
    /// The tab of the strip this pane belongs to; panes of one tab share its area.
    group: u32,
    /// How this pane divides the area of the pane before it: Some(true) side by side, Some(false)
    /// one above the other; None for a tab's first pane.
    dir: Option<bool>,
    /// What it runs, to open the same in a new pane.
    profile: Profile,
    /// Text an input method is composing (Korean, Japanese, Chinese), drawn at the cursor.
    preedit: String,
}

/// A tab or pane waiting for the window's context: what it runs, and the pane it splits (with the
/// direction) if any.
type Pending = (Profile, Option<(u32, bool)>);

/// What a pane asks of its window.
#[derive(Default)]
struct PaneAsk {
    focus: bool,
    split: Option<bool>,
    close: bool,
}

/// Areas of a tab's panes: each pane takes half of the area of the pane before it.
fn pane_rects(area: Rect, dirs: &[Option<bool>]) -> Vec<Rect> {
    const GAP: f32 = 3.0;
    let mut rects = vec![area];
    for d in dirs.iter().skip(1) {
        let prev = rects.pop().unwrap();
        let (a, b) = if *d == Some(true) {
            let x = prev.center().x;
            (Rect::from_min_max(prev.min, Pos2::new(x - GAP, prev.max.y)), Rect::from_min_max(Pos2::new(x + GAP, prev.min.y), prev.max))
        } else {
            let y = prev.center().y;
            (Rect::from_min_max(prev.min, Pos2::new(prev.max.x, y - GAP)), Rect::from_min_max(Pos2::new(prev.min.x, y + GAP), prev.max))
        };
        rects.push(a);
        rects.push(b);
    }
    rects
}

/// What a new tab runs.
#[derive(Clone)]
pub enum Launch {
    /// A shell on the SSH host of the remote-screen session.
    Agent,
    /// A program here; empty means the login shell.
    Local(Vec<String>),
}

/// An entry of the new-tab menu.
#[derive(Clone)]
pub struct Profile {
    pub name: String,
    pub icon: &'static str,
    pub launch: Launch,
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
    profiles: Vec<Profile>,
    /// Where the window's position is remembered and what its title says.
    viewport: &'static str,
    title: String,
    copy_on_select: bool,
    renaming: Option<(usize, String)>,
    /// Tabs and panes to open on the next frame (the window's context is needed to wake it for output).
    pending: Vec<Pending>,
    /// The window closes with its last tab instead of opening a fresh shell.
    standalone: bool,
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
                NamedColor::Foreground | NamedColor::Cursor => scheme_fg(),
                NamedColor::Background => scheme_bg(),
                NamedColor::BrightForeground => Color32::WHITE,
                NamedColor::DimForeground => scheme_fg().gamma_multiply(0.66),
                n if (n as usize) < 16 => pal(n as usize),
                // DimBlack.. DimWhite follow the eight base colours.
                n => pal((n as usize).saturating_sub(NamedColor::DimBlack as usize) % 8).gamma_multiply(0.66),
            }
        }
    }
}

/// The xterm 256-colour palette.
fn indexed(i: u8) -> Color32 {
    match i {
        0..16 => pal(i as usize),
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
    /// The terminal of a remote-screen session: shells on its SSH host.
    pub fn new(system: Option<Vec<String>>) -> Self {
        let mut profiles = vec![Profile { name: "Shell on the SSH host".into(), icon: ic::MONITOR, launch: Launch::Agent }];
        profiles.extend(local_profiles());
        let mut w = Self::with_profiles(profiles, "sshremote-terminal", "Terminal");
        w.system = system;
        w
    }

    /// The stand-alone terminal: local shells and ssh sessions, the first profile opening first.
    pub fn standalone(first: Profile, mut more: Vec<Profile>) -> Self {
        let mut profiles = local_profiles();
        profiles.append(&mut more);
        let mut w = Self::with_profiles(profiles, "sshremote-standalone-terminal", "SSH Remote Terminal");
        w.pending.push((first, None));
        w.standalone = true;
        w
    }

    fn with_profiles(profiles: Vec<Profile>, viewport: &'static str, title: &str) -> Self {
        let setting = |k: &str| crate::hosts::setting(k);
        if let Some(i) = setting("term_scheme").and_then(|v| v.parse().ok()) {
            SCHEME.store(i, std::sync::atomic::Ordering::Relaxed);
        }
        TermWindow {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            font_size: setting("term_font").and_then(|v| v.parse().ok()).unwrap_or(14.0),
            system: None,
            profiles,
            viewport,
            title: title.into(),
            copy_on_select: setting("term_copy_select").is_some_and(|v| v == "1"),
            renaming: None,
            pending: Vec::new(),
            standalone: false,
        }
    }

    /// Opens a tab for `profile` in this window (shown from the next frame).
    pub fn open(&mut self, profile: Profile) {
        self.pending.push((profile, None));
    }

    fn open_tab(&mut self) {
        if let Some(p) = self.profiles.first().cloned() {
            self.pending.push((p, None));
        }
    }

    /// Opens the focused pane's program again beside it (`side`) or below it, as Windows Terminal's
    /// Alt+Shift+Plus and Alt+Shift+Minus do.
    fn split(&mut self, side: bool) {
        if let Some(t) = self.tabs.get(self.active) {
            self.pending.push((t.profile.clone(), Some((t.id, side))));
        }
    }

    /// Indices of the panes of `group`, in layout order.
    fn panes(&self, group: u32) -> Vec<usize> {
        (0..self.tabs.len()).filter(|&i| self.tabs[i].group == group).collect()
    }

    /// The tabs of the strip, in order: each one's group and the pane that stands for it.
    fn groups(&self) -> Vec<(u32, usize)> {
        let mut out: Vec<(u32, usize)> = Vec::new();
        for (i, t) in self.tabs.iter().enumerate() {
            match out.iter_mut().find(|g| g.0 == t.group) {
                // The focused pane names its tab.
                Some(g) if i == self.active => g.1 = i,
                Some(_) => {}
                None => out.push((t.group, i)),
            }
        }
        out
    }

    /// Ends one pane; the next pane of its tab takes over its place in the layout.
    fn close_pane(&mut self, i: usize, out: &mut Vec<Cmd>) {
        let t = self.tabs.remove(i);
        if t.exited.is_none() {
            out.push(Cmd::Send(ToAgent::TermClose { id: t.id }));
        }
        if t.dir.is_none()
            && let Some(next) = self.tabs.iter_mut().skip(i).find(|n| n.group == t.group)
        {
            next.dir = None;
        }
        if self.tabs.is_empty() {
            out.push(Cmd::Close);
            self.active = 0;
            return;
        }
        if self.active > i {
            self.active -= 1;
        } else if self.active == i {
            // Stay in the same tab when it has panes left.
            let same = self.panes(t.group);
            self.active = same.iter().copied().find(|&k| k >= i).or(same.last().copied()).unwrap_or(i.min(self.tabs.len() - 1));
        }
    }

    /// Ends every pane of a tab.
    fn close_group(&mut self, group: u32, out: &mut Vec<Cmd>) {
        while let Some(&i) = self.panes(group).last() {
            self.close_pane(i, out);
        }
    }

    fn start(&mut self, ctx: &egui::Context, profile: Profile, split: Option<(u32, bool)>, out: &mut Vec<Cmd>) {
        let (tx, rx) = mpsc::channel();
        let size = (100u16, 30u16);
        let term = Term::new(Config::default(), &Size { cols: size.0 as usize, rows: size.1 as usize }, Listener(tx));
        let id = self.next_id;
        self.next_id += 1;
        let (backend, exited) = match &profile.launch {
            Launch::Agent => {
                out.push(Cmd::Send(ToAgent::TermOpen { id, cols: size.0, rows: size.1 }));
                (Backend::Agent, None)
            }
            Launch::Local(argv) => {
                let waker = ctx.clone();
                match crate::pty::Local::spawn(argv, size.0, size.1, Box::new(move || waker.request_repaint())) {
                    Ok(l) => (Backend::Local(l), None),
                    Err(e) => (Backend::Agent, Some(format!("cannot start {}: {e}", profile.name))),
                }
            }
        };
        let tab = Tab {
            id,
            term,
            parser: Processor::new(),
            events: rx,
            title: profile.name.clone(),
            custom: None,
            exited,
            size,
            backend,
            view: View::Terminal,
            search: None,
            preedit: String::new(),
            clean: true,
            group: id,
            dir: None,
            profile,
        };
        // Pane 0 is whichever pane is focused when this one starts.
        let at = split.and_then(|(of, side)| {
            let of = if of == 0 { self.tabs.get(self.active)?.id } else { of };
            self.tabs.iter().position(|t| t.id == of).map(|i| (i, side))
        });
        match at {
            Some((i, side)) => {
                let tab = Tab { group: self.tabs[i].group, dir: Some(side), ..tab };
                self.tabs.insert(i + 1, tab);
                self.active = i + 1;
            }
            None => {
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
            }
        }
    }

    /// Hands keystrokes and sizes of local tabs to their programs instead of the agent.
    fn route_local(&mut self, out: &mut Vec<Cmd>) {
        let mut keep = Vec::new();
        for c in out.drain(..) {
            if let Cmd::Send(m) = &c {
                let (id, data, size) = match m {
                    ToAgent::TermInput { id, data } => (*id, Some(data.clone()), None),
                    ToAgent::TermResize { id, cols, rows } => (*id, None, Some((*cols, *rows))),
                    _ => (0, None, None),
                };
                if let Some(Backend::Local(l)) = self.tabs.iter_mut().find(|t| t.id == id).map(|t| &mut t.backend) {
                    if let Some(d) = data {
                        l.write(&d);
                    }
                    if let Some((c, r)) = size {
                        l.resize(c, r);
                    }
                    continue;
                }
            }
            keep.push(c);
        }
        *out = keep;
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
        if !self.before(ctx, &mut out) {
            return vec![Cmd::Close];
        }
        let window_title = if host.is_empty() { self.title.clone() } else { format!("{} - {host}", self.title) };
        let mut builder = egui::ViewportBuilder::default()
            .with_title(window_title)
            .with_inner_size([900.0, 560.0])
            .with_min_inner_size([360.0, 200.0]);
        if let Some(icon) = crate::gui::app_icon() {
            builder = builder.with_icon(icon);
        }
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(self.viewport), builder, |ui, _| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                out.extend(self.close_all());
                out.push(Cmd::Close);
                return;
            }
            self.body(ui, &mut out);
        });
        self.after(ctx, &mut out);
        out
    }

    /// Starts pending tabs and takes in program output; false once a stand-alone window has no tabs left.
    fn before(&mut self, ctx: &egui::Context, out: &mut Vec<Cmd>) -> bool {
        if self.tabs.is_empty() && self.pending.is_empty() {
            if self.standalone {
                return false;
            }
            self.open_tab();
        }
        for (p, split) in std::mem::take(&mut self.pending) {
            self.start(ctx, p, split, out);
        }
        // Output of local programs.
        for t in &mut self.tabs {
            if let Backend::Local(l) = &mut t.backend {
                let (data, exit) = l.drain();
                if !data.is_empty() {
                    t.parser.advance(&mut t.term, &data);
                }
                if let Some(e) = exit {
                    t.exited = Some(e);
                }
            }
        }
        true
    }

    /// Hands the emulators' replies and local keystrokes on.
    fn after(&mut self, ctx: &egui::Context, out: &mut Vec<Cmd>) {
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
        self.route_local(out);
    }

    /// Saves the terminal preferences.
    fn save_settings(&self) {
        crate::hosts::set_setting("term_font", &format!("{}", self.font_size));
        crate::hosts::set_setting("term_scheme", &SCHEME.load(std::sync::atomic::Ordering::Relaxed).to_string());
        crate::hosts::set_setting("term_copy_select", if self.copy_on_select { "1" } else { "0" });
    }

    fn body(&mut self, ui: &mut egui::Ui, out: &mut Vec<Cmd>) {
        // A tab strip as in Windows Terminal or a browser: the active tab joins the terminal below.
        let strip =
            egui::Frame::new().fill(TAB_STRIP).inner_margin(egui::Margin { left: 8, right: 6, top: 6, bottom: 0 });
        egui::Panel::top("term-tabs").frame(strip).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
            ui.horizontal(|ui| {
                let mut close = None;
                let groups = self.groups();
                let current = self.tabs.get(self.active).map(|t| t.group);
                let n = groups.len().max(1) as f32;
                let room = (ui.available_width() - 150.0).max(120.0);
                let tab_w = (room / n).clamp(110.0, 220.0);
                for (gi, &(group, i)) in groups.iter().enumerate() {
                    let active = Some(group) == current;
                    let next_active = groups.get(gi + 1).is_some_and(|g| Some(g.0) == current);
                    let (rect, r) = ui.allocate_exact_size(Vec2::new(tab_w, 34.0), Sense::click());
                    let hovered = r.hovered();
                    let p = ui.painter();
                    let top_round = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
                    if active {
                        p.rect_filled(rect, top_round, scheme_bg());
                    } else if hovered {
                        p.rect_filled(rect.shrink2(Vec2::new(2.0, 3.0)), CornerRadius::same(6), TAB_HOVER);
                    } else if !next_active && gi + 1 < groups.len() {
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
                    let mut title = t.custom.clone().unwrap_or_else(|| t.title.clone());
                    if ended {
                        title.push_str(" (ended)");
                    }
                    let count = self.panes(group).len();
                    if count > 1 {
                        title = format!("{title}  ({count} panes)");
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
                            scheme_bg()
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
                            close = Some(group);
                        }
                        cr.on_hover_text("Close (middle click on the tab also closes)");
                    }
                    if r.clicked() && close.is_none() {
                        self.active = i;
                    }
                    if r.middle_clicked() {
                        close = Some(group);
                    }
                    if r.double_clicked() {
                        let name = self.tabs[i].custom.clone().unwrap_or_else(|| self.tabs[i].title.clone());
                        self.renaming = Some((i, name));
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
                if pr.on_hover_text("New tab (Ctrl+Shift+T)").clicked() {
                    self.open_tab();
                }
                // The profiles: local shells, ssh hosts, the SSH host's own shell.
                let menu_btn = egui::Button::new(RichText::new(ic::CARET_DOWN).size(12.0).color(Color32::from_gray(200)))
                    .frame_when_inactive(false)
                    .min_size(Vec2::new(22.0, 30.0));
                let profiles = self.profiles.clone();
                egui::containers::menu::MenuButton::from_button(menu_btn).ui(ui, |ui| {
                    ui.set_min_width(240.0);
                    for (k, p) in profiles.iter().enumerate() {
                        let shortcut = if k < 9 { format!("Ctrl+Shift+{}", k + 1) } else { String::new() };
                        if ui.add(egui::Button::new(format!("{}  {}", p.icon, p.name)).shortcut_text(shortcut)).clicked() {
                            self.pending.push((p.clone(), None));
                            ui.close();
                        }
                    }
                });
                if let Some(group) = close {
                    self.close_group(group, out);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let small = |ui: &mut egui::Ui, glyph: &str, tip: &str, on: bool| {
                        let color = if on { Color32::WHITE } else { Color32::from_gray(170) };
                        let b = egui::Button::new(RichText::new(glyph).size(14.0).color(color))
                            .frame_when_inactive(on)
                            .selected(on)
                            .min_size(Vec2::splat(28.0));
                        ui.add(b).on_hover_text(tip).clicked()
                    };
                    if let Some(cmd) = &self.system
                        && small(ui, ic::ARROW_SQUARE_OUT, "Open this host in the system terminal (ssh)", false)
                    {
                        let _ = spawn_system(cmd);
                    }
                    let font = &mut self.font_size;
                    let mut copy_on_select = self.copy_on_select;
                    let mut changed = false;
                    let gear = egui::Button::new(RichText::new(ic::GEAR).size(14.0).color(Color32::from_gray(170)))
                        .frame_when_inactive(false)
                        .min_size(Vec2::splat(28.0));
                    egui::containers::menu::MenuButton::from_button(gear)
                        .config(egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
                        .ui(ui, |ui| {
                            ui.set_min_width(240.0);
                            ui.label(RichText::new("Appearance").strong());
                            changed |= ui.add(egui::Slider::new(font, 8.0..=32.0).step_by(1.0).text("Text size")).changed();
                            let current = SCHEME.load(std::sync::atomic::Ordering::Relaxed) % SCHEMES.len();
                            for (k, sc) in SCHEMES.iter().enumerate() {
                                if ui.selectable_label(k == current, sc.0).clicked() {
                                    SCHEME.store(k, std::sync::atomic::Ordering::Relaxed);
                                    changed = true;
                                }
                            }
                            ui.separator();
                            changed |= ui.checkbox(&mut copy_on_select, "Copy on select").changed();
                            ui.separator();
                            ui.label(RichText::new("Shortcuts").strong());
                            for (k, v) in [
                                ("Ctrl+Shift+T", "New tab"),
                                ("Ctrl+Shift+W", "Close pane or tab"),
                                ("Ctrl+Tab", "Next tab"),
                                ("Alt+Shift+= / -", "Split right / down"),
                                ("Alt+Shift+D", "Split along the longer side"),
                                ("Alt+arrows", "Move between panes"),
                                ("Ctrl+Shift+F", "Find"),
                                ("Ctrl+Shift+C / V", "Copy / paste"),
                                ("Ctrl+= / Ctrl+-", "Text size"),
                                ("Ctrl+click", "Open a link"),
                                ("Double-click a tab", "Rename it"),
                            ] {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(k).monospace().size(12.0));
                                    ui.label(RichText::new(v).weak().size(12.0));
                                });
                            }
                        });
                    self.copy_on_select = copy_on_select;
                    if changed {
                        self.save_settings();
                    }
                    ui.add_space(6.0);
                    // How the active tab is shown.
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        for (v, glyph, tip) in [
                            (View::Markdown, ic::ARTICLE, "Read the output as Markdown, with copy buttons on code (agent replies)"),
                            (View::Text, ic::TEXT_T, "Plain text of the whole buffer, to select and copy"),
                            (View::Terminal, ic::TERMINAL, "Terminal"),
                        ] {
                            if small(ui, glyph, tip, t.view == v) {
                                t.view = v;
                            }
                        }
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
                i.modifiers.ctrl && !i.modifiers.alt && (i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus)),
                i.modifiers.ctrl && !i.modifiers.alt && i.key_pressed(Key::Minus),
            )
        });
        let (split, step) = ui.ctx().input(|i| {
            let alt_shift = i.modifiers.alt && i.modifiers.shift && !i.modifiers.ctrl;
            let split = if alt_shift && (i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus)) {
                Some(Some(true))
            } else if alt_shift && i.key_pressed(Key::Minus) {
                Some(Some(false))
            } else if alt_shift && i.key_pressed(Key::D) {
                Some(None)
            } else {
                None
            };
            let alt = i.modifiers.alt && !i.modifiers.shift && !i.modifiers.ctrl;
            let step = if alt && (i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp)) {
                -1
            } else if alt && (i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown)) {
                1
            } else {
                0
            };
            (split, step)
        });
        if new_tab {
            self.open_tab();
        }
        if close_tab && !self.tabs.is_empty() {
            self.close_pane(self.active, out);
        }
        if next && !self.tabs.is_empty() {
            let groups = self.groups();
            let current = self.tabs[self.active].group;
            let at = groups.iter().position(|g| g.0 == current).unwrap_or(0);
            self.active = groups[(at + 1) % groups.len()].1;
        }
        if let Some(side) = split {
            // Alt+Shift+D splits along the longer side of the area.
            let wide = ui.available_rect_before_wrap().aspect_ratio() > 1.6;
            self.split(side.unwrap_or(wide));
        }
        if step != 0 && let Some(t) = self.tabs.get(self.active) {
            let panes = self.panes(t.group);
            if let Some(k) = panes.iter().position(|&i| i == self.active) {
                self.active = panes[(k as isize + step).rem_euclid(panes.len() as isize) as usize];
            }
        }
        if bigger {
            self.font_size = (self.font_size + 1.0).min(32.0);
        }
        if smaller {
            self.font_size = (self.font_size - 1.0).max(8.0);
        }
        let frame = egui::Frame::new().fill(scheme_bg()).inner_margin(6.0);
        let copy_on_select = self.copy_on_select;
        // A dialog or an open menu takes the keys; the terminal gets them back when it closes.
        let keys = self.renaming.is_none() && !ui.ctx().any_popup_open();
        let mut asks: Vec<(usize, PaneAsk)> = Vec::new();
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            let font_size = self.font_size;
            let Some(group) = self.tabs.get(self.active).map(|t| t.group) else { return };
            let panes = self.panes(group);
            let dirs: Vec<Option<bool>> = panes.iter().map(|&i| self.tabs[i].dir).collect();
            let rects = pane_rects(ui.available_rect_before_wrap(), &dirs);
            let several = panes.len() > 1;
            for (&i, &r) in panes.iter().zip(&rects) {
                let focused = i == self.active;
                let keys = keys && focused;
                let t = &mut self.tabs[i];
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt(("pane", t.id)));
                let ask = match t.view {
                    View::Terminal => screen(&mut child, t, font_size, copy_on_select, focused, keys, out),
                    View::Text | View::Markdown => {
                        reader(&mut child, t);
                        PaneAsk { focus: child.response().contains_pointer() && child.input(|i| i.pointer.any_pressed()), ..Default::default() }
                    }
                };
                if several {
                    // The focused pane is outlined, as in Windows Terminal.
                    let color = if focused { ACCENT } else { Color32::from_white_alpha(18) };
                    ui.painter().rect_stroke(r.expand(1.5), CornerRadius::same(3), egui::Stroke::new(1.0, color), egui::StrokeKind::Outside);
                }
                asks.push((i, ask));
            }
        });
        let mut close = None;
        for (i, ask) in asks {
            if ask.focus {
                self.active = i;
            }
            if let Some(side) = ask.split {
                self.active = i;
                self.split(side);
            }
            if ask.close {
                close = Some(i);
            }
        }
        if let Some(i) = close {
            self.close_pane(i, out);
        }
        if let Some((i, mut name)) = self.renaming.take() {
            let mut done = None;
            let modal = egui::Modal::new(egui::Id::unique("rename-tab")).show(ui.ctx(), |ui| {
                ui.set_width(300.0);
                ui.label(RichText::new("Rename tab").strong());
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                if !r.has_focus() {
                    r.request_focus();
                }
                if ui.input(|k| k.key_pressed(Key::Enter)) {
                    done = Some(true);
                }
                ui.horizontal(|ui| {
                    if ui.button("Rename").clicked() {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    if let Some(t) = self.tabs.get_mut(i) {
                        t.custom = (!name.trim().is_empty()).then(|| name.trim().to_string());
                    }
                }
                Some(false) => {}
                None if modal.should_close() => {}
                None => self.renaming = Some((i, name)),
            }
        }
    }
}

/// Draws a shell and passes keys, paste and the mouse wheel to it.
/// The whole buffer as text, plain or read as Markdown.
fn buffer_text(t: &Tab) -> String {
    let grid = t.term.grid();
    let history = grid.total_lines() - grid.screen_lines();
    let start = Point::new(Line(-(history as i32)), Column(0));
    let end = Point::new(Line(grid.screen_lines() as i32 - 1), Column(grid.columns().saturating_sub(1)));
    let text = t.term.bounds_to_string(start, end);
    text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim_end().to_string()
}

/// The text and Markdown views of a tab.
fn reader(ui: &mut egui::Ui, t: &mut Tab) {
    let raw = buffer_text(t);
    let markdown = t.view == View::Markdown;
    let text = if markdown && t.clean { crate::markdown::clean_agent_output(&raw) } else { raw };
    ui.horizontal(|ui| {
        if ui.button(format!("{}  Copy all", ic::COPY)).clicked() {
            ui.ctx().copy_text(text.clone());
        }
        if markdown {
            ui.checkbox(&mut t.clean, "Remove terminal frames and agent markers");
        }
        ui.label(RichText::new(format!("{} lines", text.lines().count())).weak().small());
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        ui.visuals_mut().override_text_color = Some(scheme_fg());
        if markdown {
            crate::markdown::show(ui, &text);
        } else {
            ui.add(egui::Label::new(RichText::new(&text).monospace().size(13.0)).selectable(true).extend());
        }
    });
}

/// A web address under column `col` of a line, for Ctrl+click.
fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() {
        return None;
    }
    let stop = |c: char| c.is_whitespace() || "\"'<>()[]{}`".contains(c);
    let mut a = col;
    while a > 0 && !stop(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b < chars.len() && !stop(chars[b]) {
        b += 1;
    }
    let word: String = chars[a..b].iter().collect();
    let word = word.trim_end_matches(['.', ',', ';', ':']);
    (word.starts_with("http://") || word.starts_with("https://")).then(|| word.to_string())
}

fn open_url(url: &str) {
    if cfg!(windows) {
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    } else {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

fn screen(ui: &mut egui::Ui, t: &mut Tab, font_size: f32, copy_on_select: bool, focused: bool, keys: bool, out: &mut Vec<Cmd>) -> PaneAsk {
    let mut ask = PaneAsk::default();
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
    ask.focus = resp.clicked() || resp.drag_started() || resp.secondary_clicked() || resp.middle_clicked();
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
    if resp.drag_stopped() && copy_on_select && let Some(s) = t.term.selection_to_string().filter(|s| !s.is_empty()) {
        ui.ctx().copy_text(s);
    }
    let ctrl = ui.input(|i| i.modifiers.ctrl);
    if resp.hovered() && ctrl && let Some(p) = resp.hover_pos() {
        let (pt, _) = cell_at(p);
        let line = t.term.bounds_to_string(Point::new(pt.line, Column(0)), Point::new(pt.line, Column(cols as usize - 1)));
        if url_at(&line, pt.column.0).is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (pt, side) = cell_at(p);
        t.term.selection = Some(Selection::new(SelectionType::Semantic, pt, side));
    } else if resp.clicked() {
        t.term.selection = None;
        if ctrl && let Some(p) = resp.interact_pointer_pos() {
            let (pt, _) = cell_at(p);
            let line = t.term.bounds_to_string(Point::new(pt.line, Column(0)), Point::new(pt.line, Column(cols as usize - 1)));
            if let Some(url) = url_at(&line, pt.column.0) {
                open_url(&url);
            }
        }
    }
    // Find (Ctrl+Shift+F): a bar at the top right, Enter for the next match upwards.
    if focused && ui.input(|i| i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(Key::F)) {
        t.search = Some(t.search.take().unwrap_or_default());
    }
    let mut close_search = false;
    let mut search_step: Option<alacritty_terminal::index::Direction> = None;
    if let Some(search) = t.search.as_mut().filter(|_| focused) {
        let bar = Rect::from_min_size(Pos2::new(area.right() - 330.0, area.top() + 4.0), Vec2::new(322.0, 34.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar).layer_id(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(("find", t.id)))));
        egui::Frame::new().fill(Color32::from_rgb(36, 38, 46)).corner_radius(CornerRadius::same(8)).inner_margin(4.0).show(&mut child, |ui| {
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut search.query).hint_text("Find").desired_width(170.0));
                if !edit.has_focus() && search.found.is_none() && !search.missing {
                    edit.request_focus();
                }
                if edit.changed() {
                    search.found = None;
                    search.missing = false;
                }
                let enter = ui.input(|i| i.key_pressed(Key::Enter));
                let shift = ui.input(|i| i.modifiers.shift);
                if enter {
                    search_step = Some(if shift { alacritty_terminal::index::Direction::Right } else { alacritty_terminal::index::Direction::Left });
                }
                if ui.small_button(ic::CARET_UP).on_hover_text("Previous (Enter)").clicked() {
                    search_step = Some(alacritty_terminal::index::Direction::Left);
                }
                if ui.small_button(ic::CARET_DOWN).on_hover_text("Next (Shift+Enter)").clicked() {
                    search_step = Some(alacritty_terminal::index::Direction::Right);
                }
                if search.missing {
                    ui.label(RichText::new("none").small().color(Color32::from_rgb(240, 120, 110)));
                }
                if ui.small_button(ic::X).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    close_search = true;
                }
            });
        });
    }
    if close_search {
        t.search = None;
    }
    if let (Some(dir), Some(search)) = (search_step, t.search.as_mut())
        && !search.query.is_empty()
    {
        let escaped: String = search.query.chars().map(|c| if "\\.+*?()|[]{}^$#&-~".contains(c) { format!("\\{c}") } else { c.to_string() }).collect();
        if let Ok(mut re) = alacritty_terminal::term::search::RegexSearch::new(&escaped) {
            let origin = match (&search.found, dir) {
                (Some(m), alacritty_terminal::index::Direction::Left) => *m.start(),
                (Some(m), _) => *m.end(),
                (None, _) => Point::new(Line(t.term.grid().screen_lines() as i32 - 1), Column(cols as usize - 1)),
            };
            let origin = if search.found.is_some() {
                match dir {
                    alacritty_terminal::index::Direction::Left => origin.sub(&t.term, alacritty_terminal::index::Boundary::Grid, 1),
                    _ => origin.add(&t.term, alacritty_terminal::index::Boundary::Grid, 1),
                }
            } else {
                origin
            };
            match t.term.search_next(&mut re, origin, dir, Side::Left, None) {
                Some(m) => {
                    let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                    sel.update(*m.end(), Side::Right);
                    t.term.selection = Some(sel);
                    t.term.scroll_to_point(*m.start());
                    search.found = Some(m);
                    search.missing = false;
                }
                None => search.missing = true,
            }
        }
    }
    let mut copy_now = false;
    let mut paste_now = false;
    let mut select_all = false;
    let mut clear = false;
    let mut find = false;
    let mut view = None;
    resp.context_menu(|ui| {
        if ui.add(egui::Button::new("Copy").shortcut_text("Ctrl+Shift+C")).clicked() {
            copy_now = true;
            ui.close();
        }
        if ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+Shift+V")).clicked() {
            paste_now = true;
            ui.close();
        }
        if ui.button("Select all").clicked() {
            select_all = true;
            ui.close();
        }
        ui.separator();
        if ui.add(egui::Button::new("Find").shortcut_text("Ctrl+Shift+F")).clicked() {
            find = true;
            ui.close();
        }
        if ui.button("Read as text").clicked() {
            view = Some(View::Text);
            ui.close();
        }
        if ui.button("Read as Markdown").clicked() {
            view = Some(View::Markdown);
            ui.close();
        }
        ui.separator();
        if ui.add(egui::Button::new("Split right").shortcut_text("Alt+Shift+=")).clicked() {
            ask.split = Some(true);
            ui.close();
        }
        if ui.add(egui::Button::new("Split down").shortcut_text("Alt+Shift+-")).clicked() {
            ask.split = Some(false);
            ui.close();
        }
        if ui.add(egui::Button::new("Close pane").shortcut_text("Ctrl+Shift+W")).clicked() {
            ask.close = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Clear scrollback").clicked() {
            clear = true;
            ui.close();
        }
    });
    if select_all {
        let grid = t.term.grid();
        let history = (grid.total_lines() - grid.screen_lines()) as i32;
        let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(-history), Column(0)), Side::Left);
        sel.update(Point::new(Line(grid.screen_lines() as i32 - 1), Column(grid.columns() - 1)), Side::Right);
        t.term.selection = Some(sel);
    }
    if find {
        t.search = Some(Search::default());
    }
    if let Some(v) = view {
        t.view = v;
    }
    if clear {
        // Erase saved lines, as Windows Terminal's "Clear buffer" does for the scrollback.
        t.parser.advance(&mut t.term, b"\x1b[3J");
    }
    if copy_now && let Some(s) = t.term.selection_to_string() {
        ui.ctx().copy_text(s);
    }
    if paste_now {
        // The paste arrives as an Event::Paste on the next frame.
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }
    let mode = *t.term.mode();
    let keys = keys && t.search.is_none();
    if keys {
        // Hold the keyboard focus so Tab, the arrows and Escape reach the shell instead of moving
        // between the window's buttons, where Enter or Space would then press one.
        let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
        ui.memory_mut(|m| {
            m.request_focus(resp.id);
            m.set_focus_lock_filter(resp.id, filter);
        });
        // An input method composes at the terminal's cursor, as in a text field.
        let c = t.term.grid().cursor.point;
        let at = Pos2::new(origin.x + c.column.0 as f32 * cw, origin.y + (c.line.0 + offset) as f32 * ch);
        ui.ctx().output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Terminal,
                rect: area,
                cursor_rect: Rect::from_min_size(at, Vec2::new(cw, ch)),
                should_interrupt_composition: false,
            })
        });
    } else {
        t.preedit.clear();
    }
    let mut send: Vec<u8> = Vec::new();
    let events = ui.ctx().input(|i| i.events.clone());
    for e in events {
        // Only the focused pane types; the wheel scrolls the pane under the pointer.
        if !keys && !matches!(e, Event::MouseWheel { .. }) {
            continue;
        }
        match e {
            Event::Text(s) => send.extend_from_slice(s.as_bytes()),
            Event::Ime(egui::ImeEvent::Preedit { text, .. }) => t.preedit = text,
            Event::Ime(egui::ImeEvent::Commit(s)) => {
                t.preedit.clear();
                send.extend_from_slice(s.as_bytes());
            }
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
                let alt_shift = modifiers.alt && modifiers.shift && !modifiers.ctrl;
                let alt = modifiers.alt && !modifiers.shift && !modifiers.ctrl;
                if (cs && matches!(key, Key::T | Key::W))
                    || (modifiers.ctrl && matches!(key, Key::Tab | Key::Equals | Key::Plus | Key::Minus))
                    || (alt_shift && matches!(key, Key::Equals | Key::Plus | Key::Minus | Key::D))
                    || (alt && matches!(key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown))
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
    let mut run_color = scheme_fg();
    let flush = |run: &mut String, at: Pos2, color: Color32| {
        if !run.trim().is_empty() {
            painter.text(at, Align2::LEFT_TOP, run.as_str(), font.clone(), color);
        }
        run.clear();
    };
    let mut last_row = i32::MIN;
    let mut hollow = None;
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
            fg = pal(n as usize + 8);
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
        let cell_rect = Rect::from_min_size(pos, Vec2::new(width, ch));
        if at_cursor && focused {
            (fg, bg) = (scheme_bg(), scheme_fg());
        } else if at_cursor {
            // A pane without the focus shows a hollow cursor.
            hollow = Some(cell_rect);
        }
        if bg != scheme_bg() {
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
    if !t.preedit.is_empty() && show_cursor {
        // The composition in progress, underlined on the cursor's cells.
        let at = Pos2::new(origin.x + cursor.column.0 as f32 * cw, origin.y + (cursor.line.0 + offset) as f32 * ch);
        let galley = painter.layout_no_wrap(t.preedit.clone(), font.clone(), scheme_fg());
        let r = Rect::from_min_size(at, Vec2::new(galley.size().x.max(cw), ch));
        painter.rect_filled(r, CornerRadius::ZERO, scheme_bg());
        painter.galley(at, galley, scheme_fg());
        painter.hline(r.x_range(), r.bottom() - 1.0, egui::Stroke::new(1.5, ACCENT));
    }
    if let Some(r) = hollow {
        painter.rect_stroke(r.shrink(0.5), CornerRadius::ZERO, egui::Stroke::new(1.0, scheme_fg()), egui::StrokeKind::Inside);
    }
    if let Some(text) = &t.exited {
        let r = Rect::from_min_size(Pos2::new(area.left(), area.bottom() - 28.0), Vec2::new(area.width(), 28.0));
        painter.rect_filled(r, CornerRadius::same(6), Color32::from_rgba_unmultiplied(40, 42, 50, 235));
        painter.text(
            r.left_center() + Vec2::new(10.0, 0.0),
            Align2::LEFT_CENTER,
            format!("{text}. Close the tab or open a new one."),
            FontId::proportional(13.0),
            scheme_fg(),
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    ask
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

/// The shells of this computer, as in Windows Terminal's new-tab menu.
pub fn local_profiles() -> Vec<Profile> {
    let mut out = Vec::new();
    let mut add = |name: &str, argv: Vec<&str>| {
        out.push(Profile {
            name: name.into(),
            icon: ic::TERMINAL,
            launch: Launch::Local(argv.into_iter().map(str::to_string).collect()),
        })
    };
    if cfg!(windows) {
        add("PowerShell", vec!["powershell.exe", "-NoLogo"]);
        let found = |p: &str| std::path::Path::new(p).exists();
        if found(r"C:\Program Files\PowerShell\7\pwsh.exe") {
            add("PowerShell 7", vec![r"C:\Program Files\PowerShell\7\pwsh.exe", "-NoLogo"]);
        }
        add("Command Prompt", vec!["cmd.exe"]);
        if found(r"C:\Program Files\Git\bin\bash.exe") {
            add("Git Bash", vec![r"C:\Program Files\Git\bin\bash.exe", "--login", "-i"]);
        }
        if found(r"C:\Windows\System32\wsl.exe") {
            add("WSL", vec!["wsl.exe"]);
        }
    } else {
        add("Shell", Vec::new());
        for (name, path) in [("bash", "/bin/bash"), ("zsh", "/usr/bin/zsh"), ("fish", "/usr/bin/fish")] {
            if std::path::Path::new(path).exists() {
                add(name, vec![path, "-l"]);
            }
        }
    }
    out
}

/// The terminal as a program of its own (`sshremote terminal [HOST]`).
pub fn run_window(first: Profile, panes: Vec<(Profile, bool)>) -> Result<(), String> {
    struct Solo(TermWindow);
    impl eframe::App for Solo {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            let mut out = Vec::new();
            if !self.0.before(&ctx, &mut out) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.0.body(ui, &mut out);
            self.0.after(&ctx, &mut out);
        }
    }
    let native = crate::gui::native_options("SSH Remote Terminal", [960.0, 600.0]);
    eframe::run_native(
        "sshremote-terminal",
        native,
        Box::new(move |cc| {
            crate::gui::style(&cc.egui_ctx);
            let mut w = TermWindow::standalone(first, Vec::new());
            w.pending.extend(panes.into_iter().map(|(p, side)| (p, Some((0, side)))));
            Ok(Box::new(Solo(w)))
        }),
    )
    .map_err(|e| e.to_string())
}

/// An ssh session in a local terminal (password and host key prompts appear in it).
pub fn ssh_profile(host: &str, port: &str) -> Profile {
    let mut argv = vec!["ssh".to_string()];
    if !port.trim().is_empty() {
        argv.extend(["-p".into(), port.trim().to_string()]);
    }
    argv.push(host.to_string());
    Profile { name: host.to_string(), icon: ic::GLOBE, launch: Launch::Local(argv) }
}

/// Runs `cmd` in a new window of the system's terminal.
pub(crate) fn spawn_system(cmd: &[String]) -> std::io::Result<()> {
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
