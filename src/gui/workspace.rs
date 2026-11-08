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
    /// The whole window, split as it is, fills the screen.
    window_full: bool,
    /// Until when the title bar stays shown over a full-screen window, and the zoomed pane's dock.
    reveal_until: f64,
    /// Hosts reached, for the recent list.
    pub(super) reached: Vec<String>,
    /// The width the active pane's controls took last frame, to know when they must fold.
    controls_w: f32,
    /// The mode drop-down's width last frame, and all the right-hand group's.
    mode_w: f32,
    right_w: f32,
    title: String,
}

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
            window_full: false,
            reveal_until: 0.0,
            reached: Vec::new(),
            controls_w: 260.0,
            mode_w: 150.0,
            right_w: 420.0,
            title: String::new(),
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
            self.current = self.current.min(self.tabs.len().saturating_sub(1));
        }
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
        r.open_panels(panel, chat);
        self.reached.push(p.label.clone());
        p.state = State::Live(Box::new(r));
        let id = p.id;
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
            p.terms = r.take_terms();
            r.close();
        }
        p.state = State::Down(Down::new(true, why));
    }

    fn set_zoom(&mut self, ctx: &egui::Context, zoom: Option<u64>) {
        self.zoom = zoom;
        if zoom.is_some() {
            self.window_full = false;
        }
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(zoom.is_some() || self.window_full));
    }

    fn set_mode(&mut self, id: u64, mode: PaneMode) {
        let Some(i) = self.index(id) else { return };
        let p = &mut self.panes[i];
        p.mode = mode;
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
            return Out { empty: true };
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
                        }
                    }
                }
                State::Down(d) => match down_view(&mut child, *rect, d, &pane.label) {
                    Some(DownAct::Close) => closing.push(*id),
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
        Out { empty: self.tabs.is_empty() }
    }

    /// The title bar's content: tabs and + on the left; on the right the active pane's mode and
    /// controls, then the layout buttons. What does not fit folds into one menu.
    fn bar(&mut self, ui: &mut egui::Ui, action: &mut Option<(u64, remote::Deferred)>) {
        ui.spacing_mut().item_spacing.x = 2.0;
        let avail = ui.available_width();
        // Widths measured last frame: the five layout buttons and their separator, the mode
        // drop-down, the pane's controls, the "more" menu; tabs keep at least 90 each and +.
        const LAYOUT_W: f32 = 5.0 * 32.0 + 16.0;
        const MORE_W: f32 = 36.0;
        let mode_w = self.mode_w + 8.0;
        let room = avail - self.tabs.len() as f32 * 90.0 - 36.0;
        let fold_controls = room < mode_w + self.controls_w + LAYOUT_W;
        let fold_layout = room < mode_w + MORE_W + LAYOUT_W;
        // The tabs take what the right-hand group leaves, as it measured last frame.
        let tab_w = ((avail - self.right_w - 36.0 - 16.0) / self.tabs.len().max(1) as f32).clamp(72.0, 220.0);
        let mut close_tab = None;
        for t in 0..self.tabs.len() {
            let order = self.order(t);
            let (name, rtt) = self.name(self.tabs[t].active);
            let mut text = name;
            if let Some(ms) = rtt {
                text.push_str(&format!("  ·  {:.0} ms", ms.min(999.0)));
            }
            if order.len() > 1 {
                text.push_str(&format!("  {} {}", ic::SQUARES_FOUR, order.len()));
            }
            let on = t == self.current;
            let (rect, r) = ui.allocate_exact_size(Vec2::new(tab_w, 30.0), egui::Sense::click());
            let p = ui.painter();
            if on {
                p.rect_filled(rect, theme::R_SM, selected_fill());
                p.rect_stroke(rect, theme::R_SM, Stroke::new(1.0, selected_stroke()), egui::StrokeKind::Inside);
            } else if r.hovered() {
                p.rect_filled(rect, theme::R_SM, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let fg = if on { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
            let text_rect = rect.shrink2(Vec2::new(10.0, 0.0)).with_max_x(rect.right() - 26.0);
            let galley = ui.fonts_mut(|f| {
                let mut job = egui::text::LayoutJob::simple_singleline(text.clone(), FontId::proportional(13.0), fg);
                job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width());
                f.layout_job(job)
            });
            ui.painter().galley(Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0), galley, fg);
            let x = Rect::from_center_size(Pos2::new(rect.right() - 14.0, rect.center().y), Vec2::splat(20.0));
            let xr = ui.interact(x, egui::Id::unique(("tab-close", t, order.first().copied())), egui::Sense::click());
            if xr.hovered() {
                ui.painter().rect_filled(x, theme::R_SM, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            if on || r.hovered() || xr.hovered() {
                ui.painter().text(x.center(), Align2::CENTER_CENTER, ic::X, FontId::proportional(12.0), fg);
            }
            if xr.clicked() {
                close_tab = Some(t);
            } else if r.clicked() {
                self.current = t;
                self.zoom = None;
            }
            r.on_hover_text(text);
        }
        if ui.add(tool_button(ic::PLUS, false)).on_hover_text("Open: a terminal, a screen or files (new tab)").clicked() {
            self.picker = Some(Place::Tab);
        }
        if let Some(t) = close_tab {
            self.close_tab(t);
            return;
        }
        let Some(active) = self.active_id() else { return };
        let right = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
            ui.min_rect().width()
        });
        self.right_w = right.inner;
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
        egui::containers::menu::MenuButton::from_button(b)
            .ui(ui, |ui| {
                crate::gui::menu_look(ui);
                ui.set_min_width(190.0);
                for m in PaneMode::ALL {
                    if ui.add(egui::Button::selectable(cur == m, format!("{}  {}", m.glyph(), m.name()))).clicked() {
                        chosen = Some(m);
                        ui.close();
                    }
                }
            })
            .0
            .on_hover_text("What this pane shows of its connection; switching does not connect again");
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
        if let State::Live(r) = &mut self.panes[i].state {
            let mut a = None;
            r.toolbar_items(ui, &mut a);
            if let Some(f) = a {
                *action = Some((id, f));
            }
        }
    }

    /// Full screen of the window, split beside or below, and moving the active pane.
    fn layout_buttons(&mut self, ui: &mut egui::Ui, rtl: bool) {
        let ctx = ui.ctx().clone();
        let mut buttons: Vec<(&str, &str, u8)> = vec![
            (if self.window_full { ic::ARROWS_IN } else { ic::ARROWS_OUT }, "Full screen: the window as it is, split, without its title bar", 0),
            (ic::SPLIT_SIDE, "Split: a new pane beside this one", 1),
            (ic::SPLIT_BELOW, "Split: a new pane below this one", 2),
            (ic::CARET_LEFT, "Move this pane left", 3),
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
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.window_full));
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
