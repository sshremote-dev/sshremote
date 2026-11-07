//! Screenshots: PNG encoding and decoding, cropping and downscaling, file names
//! and base64, shared by the agent, both viewers and the MCP server.

use crate::capture::Image;

/// Encodes a BGRx capture as an RGB PNG.
pub fn png_from_bgrx(img: &Image) -> Result<Vec<u8>, String> {
    let mut rgb = Vec::with_capacity(img.w * img.h * 3);
    for y in 0..img.h {
        for p in img.data[y * img.stride..y * img.stride + img.w * 4].as_chunks::<4>().0 {
            rgb.extend_from_slice(&[p[2], p[1], p[0]]);
        }
    }
    png_from_rgb(img.w, img.h, &rgb, false)
}

/// Encodes RGB pixels, or grey when `grey` (a third of the size, and a third of the tokens' bytes).
pub fn png_from_rgb(w: usize, h: usize, rgb: &[u8], grey: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    if grey {
        enc.set_color(png::ColorType::Grayscale);
        let g: Vec<u8> = rgb
            .as_chunks::<3>().0.iter()
            .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
            .collect();
        enc.write_header().and_then(|mut w| w.write_image_data(&g)).map_err(|e| e.to_string())?;
    } else {
        enc.set_color(png::ColorType::Rgb);
        enc.write_header().and_then(|mut w| w.write_image_data(rgb)).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Decodes a PNG made by `png_from_rgb` (RGB or grey) into RGB.
pub fn rgb_from_png(data: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
    let mut dec = png::Decoder::new(data).read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; dec.output_buffer_size()];
    let info = dec.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf[..w * h * 3].to_vec(),
        png::ColorType::Rgba => buf[..w * h * 4].as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect(),
        png::ColorType::Grayscale => buf[..w * h].iter().flat_map(|&g| [g, g, g]).collect(),
        c => return Err(format!("unsupported PNG colour type {c:?}")),
    };
    Ok((w, h, rgb))
}

/// The rectangle `(x, y, w, h)` of an RGB image, clamped to it.
pub fn crop(w: usize, h: usize, rgb: &[u8], r: (usize, usize, usize, usize)) -> (usize, usize, Vec<u8>) {
    let (x, y) = (r.0.min(w.saturating_sub(1)), r.1.min(h.saturating_sub(1)));
    let (cw, ch) = (r.2.clamp(1, w - x), r.3.clamp(1, h - y));
    let mut out = Vec::with_capacity(cw * ch * 3);
    for row in y..y + ch {
        out.extend_from_slice(&rgb[(row * w + x) * 3..(row * w + x + cw) * 3]);
    }
    (cw, ch, out)
}

/// Box-filtered downscale so the width is at most `max_w` (text stays more legible than with nearest).
pub fn shrink(w: usize, h: usize, rgb: &[u8], max_w: usize) -> (usize, usize, Vec<u8>) {
    if w <= max_w || max_w == 0 {
        return (w, h, rgb.to_vec());
    }
    let (nw, nh) = (max_w, (h * max_w / w).max(1));
    let mut out = vec![0u8; nw * nh * 3];
    for y in 0..nh {
        let (y0, y1) = (y * h / nh, ((y + 1) * h / nh).max(y * h / nh + 1));
        for x in 0..nw {
            let (x0, x1) = (x * w / nw, ((x + 1) * w / nw).max(x * w / nw + 1));
            let mut acc = [0u32; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let o = (sy * w + sx) * 3;
                    for c in 0..3 {
                        acc[c] += rgb[o + c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            for c in 0..3 {
                out[(y * nw + x) * 3 + c] = (acc[c] / n) as u8;
            }
        }
    }
    (nw, nh, out)
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// `sshremote-YYYYMMDD-HHMMSS.png` in UTC, under the user's Pictures folder (Downloads if there is none).
pub fn save_path() -> std::path::PathBuf {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let name = format!("sshremote-{y:04}{m:02}{d:02}-{:02}{:02}{:02}.png", rem / 3600, rem % 3600 / 60, rem % 60);
    let downloads = crate::files::downloads();
    let pictures = downloads.parent().map(|h| h.join("Pictures")).filter(|p| p.is_dir());
    pictures.unwrap_or(downloads).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip_and_shrink() {
        let (w, h) = (40, 20);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
        let png = png_from_rgb(w, h, &rgb, false).unwrap();
        assert_eq!(rgb_from_png(&png).unwrap(), (w, h, rgb.clone()));
        let (sw, sh, small) = shrink(w, h, &rgb, 10);
        assert_eq!((sw, sh, small.len()), (10, 5, 150));
        let (cw, ch, _) = crop(w, h, &rgb, (30, 10, 50, 50));
        assert_eq!((cw, ch), (10, 10));
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
}
