//! In an X11 session: the program's own window held over its pane, as on Windows. It is kept above
//! this window as a window of its own (WM_TRANSIENT_FOR), moved and sized with the pane by asking
//! the window manager (_NET_MOVERESIZE_WINDOW), minimized while its tab is not shown, and lowered
//! under this window while one of this window's menus or dialogs is over it.

use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, Rect};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{self, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, PropMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_WM_PID,
        _NET_FRAME_EXTENTS,
        _GTK_FRAME_EXTENTS,
        _NET_WM_WINDOW_OPACITY,
        _NET_MOVERESIZE_WINDOW,
        _NET_RESTACK_WINDOW,
        _NET_CLOSE_WINDOW,
        _NET_ACTIVE_WINDOW,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        WM_STATE,
        WM_CHANGE_STATE,
    }
}

struct X {
    conn: RustConnection,
    root: u32,
    a: Atoms,
    /// The selection a compositing manager owns on this screen.
    cm: u32,
}

impl X {
    fn open() -> Option<X> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let a = Atoms::new(&conn).ok()?.reply().ok()?;
        let cm = conn.intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes()).ok()?.reply().ok()?.atom;
        Some(X { conn, root, a, cm })
    }

    fn cards(&self, w: u32, prop: u32, kind: impl Into<u32>) -> Vec<u32> {
        self.conn.get_property(false, w, prop, kind, 0, 1024).ok().and_then(|c| c.reply().ok()).and_then(|p| p.value32().map(|v| v.collect())).unwrap_or_default()
    }

    fn pid(&self, w: u32) -> u32 {
        self.cards(w, self.a._NET_WM_PID, AtomEnum::CARDINAL).first().copied().unwrap_or(0)
    }

    fn clients(&self) -> Vec<u32> {
        self.cards(self.root, self.a._NET_CLIENT_LIST, AtomEnum::WINDOW)
    }

    fn class(&self, w: u32) -> String {
        let p = self.conn.get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256).ok().and_then(|c| c.reply().ok());
        p.map(|p| String::from_utf8_lossy(&p.value).replace('\0', " ").trim().to_lowercase()).unwrap_or_default()
    }

    fn alive(&self, w: u32) -> bool {
        self.conn.get_window_attributes(w).ok().and_then(|c| c.reply().ok()).is_some()
    }

    fn viewable(&self, w: u32) -> bool {
        self.conn.get_window_attributes(w).ok().and_then(|c| c.reply().ok()).is_some_and(|a| a.map_state == xproto::MapState::VIEWABLE)
    }

    /// A compositing manager draws the screen. Without one, a window wholly under others is told
    /// so, and this one's frames stop then.
    fn composited(&self) -> bool {
        self.conn.get_selection_owner(self.cm).ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.owner != 0)
    }

    /// The windows it has open over it (WM_TRANSIENT_FOR): a question its program asks, say.
    fn over(&self, w: u32) -> Vec<u32> {
        self.clients().into_iter().filter(|c| self.cards(*c, AtomEnum::WM_TRANSIENT_FOR.into(), AtomEnum::WINDOW).first() == Some(&w)).collect()
    }

    /// Made the active window, then the newest window it has open over it, a question its program
    /// asks (it has the keys, and stays over it).
    fn activate(&self, w: u32, owner: u32) {
        self.message(w, self.a._NET_ACTIVE_WINDOW, [2, 0, owner, 0, 0]);
        if let Some(q) = self.over(w).pop() {
            self.message(q, self.a._NET_ACTIVE_WINDOW, [2, 0, w, 0, 0]);
        }
    }

    fn iconic(&self, w: u32) -> bool {
        self.cards(w, self.a.WM_STATE, self.a.WM_STATE).first() == Some(&3)
    }

    /// The window itself on the screen, and the part of it that shows (its frame's title bar
    /// added, a drawn shadow taken off): (x, y, width, height) each.
    fn rects(&self, w: u32) -> Option<([i32; 4], [i32; 4])> {
        let g = self.conn.get_geometry(w).ok()?.reply().ok()?;
        let t = self.conn.translate_coordinates(w, self.root, 0, 0).ok()?.reply().ok()?;
        let me = [t.dst_x as i32, t.dst_y as i32, g.width as i32, g.height as i32];
        let four = |p| {
            let v = self.cards(w, p, AtomEnum::CARDINAL);
            if v.len() == 4 { [v[0] as i32, v[1] as i32, v[2] as i32, v[3] as i32] } else { [0; 4] }
        };
        let (f, s) = (four(self.a._NET_FRAME_EXTENTS), four(self.a._GTK_FRAME_EXTENTS));
        let seen = [me[0] - f[0] + s[0], me[1] - f[2] + s[2], me[2] + f[0] + f[1] - s[0] - s[1], me[3] + f[2] + f[3] - s[2] - s[3]];
        Some((me, seen))
    }

    fn message(&self, w: u32, kind: u32, data: [u32; 5]) {
        let ev = ClientMessageEvent::new(32, w, kind, data);
        let _ = self.conn.send_event(false, self.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, ev);
    }

    fn hush(&self, w: u32) {
        let _ = self.conn.change_property32(PropMode::REPLACE, w, self.a._NET_WM_WINDOW_OPACITY, AtomEnum::CARDINAL, &[0]);
    }

    fn unhush(&self, w: u32) {
        let _ = self.conn.delete_property(w, self.a._NET_WM_WINDOW_OPACITY);
    }

    fn own(&self, w: u32, owner: u32) {
        if owner == 0 {
            let _ = self.conn.delete_property(w, AtomEnum::WM_TRANSIENT_FOR.into());
        } else {
            let _ = self.conn.change_property32(PropMode::REPLACE, w, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, &[owner]);
        }
    }

    /// Its visible part to `to`: the window manager moves and sizes it (unmaximized first).
    fn aim(&self, w: u32, to: [i32; 4]) {
        let Some((me, seen)) = self.rects(w) else { return };
        let state = self.cards(w, self.a._NET_WM_STATE, AtomEnum::ATOM);
        let a = &self.a;
        if state.iter().any(|s| [a._NET_WM_STATE_MAXIMIZED_VERT, a._NET_WM_STATE_MAXIMIZED_HORZ, a._NET_WM_STATE_FULLSCREEN].contains(s)) {
            self.message(w, a._NET_WM_STATE, [0, a._NET_WM_STATE_MAXIMIZED_VERT, a._NET_WM_STATE_MAXIMIZED_HORZ, 2, 0]);
            self.message(w, a._NET_WM_STATE, [0, a._NET_WM_STATE_FULLSCREEN, 0, 2, 0]);
        }
        let want = [to[0] + me[0] - seen[0], to[1] + me[1] - seen[1], (to[2] + me[2] - seen[2]).max(1), (to[3] + me[3] - seen[3]).max(1)];
        // Static gravity: where the window itself goes; from a pager, as a window manager allows.
        let flags = 10 | 0xf00 | (2 << 12);
        self.message(w, a._NET_MOVERESIZE_WINDOW, [flags, want[0] as u32, want[1] as u32, want[2] as u32, want[3] as u32]);
    }
}

/// An X11 session, this window one of its windows: programs here are held in their panes.
pub(in crate::gui) fn here() -> bool {
    static HERE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HERE.get_or_init(|| std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_some() && gui().is_some())
}

/// This window's thread's connection, made once.
fn gui() -> Option<std::sync::MutexGuard<'static, Option<X>>> {
    static GUI: Mutex<Option<X>> = Mutex::new(None);
    static TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let mut g = GUI.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() && !TRIED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        *g = X::open();
    }
    g.is_some().then_some(g)
}

fn with<R>(f: impl FnOnce(&X) -> R) -> Option<R> {
    let g = gui()?;
    let x = g.as_ref()?;
    let r = f(x);
    let _ = x.conn.flush();
    Some(r)
}

/// The windows held now, for `bring_back`.
static HELD: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn held(add: Option<u32>, remove: Option<u32>) {
    let mut list = HELD.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|h| Some(*h) != remove);
    list.extend(add);
}

/// A crash: the windows held are their own again, seen and shown.
pub(crate) fn bring_back() {
    let list = HELD.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if list.is_empty() {
        return;
    }
    if let Some(x) = X::open() {
        for w in list {
            x.own(w, 0);
            x.unhush(w);
            let _ = x.conn.map_window(w);
        }
        let _ = x.conn.flush();
    }
}

/// New windows are caught as they are made, by a thread that hears of every window made on the
/// screen: made see-through at once (the compositor shows nothing of them), and once the window
/// manager has them, put on their pane; shown when there. Looked for every so often instead, a
/// window would show where it opens and then jump into the pane.
mod catch {
    use super::*;

    #[derive(Clone, Copy)]
    pub(super) struct Caught {
        pub win: u32,
    }

    struct Wait {
        key: u64,
        before: Vec<u32>,
        to: Option<[i32; 4]>,
        owner: u32,
        /// Windows made since it started, see-through until one is caught (the rest are let be).
        fresh: Vec<u32>,
        caught: Option<Caught>,
        since: Instant,
    }

    struct Watch {
        waits: Vec<Wait>,
        running: bool,
        next: u64,
    }

    static WATCH: Mutex<Watch> = Mutex::new(Watch { waits: Vec::new(), running: false, next: 1 });

    fn watch() -> std::sync::MutexGuard<'static, Watch> {
        WATCH.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn wait(before: Vec<u32>) -> u64 {
        let mut w = watch();
        let key = w.next;
        w.next += 1;
        w.waits.push(Wait { key, before, to: None, owner: 0, fresh: Vec::new(), caught: None, since: Instant::now() });
        if !w.running {
            w.running = true;
            drop(w);
            let (tx, rx) = std::sync::mpsc::channel();
            if std::thread::Builder::new().name("held-catch".into()).spawn(move || run(tx)).is_err() {
                watch().running = false;
            } else {
                let _ = rx.recv_timeout(Duration::from_millis(500));
            }
        }
        key
    }

    pub(super) fn aim(key: u64, to: [i32; 4], owner: u32) {
        if let Some(x) = watch().waits.iter_mut().find(|x| x.key == key) {
            x.to = Some(to);
            x.owner = owner;
        }
    }

    /// The window caught; the wait ends with it, its other new windows seen again.
    pub(super) fn take(key: u64) -> Option<Caught> {
        let mut w = watch();
        let i = w.waits.iter().position(|x| x.key == key && x.caught.is_some())?;
        let wait = w.waits.remove(i);
        drop(w);
        release(&wait.fresh, wait.caught.map(|c| c.win));
        wait.caught
    }

    pub(super) fn forget(key: u64) -> Option<Caught> {
        let mut w = watch();
        let i = w.waits.iter().position(|x| x.key == key)?;
        let wait = w.waits.remove(i);
        drop(w);
        release(&wait.fresh, wait.caught.map(|c| c.win));
        wait.caught
    }

    fn release(fresh: &[u32], keep: Option<u32>) {
        let _ = with(|x| {
            for w in fresh.iter().filter(|w| Some(**w) != keep) {
                x.unhush(*w);
            }
        });
    }

    fn run(ready: std::sync::mpsc::Sender<()>) {
        let Some(x) = X::open() else {
            watch().running = false;
            let _ = ready.send(());
            return;
        };
        let _ = x.conn.change_window_attributes(x.root, &xproto::ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY));
        let _ = x.conn.flush();
        let _ = ready.send(());
        let fd = x.conn.stream().as_raw_fd();
        let mine = std::process::id();
        loop {
            {
                let mut w = watch();
                w.waits.retain(|x| x.since.elapsed() < Duration::from_secs(60));
                if w.waits.is_empty() {
                    w.running = false;
                    return;
                }
            }
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: one pollfd, for the connection's socket, which lives as long as `x`.
            unsafe { libc::poll(&mut pfd, 1, 250) };
            while let Ok(Some(ev)) = x.conn.poll_for_event() {
                match ev {
                    Event::CreateNotify(e) if e.parent == x.root && !e.override_redirect => {
                        let mut w = watch();
                        if let Some(wait) = w.waits.iter_mut().find(|w| w.caught.is_none()) {
                            wait.fresh.push(e.window);
                            x.hush(e.window);
                        }
                    }
                    // Taken by the window manager (framed, or mapped where it frames nothing).
                    Event::ReparentNotify(e) if e.event == x.root => claim(&x, e.window, mine),
                    Event::MapNotify(e) if e.event == x.root && !e.override_redirect => claim(&x, e.window, mine),
                    Event::DestroyNotify(e) => {
                        for wait in watch().waits.iter_mut() {
                            wait.fresh.retain(|f| *f != e.window);
                            if wait.caught.is_some_and(|c| c.win == e.window) {
                                wait.caught = None;
                            }
                        }
                    }
                    _ => {}
                }
            }
            let _ = x.conn.flush();
        }
    }

    /// A program's window of its own (not a dialog of another, not this program's), as the window
    /// manager takes it: kept above this window, put on its pane.
    fn claim(x: &X, win: u32, mine: u32) {
        let (key, to, owner) = {
            let mut w = watch();
            if w.waits.iter().any(|x| x.caught.is_some_and(|c| c.win == win)) {
                return;
            }
            let Some(wait) = w.waits.iter_mut().find(|w| w.caught.is_none() && w.fresh.contains(&win) && !w.before.contains(&win)) else { return };
            (wait.key, wait.to, wait.owner)
        };
        let types = x.cards(win, x.a._NET_WM_WINDOW_TYPE, AtomEnum::ATOM);
        let transient = !x.cards(win, AtomEnum::WM_TRANSIENT_FOR.into(), AtomEnum::WINDOW).is_empty();
        // A program's window names it (WM_CLASS) before it shows; one that does not is a part of the
        // desktop made meanwhile.
        if x.pid(win) == mine || transient || !(types.is_empty() || types.contains(&x.a._NET_WM_WINDOW_TYPE_NORMAL)) || x.class(win).is_empty() {
            return;
        }
        if owner != 0 {
            x.own(win, owner);
        }
        if let Some(to) = to {
            x.aim(win, to);
        }
        let _ = x.conn.flush();
        if let Some(wait) = watch().waits.iter_mut().find(|w| w.key == key) {
            wait.caught = Some(Caught { win });
        }
    }
}

const LOOK: Duration = Duration::from_millis(300);
const GIVE_UP: Duration = Duration::from_secs(20);
/// How soon a caught window that goes is taken for another's.
const MISTAKEN: Duration = Duration::from_secs(5);

/// Whether process `pid` runs.
fn running(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

pub(in crate::gui) struct Held {
    key: Option<u64>,
    what: String,
    name: String,
    before: Vec<u32>,
    since: Instant,
    looked: Instant,
    win: Option<Win>,
    said: Option<String>,
    ended: bool,
    /// This program's window, once found.
    owner: u32,
    /// Asked to close with its pane: the windows it had open over it then.
    asked: Option<Vec<u32>>,
    /// When it was last seen to be there while its pane is not shown.
    checked: Instant,
    /// This window was full screen last time, and the pane all of it (the whole monitor).
    full: bool,
    whole: bool,
    /// Since when it is out of sight until this window is out of full screen (`leaving_full`).
    leaving: Option<Instant>,
}

struct Win {
    id: u32,
    /// The process that made it (0: not said).
    pid: u32,
    new: bool,
    /// Where its visible part was, for one already open that is let go.
    was: Option<[i32; 4]>,
    /// Kept above this window (WM_TRANSIENT_FOR), else under it while covered.
    owned: bool,
    iconic: bool,
    /// See-through until it is on the pane; since when it is there.
    hushed: bool,
    placed: Option<Instant>,
    /// Since when it is asked to move and stays put: a window manager that places windows itself.
    stuck: Option<Instant>,
    since: Instant,
    /// Since when it is asked under this window, to see it went.
    lowered: Option<Instant>,
    /// The pane it was last asked onto.
    aimed: Option<[i32; 4]>,
    /// Since when it is asked out of being minimized.
    restored: Option<Instant>,
    /// Since when it is asked to be full screen, and whether the window manager would not; and
    /// let go of meanwhile, for one that makes no window kept above another full screen.
    full_asked: Option<Instant>,
    refused: bool,
    loose: bool,
    /// Since when it is to be over this window after a change made here (put over it again,
    /// full screen or not), and since when it is seen under it all the same.
    lift: Option<Instant>,
    sunk: Option<Instant>,
    /// Full screen, and the pane the whole monitor, last time.
    last: (bool, bool),
    /// Since when it is back from minimized or from under this window: a question its program
    /// asked meanwhile comes back with it (a window manager may keep it minimized, or under it).
    back: Option<Instant>,
}

impl Held {
    pub(in crate::gui) fn start(kind: u8, text: &str) -> Held {
        let before = with(|x| x.clients()).unwrap_or_default();
        let key = catch::wait(before.clone());
        let (name, said) = match crate::apps::launch_minimized(kind, text) {
            Ok(n) => (n, None),
            Err(e) => (String::new(), Some(format!("It could not be started: {e}."))),
        };
        if said.is_some() {
            catch::forget(key);
        }
        let what = if name.is_empty() { text.trim().trim_end_matches('/').rsplit('/').next().unwrap_or(text).to_string() } else { name.clone() };
        let now = Instant::now();
        Held { key: said.is_none().then_some(key), what, name, before, since: now, looked: now - LOOK, win: None, said, ended: false, owner: 0, asked: None, checked: now, full: false, whole: false, leaving: None }
    }

    pub(in crate::gui) fn ended(&self) -> bool {
        self.ended
    }

    /// Its window gone just after it was caught, unasked, while the process that made it runs on:
    /// a window something else made meanwhile, so its program's own is looked for again.
    fn mistaken(&self) -> bool {
        self.asked.is_none() && self.win.as_ref().is_some_and(|w| w.new && w.since.elapsed() < MISTAKEN && w.pid != 0 && running(w.pid))
    }

    fn hold(&mut self, id: u32, new: bool, hushed: bool) {
        let was = if new { None } else { with(|x| x.rects(id).map(|r| r.1)).flatten() };
        let pid = with(|x| x.pid(id)).unwrap_or(0);
        self.win = Some(Win { id, pid, new, was, owned: false, iconic: false, hushed, placed: None, stuck: None, since: Instant::now(), lowered: None, aimed: None, restored: None, full_asked: None, refused: false, loose: false, lift: None, sunk: None, last: (false, false), back: None });
        held(Some(id), None);
    }

    /// Its window when the hook missed it: the first new one not this program's, or (after a few
    /// seconds) one of the program started, already open.
    fn find(&self) -> Option<(u32, bool)> {
        if self.since.elapsed() < Duration::from_secs(2) && self.key.is_some() {
            return None;
        }
        with(|x| {
            let mine = std::process::id();
            let list = x.clients();
            let theirs = |w: u32| x.pid(w) != mine && x.viewable(w) && x.cards(w, AtomEnum::WM_TRANSIENT_FOR.into(), AtomEnum::WINDOW).is_empty() && !x.class(w).is_empty();
            if let Some(w) = list.iter().find(|w| !self.before.contains(w) && theirs(**w)) {
                return Some((*w, true));
            }
            let name = self.name.to_lowercase();
            let first = name.split_whitespace().next().unwrap_or("").to_string();
            if self.since.elapsed() < Duration::from_secs(3) || first.is_empty() {
                return None;
            }
            list.iter().rev().find(|w| theirs(**w) && x.class(**w).contains(&first)).map(|w| (*w, false))
        })
        .flatten()
    }

    pub(in crate::gui) fn show(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let place = self.place(&ctx, rect);
        if let Some(key) = self.key {
            if let Some((to, owner)) = place {
                catch::aim(key, to, owner);
            }
            if let Some(c) = catch::take(key) {
                self.key = None;
                self.hold(c.win, true, true);
            }
        }
        if self.win.is_none() && self.said.is_none() && self.looked.elapsed() >= LOOK {
            self.looked = Instant::now();
            match self.find() {
                Some((w, new)) => {
                    if let Some(k) = self.key.take() {
                        catch::forget(k);
                    }
                    self.hold(w, new, false);
                }
                None if self.since.elapsed() > GIVE_UP => {
                    if let Some(k) = self.key.take() {
                        catch::forget(k);
                    }
                    self.said = Some(format!("{} opened no window of its own to show here.", self.what));
                }
                None => {}
            }
        }
        let text = match (&self.win, &self.said) {
            (Some(_), _) => format!("{} is here", self.what),
            (None, Some(s)) => s.clone(),
            (None, None) => format!("Starting {}\u{2026}", self.what),
        };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), ui.visuals().weak_text_color());
        ctx.request_repaint_after(if self.win.is_some() { Duration::from_millis(200) } else { LOOK });
        let Some(w) = self.win.as_mut() else { return };
        if !with(|x| x.alive(w.id)).unwrap_or(false) {
            held(None, Some(w.id));
            self.ended = !self.mistaken();
            self.win = None;
            return;
        }
        let (minimized, full) = ctx.input(|i| (i.viewport().minimized.unwrap_or(false), i.viewport().fullscreen.unwrap_or(false)));
        // This window full screen and the pane all of it: the whole monitor.
        let whole = full && {
            let all = ctx.content_rect();
            (rect.min - all.min).length() < 1.0 && (rect.max - all.max).length() < 1.0
        };
        if full != self.full {
            self.full = full;
            w.stuck = None;
        }
        self.whole = whole;
        if self.leaving.is_some_and(|t| !full || t.elapsed() > Duration::from_secs(2)) {
            self.leaving = None;
        }
        match place {
            _ if self.leaving.is_some() => w.hide(),
            Some(_) if super::covered(&ctx, rect) => w.give_way(self.owner),
            Some((to, owner)) if !minimized => w.put(to, owner, whole),
            _ => w.hide(),
        }
        // In full screen a window manager may keep it off its panels (which this window covers):
        // it stays held there.
        if !full && w.stuck.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
            let id = w.id;
            held(None, Some(id));
            self.win = None;
            let _ = with(|x| {
                x.own(id, 0);
                x.unhush(id);
            });
            self.said = Some(format!("This desktop's window manager places {} itself, so it stays a window of its own.", self.what));
        }
    }

    /// The pane on the screen in pixels, and this program's window it is in.
    fn place(&mut self, ctx: &egui::Context, rect: Rect) -> Option<([i32; 4], u32)> {
        let inner = ctx.input(|i| i.viewport().inner_rect)?;
        let ppp = ctx.pixels_per_point();
        let px = |v: f32| (v * ppp).round() as i32;
        let o = inner.min.to_vec2();
        let to = [px(rect.min.x + o.x), px(rect.min.y + o.y), px(rect.width()), px(rect.height())];
        let client = [px(inner.min.x), px(inner.min.y), px(inner.width()), px(inner.height())];
        let near = |a: [i32; 4], b: [i32; 4]| a.iter().zip(b).all(|(p, q)| (p - q).abs() <= 2);
        let owner = with(|x| {
            if self.owner != 0 && x.rects(self.owner).is_some_and(|r| near(r.0, client)) {
                return self.owner;
            }
            let mine = std::process::id();
            x.clients().into_iter().find(|w| x.pid(*w) == mine && x.rects(*w).is_some_and(|r| near(r.0, client))).unwrap_or(0)
        })?;
        self.owner = owner;
        (owner != 0).then_some((to, owner))
    }

    /// This window is to leave full screen. Without a compositing manager, one wholly under the
    /// program over the whole monitor (as it is once its size is back) is told it is unseen and
    /// draws nothing more, so the program could not be put back on its pane: out of sight until
    /// then.
    pub(in crate::gui) fn leaving_full(&mut self) {
        if self.whole
            && let Some(w) = self.win.as_mut()
            && with(|x| !x.composited()).unwrap_or(false)
        {
            w.hide();
            self.leaving = Some(Instant::now());
        }
    }

    /// Not drawn this frame (another tab, the home): out of sight.
    pub(in crate::gui) fn hide(&mut self) {
        let Some(id) = self.win.as_ref().map(|w| w.id) else { return };
        if self.checked.elapsed() >= LOOK {
            self.checked = Instant::now();
            if !with(|x| x.alive(id)).unwrap_or(false) {
                held(None, Some(id));
                self.ended = !self.mistaken();
                self.win = None;
                return;
            }
        }
        if let Some(w) = self.win.as_mut() {
            w.hide();
        }
    }

    /// The pane is to close: a window opened for it is asked to close and stays on the pane
    /// meanwhile, as its program may ask first (to save, to close its tabs), answered there;
    /// false while it is open. Else it goes now (one already open back where it was): true.
    pub(in crate::gui) fn ask_close(&mut self) -> bool {
        if let Some(id) = self.win.as_ref().filter(|w| w.new).map(|w| w.id) {
            let asked = with(|x| {
                x.alive(id).then(|| {
                    let open = x.over(id);
                    x.message(id, x.a._NET_CLOSE_WINDOW, [0, 2, 0, 0, 0]);
                    open
                })
            })
            .flatten();
            if asked.is_some() {
                self.asked = asked;
                // Brought up should it be under this window, its question with it.
                if let Some(w) = self.win.as_mut() {
                    w.lift = Some(Instant::now());
                }
                return false;
            }
        }
        self.close();
        true
    }

    /// Its program, asked to close, has opened a window over its window since: the question it
    /// asks first.
    pub(in crate::gui) fn asking(&self) -> bool {
        let (Some(w), Some(open)) = (&self.win, &self.asked) else { return false };
        with(|x| x.over(w.id).iter().any(|o| !open.contains(o))).unwrap_or(false)
    }

    pub(in crate::gui) fn close(&mut self) {
        if let Some(c) = self.key.take().and_then(catch::forget) {
            let _ = with(|x| {
                x.own(c.win, 0);
                x.unhush(c.win);
                x.message(c.win, x.a._NET_CLOSE_WINDOW, [0, 2, 0, 0, 0]);
            });
        }
        self.asked = None;
        let Some(w) = self.win.take() else { return };
        held(None, Some(w.id));
        let _ = with(|x| {
            if !x.alive(w.id) {
                return;
            }
            x.own(w.id, 0);
            x.unhush(w.id);
            if w.iconic {
                let _ = x.conn.map_window(w.id);
            }
            if w.new {
                x.message(w.id, x.a._NET_CLOSE_WINDOW, [0, 2, 0, 0, 0]);
            } else if let Some(was) = w.was {
                x.aim(w.id, was);
            }
        });
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.close();
    }
}

impl Win {
    fn put(&mut self, to: [i32; 4], owner: u32, whole: bool) {
        let _ = with(|x| {
            // Without a compositing manager it leaves this window the monitor's top row instead:
            // wholly covered, this window would run no frames (and the dock is there).
            let bare = whole && !x.composited();
            let monitor = whole;
            let whole = whole && !bare;
            let to = if bare { [to[0], to[1] + 1, to[2], to[3] - 1] } else { to };
            let full = x.cards(self.id, x.a._NET_WM_STATE, AtomEnum::ATOM).contains(&x.a._NET_WM_STATE_FULLSCREEN);
            if !whole {
                (self.full_asked, self.refused) = (None, false);
                // Let go of for full screen: kept above this window again once out of it (Xfce's
                // window manager takes neither change for a window kept above another).
                if self.loose && !full {
                    self.loose = false;
                }
            }
            // Back from under this window: up again (one let go of is kept above by being active).
            if self.lowered.take().is_some() && self.loose {
                let _ = x.conn.configure_window(self.id, &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE));
                x.activate(self.id, owner);
                self.lift = Some(Instant::now());
                self.back = Some(Instant::now());
            }
            let raised = !self.owned && !self.loose;
            if raised || self.last != (full, whole) {
                self.lift = Some(Instant::now());
            }
            self.last = (full, whole);
            if raised {
                x.own(self.id, owner);
                self.owned = true;
                self.back = Some(Instant::now());
                // Over this window again, after being let under it: restacked where the window
                // manager knows how, else raised.
                x.message(self.id, x.a._NET_RESTACK_WINDOW, [2, owner, 0, 0, 0]);
                let _ = x.conn.configure_window(self.id, &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE));
            }
            if self.iconic || x.iconic(self.id) {
                let _ = x.conn.map_window(self.id);
                self.back = Some(Instant::now());
                // Still minimized a moment after: a window manager that keeps minimized windows
                // mapped (Cinnamon's) brings one back only as the active window.
                match self.restored {
                    Some(t) if !self.iconic && t.elapsed() > Duration::from_millis(300) => {
                        x.activate(self.id, owner);
                        self.restored = None;
                    }
                    Some(_) => {}
                    None => self.restored = Some(Instant::now()),
                }
                self.iconic = false;
            } else {
                self.restored = None;
            }
            // A moment after it is back (shown by then): its questions over it again, out of being
            // minimized; over the whole monitor, the newest made the active one as well.
            if let Some(t) = self.back
                && t.elapsed() > Duration::from_millis(150)
            {
                if x.viewable(self.id) && !x.iconic(self.id) {
                    let asks = x.over(self.id);
                    for q in &asks {
                        if x.iconic(*q) {
                            let _ = x.conn.map_window(*q);
                        }
                        let _ = x.conn.configure_window(*q, &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE));
                    }
                    if monitor && let Some(q) = asks.last() {
                        x.message(*q, x.a._NET_ACTIVE_WINDOW, [2, 0, self.id, 0, 0]);
                    }
                    self.back = None;
                } else if t.elapsed() > Duration::from_secs(2) {
                    self.back = None;
                }
            }
            let Some((_, seen)) = x.rects(self.id) else { return };
            // The pane is the whole monitor: full screen too, as a window manager keeps any other
            // window off its panels. One that will not for a window kept above another (Xfce's) is
            // asked again with it let go of; one that will not at all has it put on the pane.
            if whole && !self.refused && !full {
                match self.full_asked {
                    None => {
                        x.message(self.id, x.a._NET_WM_STATE, [1, x.a._NET_WM_STATE_FULLSCREEN, 0, 2, 0]);
                        self.full_asked = Some(Instant::now());
                    }
                    Some(t) if t.elapsed() > Duration::from_millis(500) => {
                        if self.loose {
                            (self.refused, self.loose) = (true, false);
                        } else {
                            x.own(self.id, 0);
                            (self.owned, self.loose, self.full_asked) = (false, true, None);
                        }
                    }
                    Some(_) => {}
                }
            }
            let there = if whole && !self.refused {
                full
            } else {
                // Where it is; its size may stop short of the pane's (a terminal sized by its
                // cells). Asked there, it leaves full screen; kept off it a while (by a panel, in
                // full screen), it is asked no more.
                let there = (seen[0] - to[0]).abs() + (seen[1] - to[1]).abs() <= 2;
                let sized = seen[2..].iter().zip(&to[2..]).all(|(a, b)| (a - b).abs() <= 1);
                let again = self.stuck.is_none_or(|t| t.elapsed() < Duration::from_secs(3));
                if (!there && (again || self.aimed != Some(to))) || (!sized && self.aimed != Some(to)) {
                    x.aim(self.id, to);
                    self.aimed = Some(to);
                }
                there
            };
            // Under this window all the same a moment after a change made here (Marco keeps a full
            // screen window with the focus over those kept above it): made the active one. Not
            // later: this window brought up over it by hand stays so.
            if self.lift.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
                self.lift = None;
            }
            let stack = x.cards(x.root, x.a._NET_CLIENT_LIST_STACKING, AtomEnum::WINDOW);
            let at = |w: u32| stack.iter().position(|s| *s == w);
            let under = matches!((at(self.id), at(owner)), (Some(a), Some(b)) if a < b);
            if under && self.lift.is_some() && !raised && !self.hushed {
                match self.sunk {
                    Some(t) if t.elapsed() > Duration::from_millis(300) => {
                        x.activate(self.id, owner);
                        (self.sunk, self.lift) = (None, None);
                    }
                    Some(_) => {}
                    None => self.sunk = Some(Instant::now()),
                }
            } else {
                self.sunk = None;
            }
            // Where it is may not stay off, unless the window manager keeps it where it wants it.
            if there {
                self.stuck = None;
            } else {
                self.stuck.get_or_insert_with(Instant::now);
            }
            if self.hushed {
                let ready = there && x.viewable(self.id);
                let placed = if ready { *self.placed.get_or_insert_with(Instant::now) } else { Instant::now() };
                if (ready && placed.elapsed() >= Duration::from_millis(80)) || self.since.elapsed() > Duration::from_secs(3) {
                    x.unhush(self.id);
                    self.hushed = false;
                    if self.new {
                        x.message(self.id, x.a._NET_ACTIVE_WINDOW, [2, 0, owner, 0, 0]);
                    }
                }
            }
        });
    }

    /// Minimized, as its tab is not shown (or this window is); let go of first, as a window
    /// manager may minimize the window it is kept above with it (Openbox does).
    fn hide(&mut self) {
        self.stuck = None;
        if self.iconic {
            return;
        }
        let _ = with(|x| {
            if self.owned {
                x.own(self.id, 0);
                self.owned = false;
            }
            x.message(self.id, x.a.WM_CHANGE_STATE, [3, 0, 0, 0, 0]);
        });
        self.iconic = true;
    }

    /// Under this window while a menu or a dialog of it is over the pane: let go of it, and put
    /// under it as the window manager allows (_NET_RESTACK_WINDOW, or lowered and this window
    /// raised); minimized should it still be over it a moment later.
    fn give_way(&mut self, owner: u32) {
        if owner == 0 || self.iconic {
            return;
        }
        let _ = with(|x| {
            if self.owned || (self.loose && self.lowered.is_none()) {
                x.own(self.id, 0);
                x.message(self.id, x.a._NET_RESTACK_WINDOW, [2, owner, 1, 0, 0]);
                let _ = x.conn.configure_window(self.id, &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::BELOW));
                let _ = x.conn.configure_window(owner, &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE));
                self.owned = false;
                self.lowered = Some(Instant::now());
                return;
            }
            let stack = x.cards(x.root, x.a._NET_CLIENT_LIST_STACKING, AtomEnum::WINDOW);
            let at = |w: u32| stack.iter().position(|s| *s == w);
            let over = matches!((at(self.id), at(owner)), (Some(a), Some(b)) if a > b);
            if over && self.lowered.is_some_and(|t| t.elapsed() > Duration::from_millis(150)) {
                x.message(self.id, x.a.WM_CHANGE_STATE, [3, 0, 0, 0, 0]);
                self.iconic = true;
            }
        });
    }
}
