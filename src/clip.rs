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
    /// Puts a picture (PNG) on the clipboard; true once it is there.
    Image(Vec<u8>, Sender<bool>),
}

/// A copied picture larger than this, as PNG, does not go over by itself (it would hold up the
/// screen): it is pasted by hand as a file instead.
pub const IMAGE_MAX: usize = 4 << 20;
/// Pictures are looked at every this many polls: reading one is costlier than text.
const IMAGE_EVERY: u32 = 3;

/// What was copied.
pub enum Copied {
    Text(String),
    /// Files and folders, by their paths.
    Files(Vec<PathBuf>),
    /// A picture (a screenshot), as PNG.
    Image(Vec<u8>),
}

/// A picture's mark: its size and a hash of its pixels.
fn mark_image(w: usize, h: usize, rgba: &[u8]) -> (usize, u64) {
    use std::hash::Hasher;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    hash.write_usize(w);
    hash.write(rgba);
    (w * h * 4 + 1, hash.finish())
}

/// RGBA pixels as PNG, quickly (a screenshot of a large screen takes a moment otherwise).
fn to_png(w: usize, h: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive)
        .write_image(rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

/// A PNG as the clipboard's pixels: one the other side sent, so no larger than it may send
/// (`IMAGE_MAX`) and no larger in pixels than a screen of 16384 a side (a small file can say huge).
fn from_png(png: &[u8]) -> Option<arboard::ImageData<'static>> {
    if png.len() > IMAGE_MAX {
        return None;
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(crate::proto::MAX_SIDE as u32);
    limits.max_image_height = Some(crate::proto::MAX_SIDE as u32);
    limits.max_alloc = Some((crate::proto::MAX_PIXELS * 4) as u64);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    if img.width() as usize * img.height() as usize > crate::proto::MAX_PIXELS {
        return None;
    }
    let img = img.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Some(arboard::ImageData { width: w, height: h, bytes: std::borrow::Cow::Owned(img.into_raw()) })
}

/// The picture on this computer's clipboard now, if it holds one (and no text): its mark, and
/// it as PNG.
pub fn image_png() -> Option<((usize, u64), Vec<u8>)> {
    let mut b = arboard::Clipboard::new().ok()?;
    if b.get_text().is_ok_and(|t| !t.is_empty()) {
        return None;
    }
    let img = b.get_image().ok()?;
    let m = mark_image(img.width, img.height, &img.bytes);
    Some((m, to_png(img.width, img.height, &img.bytes)?))
}

/// The mark of the picture on this computer's clipboard now, if it holds one.
pub fn image_mark() -> Option<(usize, u64)> {
    let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
    Some(mark_image(img.width, img.height, &img.bytes))
}

pub struct Clip {
    to_board: Sender<Req>,
    changed: Receiver<Copied>,
}

/// Files on this computer's clipboard now (copied in its file manager), if any.
pub fn files_here() -> Vec<PathBuf> {
    arboard::Clipboard::new().ok().and_then(|mut b| b.get().file_list().ok()).unwrap_or_default()
}

/// Whether this computer's clipboard holds text.
pub fn text_here() -> bool {
    arboard::Clipboard::new().ok().and_then(|mut b| b.get_text().ok()).is_some_and(|t| !t.is_empty())
}

/// A picture on this computer's clipboard (a screenshot), kept as a PNG file in a folder of its
/// own (`files::drop_dir`, cleared after a day) to be pasted as a file; None without one.
pub fn image_here() -> Option<PathBuf> {
    let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
    let pixels = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())?;
    let now = crate::sys::now_secs();
    let dir = crate::files::drop_dir(&format!("{now}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let stamp: String = crate::sys::local_time(now).chars().filter(|c| c.is_ascii_digit()).collect();
    let path = dir.join(format!("screenshot-{stamp}.png"));
    pixels.save_with_format(&path, image::ImageFormat::Png).ok()?;
    Some(path)
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

    /// Text both ways, copied files and pictures, as an agent keeps it.
    pub fn with_files() -> Clip {
        Clip::watching(true)
    }

    /// `files`: copied files and pictures are watched too (an agent's; a viewer sends its own
    /// files and pictures only when they are pasted there).
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
            if files && last.is_none() {
                last = board.get_image().ok().map(|i| mark_image(i.width, i.height, &i.bytes));
            }
            let mut polls = 0u32;
            // When to look at a picture next, and how many looks found the same one.
            let (mut next_image, mut same) = (0u32, 0u32);
            #[cfg(windows)]
            let mut seen_seq = 0u32;
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
                    Ok(Req::Image(png, done)) => {
                        let ok = match from_png(&png) {
                            Some(img) => {
                                let m = mark_image(img.width, img.height, &img.bytes);
                                let set = board.set_image(img);
                                if let Err(e) = &set {
                                    eprintln!("clipboard: the picture was not taken: {e}");
                                }
                                if set.is_ok() {
                                    last = Some(m);
                                }
                                set.is_ok()
                            }
                            None => {
                                eprintln!("clipboard: not a PNG picture");
                                false
                            }
                        };
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
                // Windows counts each change of the clipboard: nothing is read until it changes.
                #[cfg(windows)]
                {
                    let seq = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
                    if seq != 0 && seq == seen_seq && restore.is_none() {
                        polls = polls.wrapping_add(1);
                        continue;
                    }
                    seen_seq = seq;
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
                // (Windows reads it only when it changed: no waiting longer there.)
                } else if files && polls.is_multiple_of(IMAGE_EVERY) && (cfg!(windows) || polls >= next_image)
                    && let Ok(img) = board.get_image()
                {
                    // A picture copied (a screenshot): over by itself when it is not too large.
                    // The same one again is looked at less and less often (each look reads it all).
                    let m = mark_image(img.width, img.height, &img.bytes);
                    if last == Some(m) {
                        same = (same + 1).min(10);
                    } else {
                        same = 0;
                    }
                    next_image = polls.wrapping_add(IMAGE_EVERY * (1 << same.min(4)));
                    if last != Some(m) {
                        last = Some(m);
                        // Too large to bring over: told as no picture, so the other side knows
                        // its own clipboard is older and does not paste it over this one.
                        let png = (img.width * img.height <= 40_000_000).then(|| to_png(img.width, img.height, &img.bytes)).flatten().filter(|p| p.len() <= IMAGE_MAX);
                        if notify.send(Copied::Image(png.unwrap_or_default())).is_err() {
                            return;
                        }
                    }
                }
                polls = polls.wrapping_add(1);
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

    /// Puts a picture (PNG) from the other side on the local clipboard, not waited for: what
    /// comes on the channel says whether it is there.
    pub fn set_image_later(&mut self, png: Vec<u8>) -> Receiver<bool> {
        let (tx, rx) = mpsc::channel();
        let _ = self.to_board.send(Req::Image(png, tx));
        rx
    }

    /// Puts a picture (PNG) from the other side on the local clipboard; true once it is there.
    pub fn set_image(&mut self, png: Vec<u8>) -> bool {
        let (tx, rx) = mpsc::channel();
        self.to_board.send(Req::Image(png, tx)).is_ok() && rx.recv_timeout(Duration::from_secs(3)).unwrap_or(false)
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
    fn pictures_go_as_png_and_come_back_the_same() {
        let (w, h) = (40usize, 30usize);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let png = to_png(w, h, &rgba).unwrap();
        let back = from_png(&png).unwrap();
        assert_eq!((back.width, back.height), (w, h));
        assert_eq!(mark_image(back.width, back.height, &back.bytes), mark_image(w, h, &rgba), "the same picture is known again");
        let mut other = rgba.clone();
        other[5] ^= 1;
        assert_ne!(mark_image(w, h, &other), mark_image(w, h, &rgba));
    }

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
