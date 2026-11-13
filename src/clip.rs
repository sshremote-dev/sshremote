//! Clipboard shared between viewer and agent. Each side watches its own
//! clipboard on a separate thread (opening a clipboard another program holds
//! blocks and retries) and sends changes; what just arrived from the other
//! side is not sent back. The agent also watches for copied files, and puts
//! files the viewer pasted on its clipboard as files.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(500);
/// Larger texts are not mirrored; they would stall the screen stream.
const MAX: usize = 1 << 20;

enum Req {
    /// Puts text on the clipboard, saying so when done.
    Set(String, Sender<()>),
    /// Holds text on the clipboard just long enough to paste it, then puts the old text back.
    Paste(String, Sender<bool>),
    /// Puts files on the clipboard, as a file manager's copy does; true once they are there.
    Files(Vec<PathBuf>, Sender<bool>),
}

/// What was copied.
pub enum Copied {
    Text(String),
    /// Files and folders, by their paths.
    Files(Vec<PathBuf>),
}

pub struct Clip {
    to_board: Sender<Req>,
    changed: Receiver<Copied>,
}

/// Files on this computer's clipboard now (copied in its file manager), if any.
pub fn files_here() -> Vec<PathBuf> {
    arboard::Clipboard::new().ok().and_then(|mut b| b.get().file_list().ok()).unwrap_or_default()
}

/// Puts files on this computer's clipboard, for its file manager to paste.
pub fn put_files_here(paths: &[PathBuf]) -> Result<(), String> {
    arboard::Clipboard::new().and_then(|mut b| b.set().file_list(paths)).map_err(|e| e.to_string())
}

fn mark_files(paths: &[PathBuf]) -> (usize, u64) {
    mark(&paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n"))
}

/// What was on the clipboard before a paste put text there, to put back after it.
enum Before {
    Text(String),
    Image(arboard::ImageData<'static>),
    /// Something this program cannot hold (files, an empty board): left as the paste leaves it.
    Other,
}

impl Before {
    fn take(board: &mut arboard::Clipboard) -> Before {
        if let Ok(t) = board.get_text() {
            return Before::Text(t);
        }
        match board.get_image() {
            Ok(i) => Before::Image(i.to_owned_img()),
            Err(_) => Before::Other,
        }
    }

    fn put_back(self, board: &mut arboard::Clipboard) -> Option<String> {
        match self {
            Before::Text(t) => board.set_text(t.clone()).is_ok().then_some(t),
            Before::Image(i) => {
                let _ = board.set_image(i);
                None
            }
            Before::Other => None,
        }
    }
}

/// A short mark of a text: its length and a hash, to know it again without keeping it. A text too
/// large to send is marked by its two ends only: it is never sent, so a change in its middle
/// matters to nobody, and it is not hashed whole twice a second.
fn mark(text: &str) -> (usize, u64) {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let b = text.as_bytes();
    if b.len() <= MAX {
        h.write(b);
    } else {
        h.write(&b[..4096]);
        h.write(&b[b.len() - 4096..]);
    }
    (b.len(), h.finish())
}

impl Clip {
    /// Text both ways, as a viewer keeps it.
    pub fn new() -> Clip {
        Clip::watching(false)
    }

    /// Text both ways and copied files, as an agent keeps it.
    pub fn with_files() -> Clip {
        Clip::watching(true)
    }

    fn watching(files: bool) -> Clip {
        // An agent started over ssh needs the desktop session's display; Xwayland mirrors its clipboard.
        #[cfg(target_os = "linux")]
        crate::capture::ensure_session_env();
        let (to_board, incoming) = mpsc::channel::<Req>();
        let (notify, changed) = mpsc::channel();
        thread::spawn(move || {
            let Ok(mut board) = arboard::Clipboard::new() else { return };
            // What was on the clipboard before the session started is not news; a text too large
            // to send is known by its mark, so it is not read and weighed again every poll.
            let mut last = board.get_text().ok().map(|t| mark(&t));
            // Text put there only to be pasted, what to put back afterwards, and when.
            let mut restore: Option<(Before, (usize, u64), std::time::Instant)> = None;
            loop {
                // Requests fall through too, so a steady stream of them cannot hold off the restore.
                match incoming.recv_timeout(POLL) {
                    Ok(Req::Set(text, done)) => {
                        if board.set_text(text.clone()).is_ok() {
                            last = Some(mark(&text));
                        }
                        let _ = done.send(());
                    }
                    Ok(Req::Files(paths, done)) => {
                        let ok = board.set().file_list(&paths).is_ok();
                        if ok {
                            last = Some(mark_files(&paths));
                        }
                        let _ = done.send(ok);
                    }
                    Ok(Req::Paste(text, done)) => {
                        // A paste right after another keeps what was there before the first one.
                        let before = match restore.take() {
                            Some((b, _, _)) => b,
                            None => Before::take(&mut board),
                        };
                        let ok = board.set_text(text.clone()).is_ok();
                        last = Some(mark(&text));
                        restore = Some((before, mark(&text), std::time::Instant::now()));
                        let _ = done.send(ok);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if restore.as_ref().is_some_and(|r| r.2.elapsed() > Duration::from_secs(1)) {
                    let (before, pasted, _) = restore.take().expect("checked");
                    // Only if nothing was copied since; a fresh copy is the user's and stays.
                    if board.get_text().ok().is_some_and(|t| mark(&t) == pasted) {
                        let put = before.put_back(&mut board);
                        last = put.map(|t| mark(&t)).or(last);
                    }
                }
                // Copied files first: a file manager may offer their paths as text as well.
                if let Some(paths) = files.then(|| board.get().file_list().ok()).flatten().filter(|p| !p.is_empty()) {
                    let m = mark_files(&paths);
                    if last != Some(m) {
                        last = Some(m);
                        if notify.send(Copied::Files(paths)).is_err() {
                            return;
                        }
                    }
                } else if let Ok(text) = board.get_text() {
                    let m = mark(&text);
                    if last != Some(m) {
                        last = Some(m);
                        if text.len() <= MAX && notify.send(Copied::Text(text)).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Clip { to_board, changed }
    }

    /// What was copied locally since the last call.
    pub fn poll(&mut self) -> Option<Copied> {
        self.changed.try_iter().last()
    }

    /// Puts files on the local clipboard as files; true once they are there.
    pub fn set_files(&mut self, paths: Vec<PathBuf>) -> bool {
        let (tx, rx) = mpsc::channel();
        self.to_board.send(Req::Files(paths, tx)).is_ok() && rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
    }

    /// Puts text from the other side on the local clipboard.
    pub fn set(&mut self, text: String) {
        // Waited for (a little), so the Ctrl+V a viewer sends right after pastes this text.
        let (tx, rx) = mpsc::channel();
        if self.to_board.send(Req::Set(text, tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
    }

    /// Puts text on the clipboard for a paste; true once it is there.
    pub fn stage_paste(&mut self, text: String) -> bool {
        let (tx, rx) = mpsc::channel();
        self.to_board.send(Req::Paste(text, tx)).is_ok() && rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same text gets the same mark, another text another; a huge one is marked by its ends.
    #[test]
    fn marks_tell_texts_apart() {
        assert_eq!(mark("hello"), mark("hello"));
        assert_ne!(mark("hello"), mark("hellp"));
        let big = "x".repeat(MAX + 10);
        let mut other = big.clone();
        other.replace_range(..1, "y");
        assert_eq!(mark(&big).0, MAX + 10);
        assert_ne!(mark(&big), mark(&other), "a change at an end shows");
    }
}
