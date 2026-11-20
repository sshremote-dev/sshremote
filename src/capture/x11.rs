//! X11 desktops (Xfce, MATE, LXQt, Cinnamon, i3, KDE and GNOME on Xorg): the screen read with
//! MIT-SHM only when XDamage says something changed, the pointer drawn in from XFixes, and input
//! given through XTest, as X11 lets any program of the session do, with no dialog. One window or
//! one program is shown by reading only where its windows are (brought to the front).

use std::os::raw::c_void;

use x11rb::connection::Connection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use super::{Capture, Image, Input, Monitor, Share, WindowInfo};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

/// A shared memory segment the X server writes the screen into.
struct Shm {
    seg: u32,
    addr: *mut c_void,
    size: usize,
}

impl Drop for Shm {
    fn drop(&mut self) {
        unsafe { libc::shmdt(self.addr) };
    }
}

pub struct X11 {
    conn: RustConnection,
    root: xproto::Window,
    shm: Option<Shm>,
    damage: Option<damage::Damage>,
    /// The monitors (their place on the X screen) and the one shown.
    screens: Vec<(Monitor, i16, i16)>,
    current: usize,
    target: (usize, usize),
    /// The picture made smaller for the viewer, by area averaging.
    shrink: super::Shrink,
    /// Something changed since the last picture: the screen, or the pointer (which draws on no
    /// window, so it makes no damage).
    dirty: bool,
    pointer: (i16, i16, u32),
    /// The viewer draws the pointer: the pictures leave it out, its shape goes as it changes
    /// (the serial of the one sent last).
    apart: bool,
    sent_shape: u32,
    /// The last picture as read, without the pointer, for a pointer that moved alone.
    clean: Option<Vec<u8>>,
    keys: Keys,
    /// What is shown: the monitor, a window, or a program's windows; and where they are now.
    scope: Scope,
    shown: Option<(i16, i16, usize, usize)>,
    looked: std::time::Instant,
    root_size: (usize, usize),
    atoms: Atoms,
    /// A connection of its own waking the agent as the screen or the pointer's picture changes.
    watch: Option<Watch>,
}

/// Ends the watching thread as the capture goes (within a quarter second); `alive` until it ends.
struct Watch {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Watch {
    /// Waits on a connection of its own for damage on the screen and a new pointer picture,
    /// calling `wake` for each (at most every 2 ms), so the agent need not look every millisecond.
    fn start(wake: Box<dyn Fn() + Send>) -> Result<Watch, String> {
        use std::os::fd::AsRawFd;
        use x11rb::protocol::xfixes;
        let (conn, screen) = x11rb::connect(None).map_err(err)?;
        let root = conn.setup().roots[screen].root;
        conn.damage_query_version(1, 1).map_err(err)?.reply().map_err(err)?;
        conn.xfixes_query_version(4, 0).map_err(err)?.reply().map_err(err)?;
        let d = conn.generate_id().map_err(err)?;
        conn.damage_create(d, root, damage::ReportLevel::NON_EMPTY).map_err(err)?;
        conn.xfixes_select_cursor_input(root, xfixes::CursorNotifyMask::DISPLAY_CURSOR).map_err(err)?;
        conn.flush().map_err(err)?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (ended, living) = (stop.clone(), alive.clone());
        std::thread::Builder::new()
            .name("x11-watch".into())
            .spawn(move || {
                // Gone for good (the connection broke): the agent looks for itself again.
                struct Gone(std::sync::Arc<std::sync::atomic::AtomicBool>);
                impl Drop for Gone {
                    fn drop(&mut self) {
                        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                let _gone = Gone(living);
                let fd = conn.stream().as_raw_fd();
                while !ended.load(std::sync::atomic::Ordering::Relaxed) {
                    let mut came = false;
                    loop {
                        match conn.poll_for_event() {
                            Ok(Some(e)) => {
                                // Told again only once what changed is taken off.
                                if matches!(e, Event::DamageNotify(_)) {
                                    let _ = conn.damage_subtract(d, x11rb::NONE, x11rb::NONE);
                                }
                                came = true;
                            }
                            Ok(None) => break,
                            Err(_) => return,
                        }
                    }
                    if came {
                        if conn.flush().is_err() {
                            return;
                        }
                        wake();
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                    let r = unsafe { libc::poll(&mut p, 1, 250) };
                    if r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    if r < 0 || p.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                        return;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Watch { stop, alive })
    }
}

#[derive(Clone, PartialEq)]
enum Scope {
    Screen,
    Window(u32),
    /// Every window of the program by its class (WM_CLASS).
    App(String),
}

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_CLIENT_LIST_STACKING,
        _NET_ACTIVE_WINDOW,
        _NET_WM_NAME,
        _NET_FRAME_EXTENTS,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_HIDDEN,
        UTF8_STRING,
    }
}

/// A window of the desktop's own (not a panel or the desktop), as the window manager lists it.
struct Client {
    id: u32,
    title: String,
    class: String,
    /// Its frame, title bar included, on the X screen.
    frame: (i16, i16, usize, usize),
}

// SAFETY: the segment is this capture's own, used from the thread that holds it.
unsafe impl Send for X11 {}

/// The keyboard as X maps it: keysyms by keycode and level, and keycodes free to map a
/// character no key makes (as xdotool does), with what they were last mapped to.
struct Keys {
    min: u8,
    per: usize,
    syms: Vec<u32>,
    spare: Vec<(u8, u32)>,
}

impl Keys {
    fn read(conn: &RustConnection) -> Result<Keys, String> {
        let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
        let m = conn.get_keyboard_mapping(min, max - min + 1).map_err(err)?.reply().map_err(err)?;
        let per = m.keysyms_per_keycode as usize;
        let syms = m.keysyms;
        let spare = (0..=(max - min) as usize).filter(|i| syms[i * per..(i + 1) * per].iter().all(|s| *s == 0)).rev().take(4).map(|i| (min + i as u8, 0)).collect();
        Ok(Keys { min, per, syms, spare })
    }

    /// The keycode for `keysym` and whether Shift makes it there.
    fn find(&self, keysym: u32) -> Option<(u8, bool)> {
        for level in [0, 1] {
            if let Some(i) = (0..self.syms.len() / self.per).find(|i| self.syms[i * self.per + level] == keysym) {
                return Some((self.min + i as u8, level == 1));
            }
        }
        None
    }
}

fn err(e: impl std::fmt::Display) -> String {
    format!("X11: {e}")
}

impl X11 {
    pub fn open() -> Result<X11, String> {
        super::gnome::ensure_env();
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display ({e})"))?;
        let s = &conn.setup().roots[screen];
        let root = s.root;
        if s.root_depth < 24 {
            return Err(format!("the X screen has {} bit colour; 24 or 32 is needed", s.root_depth));
        }
        let (w, h) = (s.width_in_pixels as usize, s.height_in_pixels as usize);
        let keys = Keys::read(&conn)?;
        let atoms = Atoms::new(&conn).map_err(err)?.reply().map_err(err)?;
        if conn.xtest_get_version(2, 2).ok().and_then(|c| c.reply().ok()).is_none() {
            return Err("the X server has no XTest, so it cannot be controlled".into());
        }
        let mut x = X11 {
            conn,
            root,
            shm: None,
            damage: None,
            screens: Vec::new(),
            current: 0,
            target: (w, h),
            shrink: super::Shrink::default(),
            dirty: true,
            pointer: (0, 0, 0),
            apart: false,
            sent_shape: 0,
            clean: None,
            keys,
            scope: Scope::Screen,
            shown: None,
            looked: std::time::Instant::now(),
            root_size: (w, h),
            atoms,
            watch: None,
        };
        x.screens = x.read_monitors().unwrap_or_default();
        if x.screens.is_empty() {
            x.screens.push((Monitor { name: "screen".into(), w, h, primary: true }, 0, 0));
        }
        x.current = x.screens.iter().position(|(m, ..)| m.primary).unwrap_or(0);
        x.target = (x.screens[x.current].0.w, x.screens[x.current].0.h);
        // What changes on the screen is told; without the extensions every look reads it all.
        if x.conn.damage_query_version(1, 1).ok().and_then(|c| c.reply().ok()).is_some() {
            let d = x.conn.generate_id().map_err(err)?;
            if x.conn.damage_create(d, root, damage::ReportLevel::NON_EMPTY).is_ok() {
                x.damage = Some(d);
            }
        }
        let _ = x.conn.xfixes_query_version(4, 0).ok().and_then(|c| c.reply().ok());
        x.shm = x.attach_shm(w * h * 4).ok();
        x.conn.flush().map_err(err)?;
        Ok(x)
    }

    fn read_monitors(&self) -> Result<Vec<(Monitor, i16, i16)>, String> {
        let r = self.conn.randr_get_monitors(self.root, true).map_err(err)?.reply().map_err(err)?;
        Ok(r.monitors
            .iter()
            .map(|m| {
                let name = self.conn.get_atom_name(m.name).ok().and_then(|c| c.reply().ok()).map_or_else(|| "screen".to_string(), |n| String::from_utf8_lossy(&n.name).into_owned());
                (Monitor { name, w: m.width as usize, h: m.height as usize, primary: m.primary }, m.x, m.y)
            })
            .collect())
    }

    /// A segment as large as the whole X screen, shared with the server (a local one).
    fn attach_shm(&self, size: usize) -> Result<Shm, String> {
        if self.conn.shm_query_version().ok().and_then(|c| c.reply().ok()).is_none() {
            return Err("no MIT-SHM".into());
        }
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600) };
        if id < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let addr = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        // Gone with the last detach, whatever happens to this program.
        unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
        if addr as isize == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let seg = self.conn.generate_id().map_err(err)?;
        let shm = Shm { seg, addr, size };
        self.conn.shm_attach(seg, id as u32, false).map_err(err)?;
        // A server elsewhere (over the network) cannot attach: told by the first read.
        self.conn.sync().map_err(err)?;
        Ok(shm)
    }

    fn area(&self) -> (i16, i16, usize, usize) {
        if let Some(r) = self.shown {
            return r;
        }
        let (m, x, y) = &self.screens[self.current];
        (*x, *y, m.w, m.h)
    }

    fn property(&self, win: u32, prop: u32, kind: u32) -> Option<xproto::GetPropertyReply> {
        self.conn.get_property(false, win, prop, kind, 0, 1 << 16).ok()?.reply().ok()
    }

    /// The desktop's windows, the topmost first: shown ones only, panels and the desktop left out.
    fn clients(&self) -> Vec<Client> {
        let a = &self.atoms;
        let Some(list) = self.property(self.root, a._NET_CLIENT_LIST_STACKING, xproto::AtomEnum::WINDOW.into()) else { return Vec::new() };
        let ids: Vec<u32> = list.value32().map(|v| v.collect()).unwrap_or_default();
        let mut out = Vec::new();
        for &id in ids.iter().rev() {
            let viewable = self.conn.get_window_attributes(id).ok().and_then(|c| c.reply().ok()).is_some_and(|w| w.map_state == xproto::MapState::VIEWABLE);
            if !viewable {
                continue;
            }
            let atoms_of = |prop| self.property(id, prop, xproto::AtomEnum::ATOM.into()).and_then(|p| p.value32().map(|v| v.collect::<Vec<u32>>())).unwrap_or_default();
            let types = atoms_of(a._NET_WM_WINDOW_TYPE);
            if types.contains(&a._NET_WM_WINDOW_TYPE_DOCK) || types.contains(&a._NET_WM_WINDOW_TYPE_DESKTOP) || atoms_of(a._NET_WM_STATE).contains(&a._NET_WM_STATE_HIDDEN) {
                continue;
            }
            let title = self
                .property(id, a._NET_WM_NAME, a.UTF8_STRING)
                .filter(|p| !p.value.is_empty())
                .or_else(|| self.property(id, xproto::AtomEnum::WM_NAME.into(), xproto::AtomEnum::ANY.into()))
                .map(|p| String::from_utf8_lossy(&p.value).into_owned())
                .unwrap_or_default();
            // "instance\0Class\0": the class names the program.
            let class = self
                .property(id, xproto::AtomEnum::WM_CLASS.into(), xproto::AtomEnum::STRING.into())
                .map(|p| p.value.split(|b| *b == 0).rfind(|x| !x.is_empty()).map(|c| String::from_utf8_lossy(c).into_owned()).unwrap_or_default())
                .unwrap_or_default();
            let Some(g) = self.conn.get_geometry(id).ok().and_then(|c| c.reply().ok()) else { continue };
            let Some(t) = self.conn.translate_coordinates(id, self.root, 0, 0).ok().and_then(|c| c.reply().ok()) else { continue };
            let ext: Vec<u32> = self.property(id, a._NET_FRAME_EXTENTS, xproto::AtomEnum::CARDINAL.into()).and_then(|p| p.value32().map(|v| v.collect())).unwrap_or_default();
            let (l, r, top, b) = if ext.len() == 4 { (ext[0] as i32, ext[1] as i32, ext[2] as i32, ext[3] as i32) } else { (0, 0, 0, 0) };
            let frame = (t.dst_x as i32 - l, t.dst_y as i32 - top, g.width as i32 + l + r, g.height as i32 + top + b);
            out.push(Client { id, title, class, frame: self.clamp(frame) });
        }
        out
    }

    /// A rectangle within the X screen, at least a pixel each way.
    fn clamp(&self, (x, y, w, h): (i32, i32, i32, i32)) -> (i16, i16, usize, usize) {
        let (rw, rh) = (self.root_size.0 as i32, self.root_size.1 as i32);
        let (x0, y0) = (x.clamp(0, rw - 1), y.clamp(0, rh - 1));
        let (x1, y1) = ((x + w).clamp(x0 + 1, rw), (y + h).clamp(y0 + 1, rh));
        (x0 as i16, y0 as i16, (x1 - x0) as usize, (y1 - y0) as usize)
    }

    /// Where the shown window or program is now; None when it is gone.
    fn place(&self) -> Option<(i16, i16, usize, usize)> {
        let clients = self.clients();
        let frames: Vec<_> = match &self.scope {
            Scope::Screen => return None,
            Scope::Window(w) => clients.iter().filter(|c| c.id == *w).map(|c| c.frame).collect(),
            Scope::App(class) => clients.iter().filter(|c| &c.class == class).map(|c| c.frame).collect(),
        };
        let x0 = frames.iter().map(|f| f.0 as i32).min()?;
        let y0 = frames.iter().map(|f| f.1 as i32).min()?;
        let x1 = frames.iter().map(|f| f.0 as i32 + f.2 as i32).max()?;
        let y1 = frames.iter().map(|f| f.1 as i32 + f.3 as i32).max()?;
        Some(self.clamp((x0, y0, x1 - x0, y1 - y0)))
    }

    /// Follows the shown window or program as it moves; the whole monitor again once it is gone.
    fn follow(&mut self) {
        if self.scope == Scope::Screen || self.looked.elapsed() < std::time::Duration::from_millis(250) {
            return;
        }
        self.looked = std::time::Instant::now();
        let now = self.place();
        if now.is_none() {
            self.scope = Scope::Screen;
        }
        if now != self.shown {
            self.shown = now;
            (self.dirty, self.clean) = (true, None);
        }
    }

    /// Brings a window to the front, as a taskbar does.
    fn raise(&self, id: u32) {
        let ev = xproto::ClientMessageEvent::new(32, id, self.atoms._NET_ACTIVE_WINDOW, [2u32, 0, 0, 0, 0]);
        let mask = xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY;
        let _ = self.conn.send_event(false, self.root, mask, ev);
        let _ = self.conn.flush();
    }

    /// The shown monitor's pixels (BGRx rows).
    fn read(&mut self) -> Result<Vec<u8>, String> {
        let (x, y, w, h) = self.area();
        if let Some(shm) = &self.shm {
            let got = self.conn.shm_get_image(self.root, x, y, w as u16, h as u16, !0, ImageFormat::Z_PIXMAP.into(), shm.seg, 0).map_err(err).and_then(|c| c.reply().map_err(err));
            if got.is_ok() && w * h * 4 <= shm.size {
                // SAFETY: the server wrote w*h*4 bytes into the segment, which is that large.
                return Ok(unsafe { std::slice::from_raw_parts(shm.addr as *const u8, w * h * 4) }.to_vec());
            }
            self.shm = None;
        }
        let r = self.conn.get_image(ImageFormat::Z_PIXMAP, self.root, x, y, w as u16, h as u16, !0).map_err(err)?.reply().map_err(err)?;
        if r.data.len() < w * h * 4 {
            return Err("the X server sends pixels of another size".into());
        }
        Ok(r.data)
    }

    /// The pointer, over `px` (the shown monitor's pixels): its picture where it is.
    fn draw_pointer(&self, px: &mut [u8]) {
        let Some(c) = self.conn.xfixes_get_cursor_image().ok().and_then(|c| c.reply().ok()) else { return };
        let (ax, ay, w, h) = self.area();
        let (cx, cy) = (c.x as i32 - c.xhot as i32 - ax as i32, c.y as i32 - c.yhot as i32 - ay as i32);
        for row in 0..c.height as i32 {
            for col in 0..c.width as i32 {
                let (x, y) = (cx + col, cy + row);
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    continue;
                }
                let argb = c.cursor_image[(row * c.width as i32 + col) as usize];
                let a = argb >> 24;
                if a == 0 {
                    continue;
                }
                let o = (y as usize * w + x as usize) * 4;
                // Premultiplied: source plus what shows through.
                for (i, shift) in [(0, 0), (1, 8), (2, 16)] {
                    let s = (argb >> shift) & 0xff;
                    px[o + i] = (s + px[o + i] as u32 * (255 - a) / 255).min(255) as u8;
                }
            }
        }
    }

    fn keycode(&mut self, keysym: u32) -> Result<(u8, bool), String> {
        if let Some(k) = self.keys.find(keysym) {
            return Ok(k);
        }
        // A character no key makes (another layout's, Korean...): a free keycode is made to.
        if let Some(&(code, _)) = self.keys.spare.iter().find(|(_, s)| *s == keysym) {
            return Ok((code, false));
        }
        let Some((code, _)) = self.keys.spare.pop() else { return Err("no free key to type that with".into()) };
        let mut syms = vec![0u32; self.keys.per];
        syms[0] = keysym;
        if self.keys.per > 1 {
            syms[1] = keysym;
        }
        self.conn.change_keyboard_mapping(1, code, self.keys.per as u8, &syms).map_err(err)?;
        self.conn.sync().map_err(err)?;
        self.keys.spare.insert(0, (code, keysym));
        Ok((code, false))
    }

    fn fake(&self, kind: u8, detail: u8) -> Result<(), String> {
        self.conn.xtest_fake_input(kind, detail, 0, self.root, 0, 0, 0).map_err(err)?;
        Ok(())
    }

    fn key(&mut self, keysym: u32, down: bool) -> Result<(), String> {
        let (code, _) = self.keycode(keysym)?;
        self.fake(if down { xproto::KEY_PRESS_EVENT } else { xproto::KEY_RELEASE_EVENT }, code)
    }
}

impl Capture for X11 {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn monitors(&self) -> Vec<Monitor> {
        self.screens.iter().map(|(m, ..)| m.clone()).collect()
    }

    fn current(&self) -> usize {
        self.current
    }

    fn screen_id(&self) -> Option<String> {
        self.screens.get(self.current).map(|(m, ..)| m.name.clone())
    }

    fn screen_changed(&mut self) {
        // The X screen's new size and its monitors' new places, the same monitor found by name,
        // and memory shared with the server as large as the screen is now.
        let name = self.screens.get(self.current).map(|(m, ..)| m.name.clone());
        if let Some(g) = self.conn.get_geometry(self.root).ok().and_then(|c| c.reply().ok()) {
            self.root_size = (g.width as usize, g.height as usize);
        }
        if let Ok(list) = self.read_monitors()
            && !list.is_empty()
        {
            self.screens = list;
        }
        self.current = name.and_then(|n| self.screens.iter().position(|(m, ..)| m.name == n)).unwrap_or(0);
        let need = self.root_size.0 * self.root_size.1 * 4;
        if self.shm.as_ref().is_some_and(|s| s.size < need) {
            if let Some(old) = self.shm.take() {
                let _ = self.conn.shm_detach(old.seg);
            }
            self.shm = self.attach_shm(need).ok();
        }
        (self.dirty, self.clean) = (true, None);
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.screens.len() {
            return Err("no such monitor".into());
        }
        self.current = index;
        (self.scope, self.shown) = (Scope::Screen, None);
        (self.dirty, self.clean) = (true, None);
        Ok(())
    }

    fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        Ok(self.clients().into_iter().map(|c| WindowInfo { id: c.id as u64, title: c.title, app: c.class, w: c.frame.2 as u16, h: c.frame.3 as u16 }).collect())
    }

    fn share(&mut self, scope: Share) -> Result<String, String> {
        let active = || self.property(self.root, self.atoms._NET_ACTIVE_WINDOW, xproto::AtomEnum::WINDOW.into()).and_then(|p| p.value32().and_then(|mut v| v.next())).filter(|w| *w != 0);
        let (scope, id) = match scope {
            Share::Screen => {
                (self.scope, self.shown) = (Scope::Screen, None);
                (self.dirty, self.clean) = (true, None);
                return Ok("the whole screen".into());
            }
            Share::Window(w) => (true, w as u32),
            Share::App(w) => (false, w as u32),
            Share::ActiveWindow => (true, active().ok_or("no window has the focus")?),
            Share::ActiveApp => (false, active().ok_or("no window has the focus")?),
        };
        let clients = self.clients();
        let c = clients.iter().find(|c| c.id == id).ok_or("that window is gone")?;
        let text = if scope || c.class.is_empty() { c.title.clone() } else { c.class.clone() };
        self.scope = if scope || c.class.is_empty() { Scope::Window(id) } else { Scope::App(c.class.clone()) };
        self.raise(id);
        self.shown = self.place();
        (self.dirty, self.clean) = (true, None);
        Ok(text)
    }

    fn shared_window(&self) -> Option<u64> {
        match self.scope {
            Scope::Window(w) => Some(w as u64),
            _ => None,
        }
    }

    fn shared_app(&self) -> Option<String> {
        match &self.scope {
            Scope::App(c) => Some(c.clone()),
            _ => None,
        }
    }

    fn desktop_size(&self) -> (usize, usize) {
        let (_, _, w, h) = self.area();
        (w, h)
    }

    fn origin(&self) -> (i32, i32) {
        let (x, y, _, _) = self.area();
        (x as i32, y as i32)
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.max(1), h.max(1));
        self.dirty = true;
        Ok(())
    }

    fn pointer_apart(&mut self, on: bool) -> bool {
        (self.apart, self.sent_shape, self.dirty) = (on, 0, true);
        true
    }

    fn set_waker(&mut self, wake: Box<dyn Fn() + Send>) -> bool {
        // Without XDamage every look reads the whole screen: the agent looks at its own pace.
        if self.damage.is_none() {
            return false;
        }
        self.watch = Watch::start(wake).ok();
        self.watch.is_some()
    }

    fn wakes(&self) -> bool {
        self.watch.as_ref().is_some_and(|w| w.alive.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn pointer(&mut self) -> (Option<super::Pointer>, Option<(i32, i32)>) {
        let Some(c) = self.conn.xfixes_get_cursor_image().ok().and_then(|c| c.reply().ok()) else { return (None, None) };
        let (ax, ay, _, _) = self.area();
        let shape = (c.cursor_serial != self.sent_shape).then(|| {
            self.sent_shape = c.cursor_serial;
            // XFixes gives premultiplied ARGB.
            let rgba = c
                .cursor_image
                .iter()
                .flat_map(|&argb| {
                    let a = argb >> 24;
                    let un = |v: u32| ((v & 0xff) * 255).checked_div(a).map_or(0, |q| q.min(255) as u8);
                    [un(argb >> 16), un(argb >> 8), un(argb), a as u8]
                })
                .collect();
            super::Pointer { w: c.width as usize, h: c.height as usize, hot: (c.xhot as usize, c.yhot as usize), rgba }
        });
        (shape, Some((c.x as i32 - ax as i32, c.y as i32 - ay as i32)))
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        self.follow();
        // What the X server said since the last look: damage, or nothing.
        let mut damaged = self.damage.is_none();
        while let Some(e) = self.conn.poll_for_event().map_err(err)? {
            damaged |= matches!(e, Event::DamageNotify(_));
        }
        if damaged && let Some(d) = self.damage {
            self.conn.damage_subtract(d, x11rb::NONE, x11rb::NONE).map_err(err)?;
        }
        // A pointer drawn by the viewer moves without a picture.
        let moved = !self.apart && {
            let p = self.conn.query_pointer(self.root).map_err(err)?.reply().map_err(err)?;
            let serial = self.conn.xfixes_get_cursor_image().ok().and_then(|c| c.reply().ok()).map_or(0, |c| c.cursor_serial);
            let moved = (p.root_x, p.root_y, serial) != self.pointer;
            self.pointer = (p.root_x, p.root_y, serial);
            moved
        };
        if !(damaged || moved || self.dirty || self.clean.is_none()) {
            return Ok(None);
        }
        let clean = if damaged || self.dirty || self.clean.is_none() { self.read()? } else { self.clean.take().unwrap_or_default() };
        let mut px = clean.clone();
        self.clean = Some(clean);
        self.dirty = false;
        if !self.apart {
            self.draw_pointer(&mut px);
        }
        let (_, _, w, h) = self.area();
        let img = if self.target == (w, h) { Image { w, h, stride: w * 4, data: px } } else { self.shrink.image(&px, w, h, w * 4, self.target.0, self.target.1) };
        Ok(Some(img))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        let (ax, ay, w, h) = self.area();
        match ev {
            Input::Move { x, y } => {
                let (x, y) = (x.clamp(0.0, w as f64 - 1.0) as i16 + ax, y.clamp(0.0, h as f64 - 1.0) as i16 + ay);
                self.conn.xtest_fake_input(xproto::MOTION_NOTIFY_EVENT, 0, 0, self.root, x, y, 0).map_err(err)?;
            }
            Input::Button { button, pressed } => {
                let b = match button {
                    1..=3 => button,
                    8 | 9 => button,
                    _ => return Ok(()),
                };
                self.fake(if pressed { xproto::BUTTON_PRESS_EVENT } else { xproto::BUTTON_RELEASE_EVENT }, b)?;
            }
            // Wheel notches as the buttons X has for them: 4 up, 5 down, 6 left, 7 right.
            Input::Scroll { steps } | Input::HScroll { steps } => {
                let horizontal = matches!(ev, Input::HScroll { .. });
                let b = match (horizontal, steps > 0) {
                    (false, true) => 5,
                    (false, false) => 4,
                    (true, true) => 7,
                    (true, false) => 6,
                };
                for _ in 0..steps.unsigned_abs().min(50) {
                    self.fake(xproto::BUTTON_PRESS_EVENT, b)?;
                    self.fake(xproto::BUTTON_RELEASE_EVENT, b)?;
                }
            }
            Input::KeyState { keysym, down } => self.key(keysym, down)?,
            Input::Key { keysym, mods } => {
                let (_, shifted) = self.keycode(keysym)?;
                let mut held: Vec<u32> = [(MOD_CTRL, 0xffe3), (MOD_ALT, 0xffe9), (MOD_SUPER, 0xffeb)].into_iter().filter(|(m, _)| mods & m != 0).map(|(_, k)| k).collect();
                if mods & MOD_SHIFT != 0 || shifted {
                    held.push(0xffe1);
                }
                for k in &held {
                    self.key(*k, true)?;
                }
                self.key(keysym, true)?;
                self.key(keysym, false)?;
                for k in held.iter().rev() {
                    self.key(*k, false)?;
                }
            }
            Input::SecureAttention => {}
            // XTest moves it by so much when told the motion is relative.
            Input::MoveBy { dx, dy } => {
                let (dx, dy) = (dx.clamp(-32768, 32767) as i16, dy.clamp(-32768, 32767) as i16);
                self.conn.xtest_fake_input(xproto::MOTION_NOTIFY_EVENT, 1, 0, x11rb::NONE, dx, dy, 0).map_err(err)?;
            }
        }
        self.conn.flush().map_err(err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real X server (Xvfb, or the desktop's): its screen read, a pointer moved there and a key
    /// typed. SSHREMOTE_X11_TEST=1 DISPLAY=:N cargo test -- --ignored an_x11_screen_is_read_and_driven
    #[test]
    #[ignore]
    fn an_x11_screen_is_read_and_driven() {
        if std::env::var_os("SSHREMOTE_X11_TEST").is_none() {
            return;
        }
        let mut x = X11::open().expect("an X display");
        let (w, h) = x.desktop_size();
        let img = x.grab().unwrap().expect("a first picture");
        assert_eq!((img.w, img.h), (w, h));
        assert!(x.grab().unwrap().is_none(), "nothing new: no picture");
        x.input(Input::Move { x: 33.0, y: 44.0 }).unwrap();
        let p = x.conn.query_pointer(x.root).unwrap().reply().unwrap();
        let (ox, oy) = x.origin();
        assert_eq!((p.root_x as i32 - ox, p.root_y as i32 - oy), (33, 44));
        // Moved by so much, as a mouse for games moves it.
        x.input(Input::MoveBy { dx: 10, dy: -4 }).unwrap();
        let p = x.conn.query_pointer(x.root).unwrap().reply().unwrap();
        assert_eq!((p.root_x as i32 - ox, p.root_y as i32 - oy), (43, 40));
        // A character no key makes is typed through a key mapped for it.
        let (code, _) = x.keycode(xk_unicode('한')).unwrap();
        assert!(x.keys.find(xk_unicode('한')).is_none() && code >= x.keys.min);
        x.input(Input::Key { keysym: xk_unicode('한'), mods: 0 }).unwrap();
        x.set_target(w / 2, h / 2).unwrap();
        let half = x.grab().unwrap().expect("a picture at the new size");
        assert_eq!((half.w, half.h), (w / 2, h / 2));
    }

    /// The pointer kept apart: a pointer that moves alone makes no picture, its shape comes once
    /// with where it is, and the pictures leave it out. SSHREMOTE_X11_TEST=1 DISPLAY=:N cargo
    /// test -- --ignored the_x11_pointer_comes_apart
    #[test]
    #[ignore]
    fn the_x11_pointer_comes_apart() {
        if std::env::var_os("SSHREMOTE_X11_TEST").is_none() {
            return;
        }
        let mut x = X11::open().expect("an X display");
        x.input(Input::Move { x: 50.0, y: 60.0 }).unwrap();
        assert!(x.pointer_apart(true));
        let with_none = x.grab().unwrap().expect("a picture without the pointer");
        let (shape, at) = x.pointer();
        let shape = shape.expect("its shape, the first time");
        assert!(shape.w > 0 && shape.rgba.len() == shape.w * shape.h * 4);
        assert_eq!(at, Some((50, 60)));
        assert!(x.pointer().0.is_none(), "the same shape is not sent again");
        x.input(Input::Move { x: 200.0, y: 150.0 }).unwrap();
        assert!(x.grab().unwrap().is_none(), "a pointer moving alone makes no picture");
        assert_eq!(x.pointer().1, Some((200, 150)));
        // Put back into the pictures, it shows there again.
        assert!(x.pointer_apart(false));
        let with = x.grab().unwrap().expect("a picture with the pointer");
        assert_ne!(with.data, with_none.data, "the pointer drawn in");
    }

    fn xk_unicode(c: char) -> u32 {
        crate::xk::unicode(c as u32)
    }
}
