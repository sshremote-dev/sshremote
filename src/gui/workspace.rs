//! The workspace: tabs of panes split as in Windows Terminal. Each pane shows one connection as a
//! terminal, the remote screen or its files, switched without connecting again; a pane whose
//! connection is lost tries again by itself before asking.

use super::*;

mod bar;
mod layout;

use layout::Node;

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
            // A computer by its ID takes its password through the environment.
            let password = match target {
                Target::Ssh { host, password: Some(p), .. } if host.starts_with("sshremote://") => vec![(crate::PASSWORD_VAR, p.clone())],
                _ => Vec::new(),
            };
            if let Err(e) = crate::ext::spawn_provided("remote", &remote_args(target, *mode), &password) {
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
        // Under another dialog it waits: drawn there, it would take the keys typed into that one.
        if self.picker.is_some() || self.palette.is_some() || self.confirm.is_some() {
            return;
        }
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
    // Without the remote part a pane is never live, and what follows `prefs` cannot run.
    #[cfg_attr(not(feature = "remote"), allow(unreachable_code))]
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
            self.full.zoomed = None;
            // In full screen the tab's screen fills it alone, as the first one did.
            self.full_zoom();
        }
    }

    /// In full screen, a screen sharing a split tab is zoomed so it fills the monitor; that zoom
    /// is undone on the way out.
    fn full_zoom(&mut self) {
        if !self.full.on {
            return;
        }
        let active = self.active_id();
        let screen = active.and_then(|id| self.index(id)).is_some_and(|i| self.mode_of_pane(i) == PaneMode::Screen);
        if screen && self.zoom.is_none() && self.order(self.current).len() > 1 {
            self.zoom = active;
            self.full.zoomed = active;
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
            self.full_zoom();
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
    /// remote host but for Ctrl+Alt+PgUp/PgDn (tabs) and Ctrl+Alt+Shift+PgUp/PgDn (panes), keys
    /// hosts leave alone; a dialog or menu keeps them all.
    fn shortcuts(&mut self, ctx: &egui::Context, area: Rect) {
        if self.picker.is_some() || self.confirm.is_some() || ctx.any_popup_open() {
            return;
        }
        let Some(active) = self.active_id() else { return };
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
        let ca = Modifiers::CTRL | Modifiers::ALT;
        let tabs = self.tabs.len().max(1);
        if take(ca, Key::PageUp) {
            self.select_tab((self.current + tabs - 1) % tabs);
            return;
        } else if take(ca, Key::PageDown) {
            self.select_tab((self.current + 1) % tabs);
            return;
        } else if take(ca | Modifiers::SHIFT, Key::PageUp) {
            self.step_pane(active, false);
            return;
        } else if take(ca | Modifiers::SHIFT, Key::PageDown) {
            self.step_pane(active, true);
            return;
        }
        if self.screen_has_keys() {
            return;
        }
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
                self.step_pane(active, on);
            }
        }
    }

    /// The next pane of the tab (or the one before) becomes the active one.
    fn step_pane(&mut self, active: u64, on: bool) {
        let order = self.order(self.current);
        if order.len() < 2 {
            return;
        }
        if let Some(at) = order.iter().position(|&id| id == active) {
            let next = if on { (at + 1) % order.len() } else { (at + order.len() - 1) % order.len() };
            self.tabs[self.current].active = order[next];
            // In a zoomed tab, the next pane takes its place; a zoom full screen made stays its.
            if self.zoom.is_some() {
                if self.full.zoomed == self.zoom {
                    self.full.zoomed = Some(order[next]);
                }
                self.zoom = Some(order[next]);
            }
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
pub(super) fn ask(ctx: &egui::Context, title: &str, body: &str, yes: &str) -> Option<bool> {
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
mod tests;
