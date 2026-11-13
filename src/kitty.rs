//! The kitty graphics protocol (kitty, Ghostty, WezTerm): the picture in true colour, each
//! repainted rectangle a PNG image of its own, placed at the cursor. An image is deleted once newer
//! ones cover every cell of it, so the terminal keeps no more than what shows.

use std::collections::HashMap;

use base64::Engine;

/// The base64 sent in one escape sequence, as the protocol allows.
const CHUNK: usize = 4096;

pub struct Kitty {
    /// Which image shows in each cell of the picture's rows (0: none), and how many cells each has.
    owner: Vec<u32>,
    cells: HashMap<u32, usize>,
    cols: usize,
    next: u32,
}

/// Deletes every image this program placed, and frees them.
pub const CLEAR: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

impl Kitty {
    pub fn new() -> Kitty {
        Kitty { owner: Vec::new(), cells: HashMap::new(), cols: 0, next: 1 }
    }

    /// Whether any image is placed.
    pub fn any(&self) -> bool {
        !self.cells.is_empty()
    }

    /// Forgets every image; `CLEAR` deletes them on the terminal.
    pub fn clear(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(CLEAR);
        self.owner.clear();
        self.cells.clear();
    }

    /// Appends a `w`x`h` RGB picture placed at the cursor, covering columns `c0..c1` of rows
    /// `r0..r1` of a grid `cols` by `rows` cells; then deletes the images it covers wholly.
    #[allow(clippy::too_many_arguments)]
    pub fn place(&mut self, out: &mut Vec<u8>, rgb: &[u8], w: usize, h: usize, (cols, rows): (usize, usize), (c0, c1, r0, r1): (usize, usize, usize, usize)) {
        if self.owner.len() != cols * rows || self.cols != cols {
            if self.any() {
                out.extend_from_slice(CLEAR);
            }
            self.cells.clear();
            self.owner = vec![0; cols * rows];
            self.cols = cols;
        }
        let Some(png) = png(rgb, w, h) else { return };
        let id = self.next;
        self.next = self.next.checked_add(1).unwrap_or(1);
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let mut parts = b64.as_bytes().chunks(CHUNK).peekable();
        let mut first = true;
        while let Some(part) = parts.next() {
            let more = u8::from(parts.peek().is_some());
            if first {
                out.extend_from_slice(format!("\x1b_Ga=T,f=100,i={id},C=1,q=2,m={more};").as_bytes());
                first = false;
            } else {
                out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
            }
            out.extend_from_slice(part);
            out.extend_from_slice(b"\x1b\\");
        }
        let mut gone = Vec::new();
        for r in r0..r1.min(rows) {
            for c in c0..c1.min(cols) {
                let old = std::mem::replace(&mut self.owner[r * cols + c], id);
                if old != 0
                    && let Some(n) = self.cells.get_mut(&old)
                {
                    *n -= 1;
                    if *n == 0 {
                        gone.push(old);
                    }
                }
            }
        }
        self.cells.insert(id, (r1.min(rows) - r0.min(r1)) * (c1.min(cols) - c0.min(c1)));
        for old in gone {
            self.cells.remove(&old);
            out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={old},q=2\x1b\\").as_bytes());
        }
    }
}

fn png(rgb: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive)
        .write_image(rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// Whether a terminal's answer to `QUERY` says it shows kitty images.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn answered(reply: &[u8]) -> bool {
    reply.windows(9).any(|w| w == b"_Gi=31;OK")
}

/// Asks the terminal whether it shows kitty images, then for its attributes (DA1), which every
/// terminal answers: the second answer ends the wait without a timeout.
#[cfg_attr(not(unix), allow(dead_code))]
pub const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";

#[cfg(test)]
mod tests {
    use super::*;

    /// The images sent, as (id, decoded PNG size) and the ids deleted.
    fn parse(out: &[u8]) -> (Vec<(u32, (u32, u32))>, Vec<u32>) {
        let text = String::from_utf8(out.to_vec()).unwrap();
        let (mut shown, mut deleted) = (Vec::new(), Vec::new());
        let mut b64 = String::new();
        let mut id = 0;
        for seq in text.split("\x1b_G").skip(1) {
            let seq = seq.strip_suffix("\x1b\\").or_else(|| seq.split("\x1b\\").next()).unwrap();
            let (keys, data) = seq.split_once(';').unwrap_or((seq, ""));
            if keys.starts_with("a=d,d=I") {
                deleted.push(keys.split("i=").nth(1).unwrap().split(',').next().unwrap().parse().unwrap());
                continue;
            }
            if let Some(i) = keys.split(',').find_map(|k| k.strip_prefix("i=")) {
                id = i.parse().unwrap();
            }
            b64.push_str(data);
            if keys.ends_with("m=0") {
                let png = base64::engine::general_purpose::STANDARD.decode(std::mem::take(&mut b64)).unwrap();
                let img = image::load_from_memory(&png).unwrap();
                shown.push((id, (img.width(), img.height())));
            }
        }
        (shown, deleted)
    }

    #[test]
    fn images_are_sent_in_parts_and_deleted_once_covered() {
        let mut k = Kitty::new();
        // A large noisy picture, to need several parts.
        let (w, h) = (200, 100);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7919 % 251) as u8).collect();
        let mut out = Vec::new();
        k.place(&mut out, &rgb, w, h, (20, 5), (0, 20, 0, 5));
        assert!(out.windows(5).filter(|w| w == b"\x1b_Gm=").count() >= 2, "several parts");
        assert_eq!(parse(&out), (vec![(1, (200, 100))], vec![]));
        // Half of it covered: it stays.
        out.clear();
        k.place(&mut out, &rgb[..100 * 50 * 3], 100, 50, (20, 5), (0, 10, 0, 5));
        assert_eq!(parse(&out).1, Vec::<u32>::new());
        // The other half: the first goes; the second is still in view.
        out.clear();
        k.place(&mut out, &rgb[..100 * 50 * 3], 100, 50, (20, 5), (10, 20, 0, 5));
        assert_eq!(parse(&out).1, vec![1]);
        assert_eq!(k.cells.len(), 2);
        // Another grid (the terminal resized): all of them go.
        out.clear();
        k.place(&mut out, &rgb[..10 * 10 * 3], 10, 10, (30, 5), (0, 1, 0, 1));
        assert!(out.starts_with(CLEAR));
        assert_eq!(k.cells.len(), 1);
    }

    #[test]
    fn a_terminal_that_shows_kitty_images_is_told_from_one_that_does_not() {
        assert!(answered(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"));
        assert!(!answered(b"\x1b[?62;4;22c"));
        assert!(!answered(b"\x1b_Gi=31;ENOTSUPPORTED:x\x1b\\\x1b[?62c"));
    }
}
