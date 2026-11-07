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
    Scheme(
        "One Half Dark",
        [22, 23, 28],
        [215, 218, 224],
        [
            [59, 63, 74],
            [224, 108, 117],
            [152, 195, 121],
            [229, 192, 123],
            [97, 175, 239],
            [198, 120, 221],
            [86, 182, 194],
            [215, 218, 224],
            [92, 99, 112],
            [239, 123, 132],
            [169, 212, 138],
            [240, 207, 141],
            [124, 192, 245],
            [212, 143, 234],
            [111, 203, 214],
            [255, 255, 255],
        ],
    ),
    Scheme(
        "Campbell",
        [12, 12, 12],
        [204, 204, 204],
        [
            [12, 12, 12],
            [197, 15, 31],
            [19, 161, 14],
            [193, 156, 0],
            [0, 55, 218],
            [136, 23, 152],
            [58, 150, 221],
            [204, 204, 204],
            [118, 118, 118],
            [231, 72, 86],
            [22, 198, 12],
            [249, 241, 165],
            [59, 120, 255],
            [180, 0, 158],
            [97, 214, 214],
            [242, 242, 242],
        ],
    ),
    Scheme(
        "Dracula",
        [40, 42, 54],
        [248, 248, 242],
        [
            [33, 34, 44],
            [255, 85, 85],
            [80, 250, 123],
            [241, 250, 140],
            [189, 147, 249],
            [255, 121, 198],
            [139, 233, 253],
            [248, 248, 242],
            [98, 114, 164],
            [255, 110, 110],
            [105, 255, 148],
            [255, 255, 165],
            [214, 172, 255],
            [255, 146, 223],
            [164, 255, 255],
            [255, 255, 255],
        ],
    ),
    Scheme(
        "Solarized Dark",
        [0, 43, 54],
        [131, 148, 150],
        [
            [7, 54, 66],
            [220, 50, 47],
            [133, 153, 0],
            [181, 137, 0],
            [38, 139, 210],
            [211, 54, 130],
            [42, 161, 152],
            [238, 232, 213],
            [0, 43, 54],
            [203, 75, 22],
            [88, 110, 117],
            [101, 123, 131],
            [131, 148, 150],
            [108, 113, 196],
            [147, 161, 161],
            [253, 246, 227],
        ],
    ),
    Scheme(
        "Tango Dark",
        [0, 0, 0],
        [211, 215, 207],
        [
            [0, 0, 0],
            [204, 0, 0],
            [78, 154, 6],
            [196, 160, 0],
            [52, 101, 164],
            [117, 80, 123],
            [6, 152, 154],
            [211, 215, 207],
            [85, 87, 83],
            [239, 41, 41],
            [138, 226, 52],
            [252, 233, 79],
            [114, 159, 207],
            [173, 127, 168],
            [52, 226, 226],
            [238, 238, 236],
        ],
    ),
    Scheme(
        "One Half Light",
        [250, 250, 250],
        [56, 58, 66],
        [
            [56, 58, 66],
            [228, 86, 73],
            [80, 161, 79],
            [193, 132, 1],
            [1, 132, 188],
            [166, 38, 164],
            [9, 151, 179],
            [250, 250, 250],
            [79, 82, 93],
            [223, 108, 117],
            [152, 195, 121],
            [228, 192, 122],
            [97, 175, 239],
            [197, 119, 221],
            [86, 181, 193],
            [255, 255, 255],
        ],
    ),
];

/// The scheme in use, shared by every terminal window.
static SCHEME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The cursor's shape: a thin bar as in VS Code and Windows Terminal, a block, or an underline.
static CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const CURSORS: [&str; 3] = ["Bar", "Block", "Underline"];

/// Selected cells, in VS Code's dark selection colour.
const SELECTION: Color32 = Color32::from_rgb(38, 79, 120);

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
#[derive(Clone, Copy, PartialEq, Debug)]
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
    /// The tab of the strip this pane belongs to; panes of one tab share its area.
    group: u32,
    /// How this pane divides the area of the pane before it: Some(true) side by side, Some(false)
    /// one above the other; None for a tab's first pane.
    dir: Option<bool>,
    /// What it runs, to open the same in a new pane.
    profile: Profile,
    /// Bumped by output, so the text and Markdown views rebuild only after a change.
    rev: u64,
    /// The text view's text and the Markdown view's document, for `rev` and the session choice.
    reading: Option<Reading>,
    /// Text an input method is composing (Korean, Japanese, Chinese), drawn at the cursor.
    preedit: String,
    /// The text view's document and the cursor's place in it, for an output revision and cursor.
    editor: Option<((u64, i32, usize), std::rc::Rc<(String, usize)>)>,
    /// The mouse button a program that asked for the mouse holds down, for its drag reports.
    mouse_held: Option<u8>,
    /// A full-screen program's screens stitched into one document, as Markdown lines.
    stitched: Vec<String>,
    /// When something was last typed, in egui's time: the cursor stays lit, not blinking, after it.
    typed_at: f64,
}

/// The text and Markdown views' content, with the output revision and the first live row it ends above.
type Reading = (u64, i32, std::rc::Rc<(String, crate::markdown::Doc)>);

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
            (
                Rect::from_min_max(prev.min, Pos2::new(x - GAP, prev.max.y)),
                Rect::from_min_max(Pos2::new(x + GAP, prev.min.y), prev.max),
            )
        } else {
            let y = prev.center().y;
            (
                Rect::from_min_max(prev.min, Pos2::new(prev.max.x, y - GAP)),
                Rect::from_min_max(Pos2::new(prev.min.x, y + GAP), prev.max),
            )
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
        let mut profiles =
            vec![Profile { name: "Shell on the SSH host".into(), icon: ic::MONITOR, launch: Launch::Agent }];
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
        let c =
            crate::hosts::setting("term_cursor").and_then(|v| v.parse().ok()).unwrap_or(0usize).min(CURSORS.len() - 1);
        CURSOR.store(c, std::sync::atomic::Ordering::Relaxed);
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
            self.active =
                same.iter().copied().find(|&k| k >= i).or(same.last().copied()).unwrap_or(i.min(self.tabs.len() - 1));
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
            typed_at: 0.0,
            stitched: Vec::new(),
            mouse_held: None,
            editor: None,
            rev: 0,
            reading: None,
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
            t.rev += 1;
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

    /// The terminal drawn inside another window's area, as the remote screen window does.
    pub fn embedded(&mut self, ui: &mut egui::Ui) -> Vec<Cmd> {
        let mut out = Vec::new();
        let ctx = ui.ctx().clone();
        self.before(&ctx, &mut out);
        self.body(ui, &mut out);
        self.after(&ctx, &mut out);
        out
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
                    t.rev += 1;
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
        crate::hosts::set_setting("term_cursor", &CURSOR.load(std::sync::atomic::Ordering::Relaxed).to_string());
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
                let menu_btn =
                    egui::Button::new(RichText::new(ic::CARET_DOWN).size(12.0).color(Color32::from_gray(200)))
                        .frame_when_inactive(false)
                        .min_size(Vec2::new(22.0, 30.0));
                let profiles = self.profiles.clone();
                egui::containers::menu::MenuButton::from_button(menu_btn).ui(ui, |ui| {
                    ui.set_min_width(240.0);
                    for (k, p) in profiles.iter().enumerate() {
                        let shortcut = if k < 9 { format!("Ctrl+Shift+{}", k + 1) } else { String::new() };
                        if ui
                            .add(egui::Button::new(format!("{}  {}", p.icon, p.name)).shortcut_text(shortcut))
                            .clicked()
                        {
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
                        .config(
                            egui::containers::menu::MenuConfig::new()
                                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                        )
                        .ui(ui, |ui| {
                            ui.set_min_width(240.0);
                            ui.label(RichText::new("Appearance").strong());
                            changed |=
                                ui.add(egui::Slider::new(font, 8.0..=32.0).step_by(1.0).text("Text size")).changed();
                            let current = SCHEME.load(std::sync::atomic::Ordering::Relaxed) % SCHEMES.len();
                            for (k, sc) in SCHEMES.iter().enumerate() {
                                if ui.selectable_label(k == current, sc.0).clicked() {
                                    SCHEME.store(k, std::sync::atomic::Ordering::Relaxed);
                                    changed = true;
                                }
                            }
                            ui.separator();
                            changed |= ui.checkbox(&mut copy_on_select, "Copy on select").changed();
                            ui.horizontal(|ui| {
                                ui.label("Cursor");
                                let current = CURSOR.load(std::sync::atomic::Ordering::Relaxed);
                                for (k, name) in CURSORS.iter().enumerate() {
                                    if ui.selectable_label(k == current, *name).clicked() {
                                        CURSOR.store(k, std::sync::atomic::Ordering::Relaxed);
                                        changed = true;
                                    }
                                }
                            });
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
                            (
                                View::Markdown,
                                ic::ARTICLE,
                                "Read the output as Markdown, with copy buttons on code (agent replies)",
                            ),
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
        if step != 0
            && let Some(t) = self.tabs.get(self.active)
        {
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
                        reader(&mut child, t, font_size, keys, out);
                        PaneAsk {
                            focus: child.response().contains_pointer() && child.input(|i| i.pointer.any_pressed()),
                            ..Default::default()
                        }
                    }
                };
                if several {
                    // The focused pane is outlined, as in Windows Terminal.
                    let color = if focused { ACCENT } else { Color32::from_white_alpha(18) };
                    ui.painter().rect_stroke(
                        r.expand(1.5),
                        CornerRadius::same(3),
                        egui::Stroke::new(1.0, color),
                        egui::StrokeKind::Outside,
                    );
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
/// The text and Markdown views of a tab, as one document in the manner of VS Code's editor and
/// Markdown preview: the scrollback above, and at its end the live rows where the prompt and the
/// cursor are, which take the keys as the terminal view does. Only those rows show a text cursor
/// for typing; the rest can be selected and copied.
fn reader(ui: &mut egui::Ui, t: &mut Tab, font_size: f32, keys: bool, out: &mut Vec<Cmd>) {
    if t.view == View::Text {
        let font = FontId::monospace(font_size);
        let (cw, ch) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, CornerRadius::ZERO, scheme_bg());
        return text_editor(ui, t, &font, cw, ch, keys, out);
    }
    let font = FontId::monospace(font_size);
    let (cw, ch) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let (first, last) = input_rows(t);
    if t.reading.as_ref().is_none_or(|r| r.0 != t.rev || r.1 != first) {
        let history = (t.term.grid().total_lines() - t.term.grid().screen_lines()) as i32;
        let text = if t.term.mode().contains(TermMode::ALT_SCREEN) {
            // A full-screen program shows a window on a longer text: its screens are stitched
            // together, so a reply or a table cut off by scrolling keeps its shape.
            let end = if status_row(t) && first == t.term.grid().screen_lines() as i32 { first - 1 } else { first };
            let screen = content_rows(screen_markdown(t, 0, end).lines().map(str::to_string).collect());
            let mut stitched = std::mem::take(&mut t.stitched);
            stitch(&mut stitched, &screen);
            t.stitched = stitched;
            t.stitched.join("\n")
        } else {
            t.stitched.clear();
            screen_markdown(t, -history, first)
        };
        let doc = crate::markdown::Doc::new(&text);
        t.reading = Some((t.rev, first, std::rc::Rc::new((text, doc))));
    }
    let read = t.reading.as_ref().expect("just built").2.clone();
    let doc = &read.1;
    let th = crate::markdown::Theme::terminal(scheme_bg(), scheme_fg());
    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, CornerRadius::ZERO, th.bg);
    ui.visuals_mut().override_text_color = Some(th.fg);
    let markdown = t.view == View::Markdown;
    let id = egui::Id::unique(("reader-input", t.id));
    let live_rows = (last - first + 1) as usize;
    let mut live: Option<(Rect, egui::Response)> = None;
    // The keys come to the live rows unless selected text has them; typing takes them back.
    let typing = ui.input(|i| i.events.iter().any(|e| matches!(e, Event::Text(_) | Event::Ime(_) | Event::Paste(_))));
    let elsewhere = ui.memory(|m| m.focused().is_some_and(|f| f != id));
    let wheel = if ui.rect_contains_pointer(area) { wheel_lines(ui, ch, 10.0) } else { 0 };
    let scroll =
        egui::ScrollArea::both().id_salt(("reading", t.id, markdown)).auto_shrink([false, false]).stick_to_bottom(true);
    let shown = scroll.show(ui, |ui| {
        if markdown {
            // A readable column, as in the preview; the live rows sit under it on the same edge.
            let width = ui.available_width().min(960.0);
            let pad = ((ui.available_width() - width) / 2.0).max(0.0) + 20.0;
            egui::Frame::new().inner_margin(egui::Margin { left: pad as i8, right: 20, top: 14, bottom: 6 }).show(
                ui,
                |ui| {
                    ui.set_max_width(width - 40.0);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    crate::markdown::show(ui, doc, egui::Id::unique(("md-doc", t.id)));
                    live = Some(live_area(ui, id, cw, ch, live_rows));
                },
            );
        }
    });
    // At the top of the document the wheel asks the program to scroll back, and what it shows is
    // stitched on above; at the bottom it scrolls the program forward.
    let at_top = shown.state.clamped_offset().y <= 1.0;
    let at_bottom = shown.state.clamped_offset().y + shown.inner_rect.height() >= shown.content_size.y - 1.0;
    if program_scrolls(t) && t.exited.is_none() && ((wheel > 0 && at_top) || (wheel < 0 && at_bottom)) {
        let grid = t.term.grid();
        let send = wheel_bytes(*t.term.mode(), wheel, grid.columns() / 2, grid.screen_lines() / 2);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    }
    let Some((rect, resp)) = live else { return };
    let rect = rect.intersect(shown.inner_rect);
    let grab = keys && (resp.clicked() || typing || !elsewhere);
    let c = t.term.grid().cursor.point;
    let full = resp.rect;
    let at = Pos2::new(full.left() + c.column.0 as f32 * cw, full.top() + (c.line.0 - first) as f32 * ch);
    if keys {
        keyboard(ui, t, id, rect, Rect::from_min_size(at, Vec2::new(cw, ch)), grab, out);
    }
    let focused = ui.memory(|m| m.has_focus(id));
    let mut clip = ui.new_child(egui::UiBuilder::new().max_rect(shown.inner_rect));
    clip.set_clip_rect(shown.inner_rect);
    paint(&clip, t, full, &font, cw, ch, first, focused);
    // Only where typing goes shows a text cursor; elsewhere the pointer stays an arrow.
    if let Some(p) = ui.ctx().pointer_hover_pos().filter(|p| shown.inner_rect.contains(*p)) {
        let icon = if rect.contains(p) { egui::CursorIcon::Text } else { egui::CursorIcon::Default };
        ui.ctx().set_cursor_icon(icon);
    }
}

/// Space for the live rows at the end of a reading view.
fn live_area(ui: &mut egui::Ui, id: egui::Id, cw: f32, ch: f32, rows: usize) -> (Rect, egui::Response) {
    let width = ui.available_width().max(cw * 20.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, rows as f32 * ch), Sense::hover());
    let resp = ui.interact(rect, id, Sense::click());
    (rect, resp)
}

/// Keys, text, input-method compositions and pastes for the shell of `t`, typed into the widget
/// `id`, whose input method composes at `cursor`. `grab` takes the keyboard focus for it.
fn keyboard(ui: &egui::Ui, t: &mut Tab, id: egui::Id, area: Rect, cursor: Rect, grab: bool, out: &mut Vec<Cmd>) {
    keyboard_with(ui, t, id, area, cursor, grab, KeySkip::default(), out);
}

/// Keys the text view keeps for its own selection instead of sending them to the shell.
#[derive(Default, Clone, Copy)]
struct KeySkip {
    /// Ctrl+A selects the whole text.
    select_all: bool,
    /// Ctrl+C copies the selected text.
    copy: bool,
}

#[allow(clippy::too_many_arguments)]
fn keyboard_with(
    ui: &egui::Ui,
    t: &mut Tab,
    id: egui::Id,
    area: Rect,
    cursor: Rect,
    grab: bool,
    skip: KeySkip,
    out: &mut Vec<Cmd>,
) {
    // Holding the focus keeps Tab, the arrows and Escape for the shell instead of moving between the
    // window's buttons, where Enter or Space would then press one. It is asked for only when it is
    // elsewhere: each request also ends an input method's composition.
    let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
    let focused = ui.memory_mut(|m| {
        if grab && !m.has_focus(id) {
            m.request_focus(id);
        }
        if m.has_focus(id) {
            m.set_focus_lock_filter(id, filter);
        }
        m.has_focus(id)
    });
    if !focused {
        t.preedit.clear();
        return;
    }
    // An input method composes at the terminal's cursor, as in a text field.
    ui.ctx().output_mut(|o| {
        o.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Terminal,
            rect: area,
            cursor_rect: cursor,
            should_interrupt_composition: false,
        })
    });
    let mode = *t.term.mode();
    let mut send: Vec<u8> = Vec::new();
    for e in ui.ctx().input(|i| i.events.clone()) {
        match e {
            Event::Text(s) => send.extend_from_slice(s.as_bytes()),
            Event::Ime(egui::ImeEvent::Preedit { text, .. }) => t.preedit = text,
            Event::Ime(egui::ImeEvent::Commit(s)) => {
                t.preedit.clear();
                send.extend_from_slice(s.as_bytes());
            }
            Event::Copy if skip.copy => {}
            Event::Key { key: Key::A, modifiers, .. } if skip.select_all && modifiers.command => {}
            Event::Copy => match t.term.selection_to_string() {
                Some(s) if !s.is_empty() => {
                    ui.ctx().copy_text(s);
                    t.term.selection = None;
                }
                _ => send.push(0x03),
            },
            Event::Cut => send.push(0x18),
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
                if (cs && matches!(key, Key::T | Key::W | Key::F))
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
        t.typed_at = ui.input(|i| i.time);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    } else if !t.preedit.is_empty() {
        t.typed_at = ui.input(|i| i.time);
    }
}

/// Paints the terminal's rows from `first` on into `area`: backgrounds, then text in runs of the
/// same colour, the cursor (hollow without the focus) and an input method's composition.
#[allow(clippy::too_many_arguments)]
fn paint(ui: &egui::Ui, t: &Tab, area: Rect, font: &FontId, cw: f32, ch: f32, first: i32, focused: bool) {
    let origin = area.min;
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
    let mut caret_cell: Option<Rect> = None;
    let shape = CURSOR.load(std::sync::atomic::Ordering::Relaxed);
    let block = shape == 1;
    // Blinking as in VS Code: on and off every half second, and steady for a moment after typing.
    let now = ui.input(|i| i.time);
    let since = now - t.typed_at;
    let lit = !focused || since < 0.6 || ((since - 0.6) / 0.53) as u64 % 2 == 1;
    if focused && show_cursor {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(0.53 - (since - 0.6).rem_euclid(0.53)));
    }
    for cell in content.display_iter {
        let row = cell.point.line.0 + offset;
        if row < first {
            continue;
        }
        let col = cell.point.column.0;
        if row != last_row {
            flush(&mut run, run_start, run_color);
            last_row = row;
        }
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let pos = Pos2::new(origin.x + col as f32 * cw, origin.y + (row - first) as f32 * ch);
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
        if sel.is_some_and(|s| s.contains(cell.point)) {
            bg = SELECTION;
        }
        let at_cursor = show_cursor && cell.point == cursor;
        let cell_rect = Rect::from_min_size(pos, Vec2::new(width, ch));
        if at_cursor {
            caret_cell = Some(cell_rect);
            if focused && block && lit && t.preedit.is_empty() {
                (fg, bg) = (scheme_bg(), scheme_fg());
            }
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
    let mut caret = caret_cell;
    if !t.preedit.is_empty() && show_cursor {
        // The composition in progress, underlined, with the caret after it as in a text field.
        let at =
            Pos2::new(origin.x + cursor.column.0 as f32 * cw, origin.y + (cursor.line.0 + offset - first) as f32 * ch);
        let galley = painter.layout_no_wrap(t.preedit.clone(), font.clone(), scheme_fg());
        let r = Rect::from_min_size(at, Vec2::new(galley.size().x.max(cw), ch));
        painter.rect_filled(r, CornerRadius::ZERO, scheme_bg());
        painter.galley(at, galley, scheme_fg());
        painter.hline(r.x_range(), r.bottom() - 1.0, egui::Stroke::new(1.5, ACCENT));
        caret = Some(Rect::from_min_size(Pos2::new(r.right(), r.top()), Vec2::new(cw, ch)));
    }
    let Some(r) = caret else { return };
    let color = if focused { scheme_fg() } else { scheme_fg().gamma_multiply(0.45) };
    if !focused && block {
        // A pane without the focus shows a hollow block.
        let stroke = egui::Stroke::new(1.0, color);
        painter.rect_stroke(r.shrink(0.5), CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
    } else if lit && !(block && t.preedit.is_empty()) {
        let bar = match shape {
            2 => Rect::from_min_max(Pos2::new(r.left(), r.bottom() - 2.0), r.max),
            1 => r,
            _ => Rect::from_min_size(r.min, Vec2::new(2.0, r.height())),
        };
        painter.rect_filled(bar, CornerRadius::ZERO, color);
    }
}

/// Rows of the live screen shown under the text and Markdown views, so what is typed can be seen:
/// a few rows above the cursor down to the last written row (an agent's input box and status).
fn input_rows(t: &Tab) -> (i32, i32) {
    let grid = t.term.grid();
    let cursor = grid.cursor.point.line.0;
    let screen = grid.screen_lines() as i32;
    let cols = grid.columns();
    let written = |row: i32| (0..cols).any(|c| !matches!(grid[Line(row)][Column(c)].c, ' ' | '\0'));
    let last = (cursor..screen).rev().find(|&r| written(r)).unwrap_or(cursor).max(cursor);
    let first = (cursor - 3).max(0).max(last - 9);
    (first, last)
}

/// The rows `from` to `to` of the scrollback and screen, one line each as shown, without trailing
/// spaces, and the character offset of the terminal cursor in their joined text.
fn screen_text(t: &Tab, from: i32, to: i32) -> (String, usize) {
    let grid = t.term.grid();
    let cols = grid.columns();
    let cursor = grid.cursor.point;
    let mut out = String::new();
    let mut at = 0;
    for line in from..=to {
        if line > from {
            out.push('\n');
        }
        let row = &grid[Line(line)];
        let mut text = String::new();
        let mut upto_cursor = 0;
        for c in 0..cols {
            let cell = &row[Column(c)];
            if line == cursor.line.0 && c == cursor.column.0 {
                upto_cursor = text.chars().count();
            }
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(if cell.c == '\0' { ' ' } else { cell.c });
        }
        if line == cursor.line.0 {
            // The cursor's row keeps its spaces up to the cursor, so the caret sits where it is.
            let keep = text.trim_end().chars().count().max(upto_cursor);
            let kept: String = text.chars().take(keep).collect();
            at = out.chars().count() + upto_cursor;
            out.push_str(&kept);
        } else {
            out.push_str(text.trim_end());
        }
    }
    (out, at)
}

/// The text view: one read-only document of the whole scrollback and screen in the terminal's font,
/// with line numbers, selected as in VS Code (drag, double-click for a word, Shift+click, Ctrl+A,
/// Ctrl+C) and a caret where the shell's cursor is, which takes typing for the shell.
fn text_editor(ui: &mut egui::Ui, t: &mut Tab, font: &FontId, cw: f32, ch: f32, keys: bool, out: &mut Vec<Cmd>) {
    let grid = t.term.grid();
    let history = (grid.total_lines() - grid.screen_lines()) as i32;
    let (_, last) = input_rows(t);
    let key = (t.rev, t.term.grid().cursor.point.line.0, t.term.grid().cursor.point.column.0);
    if t.editor.as_ref().is_none_or(|e| e.0 != key) {
        let (text, at) = screen_text(t, -history, last);
        t.editor = Some((key, std::rc::Rc::new((text, at))));
    }
    let doc = t.editor.as_ref().expect("just built").1.clone();
    let (text, at) = (&doc.0, doc.1);
    let fg = scheme_fg();
    let id = egui::Id::unique(("text-view", t.id));
    // Typing collapses a selection onto the shell's cursor, as typing would in an editor.
    let typing = ui.input(|i| {
        i.events.iter().any(|e| matches!(e, Event::Text(_) | Event::Ime(_) | Event::Paste(_)))
            || i.events.iter().any(|e| {
                matches!(e, Event::Key { pressed: true, key, modifiers, .. }
                if !modifiers.ctrl && !modifiers.command && !matches!(key, Key::A | Key::C))
            })
    });
    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    let selecting = state.cursor.char_range().is_some_and(|r| !r.is_empty());
    if typing && selecting {
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(at))));
        state.store(ui.ctx(), id);
    }
    let lines = text.lines().count().max(1);
    let gutter = cw * (lines.to_string().len() as f32 + 2.0);
    let mut caret_rect = None;
    let own_wheel = if program_scrolls(t) {
        egui::scroll_area::ScrollSource::SCROLL_BAR
    } else {
        egui::scroll_area::ScrollSource::MOUSE_WHEEL | egui::scroll_area::ScrollSource::SCROLL_BAR
    };
    forward_wheel(ui, t, ui.available_rect_before_wrap(), ch, out);
    let scroll = egui::ScrollArea::both()
        .id_salt(("text-scroll", t.id))
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .scroll_source(own_wheel);
    let shown = scroll.show(ui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            let top = ui.cursor().top();
            let (g, _) = ui.allocate_exact_size(Vec2::new(gutter, lines as f32 * ch), Sense::hover());
            let clip = ui.clip_rect();
            let from = ((clip.top() - top) / ch).floor().max(0.0) as usize;
            let to = (((clip.bottom() - top) / ch).ceil().max(0.0) as usize).min(lines);
            for n in from..to {
                let pos = Pos2::new(g.right() - cw, top + n as f32 * ch);
                ui.painter().text(pos, Align2::RIGHT_TOP, (n + 1).to_string(), font.clone(), fg.gamma_multiply(0.4));
            }
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _: f32| {
                let job = ui.ctx().fonts_mut(|f| crate::markdown::mono_job(buf.as_str(), font, fg, f));
                ui.ctx().fonts_mut(|f| f.layout_job(job))
            };
            let mut view: &str = text;
            let edit = egui::TextEdit::multiline(&mut view)
                .id(id)
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::ZERO)
                .desired_width(f32::INFINITY)
                .desired_rows(1)
                .layouter(&mut layouter)
                .show(ui);
            edit.response.context_menu(|ui| {
                if ui.button(format!("{}  Copy all", ic::COPY)).clicked() {
                    ui.ctx().copy_text(text.clone());
                    ui.close();
                }
                if ui.button("Paste").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    ui.ctx().memory_mut(|m| m.request_focus(id));
                    ui.close();
                }
            });
            let p = edit.galley.pos_from_cursor(egui::text::CCursor::new(at));
            caret_rect = Some(p.translate(edit.galley_pos.to_vec2()).with_max_x(p.min.x + edit.galley_pos.x + cw));
        });
        ui.add_space(8.0);
    });
    let Some(caret) = caret_rect else { return };
    let caret = Rect::from_min_size(caret.min, Vec2::new(cw, ch));
    let selecting = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|s| s.cursor.char_range())
        .is_some_and(|r| !r.is_empty());
    // The keys go to the shell while the text has the focus; the view takes it when nothing has.
    let grab = keys && (typing || ui.memory(|m| m.focused().is_none()));
    if keys {
        let skip = KeySkip { select_all: true, copy: selecting };
        keyboard_with(ui, t, id, shown.inner_rect, caret, grab, skip, out);
    }
    let focused = ui.memory(|m| m.has_focus(id));
    let painter = ui.painter_at(shown.inner_rect);
    if !t.preedit.is_empty() {
        let galley = painter.layout_no_wrap(t.preedit.clone(), font.clone(), fg);
        let r = Rect::from_min_size(caret.min, Vec2::new(galley.size().x.max(cw), ch));
        painter.rect_filled(r, CornerRadius::ZERO, scheme_bg());
        painter.galley(caret.min, galley, fg);
        painter.hline(r.x_range(), r.bottom() - 1.0, egui::Stroke::new(1.5, ACCENT));
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(r.right(), r.top()), Vec2::new(2.0, ch)),
            CornerRadius::ZERO,
            fg,
        );
    } else if focused && !selecting && t.exited.is_none() {
        let since = ui.input(|i| i.time) - t.typed_at;
        if since < 0.6 || ((since - 0.6) / 0.53) as u64 % 2 == 1 {
            painter.rect_filled(Rect::from_min_size(caret.min, Vec2::new(2.0, ch)), CornerRadius::ZERO, fg);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(0.53 - (since - 0.6).rem_euclid(0.53)));
    }
}

/// Markers an agent puts before a line, kept as they are ahead of the line's own formatting.
const LINE_MARKERS: [&str; 6] = ["● ", "⏺ ", "❯ ", "› ", "• ", "▎ "];

/// The rows `from` to `to` (exclusive) as Markdown, with what the terminal showed as styles written
/// back as Markdown: a whole bold line becomes a heading (level 1 when also underlined or italic,
/// as Claude Code draws `#`), bold and italic runs become `**` and `*`. Agents draw Markdown with
/// these attributes and drop the marks, so this gives the Markdown view its structure back.
fn screen_markdown(t: &Tab, from: i32, to: i32) -> String {
    let grid = t.term.grid();
    let cols = grid.columns();
    let mut lines: Vec<String> = Vec::new();
    for line in from..to {
        let row = &grid[Line(line)];
        let mut cells: Vec<(char, bool, bool, bool)> = Vec::new();
        for c in 0..cols {
            let cell = &row[Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            let f = cell.flags;
            cells.push((ch, f.contains(Flags::BOLD), f.intersects(Flags::ITALIC), f.contains(Flags::UNDERLINE)));
        }
        while cells.last().is_some_and(|c| c.0 == ' ') {
            cells.pop();
        }
        let plain: String = cells.iter().map(|c| c.0).collect();
        // Leading spaces and an agent's marker stay as they are.
        let indent = plain.len() - plain.trim_start().len();
        let mut start = plain[..indent].chars().count();
        let rest: String = plain.trim_start().to_string();
        let mut prefix = plain[..indent].to_string();
        if let Some(m) = LINE_MARKERS.iter().find(|m| rest.starts_with(**m)) {
            prefix.push_str(m);
            start += m.chars().count();
        }
        let body = &cells[start.min(cells.len())..];
        let text: Vec<&(char, bool, bool, bool)> = body.iter().filter(|c| !c.0.is_whitespace()).collect();
        let boxy = body.iter().any(|c| ('\u{2500}'..='\u{257f}').contains(&c.0));
        let words: String = body.iter().map(|c| c.0).collect();
        // Text laid out in columns stays as it is, without marks.
        if crate::markdown::columned(&words) {
            lines.push(format!("{prefix}{words}"));
            continue;
        }
        if !text.is_empty() && !boxy && text.iter().all(|c| c.1) {
            let level = if text.iter().all(|c| c.2 || c.3) { "#" } else { "##" };
            lines.push(format!("{prefix}{level} {}", words.trim()));
            continue;
        }
        let mut out = prefix;
        let (mut bold, mut italic) = (false, false);
        for (i, &(ch, b, it, _)) in body.iter().enumerate() {
            if !ch.is_whitespace() {
                // Spaces keep the style around them, so marks hug the words.
                if (b, it) != (bold, italic) {
                    let closing = out.len();
                    if italic {
                        out.push('*');
                    }
                    if bold && !b {
                        out.push_str("**");
                    }
                    // A mark that closes and one that opens are kept apart by an invisible space, so
                    // "**bold***italic*" cannot be read the other way round.
                    if out.len() > closing && (b && !bold || it) {
                        out.push('\u{200B}');
                    }
                    if b && !bold {
                        out.push_str("**");
                    }
                    if it {
                        out.push('*');
                    }
                    (bold, italic) = (b, it);
                }
                if matches!(ch, '*' | '_' | '`') && !boxy {
                    out.push('\\');
                }
            } else if bold || italic {
                // A style ends before the space when the next word is not in it.
                match body[i..].iter().find(|c| !c.0.is_whitespace()) {
                    None => break,
                    Some(next) if (next.1, next.2) != (bold, italic) => {
                        if italic {
                            out.push('*');
                        }
                        if bold {
                            out.push_str("**");
                        }
                        (bold, italic) = (false, false);
                    }
                    Some(_) => {}
                }
            }
            out.push(ch);
        }
        let trailing = out.len() - out.trim_end().len();
        out.truncate(out.len() - trailing);
        if italic {
            out.push('*');
        }
        if bold {
            out.push_str("**");
        }
        lines.push(out);
    }
    lines.join("\n").trim_end().to_string()
}

/// A mouse report for a program that asked for the mouse: `button` 0 left, 1 middle, 2 right,
/// 64 and 65 the wheel up and down, plus 32 while dragging; `col` and `row` from 0. SGR form when
/// the program asked for it, the classic form otherwise.
fn mouse_report(mode: TermMode, button: u8, col: usize, row: usize, pressed: bool) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        format!("\x1b[<{button};{};{}{}", col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
    } else {
        let b = if pressed { button } else { 3 };
        let at = |v: usize| (32 + v + 1).min(255) as u8;
        vec![0x1b, b'[', b'M', 32 + b, at(col), at(row)]
    }
}

/// What the wheel sends to a full-screen program, `lines` up when positive: wheel reports when it
/// asked for the mouse (Claude Code, vim, htop), otherwise arrow keys (less, man).
fn wheel_bytes(mode: TermMode, lines: i32, col: usize, row: usize) -> Vec<u8> {
    let mut send = Vec::new();
    for _ in 0..lines.unsigned_abs() {
        if mode.intersects(TermMode::MOUSE_MODE) {
            send.extend(mouse_report(mode, if lines > 0 { 64 } else { 65 }, col, row, true));
        } else {
            let app = mode.contains(TermMode::APP_CURSOR);
            send.extend_from_slice(match (lines > 0, app) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            });
        }
    }
    send
}

/// Wheel steps in lines from this frame's events, up positive.
fn wheel_lines(ui: &egui::Ui, ch: f32, page: f32) -> i32 {
    let mut total = 0;
    for e in ui.ctx().input(|i| i.events.clone()) {
        let Event::MouseWheel { unit, delta, .. } = e else { continue };
        let lines = match unit {
            egui::MouseWheelUnit::Point => delta.y / ch,
            egui::MouseWheelUnit::Line => delta.y,
            egui::MouseWheelUnit::Page => delta.y * page,
        };
        total += if lines.abs() < 1.0 { lines.signum() as i32 } else { lines.round() as i32 };
    }
    total
}

/// Whether the wheel belongs to the program rather than to the view: a full-screen program, or one
/// that asked for the mouse.
fn program_scrolls(t: &Tab) -> bool {
    t.term.mode().intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
}

/// The text and Markdown views of a program that scrolls itself: the wheel over them goes to the
/// program, at the middle of its screen.
fn forward_wheel(ui: &egui::Ui, t: &Tab, area: Rect, ch: f32, out: &mut Vec<Cmd>) {
    if !program_scrolls(t) || !ui.rect_contains_pointer(area) || t.exited.is_some() {
        return;
    }
    let grid = t.term.grid();
    let lines = wheel_lines(ui, ch, grid.screen_lines() as f32);
    if lines != 0 {
        let send = wheel_bytes(*t.term.mode(), lines, grid.columns() / 2, grid.screen_lines() / 2);
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
    }
}

/// The most lines kept of a full-screen program's stitched document.
const STITCH_MAX: usize = 5000;

/// Puts a new screen of a full-screen program into the document stitched from its earlier screens.
/// Programs such as Claude Code scroll inside their own screen, so each screen is a window on a
/// longer text; where it overlaps the document (scrolled back, or new lines at the end) it replaces
/// the overlapped lines and extends the document; a screen that matches nothing starts it anew.
fn stitch(doc: &mut Vec<String>, screen: &[String]) {
    let m = screen.len();
    if m == 0 {
        return;
    }
    let n = doc.len();
    let mut best: Option<(i64, usize)> = None;
    for p in -(m as i64 - 1)..=(n as i64) {
        let from = (-p).max(0) as usize;
        let to = (m as i64).min(n as i64 - p).max(0) as usize;
        if from >= to {
            continue;
        }
        let overlap = to - from;
        let mut score = 0;
        let mut written = 0;
        for i in from..to {
            let (a, b) = (&doc[(p + i as i64) as usize], &screen[i]);
            if a == b {
                score += 1;
                written += !a.trim().is_empty() as usize;
            }
        }
        // Most of the overlap must agree, a few lines that are still being written aside, and it
        // must agree on text rather than on blank lines alone.
        if written >= 2.min(m) && score * 10 >= overlap * 7 && best.is_none_or(|(_, s)| score >= s) {
            best = Some((p, score));
        }
    }
    let Some((p, _)) = best else {
        *doc = screen.to_vec();
        return;
    };
    let mut out: Vec<String> = Vec::with_capacity(n + m);
    if p > 0 {
        out.extend_from_slice(&doc[..p as usize]);
    }
    out.extend_from_slice(screen);
    let after = (p + m as i64).max(0) as usize;
    if after < n {
        out.extend_from_slice(&doc[after..]);
    }
    if out.len() > STITCH_MAX {
        out.drain(..out.len() - STITCH_MAX);
    }
    *doc = out;
}

/// A screen of a full-screen agent without what stays at its bottom whatever is scrolled: the
/// input box, mode and status lines, and the jump-to-bottom hint with the row it covers.
fn content_rows(mut lines: Vec<String>) -> Vec<String> {
    // Claude Code pins the question being read to the top row, over the text under it.
    if lines.first().is_some_and(|l| l.starts_with("❯ ")) {
        lines[0].clear();
    }
    let frame = |l: &str| {
        let t = l.trim();
        !t.is_empty() && t.chars().all(|c| "─━═╭╮╰╯┌┐└┘│ ".contains(c))
    };
    // The input box: everything from the rule above the last prompt down.
    if let Some(p) = lines.iter().rposition(|l| l.trim_start().starts_with('❯'))
        && p > 0
        && frame(&lines[p - 1])
        && lines[p + 1..].iter().all(|l| frame(l) || l.trim().is_empty() || crate::markdown::is_chrome(l))
    {
        lines.truncate(p - 1);
    }
    while let Some(l) = lines.last() {
        if l.trim().is_empty() || crate::markdown::is_chrome(l) || frame(l) {
            lines.pop();
        } else {
            break;
        }
    }
    lines
}

/// Whether the screen's last row is a status line such as tmux draws: one background colour, not
/// the terminal's own, across the whole width.
fn status_row(t: &Tab) -> bool {
    let grid = t.term.grid();
    let row = &grid[Line(grid.screen_lines() as i32 - 1)];
    let first = row[Column(0)].bg;
    let default = matches!(first, Color::Named(alacritty_terminal::vte::ansi::NamedColor::Background));
    !default && (0..grid.columns()).all(|c| row[Column(c)].bg == first)
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

pub(crate) fn open_url(url: &str) {
    if cfg!(windows) {
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    } else {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

fn screen(
    ui: &mut egui::Ui,
    t: &mut Tab,
    font_size: f32,
    copy_on_select: bool,
    focused: bool,
    keys: bool,
    out: &mut Vec<Cmd>,
) -> PaneAsk {
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
    let mode = *t.term.mode();
    // A program that asked for the mouse gets clicks and drags; Shift keeps them for selecting here.
    let app_mouse = mode.intersects(TermMode::MOUSE_MODE) && !ui.input(|i| i.modifiers.shift) && t.exited.is_none();
    if app_mouse && (focused || ask.focus) {
        let mut send = Vec::new();
        let at = |p: Pos2| {
            let col = (((p.x - origin.x) / cw).floor().max(0.0) as usize).min(cols as usize - 1);
            let row = (((p.y - origin.y) / ch).floor().max(0.0) as usize).min(rows as usize - 1);
            (col, row)
        };
        for e in ui.ctx().input(|i| i.events.clone()) {
            match e {
                Event::PointerButton { pos, button, pressed, .. }
                    if area.contains(pos) || (!pressed && t.mouse_held.is_some()) =>
                {
                    let b = match button {
                        egui::PointerButton::Primary => 0,
                        egui::PointerButton::Middle => 1,
                        egui::PointerButton::Secondary => 2,
                        _ => continue,
                    };
                    let (col, row) = at(pos);
                    send.extend(mouse_report(mode, b, col, row, pressed));
                    t.mouse_held = pressed.then_some(b);
                }
                Event::PointerMoved(pos)
                    if area.contains(pos) && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) =>
                {
                    if let Some(b) = t.mouse_held {
                        let (col, row) = at(pos);
                        send.extend(mouse_report(mode, b + 32, col, row, true));
                    } else if mode.contains(TermMode::MOUSE_MOTION) {
                        let (col, row) = at(pos);
                        send.extend(mouse_report(mode, 35, col, row, true));
                    }
                }
                _ => {}
            }
        }
        if !send.is_empty() {
            out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: send }));
        }
    } else {
        t.mouse_held = None;
    }
    // Cell under a screen position, as a grid point.
    let cell_at = |p: Pos2| {
        let col = (((p.x - origin.x) / cw).floor().max(0.0) as usize).min(cols as usize - 1);
        let row = (((p.y - origin.y) / ch).floor().max(0.0) as i32).min(rows as i32 - 1);
        let side = if (p.x - origin.x) / cw - col as f32 > 0.5 { Side::Right } else { Side::Left };
        (Point::new(Line(row - offset), Column(col)), side)
    };

    // Mouse: drag selects, double click selects a word, the wheel scrolls back.
    if !app_mouse
        && resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (pt, side) = cell_at(p);
        t.term.selection = Some(Selection::new(SelectionType::Simple, pt, side));
    }
    if !app_mouse
        && resp.dragged_by(egui::PointerButton::Primary)
        && let (Some(p), Some(sel)) = (resp.interact_pointer_pos(), t.term.selection.as_mut())
    {
        let (pt, side) = cell_at(p);
        sel.update(pt, side);
    }
    if resp.drag_stopped()
        && copy_on_select
        && let Some(s) = t.term.selection_to_string().filter(|s| !s.is_empty())
    {
        ui.ctx().copy_text(s);
    }
    let ctrl = ui.input(|i| i.modifiers.ctrl);
    if resp.hovered()
        && ctrl
        && let Some(p) = resp.hover_pos()
    {
        let (pt, _) = cell_at(p);
        let line =
            t.term.bounds_to_string(Point::new(pt.line, Column(0)), Point::new(pt.line, Column(cols as usize - 1)));
        if url_at(&line, pt.column.0).is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    if !app_mouse
        && resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (pt, side) = cell_at(p);
        t.term.selection = Some(Selection::new(SelectionType::Semantic, pt, side));
    } else if resp.clicked() {
        t.term.selection = None;
        if ctrl && let Some(p) = resp.interact_pointer_pos() {
            let (pt, _) = cell_at(p);
            let line =
                t.term.bounds_to_string(Point::new(pt.line, Column(0)), Point::new(pt.line, Column(cols as usize - 1)));
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
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layer_id(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(("find", t.id)))),
        );
        egui::Frame::new()
            .fill(Color32::from_rgb(36, 38, 46))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(4.0)
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    let edit =
                        ui.add(egui::TextEdit::singleline(&mut search.query).hint_text("Find").desired_width(170.0));
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
                        search_step = Some(if shift {
                            alacritty_terminal::index::Direction::Right
                        } else {
                            alacritty_terminal::index::Direction::Left
                        });
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
        let escaped: String = search
            .query
            .chars()
            .map(|c| if "\\.+*?()|[]{}^$#&-~".contains(c) { format!("\\{c}") } else { c.to_string() })
            .collect();
        if let Ok(mut re) = alacritty_terminal::term::search::RegexSearch::new(&escaped) {
            let origin = match (&search.found, dir) {
                (Some(m), alacritty_terminal::index::Direction::Left) => *m.start(),
                (Some(m), _) => *m.end(),
                (None, _) => Point::new(Line(t.term.grid().screen_lines() as i32 - 1), Column(cols as usize - 1)),
            };
            let origin = if search.found.is_some() {
                match dir {
                    alacritty_terminal::index::Direction::Left => {
                        origin.sub(&t.term, alacritty_terminal::index::Boundary::Grid, 1)
                    }
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
        let c = t.term.grid().cursor.point;
        let at = Pos2::new(origin.x + c.column.0 as f32 * cw, origin.y + (c.line.0 + offset) as f32 * ch);
        keyboard(ui, t, resp.id, area, Rect::from_min_size(at, Vec2::new(cw, ch)), true, out);
    } else {
        t.preedit.clear();
    }
    // The wheel scrolls the pane under the pointer back through its scrollback; a full-screen
    // program or one that asked for the mouse scrolls itself.
    if resp.hovered() {
        let lines = wheel_lines(ui, ch, rows as f32);
        if lines != 0 && program_scrolls(t) && t.exited.is_none() {
            let (col, row) = resp.hover_pos().map_or((0, 0), |p| {
                let (pt, _) = cell_at(p);
                (pt.column.0, (pt.line.0 + offset).max(0) as usize)
            });
            out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: wheel_bytes(mode, lines, col, row) }));
        } else if lines != 0 {
            t.term.scroll_display(Scroll::Delta(lines));
        }
    }
    paint(ui, t, area, &font, cw, ch, 0, focused);
    let painter = ui.painter_at(area);
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
/// `view` ("text" or "markdown") is how the first tab starts.
pub fn run_window(first: Profile, panes: Vec<(Profile, bool)>, view: Option<String>) -> Result<(), String> {
    struct Solo(TermWindow, Option<View>);
    impl eframe::App for Solo {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            let mut out = Vec::new();
            if !self.0.before(&ctx, &mut out) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if let Some(t) = self.0.tabs.first_mut()
                && let Some(v) = self.1.take()
            {
                t.view = v;
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
            let view = match view.as_deref() {
                Some("text") => Some(View::Text),
                Some("markdown") => Some(View::Markdown),
                _ => None,
            };
            Ok(Box::new(Solo(w, view)))
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
    // With a saved password, sshremote answers ssh's password prompt itself.
    if crate::vault::list().iter().any(|h| h == host)
        && let Ok(exe) = std::env::current_exe()
    {
        let mut wrapped =
            vec![exe.to_string_lossy().to_string(), "--saved-login".into(), host.to_string(), "--".into()];
        wrapped.extend(argv);
        argv = wrapped;
    }
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

    /// A frame of the window with no real display, as the window would run it.
    fn frame(ctx: &egui::Context, w: &mut TermWindow, events: Vec<Event>) -> Vec<Cmd> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            events,
            ..Default::default()
        };
        let mut out = Vec::new();
        let _ = ctx.run_ui(input, |ui| out.extend(w.embedded(ui)));
        out
    }

    fn typed(cmds: &[Cmd]) -> String {
        let bytes: Vec<u8> = cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Send(ToAgent::TermInput { data, .. }) => Some(data.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn input_methods_type_into_the_shell() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        // The first frames open the shell and lay the terminal out.
        frame(&ctx, &mut w, Vec::new());
        frame(&ctx, &mut w, Vec::new());
        let preedit = Event::Ime(egui::ImeEvent::Preedit { text: "\u{D558}".into(), active_range_chars: None });
        let composing = frame(&ctx, &mut w, vec![preedit]);
        assert_eq!(typed(&composing), "", "nothing is sent while composing");
        assert_eq!(w.tabs[0].preedit, "\u{D558}");
        let enter = Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let commit = Event::Ime(egui::ImeEvent::Commit("\u{D55C}\u{AE00}".into()));
        let done = frame(&ctx, &mut w, vec![commit, Event::Text(" ok".into()), enter]);
        assert_eq!(typed(&done), "\u{D55C}\u{AE00} ok\r");
        assert!(w.tabs[0].preedit.is_empty());
        // The window asks the system for an input method at the cursor.
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| {
            w.embedded(ui);
        });
        let ime = out.platform_output.ime.expect("an input method at the cursor");
        // Asking for the focus again every frame would end the composition each time.
        assert!(!ime.should_interrupt_composition, "the composition is left alone between frames");
    }

    #[test]
    fn reading_views_still_type_into_the_shell() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        frame(&ctx, &mut w, Vec::new());
        for view in [View::Text, View::Markdown] {
            w.tabs[0].view = view;
            frame(&ctx, &mut w, Vec::new());
            let done = frame(
                &ctx,
                &mut w,
                vec![Event::Ime(egui::ImeEvent::Commit("\u{D55C}".into())), Event::Text("a".into())],
            );
            assert_eq!(typed(&done), "\u{D55C}a");
        }
    }

    /// Where the input method's cursor is after a frame with `events`.
    fn ime_y(ctx: &egui::Context, w: &mut TermWindow, events: Vec<Event>) -> f32 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            events,
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| {
            w.embedded(ui);
        });
        out.platform_output.ime.map_or(f32::NAN, |i| i.cursor_rect.top())
    }

    #[test]
    fn reading_views_scroll_with_the_wheel() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        for n in 0..300 {
            t.parser.advance(&mut t.term, format!("line {n}\r\n").as_bytes());
        }
        t.rev += 1;
        for view in [View::Text, View::Markdown] {
            w.tabs[0].view = view;
            for _ in 0..3 {
                ime_y(&ctx, &mut w, Vec::new());
            }
            let bottom = ime_y(&ctx, &mut w, Vec::new());
            assert!(bottom < 500.0, "{view:?}: the live rows start in view at the bottom ({bottom})");
            let wheel = Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: Vec2::new(0.0, 10.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
                source: egui::MouseWheelSource::Wheel,
            };
            ime_y(&ctx, &mut w, vec![Event::PointerMoved(Pos2::new(400.0, 250.0)), wheel]);
            let mut after = bottom;
            for _ in 0..30 {
                after = ime_y(&ctx, &mut w, vec![Event::PointerMoved(Pos2::new(400.0, 250.0))]);
            }
            assert!(after > bottom + 50.0, "{view:?}: the wheel scrolls back ({bottom} -> {after})");
        }
    }

    #[test]
    fn the_text_view_selects_and_copies_like_an_editor() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, b"hello world\r\n$ ");
        t.rev += 1;
        w.tabs[0].view = View::Text;
        frame(&ctx, &mut w, Vec::new());
        frame(&ctx, &mut w, Vec::new());
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let select_all = Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: ctrl };
        assert_eq!(typed(&frame(&ctx, &mut w, vec![select_all])), "", "Ctrl+A selects the text, not the shell's line");
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            events: vec![Event::Copy],
            ..Default::default()
        };
        let mut cmds = Vec::new();
        let out = ctx.run_ui(input, |ui| cmds.extend(w.embedded(ui)));
        assert_eq!(typed(&cmds), "", "Ctrl+C with a selection copies instead of interrupting");
        let copied = out
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s.contains("hello world")));
        assert!(copied, "the selection is copied");
        // Typing still reaches the shell, and the selection gives way to the caret.
        assert_eq!(typed(&frame(&ctx, &mut w, vec![Event::Text("ls".into())])), "ls");
    }

    #[test]
    fn styles_on_screen_come_back_as_markdown() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        let screen = "\x1b[2J\x1b[H\x1b[38;5;231m\u{25CF}\x1b[39m \x1b[1;3;4mThe Log\x1b[0m\r\n\r\n  \x1b[3mA short story\x1b[0m\r\n\r\n  \x1b[1mChapter 1\x1b[0m\r\n  plain \x1b[1mbold\x1b[0m word\r\n";
        t.parser.advance(&mut t.term, screen.as_bytes());
        let md = screen_markdown(t, 0, 6);
        assert_eq!(md, "\u{25CF} # The Log\n\n  *A short story*\n\n  ## Chapter 1\n  plain **bold** word");
    }

    #[test]
    fn screens_of_a_full_screen_program_stitch_into_one_document() {
        let lines = |r: std::ops::Range<usize>| r.map(|i| format!("line {i}")).collect::<Vec<_>>();
        let mut doc = Vec::new();
        stitch(&mut doc, &lines(20..30));
        // Scrolled back by five lines: the earlier ones go on top.
        stitch(&mut doc, &lines(15..25));
        assert_eq!(doc, lines(15..30));
        // New output at the end, with the last line still being written.
        let mut screen = lines(22..32);
        screen[9] = "line 31 (partial".into();
        stitch(&mut doc, &screen);
        assert_eq!(doc.len(), 17);
        assert_eq!(doc[16], "line 31 (partial");
        // Something else entirely starts a new document.
        stitch(&mut doc, &["menu".to_string(), "item".to_string()]);
        assert_eq!(doc, ["menu", "item"]);
    }

    /// Screens captured from a real full-screen agent (tmux capture-pane -e, s0.txt at the bottom,
    /// then one page up each), from the folder in SSHREMOTE_CAPS: cargo test -- --ignored captured
    #[test]
    #[ignore]
    fn captured_agent_screens_read_as_one_session() {
        let dir = std::env::var("SSHREMOTE_CAPS").expect("SSHREMOTE_CAPS");
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        t.term.resize(Size { cols: 120, rows: 30 });
        t.parser.advance(&mut t.term, b"\x1b[?1049h");
        let mut doc = Vec::new();
        for k in 0..6 {
            let Ok(raw) = std::fs::read_to_string(format!("{dir}/s{k}.txt")) else { break };
            let screen = format!("\x1b[H\x1b[2J{}", raw.trim_end_matches('\n').replace('\n', "\r\n"));
            t.parser.advance(&mut t.term, screen.as_bytes());
            let lines = content_rows(screen_markdown(t, 0, 30).lines().map(str::to_string).collect());
            stitch(&mut doc, &lines);
        }
        let text = doc.join("\n");
        eprintln!("{text}");
        let parsed = crate::markdown::Doc::new(&text);
        for p in &parsed.parts {
            eprintln!("--- {p:?}");
        }
        let blocks: Vec<&crate::markdown::Block> = parsed
            .parts
            .iter()
            .flat_map(|p| match p {
                crate::markdown::Part::Doc(b) | crate::markdown::Part::Reply { blocks: b, .. } => b.iter().collect(),
                _ => Vec::new(),
            })
            .collect();
        use crate::markdown::Block;
        assert!(blocks.iter().any(|b| matches!(b, Block::Table { rows, .. } if rows.len() >= 4)), "the whole table");
        assert!(blocks.iter().any(|b| matches!(b, Block::Heading(2, h) if h.contains("Chapter 5"))), "headings");
        assert!(!blocks.iter().any(|b| matches!(b, Block::Pre(_))), "prose is not read as columns");
        assert!(!text.contains("Jump to bottom") && !text.contains("bypass permissions"), "no controls");
    }
}
