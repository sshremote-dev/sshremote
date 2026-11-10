//! Types the wire protocol carries between the agent and viewers, needed even where neither runs.

/// A BGRx frame at the size requested with `set_target`.
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub data: Vec<u8>,
}

/// A top-level window that can be shared on its own.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    /// The program it belongs to, e.g. "chrome".
    pub app: String,
    pub w: u16,
    pub h: u16,
}

/// What part of the desktop is shared, as in a video call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Share {
    /// The whole monitor.
    Screen,
    /// One window, with its menus and dialogs.
    Window(u64),
    /// Every window of the program the given window belongs to.
    App(u64),
    /// Whichever window has the focus now.
    ActiveWindow,
    /// Every window of the program whose window has the focus now.
    ActiveApp,
}

impl Share {
    /// Its number on the wire and the window it names (0 for none).
    pub fn to_wire(self) -> (u8, u64) {
        match self {
            Share::Screen => (0, 0),
            Share::Window(id) => (1, id),
            Share::App(id) => (2, id),
            Share::ActiveWindow => (3, 0),
            Share::ActiveApp => (4, 0),
        }
    }

    pub fn from_wire(kind: u8, id: u64) -> Option<Share> {
        Some(match kind {
            0 => Share::Screen,
            1 => Share::Window(id),
            2 => Share::App(id),
            3 => Share::ActiveWindow,
            4 => Share::ActiveApp,
            _ => return None,
        })
    }
}
