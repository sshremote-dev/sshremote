//! Films encoded by the computer's video hardware rather than its processor, in the H.264 every
//! viewer reads: Media Foundation's hardware encoders on Windows (Intel Quick Sync, NVIDIA NVENC,
//! AMD AMF), VideoToolbox on macOS and GStreamer's VA-API and NVENC elements on Linux. A film goes
//! to the hardware once the processor falls behind it (`video::Video`); AV1 from the processor
//! costs fewer bytes.

#[cfg(windows)]
mod mf;
#[cfg(target_os = "macos")]
mod vt;

/// A hardware encoder for one film's size.
pub trait Encoder: Send {
    /// One picture as I420 planes with their strides: its access unit in Annex B form, the
    /// parameter sets before the first picture.
    fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], strides: (usize, usize, usize)) -> Result<Vec<u8>, String>;
    /// The hardware it runs on.
    fn name(&self) -> String;
}

/// Whether films go to the hardware: `SSHREMOTE_HW_ENCODE=0` never, `1` from a film's start,
/// otherwise once the processor falls behind.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Want {
    Never,
    Always,
    WhenBehind,
}

pub fn wanted() -> Want {
    match std::env::var("SSHREMOTE_HW_ENCODE").as_deref() {
        Ok("0") => Want::Never,
        Ok("1") => Want::Always,
        _ => Want::WhenBehind,
    }
}

/// An encoder for `w`x`h` pictures at `fps` and about `bitrate` bits a second, or why there is
/// none here.
#[allow(unused_variables)]
pub fn open(w: usize, h: usize, fps: u8, bitrate: u32) -> Result<Box<dyn Encoder>, String> {
    #[cfg(windows)]
    return mf::open(w, h, fps, bitrate);
    #[cfg(target_os = "macos")]
    return vt::open(w, h, fps, bitrate);
    #[cfg(not(any(windows, target_os = "macos")))]
    Err("no video hardware encoder is used on this system".into())
}

/// Whether an access unit carries a sequence parameter set (NAL type 7).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn has_sps(au: &[u8]) -> bool {
    au.windows(4).any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 7)
}

/// Whether an access unit carries a key picture (an IDR slice, NAL type 5).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn is_key(au: &[u8]) -> bool {
    au.windows(4).any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sequence_parameter_set_is_found() {
        assert!(has_sps(&[0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68]));
        assert!(!has_sps(&[0, 0, 0, 1, 0x65, 0x88]));
        assert!(is_key(&[0, 0, 0, 1, 0x65, 0x88]) && !is_key(&[0, 0, 0, 1, 0x41, 0x9a]));
    }

    /// A moving picture through the hardware encoder here and back through the viewer's H.264
    /// decoder, at three sizes: its time a picture, its bytes, and how close it comes back.
    #[test]
    #[ignore]
    fn films_go_through_the_hardware() {
        use openh264::decoder::Decoder;
        use openh264::formats::YUVSource;
        for (w, h) in [(896usize, 576usize), (1920, 1080), (3840, 2160)] {
            let fps = 30u8;
            let bitrate = ((w * h) as f64 * fps as f64 * 0.08) as u32;
            let mut enc = match open(w, h, fps, bitrate) {
                Ok(e) => e,
                Err(e) => panic!("{w}x{h}: {e}"),
            };
            let mut dec = Decoder::new().unwrap();
            let (mut times, mut bytes, mut psnr, mut shown) = (Vec::new(), 0usize, Vec::new(), 0usize);
            for f in 0..90usize {
                let (cx, cy) = ((w / 4 + f * w / 200) as i64, (h / 2) as i64);
                let y: Vec<u8> = (0..w * h)
                    .map(|i| {
                        let (x, yy) = ((i % w) as i64, (i / w) as i64);
                        let d = (x - cx) * (x - cx) + (yy - cy) * (yy - cy);
                        if d < (h as i64 / 6).pow(2) { 220 } else { (40 + (x + 2 * yy + f as i64 * 3) / 12 % 120) as u8 }
                    })
                    .collect();
                let u = vec![110u8; w * h / 4];
                let v = vec![140u8; w * h / 4];
                let t = std::time::Instant::now();
                let au = enc.encode(&y, &u, &v, (w, w / 2, w / 2)).unwrap();
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                bytes += au.len();
                if f == 0 {
                    assert!(has_sps(&au), "the first picture carries its parameter sets");
                }
                if let Ok(Some(pic)) = dec.decode(&au) {
                    shown += 1;
                    let (pw, ph) = pic.dimensions();
                    assert_eq!((pw, ph), (w, h));
                    let (ys, _, _) = pic.strides();
                    let mse = (0..h).map(|r| (0..w).map(|c| (pic.y()[r * ys + c] as f64 - y[r * w + c] as f64).powi(2)).sum::<f64>()).sum::<f64>() / (w * h) as f64;
                    psnr.push(10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10());
                    let (_, us, vs) = pic.strides();
                    let mean = |p: &[u8], s: usize| (0..h / 2).map(|r| (0..w / 2).map(|c| p[r * s + c] as f64).sum::<f64>()).sum::<f64>() / (w * h / 4) as f64;
                    let (mu, mv) = (mean(pic.u(), us), mean(pic.v(), vs));
                    assert!((mu - 110.0).abs() < 3.0 && (mv - 140.0).abs() < 3.0, "colour came back as {mu:.1}, {mv:.1}");
                }
            }
            let first = times[0];
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mean = psnr.iter().sum::<f64>() / psnr.len().max(1) as f64;
            eprintln!(
                "{} {w}x{h}: {shown}/90 shown, {:.0} kbit/s, a picture {:.1} ms median, {:.1} ms p90, first {:.1} ms; luma {mean:.1} dB",
                enc.name(),
                bytes as f64 * 8.0 / 3.0 / 1000.0,
                times[45],
                times[81],
                first,
            );
            // The processor's AV1 at the same size, to set against it.
            {
                let mut a = crate::av1::Encoder::new(w, h, fps, bitrate / 2).unwrap();
                let mut st = Vec::new();
                let y: Vec<u8> = (0..w * h).map(|i| ((i % w + i / w) / 7 % 200) as u8).collect();
                let (u, v) = (vec![110u8; w * h / 4], vec![140u8; w * h / 4]);
                for f in 0..30usize {
                    let mut y2 = y.clone();
                    y2.rotate_left(f * 13);
                    let t = std::time::Instant::now();
                    a.encode(&y2, &u, &v, (w, w / 2, w / 2)).unwrap();
                    st.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                st.sort_by(|a, b| a.partial_cmp(b).unwrap());
                eprintln!("  processor AV1 {w}x{h}: {:.1} ms median, {:.1} ms p90", st[15], st[27]);
            }
            assert!(shown >= 88, "{shown} pictures came back");
            assert!(mean > 30.0, "{mean:.1} dB");
        }
    }
}
