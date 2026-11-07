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

mod input;
mod paint;
mod reader;
mod stitch;

use input::*;
pub(crate) use paint::open_url;
use paint::*;
use reader::*;
use stitch::*;

const TAB_STRIP: Color32 = Color32::from_rgb(12, 13, 16);

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

/// The text and Markdown views are made again from the scrollback at most this often (seconds)
/// while output streams in.
const REBUILD_EVERY: f64 = 0.1;

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
    /// The scrollback's rows, worked out once each for the reading views.
    rows: reader::Rows,
    /// The text view's document as a layout job, for the same key, font size and colour.
    editor_galley: Option<(EditorKey, f32, Color32, std::sync::Arc<egui::text::LayoutJob>)>,
    /// The text view's document and the cursor's place in it, for an output revision and cursor.
    editor: Option<(EditorKey, std::rc::Rc<(String, usize)>)>,
    /// The mouse button a program that asked for the mouse holds down, for its drag reports.
    mouse_held: Option<u8>,
    /// A full-screen program's screens stitched into one document, as Markdown lines.
    stitched: Vec<String>,
    /// The full-screen program's last screen, to tell its fixed rows from those that move.
    last_screen: Vec<String>,
    /// Where the last screen went into the stitched document, to look there first.
    stitch_at: Option<i64>,
    /// Hints a full-screen program floats over its text, found by comparing screens.
    overlays: Vec<String>,
    /// Heights of the Markdown view's blocks, measured once each.
    md_heights: crate::markdown::Heights,
    /// When the text or Markdown view was last rebuilt from the scrollback, in egui's time.
    built_at: f64,
    /// When something was last typed, in egui's time: the cursor stays lit, not blinking, after it.
    typed_at: f64,
}

/// The text and Markdown views' content, with the output revision and the first live row it ends above.
type Reading = (u64, i32, std::rc::Rc<(String, crate::markdown::Doc)>);

impl Tab {
    #[cfg(test)]
    fn rows_len(&self) -> usize {
        self.rows.len()
    }

    #[cfg(test)]
    fn rows_md(&self) -> Vec<(String, bool)> {
        self.rows.md()
    }

    /// Lets go of what the text and Markdown views built, when the terminal is shown again.
    fn forget_reading(&mut self) {
        self.reading = None;
        self.editor = None;
        self.rows = Default::default();
        self.editor_galley = None;
        self.md_heights = Default::default();
        self.stitched = Vec::new();
        self.last_screen = Vec::new();
        self.overlays = Vec::new();
        self.stitch_at = None;
    }
}

/// What the text view's document was made for: the output revision and the cursor's row and column.
type EditorKey = (u64, i32, usize);

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
    viewport: egui::ViewportId,
    /// Where a window made from a torn-off tab opens, in screen points.
    position: Option<Pos2>,
    /// The tab being dragged in the strip.
    dragging: Option<u32>,
    /// Windows made from tabs dragged out of this one, for the owner to show.
    torn: Vec<TermWindow>,
    title: String,
    copy_on_select: bool,
    renaming: Option<(u32, String)>,
    /// Tabs and panes to open on the next frame (the window's context is needed to wake it for output).
    pending: Vec<Pending>,
    /// The window closes with its last tab instead of opening a fresh shell.
    standalone: bool,
    /// A window of its own, which draws its frame: tabs in the title bar beside the window buttons.
    framed: bool,
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
            viewport: egui::ViewportId::from_hash_of(viewport),
            position: None,
            dragging: None,
            torn: Vec::new(),
            title: title.into(),
            copy_on_select: setting("term_copy_select").is_some_and(|v| v == "1"),
            renaming: None,
            pending: Vec::new(),
            standalone: false,
            framed: false,
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
    /// Moves the tab `group` to where the tab `to` is.
    fn move_group(&mut self, group: u32, to: u32) {
        let active = self.tabs.get(self.active).map(|t| t.id);
        let order: Vec<u32> = self.groups().iter().map(|g| g.0).collect();
        let (Some(from), Some(target)) = (order.iter().position(|&g| g == group), order.iter().position(|&g| g == to))
        else {
            return;
        };
        let moved: Vec<Tab> = self.tabs.extract_if(.., |t| t.group == group).collect();
        // Before the target when moving left, after it when moving right.
        let at = if target < from {
            self.tabs.iter().position(|t| t.group == to).unwrap_or(0)
        } else {
            self.tabs.iter().rposition(|t| t.group == to).map_or(self.tabs.len(), |k| k + 1)
        };
        for (k, t) in moved.into_iter().enumerate() {
            self.tabs.insert(at + k, t);
        }
        if let Some(id) = active {
            self.active = self.tabs.iter().position(|t| t.id == id).unwrap_or(0);
        }
    }

    /// Takes the tab `group` out into a new window at `at` (screen points). Shells on the SSH host
    /// of a remote-screen session stay with it, since that session carries their output.
    fn tear(&mut self, group: u32, at: Pos2) {
        if !self.framed
            || !self.standalone
            || self.tabs.iter().any(|t| t.group == group && matches!(t.backend, Backend::Agent))
        {
            return;
        }
        let active = self.tabs.get(self.active).map(|t| t.id);
        let moved: Vec<Tab> = self.tabs.extract_if(.., |t| t.group == group).collect();
        if moved.is_empty() {
            return;
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut w = TermWindow::with_profiles(self.profiles.clone(), "sshremote-torn-terminal", &self.title);
        w.viewport = egui::ViewportId::from_hash_of(("sshremote-torn-terminal", n));
        w.next_id = moved.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        w.tabs = moved;
        w.framed = true;
        w.standalone = true;
        w.position = Some(at);
        w.system = self.system.clone();
        w.font_size = self.font_size;
        self.torn.push(w);
        self.active = active.and_then(|id| self.tabs.iter().position(|t| t.id == id)).unwrap_or(0);
    }

    /// Windows made from tabs dragged out of this one since the last call.
    pub fn take_torn(&mut self) -> Vec<TermWindow> {
        std::mem::take(&mut self.torn)
    }

    /// Shows windows made from torn-off tabs, keeps those still open, and adds windows torn off
    /// them in turn.
    pub fn show_torn(ctx: &egui::Context, windows: &mut Vec<TermWindow>) {
        let mut more = Vec::new();
        windows.retain_mut(|w| {
            let cmds = w.show(ctx, "");
            more.extend(w.take_torn());
            !cmds.iter().any(|c| matches!(c, Cmd::Close))
        });
        windows.extend(more);
    }

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
            md_heights: Default::default(),
            built_at: 0.0,
            stitched: Vec::new(),
            last_screen: Vec::new(),
            overlays: Vec::new(),
            stitch_at: None,
            mouse_held: None,
            editor: None,
            editor_galley: None,
            rows: Default::default(),
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
        self.framed = false;
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
            .with_min_inner_size([360.0, 200.0])
            .with_decorations(false);
        self.framed = true;
        if let Some(icon) = crate::gui::app_icon() {
            builder = builder.with_icon(icon);
        }
        if let Some(p) = self.position.take() {
            builder = builder.with_position(p);
        }
        ctx.show_viewport_immediate(self.viewport, builder, |ui, _| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                out.extend(self.close_all());
                out.push(Cmd::Close);
                return;
            }
            crate::gui::chrome::behaviour(ui.ctx());
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

    /// The tabs, the new-tab button and menu, and the window's own buttons (views, settings).
    fn strip(&mut self, ui: &mut egui::Ui, out: &mut Vec<Cmd>) {
        // Colours of the bar the strip sits in: the title bar follows the app's light or dark theme.
        let strong = ui.visuals().strong_text_color();
        let weak = ui.visuals().text_color().gamma_multiply(0.75);
        let hover_fill = ui.visuals().widgets.hovered.weak_bg_fill;
        let divider = ui.visuals().widgets.noninteractive.bg_stroke.color;
        // The active tab joins the terminal, so its text follows the terminal's colours.
        let tab_fg = scheme_fg();
        let mut close = None;
        let groups = self.groups();
        let current = self.tabs.get(self.active).map(|t| t.group);
        let n = groups.len().max(1) as f32;
        // What the tabs leave: new tab and profiles (58), the view switch (3 buttons of 30 in its
        // track), the settings (28), the system terminal button when there is one, and the gaps.
        let mut controls = 58.0 + 96.0 + 28.0 + if self.system.is_some() { 30.0 } else { 0.0 } + 24.0;
        // Too narrow for that and a tab: the view switch folds into one button with a menu.
        let fold_views = ui.available_width() < controls + 64.0;
        if fold_views {
            controls -= 96.0 - 30.0;
        }
        let room = (ui.available_width() - controls).max(64.0);
        let tab_w = (room / n).clamp(64.0, 220.0);
        let mut tab_rects: Vec<(u32, Rect)> = Vec::new();
        let mut drag_end: Option<(u32, Pos2)> = None;
        for (gi, &(group, i)) in groups.iter().enumerate() {
            let active = Some(group) == current;
            let next_active = groups.get(gi + 1).is_some_and(|g| Some(g.0) == current);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(tab_w, 34.0), Sense::hover());
            let r = ui.interact(rect, ui.make_persistent_id(("term-tab", group)), Sense::click_and_drag());
            tab_rects.push((group, rect));
            let hovered = r.hovered();
            let p = ui.painter();
            let top_round = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
            if active {
                p.rect_filled(rect, top_round, scheme_bg());
            } else if hovered {
                p.rect_filled(rect.shrink2(Vec2::new(2.0, 3.0)), CornerRadius::same(6), hover_fill);
            } else if !next_active && gi + 1 < groups.len() {
                // A thin divider between two inactive tabs.
                let x = rect.right() - 0.5;
                p.vline(x, rect.y_range().shrink(9.0), egui::Stroke::new(1.0, divider));
            }
            let t = &self.tabs[i];
            let ended = t.exited.is_some();
            let fg = if active { tab_fg } else { weak };
            let fg = if ended { fg.gamma_multiply(0.5) } else { fg };
            p.text(
                rect.left_center() + Vec2::new(12.0, 0.0),
                Align2::LEFT_CENTER,
                ic::TERMINAL,
                FontId::proportional(14.0),
                fg,
            );
            // The close button appears on the active tab and under the pointer, as in browsers.
            let close_rect = Rect::from_center_size(rect.right_center() - Vec2::new(16.0, 0.0), Vec2::splat(20.0));
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
            let title_rect =
                Rect::from_min_max(Pos2::new(rect.left() + 32.0, rect.top()), Pos2::new(title_right, rect.bottom()));
            let galley = p.layout_no_wrap(title, FontId::proportional(13.0), fg);
            let clip = ui.painter_at(title_rect);
            clip.galley(Pos2::new(title_rect.left(), rect.center().y - galley.size().y / 2.0), galley.clone(), fg);
            if galley.size().x > title_rect.width() {
                // Fade the cut end instead of ending mid-letter.
                let bg = if active {
                    scheme_bg()
                } else if hovered {
                    hover_fill
                } else {
                    ui.visuals().panel_fill
                };
                let fade = Rect::from_min_max(
                    Pos2::new(title_rect.right() - 18.0, rect.top() + 6.0),
                    Pos2::new(title_rect.right(), rect.bottom() - 6.0),
                );
                for k in 0..6 {
                    let a = (k as f32 + 1.0) / 6.0;
                    let x0 = fade.left() + fade.width() * k as f32 / 6.0;
                    let slice = Rect::from_min_max(Pos2::new(x0, fade.top()), Pos2::new(fade.right(), fade.bottom()));
                    ui.painter().rect_filled(slice, CornerRadius::ZERO, bg.gamma_multiply(a * 0.35));
                }
            }
            if show_close {
                let cr = ui.interact(close_rect, ui.make_persistent_id(("close-tab", t.id)), Sense::click());
                if cr.hovered() {
                    ui.painter().circle_filled(close_rect.center(), 10.0, fg.gamma_multiply(0.18));
                }
                ui.painter().text(close_rect.center(), Align2::CENTER_CENTER, ic::X, FontId::proportional(12.0), fg);
                if cr.clicked() {
                    close = Some(group);
                }
                cr.on_hover_text("Close (middle click on the tab also closes)");
            }
            if r.clicked() && close.is_none() {
                self.active = i;
            }
            if r.drag_started_by(egui::PointerButton::Primary) {
                if groups.len() == 1 && self.framed {
                    // The only tab moves the window, as in Windows Terminal.
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                } else {
                    self.dragging = Some(group);
                    self.active = i;
                }
            }
            if r.drag_stopped()
                && self.dragging == Some(group)
                && let Some(p) = ui.ctx().input(|inp| inp.pointer.latest_pos())
            {
                drag_end = Some((group, p));
            }
            if r.middle_clicked() {
                close = Some(group);
            }
            if r.double_clicked() {
                let name = self.tabs[i].custom.clone().unwrap_or_else(|| self.tabs[i].title.clone());
                self.renaming = Some((self.tabs[i].id, name));
            }
            r.on_hover_text(&self.tabs[i].title);
        }
        // A dragged tab moves along the strip; dropped away from it, it opens in a window of its own.
        if let Some(group) = self.dragging
            && ui.input(|i| i.pointer.primary_down())
            && let Some(p) = ui.ctx().pointer_interact_pos()
            && let Some(&(over, _)) = tab_rects.iter().find(|(g, r)| *g != group && r.x_range().contains(p.x))
        {
            self.move_group(group, over);
        }
        if let Some((group, p)) = drag_end {
            self.dragging = None;
            let strip = tab_rects.iter().fold(Rect::NOTHING, |a, (_, r)| a.union(*r));
            if (p.y - strip.center().y).abs() > 60.0 || !ui.ctx().content_rect().contains(p) {
                let origin = ui.ctx().input(|i| i.viewport().outer_rect.map_or(Pos2::ZERO, |r| r.min));
                self.tear(group, origin + p.to_vec2() - Vec2::new(60.0, 16.0));
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.dragging = None;
        }
        ui.add_space(4.0);
        let (plus, pr) = ui.allocate_exact_size(Vec2::new(32.0, 30.0), Sense::click());
        if pr.hovered() {
            ui.painter().rect_filled(plus.shrink(2.0), CornerRadius::same(6), hover_fill);
        }
        ui.painter().text(plus.center(), Align2::CENTER_CENTER, ic::PLUS, FontId::proportional(15.0), strong);
        if pr.on_hover_text("New tab (Ctrl+Shift+T)").clicked() {
            self.open_tab();
        }
        // The profiles: local shells, ssh hosts, the SSH host's own shell.
        let menu_btn = egui::Button::new(RichText::new(ic::CARET_DOWN).size(12.0).color(strong))
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
                ui.add(crate::gui::tool_button(glyph, on)).on_hover_text(tip).clicked()
            };
            if let Some(cmd) = &self.system
                && small(ui, ic::ARROW_SQUARE_OUT, "Open this host in the system terminal (ssh)", false)
            {
                let _ = spawn_system(cmd);
            }
            let font = &mut self.font_size;
            let mut copy_on_select = self.copy_on_select;
            let mut changed = false;
            let gear = egui::Button::new(RichText::new(ic::GEAR).size(14.0).color(weak))
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
            // How the active tab is shown: a segmented control, as in Windows 11.
            if let Some(t) = self.tabs.get_mut(self.active) {
                let views = [
                    (View::Markdown, ic::ARTICLE, "Markdown", "Read the output as Markdown, with copy buttons on code (agent replies)"),
                    (View::Text, ic::TEXT_T, "Text", "Plain text of the whole buffer, to select and copy"),
                    (View::Terminal, ic::TERMINAL, "Terminal", "Terminal"),
                ];
                let mut pick = None;
                if fold_views {
                    let glyph = views.iter().find(|v| v.0 == t.view).map_or(ic::TERMINAL, |v| v.1);
                    egui::containers::menu::MenuButton::from_button(crate::gui::tool_button(glyph, false))
                        .ui(ui, |ui| {
                            ui.set_min_width(160.0);
                            for &(v, glyph, name, tip) in views.iter().rev() {
                                let b = egui::Button::selectable(t.view == v, format!("{glyph}  {name}"));
                                if ui.add(b).on_hover_text(tip).clicked() {
                                    pick = Some(v);
                                    ui.close();
                                }
                            }
                        })
                        .0
                        .on_hover_text("How this tab is shown");
                } else {
                    let track =
                        if ui.visuals().dark_mode { Color32::from_white_alpha(10) } else { Color32::from_black_alpha(10) };
                    egui::Frame::new().fill(track).corner_radius(8).inner_margin(2).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for &(v, glyph, _, tip) in &views {
                            if small(ui, glyph, tip, t.view == v) {
                                pick = Some(v);
                            }
                        }
                    });
                }
                if let Some(v) = pick {
                    t.view = v;
                    if v == View::Terminal {
                        t.forget_reading();
                    }
                }
            }
        });
    }

    fn body(&mut self, ui: &mut egui::Ui, out: &mut Vec<Cmd>) {
        // A tab strip as in Windows Terminal or a browser: the active tab joins the terminal below.
        // A window of its own has it in its title bar, beside the window's buttons.
        if self.framed {
            let bar = crate::gui::chrome::title_bar(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Max), |ui| self.strip(ui, out));
            });
            if bar.close {
                out.extend(self.close_all());
                out.push(Cmd::Close);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else {
            let strip =
                egui::Frame::new().fill(TAB_STRIP).inner_margin(egui::Margin { left: 8, right: 6, top: 6, bottom: 0 });
            egui::Panel::top("term-tabs").frame(strip).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
                ui.horizontal(|ui| self.strip(ui, out));
            });
        }
        // Keyboard shortcuts of the window itself, as in other terminals.
        let shortcuts = self.renaming.is_none();
        let (new_tab, close_tab, next, bigger, smaller) = ui.ctx().input(|i| {
            if !shortcuts {
                return (false, false, false, false, false);
            }
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
            if !shortcuts {
                return (None, 0);
            }
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
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == i) {
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
    // Which shells are installed does not change while the program runs.
    static FOUND: std::sync::OnceLock<Vec<Profile>> = std::sync::OnceLock::new();
    FOUND.get_or_init(find_local_profiles).clone()
}

fn find_local_profiles() -> Vec<Profile> {
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
    struct Solo(TermWindow, Option<View>, Vec<TermWindow>);
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
            crate::gui::chrome::behaviour(&ctx);
            self.0.body(ui, &mut out);
            self.0.after(&ctx, &mut out);
            // Windows made from torn-off tabs, each with its own viewport.
            self.2.extend(self.0.take_torn());
            TermWindow::show_torn(&ctx, &mut self.2);
        }
    }
    let native = crate::gui::native_options("SSH Remote Terminal", [960.0, 600.0]);
    eframe::run_native(
        "sshremote-terminal",
        native,
        Box::new(move |cc| {
            crate::gui::style(&cc.egui_ctx);
            let mut w = TermWindow::standalone(first, Vec::new());
            w.framed = true;
            w.pending.extend(panes.into_iter().map(|(p, side)| (p, Some((0, side)))));
            let view = match view.as_deref() {
                Some("text") => Some(View::Text),
                Some("markdown") => Some(View::Markdown),
                _ => None,
            };
            Ok(Box::new(Solo(w, view, Vec::new())))
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
        for k in 0..6 {
            let Ok(raw) = std::fs::read_to_string(format!("{dir}/s{k}.txt")) else { break };
            let screen = format!("\x1b[H\x1b[2J{}", raw.trim_end_matches('\n').replace('\n', "\r\n"));
            t.parser.advance(&mut t.term, screen.as_bytes());
            // The rows above the input line, as the view takes them.
            let lines: Vec<String> = screen_markdown(t, 0, 25).lines().map(str::to_string).collect();
            stitch_screen(t, lines);
        }
        let doc = t.stitched.clone();
        let text = doc.join("\n");
        eprintln!("{text}");
        let parsed = crate::markdown::Doc::new(&text);
        let blocks: Vec<&crate::markdown::Block> = parsed.blocks.iter().collect();
        use crate::markdown::Block;
        assert!(blocks.iter().any(|b| matches!(b, Block::Table { rows, .. } if rows.len() >= 4)), "the whole table");
        assert!(blocks.iter().any(|b| matches!(b, Block::Heading(2, h) if h.contains("Chapter 5"))), "headings");
        assert!(!blocks.iter().any(|b| matches!(b, Block::Pre(_))), "prose is not read as columns");
        assert!(!text.contains("bypass permissions"), "the fixed status line is left out");
    }

    #[test]
    fn tabs_move_along_the_strip_and_session_shells_stay() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        w.open_tab();
        w.open_tab();
        frame(&ctx, &mut w, Vec::new());
        let order = |w: &TermWindow| w.groups().iter().map(|g| g.0).collect::<Vec<_>>();
        let before = order(&w);
        assert_eq!(before.len(), 3);
        w.move_group(before[0], before[2]);
        assert_eq!(order(&w), [before[1], before[2], before[0]]);
        w.move_group(before[0], before[1]);
        assert_eq!(order(&w), before);
        // Shells on the SSH host belong to the session's window.
        w.framed = true;
        w.tear(before[1], Pos2::ZERO);
        assert!(w.take_torn().is_empty());
        assert_eq!(order(&w).len(), 3);
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    /// Time to rebuild the reading views from a full scrollback, whole and after new output:
    /// cargo test --release -- --ignored --nocapture rebuild_cost
    #[test]
    #[ignore]
    fn rebuild_cost() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))), ..Default::default() };
        let _ = ctx.run_ui(input.clone(), |ui| { w.embedded(ui); });
        let _ = ctx.run_ui(input, |ui| { w.embedded(ui); });
        let t = &mut w.tabs[0];
        let out = |t: &mut Tab, from: usize, n: usize| {
            for k in from..from + n {
                t.parser.advance(&mut t.term, format!("line {k}: some \x1b[1mbold\x1b[0m output with `code`\r\n").as_bytes());
            }
        };
        out(t, 0, 12_000);
        let start = std::time::Instant::now();
        reader::sync_rows(t);
        let whole = start.elapsed();
        out(t, 12_000, 50);
        let start = std::time::Instant::now();
        reader::sync_rows(t);
        let step = start.elapsed();
        eprintln!("{} rows: whole {:.1} ms, after 50 new lines {:.2} ms", t.rows_len(), whole.as_secs_f64() * 1e3, step.as_secs_f64() * 1e3);
    }
}

#[cfg(test)]
mod rows {
    use super::*;

    fn tab_with(w: &mut TermWindow, ctx: &egui::Context) {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))), ..Default::default() };
        let _ = ctx.run_ui(input.clone(), |ui| { w.embedded(ui); });
        let _ = ctx.run_ui(input, |ui| { w.embedded(ui); });
    }

    /// The cached scrollback matches the scrollback worked out whole, while it fills, once it is
    /// full and old rows drop, and after it is cleared.
    #[test]
    fn cached_rows_follow_the_scrollback() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        tab_with(&mut w, &ctx);
        let t = &mut w.tabs[0];
        let check = |t: &mut Tab| {
            reader::sync_rows(t);
            let g = t.term.grid();
            let history = (g.total_lines() - g.screen_lines()) as i32;
            let whole = reader::screen_markdown(t, -history, 0);
            let cached = reader::join_rows(t.rows_md());
            assert_eq!(cached, whole);
        };
        for round in 0..30 {
            for k in 0..(round * 97 % 700 + 1) {
                t.parser.advance(&mut t.term, format!("r{round} l{k} \x1b[1mb\x1b[0m\r\n").as_bytes());
            }
            check(t);
        }
        t.parser.advance(&mut t.term, b"\x1b[3J\x1b[2J\x1b[Hafter clear\r\n");
        check(t);
    }

    /// Output that repeats itself does not fool the cache once the scrollback is full.
    #[test]
    fn cached_rows_survive_repeated_output() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        tab_with(&mut w, &ctx);
        let t = &mut w.tabs[0];
        let check = |t: &mut Tab| {
            reader::sync_rows(t);
            let g = t.term.grid();
            let history = (g.total_lines() - g.screen_lines()) as i32;
            assert_eq!(reader::join_rows(t.rows_md()), reader::screen_markdown(t, -history, 0));
        };
        for k in 0..10_100 {
            t.parser.advance(&mut t.term, format!("unique {k}\r\n").as_bytes());
        }
        check(t);
        for round in 0..20 {
            for _ in 0..(round % 5 + 1) * 3 {
                t.parser.advance(&mut t.term, b"same\r\n");
            }
            if round % 3 == 0 {
                t.parser.advance(&mut t.term, b"\r\n\r\n");
            }
            check(t);
        }
    }
}
