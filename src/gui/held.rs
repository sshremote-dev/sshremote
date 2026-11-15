//! A program of this computer in a pane: on Windows its own window, held over the pane (sized
//! and moved with it, above this window and hidden with it), so it is used as it is, not through
//! a picture of it. Elsewhere a program here is shown as one of another computer is.

#[cfg(all(windows, feature = "remote"))]
pub(super) use imp::Held;
#[cfg(all(windows, feature = "remote"))]
pub(crate) use imp::bring_back;

/// Whether this computer's programs are held in their panes (else a pane shows a picture of them).
pub(super) const HERE: bool = cfg!(all(windows, feature = "remote"));

#[cfg(not(all(windows, feature = "remote")))]
pub(super) struct Held(std::convert::Infallible);

#[cfg(not(all(windows, feature = "remote")))]
impl Held {
    pub(super) fn start(_: u8, _: &str) -> Held {
        unreachable!("programs are held in panes only on Windows")
    }
    pub(super) fn show(&mut self, _: &mut eframe::egui::Ui, _: eframe::egui::Rect) {
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

    use eframe::egui::{self, Align2, FontId, Order, Rect};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GUI_INMOVESIZE, GUITHREADINFO, GW_HWNDPREV, GW_OWNER, GWLP_HWNDPARENT, GetClientRect, GetForegroundWindow, GetGUIThreadInfo, GetWindow, GetWindowRect,
        GetWindowThreadProcessId, IsIconic, SWP_NOMOVE, SWP_NOSIZE,
        IsWindow, IsWindowVisible, IsZoomed, PostMessageW, SW_HIDE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
        SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindowAsync, WM_CLOSE,
    };

    use crate::capture::winlist;

    /// The windows held now: shown again by `bring_back` if this program ends with them hidden.
    static HELD: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

    fn held(add: Option<isize>, remove: Option<isize>) {
        let mut list = HELD.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|h| Some(*h) != remove);
        list.extend(add);
    }

    /// A crash: the windows held out of sight are shown, not left hidden with nothing to bring them
    /// back.
    pub(crate) fn bring_back() {
        let list = HELD.lock().unwrap_or_else(|e| e.into_inner());
        for h in list.iter() {
            unsafe {
                SetWindowLongPtrW(*h as HWND, GWLP_HWNDPARENT, 0);
                ShowWindowAsync(*h as HWND, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// How often the program's window is looked for, and how long before it is given up.
    const LOOK: Duration = Duration::from_millis(300);
    const GIVE_UP: Duration = Duration::from_secs(20);

    pub(in crate::gui) struct Held {
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
    }

    impl Held {
        pub(in crate::gui) fn start(kind: u8, text: &str) -> Held {
            let before = winlist::shareable().into_iter().map(|w| w.id).collect();
            let (name, said) = match crate::apps::launch(kind, text) {
                Ok(n) => (n, None),
                Err(e) => (String::new(), Some(format!("It could not be started: {e}."))),
            };
            let what = if name.is_empty() { text.trim().to_string() } else { name.clone() };
            Held { what, name, before, since: Instant::now(), looked: Instant::now() - LOOK, win: None, said, ended: false }
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
            if self.win.is_none() && self.said.is_none() && self.looked.elapsed() >= LOOK {
                self.looked = Instant::now();
                match self.find() {
                    Some((hwnd, new)) => {
                        let mut was = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                        unsafe { GetWindowRect(hwnd as HWND, &mut was) };
                        self.win = Some(Win { hwnd, new, owner: 0, owned: false, was, shown: false, stuck: None });
                        held(Some(hwnd), None);
                    }
                    None if self.since.elapsed() > GIVE_UP => self.said = Some(format!("{} opened no window of its own to show here.", self.what)),
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
                Some(_) if covered(&ctx, rect) => w.give_way(),
                Some((to, owner)) if !minimized => w.put(to, owner),
                _ => w.hide(),
            }
            if w.stuck.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
                let h = w.hwnd as HWND;
                held(None, Some(w.hwnd));
                self.win = None;
                unsafe {
                    SetWindowLongPtrW(h, GWLP_HWNDPARENT, 0);
                    ShowWindowAsync(h, SW_SHOWNOACTIVATE);
                }
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
            let Some(w) = self.win.take() else { return };
            held(None, Some(w.hwnd));
            let h = w.hwnd as HWND;
            unsafe {
                if IsWindow(h) == 0 {
                    return;
                }
                SetWindowLongPtrW(h, GWLP_HWNDPARENT, 0);
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
                if (want.left, want.top) == (now.left, now.top) {
                    self.stuck = None;
                } else {
                    self.stuck.get_or_insert_with(Instant::now);
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

    /// Whether a menu, a dialog or a window of this one's is over the pane: the program is kept
    /// out of their way (a window held over this one would cover them).
    fn covered(ctx: &egui::Context, rect: Rect) -> bool {
        ctx.memory(|m| {
            m.areas()
                .visible_layer_ids()
                .into_iter()
                .filter(|l| matches!(l.order, Order::Middle | Order::Foreground))
                .any(|l| m.area_rect(l.id).is_some_and(|r| r.intersects(rect)))
        })
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
