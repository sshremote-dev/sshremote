//! X11 keysyms, the key numbers the viewer and the agent exchange: printable Latin-1 characters
//! are their own code, other characters are `0x0100_0000 | code point`, and these name the rest.

pub const BACKSPACE: u32 = 0xff08;
pub const TAB: u32 = 0xff09;
pub const RETURN: u32 = 0xff0d;
pub const PAUSE: u32 = 0xff13;
pub const ESCAPE: u32 = 0xff1b;
pub const HOME: u32 = 0xff50;
pub const LEFT: u32 = 0xff51;
pub const UP: u32 = 0xff52;
pub const RIGHT: u32 = 0xff53;
pub const DOWN: u32 = 0xff54;
pub const PAGE_UP: u32 = 0xff55;
pub const PAGE_DOWN: u32 = 0xff56;
pub const END: u32 = 0xff57;
pub const PRINT: u32 = 0xff61;
pub const INSERT: u32 = 0xff63;
pub const MENU: u32 = 0xff67;
/// F1 to F24 follow one another.
pub const F1: u32 = 0xffbe;
#[cfg_attr(not(test), allow(dead_code))]
pub const F12: u32 = 0xffc9;
pub const F24: u32 = 0xffd5;
pub const SHIFT_L: u32 = 0xffe1;
pub const SHIFT_R: u32 = 0xffe2;
pub const CONTROL_L: u32 = 0xffe3;
pub const CONTROL_R: u32 = 0xffe4;
pub const CAPS_LOCK: u32 = 0xffe5;
pub const ALT_L: u32 = 0xffe9;
pub const ALT_R: u32 = 0xffea;
pub const SUPER_L: u32 = 0xffeb;
pub const SUPER_R: u32 = 0xffec;
pub const DELETE: u32 = 0xffff;

/// F`n`, for `n` from 1 to 12.
pub fn f(n: u32) -> Option<u32> {
    (1..=12).contains(&n).then(|| F1 + n - 1)
}

/// The keysym of a character: Latin-1 as itself, anything else in the Unicode range.
pub fn of_char(c: char) -> u32 {
    let c = c as u32;
    if c < 0x100 { c } else { unicode(c) }
}

/// A code point in the keysyms' Unicode range, which every keyboard layout can type.
pub fn unicode(c: u32) -> u32 {
    0x0100_0000 | c
}

/// Whether `k` is in the Unicode range: a character to type rather than a key.
pub fn is_unicode(k: u32) -> bool {
    k & 0xff00_0000 == 0x0100_0000
}
