use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

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
