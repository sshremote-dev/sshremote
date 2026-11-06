use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER, SUPER_L};

/// Maps a terminal key event to an X11 keysym plus modifier bits.
pub fn keysym(ev: &KeyEvent) -> Option<(u32, u8)> {
    let m = ev.modifiers;
    let mut mods = 0;
    if m.contains(KeyModifiers::CONTROL) {
        mods |= MOD_CTRL;
    }
    if m.contains(KeyModifiers::ALT) {
        mods |= MOD_ALT;
    }
    if m.contains(KeyModifiers::SUPER) {
        mods |= MOD_SUPER;
    }
    let shift = if m.contains(KeyModifiers::SHIFT) { MOD_SHIFT } else { 0 };
    let sym = match ev.code {
        // The character already carries its shift level.
        KeyCode::Char(c) => {
            let c = c as u32;
            return Some((if c <= 0xff { c } else { 0x0100_0000 | c }, mods));
        }
        KeyCode::Backspace => 0xff08,
        KeyCode::Tab => 0xff09,
        KeyCode::BackTab => return Some((0xff09, mods | MOD_SHIFT)),
        KeyCode::Enter => 0xff0d,
        KeyCode::Esc => 0xff1b,
        KeyCode::Home => 0xff50,
        KeyCode::Left => 0xff51,
        KeyCode::Up => 0xff52,
        KeyCode::Right => 0xff53,
        KeyCode::Down => 0xff54,
        KeyCode::PageUp => 0xff55,
        KeyCode::PageDown => 0xff56,
        KeyCode::End => 0xff57,
        KeyCode::Insert => 0xff63,
        KeyCode::Delete => 0xffff,
        KeyCode::F(n @ 1..=12) => 0xffbe + n as u32 - 1,
        _ => return None,
    };
    Some((sym, mods | shift))
}

/// The local menu prefix, Ctrl+], in the forms different terminals report it.
pub fn is_menu(ev: &KeyEvent) -> bool {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    matches!(ev.code, KeyCode::Char('\u{1d}')) || (ctrl && matches!(ev.code, KeyCode::Char(']') | KeyCode::Char('5')))
}

/// Modifier bits and the keys that hold them on the host.
pub const MODIFIERS: [(u8, u32); 4] = [(MOD_SHIFT, 0xffe1), (MOD_CTRL, 0xffe3), (MOD_ALT, 0xffe9), (MOD_SUPER, SUPER_L)];

pub fn mod_bits(m: KeyModifiers) -> u8 {
    let mut bits = 0;
    for (flag, bit) in [
        (KeyModifiers::SHIFT, MOD_SHIFT),
        (KeyModifiers::CONTROL, MOD_CTRL),
        (KeyModifiers::ALT, MOD_ALT),
        (KeyModifiers::SUPER, MOD_SUPER),
    ] {
        if m.contains(flag) {
            bits |= bit;
        }
    }
    bits
}

/// The same key whatever the Shift state, so its press and release match: letters in lower case.
pub fn base(keysym: u32) -> u32 {
    match char::from_u32(keysym) {
        Some(c) if c.is_ascii_uppercase() => c.to_ascii_lowercase() as u32,
        _ => keysym,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_letters_keep_their_key() {
        let ev = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        let (sym, _) = keysym(&ev).unwrap();
        assert_eq!(base(sym), 'a' as u32);
        assert_eq!(mod_bits(ev.modifiers), MOD_SHIFT);
    }
}
