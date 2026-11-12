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
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
            .collect();
        enc.write_header().and_then(|mut w| w.write_image_data(&g)).map_err(|e| e.to_string())?;
    } else {
        enc.set_color(png::ColorType::Rgb);
        enc.write_header().and_then(|mut w| w.write_image_data(rgb)).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Decodes a PNG into RGB: the host's screenshot, which comes from the other end and may be any
/// PNG at all (palette, 1 to 16 bits), so it is checked before anything is allocated for it.
#[cfg_attr(not(feature = "mcp"), allow(dead_code))]
pub fn rgb_from_png(data: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
    let mut d = png::Decoder::new(data);
    // Every kind as 8-bit grey, grey and alpha, RGB or RGBA.
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut dec = d.read_info().map_err(|e| e.to_string())?;
    let (w, h) = (dec.info().width as usize, dec.info().height as usize);
    if w > crate::proto::MAX_SIDE || h > crate::proto::MAX_SIDE || w * h > crate::proto::MAX_PIXELS {
        return Err(format!("a {w}x{h} screenshot is too big"));
    }
    let mut buf = vec![0; dec.output_buffer_size()];
    let info = dec.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h, line) = (info.width as usize, info.height as usize, info.line_size);
    let px = |n: usize| -> Vec<&[u8]> { (0..h).flat_map(|y| buf[y * line..y * line + w * n].chunks(n)).collect() };
    let rgb = match info.color_type {
        png::ColorType::Rgb => px(3).concat(),
        png::ColorType::Rgba => px(4).iter().flat_map(|p| [p[0], p[1], p[2]]).collect(),
        png::ColorType::Grayscale => px(1).iter().flat_map(|p| [p[0], p[0], p[0]]).collect(),
        png::ColorType::GrayscaleAlpha => px(2).iter().flat_map(|p| [p[0], p[0], p[0]]).collect(),
        c => return Err(format!("unsupported PNG colour type {c:?}")),
    };
    Ok((w, h, rgb))
}

/// The rectangle `(x, y, w, h)` of an RGB image, clamped to it.
#[cfg_attr(not(feature = "mcp"), allow(dead_code))]
pub fn crop(w: usize, h: usize, rgb: &[u8], r: (usize, usize, usize, usize)) -> (usize, usize, Vec<u8>) {
    if w == 0 || h == 0 {
        return (0, 0, Vec::new());
    }
    let (x, y) = (r.0.min(w.saturating_sub(1)), r.1.min(h.saturating_sub(1)));
    let (cw, ch) = (r.2.clamp(1, w - x), r.3.clamp(1, h - y));
    let mut out = Vec::with_capacity(cw * ch * 3);
    for row in y..y + ch {
        out.extend_from_slice(&rgb[(row * w + x) * 3..(row * w + x + cw) * 3]);
    }
    (cw, ch, out)
}

/// Box-filtered downscale so the width is at most `max_w` (text stays more legible than with nearest).
#[cfg_attr(not(feature = "mcp"), allow(dead_code))]
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
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
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

    /// Any PNG a host sends decodes, whatever its kind, or is refused; none panics.
    #[test]
    fn screenshots_of_every_kind_decode() {
        let encode = |w: u32, h: u32, colour: png::ColorType, depth: png::BitDepth, palette: Option<Vec<u8>>, data: &[u8]| {
            let mut out = Vec::new();
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(colour);
            e.set_depth(depth);
            if let Some(p) = palette {
                e.set_palette(p);
            }
            e.write_header().unwrap().write_image_data(data).unwrap();
            out
        };
        // 1-bit grey, 10 pixels a row: two bytes per row.
        let one_bit = encode(10, 2, png::ColorType::Grayscale, png::BitDepth::One, None, &[0b1010_0000, 0, 0xff, 0xc0]);
        let (w, h, rgb) = rgb_from_png(&one_bit).unwrap();
        assert_eq!((w, h, rgb.len()), (10, 2, 60));
        assert_eq!((&rgb[0..3], &rgb[3..6], &rgb[30..33]), (&[255u8; 3][..], &[0u8; 3][..], &[255u8; 3][..]));
        let palette = encode(2, 1, png::ColorType::Indexed, png::BitDepth::Eight, Some(vec![10, 20, 30, 40, 50, 60]), &[1, 0]);
        assert_eq!(rgb_from_png(&palette).unwrap().2, vec![40, 50, 60, 10, 20, 30]);
        let deep = encode(1, 1, png::ColorType::Rgb, png::BitDepth::Sixteen, None, &[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        assert_eq!(rgb_from_png(&deep).unwrap().2, vec![0x12, 0x56, 0x9a]);
        assert!(rgb_from_png(b"not a png").is_err());
        assert_eq!(crop(0, 0, &[], (0, 0, 5, 5)), (0, 0, Vec::new()));
    }

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
