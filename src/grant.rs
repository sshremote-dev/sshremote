//! What a viewer who came by this computer's ID may do: set in the remote access settings, and
//! changed by the person at the screen for one session in its window. A viewer over SSH signed in
//! as the computer's user and may do everything.

use crate::proto::ToAgent;

/// Keyboard and mouse, and acting on controls for them.
pub const CONTROL: u8 = 1;
/// The clipboard, both ways.
pub const CLIPBOARD: u8 = 2;
/// Files: sending, fetching, browsing, renaming, deleting.
pub const FILES: u8 = 4;
/// The computer's sound.
pub const SOUND: u8 = 8;
/// Terminals on the computer.
pub const TERMINAL: u8 = 16;
/// Restarting and locking the computer, keeping its own keyboard and mouse out, and hiding its screen.
pub const POWER: u8 = 32;
pub const ALL: u8 = CONTROL | CLIPBOARD | FILES | SOUND | TERMINAL | POWER;
/// Not a right: the person at the screen is asked before anyone comes in.
pub const ASK: u8 = 128;

/// Each right with its name in a list and in a sentence.
pub const RIGHTS: [(u8, &str, &str); 6] = [
    (CONTROL, "Use the keyboard and mouse", "keyboard and mouse"),
    (CLIPBOARD, "Copy and paste", "clipboard"),
    (FILES, "Send and fetch files", "files"),
    (SOUND, "Hear this computer's sound", "sound"),
    (TERMINAL, "Open terminals", "terminals"),
    (POWER, "Restart or lock this computer, block its keyboard and hide its screen", "restart and lock"),
];

/// The rights in `bits`, as words: "keyboard and mouse, files".
pub fn words(bits: u8) -> String {
    let names: Vec<&str> = RIGHTS.iter().filter(|(b, ..)| bits & b != 0).map(|(.., w)| *w).collect();
    if names.is_empty() { "nothing but watching".into() } else { names.join(", ") }
}

/// The right a request needs, if any.
pub fn needed(m: &ToAgent) -> Option<u8> {
    Some(match m {
        ToAgent::Key { .. }
        | ToAgent::KeyState { .. }
        | ToAgent::Pointer { .. }
        | ToAgent::Button { .. }
        | ToAgent::Scroll { .. }
        | ToAgent::HScroll { .. }
        | ToAgent::SecureAttention
        | ToAgent::UiAct { .. }
        | ToAgent::PickOnRemote
        | ToAgent::InstallHelper => CONTROL,
        ToAgent::Clipboard(_) => CLIPBOARD,
        ToAgent::FileStart { .. }
        | ToAgent::FileData { .. }
        | ToAgent::FileEnd { .. }
        | ToAgent::ListDir { .. }
        | ToAgent::Fetch { .. }
        | ToAgent::FetchAck { .. }
        | ToAgent::FetchCancel { .. }
        | ToAgent::FileOp { .. }
        | ToAgent::PasteFiles { .. } => FILES,
        ToAgent::Audio(true) => SOUND,
        ToAgent::TermOpen { .. } | ToAgent::TermInput { .. } | ToAgent::TermResize { .. } => TERMINAL,
        ToAgent::Host { act, .. } if *act != crate::hostctl::QUERY => POWER,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_need_their_rights() {
        assert_eq!(needed(&ToAgent::Pointer { x: 1, y: 2 }), Some(CONTROL));
        assert_eq!(needed(&ToAgent::Clipboard("x".into())), Some(CLIPBOARD));
        assert_eq!(needed(&ToAgent::ListDir { path: "/".into() }), Some(FILES));
        assert_eq!(needed(&ToAgent::Audio(true)), Some(SOUND));
        assert_eq!(needed(&ToAgent::Audio(false)), None);
        assert_eq!(needed(&ToAgent::Chat("hi".into())), None);
        assert_eq!(needed(&ToAgent::Refresh), None);
        assert_eq!(needed(&ToAgent::Host { act: crate::hostctl::RESTART, on: false }), Some(POWER));
        assert_eq!(needed(&ToAgent::Host { act: crate::hostctl::QUERY, on: false }), None);
        assert_eq!(words(CONTROL | FILES), "keyboard and mouse, files");
        assert_eq!(words(0), "nothing but watching");
        assert_eq!(ALL & ASK, 0);
    }
}
