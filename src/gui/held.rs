//! A program of this computer in a pane: its own window, held over the pane (sized and moved
//! with it, above this window and hidden with it), so it is used as it is, not through a picture of
//! it. On Windows, in X11 sessions, and under Wayland on GNOME (through its sshremote helper), KDE
//! Plasma (KWin scripts), Sway and Hyprland (their IPC); elsewhere (Wayland lets no program place
//! another's windows) a program here is shown as one of another computer is.

use eframe::egui::{self, Order, Rect};

#[cfg(all(feature = "remote", any(windows, target_os = "linux")))]
pub(super) use imp::Held;
#[cfg(all(feature = "remote", any(windows, target_os = "linux")))]
pub(crate) use imp::bring_back;

#[cfg(all(target_os = "linux", feature = "remote"))]
mod gnome;
#[cfg(all(target_os = "linux", feature = "remote"))]
mod hypr;
#[cfg(all(target_os = "linux", feature = "remote"))]
mod kwin;
#[cfg(all(target_os = "linux", feature = "remote"))]
mod sway;
#[cfg(all(target_os = "linux", feature = "remote"))]
mod tiled;
#[cfg(all(target_os = "linux", feature = "remote"))]
mod x11;

/// Whether this computer's programs are held in their panes (else a pane shows a picture of them).
pub(super) fn here() -> bool {
    #[cfg(all(windows, feature = "remote"))]
    return true;
    #[cfg(all(target_os = "linux", feature = "remote"))]
    return x11::here() || tiled::here() || gnome::here() || kwin::here();
    #[allow(unreachable_code)]
    false
}

/// On Linux the way the session allows: in X11 by asking the window manager; under Wayland on
/// GNOME through its sshremote helper, on KDE Plasma through KWin scripts, on Sway and Hyprland
/// through their IPC.
#[cfg(all(target_os = "linux", feature = "remote"))]
mod imp {
    use eframe::egui;

    pub(in crate::gui) enum Held {
        X11(super::x11::Held),
        Gnome(super::gnome::Held),
        Kwin(super::kwin::Held),
        Tiled(super::tiled::Held),
    }

    pub(crate) fn bring_back() {
        // GNOME's helper and KWin's scripts let go of the windows themselves when this window goes.
        super::x11::bring_back();
        super::tiled::bring_back();
    }

    impl Held {
        pub(in crate::gui) fn start(kind: u8, text: &str) -> Held {
            // The compositor this window is on first: another may answer on the session bus too
            // (one this session runs in a window of its own).
            if super::x11::here() {
                Held::X11(super::x11::Held::start(kind, text))
            } else if super::tiled::here() {
                Held::Tiled(super::tiled::Held::start(kind, text))
            } else if super::gnome::here() {
                Held::Gnome(super::gnome::Held::start(kind, text))
            } else {
                Held::Kwin(super::kwin::Held::start(kind, text))
            }
        }
        pub(in crate::gui) fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
            match self {
                Held::X11(h) => h.show(ui, rect),
                Held::Gnome(h) => h.show(ui, rect),
                Held::Kwin(h) => h.show(ui, rect),
                Held::Tiled(h) => h.show(ui, rect),
            }
        }
        pub(in crate::gui) fn hide(&mut self) {
            match self {
                Held::X11(h) => h.hide(),
                Held::Gnome(h) => h.hide(),
                Held::Kwin(h) => h.hide(),
                Held::Tiled(h) => h.hide(),
            }
        }
        pub(in crate::gui) fn close(&mut self) {
            match self {
                Held::X11(h) => h.close(),
                Held::Gnome(h) => h.close(),
                Held::Kwin(h) => h.close(),
                Held::Tiled(h) => h.close(),
            }
        }
        pub(in crate::gui) fn ended(&self) -> bool {
            match self {
                Held::X11(h) => h.ended(),
                Held::Gnome(h) => h.ended(),
                Held::Kwin(h) => h.ended(),
                Held::Tiled(h) => h.ended(),
            }
        }
    }
}

/// Whether a menu, a dialog or a window of this one's is over the pane: the program is kept out of
/// their way (a window held over this one would cover them).
#[allow(dead_code)]
fn covered(ctx: &egui::Context, rect: Rect) -> bool {
    ctx.memory(|m| {
        m.areas()
            .visible_layer_ids()
            .into_iter()
            .filter(|l| matches!(l.order, Order::Middle | Order::Foreground))
            .any(|l| m.area_rect(l.id).is_some_and(|r| r.intersects(rect)))
    })
}

#[cfg(not(all(feature = "remote", any(windows, target_os = "linux"))))]
pub(super) struct Held(std::convert::Infallible);

#[cfg(not(all(feature = "remote", any(windows, target_os = "linux"))))]
impl Held {
    pub(super) fn start(_: u8, _: &str) -> Held {
        unreachable!("programs are held in panes only on Windows and in X11 sessions")
    }
    pub(super) fn show(&mut self, _: &mut egui::Ui, _: Rect) {
        match self.0 {}
    }
    pub(super) fn hide(&mut self) {
        match self.0 {}
    }
    pub(super) fn close(&mut self) {
        match self.0 {}
    }
    pub(super) fn ended(&self) -> bool {
        match self.0 {}
    }
}

#[cfg(all(windows, feature = "remote"))]
mod imp {
    use std::time::{Duration, Instant};

    use eframe::egui::{self, Align2, FontId, Rect};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_TRANSITIONS_FORCEDISABLED, DwmGetWindowAttribute, DwmSetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GUI_INMOVESIZE, GUITHREADINFO, GW_HWNDPREV, GW_OWNER, GWLP_HWNDPARENT, GetClientRect, GetForegroundWindow, GetGUIThreadInfo, GetWindow, GetWindowRect,
        GetWindowThreadProcessId, IsIconic, SWP_NOMOVE, SWP_NOSIZE, GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, WS_EX_LAYERED,
        IsWindow, IsWindowVisible, IsZoomed, PostMessageW, SW_HIDE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
        SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindowAsync, WM_CLOSE,
    };

    use crate::capture::winlist;

    /// The windows held now: shown again by `bring_back` if this program ends with them hidden.
    static HELD: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

    fn held(add: Option<isize>, remove: Option<isize>) {
        let mut list = HELD.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|h| Some(*h) != remove);
        list.extend(add);
    }

    /// A crash: the windows held out of sight are shown, not left hidden (or see-through) with
    /// nothing to bring them back.
    pub(crate) fn bring_back() {
        let list = HELD.lock().unwrap_or_else(|e| e.into_inner());
        for h in list.iter() {
            let h = *h as HWND;
            unsafe {
                SetWindowLongPtrW(h, GWLP_HWNDPARENT, 0);
                SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA);
                ShowWindowAsync(h, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// Made see-through until it is on its pane: its style before, to put back (None: it is
    /// see-through of its own, and is left so). Its opening animation is left on: a window that
    /// shows before it takes the change fades in rather than shows at once.
    fn hush(h: HWND) -> Option<isize> {
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            if ex & WS_EX_LAYERED as isize != 0 {
                return None;
            }
            // Its program is sent the change and takes it when it next looks at its messages: a
            // thread of its own waits for that, not this window's.
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
            // Shown meanwhile where its program put it, before it took the change (a program
            // already running, as Chrome or Edge, opens a window and shows it at once): hidden now
            // and shown again on the pane it would blink, so it stays seen and just moves there.
            if IsWindowVisible(h) != 0 && IsIconic(h) == 0 {
                SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA);
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex);
                return None;
            }
            if SetLayeredWindowAttributes(h, 0, 0, LWA_ALPHA) == 0 {
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex);
                return None;
            }
            Some(ex)
        }
    }

    /// Seen again, as it was.
    fn unhush(h: HWND, ex: isize) {
        unsafe {
            SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex);
        }
    }

    /// Off this window's thread: the program may be slow to take the change.
    fn later(f: impl FnOnce() + Send + 'static) {
        let _ = std::thread::Builder::new().name("held-window".into()).spawn(f);
    }

    /// Windows opened for panes are caught as they are made, by a thread that hears of every new
    /// window (a WinEvent hook): made see-through and put on their pane before they first show,
    /// then shown once there. Looked for every so often instead, a window would show where its
    /// program puts it and then jump into the pane.
    mod catch {
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        use windows_sys::Win32::Foundation::{HWND, RECT};
        use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CHILDID_SELF, DispatchMessageW, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_SHOW, GA_PARENT, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GWLP_HWNDPARENT,
            GetAncestor, GetClassNameW, GetDesktopWindow, GetMessageW, GetWindow, GetWindowLongW, GetWindowRect, IsWindowVisible, MSG, OBJID_WINDOW, SWP_NOACTIVATE,
            SWP_NOOWNERZORDER, SWP_NOZORDER, SetTimer, SetWindowLongPtrW, SetWindowPos, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_TIMER, WS_CAPTION, WS_CHILD,
            WS_EX_TOOLWINDOW, WS_POPUP,
        };

        /// A window caught: its handle, and its style before it was made see-through.
        #[derive(Clone, Copy)]
        pub(super) struct Caught {
            pub hwnd: isize,
            pub style: Option<isize>,
            /// Being made see-through and put on its pane (by a thread of its own): not yet.
            pub ready: bool,
        }

        struct Wait {
            key: u64,
            before: Vec<isize>,
            to: Option<RECT>,
            owner: isize,
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

        /// Starts looking out for a new window (not one of `before`), before its program starts.
        pub(super) fn wait(before: Vec<isize>) -> u64 {
            let mut w = watch();
            let key = w.next;
            w.next += 1;
            w.waits.push(Wait { key, before, to: None, owner: 0, caught: None, since: Instant::now() });
            if !w.running {
                w.running = true;
                drop(w);
                let (tx, rx) = std::sync::mpsc::channel();
                if std::thread::Builder::new().name("held-catch".into()).spawn(move || run(tx)).is_err() {
                    watch().running = false;
                } else {
                    // Its hook is in place before the program starts.
                    let _ = rx.recv_timeout(Duration::from_millis(500));
                }
            }
            key
        }

        /// Where the window goes: the pane on the screen, and this program's window over which it is.
        pub(super) fn aim(key: u64, to: RECT, owner: isize) {
            if let Some(x) = watch().waits.iter_mut().find(|x| x.key == key) {
                x.to = Some(to);
                x.owner = owner;
            }
        }

        /// The window caught, once made ready and shown by its program (one never shown is a
        /// helper's, and another that shows takes its place); the wait ends with it.
        pub(super) fn take(key: u64) -> Option<Caught> {
            let mut w = watch();
            let i = w.waits.iter().position(|x| x.key == key && x.caught.is_some_and(|c| c.ready && unsafe { IsWindowVisible(c.hwnd as HWND) } != 0))?;
            w.waits.remove(i).caught
        }

        /// No longer looked out for; what was caught for it, if anything.
        pub(super) fn forget(key: u64) -> Option<Caught> {
            let mut w = watch();
            let i = w.waits.iter().position(|x| x.key == key)?;
            w.waits.remove(i).caught
        }

        fn run(ready: std::sync::mpsc::Sender<()>) {
            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            let hook = unsafe { SetWinEventHook(EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, std::ptr::null_mut(), Some(event), 0, 0, flags) };
            let _ = ready.send(());
            if hook.is_null() {
                watch().running = false;
                return;
            }
            unsafe { SetTimer(std::ptr::null_mut(), 0, 500, None) };
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                if msg.message == WM_TIMER {
                    let mut w = watch();
                    // A pane gone without saying so is not waited for long.
                    w.waits.retain(|x| x.since.elapsed() < Duration::from_secs(60));
                    if w.waits.is_empty() {
                        w.running = false;
                        break;
                    }
                }
                unsafe { DispatchMessageW(&msg) };
            }
            unsafe { UnhookWinEvent(hook) };
        }

        fn class(h: HWND) -> String {
            let mut buf = [0u16; 128];
            let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        }

        /// A program's window of its own: on the desktop, not a part of another (owned, a tool
        /// window), framed with a title bar or a Store app's frame, or a large pop-up once shown.
        fn main_window(h: HWND, shown: bool) -> bool {
            unsafe {
                if GetAncestor(h, GA_PARENT) != GetDesktopWindow() || !GetWindow(h, GW_OWNER).is_null() {
                    return false;
                }
                let style = GetWindowLongW(h, GWL_STYLE) as u32;
                let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
                if style & WS_CHILD != 0 || ex & WS_EX_TOOLWINDOW != 0 || (shown && IsWindowVisible(h) == 0) {
                    return false;
                }
                let class = class(h);
                // Crashpad's hidden window (Chrome, Edge, Electron apps) comes first from a launcher
                // process that hands over to the running browser and ends.
                if matches!(
                    class.as_str(),
                    "Windows.UI.Core.CoreWindow" | "IME" | "MSCTFIME UI" | "tooltips_class32" | "#32768" | "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "crashpad_SessionEndWatcher"
                ) {
                    return false;
                }
                let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetWindowRect(h, &mut r);
                let big = r.right - r.left >= 200 && r.bottom - r.top >= 120;
                style & WS_CAPTION == WS_CAPTION || class == "ApplicationFrameWindow" || (shown && style & WS_POPUP != 0 && big)
            }
        }

        unsafe extern "system" fn event(_: HWINEVENTHOOK, ev: u32, h: HWND, obj: i32, child: i32, _: u32, _: u32) {
            if obj != OBJID_WINDOW || child != CHILDID_SELF as i32 || h.is_null() {
                return;
            }
            let hw = h as isize;
            if ev == EVENT_OBJECT_DESTROY {
                for x in watch().waits.iter_mut() {
                    if x.caught.is_some_and(|c| c.hwnd == hw) {
                        x.caught = None;
                    }
                }
                return;
            }
            let shown = ev == EVENT_OBJECT_SHOW;
            if !main_window(h, shown) {
                return;
            }
            // The first wait with nothing caught, or with a window caught that is still hidden
            // while this one shows (a hidden window of its program's, made first; also while it is
            // being made ready, which waits on its program and may wait long for a helper's).
            let (key, to, owner, old) = {
                let mut w = watch();
                if w.waits.iter().any(|x| x.caught.is_some_and(|c| c.hwnd == hw)) {
                    return;
                }
                let Some(x) = w.waits.iter_mut().find(|x| {
                    !x.before.contains(&hw)
                        && match x.caught {
                            None => true,
                            Some(c) => shown && unsafe { IsWindowVisible(c.hwnd as HWND) } == 0,
                        }
                }) else {
                    return;
                };
                let old = x.caught.replace(Caught { hwnd: hw, style: None, ready: false });
                (x.key, x.to, x.owner, old)
            };
            let h = hw;
            super::later(move || {
                if let Some(o) = old {
                    super::let_go(o.hwnd as HWND, o.style);
                }
                let h = h as HWND;
                let style = super::hush(h);
                unsafe {
                    if owner != 0 {
                        SetWindowLongPtrW(h, GWLP_HWNDPARENT, owner);
                    }
                    if let Some(to) = to {
                        SetWindowPos(h, std::ptr::null_mut(), to.left, to.top, to.right - to.left, to.bottom - to.top, SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER);
                    }
                }
                let mut w = watch();
                match w.waits.iter_mut().find(|x| x.key == key && x.caught.is_some_and(|c| c.hwnd == hw)) {
                    Some(x) => x.caught = Some(Caught { hwnd: hw, style, ready: true }),
                    // Its pane went meanwhile, or a window that showed took its place.
                    None => {
                        drop(w);
                        super::let_go(h, style);
                    }
                }
            });
        }
    }

    /// A window caught for a pane and not kept: seen again, its own.
    fn let_go(h: HWND, style: Option<isize>) {
        unsafe {
            SetWindowLongPtrW(h, GWLP_HWNDPARENT, 0);
            let on: i32 = 0;
            DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED as u32, (&on as *const i32).cast(), 4);
        }
        if let Some(ex) = style {
            unhush(h, ex);
        }
    }

    /// How often the program's window is looked for, and how long before it is given up.
    const LOOK: Duration = Duration::from_millis(300);
    const GIVE_UP: Duration = Duration::from_secs(20);

    pub(in crate::gui) struct Held {
        /// The new window looked out for (`catch`), until one is held.
        key: Option<u64>,
        /// What was started, as the pane tells of it.
        what: String,
        /// Its program, to know a window it already had open by (one running brings it forward).
        name: String,
        before: Vec<u64>,
        since: Instant,
        looked: Instant,
        win: Option<Win>,
        /// Why there is no window here: it could not start, or opened none.
        said: Option<String>,
        /// Its window was closed: the pane goes too.
        ended: bool,
    }

    struct Win {
        hwnd: isize,
        /// Opened for the pane (closed with it), rather than one already open (let go).
        new: bool,
        owner: isize,
        /// The system took this window as its owner (a Store app's frame refuses): else it is put
        /// back over it whenever this window comes up over it.
        owned: bool,
        /// Where it was before, for when one already open is let go.
        was: RECT,
        shown: bool,
        /// Since when it is asked to move and stays put: a program running as administrator, which
        /// this one may not move.
        stuck: Option<Instant>,
        /// See-through until it is on the pane (its style before), and since when it is there.
        hushed: Option<isize>,
        placed: Option<Instant>,
        since: Instant,
    }

    impl Held {
        pub(in crate::gui) fn start(kind: u8, text: &str) -> Held {
            let before: Vec<u64> = winlist::shareable().into_iter().map(|w| w.id).collect();
            let key = catch::wait(before.iter().map(|id| *id as isize).collect());
            let (name, said) = match crate::apps::launch_minimized(kind, text) {
                Ok(n) => (n, None),
                Err(e) => (String::new(), Some(format!("It could not be started: {e}."))),
            };
            if said.is_some() {
                catch::forget(key);
            }
            let what = if name.is_empty() { text.trim().trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(text).to_string() } else { name.clone() };
            Held { key: said.is_none().then_some(key), what, name, before, since: Instant::now(), looked: Instant::now() - LOOK, win: None, said, ended: false }
        }

        fn hold(&mut self, hwnd: isize, new: bool, hushed: Option<isize>) {
            let mut was = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            // Hidden and shown with its tab without the system's animations.
            let off: i32 = 1;
            unsafe {
                GetWindowRect(hwnd as HWND, &mut was);
                DwmSetWindowAttribute(hwnd as HWND, DWMWA_TRANSITIONS_FORCEDISABLED as u32, (&off as *const i32).cast(), 4);
            }
            self.win = Some(Win { hwnd, new, owner: 0, owned: false, was, shown: false, stuck: None, hushed, placed: None, since: Instant::now() });
            held(Some(hwnd), None);
        }

        pub(in crate::gui) fn ended(&self) -> bool {
            self.ended
        }

        /// Its window: the first that is new and not this program's, or (after a few seconds) the
        /// topmost of the program started.
        fn find(&self) -> Option<(isize, bool)> {
            let list = winlist::shareable();
            let mine = std::process::id();
            let theirs = |id: u64| winlist::pid_of(id as HWND) != mine;
            // Not while the hook has one: it is shown when ready.
            if self.since.elapsed() < Duration::from_secs(2) && self.key.is_some() {
                return None;
            }
            if let Some(w) = list.iter().find(|w| !self.before.contains(&w.id) && theirs(w.id)) {
                return Some((w.id as isize, true));
            }
            let name = self.name.to_lowercase();
            if self.since.elapsed() < Duration::from_secs(3) || name.is_empty() {
                return None;
            }
            list.iter()
                .find(|w| {
                    let app = w.app.to_lowercase();
                    theirs(w.id) && !app.is_empty() && (app.contains(&name) || name.contains(&app))
                })
                .map(|w| (w.id as isize, false))
        }

        /// One frame of the pane at `rect`: the window put over it, or why there is none.
        pub(in crate::gui) fn show(&mut self, ui: &mut egui::Ui, rect: Rect) {
            let ctx = ui.ctx().clone();
            if let Some(key) = self.key {
                if let Some((to, owner)) = place(&ctx, rect) {
                    catch::aim(key, to, owner);
                }
                if let Some(c) = catch::take(key) {
                    self.key = None;
                    self.hold(c.hwnd, true, c.style);
                }
            }
            // Looked for as well, should the hook miss it (or it was open already).
            if self.win.is_none() && self.said.is_none() && self.looked.elapsed() >= LOOK {
                self.looked = Instant::now();
                match self.find() {
                    Some((hwnd, new)) => {
                        if let Some(c) = self.key.take().and_then(catch::forget) {
                            let_go(c.hwnd as HWND, c.style);
                        }
                        self.hold(hwnd, new, None);
                    }
                    None if self.since.elapsed() > GIVE_UP => {
                        if let Some(c) = self.key.take().and_then(catch::forget) {
                            let_go(c.hwnd as HWND, c.style);
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
            // Its state is followed (moved, closed) while nothing else wakes the window.
            ctx.request_repaint_after(if self.win.is_some() { Duration::from_millis(200) } else { LOOK });
            let Some(w) = self.win.as_mut() else { return };
            if unsafe { IsWindow(w.hwnd as HWND) } == 0 {
                held(None, Some(w.hwnd));
                self.win = None;
                self.ended = true;
                return;
            }
            let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
            match place(&ctx, rect) {
                Some(_) if super::covered(&ctx, rect) => w.give_way(),
                Some((to, owner)) if !minimized => w.put(to, owner),
                _ => w.hide(),
            }
            if w.stuck.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
                let (h, style) = (w.hwnd, w.hushed.take());
                held(None, Some(w.hwnd));
                self.win = None;
                later(move || let_go(h as HWND, style));
                unsafe { ShowWindowAsync(h as HWND, SW_SHOWNOACTIVATE) };
                self.said = Some(format!("{} runs with more rights than this app, so it stays a window of its own.", self.what));
            }
        }

        /// Not drawn this frame (another tab, the home): out of sight.
        pub(in crate::gui) fn hide(&mut self) {
            if let Some(w) = self.win.as_mut() {
                w.hide();
            }
        }

        /// The pane closes: its program with it (it may ask to save first), or a window it already
        /// had open back where it was.
        pub(in crate::gui) fn close(&mut self) {
            // Caught and not yet held: it was opened for the pane, so it closes with it.
            if let Some(c) = self.key.take().and_then(catch::forget) {
                let h = c.hwnd;
                later(move || {
                    let_go(h as HWND, c.style);
                    unsafe { PostMessageW(h as HWND, WM_CLOSE, 0, 0) };
                });
            }
            let Some(w) = self.win.take() else { return };
            held(None, Some(w.hwnd));
            let h = w.hwnd as HWND;
            unsafe {
                if IsWindow(h) == 0 {
                    return;
                }
                let (hw, style) = (w.hwnd, w.hushed);
                later(move || let_go(hw as HWND, style));
                if w.new {
                    ShowWindowAsync(h, SW_SHOWNOACTIVATE);
                    PostMessageW(h, WM_CLOSE, 0, 0);
                } else {
                    let r = w.was;
                    SetWindowPos(h, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | SWP_NOZORDER | SWP_ASYNCWINDOWPOS);
                    ShowWindowAsync(h, SW_SHOWNOACTIVATE);
                }
            }
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            self.close();
        }
    }

    impl Win {
        /// Over the pane: owned by this window (above it, hidden when it is minimized), its
        /// visible bounds on the pane's.
        fn put(&mut self, to: RECT, owner: isize) {
            let h = self.hwnd as HWND;
            unsafe {
                // Again when let go meanwhile (`bring_back`).
                if self.owner != owner || (self.owned && GetWindow(h, GW_OWNER) as isize != owner) {
                    SetWindowLongPtrW(h, GWLP_HWNDPARENT, owner);
                    self.owner = owner;
                    self.owned = GetWindow(h, GW_OWNER) as isize == owner;
                }
                if !self.owned && GetForegroundWindow() != h && GetWindow(owner as HWND, GW_HWNDPREV) != h {
                    // Just over this window, under what is over it (not over the others, unless it is
                    // the one in use).
                    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
                    SetWindowPos(h, GetWindow(owner as HWND, GW_HWNDPREV), 0, 0, 0, 0, flags);
                }
                if !self.shown || IsIconic(h) != 0 || IsZoomed(h) != 0 || IsWindowVisible(h) == 0 {
                    ShowWindowAsync(h, SW_SHOWNOACTIVATE);
                    self.shown = true;
                }
                // Dragged or sized by hand: put back once let go.
                if moving(h) {
                    self.stuck = None;
                    return;
                }
                let mut now = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetWindowRect(h, &mut now);
                // Its window is larger than what shows by its invisible borders (Windows 10 and on).
                let mut seen = now;
                DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS as u32, (&mut seen as *mut RECT).cast(), size_of::<RECT>() as u32);
                let want = RECT {
                    left: to.left - (seen.left - now.left),
                    top: to.top - (seen.top - now.top),
                    right: to.right + (now.right - seen.right),
                    bottom: to.bottom + (now.bottom - seen.bottom),
                };
                if (want.left, want.top, want.right, want.bottom) != (now.left, now.top, now.right, now.bottom) {
                    // Asynchronous: a program that hangs does not hang this window.
                    let flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
                    SetWindowPos(h, std::ptr::null_mut(), want.left, want.top, want.right - want.left, want.bottom - want.top, flags);
                }
                // Its size may stop at its smallest; where it is may not.
                let there = (want.left, want.top) == (now.left, now.top);
                if there {
                    self.stuck = None;
                } else {
                    self.stuck.get_or_insert_with(Instant::now);
                }
                // Seen once it is on the pane and has drawn itself there (or after a while anyway).
                if self.hushed.is_some() {
                    let mut cloaked = 0u32;
                    DwmGetWindowAttribute(h, DWMWA_CLOAKED as u32, (&mut cloaked as *mut u32).cast(), 4);
                    let ready = there && IsWindowVisible(h) != 0 && cloaked == 0;
                    let placed = if ready { *self.placed.get_or_insert_with(Instant::now) } else { Instant::now() };
                    if (ready && placed.elapsed() >= Duration::from_millis(80)) || self.since.elapsed() > Duration::from_secs(3) {
                        let (hw, ex) = (self.hwnd, self.hushed.take().unwrap_or(0));
                        later(move || unhush(hw as HWND, ex));
                        // Started minimized for the pane: it takes the keyboard, as one just opened does.
                        if self.new {
                            SetForegroundWindow(h);
                        }
                    }
                }
            }
        }

        /// Out of sight: hidden, or minimized when it takes no owner (a Store app's frame, which
        /// hiding leaves on the desktop).
        fn hide(&mut self) {
            self.stuck = None;
            let h = self.hwnd as HWND;
            if !self.owned && self.owner != 0 {
                if unsafe { IsIconic(h) } == 0 {
                    unsafe { ShowWindowAsync(h, SW_SHOWMINNOACTIVE) };
                }
                self.shown = false;
            } else if self.shown || unsafe { IsWindowVisible(h) } != 0 {
                unsafe { ShowWindowAsync(h, SW_HIDE) };
                self.shown = false;
            }
        }

        /// Under a menu or a dialog of this window's: one above this window is hidden meanwhile,
        /// one that takes no owner is left under this window, which came up over it with the click.
        fn give_way(&mut self) {
            if self.owned || self.owner == 0 {
                self.hide();
            }
        }
    }

    /// Whether the window's thread is in a move or size by the mouse.
    fn moving(h: HWND) -> bool {
        let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        let thread = unsafe { GetWindowThreadProcessId(h, std::ptr::null_mut()) };
        let got = unsafe { GetGUIThreadInfo(thread, &mut info) };
        got != 0 && info.flags & GUI_INMOVESIZE != 0
    }

    /// The pane on the screen, in pixels, and the window of this program it is in.
    fn place(ctx: &egui::Context, rect: Rect) -> Option<(RECT, isize)> {
        let inner = ctx.input(|i| i.viewport().inner_rect)?;
        let ppp = ctx.pixels_per_point();
        let px = |v: f32| (v * ppp).round() as i32;
        let (o, r) = (inner.min.to_vec2(), rect);
        let to = RECT { left: px(r.min.x + o.x), top: px(r.min.y + o.y), right: px(r.max.x + o.x), bottom: px(r.max.y + o.y) };
        let client = RECT { left: px(inner.min.x), top: px(inner.min.y), right: px(inner.max.x), bottom: px(inner.max.y) };
        Some((to, own_window(client)?))
    }

    /// This program's window whose inside is at `client` on the screen.
    fn own_window(client: RECT) -> Option<isize> {
        struct Look {
            pid: u32,
            client: RECT,
            found: isize,
        }
        unsafe extern "system" fn each(h: HWND, l: LPARAM) -> windows_sys::core::BOOL {
            let look = unsafe { &mut *(l as *mut Look) };
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(h, &mut pid) };
            if pid != look.pid || unsafe { IsWindowVisible(h) } == 0 {
                return 1;
            }
            let mut c = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let mut at = POINT { x: 0, y: 0 };
            unsafe {
                GetClientRect(h, &mut c);
                ClientToScreen(h, &mut at);
            }
            let near = |a: i32, b: i32| (a - b).abs() <= 2;
            let w = look.client;
            if near(at.x, w.left) && near(at.y, w.top) && near(c.right, w.right - w.left) && near(c.bottom, w.bottom - w.top) {
                look.found = h as isize;
                return 0;
            }
            1
        }
        let mut look = Look { pid: std::process::id(), client, found: 0 };
        unsafe { EnumWindows(Some(each), &mut look as *mut Look as LPARAM) };
        (look.found != 0).then_some(look.found)
    }
}
