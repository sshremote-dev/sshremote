//! AV1 for the video regions, through libaom in its real-time mode. The quantizer never goes
//! below `FLOOR`, so a film keeps one sharpness and spends only what that takes; the bitrate
//! asked for is a ceiling for harder films. On the bench's film as the agent encodes it, 5.8
//! kbit a picture at 42.0 dB, where RustDesk 1.5.3 showed 40.6 dB for 8.0 kbit (its viewer's own
//! pictures, 2026-11-25) and openh264 gave 41.5 dB for 13.2 kbit. Viewers that read it say so (`proto::FORMAT_AV1`); an access unit
//! starts with a temporal delimiter (0x12), never with H.264's start code, so the viewer tells
//! the two apart by the first byte.

use std::mem::MaybeUninit;
use std::ptr;

use libaom_sys::*;

/// The first byte of every access unit libaom writes: a temporal delimiter OBU with its size.
pub const MARK: u8 = 0x12;

/// The finest quantizer a film gets (of 63): sharper than RustDesk's films for fewer bytes
/// (20 gave 43.7 dB for 10.4 kbit a picture, 24 43.0 for 8.0, 32 41.0 for 4.3).
const FLOOR: u32 = 28;

fn err(ctx: *const aom_codec_ctx_t, what: &str, code: aom_codec_err_t) -> String {
    // SAFETY: libaom's error strings are static or kept in the context, NUL-terminated.
    let text = unsafe {
        let detail = if ctx.is_null() { ptr::null() } else { aom_codec_error_detail(ctx) };
        let s = if detail.is_null() { aom_codec_err_to_string(code) } else { detail };
        std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned()
    };
    format!("AV1 {what}: {text}")
}

/// The speed of the first picture of a film, which is coded whole: libaom's fastest, a quarter of
/// the time (5.8 ms instead of 20.5 for the bench's film) for 0.3 dB less in it and the few after
/// it. The film's start is where a viewer waits longest.
const FIRST_SPEED: i32 = 11;

/// The speed of the pictures after: faster on larger ones; both well within a frame's time.
fn speed(w: usize, h: usize) -> i32 {
    if w * h > 1280 * 720 { 10 } else { 9 }
}

/// Threads for a picture of `pixels`: more for larger ones, never more than the computer has.
fn threads(pixels: usize) -> u32 {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
    let want = if pixels >= 1280 * 720 { 4 } else { 2 };
    want.min(cores).max(1)
}

/// A real-time encoder for one region's size.
pub struct Encoder {
    ctx: Box<aom_codec_ctx_t>,
    w: usize,
    h: usize,
    pts: i64,
}

// SAFETY: the context is used from one thread at a time, through &mut self.
unsafe impl Send for Encoder {}

impl Encoder {
    /// An encoder for `w`x`h` (both even) pictures at `fps`, spending at most `bitrate` bits a second.
    pub fn new(w: usize, h: usize, fps: u8, bitrate: u32) -> Result<Encoder, String> {
        // SAFETY: plain C calls on locals; the context is initialised before it is used, and the
        // Box keeps its address while libaom holds pointers into it.
        unsafe {
            let iface = aom_codec_av1_cx();
            let mut cfg = MaybeUninit::<aom_codec_enc_cfg_t>::zeroed();
            let r = aom_codec_enc_config_default(iface, cfg.as_mut_ptr(), AOM_USAGE_REALTIME);
            if r != 0 {
                return Err(err(ptr::null(), "settings", r));
            }
            let mut cfg = cfg.assume_init();
            cfg.g_w = w as u32;
            cfg.g_h = h as u32;
            cfg.g_threads = threads(w * h);
            cfg.g_timebase = aom_rational { num: 1, den: fps.max(1) as i32 };
            cfg.g_lag_in_frames = 0;
            cfg.g_error_resilient = 0;
            cfg.rc_end_usage = AOM_CBR;
            cfg.rc_target_bitrate = (bitrate / 1000).max(30);
            cfg.rc_min_quantizer = FLOOR;
            cfg.rc_max_quantizer = 52;
            cfg.rc_undershoot_pct = 50;
            cfg.rc_overshoot_pct = 50;
            cfg.rc_buf_initial_sz = 600;
            cfg.rc_buf_optimal_sz = 600;
            cfg.rc_buf_sz = 1000;
            // Key frames only when asked for (a viewer that lost its picture, a new region).
            cfg.kf_mode = AOM_KF_DISABLED;
            let mut ctx: Box<aom_codec_ctx_t> = Box::new(MaybeUninit::zeroed().assume_init());
            let r = aom_codec_enc_init_ver(&mut *ctx, iface, &cfg, 0, AOM_ENCODER_ABI_VERSION as i32);
            if r != 0 {
                return Err(err(ptr::null(), "encoder", r));
            }
            let c: *mut aom_codec_ctx_t = &mut *ctx;
            aom_codec_control(c, AOME_SET_CPUUSED as i32, FIRST_SPEED);
            aom_codec_control(c, AV1E_SET_ROW_MT as i32, 1u32);
            aom_codec_control(c, AV1E_SET_TILE_COLUMNS as i32, if w >= 1280 { 1u32 } else { 0u32 });
            aom_codec_control(c, AV1E_SET_AQ_MODE as i32, 3u32);
            aom_codec_control(c, AV1E_SET_ENABLE_CDEF as i32, 1u32);
            aom_codec_control(c, AV1E_SET_ENABLE_ORDER_HINT as i32, 0i32);
            aom_codec_control(c, AV1E_SET_ENABLE_TPL_MODEL as i32, 0u32);
            aom_codec_control(c, AV1E_SET_DELTAQ_MODE as i32, 0u32);
            aom_codec_control(c, AV1E_SET_COEFF_COST_UPD_FREQ as i32, 3u32);
            aom_codec_control(c, AV1E_SET_MODE_COST_UPD_FREQ as i32, 3u32);
            aom_codec_control(c, AV1E_SET_MV_COST_UPD_FREQ as i32, 3u32);
            Ok(Encoder { ctx, w, h, pts: 0 })
        }
    }

    /// Encodes one picture given as I420 planes with their strides; returns its access unit.
    pub fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], strides: (usize, usize, usize)) -> Result<Vec<u8>, String> {
        let (w, h) = (self.w, self.h);
        if y.len() < strides.0 * h || u.len() < strides.1 * h / 2 || v.len() < strides.2 * h / 2 {
            return Err("AV1: a picture smaller than its region".into());
        }
        let mut out = Vec::new();
        // SAFETY: the image only points into the borrowed planes, which outlive the call; the data
        // packets are copied out before the next call into libaom.
        unsafe {
            let mut img = MaybeUninit::<aom_image_t>::zeroed();
            if aom_img_wrap(img.as_mut_ptr(), AOM_IMG_FMT_I420, w as u32, h as u32, 1, y.as_ptr() as *mut u8).is_null() {
                return Err("AV1: the picture was not taken".into());
            }
            let img = img.assume_init_mut();
            img.planes = [y.as_ptr() as *mut u8, u.as_ptr() as *mut u8, v.as_ptr() as *mut u8];
            img.stride = [strides.0 as i32, strides.1 as i32, strides.2 as i32];
            let c: *mut aom_codec_ctx_t = &mut *self.ctx;
            let r = aom_codec_encode(c, img, self.pts, 1, 0);
            if r != 0 {
                return Err(err(c, "encoding", r));
            }
            if self.pts == 0 {
                aom_codec_control(c, AOME_SET_CPUUSED as i32, speed(w, h));
            }
            self.pts += 1;
            let mut it: aom_codec_iter_t = ptr::null();
            loop {
                let p = aom_codec_get_cx_data(c, &mut it);
                if p.is_null() {
                    break;
                }
                if (*p).kind == AOM_CODEC_CX_FRAME_PKT {
                    let f = (*p).data.frame;
                    out.extend_from_slice(std::slice::from_raw_parts(f.buf as *const u8, f.sz));
                }
            }
        }
        Ok(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the context was initialised in `new`, and is destroyed once.
        unsafe { aom_codec_destroy(&mut *self.ctx) };
    }
}

/// A decoder for one region's access units.
pub struct Decoder {
    ctx: Box<aom_codec_ctx_t>,
}

// SAFETY: as the encoder's.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Decoder, String> {
        // SAFETY: as in Encoder::new.
        unsafe {
            let cfg = aom_codec_dec_cfg { w: 0, h: 0, threads: 2, allow_lowbitdepth: 1 };
            let mut ctx: Box<aom_codec_ctx_t> = Box::new(MaybeUninit::zeroed().assume_init());
            let r = aom_codec_dec_init_ver(&mut *ctx, aom_codec_av1_dx(), &cfg, 0, AOM_DECODER_ABI_VERSION as i32);
            if r != 0 {
                return Err(err(ptr::null(), "decoder", r));
            }
            Ok(Decoder { ctx })
        }
    }

    /// Decodes one access unit into `rgb` (3 bytes a pixel, rows of `w`) when it makes a picture
    /// of `w`x`h`; returns whether it did.
    pub fn decode(&mut self, data: &[u8], w: usize, h: usize, rgb: &mut Vec<u8>) -> Result<bool, String> {
        // SAFETY: the data outlives the call; the picture libaom returns is read before the next
        // call into it, within its planes' strides and sizes.
        unsafe {
            let c: *mut aom_codec_ctx_t = &mut *self.ctx;
            let r = aom_codec_decode(c, data.as_ptr(), data.len(), ptr::null_mut());
            if r != 0 {
                return Err(err(c, "decoding", r));
            }
            let mut it: aom_codec_iter_t = ptr::null();
            let mut shown = false;
            loop {
                let img = aom_codec_get_frame(c, &mut it);
                if img.is_null() {
                    break;
                }
                let img = &*img;
                if (img.d_w as usize, img.d_h as usize) != (w, h) {
                    return Err(format!("AV1 picture is {}x{}, expected {w}x{h}", img.d_w, img.d_h));
                }
                if img.fmt != AOM_IMG_FMT_I420 || img.bit_depth != 8 {
                    return Err("AV1: a picture in a form this viewer does not read".into());
                }
                let plane = |i: usize, rows: usize| std::slice::from_raw_parts(img.planes[i], img.stride[i] as usize * rows);
                let (ch, s) = (h.div_ceil(2), (img.stride[0] as usize, img.stride[1] as usize, img.stride[2] as usize));
                i420_to_rgb(plane(0, h), plane(1, ch), plane(2, ch), s, w, h, rgb);
                shown = true;
            }
            Ok(shown)
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: as the encoder's.
        unsafe { aom_codec_destroy(&mut *self.ctx) };
    }
}

/// I420 (BT.601, limited range, as the encoder side's conversion makes it) to RGB.
pub fn i420_to_rgb(y: &[u8], u: &[u8], v: &[u8], strides: (usize, usize, usize), w: usize, h: usize, rgb: &mut Vec<u8>) {
    rgb.resize(w * h * 3, 0);
    // 16.16 fixed point: 255/219, 255/224 times BT.601's chroma factors.
    const Y: i32 = 76309;
    const RV: i32 = 104597;
    const GV: i32 = -53279;
    const GU: i32 = -25675;
    const BU: i32 = 132201;
    for row in 0..h {
        let (yr, ur, vr) = (&y[row * strides.0..], &u[(row / 2) * strides.1..], &v[(row / 2) * strides.2..]);
        let out = &mut rgb[row * w * 3..(row + 1) * w * 3];
        for (col, px) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let l = (yr[col] as i32 - 16) * Y + (1 << 15);
            let (cu, cv) = (ur[col / 2] as i32 - 128, vr[col / 2] as i32 - 128);
            px[0] = ((l + RV * cv) >> 16).clamp(0, 255) as u8;
            px[1] = ((l + GU * cu + GV * cv) >> 16).clamp(0, 255) as u8;
            px[2] = ((l + BU * cu) >> 16).clamp(0, 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moving picture through the encoder and back: each access unit starts with the mark, and
    /// what comes out is the picture put in, within a film's loss.
    #[test]
    fn a_moving_picture_goes_through_av1_and_back() {
        let (w, h) = (320, 192);
        let mut enc = Encoder::new(w, h, 30, 300_000).unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut rgb = Vec::new();
        for n in 0..12 {
            // A gradient that slides two pixels a frame, in Y, U and V.
            let yp: Vec<u8> = (0..w * h).map(|i| (((i % w) + 2 * n) * 200 / (w + 24) + 30) as u8).collect();
            let up: Vec<u8> = (0..w * h / 4).map(|i| (100 + (i / (w / 2)) % 50) as u8).collect();
            let vp = vec![150u8; w * h / 4];
            let au = enc.encode(&yp, &up, &vp, (w, w / 2, w / 2)).unwrap();
            assert_eq!(au.first(), Some(&MARK), "frame {n}");
            assert!(dec.decode(&au, w, h, &mut rgb).unwrap(), "frame {n} shown");
            let mut want = Vec::new();
            i420_to_rgb(&yp, &up, &vp, (w, w / 2, w / 2), w, h, &mut want);
            let off = rgb.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).sum::<i32>() as f64 / want.len() as f64;
            assert!(off < 4.0, "frame {n} differs by {off} on average");
        }
        assert!(dec.decode(&[MARK, 0, 1, 2, 3], w, h, &mut rgb).is_err() || rgb.len() == w * h * 3);
    }

    #[test]
    fn grey_and_white_keep_their_values() {
        let mut rgb = Vec::new();
        i420_to_rgb(&[16, 235, 126, 126], &[128], &[128], (2, 1, 1), 2, 2, &mut rgb);
        assert_eq!(rgb, [0, 0, 0, 255, 255, 255, 128, 128, 128, 128, 128, 128]);
    }
}
