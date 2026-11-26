//! H.265 films decoded by the viewer's own system, for a viewer that chose H.265: Media
//! Foundation's decoder on Windows (with the HEVC Video Extensions), VideoToolbox on macOS, and
//! GStreamer's decoders on Linux. A system without one never asks for H.265 (`can_h265`).

#[cfg(windows)]
mod mf;
#[cfg(target_os = "macos")]
mod vt;
#[cfg(target_os = "linux")]
mod gst;

/// A decoder of H.265 access units (Annex B) into RGB pictures.
pub trait Decoder: Send {
    /// Decodes one access unit; whether `rgb` (3 bytes a pixel, rows of `w`) holds a new
    /// picture of `w`x`h`.
    fn decode(&mut self, au: &[u8], w: usize, h: usize, rgb: &mut Vec<u8>) -> Result<bool, String>;
}

/// Whether this computer decodes H.265 (looked at once).
pub fn can_h265() -> bool {
    static CAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN.get_or_init(|| {
        #[cfg(windows)]
        return mf::has();
        #[cfg(target_os = "macos")]
        return vt::has();
        #[cfg(target_os = "linux")]
        return gst::has();
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        false
    })
}

pub fn open_h265() -> Result<Box<dyn Decoder>, String> {
    #[cfg(windows)]
    return mf::open();
    #[cfg(target_os = "macos")]
    return vt::open();
    #[cfg(target_os = "linux")]
    return gst::open();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    Err("H.265 is not decoded on this system".into())
}

/// NV12 (BT.601, limited range, as the encoders take it) to RGB.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn nv12_to_rgb(y: &[u8], ys: usize, uv: &[u8], uvs: usize, w: usize, h: usize, rgb: &mut Vec<u8>) {
    rgb.resize(w * h * 3, 0);
    for r in 0..h {
        let (yrow, crow) = (&y[r * ys..], &uv[(r / 2) * uvs..]);
        let out = &mut rgb[r * w * 3..(r + 1) * w * 3];
        for c in 0..w {
            let yy = (yrow[c] as i32 - 16).max(0) * 298;
            let (u, v) = (crow[c & !1] as i32 - 128, crow[(c & !1) + 1] as i32 - 128);
            let px = &mut out[c * 3..c * 3 + 3];
            px[0] = ((yy + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            px[1] = ((yy - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            px[2] = ((yy + 516 * u + 128) >> 8).clamp(0, 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_stays_grey() {
        let (y, uv) = (vec![126u8; 4 * 2], vec![128u8; 4]);
        let mut rgb = Vec::new();
        nv12_to_rgb(&y, 4, &uv, 4, 4, 2, &mut rgb);
        assert!(rgb.iter().all(|&c| (127..=129).contains(&c)), "{rgb:?}");
    }

    /// A moving picture through this computer's H.265 hardware encoder and back through its
    /// H.265 decoder, at two sizes, its colour too.
    #[test]
    #[ignore]
    fn h265_films_go_through_the_hardware_and_back() {
        use crate::hwenc::{self, Codec};
        assert!(can_h265(), "this computer decodes H.265");
        for (w, h) in [(896usize, 576usize), (1920, 1080)] {
            let fps = 30u8;
            let mut enc = hwenc::open(Codec::H265, w, h, fps, ((w * h) as f64 * fps as f64 * 0.06) as u32).unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
            let mut dec = open_h265().unwrap();
            let (mut shown, mut bytes, mut psnr, mut times) = (0, 0usize, Vec::new(), Vec::new());
            let mut rgb = Vec::new();
            for f in 0..60usize {
                let (cx, cy) = ((w / 4 + f * w / 120) as i64, (h / 2) as i64);
                let y: Vec<u8> = (0..w * h)
                    .map(|i| {
                        let (x, yy) = ((i % w) as i64, (i / w) as i64);
                        let d = (x - cx) * (x - cx) + (yy - cy) * (yy - cy);
                        if d < (h as i64 / 6).pow(2) { 200 } else { (40 + (x + 2 * yy + f as i64 * 3) / 12 % 120) as u8 }
                    })
                    .collect();
                let (u, v) = (vec![100u8; w * h / 4], vec![150u8; w * h / 4]);
                let au = enc.encode(&y, &u, &v, (w, w / 2, w / 2)).unwrap();
                if f == 0 {
                    assert!(hwenc::has_sps(Codec::H265, &au), "parameter sets first");
                }
                bytes += au.len();
                let t = std::time::Instant::now();
                if dec.decode(&au, w, h, &mut rgb).unwrap() {
                    // This picture, not the one before: the moving disc's front edge is lit.
                    let at = ((h / 2) * w + (w / 4 + f * w / 120 + h / 6 - 3)) * 3;
                    let lum = 16.0 + (65.7 * rgb[at] as f64 + 129.0 * rgb[at + 1] as f64 + 25.0 * rgb[at + 2] as f64) / 255.0;
                    assert!(lum > 170.0, "picture {f} came out a picture late ({lum:.0})");
                    times.push(t.elapsed().as_secs_f64() * 1000.0);
                    shown += 1;
                    // Luma back from RGB, against what went in.
                    let mse = (0..h)
                        .step_by(3)
                        .flat_map(|r| (0..w).step_by(3).map(move |c| (r, c)))
                        .map(|(r, c)| {
                            let p = &rgb[(r * w + c) * 3..][..3];
                            let l = 16.0 + (65.7 * p[0] as f64 + 129.0 * p[1] as f64 + 25.0 * p[2] as f64) / 255.0;
                            (l - y[r * w + c] as f64).powi(2)
                        })
                        .sum::<f64>()
                        / ((h / 3 + 1) * (w / 3 + 1)) as f64;
                    psnr.push(10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10());
                }
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mean = psnr.iter().sum::<f64>() / psnr.len().max(1) as f64;
            eprintln!("{} {w}x{h}: {shown}/60 decoded here, {:.0} kbit/s, decoding {:.1} ms median; luma {mean:.1} dB", enc.name(), bytes as f64 * 8.0 / 2.0 / 1000.0, times.get(times.len() / 2).copied().unwrap_or(0.0));
            assert_eq!(shown, 60, "each picture back as it goes in");
            // A software stand-in for the hardware (a Mac without it, in tests) spends far fewer
            // bits than asked: there only the way through is checked.
            assert!(mean > if enc.name().contains("software") { 30.0 } else { 40.0 }, "{mean:.1} dB");
        }
    }
}
