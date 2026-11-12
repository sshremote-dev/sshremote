use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::xk;
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER, SUPER_L};

/// Maps a terminal key event to an X11 keysym plus modifier bits.
pub fn keysym(ev: &KeyEvent) -> Option<(u32, u8)> {
    let m = altgr_free(ev);
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
            return Some((xk::of_char(c), mods));
        }
        KeyCode::Backspace => xk::BACKSPACE,
        KeyCode::Tab => xk::TAB,
        KeyCode::BackTab => return Some((xk::TAB, mods | MOD_SHIFT)),
        KeyCode::Enter => xk::RETURN,
        KeyCode::Esc => xk::ESCAPE,
        KeyCode::Home => xk::HOME,
        KeyCode::Left => xk::LEFT,
        KeyCode::Up => xk::UP,
        KeyCode::Right => xk::RIGHT,
        KeyCode::Down => xk::DOWN,
        KeyCode::PageUp => xk::PAGE_UP,
        KeyCode::PageDown => xk::PAGE_DOWN,
        KeyCode::End => xk::END,
        KeyCode::Insert => xk::INSERT,
        KeyCode::Delete => xk::DELETE,
        KeyCode::F(n @ 1..=12) => xk::F1 + n as u32 - 1,
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
pub const MODIFIERS: [(u8, u32); 4] =
    [(MOD_SHIFT, xk::SHIFT_L), (MOD_CTRL, xk::CONTROL_L), (MOD_ALT, xk::ALT_L), (MOD_SUPER, SUPER_L)];

/// AltGr arrives as Ctrl+Alt with the character it made ('@', '{' on European layouts); the
/// character is what was meant, so the two modifiers are dropped.
pub fn altgr_free(ev: &KeyEvent) -> KeyModifiers {
    let m = ev.modifiers;
    match ev.code {
        KeyCode::Char(c) if m.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) && !c.is_ascii_alphanumeric() => {
            m - KeyModifiers::CONTROL - KeyModifiers::ALT
        }
        _ => m,
    }
}

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

/// The modifier a key is, as the bit the host holds it by (AltGr and the like are none of them).
pub fn modifier_bit(code: &KeyCode) -> Option<u8> {
    use crossterm::event::ModifierKeyCode as M;
    match code {
        KeyCode::Modifier(M::LeftShift | M::RightShift) => Some(MOD_SHIFT),
        KeyCode::Modifier(M::LeftControl | M::RightControl) => Some(MOD_CTRL),
        KeyCode::Modifier(M::LeftAlt | M::RightAlt) => Some(MOD_ALT),
        KeyCode::Modifier(M::LeftSuper | M::RightSuper) => Some(MOD_SUPER),
        _ => None,
    }
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
    fn modifier_keys_name_their_bit() {
        use crossterm::event::ModifierKeyCode as M;
        assert_eq!(modifier_bit(&KeyCode::Modifier(M::RightControl)), Some(MOD_CTRL));
        assert_eq!(modifier_bit(&KeyCode::Modifier(M::LeftSuper)), Some(MOD_SUPER));
        assert_eq!(modifier_bit(&KeyCode::Modifier(M::IsoLevel3Shift)), None);
        assert_eq!(modifier_bit(&KeyCode::Char('a')), None);
    }

    #[test]
    fn shifted_letters_keep_their_key() {
        let ev = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        let (sym, _) = keysym(&ev).unwrap();
        assert_eq!(base(sym), 'a' as u32);
        assert_eq!(mod_bits(ev.modifiers), MOD_SHIFT);
    }

    #[test]
    fn altgr_characters_lose_ctrl_and_alt() {
        let at = KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(keysym(&at), Some(('@' as u32, 0)));
        let del = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(keysym(&del), Some(('t' as u32, MOD_CTRL | MOD_ALT)));
    }
}
