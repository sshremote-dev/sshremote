//! The terminal window: shells in tabs, drawn with Alacritty's terminal emulator. A tab runs a
//! local program (a shell, or `ssh HOST`) or a shell on the host through the agent's connection.

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
mod collect;
mod reader;

use input::*;
pub(crate) use paint::open_url;
use paint::*;
use reader::*;

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

/// The scheme in use, shared by every terminal window; `AUTO` follows the app's theme.
static SCHEME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(AUTO);
const AUTO: usize = usize::MAX;

/// The scheme shown: the one chosen, or One Half Dark or Light after the app's theme (or after
/// the background of the colours chosen in Appearance, for the other colours).
fn scheme() -> &'static Scheme {
    if let Some((_, bg)) = crate::gui::look::custom_colors() {
        let [r, g, b, _] = bg.to_array();
        let light = r as u32 * 299 + g as u32 * 587 + b as u32 * 114 > 128_000;
        return if light { &SCHEMES[SCHEMES.len() - 1] } else { &SCHEMES[0] };
    }
    match SCHEME.load(std::sync::atomic::Ordering::Relaxed) {
        AUTO if crate::gui::theme::DARK.load(std::sync::atomic::Ordering::Relaxed) => &SCHEMES[0],
        AUTO => &SCHEMES[SCHEMES.len() - 1],
        i => &SCHEMES[i % SCHEMES.len()],
    }
}

/// The cursor's shape: a thin bar as in VS Code and Windows Terminal, a block, or an underline.
static CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const CURSORS: [&str; 3] = ["Bar", "Block", "Underline"];

/// The text and Markdown views are made again from the scrollback at most this often (seconds)
/// while output streams in.
const REBUILD_EVERY: f64 = 0.1;
/// The Markdown view builds its document again once output has been still this long (seconds),
/// so text a program keeps redrawing (a spinner, a reply being streamed) does not churn it.
const SETTLE: f64 = 0.35;
/// ...and at least this often while output never stops, so a long run still shows progress.
const SETTLE_AT_MOST: f64 = 1.2;

/// Selected cells, in VS Code's selection colour for a dark or a light background.
fn selection() -> Color32 {
    let [r, g, b, _] = scheme_bg().to_array();
    let light = r as u32 * 299 + g as u32 * 587 + b as u32 * 114 > 128_000;
    if light { Color32::from_rgb(173, 214, 255) } else { Color32::from_rgb(38, 79, 120) }
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn scheme_bg() -> Color32 {
    crate::gui::look::custom_colors().map_or_else(|| rgb(scheme().1), |c| c.1)
}

fn scheme_fg() -> Color32 {
    crate::gui::look::custom_colors().map_or_else(|| rgb(scheme().2), |c| c.0)
}

/// The terminal's text size set in Appearance, and a count of its changes: every terminal takes
/// it when it changes.
static TERM_SIZE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static TERM_SIZE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn set_term_size(size: f32) {
    TERM_SIZE.store(size.to_bits(), std::sync::atomic::Ordering::Relaxed);
    TERM_SIZE_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::hosts::set_setting("term_font", &format!("{size}"));
}

/// The terminal's text size, for Appearance.
pub(crate) fn size_picker(ui: &mut egui::Ui) {
    let now = f32::from_bits(TERM_SIZE.load(std::sync::atomic::Ordering::Relaxed));
    let mut size = if now > 0.0 { now } else { crate::hosts::setting("term_font").and_then(|v| v.parse().ok()).unwrap_or(14.0) };
    if ui.add(egui::Slider::new(&mut size, 8.0..=32.0).step_by(1.0).text("Text size")).changed() {
        set_term_size(size);
    }
}

/// The terminal's colour scheme, for Appearance.
pub(crate) fn scheme_picker(ui: &mut egui::Ui) {
    let current = SCHEME.load(std::sync::atomic::Ordering::Relaxed);
    ui.horizontal_wrapped(|ui| {
        ui.label("Scheme");
        let names = std::iter::once((AUTO, "Automatic")).chain(SCHEMES.iter().enumerate().map(|(k, sc)| (k, sc.0)));
        for (k, name) in names {
            if ui.selectable_label(k == current, name).clicked() {
                SCHEME.store(k, std::sync::atomic::Ordering::Relaxed);
                crate::hosts::set_setting("term_colors", &if k == AUTO { "auto".to_string() } else { k.to_string() });
            }
        }
    });
}

/// Appearance back to its defaults: the automatic scheme and the usual text size.
pub(crate) fn reset_look() {
    SCHEME.store(AUTO, std::sync::atomic::Ordering::Relaxed);
    crate::hosts::set_setting("term_colors", "auto");
    set_term_size(14.0);
}

fn pal(i: usize) -> Color32 {
    rgb(scheme().3[i % 16])
}

/// A colour as a program asking for it (OSC 4, 10, 11, 12) gets it when it set none: the
/// scheme's 16, xterm's cube and greys for the rest of the 256, and the scheme's text and
/// background for the special ones (256 text, 257 background, 258 cursor).
fn default_color(i: usize) -> Color32 {
    match i {
        0..16 => pal(i),
        16..232 => {
            let v = |x: usize| if x == 0 { 0 } else { (55 + 40 * x) as u8 };
            let n = i - 16;
            Color32::from_rgb(v(n / 36), v(n / 6 % 6), v(n % 6))
        }
        232..256 => {
            let g = (8 + 10 * (i - 232)) as u8;
            Color32::from_rgb(g, g, g)
        }
        256 => scheme_fg(),
        257 => scheme_bg(),
        _ => scheme_fg(),
    }
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
    /// Kept open when its program ends well (a tool's output to read).
    hold: bool,
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
    /// The text view's text and the Markdown view's document, for `rev` and the rows cleared by hand.
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
    /// Heights of the Markdown view's blocks, measured once each.
    md_heights: crate::markdown::Heights,
    /// When the text or Markdown view was last rebuilt from the scrollback, in egui's time.
    built_at: f64,
    /// The output revision last seen by the Markdown view, and when it last changed: the view
    /// waits for the output to settle before it builds the document again.
    seen_rev: u64,
    /// The Markdown view's record of a full-screen program, gathered from its screens.
    collect: collect::Collect,
    /// Rows of a shell's record cleared by hand (from its oldest).
    md_skip: usize,
    /// Copy the selection, asked from the Markdown view's menu.
    md_copy: bool,
    /// The Markdown view's scroll offset and the document's height, last frame.
    md_offset: f32,
    md_end: f32,
    /// The Markdown view's search.
    find: Option<reader::Find>,
    /// Whether the Markdown view was at the bottom: it stays there as the document changes.
    md_bottom: bool,
    /// The document last drawn in the Markdown view.
    md_laid: u64,
    changed_at: f64,
    /// When something was last typed, in egui's time: the cursor stays lit, not blinking, after it.
    typed_at: f64,
    /// Where a click asked the cursor to go, while arrow keys take it there.
    seek: Option<input::Seek>,
    /// Alt+arrows move between this window's own panes (not in a pane of the workspace, which
    /// takes them itself when it has panes to move between, and lets the shell have them else).
    nav_keys: bool,
}

/// The Markdown view's document and what it was built from (a record's change count, or the
/// output revision and the rows cleared by hand).
type Reading = ((bool, u64, usize), std::rc::Rc<(String, crate::markdown::Doc)>);

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
    /// A program here whose pane stays when it ends well, its output there to read (a tool's
    /// ssh-keygen or key install).
    Tool(Vec<String>),
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
    /// One shell shown as a pane of another window: no tab strip, and tabs and splits are that
    /// window's (its shortcuts here do nothing).
    pub bare: bool,
    /// The view the first tab starts in (`sshremote terminal --view`), once it is there.
    pub start_view: Option<String>,
    /// Whether keys typed in this window go to the shell; a window that shows the terminal beside
    /// something else (the remote screen) turns it off while that has the keyboard.
    pub keys_on: bool,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
    font_size: f32,
    /// The Appearance text size this window last took.
    size_gen: u64,
    /// A command that opens the same host in the system's own terminal, if there is one.
    system: Option<Vec<String>>,
    profiles: Vec<Profile>,
    /// Where the window's position is remembered and what its title says.
    viewport: egui::ViewportId,
    /// The tab being dragged in the strip.
    dragging: Option<u32>,
    title: String,
    copy_on_select: bool,
    renaming: Option<(u32, String)>,
    /// Tabs and panes to open on the next frame (the window's context is needed to wake it for output).
    pending: Vec<Pending>,
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

    /// Keeps only the shells running on this computer (a local shell, `ssh HOST`): those of an
    /// agent end with its connection. Whether any is left.
    pub fn keep_local(&mut self) -> bool {
        self.tabs.retain(|t| matches!(t.backend, Backend::Local(_)));
        self.active = 0;
        !self.tabs.is_empty()
    }

    /// A terminal of one shell, `profile`, to show inside another window (a workspace pane).
    pub fn single(profile: Profile) -> Self {
        let mut w = Self::with_profiles(local_profiles(), "sshremote-pane-terminal", "Terminal");
        w.pending.push((profile, None));
        w.bare = true;
        w
    }

    fn with_profiles(profiles: Vec<Profile>, viewport: &'static str, title: &str) -> Self {
        let c =
            crate::hosts::setting("term_cursor").and_then(|v| v.parse().ok()).unwrap_or(0usize).min(CURSORS.len() - 1);
        CURSOR.store(c, std::sync::atomic::Ordering::Relaxed);
        let setting = |k: &str| crate::hosts::setting(k);
        // "auto" or a scheme's number; the older key always held a number, chosen or not.
        let chosen = setting("term_colors").and_then(|v| v.parse::<usize>().ok()).filter(|&i| i < SCHEMES.len());
        SCHEME.store(chosen.unwrap_or(AUTO), std::sync::atomic::Ordering::Relaxed);
        TermWindow {
            bare: false,
            start_view: None,
            keys_on: true,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            font_size: setting("term_font").and_then(|v| v.parse().ok()).unwrap_or(14.0),
            size_gen: TERM_SIZE_GEN.load(std::sync::atomic::Ordering::Relaxed),
            system: None,
            profiles,
            viewport: egui::ViewportId::from_hash_of(viewport),
            dragging: None,
            title: title.into(),
            copy_on_select: setting("term_copy_select").is_some_and(|v| v == "1"),
            renaming: None,
            pending: Vec::new(),
            framed: false,
        }
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
            Launch::Local(argv) | Launch::Tool(argv) => {
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
            hold: matches!(profile.launch, Launch::Tool(_)),
            size,
            backend,
            view: View::Terminal,
            search: None,
            preedit: String::new(),
            typed_at: 0.0,
            seek: None,
            nav_keys: true,
            md_heights: Default::default(),
            built_at: 0.0,
            seen_rev: 0,
            collect: Default::default(),
            md_skip: 0,
            md_copy: false,
            md_offset: 0.0,
            md_end: 0.0,
            find: None,
            md_bottom: true,
            md_laid: 0,
            changed_at: 0.0,
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
            t.collect.saw(data);
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

    /// Output read and replies sent while the pane is not drawn (a tab in the background), so
    /// a build there goes on and its shell does not stall on a full pipe.
    pub fn background(&mut self, ctx: &egui::Context) -> Vec<Cmd> {
        let mut out = Vec::new();
        if self.before(ctx, &mut out) {
            self.after(ctx, &mut out);
        }
        out
    }

    /// How much output the shells have taken in (for tests of panes not drawn).
    #[cfg(test)]
    pub(crate) fn output_seen(&self) -> u64 {
        self.tabs.iter().map(|t| t.rev).sum()
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
            self.open_tab();
        }
        for (p, split) in std::mem::take(&mut self.pending) {
            self.start(ctx, p, split, out);
        }
        // Alt+arrows move between the panes of a tab when it has more than one; with one they are
        // the shell's (a word left or right).
        let groups: Vec<u32> = self.tabs.iter().map(|t| t.group).collect();
        for t in &mut self.tabs {
            t.nav_keys = !self.bare && groups.iter().filter(|&&g| g == t.group).count() > 1;
        }
        // Output of local programs.
        for t in &mut self.tabs {
            if let Backend::Local(l) = &mut t.backend {
                let (data, exit) = l.drain();
                if !data.is_empty() {
                    t.collect.saw(&data);
                    t.parser.advance(&mut t.term, &data);
                    t.rev += 1;
                }
                if let Some(e) = exit {
                    t.exited = Some(e);
                }
            }
        }
        // Every tab's record of a full-screen program takes in what settled on its screen.
        let now = ctx.input(|i| i.time);
        for t in &mut self.tabs {
            if let Some(at) = collect::tick(t, now) {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((at - now).max(0.02)));
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
                    // A program asking the colours (to pick a light or dark theme) or the size in
                    // pixels gets them, as from any terminal.
                    TermEvent::ColorRequest(i, reply) => {
                        let c = t.term.colors()[i].unwrap_or_else(|| {
                            let c = default_color(i);
                            Rgb { r: c.r(), g: c.g(), b: c.b() }
                        });
                        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: reply(c).into_bytes() }));
                    }
                    TermEvent::TextAreaSizeRequest(reply) => {
                        let font = FontId::monospace(self.font_size);
                        let (cw, ch) = ctx.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
                        let ws = alacritty_terminal::event::WindowSize {
                            num_cols: t.size.0,
                            num_lines: t.size.1,
                            cell_width: cw.round() as u16,
                            cell_height: ch.round() as u16,
                        };
                        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data: reply(ws).into_bytes() }));
                    }
                    _ => {}
                }
            }
            if input::seek_step(t, out) {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            }
        }
        self.route_local(out);
    }

    /// Saves the terminal preferences.
    fn save_settings(&self) {
        crate::hosts::set_setting("term_font", &format!("{}", self.font_size));
        let scheme = SCHEME.load(std::sync::atomic::Ordering::Relaxed);
        crate::hosts::set_setting("term_colors", &if scheme == AUTO { "auto".to_string() } else { scheme.to_string() });
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
        let mut drag_end = false;
        for (gi, &(group, i)) in groups.iter().enumerate() {
            let active = Some(group) == current;
            let next_active = groups.get(gi + 1).is_some_and(|g| Some(g.0) == current);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(tab_w, 34.0), Sense::hover());
            let r = ui.interact(rect, ui.make_persistent_id(("term-tab", group)), Sense::click_and_drag());
            tab_rects.push((group, rect));
            let hovered = r.hovered();
            let p = ui.painter();
            let top_round = CornerRadius { nw: crate::gui::theme::R_MD, ne: crate::gui::theme::R_MD, sw: 0, se: 0 };
            if active {
                p.rect_filled(rect, top_round, scheme_bg());
            } else if hovered {
                p.rect_filled(rect.shrink2(Vec2::new(2.0, 3.0)), CornerRadius::same(crate::gui::theme::R_SM), hover_fill);
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
                    ui.painter().rect_filled(Rect::from_center_size(close_rect.center(), Vec2::splat(20.0)), crate::gui::theme::R_SM, fg.gamma_multiply(0.18));
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
            if r.drag_stopped() && self.dragging == Some(group) {
                drag_end = true;
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
        // A dragged tab moves along the strip.
        if let Some(group) = self.dragging
            && ui.input(|i| i.pointer.primary_down())
            && let Some(p) = ui.ctx().pointer_interact_pos()
            && let Some(&(over, _)) = tab_rects.iter().find(|(g, r)| *g != group && r.x_range().contains(p.x))
        {
            self.move_group(group, over);
        }
        if drag_end {
            self.dragging = None;
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.dragging = None;
        }
        ui.add_space(4.0);
        let (plus, pr) = ui.allocate_exact_size(Vec2::new(32.0, 30.0), Sense::click());
        if pr.hovered() {
            ui.painter().rect_filled(plus.shrink(2.0), CornerRadius::same(crate::gui::theme::R_SM), hover_fill);
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
            crate::gui::menu_look(ui);
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
            self.system_button(ui);
            self.settings_menu(ui);
            ui.add_space(6.0);
            self.view_switch(ui, fold_views, true);
        });
    }

    /// The controls of the active shell, left to right, for a title bar that holds this terminal
    /// as a pane: how it is shown, its settings, the system terminal.
    pub fn controls(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 2.0;
        self.view_switch(ui, false, false);
        self.settings_menu(ui);
        self.system_button(ui);
    }

    fn system_button(&mut self, ui: &mut egui::Ui) {
        if let Some(cmd) = &self.system
            && ui.add(crate::gui::tool_button(ic::ARROW_SQUARE_OUT, false)).on_hover_text("Open this host in the system terminal (ssh)").clicked()
        {
            let _ = spawn_system(cmd);
        }
    }

    /// Text size, colours, copy on select, the cursor, and the shortcuts.
    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        let weak = ui.visuals().text_color().gamma_multiply(0.75);
        let bare = self.bare;
        let font = &mut self.font_size;
        let mut copy_on_select = self.copy_on_select;
        let mut changed = false;
        let gear = egui::Button::new(RichText::new(ic::GEAR).size(14.0).color(weak))
            .frame_when_inactive(false)
            .min_size(Vec2::splat(28.0));
        egui::containers::menu::MenuButton::from_button(gear)
            .config(egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
            .ui(ui, |ui| crate::gui::menu(ui, |ui| {
                ui.set_min_width(240.0);
                ui.label(RichText::new("Appearance").strong());
                changed |= ui.add(egui::Slider::new(font, 8.0..=32.0).step_by(1.0).text("Text size")).changed();
                let current = SCHEME.load(std::sync::atomic::Ordering::Relaxed);
                let names = std::iter::once((AUTO, "Automatic (follows the app theme)"))
                    .chain(SCHEMES.iter().enumerate().map(|(k, sc)| (k, sc.0)));
                for (k, name) in names {
                    if ui.selectable_label(k == current, name).clicked() {
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
                if ui.button(format!("{}  More: fonts, picture, overlay\u{2026}", ic::EYE)).clicked() {
                    crate::gui::look::open(ui.ctx());
                    ui.close();
                }
                ui.separator();
                ui.label(RichText::new("Shortcuts").strong());
                // A pane of the workspace has one shell: tabs and splits are the workspace's.
                let tabs: &[(&str, &str)] = if bare {
                    &[]
                } else {
                    &[
                        ("Ctrl+Shift+T", "New tab"),
                        ("Ctrl+Shift+W", "Close pane or tab"),
                        ("Ctrl+Tab", "Next tab"),
                        ("Alt+Shift+= / -", "Split right / down"),
                        ("Alt+Shift+D", "Split along the longer side"),
                        ("Alt+arrows", "Move between panes"),
                        ("Double-click a tab", "Rename it"),
                    ]
                };
                let common = [
                    ("Ctrl+Shift+F", "Find"),
                    ("Ctrl+Shift+C / V", "Copy / paste"),
                    ("Ctrl+Insert / Shift+Insert", "Copy / paste"),
                    ("Ctrl+= / Ctrl+-", "Text size"),
                    ("Ctrl+click", "Open a link"),
                ];
                for (k, v) in tabs.iter().chain(common.iter()) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(*k).monospace().size(12.0));
                        ui.label(RichText::new(*v).weak().size(12.0));
                    });
                }
            }));
        self.copy_on_select = copy_on_select;
        if changed {
            self.save_settings();
        }
    }

    /// How the active shell is shown: terminal, text or Markdown, three icon buttons (the current
    /// one marked), or one button with a menu when there is no room. `rtl` when laid out from the
    /// right.
    fn view_switch(&mut self, ui: &mut egui::Ui, fold: bool, rtl: bool) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let mut views = [
            (View::Terminal, ic::TERMINAL, "Terminal", "Terminal"),
            (View::Text, ic::TEXT_T, "Text", "Plain text of the whole buffer, to select and copy"),
            (View::Markdown, ic::ARTICLE, "Markdown", "Read the output as Markdown, with copy buttons on code (agent replies)"),
        ];
        if rtl {
            views.reverse();
        }
        let mut pick = None;
        if fold {
            let glyph = views.iter().find(|v| v.0 == t.view).map_or(ic::TERMINAL, |v| v.1);
            egui::containers::menu::MenuButton::from_button(crate::gui::tool_button(glyph, false))
                .ui(ui, |ui| {
                    crate::gui::menu_look(ui);
                    ui.set_min_width(160.0);
                    for &(v, glyph, name, tip) in &views {
                        let b = egui::Button::selectable(t.view == v, format!("{glyph}  {name}"));
                        if ui.add(b).on_hover_text(tip).clicked() {
                            pick = Some(v);
                            ui.close();
                        }
                    }
                })
                .0
                .on_hover_text("How this shell is shown");
        } else {
            for &(v, glyph, _, tip) in &views {
                if ui.add(crate::gui::tool_button(glyph, t.view == v)).on_hover_text(tip).clicked() {
                    pick = Some(v);
                }
            }
        }
        if let Some(v) = pick {
            t.view = v;
            if v == View::Terminal {
                t.forget_reading();
            }
        }
    }

    fn body(&mut self, ui: &mut egui::Ui, out: &mut Vec<Cmd>) {
        // A tab strip as in Windows Terminal or a browser: the active tab joins the terminal below.
        // A window of its own has it in its title bar, beside the window's buttons.
        if self.bare {
        } else if self.framed {
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
                egui::Frame::new().fill(crate::gui::chrome::bar_fill(ui.visuals())).inner_margin(egui::Margin { left: 8, right: 6, top: 6, bottom: 0 });
            egui::Panel::top("term-tabs").frame(strip).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
                ui.horizontal(|ui| self.strip(ui, out));
            });
        }
        // Keyboard shortcuts of the window itself, as in other terminals: only while it has the
        // keys (beside a remote screen that has them, Ctrl+Shift+W is the host's).
        let shortcuts = self.renaming.is_none() && self.keys_on;
        // Tabs and splits of a pane are the workspace's; text size stays the pane's.
        let tabs_keys = !self.bare;
        let (new_tab, close_tab, next, bigger, smaller, back) = ui.ctx().input(|i| {
            if !shortcuts {
                return (false, false, false, false, false, false);
            }
            let cs = i.modifiers.ctrl && i.modifiers.shift;
            (
                tabs_keys && cs && i.key_pressed(Key::T),
                tabs_keys && cs && i.key_pressed(Key::W),
                tabs_keys && i.modifiers.ctrl && i.key_pressed(Key::Tab),
                i.modifiers.ctrl && !i.modifiers.alt && (i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus)),
                i.modifiers.ctrl && !i.modifiers.alt && i.key_pressed(Key::Minus),
                // Ctrl+Shift+Tab goes the other way.
                i.modifiers.shift,
            )
        });
        let (split, step) = ui.ctx().input(|i| {
            if !shortcuts || !tabs_keys {
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
            let to = if back { (at + groups.len() - 1) % groups.len() } else { (at + 1) % groups.len() };
            self.active = groups[to].1;
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
        // A size set in Appearance comes to every terminal.
        let size_now = TERM_SIZE_GEN.load(std::sync::atomic::Ordering::Relaxed);
        if size_now != self.size_gen {
            self.size_gen = size_now;
            self.font_size = f32::from_bits(TERM_SIZE.load(std::sync::atomic::Ordering::Relaxed)).clamp(8.0, 32.0);
        }
        let frame = egui::Frame::new().fill(crate::gui::look::fill(scheme_bg())).inner_margin(6.0);
        let copy_on_select = self.copy_on_select;
        // A dialog or an open menu takes the keys; the terminal gets them back when it closes.
        // A pane's shell that ended well closes its pane, as Windows Terminal does by default; one
        // that failed stays, its message there to read.
        if self.bare
            && let Some(i) = self.tabs.iter().position(|t| !t.hold && t.exited.as_deref().is_some_and(|e| e.ends_with("code 0")))
        {
            self.close_pane(i, out);
        }
        if let Some(t) = self.tabs.first_mut()
            && let Some(v) = self.start_view.take()
        {
            t.view = match v.as_str() {
                "text" => View::Text,
                "markdown" => View::Markdown,
                _ => View::Terminal,
            };
        }
        let keys = self.keys_on && self.renaming.is_none() && !ui.ctx().any_popup_open();
        let mut asks: Vec<(usize, PaneAsk)> = Vec::new();
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            // The background, with the picture chosen in Appearance behind the text.
            crate::gui::look::backdrop(ui, ui.max_rect().expand(6.0), scheme_bg());
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
mod tests;

#[cfg(test)]
mod speed {
    use crate::gui::snapshot::RunTest;
    use super::*;

    /// Time to rebuild the reading views from a full scrollback, whole and after new output:
    /// cargo test --release -- --ignored --nocapture rebuild_cost
    #[test]
    #[ignore]
    fn rebuild_cost() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))), ..Default::default() };
        let _ = ctx.run_test(input.clone(), |ui| { w.embedded(ui); });
        let _ = ctx.run_test(input, |ui| { w.embedded(ui); });
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
    use crate::gui::snapshot::RunTest;
    use super::*;

    fn tab_with(w: &mut TermWindow, ctx: &egui::Context) {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))), ..Default::default() };
        let _ = ctx.run_test(input.clone(), |ui| { w.embedded(ui); });
        let _ = ctx.run_test(input, |ui| { w.embedded(ui); });
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
