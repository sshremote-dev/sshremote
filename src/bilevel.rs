//! Black and white for the lowest quality. Bit-packed tiles in the session's zstd stream were also
//! measured against a JBIG2-style context-modelled coder: with 32 px tiles zstd, which finds repeated
//! glyphs across the whole stream, came out 30-50% smaller, so only the binarization lives here.

/// Black and white for screens: where the neighbourhood has contrast (text, lines, icons) a pixel
/// takes the side of the local mean, so grey text on white and light text on dark panels both
/// survive; flat areas keep the plain threshold, so dark panels stay dark.
pub fn binarize(luma: &[u8], w: usize, h: usize) -> Vec<u8> {
    const R: usize = 6;
    const FLAT: i32 = 12;
    let iw = w + 1;
    let mut sum = vec![0u32; iw * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += luma[y * w + x] as u32;
            sum[(y + 1) * iw + x + 1] = sum[y * iw + x + 1] + row;
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(R), (y + R + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(R), (x + R + 1).min(w));
            let total = sum[y1 * iw + x1] + sum[y0 * iw + x0] - sum[y0 * iw + x1] - sum[y1 * iw + x0];
            let mean = (total / ((y1 - y0) * (x1 - x0)) as u32) as i32;
            let l = luma[y * w + x] as i32;
            let d = l - mean;
            out[y * w + x] = if d.abs() < FLAT { (l >= 128) as u8 } else { (d > 0) as u8 };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_text_on_dark_survives() {
        // A dark panel (luma 40) with a light line (luma 200) and a white page (250) with grey text (150).
        let (w, h) = (40, 20);
        let mut l = vec![40u8; w * h];
        for x in 0..w {
            l[5 * w + x] = 200;
        }
        for y in 10..h {
            for x in 0..w {
                l[y * w + x] = if y == 15 { 150 } else { 250 };
            }
        }
        let b = binarize(&l, w, h);
        assert_eq!(b[2 * w + 20], 0, "dark panel stays dark");
        assert_eq!(b[5 * w + 20], 1, "light line on it shows");
        assert_eq!(b[12 * w + 20], 1, "white page stays white");
        assert_eq!(b[15 * w + 20], 0, "grey text on it shows");
    }
}
