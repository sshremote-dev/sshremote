//! Input for Wayland desktops whose portal shares the screen but takes no input (wlroots ones:
//! Sway, Hyprland...): a virtual pointer and a virtual keyboard of this program's own, as wtype
//! and wayvnc make. The keys of a US keyboard sit where they are on one (Hyprland reads a
//! shortcut's key by its code in the desktop's own layout, so Super+Q must come as Q's code), with
//! their Shift level; any other character gets a code no US key has, so it types whatever layout
//! the desktop has. Shift and the rest are told as modifiers.

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

/// The modifiers' keysyms and their bits.
const MODS: [(u32, u32); 8] =
    [(0xffe1, 1), (0xffe2, 1), (0xffe3, 4), (0xffe4, 4), (0xffe9, 8), (0xffea, 8), (0xffeb, 64), (0xffec, 64)];

/// A US keyboard: each key's evdev code, its keysym and the one Shift gives (0: the same).
const US: &[(u32, u32, u32)] = &[
    (1, 0xff1b, 0), (2, b'1' as u32, b'!' as u32), (3, b'2' as u32, b'@' as u32), (4, b'3' as u32, b'#' as u32),
    (5, b'4' as u32, b'$' as u32), (6, b'5' as u32, b'%' as u32), (7, b'6' as u32, b'^' as u32), (8, b'7' as u32, b'&' as u32),
    (9, b'8' as u32, b'*' as u32), (10, b'9' as u32, b'(' as u32), (11, b'0' as u32, b')' as u32), (12, b'-' as u32, b'_' as u32),
    (13, b'=' as u32, b'+' as u32), (14, 0xff08, 0), (15, 0xff09, 0xfe20), (16, b'q' as u32, b'Q' as u32), (17, b'w' as u32, b'W' as u32),
    (18, b'e' as u32, b'E' as u32), (19, b'r' as u32, b'R' as u32), (20, b't' as u32, b'T' as u32), (21, b'y' as u32, b'Y' as u32),
    (22, b'u' as u32, b'U' as u32), (23, b'i' as u32, b'I' as u32), (24, b'o' as u32, b'O' as u32), (25, b'p' as u32, b'P' as u32),
    (26, b'[' as u32, b'{' as u32), (27, b']' as u32, b'}' as u32), (28, 0xff0d, 0), (29, 0xffe3, 0), (30, b'a' as u32, b'A' as u32),
    (31, b's' as u32, b'S' as u32), (32, b'd' as u32, b'D' as u32), (33, b'f' as u32, b'F' as u32), (34, b'g' as u32, b'G' as u32),
    (35, b'h' as u32, b'H' as u32), (36, b'j' as u32, b'J' as u32), (37, b'k' as u32, b'K' as u32), (38, b'l' as u32, b'L' as u32),
    (39, b';' as u32, b':' as u32), (40, b'\'' as u32, b'"' as u32), (41, b'`' as u32, b'~' as u32), (42, 0xffe1, 0),
    (43, b'\\' as u32, b'|' as u32), (44, b'z' as u32, b'Z' as u32), (45, b'x' as u32, b'X' as u32), (46, b'c' as u32, b'C' as u32),
    (47, b'v' as u32, b'V' as u32), (48, b'b' as u32, b'B' as u32), (49, b'n' as u32, b'N' as u32), (50, b'm' as u32, b'M' as u32),
    (51, b',' as u32, b'<' as u32), (52, b'.' as u32, b'>' as u32), (53, b'/' as u32, b'?' as u32), (54, 0xffe2, 0),
    (55, 0xffaa, 0), (56, 0xffe9, 0), (57, b' ' as u32, 0), (58, 0xffe5, 0), (59, 0xffbe, 0), (60, 0xffbf, 0), (61, 0xffc0, 0),
    (62, 0xffc1, 0), (63, 0xffc2, 0), (64, 0xffc3, 0), (65, 0xffc4, 0), (66, 0xffc5, 0), (67, 0xffc6, 0), (68, 0xffc7, 0),
    (69, 0xff7f, 0), (70, 0xff14, 0), (71, 0xffb7, 0), (72, 0xffb8, 0), (73, 0xffb9, 0), (74, 0xffad, 0), (75, 0xffb4, 0),
    (76, 0xffb5, 0), (77, 0xffb6, 0), (78, 0xffab, 0), (79, 0xffb1, 0), (80, 0xffb2, 0), (81, 0xffb3, 0), (82, 0xffb0, 0),
    (83, 0xffae, 0), (87, 0xffc8, 0), (88, 0xffc9, 0), (96, 0xff8d, 0), (97, 0xffe4, 0), (98, 0xffaf, 0), (99, 0xff61, 0),
    (100, 0xffea, 0), (102, 0xff50, 0), (103, 0xff52, 0), (104, 0xff55, 0), (105, 0xff51, 0), (106, 0xff53, 0), (107, 0xff57, 0),
    (108, 0xff54, 0), (109, 0xff56, 0), (110, 0xff63, 0), (111, 0xffff, 0), (119, 0xff13, 0), (125, 0xffeb, 0), (126, 0xffec, 0),
    (127, 0xff67, 0), (183, 0xffca, 0), (184, 0xffcb, 0), (185, 0xffcc, 0), (186, 0xffcd, 0), (187, 0xffce, 0), (188, 0xffcf, 0),
    (189, 0xffd0, 0), (190, 0xffd1, 0), (191, 0xffd2, 0), (192, 0xffd3, 0), (193, 0xffd4, 0), (194, 0xffd5, 0),
];

/// Codes for the other characters: ones the desktop's US layout (xkeyboard-config's evdev rules)
/// gives no keysym, so a shortcut there never takes one for a key of its own (a volume key, say).
const SPARE: [u32; 15] = [89, 95, 112, 124, 141, 146, 160, 170, 175, 176, 209, 211, 214, 222, 240];

/// Where a keysym is on the keymap: its evdev code, and whether it takes Shift there.
fn us_key(keysym: u32) -> Option<(u32, bool)> {
    US.iter().find_map(|&(code, base, shifted)| {
        if base == keysym {
            Some((code, false))
        } else {
            (shifted != 0 && shifted == keysym).then_some((code, true))
        }
    })
}

pub struct WlInput {
    conn: Connection,
    queue: EventQueue<Globals>,
    /// None where the compositor has a virtual keyboard only (COSMIC): its pointer goes by the portal.
    pointer: Option<ZwlrVirtualPointerV1>,
    keyboard: ZwpVirtualKeyboardV1,
    /// The characters no US key has, on the spare codes: (keysym, code), the latest used last.
    extra: Vec<(u32, u32)>,
    /// Modifier keys held, their bits together, and when this began (event times count from it).
    held: Vec<u32>,
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
        let mut w = WlInput { conn, queue, pointer, keyboard, extra: Vec::new(), held: Vec::new(), mods: 0, start: std::time::Instant::now(), last_key: None };
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

    /// The keymap: the US keys with their Shift level, then the other characters, one level each.
    fn upload(&mut self) -> Result<(), String> {
        let text = keymap(&self.extra);
        let mut f = tempfile_fd()?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        // wl_keyboard's XKB_V1 format.
        self.keyboard.keymap(1, f.as_fd(), text.len() as u32);
        self.conn.flush().map_err(|e| e.to_string())
    }

    /// The evdev code for `keysym` and whether it needs Shift; a character no US key has takes
    /// a spare code (the one used longest ago when all are taken) and the keymap is sent again.
    fn key_of(&mut self, keysym: u32) -> Result<(u32, bool, bool), String> {
        if let Some((code, shift)) = us_key(keysym) {
            return Ok((code, shift, false));
        }
        if let Some(i) = self.extra.iter().position(|&(s, _)| s == keysym) {
            let e = self.extra.remove(i);
            self.extra.push(e);
            return Ok((e.1, false, false));
        }
        let code = if self.extra.len() < SPARE.len() { SPARE[self.extra.len()] } else { self.extra.remove(0).1 };
        self.extra.push((keysym, code));
        self.upload()?;
        Ok((code, false, true))
    }

    fn press(&mut self, keysym: u32, down: bool) -> Result<(), String> {
        // Typing after a pause: the keymap again first, as the window with the keyboard may have
        // changed meanwhile and some compositors (Hyprland) give it the real keyboard's keymap.
        let mut sent = self.last_key.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(1));
        if sent {
            self.upload()?;
        }
        self.last_key = Some(std::time::Instant::now());
        let (key, shift, new) = self.key_of(keysym)?;
        sent |= new;
        let before = self.mods;
        if MODS.iter().any(|(s, _)| *s == keysym) {
            if down && !self.held.contains(&keysym) {
                self.held.push(keysym);
            } else if !down {
                self.held.retain(|&k| k != keysym);
            }
            self.mods = self.held.iter().filter_map(|k| MODS.iter().find(|(s, _)| s == k)).fold(0, |m, (_, b)| m | b);
        }
        // A new keymap leaves the modifiers held unknown there (Hyprland forgets them): they are
        // told again before the key, and Shift for a character on a key's Shift level.
        let lift = down && shift && before & 1 == 0;
        if sent || lift {
            self.keyboard.modifiers(before | if lift { 1 } else { 0 }, 0, 0, 0);
        }
        let t = self.time();
        self.keyboard.key(t, key, u32::from(down));
        self.keyboard.modifiers(self.mods, 0, 0, 0);
        Ok(())
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
            Input::KeyState { keysym, down } => self.press(keysym, down)?,
            Input::Key { keysym, mods } => {
                let held: Vec<u32> = [(MOD_SHIFT, 0xffe1), (MOD_CTRL, 0xffe3), (MOD_ALT, 0xffe9), (MOD_SUPER, 0xffeb)].into_iter().filter(|(m, _)| mods & m != 0).map(|(_, k)| k).collect();
                for &k in &held {
                    self.press(k, true)?;
                }
                self.press(keysym, true)?;
                self.press(keysym, false)?;
                for &k in held.iter().rev() {
                    self.press(k, false)?;
                }
            }
            Input::SecureAttention => {}
        }
        self.conn.flush().map_err(|e| e.to_string())
    }
}

/// The keymap text: the US keys at their codes with their Shift level, the other characters at
/// their spare codes, and the modifiers.
fn keymap(extra: &[(u32, u32)]) -> String {
    let mut keycodes = String::new();
    let mut symbols = String::new();
    for &(code, base, shifted) in US {
        let x = code + 8;
        keycodes.push_str(&format!("<K{x}> = {x};\n"));
        if shifted == 0 {
            symbols.push_str(&format!("key <K{x}> {{ [ 0x{base:x} ] }};\n"));
        } else {
            symbols.push_str(&format!("key <K{x}> {{ [ 0x{base:x}, 0x{shifted:x} ] }};\n"));
        }
    }
    for &(sym, code) in extra {
        let x = code + 8;
        keycodes.push_str(&format!("<K{x}> = {x};\n"));
        symbols.push_str(&format!("key <K{x}> {{ [ 0x{sym:x} ] }};\n"));
    }
    for (name, bit) in [("Shift", 1), ("Control", 4), ("Mod1", 8), ("Mod4", 64)] {
        let keys: Vec<String> = MODS
            .iter()
            .filter(|(_, b)| *b == bit)
            .filter_map(|(s, _)| us_key(*s))
            .map(|(code, _)| format!("<K{}>", code + 8))
            .collect();
        symbols.push_str(&format!("modifier_map {name} {{ {} }};\n", keys.join(", ")));
    }
    format!(
        "xkb_keymap {{\nxkb_keycodes \"sshremote\" {{ minimum = 8; maximum = 255; {keycodes} }};\nxkb_types \"sshremote\" {{ include \"complete\" }};\nxkb_compat \"sshremote\" {{ include \"complete\" }};\nxkb_symbols \"sshremote\" {{ {symbols} }};\n}};\n\0"
    )
}

/// An unnamed file for the keymap (memfd).
pub(super) fn tempfile_fd() -> Result<std::fs::File, String> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::memfd_create(c"sshremote-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Super+Q reaches Hyprland as Q's own code with Super held, and Shift's characters come
    /// from the keys a US keyboard has them on.
    #[test]
    fn keys_sit_where_a_us_keyboard_has_them() {
        assert_eq!(us_key('q' as u32), Some((16, false)));
        assert_eq!(us_key('Q' as u32), Some((16, true)));
        assert_eq!(us_key('!' as u32), Some((2, true)));
        assert_eq!(us_key(crate::xk::SUPER_L), Some((125, false)));
        assert_eq!(us_key(crate::xk::F24), Some((194, false)));
        assert_eq!(us_key(0x100c548), None);
        let mut codes: Vec<u32> = US.iter().map(|k| k.0).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), US.len(), "one key per code");
        assert!(SPARE.iter().all(|c| !codes.contains(c) && c + 8 <= 255), "spare codes are no US key's");
        let text = keymap(&[(0x100c548, SPARE[0])]);
        assert!(text.contains("key <K24> { [ 0x71, 0x51 ] };"));
        assert!(text.contains("key <K97> { [ 0x100c548 ] };"));
        assert!(text.contains("modifier_map Mod4 { <K133>, <K134> };"));
    }
}
