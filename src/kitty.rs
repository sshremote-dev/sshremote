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

/// The cell that stands for an image's cell in text (Unicode placeholders), and the marks that
/// number its row and column (kitty's rowcolumn-diacritics.txt): through tmux an image goes as
/// these, which tmux keeps in its panes as any text, so the picture moves and hides with them.
const PLACEHOLDER: char = '\u{10EEEE}';
const MARKS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

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
    /// With `text`, the image is placed by placeholders written there (from the grid's top left).
    pub fn place(&mut self, out: &mut Vec<u8>, rgb: &[u8], w: usize, h: usize, (cols, rows): (usize, usize), (c0, c1, r0, r1): (usize, usize, usize, usize), text: Option<&mut Vec<u8>>) {
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
        // An id in text is a 24-bit colour.
        self.next = if self.next >= 0xff_ffff { 1 } else { self.next + 1 };
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let mut parts = b64.as_bytes().chunks(CHUNK).peekable();
        let mut first = true;
        let (pc, pr) = (c1.min(cols) - c0.min(c1), r1.min(rows) - r0.min(r1));
        let place = match &text {
            Some(_) => format!("U=1,c={pc},r={pr}"),
            None => "C=1".to_string(),
        };
        while let Some(part) = parts.next() {
            let more = u8::from(parts.peek().is_some());
            if first {
                out.extend_from_slice(format!("\x1b_Ga=T,f=100,i={id},{place},q=2,m={more};").as_bytes());
                first = false;
            } else {
                out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
            }
            out.extend_from_slice(part);
            out.extend_from_slice(b"\x1b\\");
        }
        if let Some(text) = text {
            // Its id as the colour; the first cell of each row says the row and column, those
            // after it follow on.
            use std::io::Write;
            let _ = write!(text, "\x1b[38;2;{};{};{}m", (id >> 16) & 0xff, (id >> 8) & 0xff, id & 0xff);
            for (k, r) in (r0..r1.min(rows)).enumerate().take(MARKS.len()) {
                let _ = write!(text, "\x1b[{};{}H{PLACEHOLDER}", r + 1, c0 + 1);
                text.extend(char::from_u32(MARKS[k]).map(|m| m.to_string()).unwrap_or_default().as_bytes());
                text.extend(char::from_u32(MARKS[0]).map(|m| m.to_string()).unwrap_or_default().as_bytes());
                text.extend(PLACEHOLDER.to_string().repeat(pc.saturating_sub(1)).as_bytes());
            }
            text.extend_from_slice(b"\x1b[39m");
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
        k.place(&mut out, &rgb, w, h, (20, 5), (0, 20, 0, 5), None);
        assert!(out.windows(5).filter(|w| w == b"\x1b_Gm=").count() >= 2, "several parts");
        assert_eq!(parse(&out), (vec![(1, (200, 100))], vec![]));
        // Half of it covered: it stays.
        out.clear();
        k.place(&mut out, &rgb[..100 * 50 * 3], 100, 50, (20, 5), (0, 10, 0, 5), None);
        assert_eq!(parse(&out).1, Vec::<u32>::new());
        // The other half: the first goes; the second is still in view.
        out.clear();
        k.place(&mut out, &rgb[..100 * 50 * 3], 100, 50, (20, 5), (10, 20, 0, 5), None);
        assert_eq!(parse(&out).1, vec![1]);
        assert_eq!(k.cells.len(), 2);
        // Another grid (the terminal resized): all of them go.
        out.clear();
        k.place(&mut out, &rgb[..10 * 10 * 3], 10, 10, (30, 5), (0, 1, 0, 1), None);
        assert!(out.starts_with(CLEAR));
        assert_eq!(k.cells.len(), 1);
    }

    /// Through tmux: the image is sent unplaced, and its cells written as placeholders, its id
    /// in the colour, each row's first cell marked with its row and column 0.
    #[test]
    fn through_tmux_an_image_is_placed_by_placeholders() {
        let mut k = Kitty::new();
        k.next = 0x01_0203;
        let rgb = vec![9u8; 40 * 40 * 3];
        let (mut out, mut text) = (Vec::new(), Vec::new());
        k.place(&mut out, &rgb, 40, 40, (20, 10), (4, 8, 2, 4), Some(&mut text));
        let apc = String::from_utf8(out).unwrap();
        assert!(apc.starts_with("\x1b_Ga=T,f=100,i=66051,U=1,c=4,r=2,q=2"), "{apc:.60}");
        let text = String::from_utf8(text).unwrap();
        let mark = |n: usize| char::from_u32(MARKS[n]).unwrap();
        let row = |r: usize, n: usize| format!("\x1b[{};5H{PLACEHOLDER}{}{}{}", r + 1, mark(n), mark(0), PLACEHOLDER.to_string().repeat(3));
        assert_eq!(text, format!("\x1b[38;2;1;2;3m{}{}\x1b[39m", row(2, 0), row(3, 1)));
    }

    #[test]
    fn a_terminal_that_shows_kitty_images_is_told_from_one_that_does_not() {
        assert!(answered(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"));
        assert!(!answered(b"\x1b[?62;4;22c"));
        assert!(!answered(b"\x1b_Gi=31;ENOTSUPPORTED:x\x1b\\\x1b[?62c"));
    }
}
