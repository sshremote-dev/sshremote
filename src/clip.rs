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

enum Req {
    Set(String),
    /// Holds text on the clipboard just long enough to paste it, then puts the old text back.
    Paste(String, Sender<bool>),
}

pub struct Clip {
    to_board: Sender<Req>,
    changed: Receiver<String>,
}

impl Clip {
    pub fn new() -> Clip {
        // An agent started over ssh needs the desktop session's display; Xwayland mirrors its clipboard.
        #[cfg(target_os = "linux")]
        crate::capture::ensure_session_env();
        let (to_board, incoming) = mpsc::channel::<Req>();
        let (notify, changed) = mpsc::channel();
        thread::spawn(move || {
            let Ok(mut board) = arboard::Clipboard::new() else { return };
            // What was on the clipboard before the session started is not news.
            let mut last = board.get_text().ok();
            // Text put there only to be pasted, and what to put back afterwards.
            let mut restore: Option<(String, String, std::time::Instant)> = None;
            loop {
                // Requests fall through too, so a steady stream of them cannot hold off the restore.
                match incoming.recv_timeout(POLL) {
                    Ok(Req::Set(text)) => {
                        if board.set_text(text.clone()).is_ok() {
                            last = Some(text);
                        }
                    }
                    Ok(Req::Paste(text, done)) => {
                        // A paste right after another keeps the text from before the first one.
                        let old = match restore.take() {
                            Some((old, _, _)) => old,
                            None => board.get_text().unwrap_or_default(),
                        };
                        let ok = board.set_text(text.clone()).is_ok();
                        last = Some(text.clone());
                        restore = Some((old, text, std::time::Instant::now()));
                        let _ = done.send(ok);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if restore.as_ref().is_some_and(|r| r.2.elapsed() > Duration::from_secs(1)) {
                    let (old, pasted, _) = restore.take().unwrap();
                    // Only if nothing was copied since; a fresh copy is the user's and stays.
                    if board.get_text().ok().as_deref() == Some(pasted.as_str()) && board.set_text(old.clone()).is_ok()
                    {
                        last = Some(old);
                    }
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
        let _ = self.to_board.send(Req::Set(text));
    }

    /// Puts text on the clipboard for a paste; true once it is there.
    pub fn stage_paste(&mut self, text: String) -> bool {
        let (tx, rx) = mpsc::channel();
        self.to_board.send(Req::Paste(text, tx)).is_ok() && rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
    }
}
