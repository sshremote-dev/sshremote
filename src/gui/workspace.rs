//! The workspace: tabs of panes split as in Windows Terminal. Each pane shows one connection as a
//! terminal, the remote screen or its files, switched without connecting again; a pane whose
//! connection is lost tries again by itself before asking.

use super::*;

/// What a pane shows of its connection.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PaneMode {
    Terminal,
    Screen,
    Files,
}

impl PaneMode {
    pub(crate) const ALL: [PaneMode; 3] = [PaneMode::Terminal, PaneMode::Screen, PaneMode::Files];

    /// Its name in the settings file.
    pub(crate) fn key(self) -> &'static str {
        match self {
            PaneMode::Terminal => "terminal",
            PaneMode::Screen => "screen",
            PaneMode::Files => "files",
        }
    }

    pub(crate) fn from_key(k: &str) -> Option<PaneMode> {
        PaneMode::ALL.into_iter().find(|m| m.key() == k)
    }

    pub(crate) fn glyph(self) -> &'static str {
        match self {
            PaneMode::Terminal => ic::TERMINAL,
            PaneMode::Screen => ic::MONITOR,
            PaneMode::Files => ic::FOLDER,
        }
    }

    /// Whether its module is on (the terminal always is).
    pub(crate) fn on(self) -> bool {
        match self {
            PaneMode::Terminal => true,
            PaneMode::Screen => crate::modules::on("screen"),
            PaneMode::Files => crate::modules::on("files"),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            PaneMode::Terminal => "Terminal",
            PaneMode::Screen => "Screen sharing",
            PaneMode::Files => "Files",
        }
    }
}

/// What the open view asks for.
pub(super) enum Launch {
    /// A shell, local or over ssh, needing nothing on the host; `target` reaches the agent of the
    /// same machine when the pane turns to its screen or files.
    Shell { profile: crate::termwin::Profile, label: String, target: Target, ssh_cmd: Option<Vec<String>> },
    /// A connection to the agent, showing `mode` first.
    Agent { target: Target, label: String, ssh_cmd: Option<Vec<String>>, mode: PaneMode },
}

/// Where a new pane goes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Place {
    Tab,
    /// Beside the active pane, or below it.
    Split { below: bool },
}

/// What a new pane opens with (--open): the terminal panel, the chat beside the screen, full screen.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) struct PaneStart {
    pub(super) panel: bool,
    pub(super) chat: bool,
    pub(super) fullscreen: bool,
}

/// The command line that opens `target` in the Remote desktop extension's window.
#[cfg(not(feature = "remote"))]
fn remote_args(target: &Target, mode: PaneMode) -> Vec<String> {
    let mut a = vec!["--gui".to_string()];
    match target {
        // This computer's own screen (--local alone is the synthetic test desktop).
        Target::Local { capture } if capture == "synthetic" => a.push("--local".into()),
        Target::Local { .. } => a.push("--here".into()),
        Target::Ssh { host, port, args, .. } => {
            if let Some(p) = port {
                a.extend(["--ssh".into(), "-p".into(), "--ssh".into(), p.to_string()]);
            }
            for x in args {
                a.extend(["--ssh".into(), x.clone()]);
            }
            a.push(host.clone());
        }
    }
    match mode {
        PaneMode::Files => a.extend(["--open".into(), "files".into()]),
        PaneMode::Terminal => a.extend(["--open".into(), "terminal".into()]),
        PaneMode::Screen => {}
    }
    a
}

/// A pane as extensions are told of it: its label, what it shows, and the host.
fn pane_info(p: &Pane) -> serde_json::Value {
    let host = match &p.target {
        Target::Ssh { host, .. } => host.clone(),
        _ => String::new(),
    };
    let mode = match &p.state {
        State::Live(r) => r.mode(),
        _ => p.mode,
    };
    serde_json::json!({ "pane": p.id, "label": p.label, "mode": mode.name(), "host": host })
}

/// What the open window knows a reached connection by: an ID's address, or the host as typed
/// (which is the pane's label).
fn reached_as(target: &Target, label: &str) -> String {
    match target {
        #[cfg(feature = "remote")]
        Target::Direct { addr, .. } => crate::access::address(&addr.id, &addr.relay),
        _ => label.to_string(),
    }
}

/// Pane ids, one count for every window of the program, so extensions told of panes never see one
/// id twice.
static NEXT_PANE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Seconds a lost connection is tried again before the pane asks.
const GIVE_UP: f64 = 30.0;

enum State {
    Shell(Box<crate::termwin::TermWindow>),
    Live(Box<Remote>),
    Down(Down),
}

/// Connecting, or connecting again after the link was lost.
struct Down {
    rx: Option<Pending>,
    since: Instant,
    next: Instant,
    attempt: u32,
    why: String,
    given_up: bool,
    /// A connection that worked before and was lost (rather than a first attempt).
    again: bool,
    /// The sign-in dialog was shown for this failure.
    asked: bool,
}

impl Down {
    fn new(again: bool, why: String) -> Down {
        let now = Instant::now();
        Down { rx: None, since: now, next: now, attempt: 0, why, given_up: false, again, asked: false }
    }
}

struct Pane {
    id: u64,
    label: String,
    target: Target,
    ssh_cmd: Option<Vec<String>>,
    mode: PaneMode,
    /// Shells kept while the connection is made, or made again.
    terms: Option<crate::termwin::TermWindow>,
    /// Asked for at start (--open): the terminal panel and the chat beside the screen, full screen.
    start: PaneStart,
    /// What the user chose in the connection that dropped, for the next one.
    prefs: Option<remote::Prefs>,
    /// A password typed in the sign-in dialog, kept in the password store once it works.
    keep: Option<(String, String)>,
    state: State,
}

/// Panes laid out in a tab: each split holds two parts, side by side or one below the other.
enum Node {
    Leaf(u64),
    Split { below: bool, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Node {
    fn leaves(&self, out: &mut Vec<u64>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    fn split(&mut self, at: u64, new: u64, below: bool) -> bool {
        match self {
            Node::Leaf(id) if *id == at => {
                *self = Node::Split { below, ratio: 0.5, a: Box::new(Node::Leaf(at)), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(at, new, below) || b.split(at, new, below),
        }
    }

    /// The layout without `id`, or nothing when it was the last pane.
    fn remove(self, id: u64) -> Option<Node> {
        match self {
            Node::Leaf(x) => (x != id).then_some(Node::Leaf(x)),
            Node::Split { below, ratio, a, b } => match (a.remove(id), b.remove(id)) {
                (Some(a), Some(b)) => Some(Node::Split { below, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            },
        }
    }

    /// How many panes share this part along `below`'s direction: a split the same way counts its
    /// two sides, anything else is one.
    fn weight(&self, below: bool) -> f32 {
        match self {
            Node::Split { below: b, a, b: c, .. } if *b == below => a.weight(below) + c.weight(below),
            _ => 1.0,
        }
    }

    /// The same arrangement with every pane in a row or column the same size: three side by
    /// side are a third each, not a half and two quarters.
    /// Whether every pane in each row and column is already the same size.
    fn is_even(&self) -> bool {
        match self {
            Node::Leaf(_) => true,
            Node::Split { below, ratio, a, b } => {
                let (wa, wb) = (a.weight(*below), b.weight(*below));
                (*ratio - wa / (wa + wb)).abs() < 0.005 && a.is_even() && b.is_even()
            }
        }
    }

    fn balance(&mut self) {
        if let Node::Split { below, ratio, a, b } = self {
            let (wa, wb) = (a.weight(*below), b.weight(*below));
            *ratio = wa / (wa + wb);
            a.balance();
            b.balance();
        }
    }

    /// After `id` was split off along `below`: the row (or column) it is in shares its room
    /// evenly, and every other size set by hand stays.
    fn even_out(&mut self, id: u64, below: bool) {
        // The way down to the pane: each split's direction, and whether it went to the first part.
        fn path(n: &Node, id: u64, out: &mut Vec<(bool, bool)>) -> bool {
            match n {
                Node::Leaf(x) => *x == id,
                Node::Split { below, a, b, .. } => {
                    out.push((*below, true));
                    if path(a, id, out) {
                        return true;
                    }
                    out.last_mut().expect("pushed").1 = false;
                    if path(b, id, out) {
                        return true;
                    }
                    out.pop();
                    false
                }
            }
        }
        let mut steps = Vec::new();
        if !path(self, id, &mut steps) {
            return;
        }
        // The run of same-way splits that ends at the new one starts here.
        let start = steps.iter().rposition(|&(b, _)| b != below).map_or(0, |i| i + 1);
        let mut node = self;
        for &(_, first) in &steps[..start] {
            let Node::Split { a, b, .. } = node else { return };
            node = if first { a } else { b };
        }
        node.even_run(below);
    }

    /// Even shares along `below` for a run of splits that way, leaving the parts inside alone.
    fn even_run(&mut self, below: bool) {
        if let Node::Split { below: b, ratio, a, b: c } = self
            && *b == below
        {
            let (wa, wc) = (a.weight(below), c.weight(below));
            *ratio = wa / (wa + wc);
            a.even_run(below);
            c.even_run(below);
        }
    }

    /// Panes one after another, beside each other or one above the other, all the same size.
    fn even(ids: &[u64], below: bool) -> Node {
        match ids {
            [] => Node::Leaf(0),
            [id] => Node::Leaf(*id),
            [first, rest @ ..] => Node::Split {
                below,
                ratio: 1.0 / ids.len() as f32,
                a: Box::new(Node::Leaf(*first)),
                b: Box::new(Node::even(rest, below)),
            },
        }
    }

    /// The panes in one of the standard arrangements, `main` first where one is bigger.
    fn arranged(ids: &[u64], main: u64, layout: Layout) -> Node {
        let mut ids = ids.to_vec();
        if let Some(at) = ids.iter().position(|&id| id == main)
            && matches!(layout, Layout::MainLeft | Layout::MainTop)
        {
            ids.remove(at);
            ids.insert(0, main);
        }
        match (layout, ids.as_slice()) {
            (_, [] | [_]) => Node::even(&ids, false),
            (Layout::Side, _) => Node::even(&ids, false),
            (Layout::Stacked, _) => Node::even(&ids, true),
            (Layout::MainLeft | Layout::MainTop, [first, rest @ ..]) => {
                let below = layout == Layout::MainTop;
                Node::Split { below, ratio: 0.6, a: Box::new(Node::Leaf(*first)), b: Box::new(Node::even(rest, !below)) }
            }
            (Layout::Grid, _) => {
                let cols = (ids.len() as f32).sqrt().ceil() as usize;
                let rows: Vec<Node> = ids.chunks(cols).map(|r| Node::even(r, false)).collect();
                let mut grid = rows.into_iter().rev().reduce(|below_rows, row| Node::Split {
                    below: true,
                    ratio: 0.5,
                    a: Box::new(row),
                    b: Box::new(below_rows),
                });
                let mut grid = grid.take().unwrap_or(Node::Leaf(0));
                grid.balance();
                grid
            }
            (Layout::Even, _) => Node::even(&ids, false),
        }
    }

    fn swap(&mut self, x: u64, y: u64) {
        match self {
            Node::Leaf(id) if *id == x => *id = y,
            Node::Leaf(id) if *id == y => *id = x,
            Node::Leaf(_) => {}
            Node::Split { a, b, .. } => {
                a.swap(x, y);
                b.swap(x, y);
            }
        }
    }

    /// The rectangle of each pane; the line between two parts can be dragged.
    fn layout(&mut self, ui: &mut egui::Ui, rect: Rect, path: u64, out: &mut Vec<(u64, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split { below, ratio, a, b } => {
                const GAP: f32 = 4.0;
                let (ra, rb, line) = if *below {
                    let y = rect.top() + rect.height() * *ratio;
                    let line = Rect::from_min_max(Pos2::new(rect.left(), y - GAP), Pos2::new(rect.right(), y + GAP));
                    (rect.with_max_y(y - GAP / 2.0), rect.with_min_y(y + GAP / 2.0), line)
                } else {
                    let x = rect.left() + rect.width() * *ratio;
                    let line = Rect::from_min_max(Pos2::new(x - GAP, rect.top()), Pos2::new(x + GAP, rect.bottom()));
                    (rect.with_max_x(x - GAP / 2.0), rect.with_min_x(x + GAP / 2.0), line)
                };
                let r = ui.interact(line, ui.scope_id().with(("pane-split", path)), egui::Sense::drag());
                if r.hovered() || r.dragged() {
                    ui.ctx().set_cursor_icon(if *below {
                        egui::CursorIcon::ResizeVertical
                    } else {
                        egui::CursorIcon::ResizeHorizontal
                    });
                }
                if r.dragged() {
                    let d = if *below { r.drag_delta().y / rect.height() } else { r.drag_delta().x / rect.width() };
                    *ratio = (*ratio + d).clamp(0.05, 0.95);
                }
                a.layout(ui, ra, path * 2 + 1, out);
                b.layout(ui, rb, path * 2 + 2, out);
            }
        }
    }
}

struct Tab {
    root: Node,
    active: u64,
}

/// A closing waiting for a yes: a pane (and its connection), or a whole tab, named by a pane in it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Confirm {
    Pane(u64),
    Tab(u64),
    /// The whole window, with every tab in it.
    Window,
}

/// The arrangements of a tab's panes, as tmux has them.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Layout {
    /// The same arrangement, every pane of a row or column the same size.
    Even,
    Side,
    Stacked,
    /// The active pane big on the left, the others one above the other on the right.
    MainLeft,
    /// The active pane big on top, the others side by side below it.
    MainTop,
    Grid,
}

/// What the workspace tells the app after a frame.
#[derive(Default)]
pub(super) struct Out {
    /// No pane is left: the app goes back to the open window.
    pub empty: bool,
    /// The active pane wants the system keys (Win, Alt+Tab...) for its host.
    pub grab: bool,
}

pub(super) struct Workspace {
    tabs: Vec<Tab>,
    panes: Vec<Pane>,
    current: usize,
    /// The open dialog is asked for: where its choice goes.
    pub(super) picker: Option<Place>,
    opt: Options,
    /// One pane fills the tab, the others wait (a layout, not full screen).
    zoom: Option<u64>,
    /// The window on the whole monitor, and the dock that stands in for its title bar there.
    full: super::dock::FullScreen,
    dock: super::dock::Dock,
    /// Hosts reached, for the recent list.
    pub(super) reached: Vec<String>,
    /// Why something asked for could not open, for the app to show when no tab is left.
    pub(super) failed: Option<String>,
    /// Notices of the workspace itself (an extension's command that failed, say), over the panes.
    toasts: super::toast::Toasts,
    /// A host refused the login: the dialog asking how to sign in.
    signin: Option<super::signin::SignIn>,
    /// The extensions' side panel, and the command palette while open (its filter and choice).
    side: super::extui::Side,
    palette: Option<(String, usize)>,
    /// The panes the extensions were last told of.
    told: Vec<u64>,
    /// The width the active pane's controls took last frame, to know when they must fold.
    controls_w: f32,
    /// The tab being dragged (to a new place, or out into a window of its own).
    dragging: Option<usize>,
    /// Where the dragged tab was grabbed, from its left edge, so it follows the pointer from there.
    grab_dx: f32,
    /// Where the pointer was last while dragging a tab.
    drag_last: Option<Pos2>,
    /// The pane and mode the controls were last measured for (they are measured again when
    /// another pane or mode may fit where these did not).
    measured_for: Option<(u64, PaneMode)>,
    /// The dragged tab is away from this bar: where the pointer is on the screen.
    pub(super) drag_out: Option<Pos2>,
    /// A tab let go away from this bar, and where on the screen: the app moves it into the
    /// window whose tabs it was dropped on, or into a window of its own.
    pub(super) dropped: Option<(usize, Pos2)>,
    /// The tab strip and each tab on the screen last frame, for tabs dropped from other windows.
    pub(super) strip_screen: Rect,
    pub(super) tabs_screen: Vec<Rect>,
    /// Where a tab dragged over from another window would go in: the app sets it each frame.
    pub(super) drop_hint: Option<usize>,
    /// The mode drop-down's width last frame, and whether it shows its icon only.
    mode_w: f32,
    mode_compact: bool,
    title: String,
    /// The first tab shown when they do not all fit, and the wheel not yet a whole step.
    tab_first: usize,
    tab_wheel: f32,
    /// A pane or tab to close once the user says yes.
    confirm: Option<Confirm>,
    /// Where the panes were drawn last frame.
    panes_area: Rect,
    /// The window is closing for good (told yes): nothing else is shown in it meanwhile.
    pub(super) closing_window: bool,
    /// The current tab and room it was last brought into view for.
    scrolled_for: Option<(usize, usize)>,
}

/// A tab's width: as wide as fits, between these; past the narrowest the tabs scroll.
const TAB_MAX: f32 = 220.0;
const TAB_MIN: f32 = 120.0;
/// The + after the tabs.
const PLUS_W: f32 = 34.0;
/// The six layout buttons and their separator.
const LAYOUT_W: f32 = 6.0 * 32.0 + 16.0;

impl Workspace {
    pub(super) fn new(opt: Options) -> Workspace {
        Workspace {
            tabs: Vec::new(),
            panes: Vec::new(),
            current: 0,
            picker: None,
            opt,
            zoom: None,
            full: Default::default(),
            dock: Default::default(),
            reached: Vec::new(),
            failed: None,
            toasts: Default::default(),
            signin: None,
            side: Default::default(),
            palette: None,
            told: Vec::new(),
            controls_w: 260.0,
            dragging: None,
            grab_dx: 0.0,
            drag_last: None,
            measured_for: None,
            drag_out: None,
            dropped: None,
            strip_screen: Rect::NOTHING,
            tabs_screen: Vec::new(),
            drop_hint: None,
            mode_w: 150.0,
            mode_compact: false,
            title: String::new(),
            tab_first: 0,
            tab_wheel: 0.0,
            confirm: None,
            panes_area: Rect::NOTHING,
            closing_window: false,
            scrolled_for: None,
        }
    }

    /// Opens what the open view chose, in a new tab or beside the active pane.
    pub(super) fn add(&mut self, launch: Launch, place: Place, start: PaneStart) {
        let Some(id) = self.new_pane(launch, start) else { return };
        match place {
            Place::Split { below } if !self.tabs.is_empty() => {
                let tab = &mut self.tabs[self.current];
                tab.root.split(tab.active, id, below);
                tab.root.even_out(id, below);
                tab.active = id;
            }
            _ => {
                self.tabs.push(Tab { root: Node::Leaf(id), active: id });
                self.current = self.tabs.len() - 1;
            }
        }
        self.zoom = None;
    }

    /// A pane for `launch`, not yet in any tab; None when it opens elsewhere.
    fn new_pane(&mut self, launch: Launch, start: PaneStart) -> Option<u64> {
        // Screens and files come from the Remote desktop extension, in a window of its own.
        #[cfg(not(feature = "remote"))]
        if let Launch::Agent { target, mode, .. } = &launch {
            if let Err(e) = crate::ext::spawn_provided("remote", &remote_args(target, *mode)) {
                self.failed = Some(e);
            }
            return None;
        }
        let id = NEXT_PANE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pane = match launch {
            Launch::Shell { profile, label, target, ssh_cmd } => Pane {
                id,
                label,
                target,
                ssh_cmd,
                mode: PaneMode::Terminal,
                terms: None,
                start,
                prefs: None,
                keep: None,
                state: State::Shell(Box::new(crate::termwin::TermWindow::single(profile))),
            },
            Launch::Agent { target, label, ssh_cmd, mode } => Pane {
                id,
                label,
                target,
                ssh_cmd,
                mode,
                terms: None,
                start,
                prefs: None,
                keep: None,
                state: State::Down(Down::new(false, String::new())),
            },
        };
        self.panes.push(pane);
        Some(id)
    }

    /// The first pane's shell starts in this view (terminal, text or markdown).
    pub(super) fn start_view(&mut self, view: &str) {
        if let Some(Pane { state: State::Shell(t), .. }) = self.panes.first_mut() {
            t.start_view = Some(view.to_string());
        }
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.panes.iter().position(|p| p.id == id)
    }

    fn active_id(&self) -> Option<u64> {
        self.tabs.get(self.current).map(|t| t.active)
    }

    /// The tab's panes in order (left to right, top to bottom).
    fn order(&self, tab: usize) -> Vec<u64> {
        let mut out = Vec::new();
        if let Some(t) = self.tabs.get(tab) {
            t.root.leaves(&mut out);
        }
        out
    }

    /// The pane's name and round trip for its tab and the dock.
    fn name(&self, id: u64) -> (String, Option<f32>) {
        let Some(p) = self.index(id).map(|i| &self.panes[i]) else { return (String::new(), None) };
        let rtt = match &p.state {
            State::Live(r) => Some(r.rtt()),
            _ => None,
        };
        (p.label.clone(), rtt)
    }

    fn close_pane(&mut self, id: u64) {
        if let Some(i) = self.index(id) {
            let mut p = self.panes.remove(i);
            match &mut p.state {
                State::Live(r) => r.close(),
                State::Shell(t) => {
                    let _ = t.close_all();
                }
                State::Down(_) => {}
            }
        }
        if self.zoom == Some(id) {
            self.zoom = None;
        }
        let mut emptied = None;
        for (ti, tab) in self.tabs.iter_mut().enumerate() {
            let root = std::mem::replace(&mut tab.root, Node::Leaf(0));
            match root.remove(id) {
                Some(n) => {
                    tab.root = n;
                    if tab.active == id {
                        let mut left = Vec::new();
                        tab.root.leaves(&mut left);
                        tab.active = left.first().copied().unwrap_or(0);
                    }
                }
                None => emptied = Some(ti),
            }
        }
        if let Some(ti) = emptied {
            self.tabs.remove(ti);
            (self.dragging, self.dropped, self.drag_out) = (None, None, None);
            if ti < self.current {
                self.current -= 1;
            }
            self.current = self.current.min(self.tabs.len().saturating_sub(1));
        }
    }

    /// Closes everything: the window holding the workspace is closing.
    pub(super) fn close_all(&mut self) {
        while !self.tabs.is_empty() {
            self.close_tab(0);
        }
    }

    /// Takes tab `t` with its panes into a workspace of its own.
    pub(super) fn take_tab(&mut self, t: usize) -> Option<Workspace> {
        if t >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(t);
        let mut ids = Vec::new();
        tab.root.leaves(&mut ids);
        let mut w = Workspace::new(self.opt.clone());
        let (moved, kept): (Vec<Pane>, Vec<Pane>) = std::mem::take(&mut self.panes).into_iter().partition(|p| ids.contains(&p.id));
        self.panes = kept;
        w.panes = moved;
        w.tabs.push(tab);
        if self.zoom.is_some_and(|z| ids.contains(&z)) {
            self.zoom = None;
        }
        self.current = self.current.min(self.tabs.len().saturating_sub(1));
        Some(w)
    }

    /// Takes in the tab of a workspace from another window (one `take_tab` made), at `at`, and
    /// shows it. Its panes keep their ids: those are unique in the whole program.
    pub(super) fn put_tab(&mut self, mut from: Workspace, at: usize) {
        let Some(tab) = from.tabs.pop() else { return };
        self.panes.append(&mut from.panes);
        let at = at.min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.current = at;
        self.zoom = None;
    }

    pub(super) fn dragging_tab(&self) -> Option<usize> {
        self.dragging
    }

    pub(super) fn tabs_len(&self) -> usize {
        self.tabs.len()
    }

    /// What a dragged tab shows of itself: its icon, name and number of panes.
    pub(super) fn tab_label(&self, t: usize) -> (&'static str, String, usize) {
        let Some(tab) = self.tabs.get(t) else { return (ic::TERMINAL, String::new(), 0) };
        (self.glyph(tab.active).0, self.name(tab.active).0, self.order(t).len())
    }

    fn close_tab(&mut self, tab: usize) {
        for id in self.order(tab) {
            self.close_pane(id);
        }
    }

    /// Connections being made: started, finished, or tried again later.
    fn tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let mut waiting = false;
        for i in 0..self.panes.len() {
            let target = self.panes[i].target.clone();
            let State::Down(d) = &mut self.panes[i].state else { continue };
            let mut live = None;
            match d.rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(Ok(s))) => live = Some(s),
                Some(Ok(Err(e))) => d.failed(e),
                Some(Err(mpsc::TryRecvError::Disconnected)) => d.failed("the connection attempt ended".into()),
                Some(Err(mpsc::TryRecvError::Empty)) => waiting = true,
                None if !d.given_up && now >= d.next => {
                    d.rx = Some(connect_in_background(target, ctx));
                    waiting = true;
                }
                None => waiting |= !d.given_up,
            }
            if let Some((sess, rx)) = live {
                self.go_live(i, sess, rx, ctx);
            }
        }
        if waiting {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        self.sign_in(ctx);
    }

    /// What extensions ask of the workspace, the command palette, and what they are told.
    fn extensions(&mut self, ctx: &egui::Context) {
        for r in crate::exthost::take_requests() {
            match r {
                crate::exthost::Request::OpenTerminal { argv, label, split } => self.open_command(argv, label, split),
                crate::exthost::Request::Paste(text) => self.paste(ctx, text),
            }
        }
        super::extui::question(ctx, &mut self.side);
        if self.palette.is_none() && self.picker.is_none() && !self.screen_has_keys() {
            super::extui::keys(ctx);
        }
        if let Some((filter, at)) = &mut self.palette {
            match super::extui::palette(ctx, filter, at) {
                Some(Some(e)) => {
                    self.palette = None;
                    self.run_entry(e, ctx);
                }
                Some(None) => self.palette = None,
                None => {}
            }
        }
        // Panes opened and closed, for the extensions that watch the workspace.
        let now: Vec<u64> = self.panes.iter().map(|p| p.id).collect();
        if now != self.told {
            for p in self.panes.iter().filter(|p| !self.told.contains(&p.id)) {
                crate::exthost::event("pane.opened", pane_info(p));
            }
            for id in self.told.iter().filter(|id| !now.contains(id)) {
                crate::exthost::event("pane.closed", serde_json::json!({ "pane": id }));
            }
            self.told = now;
        }
    }

    /// Text an extension or the palette pastes, into the active pane: a shell types it, a remote
    /// pane takes it by what it shows.
    fn paste(&mut self, ctx: &egui::Context, text: String) {
        let Some(i) = self.active_id().and_then(|id| self.index(id)) else { return };
        let result = match &mut self.panes[i].state {
            State::Live(r) => r.paste_text(ctx, text),
            _ => {
                ctx.input_mut(|i| i.events.push(egui::Event::Paste(text)));
                Ok(())
            }
        };
        if let Err(e) = result {
            self.lost(i, e);
        }
    }

    /// A terminal running `argv` (the login shell when empty), in a tab or beside the active pane.
    fn open_command(&mut self, argv: Vec<String>, label: String, split: Option<bool>) {
        let local = crate::termwin::local_profiles().into_iter().next().expect("a local shell");
        let label = if label.is_empty() { argv.first().cloned().unwrap_or_else(|| local.name.clone()) } else { label };
        let profile = if argv.is_empty() { local } else { crate::termwin::Profile { name: label.clone(), icon: super::ic::TERMINAL, launch: crate::termwin::Launch::Local(argv) } };
        let launch = Launch::Shell { profile, label, target: Target::Local { capture: "auto".into() }, ssh_cmd: None };
        let place = match split {
            Some(below) if !self.tabs.is_empty() => Place::Split { below },
            _ => Place::Tab,
        };
        self.add(launch, place, PaneStart::default());
    }

    fn run_entry(&mut self, e: super::extui::Entry, ctx: &egui::Context) {
        use super::extui::Run;
        match e.run {
            Run::Host { ext, id } => crate::exthost::command(&ext, &id, ""),
            Run::Script(c) => match crate::script::run_command(&c, "", "") {
                Ok((text, true)) if !text.is_empty() => self.paste(ctx, text),
                Ok((text, _)) => ctx.copy_text(text),
                Err(err) => self.failed = Some(err),
            },
            Run::Builtin("tab") => self.picker = Some(Place::Tab),
            Run::Builtin("right") => self.picker = Some(Place::Split { below: false }),
            Run::Builtin("down") => self.picker = Some(Place::Split { below: true }),
            Run::Builtin("side") => self.side.open = !self.side.open,
            Run::Builtin(_) => {}
        }
    }

    /// A host that refused the login is asked about once: the dialog with what it is, then the
    /// connection tried again with what was typed.
    fn sign_in(&mut self, ctx: &egui::Context) {
        if self.signin.is_none() {
            for p in &mut self.panes {
                if let State::Down(d) = &mut p.state
                    && d.given_up
                    && !d.asked
                    && super::signin::refused(&d.why)
                {
                    d.asked = true;
                    self.signin = super::signin::SignIn::new(p.id, &p.target, p.mode.name(), &d.why);
                    break;
                }
            }
        }
        let Some(pane) = self.signin.as_ref().map(|s| s.pane) else { return };
        let Some(i) = self.index(pane) else {
            self.signin = None;
            return;
        };
        let target = self.panes[i].target.clone();
        let Some(s) = &mut self.signin else { return };
        match s.show(ctx, &target) {
            Some(super::signin::Answer::Connect(t)) => {
                let keep = s.keep(&t);
                let p = &mut self.panes[i];
                p.keep = keep;
                p.target = t;
                p.state = State::Down(Down::new(false, String::new()));
                self.signin = None;
            }
            Some(super::signin::Answer::Cancel) => self.signin = None,
            None => {}
        }
    }

    #[cfg(not(feature = "remote"))]
    fn go_live(&mut self, _: usize, sess: Session, _: mpsc::Receiver<Ev>, _: &egui::Context) {
        match sess {}
    }

    #[cfg(feature = "remote")]
    fn go_live(&mut self, i: usize, sess: Session, rx: mpsc::Receiver<Ev>, ctx: &egui::Context) {
        let ctl = Control::new(&self.opt, Mode::True);
        let waker = ctx.clone();
        sess.on_message(move || waker.request_repaint());
        let p = &mut self.panes[i];
        let again = matches!(&p.state, State::Down(d) if d.again);
        let label = format!("{}  {}x{}  {}", p.label, sess.desk.0, sess.desk.1, sess.backend);
        let mut r = Remote::new(sess, rx, ctl, label, p.ssh_cmd.clone());
        if let Some(t) = p.terms.take() {
            r.adopt_terms(t);
        }
        let mut result = r.set_mode(p.mode);
        if let Some(prefs) = p.prefs.take() {
            result = result.and_then(|_| r.restore(prefs));
        }
        if again {
            r.notify("Connected again");
        }
        // The password typed to sign in worked: kept now, so it is not asked again.
        if let Some((name, password)) = p.keep.take()
            && let Err(e) = crate::vault::set(&name, &password)
        {
            r.notify(&format!("The password was not saved: {e}"));
        }
        let PaneStart { panel, chat, fullscreen: full } = std::mem::take(&mut p.start);
        r.open_panels(false, chat);
        self.reached.push(reached_as(&p.target, &p.label));
        p.state = State::Live(Box::new(r));
        let id = p.id;
        // The terminal asked for beside the screen is a pane of its own below it, as every pane
        // is; the tab shown stays the one the user is in.
        if panel {
            let launch = Launch::Agent { target: p.target.clone(), label: p.label.clone(), ssh_cmd: p.ssh_cmd.clone(), mode: PaneMode::Terminal };
            if let Some(t) = self.tab_of(id)
                && let Some(new) = self.new_pane(launch, PaneStart::default())
            {
                let tab = &mut self.tabs[t];
                tab.root.split(id, new, true);
                tab.root.even_out(new, true);
                tab.active = new;
            }
        }
        if full {
            // `--fullscreen`: this screen alone on the whole monitor.
            if let Some(t) = self.tab_of(id) {
                self.select_tab(t);
                self.tabs[t].active = id;
            }
            self.set_full(ctx, true);
        }
        if let Err(e) = result {
            self.lost(i, e);
        }
    }

    /// The pane's connection broke: its shells are kept and it starts trying again.
    fn lost(&mut self, i: usize, why: String) {
        let p = &mut self.panes[i];
        if let State::Live(r) = &mut p.state {
            p.mode = r.mode();
            p.prefs = Some(r.prefs());
            p.terms = r.take_terms().filter(|_| true).and_then(|mut t| t.keep_local().then_some(t));
            r.close();
        }
        p.state = State::Down(Down::new(true, why));
    }

    /// One pane alone in its tab, or the grid again. What fills the tab is what takes the keys.
    fn set_zoom(&mut self, zoom: Option<u64>) {
        self.zoom = zoom;
        if let Some(z) = zoom
            && let Some(t) = self.tab_of(z)
        {
            self.tabs[t].active = z;
        }
        // A zoom the full screen made is the user's now.
        self.full.zoomed = None;
    }

    /// Shows tab `t`; a pane zoomed in the tab left is drawn there only.
    fn select_tab(&mut self, t: usize) {
        if t != self.current {
            self.current = t;
            self.zoom = None;
        }
    }

    /// The tab holding pane `id`.
    fn tab_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| {
            let mut l = Vec::new();
            t.root.leaves(&mut l);
            l.contains(&id)
        })
    }

    /// The window on the whole monitor, or back. A screen in a split tab takes the monitor alone,
    /// as Remote Desktop and RustDesk show one computer; that zoom goes again on the way out.
    fn set_full(&mut self, ctx: &egui::Context, on: bool) {
        if !self.full.set(ctx, on) {
            return;
        }
        if on {
            self.dock.greet(ctx.input(|i| i.time));
            let active = self.active_id();
            let screen = active.and_then(|id| self.index(id)).is_some_and(|i| self.mode_of_pane(i) == PaneMode::Screen);
            if screen && self.zoom.is_none() && self.order(self.current).len() > 1 {
                self.zoom = active;
                self.full.zoomed = active;
            }
        } else {
            self.left_full();
        }
    }

    /// After full screen ended: the zoom it made goes, and the dock folds for next time.
    fn left_full(&mut self) {
        if self.full.zoomed.is_some() && self.zoom == self.full.zoomed {
            self.zoom = None;
        }
        self.full.zoomed = None;
        self.dock.open = false;
    }

    /// The dock while the window is full screen: Ctrl+Alt+Home unfolds or folds it, even over a
    /// remote screen (whose keys are otherwise the host's); unfolded it holds the title bar's
    /// tabs and controls.
    fn full_screen_dock(&mut self, ctx: &egui::Context, area: Rect, action: &mut Option<(u64, remote::Deferred)>) {
        let home = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::ALT, egui::Key::Home));
        if home {
            self.dock.toggle(ctx.input(|i| i.time));
        }
        let status = self.status();
        let width = self.bar_width();
        let mut dock = std::mem::take(&mut self.dock);
        dock.show(ctx, area, &status, width, |ui| self.bar(ui, action));
        self.dock = dock;
    }

    /// The title bar's contents at their natural width: the tabs at their widest (up to four), the
    /// +, the mode, the pane's controls as measured last frame, the layout and full-screen buttons.
    fn bar_width(&self) -> f32 {
        let tabs = self.tabs.len().clamp(1, 4) as f32 * TAB_MAX + PLUS_W + 12.0;
        tabs + self.mode_w + 8.0 + self.controls_w + LAYOUT_W + 46.0 + 16.0
    }

    /// What the folded dock tells of the active pane.
    fn status(&self) -> super::dock::Status {
        let Some((id, i)) = self.active_id().and_then(|id| self.index(id).map(|i| (id, i))) else {
            return super::dock::Status { glyph: ic::TERMINAL, name: String::new(), rtt: None, view_only: false, details: String::new() };
        };
        let (glyph, _) = self.glyph(id);
        let name = self.panes[i].label.clone();
        match &self.panes[i].state {
            #[cfg(feature = "remote")]
            State::Live(r) if r.mode() == PaneMode::Screen => {
                super::dock::Status { glyph, rtt: Some(r.rtt()), view_only: r.view_only(), details: r.details(), name }
            }
            _ => super::dock::Status { glyph, rtt: None, view_only: false, details: name.clone(), name },
        }
    }

    fn set_mode(&mut self, id: u64, mode: PaneMode) {
        let Some(i) = self.index(id) else { return };
        let p = &mut self.panes[i];
        p.mode = mode;
        // A shell on its way to the agent (or that could not reach it) is a shell again.
        if mode == PaneMode::Terminal && matches!(&p.state, State::Down(d) if !d.again) && p.terms.is_some() {
            let t = p.terms.take().expect("checked");
            p.state = State::Shell(Box::new(t));
            return;
        }
        match &mut p.state {
            State::Live(r) => {
                if let Err(e) = r.set_mode(mode) {
                    self.lost(i, e);
                }
            }
            // A plain shell reaches the agent of the same machine once, keeping its shells.
            State::Shell(_) if mode != PaneMode::Terminal => {
                let State::Shell(t) = std::mem::replace(&mut p.state, State::Down(Down::new(false, String::new()))) else {
                    unreachable!()
                };
                p.terms = Some(*t);
            }
            _ => {}
        }
    }

    /// One frame of the whole window.
    pub(super) fn ui(&mut self, ui: &mut egui::Ui, grabbed: Vec<(u32, bool)>) -> Out {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        if self.tabs.is_empty() {
            return Out { empty: true, grab: false };
        }
        // Alt+F4 or the system's close: the window and its connections go only once confirmed.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.confirm = Some(Confirm::Window);
        }
        let mut remote_action: Option<(u64, remote::Deferred)> = None;
        if self.full.follow_system(&ctx) {
            self.left_full();
        }
        // In a window, the title bar; in full screen the dock stands in for it, drawn over the panes.
        if !self.full.on {
            let bar = super::chrome::title_bar(ui, |ui| self.bar(ui, &mut remote_action));
            if bar.close {
                self.confirm = Some(Confirm::Window);
            }
            if bar.leave_full {
                self.set_full(&ctx, false);
            }
        }
        self.extensions(&ctx);
        // Full screen is the picture alone: the extensions' status bar and panel wait outside it.
        let area = if self.full.on { ui.available_rect_before_wrap() } else { super::extui::edges(ui, ui.available_rect_before_wrap(), &mut self.side) };
        self.panes_area = area;
        self.shortcuts(&ctx, area);
        if self.full.on {
            self.full_screen_dock(&ctx, area, &mut remote_action);
        }
        if self.tabs.is_empty() {
            return Out { empty: true, grab: false };
        }
        ui.painter().rect_filled(area, CornerRadius::ZERO, ui.visuals().panel_fill);
        // A zoom is of a pane in the tab shown; one left from another tab (or a closed pane) goes.
        if self.zoom.is_some_and(|z| self.tab_of(z) != Some(self.current)) {
            self.zoom = None;
        }
        let Some(tab) = self.tabs.get_mut(self.current) else { return Out::default() };
        let mut rects = Vec::new();
        match self.zoom {
            Some(z) => rects.push((z, area)),
            None => tab.root.layout(ui, area, 0, &mut rects),
        }
        // Files dropped go to the pane they were dropped on, which becomes the active one.
        if ctx.input(|i| !i.raw.dropped_files.is_empty())
            && let Some((x, y)) = crate::sys::pointer_on_screen()
            && let p = window_point(&ctx, Pos2::new(x as f32, y as f32))
            && let Some((id, _)) = rects.iter().find(|(_, r)| r.contains(p))
        {
            tab.active = *id;
        }
        // A click in a pane makes it the active one, with the keyboard.
        if let Some(p) = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten())
            && let Some((id, _)) = rects.iter().find(|(_, r)| r.contains(p))
            && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background)
        {
            tab.active = *id;
        }
        let active = tab.active;
        let several = rects.len() > 1;
        // While a dialog asks something, keys are the dialog's, not a pane's.
        let asking = self.confirm.is_some() || self.picker.is_some();
        let mut grabbed = Some(grabbed);
        let mut closing = Vec::new();
        let mut lost = Vec::new();
        let mut full_toggle = false;
        let mut grab = false;
        let mut to_shell = Vec::new();
        for (id, rect) in &rects {
            let Some(i) = self.index(*id) else { continue };
            let is_active = *id == active && !asking;
            let pane = &mut self.panes[i];
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(*rect).id_salt(("pane", *id)));
            child.set_clip_rect(*rect);
            match &mut pane.state {
                State::Shell(t) => {
                    t.keys_on = is_active;
                    if t.embedded(&mut child).iter().any(|c| matches!(c, crate::termwin::Cmd::Close)) {
                        closing.push(*id);
                    }
                }
                State::Live(r) => {
                    r.active = is_active;
                    // Alone on the whole monitor, the picture fills it.
                    r.filling = self.full.on && (self.zoom == Some(*id) || !several);
                    let keys = if is_active { grabbed.take().unwrap_or_default() } else { Vec::new() };
                    match r.show(&mut child, keys) {
                        Err(e) => lost.push((i, e)),
                        Ok(()) => {
                            full_toggle |= std::mem::take(&mut r.full_toggle);
                            if r.pane_closed {
                                closing.push(*id);
                            }
                            grab |= is_active && r.grab_wanted;
                        }
                    }
                }
                State::Down(d) => match {
                    // Its shells, kept while it connects, go on taking their output meanwhile.
                    if let Some(t) = pane.terms.as_mut() {
                        let _ = t.background(&ctx);
                    }
                    down_view(&mut child, *rect, d, &pane.label)
                } {
                    Some(DownAct::Close) => closing.push(*id),
                    // A shell that was reaching for the agent stays the shell it was.
                    Some(DownAct::Cancel) if !d.again && pane.terms.is_some() => to_shell.push(*id),
                    Some(DownAct::Cancel) => {
                        d.rx = None;
                        d.given_up = true;
                        if d.why.is_empty() {
                            d.why = "Cancelled.".into();
                        }
                    }
                    Some(DownAct::Retry) => *d = Down::new(d.again, String::new()),
                    None => {}
                },
            }
            // In a terminal, what the computer it runs on is doing (its SSH host's, or this one's).
            let terminal = match &pane.state {
                State::Shell(_) => true,
                State::Live(r) => r.mode() == PaneMode::Terminal,
                State::Down(_) => pane.mode == PaneMode::Terminal,
            };
            if terminal {
                crate::gui::look::pane_overlay(ui, *rect, pane.ssh_cmd.as_deref(), &pane.label, is_active);
            }
            if several && is_active && self.zoom.is_none() {
                ui.painter().rect_stroke(*rect, CornerRadius::ZERO, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
            }
        }
        // The panes of other tabs (and those behind a zoomed one) keep running unseen.
        let drawn: Vec<u64> = rects.iter().map(|(id, _)| *id).collect();
        for i in 0..self.panes.len() {
            let id = self.panes[i].id;
            if drawn.contains(&id) {
                continue;
            }
            let pane = &mut self.panes[i];
            if let Some(t) = pane.terms.as_mut() {
                let _ = t.background(&ctx);
            }
            match &mut pane.state {
                State::Shell(t) => {
                    if t.background(&ctx).iter().any(|c| matches!(c, crate::termwin::Cmd::Close)) {
                        closing.push(id);
                    }
                }
                State::Live(r) => {
                    r.active = false;
                    if let Err(e) = r.background(&ctx) {
                        lost.push((i, e));
                    }
                }
                State::Down(_) => {}
            }
        }
        if let Some((id, f)) = remote_action
            && let Some(i) = self.index(id)
            && let State::Live(r) = &mut self.panes[i].state
            && let Err(e) = f(r, &ctx)
        {
            lost.push((i, e));
        }
        for (i, e) in lost {
            self.lost(i, e);
        }
        for id in to_shell {
            self.set_mode(id, PaneMode::Terminal);
        }
        if full_toggle {
            self.set_full(&ctx, !self.full.on);
        }
        for id in closing {
            self.close_pane(id);
        }
        // Shown here while tabs are open; with none left the open window shows it.
        if !self.tabs.is_empty()
            && let Some(e) = self.failed.take()
        {
            self.toasts.push(&e);
        }
        self.toasts.draw(&ctx, area);
        self.confirm_dialog(&ctx);
        // The window's title follows the active pane.
        if let Some(id) = self.active_id() {
            let title = format!("SSH Remote - {}", self.name(id).0);
            if title != self.title {
                ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
                self.title = title;
            }
        }
        Out { empty: self.tabs.is_empty(), grab }
    }

    /// The active pane shows a remote screen, whose keys all go to its host.
    fn screen_has_keys(&self) -> bool {
        self.active_id().and_then(|id| self.index(id)).is_some_and(|i| matches!(&self.panes[i].state, State::Live(r) if r.mode() == PaneMode::Screen))
    }

    /// Windows Terminal's keys for tabs and panes. A pane sharing a screen keeps its keys for the
    /// remote host; a dialog or menu keeps them too.
    fn shortcuts(&mut self, ctx: &egui::Context, area: Rect) {
        if self.picker.is_some() || self.confirm.is_some() || ctx.any_popup_open() {
            return;
        }
        let Some(active) = self.active_id() else { return };
        if self.screen_has_keys() {
            return;
        }
        use egui::{Key, Modifiers};
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
        // Exactly these modifiers: Alt+Shift+arrows and the like stay the shell's.
        let take = |m: Modifiers, k: Key| {
            ctx.input_mut(|i| {
                let at = i.events.iter().position(|e| {
                    matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if *key == k && modifiers.matches_exact(m))
                });
                at.map(|n| i.events.remove(n)).is_some()
            })
        };
        let panes = self.order(self.current).len();
        if take(Modifiers::NONE, Key::F11) {
            self.set_full(ctx, !self.full.on);
        } else if take(cs, Key::Enter) {
            let z = if self.zoom == Some(active) { None } else { Some(active) };
            self.set_zoom(z);
        } else if take(cs, Key::T) {
            self.picker = Some(Place::Tab);
        } else if take(cs, Key::P) {
            self.palette = Some((String::new(), 0));
        } else if take(cs, Key::W) {
            self.confirm = Some(Confirm::Pane(active));
        } else if take(cs, Key::Tab) && !self.tabs.is_empty() {
            self.select_tab((self.current + self.tabs.len() - 1) % self.tabs.len());
        } else if take(Modifiers::CTRL, Key::Tab) && !self.tabs.is_empty() {
            self.select_tab((self.current + 1) % self.tabs.len());
        } else if take(alt_shift, Key::Plus) || take(alt_shift, Key::Equals) {
            self.picker = Some(Place::Split { below: false });
        } else if take(alt_shift, Key::Minus) {
            self.picker = Some(Place::Split { below: true });
        } else if take(alt_shift, Key::D) {
            // Along the longer side, as Windows Terminal does.
            self.picker = Some(Place::Split { below: area.aspect_ratio() < 1.6 });
        } else if panes > 1 {
            let back = take(Modifiers::ALT, Key::ArrowLeft) || take(Modifiers::ALT, Key::ArrowUp);
            let on = take(Modifiers::ALT, Key::ArrowRight) || take(Modifiers::ALT, Key::ArrowDown);
            if back || on {
                let order = self.order(self.current);
                if let Some(at) = order.iter().position(|&id| id == active) {
                    let next = if on { (at + 1) % order.len() } else { (at + order.len() - 1) % order.len() };
                    self.tabs[self.current].active = order[next];
                    // In a zoomed tab, the next pane takes its place.
                    if self.zoom.is_some() {
                        self.zoom = Some(order[next]);
                    }
                }
            }
        }
    }

    /// The title bar's content: tabs and + on the left; on the right the active pane's mode and
    /// controls, then the layout buttons. What does not fit folds into one menu.
    fn bar(&mut self, ui: &mut egui::Ui, action: &mut Option<(u64, remote::Deferred)>) {
        ui.spacing_mut().item_spacing.x = 2.0;
        let avail = ui.available_width();
        // Widths: the mode drop-down and the pane's controls (as measured last frame), the "more"
        // menu; the layout buttons' is `LAYOUT_W`.
        const MORE_W: f32 = 36.0;
        let mode_w = self.mode_w + 8.0;
        let now_for = self.active_id().and_then(|id| self.index(id).map(|i| (id, self.mode_of_pane(i))));
        if now_for != self.measured_for {
            self.measured_for = now_for;
            // Tried unfolded once; measured there, it folds again if it does not fit.
            self.controls_w = 0.0;
        }
        // The controls fold before the tabs shrink below three at their narrowest.
        // The window's full-screen button and its separator stay at the right in any width.
        let room = avail - self.tabs.len().min(3) as f32 * TAB_MIN - PLUS_W - 46.0;
        let fold_controls = room < mode_w + self.controls_w + LAYOUT_W;
        let fold_layout = room < mode_w + MORE_W + LAYOUT_W;
        // Narrower still: the mode drop-down shows its icon only (its full width is about 150).
        self.mode_compact = room < 150.0 + MORE_W + 40.0;
        // The right-hand group first, from the right edge, so the tabs take exactly what it leaves.
        let full = ui.max_rect();
        let mut right = ui.new_child(egui::UiBuilder::new().id_salt("bar-right").max_rect(full).layout(egui::Layout::right_to_left(egui::Align::Center)));
        if let Some(active) = self.active_id() {
            let ui = &mut right;
            ui.spacing_mut().item_spacing.x = 2.0;
            // The whole window on the monitor, beside the window's own buttons.
            // A shared screen keeps F11 for the remote computer: its key is Ctrl+Alt+Enter.
            let screen = self.index(active).is_some_and(|i| matches!(&self.panes[i].state, State::Live(r) if r.mode() == PaneMode::Screen));
            let (glyph, tip) = match (self.full.on, screen) {
                (true, false) => (ic::ARROWS_IN, "Leave full screen (F11)"),
                (true, true) => (ic::ARROWS_IN, "Leave full screen (Ctrl+Alt+Enter; F11 goes to the remote computer)"),
                (false, false) => (ic::ARROWS_OUT, "Full screen: the window on the whole monitor, as it is (F11)"),
                (false, true) => (
                    ic::ARROWS_OUT,
                    "Full screen: the window on the whole monitor, as it is (F11 goes to the remote computer; Ctrl+Alt+Enter fills the screen with this one)",
                ),
            };
            if ui.add(tool_button(glyph, self.full.on)).on_hover_text(tip).clicked() {
                let on = !self.full.on;
                self.set_full(ui.ctx(), on);
            }
            ui.separator();
            if !fold_layout {
                self.layout_buttons(ui, true);
                ui.separator();
            }
            if fold_controls {
                // One menu for what does not fit: the pane's controls, and the layout if needed.
                let more = icon_menu(ui, ic::DOTS, |ui| {
                    ui.set_max_width(440.0);
                    ui.horizontal_wrapped(|ui| self.controls(ui, active, action));
                    if fold_layout {
                        ui.separator();
                        ui.horizontal_wrapped(|ui| self.layout_buttons(ui, false));
                    }
                });
                more.on_hover_text("More");
                self.mode_menu(ui, active);
            } else {
                // The mode then its controls, left to right, at the width they took last time.
                let w = self.controls_w + self.mode_w + 4.0;
                ui.allocate_ui_with_layout(Vec2::new(w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    self.mode_menu(ui, active);
                    let start = ui.cursor().left();
                    self.controls(ui, active, action);
                    self.controls_w = (ui.cursor().left() - start).max(0.0);
                });
            }
        }
        let right_left = right.min_rect().left().min(full.right());
        let width = (right_left - ui.cursor().left() - PLUS_W - 12.0).max(84.0);
        let tab_rects = self.tab_strip(ui, width);
        if ui.add(tool_button(ic::PLUS, false)).on_hover_text("Open: a terminal, a screen or files in a new tab (Ctrl+Shift+T)").clicked() {
            self.picker = Some(Place::Tab);
        }
        self.drag_tab(ui, &tab_rects);
    }

    /// The tabs, as a terminal draws them: the current one in the panes' colour and joined to
    /// them, the others flat with a thin rule between. They share `width` down to `TAB_MIN`
    /// each; past that the strip scrolls (wheel, arrows, the current tab kept in view) and a
    /// list of every tab opens from the end. Returns where each tab is drawn (for dragging); a
    /// tab's close asks first.
    fn tab_strip(&mut self, ui: &mut egui::Ui, width: f32) -> Vec<Rect> {
        let n = self.tabs.len();
        let over = n as f32 * TAB_MIN > width + 0.5;
        // Too little room for arrows too: the current tab alone, and the list of all of them.
        let cramped = over && width < TAB_MIN + 3.0 * 32.0;
        let mut view_w = width;
        if cramped {
            view_w = width - 34.0;
        } else if over {
            let start = ui.cursor().left();
            if ui.add(tool_button(ic::CARET_LEFT, false)).on_hover_text("Scroll the tabs").clicked() {
                self.tab_first = self.tab_first.saturating_sub(1);
            }
            // The arrow's width, with its spacing, for it, the other arrow and the list.
            let step = ui.cursor().left() - start;
            view_w = width - 3.0 * step - ui.spacing().item_spacing.x;
        }
        // Whole tabs only: as many as fit at their narrowest, sharing the room.
        let shown = if cramped { 1 } else if over { ((view_w / TAB_MIN).floor() as usize).max(1) } else { n };
        let fit = if over { view_w / shown as f32 } else { (width / n.max(1) as f32).clamp(TAB_MIN, TAB_MAX) };
        let bar = ui.max_rect();
        let (view, strip) = ui.allocate_exact_size(Vec2::new(fit * shown as f32, bar.height()), egui::Sense::hover());
        let view = Rect::from_x_y_ranges(view.x_range(), bar.y_range());
        // The wheel over the tabs steps through them; the current one comes into view when it
        // or the room changes.
        if over && strip.hovered() {
            let d = ui.input(|i| i.smooth_scroll_delta);
            self.tab_wheel -= if d.x != 0.0 { d.x } else { d.y };
            while self.tab_wheel.abs() >= 40.0 {
                let step = self.tab_wheel.signum();
                self.tab_wheel -= step * 40.0;
                self.tab_first = if step > 0.0 { self.tab_first + 1 } else { self.tab_first.saturating_sub(1) };
            }
        }
        if self.scrolled_for != Some((self.current, shown)) {
            self.scrolled_for = Some((self.current, shown));
            self.tab_first = self.tab_first.clamp((self.current + 1).saturating_sub(shown), self.current);
        }
        self.tab_first = self.tab_first.min(n - shown);
        let first = self.tab_first;
        let painter = ui.painter().with_clip_rect(view.intersect(ui.clip_rect()));
        let visuals = ui.visuals().clone();
        let mut rects = vec![Rect::NOTHING; n];
        let mut hovered = None;
        let mut shapes = Vec::new();
        // Each tab has its slot; the one being dragged along the bar floats under the pointer,
        // drawn over the others.
        let floating = self.dragging.filter(|_| self.drag_out.is_none());
        let pointer_x = ui.input(|i| i.pointer.latest_pos()).map(|p| p.x);
        let float_painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("tab-float"))).with_clip_rect(view);
        let mut draw: Vec<usize> = (first..first + shown).filter(|&t| Some(t) != floating).collect();
        draw.extend(floating.filter(|&t| t >= first && t < first + shown));
        for t in draw {
            let x = view.left() + (t - first) as f32 * fit;
            let slot = Rect::from_min_max(Pos2::new(x, view.top() + 6.0), Pos2::new(x + fit, view.bottom()));
            rects[t] = slot;
            let float = Some(t) == floating;
            // Tabs slide to their new places as the dragged one passes them.
            let key = self.order(t).first().copied().unwrap_or(0);
            let slide = egui::Id::unique(("tab-x", key));
            let rect = match (float, pointer_x) {
                (true, Some(px)) => {
                    let left = (px - self.grab_dx).clamp(view.left(), view.right() - fit);
                    ui.ctx().animate_value_with_time(slide, left, 0.0);
                    slot.translate(Vec2::new(left - slot.left(), 0.0))
                }
                _ => {
                    let left = ui.ctx().animate_value_with_time(slide, slot.left(), 0.12);
                    slot.translate(Vec2::new(left - slot.left(), 0.0))
                }
            };
            let painter = if float { &float_painter } else { &painter };
            if float {
                let shadow = egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(90) };
                painter.add(shadow.as_shape(rect, egui::CornerRadius { nw: theme::R_LG, ne: theme::R_LG, sw: 0, se: 0 }));
            }
            let id = self.tabs[t].active;
            let order_len = self.order(t).len();
            let (name, rtt) = self.name(id);
            let (glyph, down) = self.glyph(id);
            let on = t == self.current;
            let r = ui.interact(slot, egui::Id::unique(("tab", t)), egui::Sense::click_and_drag());
            let x_rect = Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.center().y), Vec2::splat(20.0));
            let xr = ui.interact(x_rect, egui::Id::unique(("tab-close", t)), egui::Sense::click());
            let hot = (r.hovered() || xr.hovered()) && self.dragging.is_none();
            if hot {
                hovered = Some(t);
            }
            let top = egui::CornerRadius { nw: theme::R_LG, ne: theme::R_LG, sw: 0, se: 0 };
            if on {
                painter.rect_filled(rect, top, visuals.panel_fill);
                painter.rect_stroke(rect.expand2(Vec2::new(0.0, 2.0)).translate(Vec2::new(0.0, 2.0)), top, visuals.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
            } else if hot {
                painter.rect_filled(rect.with_max_y(rect.bottom() - 4.0), theme::R_LG, visuals.widgets.hovered.weak_bg_fill);
            }
            let fg = if on { visuals.strong_text_color() } else { visuals.text_color() };
            let weak = visuals.weak_text_color();
            let mid = rect.center().y - 1.0;
            let icon_color = if down { warn() } else { weak };
            painter.text(Pos2::new(rect.left() + 12.0, mid), Align2::LEFT_CENTER, glyph, FontId::proportional(14.0), icon_color);
            let text_left = rect.left() + 34.0;
            let text_rect = Rect::from_min_max(Pos2::new(text_left, rect.top()), Pos2::new(rect.right() - 30.0, rect.bottom()));
            let mut job = egui::text::LayoutJob::default();
            let font = FontId::proportional(13.0);
            job.append(&name, 0.0, egui::TextFormat::simple(font.clone(), fg));
            if let Some(ms) = rtt {
                job.append(&format!("{:.0} ms", ms.min(999.0)), 8.0, egui::TextFormat::simple(FontId::proportional(11.5), weak));
            }
            if order_len > 1 {
                job.append(&format!("{} {}", ic::SQUARES_FOUR, order_len), 8.0, egui::TextFormat::simple(FontId::proportional(11.5), weak));
            }
            job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width().max(8.0));
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(Pos2::new(text_left, mid - galley.size().y / 2.0), galley, fg);
            if xr.hovered() {
                painter.rect_filled(x_rect, theme::R_SM, visuals.widgets.hovered.weak_bg_fill);
            }
            if on || hot {
                painter.text(x_rect.center(), Align2::CENTER_CENTER, ic::X, FontId::proportional(12.0), fg);
            }
            if xr.clicked() || r.middle_clicked() {
                self.confirm = Some(Confirm::Tab(id));
            } else if r.clicked() {
                self.select_tab(t);
            }
            if r.drag_started() {
                self.dragging = Some(t);
                self.drag_last = None;
                self.select_tab(t);
                self.grab_dx = r.interact_pointer_pos().map_or(fit / 2.0, |p| p.x - slot.left());
            }
            let mut tip = name;
            if let Some(ms) = rtt {
                tip.push_str(&format!("  ·  {:.0} ms", ms));
            }
            if order_len > 1 {
                tip.push_str(&format!("  ·  {order_len} panes"));
            }
            shapes.push((r, tip));
        }
        // Thin rules between tabs that are neither current nor under the pointer.
        for (t, rect) in rects.iter().enumerate().take(first + shown).skip(first + 1) {
            let quiet = |k: usize| k != self.current && Some(k) != hovered && Some(k) != floating;
            if quiet(t - 1) && quiet(t) {
                let x = rect.left();
                let y = rect.center().y - 1.0;
                painter.vline(x, (y - 8.0)..=(y + 8.0), Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));
            }
        }
        for (r, tip) in shapes {
            if self.dragging.is_none() {
                r.on_hover_text(tip);
            }
        }
        // A tab dragged over from another window: where it would go in.
        if let Some(at) = self.drop_hint {
            let x = if at < first {
                view.left()
            } else if at < first + shown {
                rects[at].left()
            } else {
                view.left() + shown as f32 * fit
            };
            let accent = super::ACCENT;
            painter.rect_filled(view.with_min_y(view.top() + 6.0), theme::R_LG, accent.gamma_multiply(0.10));
            ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("tab-drop"))).rect_filled(
                Rect::from_center_size(Pos2::new(x.clamp(view.left() + 1.5, view.right() - 1.5), view.center().y + 3.0), Vec2::new(3.0, view.height() - 10.0)),
                1.5,
                accent,
            );
        }
        // Where the tabs are on the screen, in pixels, for drops from windows on other monitors.
        let ctx = ui.ctx().clone();
        let on_screen = |r: Rect| Rect::from_min_max(screen_point(&ctx, r.min), screen_point(&ctx, r.max));
        self.strip_screen = on_screen(view);
        self.tabs_screen = rects.iter().map(|r| if r.is_finite() && r.is_positive() { on_screen(*r) } else { Rect::NOTHING }).collect();
        if over {
            if !cramped && ui.add(tool_button(ic::CARET_RIGHT, false)).on_hover_text("Scroll the tabs").clicked() {
                self.tab_first += 1;
            }
            let names: Vec<(String, &'static str)> = (0..n).map(|t| (self.name(self.tabs[t].active).0, self.glyph(self.tabs[t].active).0)).collect();
            let current = self.current;
            let mut pick = None;
            // Its own id: the bar's right-hand group is laid out first, in a child of this ui.
            ui.push_id("tab-list", |ui| {
                let r = icon_menu(ui, ic::CARET_DOWN, |ui| {
                    for (t, (name, glyph)) in names.iter().enumerate() {
                        if ui.selectable_label(t == current, format!("{glyph}  {name}")).clicked() {
                            pick = Some(t);
                            ui.close();
                        }
                    }
                });
                r.on_hover_text(format!("All {n} tabs"));
            });
            if let Some(t) = pick {
                self.select_tab(t);
            }
        }
        rects
    }

    /// The icon of a pane's mode, and whether it is down (connecting again, or given up).
    fn glyph(&self, id: u64) -> (&'static str, bool) {
        let Some(p) = self.index(id).map(|i| &self.panes[i]) else { return (ic::TERMINAL, false) };
        let mode = match &p.state {
            State::Live(r) => r.mode(),
            State::Shell(_) => PaneMode::Terminal,
            State::Down(_) => p.mode,
        };
        let glyph = match mode {
            PaneMode::Terminal => ic::TERMINAL,
            PaneMode::Screen => ic::DESKTOP,
            PaneMode::Files => ic::FOLDER,
        };
        (glyph, matches!(p.state, State::Down(_)))
    }

    /// A tab being dragged: along the bar it changes places with the others; away from the bar
    /// the app shows where it would go, and takes it there when let go.
    fn drag_tab(&mut self, ui: &egui::Ui, rects: &[Rect]) {
        let Some(d) = self.dragging else { return };
        let (pos, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released() || !i.pointer.any_down()));
        // Let go outside the window, the pointer may be gone in the same frame: where it was last.
        if let Some(p) = pos {
            self.drag_last = Some(p);
        }
        let Some(p) = pos.or(self.drag_last) else {
            if released {
                self.dragging = None;
                self.drag_out = None;
            }
            return;
        };
        let size = ui.ctx().content_rect().size();
        let away = p.y > super::chrome::HEIGHT + 40.0 || p.y < -20.0 || p.x < -20.0 || p.x > size.x + 20.0;
        let at = screen_point(ui.ctx(), p);
        if released {
            self.dragging = None;
            self.drag_out = None;
            if away && d < self.tabs.len() {
                self.dropped = Some((d, at));
            }
            return;
        }
        self.drag_out = away.then_some(at);
        if away {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            return;
        }
        // It changes places once the middle of the floating tab passes into another's slot.
        let width = rects.get(d).map_or(0.0, |r| r.width());
        let middle = p.x - self.grab_dx + width / 2.0;
        if let Some(to) = rects.iter().position(|r| middle >= r.left() && middle < r.right())
            && to != d
            && d < self.tabs.len()
        {
            let tab = self.tabs.remove(d);
            self.tabs.insert(to, tab);
            self.current = to;
            self.dragging = Some(to);
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    /// The drop-down that turns the active pane into a terminal, the screen or the files.
    fn mode_menu(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(i) = self.index(id) else { return };
        let start = ui.cursor().left();
        let cur = match &self.panes[i].state {
            State::Live(r) => r.mode(),
            _ => self.panes[i].mode,
        };
        let mut chosen = None;
        let text = if self.mode_compact {
            format!("{} {}", cur.glyph(), ic::CARET_DOWN)
        } else {
            format!("{}  {}  {}", cur.glyph(), cur.name(), ic::CARET_DOWN)
        };
        let label = RichText::new(text).size(13.0);
        let b = egui::Button::new(label).frame_when_inactive(false).min_size(Vec2::new(0.0, 30.0));
        let (r, _) = egui::containers::menu::MenuButton::from_button(b)
            .ui(ui, |ui| {
                crate::gui::menu_look(ui);
                ui.set_min_width(190.0);
                for m in PaneMode::ALL.into_iter().filter(|m| m.on() && (cfg!(feature = "remote") || *m == PaneMode::Terminal)) {
                    if ui.add(egui::Button::selectable(cur == m, format!("{}  {}", m.glyph(), m.name()))).clicked() {
                        chosen = Some(m);
                        ui.close();
                    }
                }
            });
        r.on_hover_text("What this pane shows of its connection; switching does not connect again");
        // Laid out left to right here or right to left in the bar: the width either way.
        let w = (ui.cursor().left() - start).abs();
        if w > 20.0 {
            self.mode_w = w;
        }
        if let Some(m) = chosen.filter(|m| *m != cur) {
            self.set_mode(id, m);
        }
    }

    /// The active pane's own controls (the screen's quality, keys, chat...).
    fn controls(&mut self, ui: &mut egui::Ui, id: u64, action: &mut Option<(u64, remote::Deferred)>) {
        let Some(i) = self.index(id) else { return };
        // Close comes last, after the mode's controls.
        match &mut self.panes[i].state {
            State::Live(r) => {
                let mut a = None;
                r.toolbar_items(ui, &mut a);
                if let Some(f) = a {
                    *action = Some((id, f));
                }
            }
            // A plain shell: how it is shown and its settings.
            State::Shell(t) => t.controls(ui),
            State::Down(_) => {}
        }
        // Close last: the pane closes (and its connection ends), and with the tab's last pane the tab.
        let (glyph, tip) = if self.is_remote(id) || !self.is_shell(id) {
            (ic::X, "Close: end this connection and close the pane (Ctrl+Shift+W)")
        } else {
            (ic::X, "Close this pane (Ctrl+Shift+W)")
        };
        if ui.add(tool_button(glyph, false)).on_hover_text(tip).clicked() {
            self.confirm = Some(Confirm::Pane(id));
        }
    }

    fn mode_of_pane(&self, i: usize) -> PaneMode {
        match &self.panes[i].state {
            State::Live(r) => r.mode(),
            State::Shell(_) => PaneMode::Terminal,
            State::Down(_) => self.panes[i].mode,
        }
    }

    /// Whether a pane is a plain shell here (closing it ends the shell, not a connection).
    fn is_shell(&self, id: u64) -> bool {
        self.index(id).is_some_and(|i| matches!(self.panes[i].state, State::Shell(_)))
    }

    /// Whether a pane reaches another computer (closing it disconnects).
    fn is_remote(&self, id: u64) -> bool {
        self.index(id).is_some_and(|i| {
            let p = &self.panes[i];
            p.ssh_cmd.is_some() || matches!(p.target, Target::Ssh { .. })
        })
    }

    /// Asks before closing a pane or a tab: closing ends connections and shells for good.
    /// Enter says yes, Escape no.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(what) = self.confirm else { return };
        if what == Confirm::Window {
            let panes = self.panes.len();
            let tabs = self.tabs.len();
            let body = format!(
                "{} and {} close, and their connections and shells end.",
                if tabs == 1 { "Its tab".to_string() } else { format!("Its {tabs} tabs") },
                if panes == 1 { "its pane".to_string() } else { format!("{panes} panes") }
            );
            if let Some(yes) = ask(ctx, "Close this window?", &body, "Close window") {
                self.confirm = None;
                if yes {
                    self.close_all();
                    self.closing_window = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            return;
        }
        let (id, tab) = match what {
            Confirm::Window => unreachable!(),
            Confirm::Pane(id) => (id, None),
            Confirm::Tab(id) => (id, self.tabs.iter().position(|t| {
                let mut l = Vec::new();
                t.root.leaves(&mut l);
                l.contains(&id)
            })),
        };
        if self.index(id).is_none() || matches!(what, Confirm::Tab(_)) && tab.is_none() {
            self.confirm = None;
            return;
        }
        let name = self.name(id).0;
        let (title, body, yes) = match tab {
            Some(t) => {
                let n = self.order(t).len();
                let name = self.name(self.tabs[t].active).0;
                let body = if n > 1 {
                    format!("Its {n} panes close, and their connections and shells end.")
                } else if self.is_remote(id) || !self.is_shell(id) {
                    "The connection ends and the tab closes.".to_string()
                } else {
                    "The shell ends and the tab closes.".to_string()
                };
                (format!("Close the tab {name}?"), body, "Close tab")
            }
            None if self.is_remote(id) || !self.is_shell(id) => (
                format!("Close {name}?"),
                "The connection ends and the pane closes; with the tab's last pane, the tab closes too.".to_string(),
                "Close",
            ),
            None => (
                format!("Close {name}?"),
                "The shell ends and the pane closes; with the tab's last pane, the tab closes too.".to_string(),
                "Close",
            ),
        };
        let answer = ask(ctx, &title, &body, yes);
        match answer {
            Some(true) => {
                self.confirm = None;
                match tab {
                    Some(t) => self.close_tab(t),
                    None => self.close_pane(id),
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    /// The tab's layout: the active pane alone or the grid, split beside or below, arrange, and
    /// move the active pane.
    fn layout_buttons(&mut self, ui: &mut egui::Ui, rtl: bool) {
        let zoomed = self.zoom.is_some();
        let mut buttons: Vec<(&str, &str, u8)> = vec![
            (
                if zoomed { ic::CORNERS_IN } else { ic::CORNERS_OUT },
                if zoomed { "Back to the grid: every pane of the tab (Ctrl+Shift+Enter)" } else { "This pane alone: it fills the tab, the others wait (Ctrl+Shift+Enter)" },
                0,
            ),
            (ic::SPLIT_SIDE, "Split: a new pane beside this one (Alt+Shift+=)", 1),
            (ic::SPLIT_BELOW, "Split: a new pane below this one (Alt+Shift+-)", 2),
            (ic::CARET_LEFT, "Move this pane left (Alt+arrows go between panes)", 3),
            (ic::CARET_RIGHT, "Move this pane right", 4),
        ];
        // The arrangement menu after the splits.
        buttons.insert(3, (ic::LAYOUT, "Layout: arrange this tab's panes", 5));
        // One pane: nothing to zoom, as nothing to arrange; both show, greyed.
        let many = self.order(self.current).len() > 1;
        if rtl {
            buttons.reverse();
        }
        for (glyph, tip, what) in buttons {
            if what == 5 {
                self.layout_menu(ui, glyph, tip);
                continue;
            }
            let on = what == 0 && zoomed;
            // Zoom and moving need another pane; splitting is how one gets there.
            let enabled = matches!(what, 1 | 2) || many || (what == 0 && zoomed);
            let button = ui.add_enabled(enabled, tool_button(glyph, on)).on_hover_text(tip);
            let button = button.on_disabled_hover_text(format!("{tip}: for a tab of two panes or more"));
            if !button.clicked() {
                continue;
            }
            match what {
                0 => {
                    let z = if zoomed { None } else { self.active_id() };
                    self.set_zoom(z);
                }
                1 | 2 => self.picker = Some(Place::Split { below: what == 2 }),
                _ => self.move_active(what == 3),
            }
        }
    }

    /// The tab's arrangements: the same sizes, side by side, stacked, a main pane, a grid.
    fn layout_menu(&mut self, ui: &mut egui::Ui, glyph: &str, tip: &str) {
        let many = self.order(self.current).len() > 1;
        let even = self.tabs.get(self.current).is_none_or(|t| t.root.is_even());
        let mut chosen = None;
        ui.add_enabled_ui(many, |ui| {
            icon_menu(ui, glyph, |ui| {
                for (layout, g, text) in [
                    (Layout::Even, ic::EQUALS, "Even sizes  (keep the arrangement)"),
                    (Layout::Side, ic::SPLIT_SIDE, "Side by side"),
                    (Layout::Stacked, ic::SPLIT_BELOW, "Stacked"),
                    (Layout::MainLeft, ic::LAYOUT, "Main pane left, the others stacked"),
                    (Layout::MainTop, ic::ROWS, "Main pane on top, the others side by side"),
                    (Layout::Grid, ic::SQUARES_FOUR, "Grid"),
                ] {
                    // Sizes already even: nothing it would change.
                    let item = ui.add_enabled(layout != Layout::Even || !even, egui::Button::new(format!("{g}  {text}")));
                    if item.on_disabled_hover_text("The panes are already the same size").clicked() {
                        chosen = Some(layout);
                        ui.close();
                    }
                }
            })
            .on_hover_text(tip)
            .on_disabled_hover_text("Layout: for a tab of two panes or more");
        });
        if let Some(layout) = chosen {
            self.arrange(layout);
        }
    }

    /// Arranges the current tab's panes; the active pane stays active (and is the main one).
    fn arrange(&mut self, layout: Layout) {
        let ids = self.order(self.current);
        let Some(tab) = self.tabs.get_mut(self.current) else { return };
        if layout == Layout::Even {
            tab.root.balance();
        } else {
            tab.root = Node::arranged(&ids, tab.active, layout);
        }
        self.zoom = None;
    }

    fn move_active(&mut self, left: bool) {
        let order = self.order(self.current);
        let Some(tab) = self.tabs.get_mut(self.current) else { return };
        let Some(at) = order.iter().position(|&id| id == tab.active) else { return };
        let other = if left { at.checked_sub(1) } else { (at + 1 < order.len()).then_some(at + 1) };
        if let Some(o) = other {
            tab.root.swap(tab.active, order[o]);
        }
    }

}

/// A point of this window as a point of the whole screen in pixels: windows on monitors of
/// different scales compare there.
pub(super) fn screen_point(ctx: &egui::Context, p: Pos2) -> Pos2 {
    // The window's place is in the system's points; `p` is in the page's, which a UI scale other
    // than 100 % makes larger or smaller.
    let (origin, native) = window_origin(ctx);
    (origin * native + p.to_vec2() * ctx.pixels_per_point()).to_pos2()
}

/// The point in this window at a place on the screen in pixels: the opposite of `screen_point`.
pub(super) fn window_point(ctx: &egui::Context, px: Pos2) -> Pos2 {
    let (origin, native) = window_origin(ctx);
    ((px.to_vec2() - origin * native) / ctx.pixels_per_point()).to_pos2()
}

fn window_origin(ctx: &egui::Context) -> (Vec2, f32) {
    ctx.input(|i| {
        let v = i.viewport();
        (v.inner_rect.map_or(Vec2::ZERO, |r| r.min.to_vec2()), v.native_pixels_per_point.unwrap_or(1.0))
    })
}

/// A question before something that cannot be undone: Cancel, or the red action. Enter says
/// yes (unless a button has the keyboard), Escape no; None while unanswered.
fn ask(ctx: &egui::Context, title: &str, body: &str, yes: &str) -> Option<bool> {
    let (modal, fit) = super::modal(ctx, "confirm-close", Vec2::new(420.0, 0.0));
    let mut answer = None;
    modal.show(ctx, |ui| {
        ui.set_width(fit.x);
        ui.label(RichText::new(title).size(16.0).strong());
        ui.add_space(6.0);
        ui.label(RichText::new(body).weak());
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let danger = egui::Button::new(RichText::new(yes).strong().color(super::bad()))
                .stroke(Stroke::new(1.0, super::bad().gamma_multiply(0.6)))
                .min_size(Vec2::new(0.0, 32.0));
            if ui.add(danger).clicked() {
                answer = Some(true);
            }
            if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(0.0, 32.0))).clicked() {
                answer = Some(false);
            }
        });
    });
    let button_focused = ctx.memory(|m| m.focused().is_some());
    let (enter, escape) = ctx.input_mut(|i| {
        let enter = !button_focused && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        (enter, i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    });
    if enter {
        answer = Some(true);
    } else if escape {
        answer = Some(false);
    }
    answer
}

impl Down {
    fn failed(&mut self, why: String) {
        self.rx = None;
        self.attempt += 1;
        self.why = why;
        // Tried again soon, then less often: 1, 2, 4, 8 seconds.
        let wait = 1u64 << (self.attempt - 1).min(3);
        self.next = Instant::now() + std::time::Duration::from_secs(wait);
        // A first attempt asks at once (a wrong host or password will not mend itself).
        if !self.again || self.since.elapsed().as_secs_f64() > GIVE_UP {
            self.given_up = true;
        }
    }
}

enum DownAct {
    Cancel,
    Retry,
    Close,
}

/// A pane connecting, or connecting again: a spinner and Cancel; once given up, why and
/// Reconnect or Close.
fn down_view(ui: &mut egui::Ui, rect: Rect, d: &Down, label: &str) -> Option<DownAct> {
    let mut act = None;
    ui.painter().rect_filled(rect, CornerRadius::ZERO, ui.visuals().panel_fill);
    let card = Rect::from_center_size(rect.center(), Vec2::new(rect.width().min(420.0), 170.0));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(card).layout(egui::Layout::top_down(egui::Align::Center)));
    if !d.given_up {
        c.add(egui::Spinner::new().size(28.0));
        c.add_space(6.0);
        let what = if d.again { format!("Connection to {label} lost; connecting again") } else { format!("Connecting to {label}") };
        c.label(RichText::new(what).strong());
        let secs = d.since.elapsed().as_secs();
        let sub = if d.attempt > 0 { format!("Attempt {}  ·  {secs} s", d.attempt + 1) } else { format!("{secs} s") };
        c.label(RichText::new(sub).small().weak());
        c.add_space(8.0);
        if c.button("Cancel").clicked() {
            act = Some(DownAct::Cancel);
        }
    } else {
        let title = if d.again { format!("The connection to {label} was lost") } else { format!("Could not connect to {label}") };
        c.label(RichText::new(title).strong());
        let (what, hint) = friendly(&d.why);
        c.label(RichText::new(what).weak()).on_hover_text(d.why.as_str());
        if !hint.is_empty() {
            c.label(RichText::new(hint).small().weak());
        }
        c.add_space(8.0);
        c.horizontal(|ui| {
            if ui.add(theme::primary(format!("{}  Reconnect", ic::ARROW_CLOCKWISE))).clicked() {
                act = Some(DownAct::Retry);
            }
            if ui.button("Close").clicked() {
                act = Some(DownAct::Close);
            }
        });
    }
    act
}

/// The workspace driven as a user would, against the built-in test desktop and a local shell:
/// cargo build --release, then
/// SSHREMOTE_EXE=target/release/sshremote.exe cargo test --release -- --ignored --test-threads=1 workspace
#[cfg(test)]
mod tests {
    use crate::gui::snapshot::RunTest;
    use super::*;

    /// A terminal-only core hands screens and files to the Remote desktop extension's window.
    #[cfg(not(feature = "remote"))]
    #[test]
    fn remote_launches_become_extension_command_lines() {
        let here = Target::Local { capture: "auto".into() };
        assert_eq!(remote_args(&here, PaneMode::Screen), ["--gui", "--here"]);
        let ssh = Target::Ssh { host: "a@b".into(), port: Some(2222), args: vec!["-A".into()], remote: String::new(), capture: String::new(), password: None, unattended: false };
        assert_eq!(remote_args(&ssh, PaneMode::Files), ["--gui", "--ssh", "-p", "--ssh", "2222", "--ssh", "-A", "a@b", "--open", "files"]);
    }
    use egui::{Event, PointerButton};

    struct Harness {
        ctx: egui::Context,
        w: Workspace,
        snap: super::super::snapshot::Snap,
        size: Vec2,
        clock: f64,
    }

    impl Harness {
        fn new() -> Harness {
            static SCRATCH: std::sync::Once = std::sync::Once::new();
            SCRATCH.call_once(|| {
                let dir = std::env::temp_dir().join(format!("sshremote-ws-test-{}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                for var in ["APPDATA", "XDG_CONFIG_HOME"] {
                    unsafe { std::env::set_var(var, &dir) };
                }
            });
            let ctx = egui::Context::default();
            crate::gui::theme::style(&ctx);
            let opt = crate::parse_client(&["--gui".into(), "--local".into()]).unwrap();
            Harness { ctx, w: Workspace::new(opt), snap: Default::default(), size: Vec2::new(1280.0, 800.0), clock: 0.0 }
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.clock += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
                time: Some(self.clock),
                focused: true,
                events,
                ..Default::default()
            };
            let w = &mut self.w;
            self.snap.frame(&self.ctx, input, |ui| {
                crate::gui::look::frame(ui.ctx());
                w.ui(ui, Vec::new());
            });
        }

        /// Frames until `done`, or panics after a few seconds.
        fn until(&mut self, what: &str, done: impl Fn(&Workspace) -> bool) {
            for _ in 0..200 {
                self.frame(Vec::new());
                if done(&self.w) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            panic!("never: {what}");
        }

        fn click_at(&mut self, at: Pos2, button: PointerButton) {
            for e in super::super::snapshot::click(at, button) {
                self.frame(e);
            }
            self.frame(Vec::new());
            self.frame(Vec::new());
        }

        /// Clicks what reads `what` in the title bar.
        fn bar(&mut self, what: &str) {
            self.frame(Vec::new());
            let r = self
                .snap
                .find(what)
                .into_iter()
                .rev()
                .find(|r| r.center().y < super::super::chrome::HEIGHT)
                .unwrap_or_else(|| panic!("no {what:?} in the title bar"));
            self.click_at(r.center(), PointerButton::Primary);
        }

        /// Clicks what reads `what`, the topmost drawn (a menu's item).
        fn item(&mut self, what: &str) {
            self.frame(Vec::new());
            let r = *self.snap.find(what).first().unwrap_or_else(|| panic!("no {what:?} drawn"));
            self.click_at(r.center(), PointerButton::Primary);
        }
    }

    fn live(w: &Workspace, i: usize) -> bool {
        w.panes.get(i).is_some_and(|p| matches!(p.state, State::Live(_)))
    }

    fn mode_of(w: &Workspace, i: usize) -> Option<PaneMode> {
        match &w.panes.get(i)?.state {
            State::Live(r) => Some(r.mode()),
            _ => None,
        }
    }

    fn test_desktop() -> Launch {
        Launch::Agent {
            target: Target::Local { capture: "synthetic".into() },
            label: "Test desktop".into(),
            ssh_cmd: None,
            mode: PaneMode::Screen,
        }
    }

    /// In a shared screen F11 is the remote computer's; Ctrl+Alt+Enter is the full-screen key
    /// there, and it leaves a full screen the window's button started.
    #[test]
    #[ignore]
    fn workspace_full_screen_keys_in_a_shared_screen() {
        let mut h = Harness::new();
        h.w.add(test_desktop(), Place::Tab, PaneStart::default());
        h.until("the test desktop connects", |w| live(w, 0));
        let cae = egui::Modifiers::CTRL | egui::Modifiers::ALT;
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        h.frame(vec![key(egui::Key::F11, egui::Modifiers::NONE)]);
        assert!(!h.w.full.on, "F11 goes to the remote computer");
        h.bar(ic::ARROWS_OUT);
        assert!(h.w.full.on, "the button fills the monitor");
        h.frame(vec![key(egui::Key::Enter, cae)]);
        h.frame(Vec::new());
        assert!(!h.w.full.on, "Ctrl+Alt+Enter leaves it");
        h.frame(vec![key(egui::Key::Enter, cae)]);
        h.frame(Vec::new());
        assert!(h.w.full.on, "and fills the monitor with the screen again");
        h.frame(vec![key(egui::Key::Enter, cae)]);
        h.frame(Vec::new());
        assert!(!h.w.full.on && h.w.zoom.is_none(), "and leaves");
        h.w.close_all();
    }

    /// The look of Appearance on a workspace of two terminals: a picture across the window, then
    /// one in each pane and tiled, own colours, the overlay, and the Appearance window itself.
    #[test]
    #[ignore]
    fn workspace_look_pictures() {
        use crate::gui::look::{self, BgLayer, Fit, Look, Scope};
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        let shell = || Launch::Shell { label: profile.name.clone(), profile: profile.clone(), target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
        h.w.add(shell(), Place::Tab, PaneStart::default());
        h.w.add(shell(), Place::Split { below: false }, PaneStart::default());
        let shot = |h: &mut Harness, l: &Look, name: &str| {
            look::set(l.clone());
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(30));
                h.frame(Vec::new());
            }
            h.snap.save(&h.ctx, h.size, &dir.join(format!("look_{name}.png")));
        };
        let built = |id: &str, strength: f32, fit: Fit| BgLayer { src: format!("builtin:{id}"), on: true, strength, fit, scope: Scope::Window };
        // Out of the box: the 3D title over the night lake.
        shot(&mut h, &Look::default(), "default");
        // The built-in style: its picture and its animation, across the window.
        for style in ["night"] {
            let l = Look { layers: vec![built(&format!("{style}-motion"), 0.7, Fit::Fill), built(&format!("{style}-still"), 0.7, Fit::Fill)], ..Look::default() };
            shot(&mut h, &l, style);
        }
        // The title in front of the night lake, and the same with each pane its own.
        let night = vec![built("brand", 0.8, Fit::Whole), built("night-motion", 0.7, Fit::Fill), built("night-still", 0.7, Fit::Fill)];
        shot(&mut h, &Look { layers: night.clone(), ..Look::default() }, "brand_night");
        let each: Vec<BgLayer> = night.iter().map(|b| BgLayer { scope: Scope::Pane, ..b.clone() }).collect();
        shot(&mut h, &Look { layers: each, ..Look::default() }, "brand_night_panes");
        // The layer list in Appearance.
        look::open(&h.ctx);
        shot(&mut h, &Look::default(), "editor");
        let wheel = Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -500.0), modifiers: Default::default(), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
        h.frame(vec![Event::PointerMoved(Pos2::new(640.0, 450.0))]);
        for _ in 0..3 {
            h.frame(vec![wheel.clone()]);
        }
        shot(&mut h, &Look::default(), "editor_layers");
        look::close();
        // Other fonts and a larger interface, taken at once.
        if cfg!(windows) {
            let fonts = Look { ui_font: r"C:\Windows\Fonts\georgia.ttf".into(), mono_font: r"C:\Windows\Fonts\cour.ttf".into(), ui_scale: 1.2, ..Look::default() };
            shot(&mut h, &fonts, "fonts");
        }
        look::set(Look::default());
        crate::termwin::reset_look();
        h.w.close_all();
    }

    /// Each terminal shows what its own computer is doing: an SSH terminal its host's figures (from
    /// SSHREMOTE_SSH_HOST), a local one this computer's; then only in the active one.
    #[test]
    #[ignore]
    fn workspace_host_figures() {
        use crate::gui::look::{self, HudScope, Look};
        let (Some(dir), Ok(host)) = (std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from), std::env::var("SSHREMOTE_SSH_HOST")) else { return };
        let mut h = Harness::new();
        look::set(Look { layers: Vec::new(), ..Look::default() });
        let ssh = Launch::Shell { label: host.clone(), profile: crate::termwin::ssh_profile(&host, ""), target: Target::Local { capture: "synthetic".into() }, ssh_cmd: Some(vec!["ssh".into(), host.clone()]) };
        h.w.add(ssh, Place::Tab, PaneStart::default());
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Split { below: false }, PaneStart::default());
        let shown = |h: &mut Harness, what: &str| {
            for _ in 0..160 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                h.frame(Vec::new());
                if !h.snap.find(what).is_empty() {
                    return true;
                }
            }
            false
        };
        assert!(shown(&mut h, &format!("{host}  \u{B7}  CPU")), "the SSH terminal shows its host's figures");
        assert!(shown(&mut h, "this computer  \u{B7}  CPU"), "the local one this computer's");
        h.snap.save(&h.ctx, h.size, &dir.join("host_figures.png"));
        look::set(Look { layers: Vec::new(), hud_active_only: true, ..Look::default() });
        for _ in 0..5 {
            h.frame(Vec::new());
        }
        assert!(h.snap.find(&format!("{host}  \u{B7}")).is_empty(), "only the active terminal (the local one) shows them");
        h.snap.save(&h.ctx, h.size, &dir.join("host_figures_active.png"));
        assert_eq!(Look::default().hud_scope, HudScope::Terminals);
        look::set(Look::default());
        h.w.close_all();
    }

    #[test]
    #[ignore]
    fn workspace_modes_splits_and_reconnect() {
        let mut h = Harness::new();
        h.w.add(test_desktop(), Place::Tab, PaneStart::default());
        h.until("the test desktop connects", |w| live(w, 0));

        // The mode drop-down turns the pane into a terminal and into files, without connecting again.
        h.bar("Screen sharing");
        h.item("Terminal");
        h.until("terminal mode", |w| mode_of(w, 0) == Some(PaneMode::Terminal));
        h.bar("Terminal");
        h.item("Files");
        h.until("files mode", |w| mode_of(w, 0) == Some(PaneMode::Files));
        h.bar("Files");
        h.item("Screen sharing");
        h.until("screen mode", |w| mode_of(w, 0) == Some(PaneMode::Screen));

        // Split beside: the open dialog is asked for; what it opens goes beside, and is active.
        h.bar(ic::SPLIT_SIDE);
        assert_eq!(h.w.picker, Some(Place::Split { below: false }));
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        let shell = Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
        h.w.picker = None;
        h.w.add(shell, Place::Split { below: false }, PaneStart::default());
        h.frame(Vec::new());
        let order = h.w.order(0);
        assert_eq!(order.len(), 2, "two panes side by side");
        assert_eq!(h.w.tabs[0].active, order[1], "the new pane is active");

        // A click in the left pane makes it active.
        h.click_at(Pos2::new(200.0, 400.0), PointerButton::Primary);
        assert_eq!(h.w.tabs[0].active, order[0], "a click activates the pane");
        // Move it right: the two swap places.
        h.bar(ic::CARET_RIGHT);
        assert_eq!(h.w.order(0), vec![order[1], order[0]], "moved right");
        h.bar(ic::CARET_LEFT);
        assert_eq!(h.w.order(0), order, "and back");

        // The pane's own full screen fills the window with it alone.
        h.click_at(Pos2::new(200.0, 400.0), PointerButton::Primary);
        h.bar(ic::CORNERS_OUT);
        assert_eq!(h.w.zoom, Some(order[0]), "pane full screen");
        h.w.set_zoom(None);
        // Full screen from a screen shows it alone; leaving brings the split back.
        h.bar(ic::ARROWS_OUT);
        assert!(h.w.full.on && h.w.zoom == Some(order[0]), "the screen alone on the monitor");
        let ctx = h.ctx.clone();
        h.w.set_full(&ctx, false);
        assert!(!h.w.full.on && h.w.zoom.is_none(), "the split again");

        // A lost connection is made again by itself, in the mode it had.
        h.w.set_mode(order[0], PaneMode::Terminal);
        h.until("terminal again", |w| mode_of(w, 0) == Some(PaneMode::Terminal));
        if let State::Live(r) = &mut h.w.panes[0].state {
            r.close();
        }
        h.until("the pane notices", |w| matches!(w.panes[0].state, State::Down(_)));
        h.until("it connects again", |w| live(w, 0));
        assert_eq!(mode_of(&h.w, 0), Some(PaneMode::Terminal), "in the mode it had");

        // What the user chose stays chosen: a screen set to view only is view only again.
        h.w.set_mode(order[0], PaneMode::Screen);
        h.until("screen again", |w| mode_of(w, 0) == Some(PaneMode::Screen));
        if let State::Live(r) = &mut h.w.panes[0].state {
            r.set_view_only(true);
            r.close();
        }
        h.until("the pane notices", |w| matches!(w.panes[0].state, State::Down(_)));
        h.until("it connects again", |w| live(w, 0));
        let State::Live(r) = &h.w.panes[0].state else { unreachable!() };
        assert!(r.view_only(), "still view only after the reconnect");
        assert!(r.toast_texts().contains(&"Connected again"), "and it says it is back");

        // Closing the tab closes both panes; nothing is left.
        h.w.close_tab(0);
        h.frame(Vec::new());
        assert!(h.w.tabs.is_empty() && h.w.panes.is_empty());
    }

    /// What could not be done while tabs are open shows over the panes, not on the open window
    /// the user is no longer looking at.
    #[test]
    fn a_failure_in_the_workspace_shows_there() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        let shell = Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
        h.w.add(shell, Place::Tab, PaneStart::default());
        h.frame(Vec::new());
        h.w.failed = Some("The script failed: no such file".into());
        h.frame(Vec::new());
        assert!(h.w.failed.is_none(), "taken by the workspace");
        assert_eq!(h.w.toasts.texts(), ["The script failed: no such file"]);
        h.w.close_all();
    }

    /// Pictures of the workspace to look at: split, narrow, a pane in full screen, connecting.
    #[test]
    #[ignore]
    fn workspace_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        for dark in [true, false] {
            let mut h = Harness::new();
            crate::gui::theme::apply_theme(&h.ctx, dark);
            let name = if dark { "dark" } else { "light" };
            let save = |h: &mut Harness, what: &str| {
                for _ in 0..8 {
                    h.frame(Vec::new());
                }
                h.snap.save(&h.ctx, h.size, &dir.join(format!("ws_{name}_{what}.png")));
            };
            h.w.add(test_desktop(), Place::Tab, PaneStart::default());
            h.until("connected", |w| live(w, 0));
            // One pane: zoom and layout show, greyed.
            save(&mut h, "one_pane");
            let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            h.w.add(
                Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
                Place::Split { below: false },
                PaneStart::default(),
            );
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                h.frame(Vec::new());
            }
            save(&mut h, "split");
            h.bar(ic::LAYOUT);
            save(&mut h, "layout_menu");
            h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
            let first = h.w.order(0)[0];
            h.w.confirm = Some(Confirm::Pane(first));
            save(&mut h, "confirm");
            h.w.confirm = None;
            h.w.tabs[0].active = first;
            h.w.set_mode(first, PaneMode::Files);
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(30));
                h.frame(Vec::new());
            }
            save(&mut h, "files");
            h.w.set_mode(first, PaneMode::Screen);
            h.w.tabs[0].active = h.w.order(0)[1];
            h.bar("Terminal");
            save(&mut h, "mode_menu");
            h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
            h.size = Vec2::new(760.0, 520.0);
            h.click_at(Pos2::new(150.0, 300.0), PointerButton::Primary);
            save(&mut h, "narrow");
            h.bar(ic::DOTS);
            save(&mut h, "narrow_more");
            h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
            h.size = Vec2::new(1280.0, 800.0);
            let first = h.w.order(0)[0];
            h.w.tabs[0].active = first;
            h.w.set_zoom(Some(first));
            h.frame(vec![Event::PointerMoved(Pos2::new(640.0, 2.0))]);
            save(&mut h, "zoom_dock");
            h.w.set_zoom(None);
            if let State::Live(r) = &mut h.w.panes[0].state {
                r.close();
            }
            h.until("down", |w| matches!(w.panes[0].state, State::Down(_)));
            h.frame(Vec::new());
            h.snap.save(&h.ctx, h.size, &dir.join(format!("ws_{name}_reconnecting.png")));
            h.w.close_tab(0);
            h.frame(Vec::new());
        }
    }

    /// Many tabs scroll as whole tabs, the current one always in view, with the + and the
    /// pane's controls kept clear; the list at the end goes to any tab, middle click closes one.
    #[test]
    fn many_tabs_scroll_and_keep_the_bar_whole() {
        let mut h = Harness::new();
        for i in 0..30 {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = format!("host{i:02}");
            h.w.add(
                Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
                Place::Tab,
                PaneStart::default(),
            );
        }
        let in_bar = |h: &Harness, what: &str| h.snap.find(what).into_iter().filter(|r| r.center().y < super::super::chrome::HEIGHT).collect::<Vec<_>>();
        for current in [29, 0, 17] {
            h.w.current = current;
            h.frame(Vec::new());
            h.frame(Vec::new());
            assert!(!in_bar(&h, &format!("host{current:02}")).is_empty(), "tab {current} is in view");
            let plus = in_bar(&h, ic::PLUS)[0];
            let mode = in_bar(&h, "Terminal")[0];
            assert!(plus.right() < mode.left(), "+ clear of the controls");
        }
        // The list is the caret before the + (the mode drop-down has one too).
        h.frame(Vec::new());
        let plus = in_bar(&h, ic::PLUS)[0];
        let carets = in_bar(&h, ic::CARET_DOWN);
        let list = carets.iter().find(|r| r.right() < plus.left()).unwrap_or_else(|| panic!("no list in {carets:?}, + at {plus:?}"));
        h.click_at(list.center(), PointerButton::Primary);
        h.item("host03");
        assert_eq!(h.w.current, 3, "the list goes to a tab");
        let at = in_bar(&h, "host04")[0].center();
        h.click_at(at, PointerButton::Middle);
        assert!(matches!(h.w.confirm, Some(Confirm::Tab(_))), "middle click asks to close the tab");
        h.frame(vec![Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        assert_eq!(h.w.tabs.len(), 29, "and closes it");
        h.frame(Vec::new());
        assert!(in_bar(&h, "host04").is_empty());
        h.w.close_all();
    }

    #[test]
    fn tab_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        for n in [1usize, 3] {
            let mut h = Harness::new();
            h.size = Vec2::new(480.0, 320.0);
            for i in 0..n {
                let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
                profile.name = format!("server-{i}");
                h.w.add(Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
            }
            for _ in 0..8 {
                h.frame(Vec::new());
            }
            h.snap.save(&h.ctx, h.size, &dir.join(format!("tabs_min_{n}.png")));
            h.w.close_all();
        }
        for n in [2usize, 8, 16, 30] {
            let mut h = Harness::new();
            for i in 0..n {
                let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
                profile.name = format!("server-{i}.example.com");
                h.w.add(
                    Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
                    Place::Tab,
                    PaneStart::default(),
                );
            }
            h.w.current = n / 2;
            for _ in 0..8 {
                h.frame(vec![Event::PointerMoved(Pos2::new(160.0, 20.0))]);
            }
            h.snap.save(&h.ctx, h.size, &dir.join(format!("tabs_{n}.png")));
            crate::gui::theme::apply_theme(&h.ctx, false);
            h.size = Vec2::new(760.0, 500.0);
            for _ in 0..8 {
                h.frame(Vec::new());
            }
            h.snap.save(&h.ctx, h.size, &dir.join(format!("tabs_{n}_light_narrow.png")));
            h.w.close_all();
        }
        // A tab dragged along the bar, one coming in from another window, and one out.
        let mut h = Harness::new();
        for name in ["alpha", "beta", "gamma"] {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            h.w.add(Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
        }
        h.frame(Vec::new());
        let a = h.snap.find("alpha")[0].center();
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        h.frame(vec![Event::PointerMoved(a)]);
        h.frame(vec![press(a, true)]);
        for k in 1..=6 {
            h.frame(vec![Event::PointerMoved(a + Vec2::new(22.0 * k as f32, 0.0))]);
        }
        for _ in 0..6 {
            h.frame(Vec::new());
        }
        h.snap.save(&h.ctx, h.size, &dir.join("tab_drag_float.png"));
        h.frame(vec![press(a + Vec2::new(132.0, 0.0), false)]);
        h.w.drop_hint = Some(2);
        h.frame(Vec::new());
        h.snap.save(&h.ctx, h.size, &dir.join("tab_drop_hint.png"));
        h.w.drop_hint = None;
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, h.size)), ..Default::default() };
        let w = &mut h.w;
        h.snap.frame(&h.ctx, input, |ui| {
            w.ui(ui, Vec::new());
            super::super::tab_ghost(ui.ctx(), ic::TERMINAL, "alpha", 1, Pos2::new(600.0, 400.0));
        });
        h.snap.save(&h.ctx, h.size, &dir.join("tab_ghost.png"));
        h.w.close_all();
    }

    /// Tabs dragged along the bar change places; one dragged away from the bar leaves for a
    /// window of its own, with its panes.
    #[test]
    fn tabs_drag_to_reorder_and_tear_off() {
        let mut h = Harness::new();
        let shell = |name: &str| {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        h.w.add(shell("first"), Place::Tab, PaneStart::default());
        h.w.add(shell("second"), Place::Tab, PaneStart::default());
        h.frame(Vec::new());
        h.frame(Vec::new());
        let at = |h: &Harness, name: &str| {
            h.snap.find(name).into_iter().find(|r| r.center().y < super::super::chrome::HEIGHT).unwrap().center()
        };
        let drag = |h: &mut Harness, from: Pos2, to: Pos2| {
            let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
            h.frame(vec![Event::PointerMoved(from)]);
            h.frame(vec![press(from, true)]);
            for k in 1..=8 {
                h.frame(vec![Event::PointerMoved(from + (to - from) * (k as f32 / 8.0))]);
            }
            h.frame(vec![press(to, false)]);
            h.frame(Vec::new());
        };
        let names = |w: &Workspace| -> Vec<String> { (0..w.tabs.len()).map(|t| w.name(w.tabs[t].active).0).collect() };
        assert_eq!(names(&h.w), ["first", "second"]);
        let (a, b) = (at(&h, "first"), at(&h, "second"));
        drag(&mut h, a, b);
        assert_eq!(names(&h.w), ["second", "first"], "dragged along the bar");
        // Away from the bar it is out while held, and handed to the app when let go there.
        let a = at(&h, "first");
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        h.frame(vec![Event::PointerMoved(a)]);
        h.frame(vec![press(a, true)]);
        for k in 1..=8 {
            h.frame(vec![Event::PointerMoved(a + Vec2::new(0.0, 40.0 * k as f32))]);
        }
        assert_eq!(h.w.drag_out, Some(a + Vec2::new(0.0, 320.0)), "out while held");
        h.frame(vec![press(a + Vec2::new(0.0, 320.0), false)]);
        assert_eq!(h.w.dropped, Some((1, a + Vec2::new(0.0, 320.0))), "let go away from the bar");
        assert_eq!(h.w.drag_out, None);
        assert_eq!(names(&h.w), ["second", "first"], "the app takes it from here");
        h.w.close_all();
    }

    /// A tab from another window goes in where it was dropped, its panes numbered after these.
    #[test]
    fn a_tab_from_another_window_goes_in_where_dropped() {
        let mut h = Harness::new();
        let shell = |name: &str| {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        h.w.add(shell("a"), Place::Tab, PaneStart::default());
        h.w.add(shell("b"), Place::Tab, PaneStart::default());
        let mut other = Workspace::new(h.w.opt.clone());
        other.add(shell("c"), Place::Tab, PaneStart::default());
        other.add(shell("d"), Place::Split { below: false }, PaneStart::default());
        let moved = other.take_tab(0).expect("a tab");
        h.w.put_tab(moved, 1);
        h.frame(Vec::new());
        let names: Vec<String> = (0..h.w.tabs.len()).map(|t| h.w.name(h.w.tabs[t].active).0).collect();
        assert_eq!(names, ["a", "d", "b"], "in between, its active pane named");
        assert_eq!(h.w.current, 1, "and shown");
        let mut ids: Vec<u64> = h.w.panes.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4, "every pane its own number");
        assert_eq!(h.w.order(1).len(), 2, "with both its panes");
        h.w.close_all();
        other.close_all();
    }

    /// Windows Terminal's keys: new tab, next tab, split, between panes, close pane.
    #[test]
    fn terminal_keys_for_tabs_and_panes() {
        let mut h = Harness::new();
        let shell = |name: &str| {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        h.w.add(shell("a"), Place::Tab, PaneStart::default());
        h.w.add(shell("b"), Place::Tab, PaneStart::default());
        h.w.add(shell("c"), Place::Split { below: false }, PaneStart::default());
        h.frame(Vec::new());
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let cs = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        let alt = egui::Modifiers::ALT;
        h.frame(vec![key(egui::Key::T, cs)]);
        assert_eq!(h.w.picker, Some(Place::Tab), "Ctrl+Shift+T opens the dialog");
        h.w.picker = None;
        h.frame(vec![key(egui::Key::Tab, egui::Modifiers::CTRL)]);
        assert_eq!(h.w.current, 0, "Ctrl+Tab: the next tab, round to the first");
        h.frame(vec![key(egui::Key::Tab, egui::Modifiers::CTRL)]);
        assert_eq!(h.w.current, 1);
        let order = h.w.order(1);
        assert_eq!(h.w.tabs[1].active, order[1]);
        h.frame(vec![key(egui::Key::ArrowLeft, alt)]);
        assert_eq!(h.w.tabs[1].active, order[0], "Alt+Left: the pane before");
        h.frame(vec![key(egui::Key::Minus, egui::Modifiers::ALT | egui::Modifiers::SHIFT)]);
        assert_eq!(h.w.picker, Some(Place::Split { below: true }), "Alt+Shift+-: split below");
        h.w.picker = None;
        h.frame(vec![key(egui::Key::W, cs)]);
        assert_eq!(h.w.confirm, Some(Confirm::Pane(order[0])), "Ctrl+Shift+W asks first");
        h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
        assert_eq!(h.w.order(1), vec![order[1]], "and closes the pane once told yes");
        h.w.close_all();
    }

    /// A screen in a tab not shown stops its picture, so it costs the link nothing, and has it
    /// again when shown.
    #[test]
    #[ignore]
    fn a_screen_in_a_hidden_tab_stops_its_picture() {
        let mut h = Harness::new();
        h.w.add(test_desktop(), Place::Tab, PaneStart::default());
        h.until("the test desktop connects", |w| live(w, 0));
        let frames = |w: &Workspace| match &w.panes[0].state {
            State::Live(r) => r.frames(),
            _ => 0,
        };
        let settle = |h: &mut Harness, n: usize| {
            for _ in 0..n {
                h.frame(Vec::new());
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
        };
        settle(&mut h, 20);
        // How many come in a while shown, then in as long hidden.
        let start = frames(&h.w);
        settle(&mut h, 70);
        let shown = frames(&h.w) - start;
        assert!(shown >= 4, "pictures while shown: {shown}");
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(Launch::Shell { label: "shell".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
        settle(&mut h, 15);
        let hidden = frames(&h.w);
        settle(&mut h, 70);
        assert!(frames(&h.w) <= hidden + 1, "none while hidden ({shown} when shown): {hidden} then {}", frames(&h.w));
        h.w.select_tab(0);
        h.until("pictures again when shown", |w| frames(w) > hidden + 2);
        h.w.close_all();
    }

    /// Panes in different windows never share an id, so extensions told of them never see one
    /// twice, or a "closed" for a pane still open elsewhere.
    #[test]
    fn pane_ids_are_unique_across_windows() {
        let profile = || crate::termwin::local_profiles().into_iter().next().unwrap();
        let shell = || Launch::Shell { label: "s".into(), profile: profile(), target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
        let opt = crate::parse_client(&["--gui".into(), "--local".into()]).unwrap();
        let mut a = Workspace::new(opt.clone());
        a.add(shell(), Place::Tab, PaneStart::default());
        a.add(shell(), Place::Tab, PaneStart::default());
        let mut b = a.take_tab(1).expect("a tab");
        a.add(shell(), Place::Tab, PaneStart::default());
        b.add(shell(), Place::Tab, PaneStart::default());
        let mut ids: Vec<u64> = a.panes.iter().chain(&b.panes).map(|p| p.id).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "all different: {ids:?}");
        a.put_tab(b.take_tab(0).unwrap(), 0);
        let mut back: Vec<u64> = a.panes.iter().map(|p| p.id).collect();
        back.sort();
        back.dedup();
        assert_eq!(back.len(), a.panes.len());
        a.close_all();
        b.close_all();
    }

    /// A shell in a tab not shown keeps taking its output, so it does not stall.
    #[test]
    fn shells_in_hidden_tabs_keep_running() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        let shell = |profile: crate::termwin::Profile| Launch::Shell { label: "s".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
        h.w.add(shell(profile.clone()), Place::Tab, PaneStart::default());
        h.w.add(shell(profile), Place::Tab, PaneStart::default());
        h.w.current = 1;
        let hidden = h.w.tabs[0].active;
        for _ in 0..60 {
            h.frame(Vec::new());
            std::thread::sleep(std::time::Duration::from_millis(50));
            let i = h.w.index(hidden).unwrap();
            if let State::Shell(t) = &h.w.panes[i].state
                && t.output_seen() > 0
            {
                break;
            }
        }
        let i = h.w.index(hidden).unwrap();
        let State::Shell(t) = &h.w.panes[i].state else { panic!("a shell") };
        assert!(t.output_seen() > 0, "the hidden shell's prompt was read");
        h.w.close_all();
    }

    /// While a dialog asks something, the panes behind it get no keys; closing the window asks
    /// too; F11 and Ctrl+Shift+Enter fill the screen with the window or the pane.
    #[test]
    fn dialogs_keep_the_keys_and_the_window_asks_before_closing() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(Launch::Shell { label: "s".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
        h.frame(Vec::new());
        let id = h.w.tabs[0].active;
        let keys_on = |h: &Harness| match &h.w.panes[0].state {
            State::Shell(t) => t.keys_on,
            _ => false,
        };
        assert!(keys_on(&h));
        h.w.confirm = Some(Confirm::Pane(id));
        h.frame(Vec::new());
        assert!(!keys_on(&h), "the dialog has the keys");
        h.w.confirm = None;
        h.frame(Vec::new());
        assert!(keys_on(&h));
        // The system's close (Alt+F4): asked, not done.
        h.clock += 0.05;
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, h.size)),
            time: Some(h.clock),
            focused: true,
            ..Default::default()
        };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        let w = &mut h.w;
        h.snap.frame(&h.ctx, input, |ui| {
            w.ui(ui, Vec::new());
        });
        assert_eq!(h.w.confirm, Some(Confirm::Window), "closing the window asks");
        assert_eq!(h.w.panes.len(), 1);
        h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        assert_eq!(h.w.confirm, None);
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        h.frame(vec![key(egui::Key::F11, egui::Modifiers::NONE)]);
        assert!(h.w.full.on, "F11: the window's full screen");
        h.frame(vec![key(egui::Key::F11, egui::Modifiers::NONE)]);
        assert!(!h.w.full.on);
        h.frame(vec![key(egui::Key::Enter, egui::Modifiers::CTRL | egui::Modifiers::SHIFT)]);
        assert_eq!(h.w.zoom, Some(id), "Ctrl+Shift+Enter: the pane's");
        h.w.close_all();
    }

    /// Points of windows on monitors of different scales meet in screen pixels.
    #[test]
    fn screen_points_are_pixels() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        let info = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        info.inner_rect = Some(Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0)));
        info.native_pixels_per_point = Some(1.5);
        let mut got = Pos2::ZERO;
        let _ = ctx.run_test(input.clone(), |ui| got = screen_point(ui.ctx(), Pos2::new(10.0, 20.0)));
        assert_eq!(got, Pos2::new(165.0, 105.0));
        // With the page zoomed, a point in it is that much further from the window's corner, and
        // a place on the screen comes back to the same point.
        ctx.set_zoom_factor(1.25);
        let mut back = Pos2::ZERO;
        let _ = ctx.run_test(input, |ui| {
            got = screen_point(ui.ctx(), Pos2::new(10.0, 20.0));
            back = window_point(ui.ctx(), got);
        });
        assert_eq!(got, Pos2::new(150.0 + 18.75, 75.0 + 37.5));
        assert!((back - Pos2::new(10.0, 20.0)).length() < 0.01, "{back:?}");
    }

    /// Let go outside the window, a tab drag ends even when the pointer is gone in that frame.
    #[test]
    fn a_drag_let_go_outside_ends() {
        let mut h = Harness::new();
        for name in ["a", "b"] {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            h.w.add(Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
        }
        h.frame(Vec::new());
        let a = h.snap.find("a").into_iter().find(|r| r.center().y < super::super::chrome::HEIGHT).unwrap().center();
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        h.frame(vec![Event::PointerMoved(a)]);
        h.frame(vec![press(a, true)]);
        for k in 1..=6 {
            h.frame(vec![Event::PointerMoved(a + Vec2::new(0.0, 60.0 * k as f32))]);
        }
        let out = a + Vec2::new(0.0, 360.0);
        h.frame(vec![press(out, false), Event::PointerGone]);
        assert_eq!(h.w.dragging_tab(), None, "the drag ended");
        assert!(h.w.dropped.is_some(), "and the tab was let go away from the bar");
        h.w.close_all();
    }

    /// Closing a tab or a pane asks first: Escape keeps it, Enter closes it.
    #[test]
    fn closing_asks_first() {
        let mut h = Harness::new();
        let shell = |name: &str| {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        h.w.add(shell("one"), Place::Tab, PaneStart::default());
        h.w.add(shell("two"), Place::Tab, PaneStart::default());
        h.w.add(shell("three"), Place::Split { below: false }, PaneStart::default());
        h.frame(Vec::new());
        let key = |key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        // The pane's own close, last of its controls (the tab's is on the left).
        for _ in 0..4 {
            h.frame(Vec::new());
        }
        let x = h.snap.find(ic::X).into_iter().filter(|r| r.center().y < super::super::chrome::HEIGHT).max_by(|a, b| a.left().total_cmp(&b.left())).unwrap();
        h.click_at(x.center(), PointerButton::Primary);
        assert!(matches!(h.w.confirm, Some(Confirm::Pane(_))), "a pane's close asks");
        assert!(!h.snap.find("Close three?").is_empty(), "naming the pane");
        h.frame(vec![key(egui::Key::Escape)]);
        assert_eq!(h.w.confirm, None);
        assert_eq!(h.w.order(1).len(), 2, "Escape keeps it");
        // A tab's close: the whole tab, its panes too.
        let id = h.w.tabs[1].active;
        h.w.confirm = Some(Confirm::Tab(id));
        h.frame(Vec::new());
        assert!(!h.snap.find("Its 2 panes close").is_empty());
        h.frame(vec![key(egui::Key::Enter)]);
        assert_eq!(h.w.tabs.len(), 1, "the tab went, with both panes");
        assert_eq!(h.w.panes.len(), 1);
        h.w.close_all();
    }

    /// Every pane can fill the window: a terminal gets the button among its controls.
    /// What extensions add, driven as a user would: a panel's link goes back to its extension as
    /// an action, the palette runs what is chosen, and a terminal asked for opens.
    #[test]
    fn extensions_show_panels_take_commands_and_open_terminals() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(Launch::Shell { label: "t".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
        let md = "Tunnels

| Host | |
|---|---|
| db | [Start](sshremote:start?host=db) |
";
        crate::exthost::test_message("t.panel", serde_json::json!({ "method": "panel.show", "params": { "id": "p", "title": "Tunnels here", "markdown": md } }));
        crate::exthost::test_message("t.panel", serde_json::json!({ "method": "status.set", "params": { "id": "n", "text": "2 tunnels" } }));
        for _ in 0..4 {
            h.frame(Vec::new());
        }
        assert!(!h.snap.find("Tunnels here").is_empty() && !h.snap.find("2 tunnels").is_empty(), "the panel and the status item show");
        if let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR") {
            h.snap.save(&h.ctx, h.size, &std::path::Path::new(&dir).join("ext_panel.png"));
        }
        let start = h.snap.find("Start")[0].center();
        crate::exthost::test_sent();
        h.frame(vec![Event::PointerMoved(start)]);
        let press = |pressed| Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        h.frame(vec![press(true)]);
        h.frame(vec![press(false)]);
        let sent = crate::exthost::test_sent();
        assert!(
            sent.iter().any(|(e, m, p)| e == "t.panel" && m == "action" && p["action"] == "start" && p["args"]["host"] == "db"),
            "the link came back as an action: {sent:?}"
        );
        // The palette: Ctrl+Shift+P, a filter, Enter.
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        h.frame(vec![Event::PointerMoved(Pos2::new(300.0, 300.0))]);
        h.frame(Vec::new());
        h.frame(vec![key(egui::Key::P, egui::Modifiers::CTRL | egui::Modifiers::SHIFT)]);
        h.frame(Vec::new());
        h.frame(Vec::new());
        assert!(!h.snap.find("Split down").is_empty(), "the palette lists the workspace's commands");
        h.frame(vec![Event::Text("split down".into())]);
        h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
        assert_eq!(h.w.picker, Some(Place::Split { below: true }), "Enter ran the command");
        h.w.picker = None;
        // A terminal an extension asks for opens beside the active pane.
        crate::exthost::test_request(crate::exthost::Request::OpenTerminal { argv: vec!["ssh".into(), "db".into()], label: "db".into(), split: Some(false) });
        h.frame(Vec::new());
        assert!(h.w.panes.iter().any(|p| p.label == "db"), "the terminal opened");
        // An extension's question: a dialog, an answer typed, Enter; the answer goes back.
        crate::exthost::test_sent();
        crate::exthost::test_message("t.panel", serde_json::json!({ "id": 5, "method": "input.ask", "params": { "title": "Port", "prompt": "Which port to forward?" } }));
        h.frame(Vec::new());
        h.frame(Vec::new());
        assert!(!h.snap.find("Which port to forward?").is_empty(), "the question shows");
        h.frame(vec![Event::Text("8080".into())]);
        h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
        let sent = crate::exthost::test_sent();
        assert!(sent.iter().any(|(e, m, p)| e == "t.panel" && m == "reply" && p["id"] == 5 && p["result"]["value"] == "8080"), "{sent:?}");
        // A form: the first field typed in, the box ticked, Enter; the values come back by id.
        crate::exthost::test_message("t.panel", serde_json::json!({ "id": 6, "method": "input.form", "params": { "title": "Forward", "submit": "Forward", "fields": [
            { "id": "dest", "label": "Leads to", "type": "text" },
            { "id": "tls", "label": "Use TLS", "type": "bool" },
            { "id": "host", "label": "Host", "type": "choice", "choices": ["db", "web"] }
        ] } }));
        h.frame(Vec::new());
        h.frame(Vec::new());
        assert!(!h.snap.find("Leads to").is_empty() && !h.snap.find("Use TLS").is_empty(), "the form's fields show");
        h.frame(vec![Event::Text("localhost:80".into())]);
        let tick = h.snap.find("Use TLS")[0];
        let at = Pos2::new(tick.right() + 20.0, tick.center().y);
        h.frame(vec![Event::PointerMoved(at)]);
        let press = |pressed| Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        h.frame(vec![press(true)]);
        h.frame(vec![press(false)]);
        h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
        let sent = crate::exthost::test_sent();
        assert!(
            sent.iter().any(|(_, m, p)| m == "reply" && p["id"] == 6 && p["result"]["values"] == serde_json::json!({ "dest": "localhost:80", "tls": true, "host": "db" })),
            "{sent:?}"
        );
        crate::exthost::test_message("t.panel", serde_json::json!({ "method": "exit" }));
    }

    #[test]
    fn a_terminal_pane_fills_the_window() {
        let mut h = Harness::new();
        let shell = || {
            let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            Launch::Shell { label: "t".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        // One pane: nothing to zoom; the button shows, greyed, as the layout menu does.
        h.w.add(shell(), Place::Tab, PaneStart::default());
        for _ in 0..4 {
            h.frame(Vec::new());
        }
        assert!(!h.snap.find(ic::CORNERS_OUT).is_empty(), "the zoom button shows with one pane");
        h.bar(ic::CORNERS_OUT);
        assert_eq!(h.w.zoom, None, "and does nothing");
        h.w.add(shell(), Place::Split { below: false }, PaneStart::default());
        for _ in 0..4 {
            h.frame(Vec::new());
        }
        h.bar(ic::CORNERS_OUT);
        assert_eq!(h.w.zoom, Some(h.w.tabs[0].active), "the pane alone, in the tab");
        assert!(!h.w.full.on, "the window stays as it is");
        for _ in 0..3 {
            h.frame(Vec::new());
        }
        assert!(h.snap.find(ic::PLUS).iter().any(|r| r.center().y < super::super::chrome::HEIGHT), "the tabs stay");
        h.bar(ic::CORNERS_IN);
        assert_eq!(h.w.zoom, None, "back to the grid");
        // The window's full screen is its own button, at the right, and keeps a grid of shells.
        h.bar(ic::ARROWS_OUT);
        assert!(h.w.full.on && h.w.zoom.is_none());
        h.w.close_all();
    }

    /// Splits share the room evenly; the layouts arrange a tab as tmux's do.
    #[test]
    fn splits_are_even_and_layouts_arrange() {
        let rects = |node: &Node| {
            fn walk(n: &Node, r: Rect, out: &mut Vec<(u64, Rect)>) {
                match n {
                    Node::Leaf(id) => out.push((*id, r)),
                    Node::Split { below, ratio, a, b } => {
                        let (ra, rb) = if *below {
                            let y = r.top() + r.height() * ratio;
                            (r.with_max_y(y), r.with_min_y(y))
                        } else {
                            let x = r.left() + r.width() * ratio;
                            (r.with_max_x(x), r.with_min_x(x))
                        };
                        walk(a, ra, out);
                        walk(b, rb, out);
                    }
                }
            }
            let mut out = Vec::new();
            walk(node, Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 600.0)), &mut out);
            out
        };
        // Three side by side, the third split off the second: a third each.
        let mut root = Node::Leaf(1);
        root.split(1, 2, false);
        root.even_out(2, false);
        root.split(2, 3, false);
        root.even_out(3, false);
        for (_, r) in rects(&root) {
            assert!((r.width() - 400.0).abs() < 0.5, "{r:?}");
        }
        // A size set by hand elsewhere stays: one wide on the left, a stack on the right split.
        let mut root = Node::Leaf(1);
        root.split(1, 2, false);
        root.split(2, 3, true);
        if let Node::Split { ratio, .. } = &mut root {
            *ratio = 0.7;
        }
        root.split(3, 4, false);
        root.even_out(4, false);
        let r = rects(&root);
        assert!((r[0].1.width() - 840.0).abs() < 0.5, "the hand-set width stays: {r:?}");
        assert!((r[2].1.width() - r[3].1.width()).abs() < 0.5, "the new pair is even: {r:?}");
        // Mixed: one on the left, two stacked on the right; the stack's panes share its height.
        let mut root = Node::Leaf(1);
        root.split(1, 2, false);
        root.split(2, 3, true);
        root.balance();
        let r = rects(&root);
        assert!((r[0].1.width() - 600.0).abs() < 0.5 && (r[1].1.height() - 300.0).abs() < 0.5, "{r:?}");
        let ids = [1, 2, 3, 4, 5];
        let side = rects(&Node::arranged(&ids, 3, Layout::Side));
        assert!(side.iter().all(|(_, r)| (r.width() - 240.0).abs() < 0.5 && r.height() == 600.0));
        let stacked = rects(&Node::arranged(&ids, 3, Layout::Stacked));
        assert!(stacked.iter().all(|(_, r)| (r.height() - 120.0).abs() < 0.5));
        let main = rects(&Node::arranged(&ids, 3, Layout::MainLeft));
        assert_eq!(main[0].0, 3, "the active pane is the main one");
        assert!((main[0].1.width() - 720.0).abs() < 0.5 && (main[1].1.height() - 150.0).abs() < 0.5);
        let top = rects(&Node::arranged(&ids, 3, Layout::MainTop));
        assert_eq!(top[0].0, 3);
        assert!((top[0].1.height() - 360.0).abs() < 0.5 && (top[1].1.width() - 300.0).abs() < 0.5);
        let grid = rects(&Node::arranged(&[1, 2, 3, 4], 1, Layout::Grid));
        assert!(grid.iter().all(|(_, r)| (r.width() - 600.0).abs() < 0.5 && (r.height() - 300.0).abs() < 0.5), "{grid:?}");
        let grid = rects(&Node::arranged(&ids, 1, Layout::Grid));
        assert_eq!(grid.len(), 5);
        assert!((grid[0].1.height() - 300.0).abs() < 0.5, "two rows: {grid:?}");
    }

    /// Closing panes and tabs keeps the rest as it was: the current tab, full screen off once
    /// the pane that filled the screen is gone, and Alt+arrows left to a lone shell.
    #[test]
    fn closing_keeps_the_rest_in_order() {
        let mut h = Harness::new();
        let shell = |name: &str| {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = name.into();
            Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
        };
        for n in ["a", "b", "c"] {
            h.w.add(shell(n), Place::Tab, PaneStart::default());
        }
        h.w.current = 1;
        h.frame(Vec::new());
        h.w.close_tab(0);
        assert_eq!(h.w.name(h.w.tabs[h.w.current].active).0, "b", "the current tab stays");
        // A zoomed pane, then closed: the tab is a grid again.
        h.w.add(shell("d"), Place::Split { below: false }, PaneStart::default());
        let d = h.w.tabs[h.w.current].active;
        h.w.set_zoom(Some(d));
        h.frame(Vec::new());
        assert_eq!(h.w.zoom, Some(d));
        h.w.close_pane(d);
        h.frame(Vec::new());
        assert_eq!(h.w.zoom, None, "the zoom ends with its pane");
        // Alt+Left in a tab of one pane is the shell's.
        h.frame(vec![Event::Key { key: egui::Key::ArrowLeft, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::ALT }]);
        let kept = h.ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::ArrowLeft, .. })));
        assert!(kept, "Alt+Left reaches the shell");
        h.w.close_all();
    }

    /// A shell turned to the screen of a host without the agent, then back to Terminal, is the
    /// same shell again.
    #[test]
    fn a_shell_that_cannot_reach_the_agent_stays_a_shell() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        let target = Target::Ssh {
            host: "nobody@127.0.0.1".into(),
            port: Some(1),
            args: vec!["-o".into(), "ConnectTimeout=2".into()],
            remote: "sshremote --agent".into(),
            capture: "auto".into(),
            password: None,
            unattended: true,
        };
        h.w.add(Launch::Shell { label: "s".into(), profile, target, ssh_cmd: None }, Place::Tab, PaneStart::default());
        h.frame(Vec::new());
        let id = h.w.tabs[0].active;
        h.w.set_mode(id, PaneMode::Screen);
        assert!(matches!(h.w.panes[0].state, State::Down(_)));
        h.w.set_mode(id, PaneMode::Terminal);
        assert!(matches!(h.w.panes[0].state, State::Shell(_)), "back to the shell it was");
        h.w.close_all();
    }

    /// In full screen the title bar gives way to a dock at the top edge: folded, the pane's state and
    /// a caret; unfolded, the bar's tabs and controls.
    #[test]
    fn full_screen_dock_folds_and_unfolds() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(
            Launch::Shell { label: "shell".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
            Place::Tab,
            PaneStart::default(),
        );
        h.frame(Vec::new());
        h.bar(ic::ARROWS_OUT);
        assert!(h.w.full.on);
        let away = Pos2::new(600.0, 500.0);
        let tabs_shown = |h: &Harness| !h.snap.find(ic::PLUS).is_empty();
        h.frame(vec![Event::PointerMoved(away)]);
        assert!(!tabs_shown(&h), "folded: no title bar");
        assert!(h.w.panes_area.top() < 1.0, "the panes start at the top: {:?}", h.w.panes_area);
        assert!(!h.snap.find(ic::CARET_DOWN).is_empty(), "the folded dock greets at first");
        for _ in 0..80 {
            h.frame(vec![Event::PointerMoved(away)]);
        }
        assert!(h.snap.find(ic::CARET_DOWN).is_empty(), "then steps aside: the picture is whole");
        h.frame(vec![Event::PointerMoved(Pos2::new(900.0, 1.0))]);
        h.frame(Vec::new());
        let caret = h.snap.find(ic::CARET_DOWN).into_iter().next().expect("back at the top edge");
        assert!(caret.top() < 20.0 && caret.height() < 16.0, "a small tab at the top edge: {caret:?}");
        let name = h.snap.find("shell").into_iter().next().expect("the pane's name in the tab");
        assert!(name.height() < 16.0, "in small type: {name:?}");
        // A click on the tab unfolds it: the band takes its place at the top edge.
        h.click_at(name.center(), PointerButton::Primary);
        assert!(h.w.dock.open && tabs_shown(&h), "unfolded");
        let plus = h.snap.find(ic::PLUS).into_iter().next().unwrap();
        assert!(plus.top() < 20.0, "the band hangs from the top edge, not below the tab: {plus:?}");
        assert!(h.snap.find("shell").iter().all(|r| r.height() > 12.0), "the tab gave way to the band");
        // Away from it, it folds by itself after a moment.
        for _ in 0..60 {
            h.frame(vec![Event::PointerMoved(away)]);
        }
        assert!(!h.w.dock.open && !tabs_shown(&h), "folded again");
        // Ctrl+Alt+Home unfolds and folds it.
        let home = |pressed| Event::Key { key: egui::Key::Home, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::CTRL | egui::Modifiers::ALT };
        h.frame(vec![home(true)]);
        h.frame(vec![home(false)]);
        assert!(h.w.dock.open, "Ctrl+Alt+Home unfolds it");
        // The band's own caret folds it back into the tab.
        let fold = h.snap.find(ic::CARET_UP).into_iter().next().expect("the band's fold caret");
        h.click_at(fold.center(), PointerButton::Primary);
        assert!(!h.w.dock.open && !tabs_shown(&h), "folded into the tab");
        assert!(!h.snap.find(ic::CARET_DOWN).is_empty(), "the tab is back");
        // Leaving full screen from the band.
        h.frame(vec![Event::PointerMoved(Pos2::new(900.0, 1.0))]);
        let name = h.snap.find("shell").into_iter().next().expect("the tab");
        h.click_at(name.center(), PointerButton::Primary);
        let leave = h.snap.find(ic::ARROWS_IN).into_iter().next().expect("leave full screen in the band");
        h.click_at(leave.center(), PointerButton::Primary);
        assert!(!h.w.full.on, "left full screen");
        h.frame(Vec::new());
        assert!(tabs_shown(&h), "the title bar is back");
        h.w.close_all();
    }

    /// In full screen a screen pane that has the window to itself fills it to the edges, zoomed or
    /// as the window's only pane.
    #[test]
    #[ignore]
    fn full_screen_pictures_fill_the_window() {
        let mut h = Harness::new();
        h.w.add(test_desktop(), Place::Tab, PaneStart::default());
        h.until("connected", |w| live(w, 0));
        let ctx = h.ctx.clone();
        h.w.set_full(&ctx, true);
        let shown_of = |w: &Workspace| match &w.panes[0].state {
            State::Live(r) => r.shown_rect(),
            _ => None,
        };
        h.until("a picture", |w| shown_of(w).is_some());
        for _ in 0..5 {
            h.frame(Vec::new());
        }
        let shown = shown_of(&h.w).expect("a picture");
        assert_eq!(shown, h.w.panes_area, "edge to edge, no bars");
        h.w.close_all();
    }

    /// "This computer": its terminal turned to the screen goes on to the local agent's picture.
    #[test]
    #[ignore]
    fn a_local_terminal_turns_to_this_computers_screen() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(Launch::Shell { label: "PowerShell".into(), profile, target: Target::Local { capture: "auto".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
        h.frame(Vec::new());
        let id = h.w.tabs[0].active;
        h.w.set_mode(id, PaneMode::Screen);
        h.until("this computer's screen", |w| live(w, 0));
        for _ in 0..20 {
            h.frame(Vec::new());
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        assert!(live(&h.w, 0), "still there");
        assert_eq!(mode_of(&h.w, 0), Some(PaneMode::Screen));
        h.w.close_all();
    }

    /// A first connection that fails asks at once: Reconnect or Close.
    #[test]
    #[ignore]
    fn a_failed_connection_offers_reconnect_or_close() {
        let mut h = Harness::new();
        let target = Target::Ssh {
            host: "nobody@127.0.0.1".into(),
            port: Some(1),
            args: vec!["-o".into(), "ConnectTimeout=2".into()],
            remote: "sshremote --agent".into(),
            capture: "auto".into(),
            password: None,
            unattended: true,
        };
        h.w.add(Launch::Agent { target, label: "nowhere".into(), ssh_cmd: None, mode: PaneMode::Screen }, Place::Tab, PaneStart::default());
        h.until("it gives up", |w| matches!(&w.panes[0].state, State::Down(d) if d.given_up));
        h.frame(Vec::new());
        assert!(!h.snap.find("Reconnect").is_empty(), "Reconnect is offered");
        assert!(!h.snap.find("Close").is_empty(), "Close is offered");
        assert!(h.snap.find("Cancel").is_empty(), "no Cancel once given up");
        h.item("Close");
        assert!(h.w.panes.is_empty(), "Close closes the pane");
    }
}
