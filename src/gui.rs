//! Window viewer: the same session as the terminal viewer, drawn into a native
//! window at full colour depth with direct keyboard and mouse input.

use std::sync::{Arc, Mutex, mpsc};

use minifb::{InputCallback, Key, KeyRepeat, MouseButton, MouseMode, ScaleMode, Window, WindowOptions};

use crate::color::Mode;
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER, ToAgent, ToClient};
use crate::session::{Action, Control, Ev, Framebuffer, Options, Session};

struct Chars(Arc<Mutex<Vec<u32>>>);

impl InputCallback for Chars {
    fn add_char(&mut self, c: u32) {
        self.0.lock().unwrap().push(c);
    }
}

fn keysym_of(c: u32) -> u32 {
    if c <= 0xff { c } else { 0x0100_0000 | c }
}

/// Keys that produce no text, as X11 keysyms.
fn special(k: Key) -> Option<u32> {
    Some(match k {
        Key::Backspace => 0xff08,
        Key::Tab => 0xff09,
        Key::Enter | Key::NumPadEnter => 0xff0d,
        Key::Escape => 0xff1b,
        Key::Home => 0xff50,
        Key::Left => 0xff51,
        Key::Up => 0xff52,
        Key::Right => 0xff53,
        Key::Down => 0xff54,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::End => 0xff57,
        Key::Insert => 0xff63,
        Key::Delete => 0xffff,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
        _ => return None,
    })
}

/// The unshifted character of a key, for shortcuts such as Ctrl+C where no text is produced.
fn base_char(k: Key) -> Option<char> {
    let n = k as u32;
    Some(match k {
        Key::Key0
        | Key::Key1
        | Key::Key2
        | Key::Key3
        | Key::Key4
        | Key::Key5
        | Key::Key6
        | Key::Key7
        | Key::Key8
        | Key::Key9 => char::from(b'0' + (n - Key::Key0 as u32) as u8),
        k if (Key::A as u32..=Key::Z as u32).contains(&(k as u32)) => char::from(b'a' + (n - Key::A as u32) as u8),
        Key::Space => ' ',
        Key::Minus => '-',
        Key::Equal => '=',
        Key::LeftBracket => '[',
        Key::RightBracket => ']',
        Key::Backslash => '\\',
        Key::Semicolon => ';',
        Key::Apostrophe => '\'',
        Key::Comma => ',',
        Key::Period => '.',
        Key::Slash => '/',
        Key::Backquote => '`',
        _ => return None,
    })
}

struct Gui {
    win: Window,
    buf: Vec<u32>,
    bw: usize,
    bh: usize,
    fb: Framebuffer,
    sess: Session,
    ctl: Control,
    label: String,
    chars: Arc<Mutex<Vec<u32>>>,
    menu: bool,
    buttons: [bool; 3],
    pointer: (u16, u16),
    quit: bool,
}

impl Gui {
    fn mods(&self) -> u8 {
        let down = |a, b| self.win.is_key_down(a) || self.win.is_key_down(b);
        let mut m = 0;
        if down(Key::LeftShift, Key::RightShift) {
            m |= MOD_SHIFT;
        }
        if down(Key::LeftCtrl, Key::RightCtrl) {
            m |= MOD_CTRL;
        }
        if down(Key::LeftAlt, Key::RightAlt) {
            m |= MOD_ALT;
        }
        if down(Key::LeftSuper, Key::RightSuper) {
            m |= MOD_SUPER;
        }
        m
    }

    /// Redraws the display rectangle covering frame pixels `x0..x1, y0..y1`.
    fn draw(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        let (dw, dh) = self.fb.fit((self.bw, self.bh));
        let (fw, fh) = (self.fb.w, self.fb.h);
        if dw == 0 {
            return;
        }
        let (dx0, dx1) = (x0 * dw / fw, (x1 * dw).div_ceil(fw).min(dw));
        let (dy0, dy1) = (y0 * dh / fh, (y1 * dh).div_ceil(fh).min(dh));
        for dy in dy0..dy1 {
            let frow = (dy * fh / dh) * fw;
            let line = &mut self.buf[dy * self.bw..];
            for dx in dx0..dx1 {
                let o = (frow + dx * fw / dw) * 3;
                let p = &self.fb.rgb[o..o + 3];
                line[dx] = (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32;
            }
        }
    }

    fn redraw_all(&mut self) {
        self.buf.fill(0);
        let (w, h) = (self.fb.w, self.fb.h);
        self.draw(0, 0, w, h);
    }

    fn title(&mut self) {
        let t = if self.menu {
            format!("desk menu: {}", Control::MENU_HELP)
        } else {
            format!("desk | {} | Ctrl+] menu", self.ctl.status(&self.label, self.fb.w, self.fb.h))
        };
        self.win.set_title(&t);
    }

    fn act(&mut self, a: Action) -> std::io::Result<()> {
        match a {
            Action::None => {}
            Action::Quit => self.quit = true,
            Action::Refresh => {
                self.fb = Framebuffer::default();
                self.buf.fill(0);
                self.sess.send(ToAgent::Refresh)?;
            }
            Action::Settings => self.sess.send(ToAgent::Settings(self.ctl.s))?,
            Action::Send(m) => self.sess.send(m)?,
        }
        self.title();
        Ok(())
    }

    fn keyboard(&mut self) -> std::io::Result<()> {
        let mods = self.mods();
        let chars: Vec<u32> = std::mem::take(&mut *self.chars.lock().unwrap());
        let shortcut = mods & (MOD_CTRL | MOD_ALT | MOD_SUPER) != 0;
        for k in self.win.get_keys_pressed(KeyRepeat::Yes) {
            if self.menu {
                if let Some(c) = base_char(k) {
                    self.menu = false;
                    let c = if c == '=' && mods & MOD_SHIFT != 0 { '+' } else { c };
                    let a = self.ctl.menu(c);
                    self.act(a)?;
                }
                continue;
            }
            if k == Key::RightBracket && mods & MOD_CTRL != 0 {
                self.menu = true;
                self.title();
                continue;
            }
            if let Some(sym) = special(k) {
                self.sess.send(ToAgent::Key { keysym: sym, mods })?;
            } else if shortcut && let Some(c) = base_char(k) {
                self.sess.send(ToAgent::Key { keysym: c as u32, mods })?;
            }
        }
        if !shortcut && !self.menu {
            // Text arrives already shifted and laid out, so shift is not sent again.
            for c in chars.into_iter().filter(|&c| c >= 0x20 && c != 0x7f) {
                self.sess.send(ToAgent::Key { keysym: keysym_of(c), mods: mods & !MOD_SHIFT })?;
            }
        }
        Ok(())
    }

    fn mouse(&mut self) -> std::io::Result<()> {
        let (dw, dh) = self.fb.fit((self.bw, self.bh));
        let Some((mx, my)) = self.win.get_mouse_pos(MouseMode::Discard) else {
            return Ok(());
        };
        let (mx, my) = (mx as usize, my as usize);
        if dw == 0 || mx >= dw || my >= dh {
            return Ok(());
        }
        let p = ((mx * self.fb.w / dw) as u16, (my * self.fb.h / dh) as u16);
        if p != self.pointer {
            self.pointer = p;
            self.sess.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
        }
        for (i, b) in [MouseButton::Left, MouseButton::Middle, MouseButton::Right].into_iter().enumerate() {
            let down = self.win.get_mouse_down(b);
            if down != self.buttons[i] {
                self.buttons[i] = down;
                self.sess.send(ToAgent::Button { button: i as u8 + 1, pressed: down })?;
            }
        }
        if let Some((_, dy)) = self.win.get_scroll_wheel()
            && dy.abs() >= 1.0
        {
            let steps = (-dy / 120.0).round().clamp(-10.0, 10.0) as i8;
            let steps = if steps == 0 { -dy.signum() as i8 } else { steps };
            self.sess.send(ToAgent::Scroll { steps })?;
        }
        Ok(())
    }
}

pub fn run(opt: &Options, sess: Session, rx: mpsc::Receiver<Ev>) -> Result<(), String> {
    let (w, h) = (sess.desk.0.max(16) as f64, sess.desk.1.max(16) as f64);
    let k = (1600.0 / w).min(900.0 / h).min(1.0);
    let (ww, wh) = ((w * k) as usize, (h * k) as usize);
    let mut win = Window::new(
        "desk",
        ww,
        wh,
        WindowOptions { resize: true, scale_mode: ScaleMode::UpperLeft, ..WindowOptions::default() },
    )
    .map_err(|e| format!("could not open a window: {e}"))?;
    win.set_target_fps(120);
    let chars = Arc::new(Mutex::new(Vec::new()));
    win.set_input_callback(Box::new(Chars(chars.clone())));
    let host = opt.host.clone().unwrap_or_else(|| "local".into());
    let mut g = Gui {
        win,
        buf: vec![0; ww * wh],
        bw: ww,
        bh: wh,
        fb: Framebuffer::default(),
        label: format!("{host} {}x{} {}", sess.desk.0, sess.desk.1, sess.backend),
        sess,
        ctl: Control::new(opt, Mode::True),
        chars,
        menu: false,
        buttons: [false; 3],
        pointer: (u16::MAX, u16::MAX),
        quit: false,
    };
    (g.ctl.s.view_w, g.ctl.s.view_h) = (ww as u16, wh as u16);
    let mut failure = None;
    let r = (|| -> std::io::Result<()> {
        g.sess.send(ToAgent::Settings(g.ctl.s))?;
        g.title();
        while g.win.is_open() && !g.quit {
            let size = g.win.get_size();
            if size != (g.bw, g.bh) && size.0 > 0 && size.1 > 0 {
                (g.bw, g.bh) = size;
                g.buf = vec![0; size.0 * size.1];
                g.redraw_all();
                (g.ctl.s.view_w, g.ctl.s.view_h) = (size.0 as u16, size.1 as u16);
                g.sess.send(ToAgent::Settings(g.ctl.s))?;
            }
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    Ev::Net(ToClient::Frame(f)) => {
                        if g.fb.apply(&f)? {
                            g.redraw_all();
                        } else {
                            for t in &f.tiles {
                                let (x, y) = (t.x as usize, t.y as usize);
                                g.draw(x, y, x + t.w as usize, y + t.h as usize);
                            }
                        }
                        g.ctl.frame_done(f.covered() * 2 >= f.w as usize * f.h as usize);
                        g.sess.send(ToAgent::Ack(f.id))?;
                    }
                    Ev::Net(ToClient::Pong(t)) => g.ctl.pong(t),
                    Ev::Net(ToClient::Error(e)) | Ev::Closed(e) => {
                        failure = Some(e);
                        return Ok(());
                    }
                    _ => {}
                }
            }
            g.keyboard()?;
            g.mouse()?;
            if g.ctl.tick(&mut g.sess)? {
                g.title();
            }
            g.win.update_with_buffer(&g.buf, g.bw, g.bh).map_err(std::io::Error::other)?;
        }
        Ok(())
    })();
    g.sess.close();
    if let Err(e) = r {
        failure.get_or_insert(e.to_string());
    }
    match failure {
        Some(e) => Err(format!("{e}\n{}", g.sess.log())),
        None => Ok(()),
    }
}
