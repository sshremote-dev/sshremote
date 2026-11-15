//! Input for Wayland desktops whose portal shares the screen but takes no input (wlroots ones:
//! Sway, Hyprland...): a virtual pointer and a virtual keyboard of this program's own, as wtype
//! and wayvnc make. The keyboard's keymap gives each key it needs a keycode of its own, so any
//! character types whatever layout the desktop has; Shift and the rest are told as modifiers.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use super::Input;
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

#[derive(Default)]
struct Globals {
    seat: Option<WlSeat>,
    pointers: Option<ZwlrVirtualPointerManagerV1>,
    keyboards: Option<ZwpVirtualKeyboardManagerV1>,
}

impl Dispatch<WlRegistry, ()> for Globals {
    fn event(g: &mut Globals, r: &WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Globals>) {
        if let wl_registry::Event::Global { name, interface, version } = e {
            match interface.as_str() {
                "wl_seat" if g.seat.is_none() => g.seat = Some(r.bind(name, version.min(1), qh, ())),
                "zwlr_virtual_pointer_manager_v1" => g.pointers = Some(r.bind(name, version.min(1), qh, ())),
                "zwp_virtual_keyboard_manager_v1" => g.keyboards = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Globals {
            fn event(_: &mut Globals, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Globals>) {}
        }
    )*};
}
ignore!(WlSeat, ZwlrVirtualPointerManagerV1, ZwpVirtualKeyboardManagerV1, ZwlrVirtualPointerV1, ZwpVirtualKeyboardV1);

/// The modifiers' keysyms, their keycodes (the keymap's first) and their bits.
const MODS: [(u32, u32); 4] = [(0xffe1, 1), (0xffe3, 4), (0xffe9, 8), (0xffeb, 64)];

pub struct WlInput {
    conn: Connection,
    queue: EventQueue<Globals>,
    /// None where the compositor has a virtual keyboard only (COSMIC): its pointer goes by the portal.
    pointer: Option<ZwlrVirtualPointerV1>,
    keyboard: ZwpVirtualKeyboardV1,
    /// The keysyms the keymap has, each on its own key after the modifiers'.
    syms: Vec<u32>,
    /// Modifiers held (their bits), and when this began (event times count from it).
    mods: u32,
    start: std::time::Instant,
    /// The last key, to send the keymap again when typing starts after a pause.
    last_key: Option<std::time::Instant>,
}

impl WlInput {
    pub fn open() -> Result<WlInput, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display ({e})"))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut g = Globals::default();
        queue.roundtrip(&mut g).map_err(|e| e.to_string())?;
        let (Some(seat), Some(km)) = (g.seat.clone(), g.keyboards.clone()) else {
            return Err("this desktop offers no virtual keyboard".into());
        };
        let pointer = g.pointers.clone().map(|pm| pm.create_virtual_pointer(Some(&seat), &qh, ()));
        let keyboard = km.create_virtual_keyboard(&seat, &qh, ());
        let mut w = WlInput { conn, queue, pointer, keyboard, syms: MODS.iter().map(|m| m.0).collect(), mods: 0, start: std::time::Instant::now(), last_key: None };
        w.upload()?;
        w.queue.roundtrip(&mut g).map_err(|e| e.to_string())?;
        Ok(w)
    }

    /// Whether the pointer goes this way too.
    pub fn has_pointer(&self) -> bool {
        self.pointer.is_some()
    }

    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// The keymap: a key for each keysym in `syms`, one level each.
    fn upload(&mut self) -> Result<(), String> {
        let mut keycodes = String::new();
        let mut symbols = String::new();
        for (i, s) in self.syms.iter().enumerate() {
            let code = i + 9;
            keycodes.push_str(&format!("<K{code}> = {code};\n"));
            symbols.push_str(&format!("key <K{code}> {{ [ 0x{s:x} ] }};\n"));
        }
        for ((sym, _), name) in MODS.iter().zip(["Shift", "Control", "Mod1", "Mod4"]) {
            let code = self.syms.iter().position(|s| s == sym).unwrap_or(0) + 9;
            symbols.push_str(&format!("modifier_map {name} {{ <K{code}> }};\n"));
        }
        let text = format!(
            "xkb_keymap {{\nxkb_keycodes \"sshremote\" {{ minimum = 8; maximum = {}; {keycodes} }};\nxkb_types \"sshremote\" {{ include \"complete\" }};\nxkb_compat \"sshremote\" {{ include \"complete\" }};\nxkb_symbols \"sshremote\" {{ {symbols} }};\n}};\n\0",
            (self.syms.len() + 9).max(255)
        );
        let mut f = tempfile_fd()?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        // wl_keyboard's XKB_V1 format.
        self.keyboard.keymap(1, f.as_fd(), text.len() as u32);
        self.conn.flush().map_err(|e| e.to_string())
    }

    /// The evdev key for `keysym`, added to the keymap when it is not there yet.
    fn key_of(&mut self, keysym: u32) -> Result<u32, String> {
        if let Some(i) = self.syms.iter().position(|s| *s == keysym) {
            return Ok(i as u32 + 1);
        }
        // Room for 240 keys: the oldest that is no modifier makes way.
        if self.syms.len() >= 240 {
            self.syms.remove(MODS.len());
        }
        self.syms.push(keysym);
        self.upload()?;
        Ok(self.syms.len() as u32)
    }

    fn press(&mut self, keysym: u32, down: bool) -> Result<(), String> {
        // Typing after a pause: the keymap again first, as the window with the keyboard may have
        // changed meanwhile and some compositors (Hyprland) give it the real keyboard's keymap.
        if self.last_key.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(1)) {
            self.upload()?;
        }
        self.last_key = Some(std::time::Instant::now());
        let key = self.key_of(keysym)?;
        if let Some((_, bit)) = MODS.iter().find(|(s, _)| *s == keysym) {
            self.mods = if down { self.mods | bit } else { self.mods & !bit };
        }
        let t = self.time();
        self.keyboard.key(t, key, u32::from(down));
        self.keyboard.modifiers(self.mods, 0, 0, 0);
        Ok(())
    }

    /// The keysym a key makes with Shift held, on a US keyboard (the viewer sends the key's own).
    fn shifted(&self, keysym: u32) -> u32 {
        if self.mods & 1 == 0 {
            return keysym;
        }
        const PAIRS: &[(u8, u8)] = &[
            (b'1', b'!'), (b'2', b'@'), (b'3', b'#'), (b'4', b'$'), (b'5', b'%'), (b'6', b'^'), (b'7', b'&'), (b'8', b'*'), (b'9', b'('), (b'0', b')'),
            (b'-', b'_'), (b'=', b'+'), (b'[', b'{'), (b']', b'}'), (b'\\', b'|'), (b';', b':'), (b'\'', b'"'), (b',', b'<'), (b'.', b'>'), (b'/', b'?'), (b'`', b'~'),
        ];
        match keysym {
            k @ 0x61..=0x7a => k - 0x20,
            k if k < 0x80 => PAIRS.iter().find(|(a, _)| *a as u32 == k).map_or(k, |(_, b)| *b as u32),
            k => k,
        }
    }

    /// The viewer's input, on a screen of `desk` pixels.
    pub fn input(&mut self, ev: Input, desk: (usize, usize)) -> Result<(), String> {
        let t = self.time();
        let pointer = || self.pointer.as_ref().ok_or_else(|| "this desktop offers no virtual pointer".to_string());
        match ev {
            Input::Move { x, y } => {
                let (w, h) = (desk.0.max(1) as u32, desk.1.max(1) as u32);
                let p = pointer()?;
                p.motion_absolute(t, (x.max(0.0) as u32).min(w - 1), (y.max(0.0) as u32).min(h - 1), w, h);
                p.frame();
            }
            Input::Button { button, pressed } => {
                let code = match button {
                    1 => 0x110,
                    2 => 0x112,
                    3 => 0x111,
                    8 => 0x113,
                    9 => 0x114,
                    _ => return Ok(()),
                };
                let state = if pressed { wayland_client::protocol::wl_pointer::ButtonState::Pressed } else { wayland_client::protocol::wl_pointer::ButtonState::Released };
                let p = pointer()?;
                p.button(t, code, state);
                p.frame();
            }
            Input::Scroll { steps } | Input::HScroll { steps } => {
                use wayland_client::protocol::wl_pointer::{Axis, AxisSource};
                let axis = if matches!(ev, Input::HScroll { .. }) { Axis::HorizontalScroll } else { Axis::VerticalScroll };
                let p = pointer()?;
                p.axis_source(AxisSource::Wheel);
                p.axis_discrete(t, axis, steps as f64 * 15.0, steps);
                p.frame();
            }
            Input::KeyState { keysym, down } => {
                let sym = self.shifted(keysym);
                self.press(sym, down)?;
            }
            Input::Key { keysym, mods } => {
                let held: Vec<u32> = [(MOD_SHIFT, 0xffe1), (MOD_CTRL, 0xffe3), (MOD_ALT, 0xffe9), (MOD_SUPER, 0xffeb)].into_iter().filter(|(m, _)| mods & m != 0).map(|(_, k)| k).collect();
                for &k in &held {
                    self.press(k, true)?;
                }
                let sym = self.shifted(keysym);
                self.press(sym, true)?;
                self.press(sym, false)?;
                for &k in held.iter().rev() {
                    self.press(k, false)?;
                }
            }
            Input::SecureAttention => {}
        }
        self.conn.flush().map_err(|e| e.to_string())
    }
}

/// An unnamed file for the keymap (memfd).
fn tempfile_fd() -> Result<std::fs::File, String> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::memfd_create(c"sshremote-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}
