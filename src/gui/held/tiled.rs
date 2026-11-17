//! On Sway and Hyprland, where only the compositor places windows and runs no one else's scripts:
//! a thread of this program holds the program's own window over its pane through the compositor's
//! IPC. The compositor starts the program where its first window opens out of sight (Sway: on a
//! workspace of its own, left again in the same step; Hyprland: on a special workspace), so it is
//! never seen tiled. There it is made a borderless floating window of the pane's size, then put
//! over the pane: moved with this window, kept over it, put out of sight while the pane is not
//! shown, and let go when the pane closes.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, Rect};

/// A window as the compositor tells of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Win {
    pub id: String,
    pub pid: u32,
    /// Its workspace, as the compositor takes it in a move.
    pub space: String,
    /// Where its contents are (x, y, width, height), in the layout's logical pixels.
    pub rect: [i32; 4],
    pub floating: bool,
    /// Full screen in its compositor.
    pub full: bool,
    pub visible: bool,
    pub focused: bool,
    /// Its program's id and its title, to find a program's window by name.
    pub name: String,
    /// Its place in the stack (higher is over), where the compositor tells it.
    pub order: Option<usize>,
    /// The sshremote process keeping it out of sight, by its mark or its workspace's name.
    pub tag: Option<u32>,
    /// How it looked before it was held, to look so again when let go (Sway: its border).
    pub look: String,
}

/// What this program asks of the compositor.
pub(super) trait Wm: Send + Sync {
    fn name(&self) -> &'static str;
    fn windows(&self) -> Option<Vec<Win>>;
    /// The workspace in front, where windows let go of are put.
    fn focused_space(&self) -> Option<String>;
    /// A connection the compositor tells of changes on (only read to wake up).
    fn events(&self) -> Option<UnixStream>;
    /// The workspace the window of launch `token` opens on, out of sight.
    fn catch_space(&self, token: u32) -> String;
    /// Has the compositor run `script`, its first window opening on `space`.
    fn launch(&self, space: &str, script: &str) -> bool;
    fn out_of_sight(&self, w: &Win) -> bool;
    /// Made borderless and floating, and put out of sight.
    fn ready(&self, w: &Win);
    fn size(&self, id: &str, w: i32, h: i32);
    /// Put at `rect` on `space` (from out of sight, or moved).
    fn put(&self, w: &Win, space: &str, rect: [i32; 4]);
    /// Put out of sight (where it is not already).
    fn hide(&self, w: &Win);
    /// Put over the rest of its workspace's floating windows.
    fn raise(&self, w: &Win, space: &str, rect: [i32; 4]);
    fn focus(&self, id: &str);
    /// Whether a window can be over a full screen one (Hyprland: raised and focused; Sway shows
    /// nothing over one).
    fn over_full(&self) -> bool;
    fn close(&self, id: &str);
    /// Let go of: a window of its own on `space`, as it was before it was held (else tiled).
    fn free(&self, w: &Win, space: &str, was: Option<&Win>);
}

/// The compositor here, if it is Sway or Hyprland: the one that has this program's window (its
/// socket may be named to a program it does not show, one in a desktop nested in it).
fn wm() -> Option<&'static dyn Wm> {
    static FOUND: OnceLock<Vec<Box<dyn Wm>>> = OnceLock::new();
    // Known once this window is seen there (it may not be yet, as the window opens).
    static OURS: OnceLock<usize> = OnceLock::new();
    let found = FOUND.get_or_init(|| {
        let sway = super::sway::Sway::find().map(|s| Box::new(s) as Box<dyn Wm>);
        let hypr = super::hypr::Hypr::find().map(|h| Box::new(h) as Box<dyn Wm>);
        sway.into_iter().chain(hypr).collect()
    });
    if let Some(&i) = OURS.get() {
        return Some(found[i].as_ref());
    }
    let me = std::process::id();
    let i = found.iter().position(|wm| wm.windows().is_some_and(|all| all.iter().any(|w| w.pid == me)))?;
    Some(found[*OURS.get_or_init(|| i)].as_ref())
}

/// A Sway or Hyprland session.
pub(in crate::gui) fn here() -> bool {
    wm().is_some()
}

/// A compositor that shows nothing over a full screen window (Sway).
pub(in crate::gui) fn full_hides() -> bool {
    wm().is_some_and(|wm| !wm.over_full())
}

/// How long a window may take to take the pane's size before it is shown anyway, and how often
/// it is asked meanwhile.
const SIZING: Duration = Duration::from_millis(600);
const ASK_AGAIN: Duration = Duration::from_millis(100);
/// After this a program's window may be found by its name (one that shows a window it had open).
const BY_NAME: Duration = Duration::from_secs(3);
const GIVE_UP: Duration = Duration::from_secs(20);
/// A window moved or sized by hand is put back no sooner than this.
const AGAIN: Duration = Duration::from_secs(1);
/// Size steps some programs keep to (a terminal's cells): nearer than this is the pane's size.
const SLACK: i32 = 40;

struct Entry {
    token: u32,
    catch: String,
    /// The first word of the program's name, lowercase, to find its window by.
    first: String,
    since: Instant,
    id: Option<String>,
    /// Opened for the pane (closed with it), rather than one already open.
    new: bool,
    was: Option<Win>,
    /// Still out of sight, taking the pane's size (asked when).
    preparing: bool,
    sizing: Option<Instant>,
    sized: Option<((i32, i32), Instant)>,
    seen: bool,
    /// From the pane: this window's width and height, the pane's x, y, width and height in it.
    rel: Option<[i32; 6]>,
    shown: bool,
    /// Where it was last put (none while out of sight), and the last place it was seen at.
    aim: Option<(String, [i32; 4])>,
    last: Option<(String, [i32; 4])>,
    put_at: Instant,
    gone: bool,
    /// Asked to close with its pane: its program's other windows then; and whether it has opened
    /// one since (the question it asks first, to save or to close its tabs).
    asked: Option<Vec<String>>,
    asking: bool,
    /// Raised over this window while it is full screen, since it was last put out of sight.
    lifted: bool,
}

#[derive(Default)]
struct Keep {
    entries: Vec<Entry>,
    /// Workspaces of panes closed before their window came, while one may still come there.
    dead: Vec<(String, Instant)>,
    /// Windows asked to close with their pane, left to it a while (they may ask to save first).
    closing: Vec<(String, Instant)>,
    /// This window floats (it is followed closely while it may be dragged).
    follow: bool,
}

fn kept() -> &'static Mutex<Keep> {
    static KEEP: OnceLock<Mutex<Keep>> = OnceLock::new();
    KEEP.get_or_init(Default::default)
}

fn keep() -> MutexGuard<'static, Keep> {
    kept().lock().unwrap_or_else(|e| e.into_inner())
}

fn near(a: i32, b: i32, by: i32) -> bool {
    (a - b).abs() <= by
}

/// Whether process `pid` still runs.
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

impl Keep {
    /// How long the thread may wait before looking again (never, with nothing held).
    fn pace(&self) -> Option<Duration> {
        if self.entries.is_empty() && self.dead.is_empty() {
            None
        } else if self.entries.iter().any(|e| e.preparing) {
            Some(Duration::from_millis(30))
        } else if self.entries.iter().any(|e| e.id.is_none()) {
            Some(Duration::from_millis(100))
        } else if self.follow && self.entries.iter().any(|e| e.aim.is_some()) {
            Some(Duration::from_millis(60))
        } else {
            Some(Duration::from_millis(400))
        }
    }

    /// This window: its process's one of the pane's window size, or its only one.
    fn ours<'a>(&self, wm: &dyn Wm, wins: &'a [Win]) -> Option<&'a Win> {
        let me = std::process::id();
        let mine: Vec<&Win> = wins.iter().filter(|w| w.pid == me && !wm.out_of_sight(w)).collect();
        let size = self.entries.iter().find_map(|e| e.rel);
        size.and_then(|r| mine.iter().find(|w| near(w.rect[2], r[0], 3) && near(w.rect[3], r[1], 3)).copied()).or(if mine.len() == 1 { Some(mine[0]) } else { None })
    }

    /// Brings the windows to where they should be.
    fn mind(&mut self, wm: &dyn Wm, wins: &[Win]) {
        let me = std::process::id();
        self.dead.retain(|(_, t)| t.elapsed() < Duration::from_secs(60));
        self.closing.retain(|(_, t)| t.elapsed() < Duration::from_secs(10));
        let held: Vec<String> = self.entries.iter().filter_map(|e| e.id.clone()).collect();
        // Windows no pane waits for: of panes closed before they came, or of a program that ended.
        for w in wins {
            let late = self.dead.iter().any(|(s, _)| *s == w.space);
            let orphan = w.tag.is_some_and(|p| p != me && !alive(p)) || (w.tag == Some(me) && !held.contains(&w.id) && !self.entries.iter().any(|e| e.catch == w.space));
            let closing = self.closing.iter().any(|(id, _)| *id == w.id);
            if (late || orphan) && !closing && let Some(front) = wm.focused_space() {
                wm.free(w, &front, None);
            }
        }
        let ours = self.ours(wm, wins).cloned();
        self.follow = ours.as_ref().is_some_and(|o| o.floating);
        for e in &mut self.entries {
            let Some(id) = e.id.clone() else {
                let by_name = || {
                    wins.iter().find(|w| {
                        let name = w.name.to_lowercase();
                        w.pid != me && !held.contains(&w.id) && !wm.out_of_sight(w) && name.contains(&e.first) && !name.contains("ssh remote")
                    })
                };
                if let Some(w) = wins.iter().find(|w| w.space == e.catch) {
                    (e.id, e.new, e.preparing) = (Some(w.id.clone()), true, true);
                    wm.ready(w);
                } else if e.since.elapsed() >= BY_NAME
                    && !e.first.is_empty()
                    && let Some(w) = by_name()
                {
                    (e.id, e.new, e.preparing, e.was) = (Some(w.id.clone()), false, true, Some(w.clone()));
                    wm.ready(w);
                }
                continue;
            };
            let Some(w) = wins.iter().find(|w| w.id == id) else {
                e.gone = true;
                continue;
            };
            if let Some(open) = &e.asked {
                e.asking = wins.iter().any(|o| o.pid == w.pid && o.id != id && !open.contains(&o.id));
            }
            let Some(rel) = e.rel else { continue };
            if e.preparing {
                // Asked again until it has the size (a program may yet answer an older request, and
                // the compositor then takes its size), or shown anyway after a while.
                let want = (rel[4], rel[5]);
                let sized = near(w.rect[2], want.0, SLACK) && near(w.rect[3], want.1, SLACK);
                let since = *e.sizing.get_or_insert_with(Instant::now);
                if !sized && since.elapsed() < SIZING {
                    if e.sized.is_none_or(|(size, at)| size != want || at.elapsed() >= ASK_AGAIN) {
                        wm.size(&id, want.0, want.1);
                        e.sized = Some((want, Instant::now()));
                    }
                    continue;
                }
                e.preparing = false;
            }
            match ours.as_ref().filter(|o| o.visible && e.shown) {
                Some(o) => {
                    let aim = [o.rect[0] + rel[2], o.rect[1] + rel[3], rel[4], rel[5]];
                    let wanted = (o.space.clone(), aim);
                    let off = w.space != o.space
                        || wm.out_of_sight(w)
                        || !near(w.rect[0], aim[0], 1)
                        || !near(w.rect[1], aim[1], 1)
                        || !near(w.rect[2], aim[2], SLACK)
                        || !near(w.rect[3], aim[3], SLACK);
                    if e.aim.as_ref() != Some(&wanted) || (off && e.put_at.elapsed() >= AGAIN) {
                        wm.put(w, &o.space, aim);
                        e.put_at = Instant::now();
                        if !e.seen {
                            e.seen = true;
                            wm.focus(&id);
                        }
                        e.last = Some(wanted.clone());
                        e.aim = Some(wanted);
                    } else if o.floating && o.space == w.space && matches!((o.order, w.order), (Some(a), Some(b)) if a > b) {
                        // This window was brought up over it.
                        wm.raise(w, &o.space, aim);
                    }
                    // This window full screen, which is drawn over the rest: raised and focused once
                    // where the compositor then shows it over it (Hyprland).
                    if o.full && !e.lifted && wm.over_full() {
                        wm.raise(w, &o.space, aim);
                        wm.focus(&id);
                        e.lifted = true;
                    }
                    if !o.full {
                        e.lifted = false;
                    }
                }
                None => {
                    if !wm.out_of_sight(w) {
                        wm.hide(w);
                    }
                    e.aim = None;
                    e.lifted = false;
                }
            }
        }
    }

    /// The pane of `token` is to close: a window opened for it is asked to close and stays held
    /// meanwhile, as its program may ask first; false if there is none.
    fn ask(&mut self, wm: &dyn Wm, token: u32) -> bool {
        let Some(e) = self.entries.iter_mut().find(|e| e.token == token && e.new) else { return false };
        let Some(id) = e.id.clone() else { return false };
        let wins = wm.windows().unwrap_or_default();
        let Some(w) = wins.iter().find(|w| w.id == id) else { return false };
        e.asked = Some(wins.iter().filter(|o| o.pid == w.pid && o.id != id).map(|o| o.id.clone()).collect());
        e.asking = false;
        wm.close(&id);
        true
    }

    /// The pane of `token` closes: its window closed with it (it may ask to save first, seen where
    /// the pane was), or one that was open before let go as it was.
    fn release(&mut self, wm: &dyn Wm, token: u32) {
        let Some(i) = self.entries.iter().position(|e| e.token == token) else { return };
        let e = self.entries.remove(i);
        let Some(id) = e.id else {
            self.dead.push((e.catch, Instant::now()));
            return;
        };
        let wins = wm.windows().unwrap_or_default();
        let Some(w) = wins.iter().find(|w| w.id == id) else { return };
        if e.new {
            if wm.out_of_sight(w)
                && e.seen
                && let Some((space, rect)) = &e.last
            {
                wm.put(w, space, *rect);
            }
            wm.close(&id);
            self.closing.push((id, Instant::now()));
        } else if let Some(space) = e.was.as_ref().map(|v| v.space.clone()).or_else(|| wm.focused_space()) {
            wm.free(w, &space, e.was.as_ref());
        }
    }
}

/// How this program's other threads wake the keeping thread.
fn poke_pair() -> Option<&'static (UnixStream, UnixStream)> {
    static PAIR: OnceLock<Option<(UnixStream, UnixStream)>> = OnceLock::new();
    PAIR.get_or_init(|| {
        let (a, b) = UnixStream::pair().ok()?;
        a.set_nonblocking(true).ok()?;
        b.set_nonblocking(true).ok()?;
        Some((a, b))
    })
    .as_ref()
}

fn poke() {
    if let Some((w, _)) = poke_pair() {
        let _ = (&*w).write(&[1]);
    }
}

/// Reads all there is (false once the other end is closed).
fn drain(mut s: &UnixStream) -> bool {
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) => return false,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return true,
        }
    }
}

/// The keeping thread, started with the first held window.
fn start_keeper(wm: &'static dyn Wm) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let Some((_, wake)) = poke_pair() else { return };
        let _ = std::thread::Builder::new().name("held-windows".into()).spawn(move || {
            let mut events: Option<UnixStream> = None;
            loop {
                let pace = keep().pace();
                if pace.is_none() {
                    events = None;
                } else if events.is_none() {
                    events = wm.events().filter(|s| s.set_nonblocking(true).is_ok());
                }
                let mut fds = [
                    libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                    libc::pollfd { fd: events.as_ref().map_or(-1, |s| s.as_raw_fd()), events: libc::POLLIN, revents: 0 },
                ];
                let ms = pace.map_or(-1, |d| d.as_millis() as i32);
                // SAFETY: two pollfd entries, as the count says.
                unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
                drain(wake);
                if fds[1].revents != 0 && !events.as_ref().is_some_and(drain) {
                    events = None;
                }
                if pace.is_none() && keep().pace().is_none() {
                    continue;
                }
                if let Some(wins) = wm.windows() {
                    keep().mind(wm, &wins);
                }
            }
        });
    });
}

/// The script the compositor runs: it removes itself, then becomes the line's shell, which becomes
/// the program (Hyprland knows a program's window by the process it started; Sway by any of its
/// children too).
fn script(token: u32, line: &str) -> Option<String> {
    let dir = std::env::var("XDG_RUNTIME_DIR").ok().filter(|d| !d.is_empty())?;
    let path = format!("{dir}/sshremote-start-{}-{token}.sh", std::process::id());
    // Passed on in a command line as it is.
    if !path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)) {
        return None;
    }
    let quoted = format!("'{}'", line.replace('\'', "'\\''"));
    std::fs::write(&path, format!("rm -f -- \"$0\"\nexec sh -c {quoted}\n")).ok()?;
    Some(path)
}

/// Shows the held windows again if this program ends with them out of sight.
pub(crate) fn bring_back() {
    let Some(wm) = wm() else { return };
    // Not waited for: this may be the thread that holds it, failing.
    let ids: Vec<String> = match kept().try_lock() {
        Ok(k) => k.entries.iter().filter_map(|e| e.id.clone()).collect(),
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().entries.iter().filter_map(|e| e.id.clone()).collect(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    if ids.is_empty() {
        return;
    }
    let (Some(wins), Some(front)) = (wm.windows(), wm.focused_space()) else { return };
    for w in wins.iter().filter(|w| ids.contains(&w.id) && wm.out_of_sight(w)) {
        wm.free(w, &front, None);
    }
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

pub(in crate::gui) struct Held {
    token: u32,
    what: String,
    said: Option<String>,
    since: Instant,
    told: Option<([i32; 6], bool)>,
}

impl Held {
    pub(in crate::gui) fn start(kind: u8, text: &str) -> Held {
        let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut what = text.trim().trim_end_matches('/').rsplit('/').next().unwrap_or(text).to_string();
        let mut said = None;
        let started = match (wm(), crate::apps::shell_line(kind, text)) {
            (_, Err(e)) => {
                said = Some(format!("It could not be started: {e}."));
                false
            }
            (None, _) => false,
            (Some(wm), Ok((line, name))) => {
                if !name.is_empty() {
                    what = name.clone();
                }
                let catch = wm.catch_space(token);
                let first = name.to_lowercase().split_whitespace().next().unwrap_or("").to_string();
                // In the list before it is started, so its window is looked for from the start.
                keep().entries.push(Entry {
                    token,
                    catch: catch.clone(),
                    first,
                    since: Instant::now(),
                    id: None,
                    new: false,
                    was: None,
                    preparing: false,
                    sizing: None,
                    sized: None,
                    seen: false,
                    rel: None,
                    shown: false,
                    aim: None,
                    last: None,
                    put_at: Instant::now(),
                    gone: false,
                    asked: None,
                    asking: false,
                    lifted: false,
                });
                let ok = script(token, &line).is_some_and(|path| wm.launch(&catch, &path));
                if ok {
                    start_keeper(wm);
                    poke();
                } else {
                    keep().entries.retain(|e| e.token != token);
                }
                ok
            }
        };
        if !started && said.is_none() {
            // Started as any program is: a window of its own.
            said = Some(match crate::apps::launch(kind, text) {
                Ok(_) => format!("{} did not start it for this app, so this program stays a window of its own.", wm().map_or("The desktop", |w| w.name())),
                Err(e) => format!("It could not be started: {e}."),
            });
        }
        Held { token, what, said, since: Instant::now(), told: None }
    }

    pub(in crate::gui) fn ended(&self) -> bool {
        keep().entries.iter().any(|e| e.token == self.token && e.gone)
    }

    pub(in crate::gui) fn show(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let caught = keep().entries.iter().any(|e| e.token == self.token && e.id.is_some());
        let text = match (&self.said, caught) {
            (Some(s), _) => s.clone(),
            (None, true) => format!("{} is here", self.what),
            (None, false) if self.since.elapsed() > GIVE_UP => format!("{} opened no window of its own to show here.", self.what),
            (None, false) => format!("Starting {}\u{2026}", self.what),
        };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), ui.visuals().weak_text_color());
        ctx.request_repaint_after(Duration::from_millis(250));
        if self.said.is_some() {
            return;
        }
        let shown = !super::covered(&ctx, rect);
        let z = ctx.zoom_factor();
        let px = |v: f32| (v * z).round() as i32;
        let whole = ctx.content_rect();
        let at = [px(whole.width()), px(whole.height()), px(rect.min.x - whole.min.x), px(rect.min.y - whole.min.y), px(rect.width()), px(rect.height())];
        self.tell(at, shown);
    }

    fn tell(&mut self, at: [i32; 6], shown: bool) {
        if self.told == Some((at, shown)) {
            return;
        }
        self.told = Some((at, shown));
        if let Some(e) = keep().entries.iter_mut().find(|e| e.token == self.token) {
            (e.rel, e.shown) = (Some(at), shown);
        }
        poke();
    }

    pub(in crate::gui) fn hide(&mut self) {
        if let Some((at, true)) = self.told {
            self.tell(at, false);
        }
    }

    /// The pane is to close: a window opened for it is asked to close and stays on the pane
    /// meanwhile, as its program may ask first (to save, to close its tabs), answered there;
    /// false while it is open. Else it goes now (one already open back where it was): true.
    pub(in crate::gui) fn ask_close(&mut self) -> bool {
        if wm().is_some_and(|wm| keep().ask(wm, self.token)) {
            poke();
            return false;
        }
        self.close();
        true
    }

    /// Its program, asked to close, has opened another window since: the question it asks first.
    pub(in crate::gui) fn asking(&self) -> bool {
        keep().entries.iter().any(|e| e.token == self.token && e.asking)
    }

    pub(in crate::gui) fn close(&mut self) {
        if let Some(wm) = wm() {
            keep().release(wm, self.token);
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compositor that does what it is asked at once, and says what it was asked.
    #[derive(Default)]
    struct Fake {
        wins: Mutex<Vec<Win>>,
        said: Mutex<Vec<String>>,
    }

    impl Fake {
        fn with(&self, id: &str, f: impl FnOnce(&mut Win)) {
            if let Some(w) = self.wins.lock().unwrap().iter_mut().find(|w| w.id == id) {
                f(w);
            }
        }
        fn say(&self, s: String) {
            self.said.lock().unwrap().push(s);
        }
        fn win(&self, id: &str) -> Win {
            self.wins.lock().unwrap().iter().find(|w| w.id == id).cloned().unwrap()
        }
    }

    impl Wm for Fake {
        fn name(&self) -> &'static str {
            "Fake"
        }
        fn windows(&self) -> Option<Vec<Win>> {
            Some(self.wins.lock().unwrap().clone())
        }
        fn focused_space(&self) -> Option<String> {
            Some("1".into())
        }
        fn events(&self) -> Option<UnixStream> {
            None
        }
        fn catch_space(&self, token: u32) -> String {
            format!("catch-{token}")
        }
        fn launch(&self, _: &str, _: &str) -> bool {
            true
        }
        fn out_of_sight(&self, w: &Win) -> bool {
            w.space == "hidden" || w.space.starts_with("catch-")
        }
        fn ready(&self, w: &Win) {
            self.say(format!("ready {}", w.id));
            self.with(&w.id, |w| (w.space, w.floating) = ("hidden".into(), true));
        }
        fn size(&self, id: &str, w: i32, h: i32) {
            self.say(format!("size {id} {w}x{h}"));
            self.with(id, |v| (v.rect[2], v.rect[3]) = (w, h));
        }
        fn put(&self, w: &Win, space: &str, rect: [i32; 4]) {
            self.say(format!("put {} {space} {rect:?}", w.id));
            self.with(&w.id, |w| (w.space, w.rect) = (space.into(), rect));
        }
        fn hide(&self, w: &Win) {
            self.say(format!("hide {}", w.id));
            self.with(&w.id, |w| w.space = "hidden".into());
        }
        fn raise(&self, w: &Win, _: &str, _: [i32; 4]) {
            self.say(format!("raise {}", w.id));
        }
        fn focus(&self, id: &str) {
            self.say(format!("focus {id}"));
        }
        fn over_full(&self) -> bool {
            true
        }
        fn close(&self, id: &str) {
            self.say(format!("close {id}"));
        }
        fn free(&self, w: &Win, space: &str, _: Option<&Win>) {
            self.say(format!("free {} {space}", w.id));
        }
    }

    fn entry(token: u32, catch: &str) -> Entry {
        Entry {
            token,
            catch: catch.into(),
            first: "calc".into(),
            since: Instant::now(),
            id: None,
            new: false,
            was: None,
            preparing: false,
            sizing: None,
            sized: None,
            seen: false,
            rel: None,
            shown: false,
            aim: None,
            last: None,
            put_at: Instant::now(),
            gone: false,
            asked: None,
            asking: false,
            lifted: false,
        }
    }

    #[test]
    fn a_window_is_caught_out_of_sight_sized_then_held_over_its_pane() {
        let me = std::process::id();
        let wm = Fake::default();
        let ours = Win { id: "ours".into(), pid: me, space: "1".into(), rect: [100, 50, 800, 600], visible: true, ..Default::default() };
        wm.wins.lock().unwrap().push(ours);
        let mut k = Keep::default();
        k.entries.push(entry(7, "catch-7"));
        k.entries[0].rel = Some([800, 600, 10, 40, 500, 400]);
        k.entries[0].shown = true;
        k.mind(&wm, &wm.windows().unwrap());
        assert!(wm.said.lock().unwrap().is_empty(), "nothing yet");
        // The program's window opens where it is out of sight.
        wm.wins.lock().unwrap().push(Win { id: "calc".into(), pid: 4242, space: "catch-7".into(), rect: [0, 0, 1280, 800], ..Default::default() });
        for _ in 0..3 {
            k.mind(&wm, &wm.windows().unwrap());
        }
        let said = wm.said.lock().unwrap().clone();
        assert_eq!(said, ["ready calc", "size calc 500x400", "put calc 1 [110, 90, 500, 400]", "focus calc"], "sized unseen, then put over the pane");
        // The pane is covered: out of sight; shown again: back.
        k.entries[0].shown = false;
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(wm.win("calc").space, "hidden");
        k.entries[0].shown = true;
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(wm.win("calc").rect, [110, 90, 500, 400]);
        assert_eq!(wm.said.lock().unwrap().iter().filter(|s| s.starts_with("focus")).count(), 1, "focused the first time only");
        // This window moves: the held one with it; it floats and is raised over it: back over.
        wm.with("ours", |w| (w.rect[0], w.floating, w.order) = (300, true, Some(2)));
        wm.with("calc", |w| w.order = Some(1));
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(wm.win("calc").rect, [310, 90, 500, 400]);
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(wm.said.lock().unwrap().last().unwrap(), "raise calc");
        // This window goes out of sight (another tab in front of it): so does the held one.
        wm.with("ours", |w| w.visible = false);
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(wm.win("calc").space, "hidden");
        // The pane closes: the window was opened for it, so it is closed, seen where it was.
        wm.with("ours", |w| w.visible = true);
        k.release(&wm, 7);
        let said = wm.said.lock().unwrap().clone();
        assert_eq!(&said[said.len() - 2..], ["put calc 1 [310, 90, 500, 400]", "close calc"]);
        assert!(k.entries.is_empty());
    }

    #[test]
    fn a_window_asked_to_close_stays_held_while_its_program_asks() {
        let me = std::process::id();
        let wm = Fake::default();
        wm.wins.lock().unwrap().push(Win { id: "ours".into(), pid: me, space: "1".into(), rect: [0, 0, 800, 600], visible: true, ..Default::default() });
        wm.wins.lock().unwrap().push(Win { id: "other".into(), pid: 500, space: "2".into(), ..Default::default() });
        wm.wins.lock().unwrap().push(Win { id: "ed".into(), pid: 500, space: "catch-5".into(), rect: [0, 0, 400, 300], ..Default::default() });
        let mut k = Keep::default();
        k.entries.push(entry(5, "catch-5"));
        k.entries[0].rel = Some([800, 600, 0, 30, 400, 300]);
        k.entries[0].shown = true;
        for _ in 0..3 {
            k.mind(&wm, &wm.windows().unwrap());
        }
        assert!(k.ask(&wm, 5));
        assert_eq!(wm.said.lock().unwrap().last().unwrap(), "close ed");
        k.mind(&wm, &wm.windows().unwrap());
        assert!(!k.entries[0].asking, "a window it had open before is no question");
        // It asks to save in a window of its own: held still, and asking.
        wm.wins.lock().unwrap().push(Win { id: "save?".into(), pid: 500, space: "1".into(), ..Default::default() });
        k.mind(&wm, &wm.windows().unwrap());
        assert!(k.entries[0].asking);
        assert_eq!(wm.win("ed").space, "1", "still over its pane");
        // Told yes: both go, and the pane ends with its window.
        wm.wins.lock().unwrap().retain(|w| w.id != "save?" && w.id != "ed");
        k.mind(&wm, &wm.windows().unwrap());
        assert!(k.entries[0].gone);
        // One already open is not asked: it is let go as it was.
        k.entries[0].new = false;
        assert!(!k.ask(&wm, 5));
    }

    #[test]
    fn over_this_window_full_screen_it_is_raised_and_focused_once_each_time_it_is_shown() {
        let me = std::process::id();
        let wm = Fake::default();
        wm.wins.lock().unwrap().push(Win { id: "ours".into(), pid: me, space: "1".into(), rect: [0, 0, 1280, 800], visible: true, full: true, ..Default::default() });
        wm.wins.lock().unwrap().push(Win { id: "ed".into(), pid: 500, space: "catch-6".into(), rect: [0, 0, 1280, 800], ..Default::default() });
        let mut k = Keep::default();
        k.entries.push(entry(6, "catch-6"));
        k.entries[0].rel = Some([1280, 800, 0, 0, 1280, 800]);
        k.entries[0].shown = true;
        for _ in 0..4 {
            k.mind(&wm, &wm.windows().unwrap());
        }
        let raised = |wm: &Fake| wm.said.lock().unwrap().iter().filter(|s| *s == "raise ed").count();
        assert_eq!(raised(&wm), 1, "once");
        // Out of sight (a notice over it) and back: once more.
        k.entries[0].shown = false;
        k.mind(&wm, &wm.windows().unwrap());
        k.entries[0].shown = true;
        k.mind(&wm, &wm.windows().unwrap());
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(raised(&wm), 2);
        assert_eq!(wm.said.lock().unwrap().last().unwrap(), "focus ed");
    }

    #[test]
    fn the_start_script_removes_itself_and_becomes_the_program() {
        let Some(path) = script(9999, r#"cd '/tmp' && printf '%s\n' "it's" done"#) else { return };
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(text.starts_with("rm -f -- \"$0\"\nexec sh -c '"));
        let line = text.lines().nth(1).unwrap().strip_prefix("exec ").unwrap();
        let out = std::process::Command::new("sh").arg("-c").arg(line).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "it's\ndone\n");
    }

    #[test]
    fn a_window_that_comes_after_its_pane_closed_is_let_go() {
        let wm = Fake::default();
        let mut k = Keep::default();
        k.entries.push(entry(3, "catch-3"));
        k.release(&wm, 3);
        assert_eq!(k.dead.len(), 1);
        wm.wins.lock().unwrap().push(Win { id: "late".into(), pid: 99, space: "catch-3".into(), ..Default::default() });
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(wm.said.lock().unwrap().clone(), ["free late 1"]);
    }

    #[test]
    fn a_window_already_open_is_found_by_name_and_let_go_as_it_was() {
        let me = std::process::id();
        let wm = Fake::default();
        wm.wins.lock().unwrap().push(Win { id: "ours".into(), pid: me, space: "1".into(), rect: [0, 0, 800, 600], visible: true, ..Default::default() });
        wm.wins.lock().unwrap().push(Win { id: "old".into(), pid: 77, space: "2".into(), rect: [5, 5, 300, 200], name: "org.gnome.Calculator Calculator".into(), ..Default::default() });
        let mut k = Keep::default();
        k.entries.push(entry(4, "catch-4"));
        k.entries[0].since -= BY_NAME;
        k.entries[0].rel = Some([800, 600, 0, 30, 400, 300]);
        k.entries[0].shown = true;
        k.mind(&wm, &wm.windows().unwrap());
        assert_eq!(k.entries[0].id.as_deref(), Some("old"));
        assert!(!k.entries[0].new);
        k.release(&wm, 4);
        assert_eq!(wm.said.lock().unwrap().last().unwrap(), "free old 2", "put back on its own workspace");
    }
}
