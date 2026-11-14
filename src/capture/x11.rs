//! X11 desktops (Xfce, MATE, LXQt, Cinnamon, i3, KDE and GNOME on Xorg): the screen read with
//! MIT-SHM only when XDamage says something changed, the pointer drawn in from XFixes, and input
//! given through XTest, as X11 lets any program of the session do, with no dialog.

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

use super::{Capture, Image, Input, Monitor, scale_nearest};
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
    /// Something changed since the last picture: the screen, or the pointer (which draws on no
    /// window, so it makes no damage).
    dirty: bool,
    pointer: (i16, i16, u32),
    /// The last picture as read, without the pointer, for a pointer that moved alone.
    clean: Option<Vec<u8>>,
    keys: Keys,
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
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display ({e})"))?;
        let s = &conn.setup().roots[screen];
        let root = s.root;
        if s.root_depth < 24 {
            return Err(format!("the X screen has {} bit colour; 24 or 32 is needed", s.root_depth));
        }
        let (w, h) = (s.width_in_pixels as usize, s.height_in_pixels as usize);
        let keys = Keys::read(&conn)?;
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
            dirty: true,
            pointer: (0, 0, 0),
            clean: None,
            keys,
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
        let (m, x, y) = &self.screens[self.current];
        (*x, *y, m.w, m.h)
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

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.screens.len() {
            return Err("no such monitor".into());
        }
        self.current = index;
        (self.dirty, self.clean) = (true, None);
        Ok(())
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

    fn grab(&mut self) -> Result<Option<Image>, String> {
        // What the X server said since the last look: damage, or nothing.
        let mut damaged = self.damage.is_none();
        while let Some(e) = self.conn.poll_for_event().map_err(err)? {
            damaged |= matches!(e, Event::DamageNotify(_));
        }
        if damaged && let Some(d) = self.damage {
            self.conn.damage_subtract(d, x11rb::NONE, x11rb::NONE).map_err(err)?;
        }
        let p = self.conn.query_pointer(self.root).map_err(err)?.reply().map_err(err)?;
        let serial = self.conn.xfixes_get_cursor_image().ok().and_then(|c| c.reply().ok()).map_or(0, |c| c.cursor_serial);
        let moved = (p.root_x, p.root_y, serial) != self.pointer;
        self.pointer = (p.root_x, p.root_y, serial);
        if !(damaged || moved || self.dirty || self.clean.is_none()) {
            return Ok(None);
        }
        let clean = if damaged || self.dirty || self.clean.is_none() { self.read()? } else { self.clean.take().unwrap_or_default() };
        let mut px = clean.clone();
        self.clean = Some(clean);
        self.dirty = false;
        self.draw_pointer(&mut px);
        let (_, _, w, h) = self.area();
        let img = if self.target == (w, h) { Image { w, h, stride: w * 4, data: px } } else { scale_nearest(&px, w, h, w * 4, self.target.0, self.target.1) };
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
        // A character no key makes is typed through a key mapped for it.
        let (code, _) = x.keycode(xk_unicode('한')).unwrap();
        assert!(x.keys.find(xk_unicode('한')).is_none() && code >= x.keys.min);
        x.input(Input::Key { keysym: xk_unicode('한'), mods: 0 }).unwrap();
        x.set_target(w / 2, h / 2).unwrap();
        let half = x.grab().unwrap().expect("a picture at the new size");
        assert_eq!((half.w, half.h), (w / 2, h / 2));
    }

    fn xk_unicode(c: char) -> u32 {
        crate::xk::unicode(c as u32)
    }
}
