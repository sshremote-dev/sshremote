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

    /// Keeps only the shells running on this computer (a local shell, `ssh HOST`): those of an
    /// agent end with its connection. Whether any is left.
    pub fn keep_local(&mut self) -> bool {
        self.tabs.retain(|t| matches!(t.backend, Backend::Local(_)));
        self.active = 0;
        !self.tabs.is_empty()
    }

    /// A terminal of one shell, `profile`, to show inside another window (a workspace pane).
    pub fn single(profile: Profile) -> Self {
        let mut w = Self::standalone(profile, Vec::new());
        w.standalone = false;
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
        for t in &mut self.tabs {
            t.nav_keys = !self.bare;
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
        let mut drag_end: Option<(u32, Pos2)> = None;
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
        // Keyboard shortcuts of the window itself, as in other terminals.
        let shortcuts = self.renaming.is_none();
        // Tabs and splits of a pane are the workspace's; text size stays the pane's.
        let tabs_keys = !self.bare;
        let (new_tab, close_tab, next, bigger, smaller) = ui.ctx().input(|i| {
            if !shortcuts {
                return (false, false, false, false, false);
            }
            let cs = i.modifiers.ctrl && i.modifiers.shift;
            (
                tabs_keys && cs && i.key_pressed(Key::T),
                tabs_keys && cs && i.key_pressed(Key::W),
                tabs_keys && i.modifiers.ctrl && i.key_pressed(Key::Tab),
                i.modifiers.ctrl && !i.modifiers.alt && (i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus)),
                i.modifiers.ctrl && !i.modifiers.alt && i.key_pressed(Key::Minus),
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
            && let Some(i) = self.tabs.iter().position(|t| t.exited.as_deref().is_some_and(|e| e.ends_with("code 0")))
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

    /// Syntax-coloured code is fenced with its indentation, and a frame drawn right above the
    /// prompt (as an agent's input box) stays with the prompt in the live rows.
    #[test]
    fn coloured_code_and_framed_prompts() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        let cols = t.term.grid().columns();
        let kw = "\x1b[38;5;170m";
        let st = "\x1b[38;5;114m";
        let r = "\x1b[0m";
        let out = format!(
            "\x1b[2J\x1b[HSome text\r\n\r\n  {kw}def{r} f(x):\r\n      {kw}return{r} {st}\"x\"{r}\r\n\r\n  after\r\n{}\r\n> \r\n{}\r\n  status\x1b[2A\r\x1b[2C",
            "\u{2500}".repeat(cols),
            "\u{2500}".repeat(cols)
        );
        t.parser.advance(&mut t.term, out.as_bytes());
        let (first, last) = reader::input_rows(t);
        let md = reader::screen_markdown(t, 0, first);
        assert!(md.contains("  ```\n  def f(x):\n      return \"x\"\n  ```"), "{md}");
        assert!(md.contains("after"), "{md}");
        assert!(!md.contains('\u{2500}'), "the frame goes with the prompt: {md}");
        assert_eq!(last - first, 3, "rule, prompt, rule, status");
    }

    /// Code as Claude Code colours it stays one block: rows left in the plain colour ("Empty,",
    /// "Ok(x)", "}") join it, the hint it floats over its text is dropped, and a coloured prompt
    /// marker or a heading does not make code.
    #[test]
    fn claude_code_blocks_stay_whole() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        let kw = "\x1b[38;5;170m";
        let ty = "\x1b[38;5;75m";
        let mk = "\x1b[38;5;244m";
        let r = "\x1b[0m";
        let out = format!(
            "\x1b[2J\x1b[H{mk}\u{276F}{r} {ty}explain errors{r}\r\n\r\n\x1b[1mError Handling{r}\r\n\r\n  {kw}use{r} std::fmt;\r\n\r\n  {kw}enum{r} {ty}E{r} {{\r\n      Empty,\r\n      {ty}Bad{r}({ty}String{r}),\r\n  }}\r\n\r\n  {kw}fn{r} {ty}f{r}() -> {ty}R{r} {{\r\n      {kw}let{r} x = {ty}1{r};\r\n      x\r\n      Ok(x)\r\n  }}                Jump to bottom (ctrl+End) \u{2193}\r\n\r\nSummary: a {ty}&self{r} method reads the value and another one changes\r\nit in place.\r\n\r\n{kw}\u{273B}{r} {ty}Thinking\u{2026}{r} (3s \u{B7} {mk}9 tokens{r})\r\ntmux off\r\n\r\n  {kw}use{r} std::io;\r\n"
        );
        t.parser.advance(&mut t.term, out.as_bytes());
        let md = reader::screen_markdown(t, 0, 24);
        assert!(md.contains("\n\u{273B} Thinking\u{2026} (3s \u{B7} 9 tokens)\n\ntmux off"), "a status line is text: {md}");
        assert!(md.contains("      let x = 1;\n      x\n      Ok(x)"), "{md}");
        assert!(md.ends_with("```\n  use std::io;\n  ```"), "a lone first row of code: {md}");
        assert!(!md.contains("Jump to bottom"), "{md}");
        assert_eq!(md.matches("```").count(), 4, "one block, and the lone row: {md}");
        assert!(md.contains("```\n  use std::fmt;\n\n  enum E {\n      Empty,"), "{md}");
        assert!(md.contains("      Ok(x)\n  }\n  ```"), "{md}");
        assert!(md.starts_with("\u{276F} explain errors"), "{md}");
        assert!(md.contains("# Error Handling"), "{md}");
        assert!(md.contains("```\n\nSummary: a &self method reads the value and another one changes\nit in place."), "{md}");
    }

    /// Draws `rows` as a full-screen program's screen (one per row, the rest blank).
    fn draw(t: &mut Tab, rows: &[String]) {
        let n = t.term.grid().screen_lines();
        let mut out = String::new();
        for r in 0..n {
            out.push_str(&format!("\x1b[{};1H\x1b[2K{}", r + 1, rows.get(r).map_or("", String::as_str)));
        }
        t.parser.advance(&mut t.term, out.as_bytes());
        t.rev += 1;
    }

    /// The record's rows as plain text.
    fn record(t: &Tab) -> Vec<String> {
        t.collect.rows().into_iter().map(|k| k.plain).collect()
    }

    /// Ticks the record at `now` and once the screen has settled.
    fn settle(t: &mut Tab, now: &mut f64) {
        collect::tick(t, *now);
        *now += collect::STABLE + 0.1;
        collect::tick(t, *now);
        *now += 0.1;
    }

    fn alt_tab() -> (TermWindow, usize) {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, b"\x1b[?1049h");
        t.rev += 1;
        let n = t.term.grid().screen_lines();
        (w, n)
    }

    /// Screens of a program scrolled down and back up go into one record, in order, each row once;
    /// a screen too far to place goes after it.
    #[test]
    fn the_record_gathers_scrolled_screens_in_order() {
        let (mut w, n) = alt_tab();
        let t = &mut w.tabs[0];
        let line = |i: usize| format!("line {i:03} of the file");
        let page = |k: usize| (k..k + n).map(line).collect::<Vec<_>>();
        let mut now = 0.0;
        for k in [0, 5, 9, 3, 0, 12] {
            draw(t, &page(k));
            settle(t, &mut now);
        }
        assert_eq!(record(t), (0..12 + n).map(line).collect::<Vec<_>>(), "in order, each once");
        // Scrolled too far at once: after the record, apart.
        draw(t, &page(200));
        settle(t, &mut now);
        let r = record(t);
        assert_eq!(r[..12 + n], (0..12 + n).map(line).collect::<Vec<_>>()[..]);
        assert_eq!(r[12 + n], "", "a gap");
        assert_eq!(r[13 + n..], page(200)[..]);
        // The program leaves: the record goes.
        t.parser.advance(&mut t.term, b"\x1b[?1049l");
        t.rev += 1;
        settle(t, &mut now);
        assert!(t.collect.is_empty(), "a new record when the program leaves");
    }

    /// An answer that appears at once, longer than the screen, shows only its end; scrolling up
    /// to its start joins it with the question in one document, each row once.
    #[test]
    fn pieces_of_the_record_join_up_as_it_is_scrolled() {
        let (mut w, n) = alt_tab();
        let t = &mut w.tabs[0];
        let mut doc: Vec<String> = vec!["Banner".into(), "version 1".into(), String::new(), "\u{276F} the question".into(), String::new()];
        doc.extend((0..3 * n).map(|i| format!("answer row {i}")));
        let mut now = 0.0;
        // The question, then the whole answer at once: the screen shows its end.
        draw(t, &doc[..4]);
        settle(t, &mut now);
        let len = doc.len();
        draw(t, &doc[len - n..]);
        settle(t, &mut now);
        // Scrolled up a few rows at a time to the top.
        let mut s = len - n;
        while s > 0 {
            s = s.saturating_sub(4);
            // Scrolled back, the agent covers its last row with a hint.
            let mut page = doc[s..s + n].to_vec();
            page[n - 1] = "                Jump to bottom (ctrl+End) \u{2193}".into();
            draw(t, &page);
            settle(t, &mut now);
        }
        assert_eq!(record(t), doc, "one document, each row once");
    }

    /// A line that keeps changing (an agent's spinner) and the agent's input box stay out; a
    /// redraw from the top with room below (as after /clear) starts the record again.
    #[test]
    fn the_record_leaves_out_live_lines_and_starts_again_when_cleared() {
        let (mut w, n) = alt_tab();
        let t = &mut w.tabs[0];
        let cols = t.term.grid().columns();
        let rule = "\u{2500}".repeat(cols);
        let mut now = 0.0;
        let screen = |answer: usize, k: usize| {
            let mut rows: Vec<String> = vec!["Banner of the agent".into(), "version 1".into(), String::new()];
            rows.extend((0..answer).map(|i| format!("answer line {i}")));
            rows.push(format!("\u{273B} Thinking\u{2026} ({k}s \u{B7} {} tokens)", k * 7));
            rows.resize(n - 4, String::new());
            rows.extend([rule.clone(), "\u{276F} typing here".into(), rule.clone(), "  status".into()]);
            rows
        };
        for k in 0..6 {
            draw(t, &screen(k, k));
            settle(t, &mut now);
        }
        let r = record(t);
        assert_eq!(r.first().map(String::as_str), Some("Banner of the agent"));
        assert!(r.iter().any(|l| l == "answer line 4"), "{r:?}");
        assert!(!r.iter().any(|l| l.contains("Thinking") || l.contains("typing") || l.contains('\u{2500}')), "{r:?}");
        // /clear: the banner and the command, nothing below.
        let mut cleared: Vec<String> = vec!["Banner of the agent".into(), "version 1".into(), String::new(), "\u{276F} /clear".into()];
        cleared.resize(n - 4, String::new());
        cleared.extend([rule.clone(), "\u{276F} ".into(), rule.clone(), "  status".into()]);
        // The command's row comes a moment after the rest: it is still taken once it settles.
        let mut first = cleared.clone();
        first[3] = String::new();
        draw(t, &first);
        collect::tick(t, now);
        now += 0.3;
        draw(t, &cleared);
        collect::tick(t, now);
        now += 0.3;
        collect::tick(t, now);
        settle(t, &mut now);
        assert_eq!(record(t), vec!["Banner of the agent", "version 1", "", "\u{276F} /clear"]);
    }

    /// The Markdown view shows the record, its menu copies it, and clearing empties it.
    #[test]
    fn the_markdown_view_shows_copies_and_clears_the_record() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        let mut clock = 0.0;
        let mut tick = |w: &mut TermWindow, events: Vec<Event>| {
            clock += 0.2;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
                focused: true,
                time: Some(clock),
                events,
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                w.embedded(ui);
            })
        };
        tick(&mut w, Vec::new());
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, b"\x1b[?1049h");
        draw(t, &["# Title".into(), "Some words.".into()]);
        w.tabs[0].view = View::Markdown;
        for _ in 0..6 {
            tick(&mut w, Vec::new());
        }
        let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
        assert!(text.contains("Some words."), "{text:?}");
        // Right click: the menu; "Copy all as Markdown" copies the record.
        let at = Pos2::new(400.0, 300.0);
        let click = |button, pressed| Event::PointerButton { pos: at, button, pressed, modifiers: Default::default() };
        tick(&mut w, vec![Event::PointerMoved(at), click(egui::PointerButton::Secondary, true)]);
        tick(&mut w, vec![click(egui::PointerButton::Secondary, false)]);
        let item = tick(&mut w, Vec::new());
        let find = |out: &egui::FullOutput, what: &str| {
            let mut found = None;
            for c in &out.shapes {
                if let egui::Shape::Text(s) = &c.shape
                    && s.galley.job.text.contains(what)
                {
                    found = Some(s.galley.rect.translate(s.pos.to_vec2()).center());
                }
            }
            found
        };
        let copy_at = find(&item, "Copy all as Markdown").expect("the menu shows");
        let press = |pressed| Event::PointerButton { pos: copy_at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        tick(&mut w, vec![Event::PointerMoved(copy_at), press(true)]);
        let out = tick(&mut w, vec![press(false)]);
        let copied = out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s.contains("Some words.")));
        assert!(copied, "the record is copied");
        // The clear button empties it, and what is on the screen does not come back by itself.
        let shown = tick(&mut w, Vec::new());
        let clear_at = find(&shown, ic::ERASER).expect("the clear button");
        tick(&mut w, vec![Event::PointerMoved(clear_at), Event::PointerButton { pos: clear_at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
        tick(&mut w, vec![Event::PointerButton { pos: clear_at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
        for _ in 0..6 {
            tick(&mut w, Vec::new());
        }
        assert!(w.tabs[0].collect.is_empty(), "cleared");
        // Ctrl+F opens the search, which finds the words.
        let t = &mut w.tabs[0];
        draw(t, &["# Title".into(), "Other words here.".into()]);
        for _ in 0..6 {
            tick(&mut w, Vec::new());
        }
        tick(&mut w, vec![Event::Key { key: Key::F, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }]);
        tick(&mut w, vec![Event::Text("other".into())]);
        tick(&mut w, Vec::new());
        let out = tick(&mut w, Vec::new());
        assert!(find(&out, "1 of 1").is_some(), "the search finds it");
    }

    /// Python as Claude Code colours it: comments, imports with one colour and functions two
    /// blank lines apart stay one block; a bold heading and a numbered list stay text.
    #[test]
    fn python_blocks_stay_whole() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        let (kw, st, cm, r) = ("\x1b[38;5;170m", "\x1b[38;5;114m", "\x1b[38;5;244m", "\x1b[0m");
        let out = format!(
            "\x1b[2J\x1b[H\x1b[1mWord Count{r}\r\n\r\n  1. {kw}Read{r} the file.\r\n\r\n  {st}\"\"\"Count words.\"\"\"{r}\r\n\r\n  {kw}import{r} sys\r\n  {kw}from{r} pathlib {kw}import{r} Path\r\n\r\n  {cm}# Words shorter than this are skipped.{r}\r\n  MIN = {st}3{r}\r\n\r\n\r\n  {kw}def{r} {st}read{r}(path):\r\n      {cm}# Lowercase it.{r}\r\n      {kw}return{r} path\r\n\r\nThat is all.\r\n"
        );
        t.parser.advance(&mut t.term, out.as_bytes());
        let md = reader::screen_markdown(t, 0, 20);
        assert_eq!(md.matches("```").count(), 2, "one block: {md}");
        assert!(md.starts_with("## Word Count\n\n  1. Read the file.\n\n  ```\n  \"\"\"Count words.\"\"\""), "{md}");
        assert!(md.contains("  # Words shorter than this are skipped.\n  MIN = 3\n\n\n  def read(path):\n      # Lowercase it.\n      return path\n  ```\n\nThat is all."), "{md}");
    }

    /// The reading views follow the window: the terminal takes their size, the program is told,
    /// and the document is made again from the rows as they wrap at the new width.
    #[test]
    fn reading_views_follow_the_window_size() {
        // The Markdown view is read, not typed into: the terminal keeps the size the others give it.
        {
            let view = View::Text;
            let ctx = egui::Context::default();
            let mut w = TermWindow::new(None);
            let mut clock = 0.0;
            let mut at = |w: &mut TermWindow, width: f32| {
                clock += 0.2;
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 500.0))),
                    time: Some(clock),
                    ..Default::default()
                };
                let mut cmds = Vec::new();
                let _ = ctx.run_ui(input, |ui| cmds.extend(w.embedded(ui)));
                cmds
            };
            at(&mut w, 900.0);
            w.tabs[0].view = view;
            let t = &mut w.tabs[0];
            t.parser.advance(&mut t.term, "word ".repeat(60).as_bytes());
            t.rev += 1;
            at(&mut w, 900.0);
            at(&mut w, 900.0);
            let wide = w.tabs[0].size.0;
            let cmds = at(&mut w, 500.0);
            let narrow = w.tabs[0].size.0;
            assert!(narrow < wide, "{view:?}: the terminal narrows with the view ({wide} to {narrow})");
            let told = cmds.iter().any(|c| matches!(c, Cmd::Send(ToAgent::TermResize { cols, .. }) if *cols == narrow));
            assert!(told, "{view:?}: the program is told the new size");
            at(&mut w, 500.0);
            // The rows wrap at the new width and the document is made from them.
            let rows_now = w.tabs[0].term.grid().columns();
            assert_eq!(rows_now, narrow as usize, "{view:?}");
            let longest = match view {
                View::Text => w.tabs[0].editor.as_ref().unwrap().1.0.lines().map(|l| l.chars().count()).max().unwrap(),
                _ => reader::screen_markdown(&w.tabs[0], 0, 10).lines().map(|l| l.chars().count()).max().unwrap(),
            };
            assert!(longest <= narrow as usize + 1 || view == View::Markdown, "{view:?}: lines fit {narrow}: {longest}");
        }
    }

    /// Rows the terminal wraps again at a new width still read as the sentence they were, with
    /// every space between words wherever the break falls.
    #[test]
    fn rewrapped_rows_keep_their_spaces() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        t.term.resize(Size { cols: 120, rows: 20 });
        let line = "(or `mcp --here` for this computer) lets an AI agent read the window in front as text";
        t.parser.advance(&mut t.term, format!("{line}\r\n").as_bytes());
        for cols in (20..=110).rev() {
            t.term.resize(Size { cols, rows: 20 });
            let history = (t.term.grid().total_lines() - t.term.grid().screen_lines()) as i32;
            let md = reader::screen_markdown(t, -history, 20);
            let words: Vec<&str> = md.split_whitespace().collect();
            assert_eq!(words.join(" "), line, "at {cols} columns: {md:?}");
        }
    }

    /// The text view shows output that arrives after it opens, as it streams in.
    #[test]
    fn text_view_follows_new_output() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        // Frames closer together than the rebuild interval, as when output streams in.
        let mut clock = 0.0;
        let mut tick = |w: &mut TermWindow| {
            clock += 0.04;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
                focused: true,
                time: Some(clock),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                w.embedded(ui);
            });
        };
        tick(&mut w);
        w.tabs[0].view = View::Text;
        tick(&mut w);
        for k in 0..5 {
            let t = &mut w.tabs[0];
            t.parser.advance(&mut t.term, format!("streamed line {k}\r\n").as_bytes());
            t.rev += 1;
            tick(&mut w);
        }
        for _ in 0..5 {
            tick(&mut w);
        }
        let doc = &w.tabs[0].editor.as_ref().expect("built").1.0;
        assert!(doc.contains("streamed line 4"), "{doc:?}");
        // What is drawn: the laid-out text, not only the document behind it.
        let shown = &w.tabs[0].editor_galley.as_ref().expect("laid out").3.text;
        assert!(shown.contains("streamed line 4"), "{shown:?}");
    }

    /// A click puts the cursor where it landed: arrow keys in a full-screen program (down first,
    /// then across, as the program moves it, not past the text), Left and Right along the shell's
    /// own line only, the click itself to a program that asked for the mouse.
    #[test]
    fn a_click_moves_the_cursor_there() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        let feed = |t: &mut Tab, s: &str| t.parser.advance(&mut t.term, s.as_bytes());
        feed(t, "\x1b[?1049h\x1b[2J\x1b[Hline one\r\nsecond line here\r\nthird\x1b[H");
        let mut out = Vec::new();
        input::click_cursor(t, 1, 7, &mut out);
        assert_eq!(typed(&out), "\x1b[B", "down first");
        out.clear();
        assert!(input::seek_step(t, &mut out), "waits for the program");
        assert!(out.is_empty());
        feed(t, "\x1b[2;1H");
        assert!(input::seek_step(t, &mut out));
        assert_eq!(typed(&out), "\x1b[C".repeat(7), "then across");
        out.clear();
        feed(t, "\x1b[2;8H");
        assert!(!input::seek_step(t, &mut out), "there");
        assert!(out.is_empty());
        // Past the end of a row: to its end, not on into the next line.
        input::click_cursor(t, 1, 40, &mut out);
        assert_eq!(typed(&out), "\x1b[C".repeat(9));
        out.clear();
        // The shell: along its line only.
        feed(t, "\x1b[?1049l\x1b[2J\x1b[Hold output\r\n$ echo hello");
        t.seek = None;
        input::click_cursor(t, 1, 3, &mut out);
        assert_eq!(typed(&out), "\x1b[D".repeat(9), "left along the line");
        out.clear();
        t.seek = None;
        input::click_cursor(t, 0, 3, &mut out);
        assert!(out.is_empty() && t.seek.is_none(), "not into the output above");
        // A program that asked for the mouse gets the click.
        feed(t, "\x1b[?1000h\x1b[?1006h");
        input::click_cursor(t, 1, 7, &mut out);
        assert_eq!(typed(&out), "\x1b[<0;8;2M\x1b[<0;8;2m");
        w.close_all();
    }

    /// Claude Code run in a pane here, shown in the Markdown view while it answers and while
    /// the answer is scrolled, as pictures to look at (it costs a request):
    /// SSHREMOTE_SNAP_DIR=folder SSHREMOTE_CLAUDE=1 cargo test --release markdown_view_with_claude -- --ignored
    #[test]
    #[ignore]
    fn markdown_view_with_claude_code() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        if std::env::var_os("SSHREMOTE_CLAUDE").is_none() {
            return;
        }
        let prompt = std::env::var("SSHREMOTE_CLAUDE_PROMPT").unwrap_or_else(|_| {
            "Without using any tools, answer in Markdown: a heading, two paragraphs, a bulleted list of five items, a table of four rows, and a 40-line Rust example in a code block, then a short summary."
                .into()
        });
        // Claude Code starts in SSHREMOTE_CLAUDE_DIR (the current folder when unset), the prompt one argument.
        let dir_arg = std::env::var("SSHREMOTE_CLAUDE_DIR").unwrap_or_else(|_| ".".into());
        let profile = Profile { name: "claude".into(), icon: ic::TERMINAL, launch: Launch::Local(vec!["cmd".into(), "/c".into(), "cd".into(), "/d".into(), dir_arg, "&".into(), "claude".into(), prompt]) };
        let mut w = TermWindow::single(profile);
        let ctx = egui::Context::default();
        crate::gui::style(&ctx);
        let size = Vec2::new(1100.0, 760.0);
        let mut snap = crate::gui::snapshot::Snap::default();
        let mut clock = 0.0;
        let mut frame = |w: &mut TermWindow, snap: &mut crate::gui::snapshot::Snap, events: Vec<Event>| {
            clock += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                focused: true,
                time: Some(clock),
                events,
                ..Default::default()
            };
            snap.frame(&ctx, input, |ui| {
                w.embedded(ui);
            });
        };
        let secs: usize = std::env::var("SSHREMOTE_CLAUDE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
        for k in 0..secs * 20 {
            if k == 60 {
                w.tabs[0].view = View::Markdown;
            }
            frame(&mut w, &mut snap, Vec::new());
            std::thread::sleep(std::time::Duration::from_millis(50));
            if k % 40 == 39 {
                snap.save(&ctx, size, &dir.join(format!("claude_md_{:03}.png", k / 20 + 1)));
            }
        }
        // Scrolled up a little at a time, as a wheel does, then back down.
        let wheel = |dy: f32| Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, dy), modifiers: Default::default(), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
        frame(&mut w, &mut snap, vec![Event::PointerMoved(Pos2::new(500.0, 300.0))]);
        // In real time, as a hand on the wheel: a notch every 100 ms, every frame recorded with
        // the document it showed.
        let mut log = String::new();
        let mut n = 0;
        for (dir_dy, notches) in [(60.0, 30), (-60.0, 20)] {
            for k in 0..notches * 2 {
                let events = if k % 2 == 0 { vec![wheel(dir_dy)] } else { Vec::new() };
                frame(&mut w, &mut snap, events);
                std::thread::sleep(std::time::Duration::from_millis(50));
                let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
                log.push_str(&format!("===== frame {n} wheel {}\n{text}\n", if k % 2 == 0 { dir_dy } else { 0.0 }));
                snap.save(&ctx, size, &dir.join(format!("claude_md_scroll_{n:03}.png")));
                n += 1;
            }
        }
        std::fs::write(dir.join("scroll_frames.txt"), log).unwrap();
        // /clear typed in the text view: the screen is cleared.
        w.tabs[0].view = View::Text;
        frame(&mut w, &mut snap, vec![Event::Text("/clear".into())]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        frame(&mut w, &mut snap, vec![Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        for _ in 0..60 {
            frame(&mut w, &mut snap, Vec::new());
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        snap.save(&ctx, size, &dir.join("claude_cleared.png"));
        w.tabs[0].view = View::Markdown;
        for _ in 0..30 {
            frame(&mut w, &mut snap, Vec::new());
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        snap.save(&ctx, size, &dir.join("claude_md_cleared.png"));
        let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
        let screen = reader::screen_text(&w.tabs[0], 0, w.tabs[0].term.grid().screen_lines() as i32 - 1).0;
        std::fs::write(dir.join("cleared.md"), format!("{text}\n----\n{}\n----\n{screen}", collect::debug(&w.tabs[0]))).unwrap();
        w.tabs[0].view = View::Text;
        for _ in 0..10 {
            frame(&mut w, &mut snap, Vec::new());
        }
        snap.save(&ctx, size, &dir.join("claude_text.png"));
        let _ = w.close_all();
    }

    /// nano on the VM in the Markdown view: scrolling through a file with the wheel gathers it, in
    /// order and once each line; leaving nano starts the record again. Saves pictures and the
    /// record to SSHREMOTE_SNAP_DIR; needs SSHREMOTE_NANO=1 and `ssh ubuntu` working.
    #[test]
    #[ignore]
    fn markdown_view_with_nano() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        if std::env::var_os("SSHREMOTE_NANO").is_none() {
            return;
        }
        let file = std::env::var("SSHREMOTE_NANO_FILE").unwrap_or_else(|_| "/home/a/sshremote-src/README.md".into());
        let launch = vec!["ssh".into(), "-t".into(), "ubuntu".into(), "nano".into(), "-v".into(), file];
        let profile = Profile { name: "nano".into(), icon: ic::TERMINAL, launch: Launch::Local(launch) };
        let mut w = TermWindow::single(profile);
        let ctx = egui::Context::default();
        crate::gui::style(&ctx);
        let size = Vec2::new(1100.0, 760.0);
        let mut snap = crate::gui::snapshot::Snap::default();
        let mut clock = 0.0;
        let mut frame = |w: &mut TermWindow, snap: &mut crate::gui::snapshot::Snap, events: Vec<Event>| {
            clock += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                focused: true,
                time: Some(clock),
                events,
                ..Default::default()
            };
            snap.frame(&ctx, input, |ui| {
                w.embedded(ui);
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        for _ in 0..80 {
            frame(&mut w, &mut snap, Vec::new());
        }
        snap.save(&ctx, size, &dir.join("nano_text.png"));
        w.tabs[0].view = View::Markdown;
        for _ in 0..30 {
            frame(&mut w, &mut snap, Vec::new());
        }
        snap.save(&ctx, size, &dir.join("nano_md_start.png"));
        let wheel = |dy: f32| Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, dy), modifiers: Default::default(), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
        frame(&mut w, &mut snap, vec![Event::PointerMoved(Pos2::new(500.0, 300.0))]);
        // Down through the file a notch at a time, as a hand does.
        for k in 0..240 {
            frame(&mut w, &mut snap, if k % 2 == 0 { vec![wheel(-60.0)] } else { Vec::new() });
            if k % 60 == 59 {
                snap.save(&ctx, size, &dir.join(format!("nano_md_scroll_{:02}.png", k / 60)));
            }
        }
        for _ in 0..20 {
            frame(&mut w, &mut snap, Vec::new());
        }
        snap.save(&ctx, size, &dir.join("nano_md_end.png"));
        let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
        let plain = w.tabs[0].collect.rows().into_iter().map(|k| k.plain).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("nano_record.md"), &text).unwrap();
        std::fs::write(dir.join("nano_record.txt"), &plain).unwrap();
        // Ctrl+X leaves nano: the record starts again.
        if let Backend::Local(l) = &mut w.tabs[0].backend {
            l.write(b"\x18");
        }
        for _ in 0..40 {
            frame(&mut w, &mut snap, Vec::new());
        }
        snap.save(&ctx, size, &dir.join("nano_md_left.png"));
        assert!(w.tabs[0].collect.is_empty(), "the record goes when nano leaves");
        let _ = w.close_all();
    }

    /// Output that never pauses (a spinner, a reply streaming in) does not rebuild the Markdown
    /// view every frame: it settles first, or catches up now and then; empty code blocks do not
    /// show as boxes.
    #[test]
    fn the_markdown_view_waits_for_output_to_settle() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        let mut clock = 0.0;
        let mut tick = |w: &mut TermWindow| {
            clock += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
                focused: true,
                time: Some(clock),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                w.embedded(ui);
            });
        };
        tick(&mut w);
        w.tabs[0].view = View::Markdown;
        tick(&mut w);
        let built = |w: &TermWindow| w.tabs[0].reading.as_ref().map(|r| std::rc::Rc::as_ptr(&r.1) as usize);
        let mut builds = std::collections::BTreeSet::new();
        // One second of output every frame.
        for k in 0..20 {
            let t = &mut w.tabs[0];
            t.parser.advance(&mut t.term, format!("\r\x1b[2K working {k}").as_bytes());
            t.rev += 1;
            tick(&mut w);
            builds.extend(built(&w));
        }
        assert!(builds.len() <= 3, "rebuilt {} times in a second of streaming", builds.len());
        // Then it stops: the last text shows once settled.
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, b"\r\n```\n```\ndone streaming\r\n");
        t.rev += 1;
        for _ in 0..12 {
            tick(&mut w);
        }
        let doc = &w.tabs[0].reading.as_ref().expect("built").1;
        assert!(doc.0.contains("done streaming"), "{:?}", doc.0);
        assert!(!doc.1.blocks.iter().any(|b| matches!(b, crate::markdown::Block::Code { code, .. } if code.trim().is_empty())));
    }

    /// Shift+Delete reaches the windows as a cut (as Ctrl+X does): the shell gets the key that was
    /// pressed, not Ctrl+X.
    #[test]
    fn shift_delete_is_shift_delete() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        frame(&ctx, &mut w, Vec::new());
        let cut = |ctx: &egui::Context, w: &mut TermWindow, modifiers: egui::Modifiers| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
                focused: true,
                events: vec![Event::ModifiersChanged(modifiers), Event::Cut],
                ..Default::default()
            };
            let mut out = Vec::new();
            let _ = ctx.run_ui(input, |ui| out.extend(w.embedded(ui)));
            typed(&out)
        };
        assert_eq!(cut(&ctx, &mut w, egui::Modifiers::SHIFT), "\u{1b}[3;2~");
        assert_eq!(cut(&ctx, &mut w, egui::Modifiers::CTRL), "\u{18}");
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
            // The Markdown view is a record to read and copy: nothing typed there goes to the shell.
            let want = if view == View::Text { "\u{D55C}a" } else { "" };
            assert_eq!(typed(&done), want, "{view:?}");
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
        {
            let view = View::Text;
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

    /// Ctrl+Insert copies the selection without interrupting, and Shift+Insert pastes, as in
    /// GNOME Terminal.
    #[test]
    fn insert_keys_copy_and_paste() {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, b"\x1b[2J\x1b[Hcopy me\r\n");
        let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
        sel.update(Point::new(Line(0), Column(6)), Side::Right);
        t.term.selection = Some(sel);
        let key = |modifiers| Event::Key { key: Key::Insert, physical_key: None, pressed: true, repeat: false, modifiers };
        let input = |events| egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            events,
            ..Default::default()
        };
        let mut cmds = Vec::new();
        let out = ctx.run_ui(input(vec![key(egui::Modifiers::CTRL)]), |ui| cmds.extend(w.embedded(ui)));
        assert_eq!(typed(&cmds), "", "nothing typed");
        assert!(out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s == "copy me")));
        cmds.clear();
        let out = ctx.run_ui(input(vec![key(egui::Modifiers::SHIFT)]), |ui| cmds.extend(w.embedded(ui)));
        assert_eq!(typed(&cmds), "", "nothing typed");
        let pasting = out.viewport_output.values().any(|v| v.commands.contains(&egui::ViewportCommand::RequestPaste));
        assert!(pasting, "Shift+Insert asks for the clipboard");
        let _ = w.close_all();
    }

    /// In the text and Markdown views, Ctrl+C copies text selected with the mouse and sends
    /// nothing to the shell; with nothing selected it interrupts, as in the terminal.
    #[test]
    fn ctrl_c_copies_a_mouse_selection_in_the_reading_views() {
        for view in [View::Text, View::Markdown] {
            let ctx = egui::Context::default();
            let mut w = TermWindow::new(None);
            frame(&ctx, &mut w, Vec::new());
            let t = &mut w.tabs[0];
            t.parser.advance(&mut t.term, b"alpha beta gamma\r\n\r\n$ ");
            t.rev += 1;
            w.tabs[0].view = view;
            let mut shapes = Vec::new();
            for _ in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
                    focused: true,
                    ..Default::default()
                };
                shapes = ctx.run_ui(input, |ui| { w.embedded(ui); }).shapes;
            }
            // Where "alpha beta gamma" was drawn.
            fn find(s: &egui::Shape, out: &mut Option<Rect>) {
                match s {
                    egui::Shape::Text(t) if t.galley.job.text.contains("alpha beta gamma") => {
                        let row = t.galley.rows.iter().find(|r| r.text().contains("alpha")).map(|r| r.rect()).unwrap_or(t.galley.rect);
                        *out = Some(row.translate(t.pos.to_vec2()));
                    }
                    egui::Shape::Vec(v) => v.iter().for_each(|s| find(s, out)),
                    _ => {}
                }
            }
            let mut at = None;
            shapes.iter().for_each(|c| find(&c.shape, &mut at));
            let r = at.unwrap_or_else(|| panic!("{view:?}: the text is drawn"));
            let (from, to) = (Pos2::new(r.left() + 1.0, r.center().y), Pos2::new(r.left() + 60.0, r.center().y));
            let button = |pos, pressed| Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            frame(&ctx, &mut w, vec![Event::PointerMoved(from)]);
            frame(&ctx, &mut w, vec![button(from, true)]);
            frame(&ctx, &mut w, vec![Event::PointerMoved(to)]);
            frame(&ctx, &mut w, vec![button(to, false)]);
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
                focused: true,
                events: vec![Event::Copy],
                ..Default::default()
            };
            let mut cmds = Vec::new();
            let out = ctx.run_ui(input, |ui| cmds.extend(w.embedded(ui)));
            assert_eq!(typed(&cmds), "", "{view:?}: Ctrl+C with a selection does not interrupt");
            let copied: Vec<&String> = out
                .platform_output
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(s) => Some(s),
                    _ => None,
                })
                .collect();
            assert!(copied.iter().any(|s| s.contains("alpha")), "{view:?}: the selection is copied: {copied:?}");
            // Copying ends the selection, so the next Ctrl+C interrupts, as in Windows Terminal; the
            // Markdown view takes no keys, so there it does nothing.
            frame(&ctx, &mut w, Vec::new());
            let want = if view == View::Text { "\u{3}" } else { "" };
            assert_eq!(typed(&frame(&ctx, &mut w, vec![Event::Copy])), want, "{view:?}: the next Ctrl+C");
        }
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
