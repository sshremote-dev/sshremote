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

    pub(crate) fn glyph(self) -> &'static str {
        match self {
            PaneMode::Terminal => ic::TERMINAL,
            PaneMode::Screen => ic::MONITOR,
            PaneMode::Files => ic::FOLDER,
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
}

impl Down {
    fn new(again: bool, why: String) -> Down {
        let now = Instant::now();
        Down { rx: None, since: now, next: now, attempt: 0, why, given_up: false, again }
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
    start: (bool, bool, bool),
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
                let r = ui.interact(line, egui::Id::unique(("pane-split", path)), egui::Sense::drag());
                if r.hovered() || r.dragged() {
                    ui.ctx().set_cursor_icon(if *below {
                        egui::CursorIcon::ResizeVertical
                    } else {
                        egui::CursorIcon::ResizeHorizontal
                    });
                }
                if r.dragged() {
                    let d = if *below { r.drag_delta().y / rect.height() } else { r.drag_delta().x / rect.width() };
                    *ratio = (*ratio + d).clamp(0.12, 0.88);
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
    next_id: u64,
    /// One pane fills the screen (its own full screen).
    zoom: Option<u64>,
    /// Whether the window was last told to be full screen: it follows `zoom` and `window_full`.
    sent_full: bool,
    /// The whole window, split as it is, fills the screen.
    window_full: bool,
    /// Until when the title bar stays shown over a full-screen window, and the zoomed pane's dock.
    reveal_until: f64,
    /// Hosts reached, for the recent list.
    pub(super) reached: Vec<String>,
    /// The width the active pane's controls took last frame, to know when they must fold.
    controls_w: f32,
    /// The tab being dragged (to a new place, or out into a window of its own).
    dragging: Option<usize>,
    /// Tabs dragged out of the bar: each becomes a window of its own (the app opens them).
    pub(super) torn_out: Vec<Workspace>,
    /// The mode drop-down's width last frame.
    mode_w: f32,
    title: String,
    /// The first tab shown when they do not all fit, the wheel not yet a whole step, and the
    /// current tab and room it was last brought into view for.
    tab_first: usize,
    tab_wheel: f32,
    scrolled_for: Option<(usize, usize)>,
}

/// A tab's width: as wide as fits, between these; past the narrowest the tabs scroll.
const TAB_MAX: f32 = 220.0;
const TAB_MIN: f32 = 120.0;
/// The + after the tabs.
const PLUS_W: f32 = 34.0;

impl Workspace {
    pub(super) fn new(opt: Options) -> Workspace {
        Workspace {
            tabs: Vec::new(),
            panes: Vec::new(),
            current: 0,
            picker: None,
            opt,
            next_id: 1,
            zoom: None,
            sent_full: false,
            window_full: false,
            reveal_until: 0.0,
            reached: Vec::new(),
            controls_w: 260.0,
            dragging: None,
            torn_out: Vec::new(),
            mode_w: 150.0,
            title: String::new(),
            tab_first: 0,
            tab_wheel: 0.0,
            scrolled_for: None,
        }
    }

    /// Opens what the open view chose, in a new tab or beside the active pane.
    pub(super) fn add(&mut self, launch: Launch, place: Place, start: (bool, bool, bool)) {
        let id = self.next_id;
        self.next_id += 1;
        let pane = match launch {
            Launch::Shell { profile, label, target, ssh_cmd } => Pane {
                id,
                label,
                target,
                ssh_cmd,
                mode: PaneMode::Terminal,
                terms: None,
                start,
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
                state: State::Down(Down::new(false, String::new())),
            },
        };
        self.panes.push(pane);
        match place {
            Place::Split { below } if !self.tabs.is_empty() => {
                let tab = &mut self.tabs[self.current];
                tab.root.split(tab.active, id, below);
                tab.active = id;
            }
            _ => {
                self.tabs.push(Tab { root: Node::Leaf(id), active: id });
                self.current = self.tabs.len() - 1;
            }
        }
        self.zoom = None;
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
    fn take_tab(&mut self, t: usize) -> Workspace {
        let tab = self.tabs.remove(t);
        let mut ids = Vec::new();
        tab.root.leaves(&mut ids);
        let mut w = Workspace::new(self.opt.clone());
        let (moved, kept): (Vec<Pane>, Vec<Pane>) = std::mem::take(&mut self.panes).into_iter().partition(|p| ids.contains(&p.id));
        self.panes = kept;
        w.panes = moved;
        w.tabs.push(tab);
        // Ids stay unique in each: the new one numbers its panes after these.
        w.next_id = self.next_id;
        if self.zoom.is_some_and(|z| ids.contains(&z)) {
            self.zoom = None;
        }
        self.current = self.current.min(self.tabs.len().saturating_sub(1));
        w
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
    }

    fn go_live(&mut self, i: usize, sess: Session, rx: mpsc::Receiver<Ev>, ctx: &egui::Context) {
        let ctl = Control::new(&self.opt, Mode::True);
        let waker = ctx.clone();
        sess.on_message(move || waker.request_repaint());
        let p = &mut self.panes[i];
        let label = format!("{}  {}x{}  {}", p.label, sess.desk.0, sess.desk.1, sess.backend);
        let mut r = Remote::new(sess, rx, ctl, label, p.ssh_cmd.clone(), false);
        r.in_pane = true;
        if let Some(t) = p.terms.take() {
            r.adopt_terms(t);
        }
        let result = r.set_mode(p.mode);
        let (panel, chat, full) = std::mem::take(&mut p.start);
        r.open_panels(false, chat);
        self.reached.push(p.label.clone());
        p.state = State::Live(Box::new(r));
        let id = p.id;
        // The terminal asked for beside the screen is a pane of its own below it, as every pane is.
        if panel {
            let launch = Launch::Agent { target: p.target.clone(), label: p.label.clone(), ssh_cmd: p.ssh_cmd.clone(), mode: PaneMode::Terminal };
            let tab = self.tabs.iter().position(|t| { let mut l = Vec::new(); t.root.leaves(&mut l); l.contains(&id) });
            if let Some(t) = tab {
                self.current = t;
                self.tabs[t].active = id;
                self.add(launch, Place::Split { below: true }, (false, false, false));
            }
        }
        if full {
            self.set_zoom(ctx, Some(id));
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
            p.terms = r.take_terms().filter(|_| true).and_then(|mut t| t.keep_local().then_some(t));
            r.close();
        }
        p.state = State::Down(Down::new(true, why));
    }

    fn set_zoom(&mut self, _ctx: &egui::Context, zoom: Option<u64>) {
        self.zoom = zoom;
        if zoom.is_some() {
            self.window_full = false;
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
        let now = ctx.input(|i| i.time);
        let pointer_top = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y <= 6.0);
        if pointer_top {
            self.reveal_until = now + 1.2;
        }
        // The title bar, unless a full screen hides it until the pointer reaches the top edge.
        let mut remote_action: Option<(u64, remote::Deferred)> = None;
        let show_bar = self.zoom.is_none() && (!self.window_full || now < self.reveal_until);
        if show_bar {
            let bar = super::chrome::title_bar(ui, |ui| self.bar(ui, &mut remote_action));
            if bar.close {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            if self.window_full && ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y <= super::chrome::HEIGHT + 4.0) {
                self.reveal_until = now + 0.6;
            }
        }
        let area = ui.available_rect_before_wrap();
        self.shortcuts(&ctx, area);
        let full = self.zoom.is_some() || self.window_full;
        if full != self.sent_full {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(full));
            self.sent_full = full;
        }
        if self.tabs.is_empty() {
            return Out { empty: true, grab: false };
        }
        ui.painter().rect_filled(area, CornerRadius::ZERO, ui.visuals().panel_fill);
        let Some(tab) = self.tabs.get_mut(self.current) else { return Out::default() };
        let mut rects = Vec::new();
        match self.zoom {
            Some(z) => rects.push((z, area)),
            None => tab.root.layout(ui, area, 0, &mut rects),
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
        let mut grabbed = Some(grabbed);
        let mut closing = Vec::new();
        let mut lost = Vec::new();
        let mut zoom_req = None;
        let mut grab = false;
        let mut to_shell = Vec::new();
        for (id, rect) in &rects {
            let Some(i) = self.index(*id) else { continue };
            let is_active = *id == active;
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
                    r.zoomed = self.zoom == Some(*id);
                    let keys = if is_active { grabbed.take().unwrap_or_default() } else { Vec::new() };
                    match r.show(&mut child, keys) {
                        Err(e) if e == "disconnected" => closing.push(*id),
                        Err(e) => lost.push((i, e)),
                        Ok(()) => {
                            if let Some(on) = r.zoom_request.take() {
                                zoom_req = Some(on.then_some(*id));
                            }
                            if r.pane_closed {
                                closing.push(*id);
                            }
                            grab |= is_active && r.grab_wanted;
                        }
                    }
                }
                State::Down(d) => match down_view(&mut child, *rect, d, &pane.label) {
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
            if several && is_active && self.zoom.is_none() {
                ui.painter().rect_stroke(*rect, CornerRadius::ZERO, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
            }
        }
        if let Some((id, f)) = remote_action
            && let Some(i) = self.index(id)
            && let State::Live(r) = &mut self.panes[i].state
            && let Err(e) = f(r, &ctx)
        {
            if e == "disconnected" {
                closing.push(id);
            } else {
                lost.push((i, e));
            }
        }
        for (i, e) in lost {
            self.lost(i, e);
        }
        for id in to_shell {
            self.set_mode(id, PaneMode::Terminal);
        }
        if let Some(z) = zoom_req {
            self.set_zoom(&ctx, z);
        }
        if self.zoom.is_some() {
            self.dock(ui, area, now);
        }
        for id in closing {
            self.close_pane(id);
        }
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

    /// Windows Terminal's keys for tabs and panes. A pane sharing a screen keeps its keys for the
    /// remote host; a dialog or menu keeps them too.
    fn shortcuts(&mut self, ctx: &egui::Context, area: Rect) {
        if self.picker.is_some() || ctx.any_popup_open() {
            return;
        }
        let Some(active) = self.active_id() else { return };
        let screen = self.index(active).is_some_and(|i| matches!(&self.panes[i].state, State::Live(r) if r.mode() == PaneMode::Screen));
        if screen {
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
        if take(cs, Key::T) {
            self.picker = Some(Place::Tab);
        } else if take(cs, Key::W) {
            self.close_pane(active);
        } else if take(cs, Key::Tab) && !self.tabs.is_empty() {
            self.current = (self.current + self.tabs.len() - 1) % self.tabs.len();
            self.zoom = None;
        } else if take(Modifiers::CTRL, Key::Tab) && !self.tabs.is_empty() {
            self.current = (self.current + 1) % self.tabs.len();
            self.zoom = None;
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
                    // In a pane's full screen, the next pane takes the screen.
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
        // Widths: the five layout buttons and their separator, the mode drop-down and the pane's
        // controls (as measured last frame), the "more" menu.
        const LAYOUT_W: f32 = 5.0 * 32.0 + 16.0;
        const MORE_W: f32 = 36.0;
        let mode_w = self.mode_w + 8.0;
        // The controls fold before the tabs shrink below three at their narrowest.
        let room = avail - self.tabs.len().min(3) as f32 * TAB_MIN - PLUS_W;
        let fold_controls = room < mode_w + self.controls_w + LAYOUT_W;
        let fold_layout = room < mode_w + MORE_W + LAYOUT_W;
        // The right-hand group first, from the right edge, so the tabs take exactly what it leaves.
        let full = ui.max_rect();
        let mut right = ui.new_child(egui::UiBuilder::new().id_salt("bar-right").max_rect(full).layout(egui::Layout::right_to_left(egui::Align::Center)));
        if let Some(active) = self.active_id() {
            let ui = &mut right;
            ui.spacing_mut().item_spacing.x = 2.0;
            if !fold_layout {
                self.layout_buttons(ui, true);
                ui.separator();
            }
            if fold_controls {
                // One menu for what does not fit: the pane's controls, and the layout if needed.
                let more = icon_menu(ui, ic::DOTS, |ui| {
                    ui.set_max_width(300.0);
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
        let width = (right_left - ui.cursor().left() - PLUS_W - 12.0).max(TAB_MIN);
        let (close_tab, tab_rects) = self.tab_strip(ui, width);
        if ui.add(tool_button(ic::PLUS, false)).on_hover_text("Open: a terminal, a screen or files in a new tab (Ctrl+Shift+T)").clicked() {
            self.picker = Some(Place::Tab);
        }
        if let Some(t) = close_tab {
            self.close_tab(t);
            return;
        }
        self.drag_tab(ui, &tab_rects);
    }

    /// The tabs, as a terminal draws them: the current one in the panes' colour and joined to
    /// them, the others flat with a thin rule between. They share `width` down to `TAB_MIN`
    /// each; past that the strip scrolls (wheel, arrows, the current tab kept in view) and a
    /// list of every tab opens from the end. Returns a tab whose close was clicked, and where
    /// each tab is drawn (for dragging).
    fn tab_strip(&mut self, ui: &mut egui::Ui, width: f32) -> (Option<usize>, Vec<Rect>) {
        let n = self.tabs.len();
        let over = n as f32 * TAB_MIN > width + 0.5;
        let mut close = None;
        let mut view_w = width;
        if over {
            let start = ui.cursor().left();
            if ui.add(tool_button(ic::CARET_LEFT, false)).on_hover_text("Scroll the tabs").clicked() {
                self.tab_first = self.tab_first.saturating_sub(1);
            }
            // The arrow's width, with its spacing, for it, the other arrow and the list.
            let step = ui.cursor().left() - start;
            view_w = width - 3.0 * step - ui.spacing().item_spacing.x;
        }
        // Whole tabs only: as many as fit at their narrowest, sharing the room.
        let shown = if over { ((view_w / TAB_MIN).floor() as usize).max(1) } else { n };
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
        let mut rects = Vec::with_capacity(n);
        let mut hovered = None;
        let mut shapes = Vec::new();
        for t in 0..n {
            if t < first || t >= first + shown {
                rects.push(Rect::NOTHING);
                continue;
            }
            let x = view.left() + (t - first) as f32 * fit;
            let rect = Rect::from_min_max(Pos2::new(x, view.top() + 6.0), Pos2::new(x + fit, view.bottom()));
            rects.push(rect);
            let id = self.tabs[t].active;
            let order_len = self.order(t).len();
            let (name, rtt) = self.name(id);
            let (glyph, down) = self.glyph(id);
            let on = t == self.current;
            let r = ui.interact(rect, egui::Id::unique(("tab", t)), egui::Sense::click_and_drag());
            let x_rect = Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.center().y), Vec2::splat(20.0));
            let xr = ui.interact(x_rect, egui::Id::unique(("tab-close", t)), egui::Sense::click());
            let hot = r.hovered() || xr.hovered();
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
                close = Some(t);
            } else if r.clicked() {
                self.current = t;
                self.zoom = None;
            }
            if r.drag_started() {
                self.dragging = Some(t);
                self.current = t;
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
            let quiet = |k: usize| k != self.current && Some(k) != hovered;
            if quiet(t - 1) && quiet(t) {
                let x = rect.left();
                let y = rect.center().y - 1.0;
                painter.vline(x, (y - 8.0)..=(y + 8.0), Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));
            }
        }
        for (r, tip) in shapes {
            r.on_hover_text(tip);
        }
        if over {
            if ui.add(tool_button(ic::CARET_RIGHT, false)).on_hover_text("Scroll the tabs").clicked() {
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
                self.current = t;
                self.zoom = None;
            }
        }
        (close, rects)
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

    /// A tab being dragged: along the bar it changes places with the others; let go away from the
    /// bar, it leaves for a window of its own (unless it is the only one).
    fn drag_tab(&mut self, ui: &egui::Ui, rects: &[Rect]) {
        let Some(d) = self.dragging else { return };
        let (pos, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released() || !i.pointer.any_down()));
        let Some(p) = pos else { return };
        if released {
            self.dragging = None;
            let away = p.y > super::chrome::HEIGHT + 40.0 || p.y < -20.0;
            if away && self.tabs.len() > 1 && d < self.tabs.len() {
                let w = self.take_tab(d);
                self.torn_out.push(w);
            }
            return;
        }
        if let Some(to) = rects.iter().position(|r| p.x >= r.left() && p.x < r.right())
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
        let label = RichText::new(format!("{}  {}  {}", cur.glyph(), cur.name(), ic::CARET_DOWN)).size(13.0);
        let b = egui::Button::new(label).frame_when_inactive(false).min_size(Vec2::new(0.0, 30.0));
        let (r, _) = egui::containers::menu::MenuButton::from_button(b)
            .ui(ui, |ui| {
                crate::gui::menu_look(ui);
                ui.set_min_width(190.0);
                for m in PaneMode::ALL {
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
    }

    /// Full screen of the window, split beside or below, and moving the active pane.
    fn layout_buttons(&mut self, ui: &mut egui::Ui, rtl: bool) {
        let mut buttons: Vec<(&str, &str, u8)> = vec![
            (if self.window_full { ic::ARROWS_IN } else { ic::ARROWS_OUT }, "Full screen: the window as it is, split, without its title bar", 0),
            (ic::SPLIT_SIDE, "Split: a new pane beside this one (Alt+Shift+=)", 1),
            (ic::SPLIT_BELOW, "Split: a new pane below this one (Alt+Shift+-)", 2),
            (ic::CARET_LEFT, "Move this pane left (Alt+arrows go between panes)", 3),
            (ic::CARET_RIGHT, "Move this pane right", 4),
        ];
        if rtl {
            buttons.reverse();
        }
        for (glyph, tip, what) in buttons {
            let on = what == 0 && self.window_full;
            if !ui.add(tool_button(glyph, on)).on_hover_text(tip).clicked() {
                continue;
            }
            match what {
                0 => {
                    self.window_full = !self.window_full;
                    self.zoom = None;
                }
                1 | 2 => self.picker = Some(Place::Split { below: what == 2 }),
                _ => self.move_active(what == 3),
            }
        }
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

    /// Over a pane in full screen: hidden until the pointer reaches the top edge; its name is a
    /// drop-down to go to the tab's other panes.
    fn dock(&mut self, ui: &mut egui::Ui, area: Rect, now: f64) {
        let Some(z) = self.zoom else { return };
        if now >= self.reveal_until && !menu_open(ui.ctx()) {
            return;
        }
        let ctx = ui.ctx().clone();
        let order = self.order(self.current);
        let mut go = None;
        let mut leave = false;
        let mut action = None;
        let r = egui::Area::new(egui::Id::unique("pane-dock"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 4.0))
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let (name, rtt) = self.name(z);
                        let text = match rtt {
                            Some(ms) => format!("{name}  ·  {:.0} ms  {}", ms.min(999.0), ic::CARET_DOWN),
                            None => format!("{name}  {}", ic::CARET_DOWN),
                        };
                        let b = egui::Button::new(RichText::new(text).size(13.0)).frame_when_inactive(false);
                        egui::containers::menu::MenuButton::from_button(b).ui(ui, |ui| {
                            crate::gui::menu_look(ui);
                            for &id in &order {
                                let (n, _) = self.name(id);
                                if ui.add(egui::Button::selectable(id == z, n)).clicked() {
                                    go = Some(id);
                                    ui.close();
                                }
                            }
                        });
                        ui.separator();
                        self.mode_menu(ui, z);
                        self.controls(ui, z, &mut action);
                        ui.separator();
                        if ui.add(tool_button(ic::ARROWS_IN, false)).on_hover_text("Leave full screen (Ctrl+Alt+Enter)").clicked() {
                            leave = true;
                        }
                    });
                });
            });
        if r.response.hovered() {
            self.reveal_until = now + 0.6;
        }
        let _ = area;
        if let Some((id, f)) = action
            && let Some(i) = self.index(id)
            && let State::Live(r) = &mut self.panes[i].state
            && let Err(e) = f(r, &ctx)
        {
            self.lost(i, e);
        }
        if let Some(id) = go {
            if let Some(tab) = self.tabs.get_mut(self.current) {
                tab.active = id;
            }
            self.zoom = Some(id);
        }
        if leave {
            self.set_zoom(&ctx, None);
        }
    }
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
    use super::*;
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

    #[test]
    #[ignore]
    fn workspace_modes_splits_and_reconnect() {
        let mut h = Harness::new();
        h.w.add(test_desktop(), Place::Tab, (false, false, false));
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
        h.w.add(shell, Place::Split { below: false }, (false, false, false));
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
        h.w.set_zoom(&h.ctx.clone(), None);
        // The window's full screen keeps the split.
        h.bar(ic::ARROWS_OUT);
        assert!(h.w.window_full && h.w.zoom.is_none(), "window full screen");
        h.w.window_full = false;

        // A lost connection is made again by itself, in the mode it had.
        h.w.set_mode(order[0], PaneMode::Terminal);
        h.until("terminal again", |w| mode_of(w, 0) == Some(PaneMode::Terminal));
        if let State::Live(r) = &mut h.w.panes[0].state {
            r.close();
        }
        h.until("the pane notices", |w| matches!(w.panes[0].state, State::Down(_)));
        h.until("it connects again", |w| live(w, 0));
        assert_eq!(mode_of(&h.w, 0), Some(PaneMode::Terminal), "in the mode it had");

        // Closing the tab closes both panes; nothing is left.
        h.w.close_tab(0);
        h.frame(Vec::new());
        assert!(h.w.tabs.is_empty() && h.w.panes.is_empty());
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
            h.w.add(test_desktop(), Place::Tab, (false, false, false));
            h.until("connected", |w| live(w, 0));
            let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            h.w.add(
                Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
                Place::Split { below: false },
                (false, false, false),
            );
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                h.frame(Vec::new());
            }
            save(&mut h, "split");
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
            let ctx = h.ctx.clone();
            h.w.tabs[0].active = first;
            h.w.set_zoom(&ctx, Some(first));
            h.frame(vec![Event::PointerMoved(Pos2::new(640.0, 2.0))]);
            save(&mut h, "zoom_dock");
            h.w.set_zoom(&ctx, None);
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
                (false, false, false),
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
        assert_eq!(h.w.tabs.len(), 29, "middle click closes");
        assert!(in_bar(&h, "host04").is_empty());
        h.w.close_all();
    }

    #[test]
    fn tab_pictures() {
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
        for n in [2usize, 8, 16, 30] {
            let mut h = Harness::new();
            for i in 0..n {
                let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
                profile.name = format!("server-{i}.example.com");
                h.w.add(
                    Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
                    Place::Tab,
                    (false, false, false),
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
        h.w.add(shell("first"), Place::Tab, (false, false, false));
        h.w.add(shell("second"), Place::Tab, (false, false, false));
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
        let a = at(&h, "first");
        drag(&mut h, a, a + Vec2::new(0.0, 300.0));
        assert_eq!(names(&h.w), ["second"], "dragged away from the bar");
        assert_eq!(h.w.torn_out.len(), 1, "it leaves for a window of its own");
        assert_eq!(names(&h.w.torn_out[0]), ["first"]);
        assert_eq!(h.w.torn_out[0].panes.len(), 1, "with its pane");
        // The last tab stays: there is nothing to leave from.
        let a = at(&h, "second");
        drag(&mut h, a, a + Vec2::new(0.0, 300.0));
        assert_eq!(names(&h.w), ["second"]);
        h.w.close_all();
        for mut w in std::mem::take(&mut h.w.torn_out) {
            w.close_all();
        }
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
        h.w.add(shell("a"), Place::Tab, (false, false, false));
        h.w.add(shell("b"), Place::Tab, (false, false, false));
        h.w.add(shell("c"), Place::Split { below: false }, (false, false, false));
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
        assert_eq!(h.w.order(1), vec![order[1]], "Ctrl+Shift+W closes the pane");
        h.w.close_all();
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
            h.w.add(shell(n), Place::Tab, (false, false, false));
        }
        h.w.current = 1;
        h.frame(Vec::new());
        h.w.close_tab(0);
        assert_eq!(h.w.name(h.w.tabs[h.w.current].active).0, "b", "the current tab stays");
        // A pane in full screen, then closed: the window leaves full screen.
        h.w.add(shell("d"), Place::Split { below: false }, (false, false, false));
        let d = h.w.tabs[h.w.current].active;
        let ctx = h.ctx.clone();
        h.w.set_zoom(&ctx, Some(d));
        h.frame(Vec::new());
        assert!(h.w.sent_full);
        h.w.close_pane(d);
        h.frame(Vec::new());
        assert!(!h.w.sent_full, "full screen ends with its pane");
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
        h.w.add(Launch::Shell { label: "s".into(), profile, target, ssh_cmd: None }, Place::Tab, (false, false, false));
        h.frame(Vec::new());
        let id = h.w.tabs[0].active;
        h.w.set_mode(id, PaneMode::Screen);
        assert!(matches!(h.w.panes[0].state, State::Down(_)));
        h.w.set_mode(id, PaneMode::Terminal);
        assert!(matches!(h.w.panes[0].state, State::Shell(_)), "back to the shell it was");
        h.w.close_all();
    }

    /// In the window's full screen the title bar hides, shows when the pointer reaches the top
    /// edge, and hides again once it leaves.
    #[test]
    fn full_window_shows_its_bar_at_the_top_edge() {
        let mut h = Harness::new();
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(
            Launch::Shell { label: "shell".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
            Place::Tab,
            (false, false, false),
        );
        h.frame(Vec::new());
        h.bar(ic::ARROWS_OUT);
        assert!(h.w.window_full);
        let bar_shown = |h: &Harness| h.snap.find(ic::PLUS).iter().any(|r| r.center().y < super::super::chrome::HEIGHT);
        for _ in 0..40 {
            h.frame(vec![Event::PointerMoved(Pos2::new(600.0, 400.0))]);
        }
        assert!(!bar_shown(&h), "hidden in full screen");
        h.frame(vec![Event::PointerMoved(Pos2::new(600.0, 1.0))]);
        h.frame(Vec::new());
        assert!(bar_shown(&h), "shown at the top edge");
        for _ in 0..40 {
            h.frame(vec![Event::PointerMoved(Pos2::new(600.0, 400.0))]);
        }
        assert!(!bar_shown(&h), "hidden again");
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
        h.w.add(Launch::Agent { target, label: "nowhere".into(), ssh_cmd: None, mode: PaneMode::Screen }, Place::Tab, (false, false, false));
        h.until("it gives up", |w| matches!(&w.panes[0].state, State::Down(d) if d.given_up));
        h.frame(Vec::new());
        assert!(!h.snap.find("Reconnect").is_empty(), "Reconnect is offered");
        assert!(!h.snap.find("Close").is_empty(), "Close is offered");
        assert!(h.snap.find("Cancel").is_empty(), "no Cancel once given up");
        h.item("Close");
        assert!(h.w.panes.is_empty(), "Close closes the pane");
    }
}
