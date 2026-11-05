//! Text clipboard shared between viewer and agent. Each side watches its own
//! clipboard on a separate thread (opening a clipboard another program holds
//! blocks and retries) and sends changes; text that just arrived from the other
//! side is not sent back.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(500);
/// Larger texts are not mirrored; they would stall the screen stream.
const MAX: usize = 1 << 20;

pub struct Clip {
    to_board: Sender<String>,
    changed: Receiver<String>,
}

impl Clip {
    pub fn new() -> Clip {
        #[cfg(target_os = "linux")]
        if std::env::var_os("DISPLAY").is_none() {
            // An agent started over ssh has no display variable; GNOME's Xwayland mirrors the clipboard.
            unsafe { std::env::set_var("DISPLAY", ":0") };
        }
        let (to_board, incoming) = mpsc::channel::<String>();
        let (notify, changed) = mpsc::channel();
        thread::spawn(move || {
            let Ok(mut board) = arboard::Clipboard::new() else { return };
            // What was on the clipboard before the session started is not news.
            let mut last = board.get_text().ok();
            loop {
                match incoming.recv_timeout(POLL) {
                    Ok(text) => {
                        if board.set_text(text.clone()).is_ok() {
                            last = Some(text);
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if let Ok(text) = board.get_text()
                    && text.len() <= MAX
                    && last.as_deref() != Some(text.as_str())
                {
                    last = Some(text.clone());
                    if notify.send(text).is_err() {
                        return;
                    }
                }
            }
        });
        Clip { to_board, changed }
    }

    /// Text copied locally since the last call.
    pub fn poll(&mut self) -> Option<String> {
        self.changed.try_iter().last()
    }

    /// Puts text from the other side on the local clipboard.
    pub fn set(&mut self, text: String) {
        let _ = self.to_board.send(text);
    }
}
