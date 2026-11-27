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
/// Ports forwarded through the computer to what it reaches.
pub const PORTS: u8 = 64;
pub const ALL: u8 = CONTROL | CLIPBOARD | FILES | SOUND | TERMINAL | POWER | PORTS;
/// Not a right: the person at the screen is asked before anyone comes in.
pub const ASK: u8 = 128;

/// Each right with its name in a list and in a sentence.
pub const RIGHTS: [(u8, &str, &str); 7] = [
    (CONTROL, n!("Use the keyboard and mouse"), n!("keyboard and mouse")),
    (CLIPBOARD, n!("Copy and paste"), n!("clipboard")),
    (FILES, n!("Send and fetch files"), n!("files")),
    (SOUND, n!("Hear this computer's sound and call it"), n!("sound and calls")),
    (TERMINAL, n!("Open terminals"), n!("terminals")),
    (POWER, n!("Restart or lock this computer, block its keyboard and hide its screen"), n!("restart and lock")),
    (PORTS, n!("Reach ports through this computer (tunnels)"), n!("ports")),
];

/// The rights in `bits`, as words: "keyboard and mouse, files". English, as it is sent.
pub fn words(bits: u8) -> String {
    let names: Vec<&str> = RIGHTS.iter().filter(|(b, ..)| bits & b != 0).map(|(.., w)| *w).collect();
    if names.is_empty() { n!("nothing but watching").into() } else { names.join(", ") }
}

/// [`words`] in the reader's language, for the window to show.
pub fn shown(bits: u8) -> String {
    let names: Vec<&str> = RIGHTS.iter().filter(|(b, ..)| bits & b != 0).map(|(.., w)| crate::lang::tr(w)).collect();
    // The separator of a list, as the language writes it.
    if names.is_empty() { t!("nothing but watching").into() } else { names.join(t!(", ")) }
}

/// The right a request needs, if any.
pub fn needed(m: &ToAgent) -> Option<u8> {
    Some(match m {
        ToAgent::Key { .. }
        | ToAgent::KeyState { .. }
        | ToAgent::Pointer { .. }
        | ToAgent::PointerBy { .. }
        | ToAgent::Button { .. }
        | ToAgent::Scroll { .. }
        | ToAgent::HScroll { .. }
        | ToAgent::SecureAttention
        | ToAgent::UiAct { .. }
        | ToAgent::PickOnRemote
        | ToAgent::InstallHelper
        | ToAgent::Apps
        | ToAgent::Launch { .. }
        | ToAgent::SetMode { .. }
        | ToAgent::AddScreen { .. }
        | ToAgent::RemoveScreen(_) => CONTROL,
        ToAgent::Clipboard(_) | ToAgent::ClipImage(_) => CLIPBOARD,
        ToAgent::FileStart { .. }
        | ToAgent::FileData { .. }
        | ToAgent::FileEnd { .. }
        | ToAgent::ListDir { .. }
        | ToAgent::Fetch { .. }
        | ToAgent::FetchAck { .. }
        | ToAgent::FetchCancel { .. }
        | ToAgent::FileResume { .. }
        | ToAgent::FetchFrom { .. }
        | ToAgent::FileOp { .. }
        | ToAgent::PasteFiles { .. }
        | ToAgent::Prints(_)
        | ToAgent::Printed(_) => FILES,
        ToAgent::Audio(true) | ToAgent::Call(true) | ToAgent::Voice(_) => SOUND,
        ToAgent::TermOpen { .. } | ToAgent::TermInput { .. } | ToAgent::TermResize { .. } => TERMINAL,
        ToAgent::Host { act, .. } if *act != crate::hostctl::QUERY => POWER,
        ToAgent::PortOpen { .. } | ToAgent::PortData { .. } => PORTS,
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
