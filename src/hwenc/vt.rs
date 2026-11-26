//! VideoToolbox's hardware H.264 encoder on macOS: a real-time compression session, constrained
//! baseline with no reordering, fed NV12 pixel buffers from its own pool, each picture completed
//! before the call returns, its length-prefixed units turned into Annex B.

use std::ffi::c_void;
use std::sync::Mutex;

use super::Encoder;

type Ref = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

type Done = unsafe extern "C" fn(refcon: *mut c_void, frame: *mut c_void, status: i32, flags: u32, sample: Ref);

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTCompressionSessionCreate(alloc: Ref, w: i32, h: i32, codec: u32, spec: Ref, source: Ref, out_alloc: Ref, done: Option<Done>, refcon: *mut c_void, session: *mut Ref) -> i32;
    fn VTSessionSetProperty(session: Ref, key: Ref, value: Ref) -> i32;
    fn VTSessionCopyProperty(session: Ref, key: Ref, alloc: Ref, value: *mut Ref) -> i32;
    fn VTCompressionSessionPrepareToEncodeFrames(session: Ref) -> i32;
    fn VTCompressionSessionGetPixelBufferPool(session: Ref) -> Ref;
    fn VTCompressionSessionEncodeFrame(session: Ref, image: Ref, pts: CMTime, duration: CMTime, props: Ref, frame: *mut c_void, info: *mut u32) -> i32;
    fn VTCompressionSessionCompleteFrames(session: Ref, until: CMTime) -> i32;
    fn VTCompressionSessionInvalidate(session: Ref);
    static kVTCompressionPropertyKey_RealTime: Ref;
    static kVTCompressionPropertyKey_ProfileLevel: Ref;
    static kVTProfileLevel_H264_Baseline_AutoLevel: Ref;
    static kVTCompressionPropertyKey_AverageBitRate: Ref;
    static kVTCompressionPropertyKey_AllowFrameReordering: Ref;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: Ref;
    static kVTCompressionPropertyKey_ExpectedFrameRate: Ref;
    static kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder: Ref;
    static kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: Ref;
    static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: Ref;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetDataBuffer(sample: Ref) -> Ref;
    fn CMSampleBufferGetFormatDescription(sample: Ref) -> Ref;
    fn CMBlockBufferGetDataLength(block: Ref) -> usize;
    fn CMBlockBufferCopyDataBytes(block: Ref, offset: usize, len: usize, to: *mut c_void) -> i32;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc: Ref, index: usize, set: *mut *const u8, size: *mut usize, count: *mut usize, header: *mut i32) -> i32;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferPoolCreatePixelBuffer(alloc: Ref, pool: Ref, out: *mut Ref) -> i32;
    fn CVPixelBufferLockBaseAddress(buffer: Ref, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: Ref, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: Ref, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: Ref, plane: usize) -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: Ref);
    fn CFNumberCreate(alloc: Ref, kind: isize, value: *const c_void) -> Ref;
    fn CFDictionaryCreateMutable(alloc: Ref, capacity: isize, keys: Ref, values: Ref) -> Ref;
    fn CFDictionarySetValue(dict: Ref, key: Ref, value: Ref);
    fn CFBooleanGetValue(b: Ref) -> bool;
    static kCFBooleanTrue: Ref;
    static kCFBooleanFalse: Ref;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

const H264: u32 = u32::from_be_bytes(*b"avc1");
const INT32: isize = 3;

/// What the session hands back for one picture: its units, or why there are none.
type Made = Mutex<Vec<Result<Vec<u8>, i32>>>;

pub struct Vt {
    session: Ref,
    made: Box<Made>,
    size: (usize, usize),
    pts: i64,
    fps: i32,
    hardware: bool,
}

// SAFETY: the session is used from one thread at a time; VideoToolbox allows that.
unsafe impl Send for Vt {}

/// The picture's units, length-prefixed in `data`, as Annex B; the parameter sets of `desc`
/// before them.
unsafe fn annex_b(desc: Ref, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    if !desc.is_null() {
        let (mut count, mut header) = (0usize, 0i32);
        let (mut p, mut n) = (std::ptr::null(), 0usize);
        // SAFETY: the description's own sets, read while it lives.
        unsafe {
            if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, 0, &mut p, &mut n, &mut count, &mut header) == 0 {
                for i in 0..count {
                    if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, i, &mut p, &mut n, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                        out.extend_from_slice(&[0, 0, 0, 1]);
                        out.extend_from_slice(std::slice::from_raw_parts(p, n));
                    }
                }
            }
        }
    }
    let mut at = 0;
    while at + 4 <= data.len() {
        let n = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        at += 4;
        let end = (at + n).min(data.len());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[at..end]);
        at = end;
    }
    out
}

unsafe extern "C" fn done(refcon: *mut c_void, _: *mut c_void, status: i32, _: u32, sample: Ref) {
    // SAFETY: `refcon` is the encoder's `made`, which outlives the session.
    let made = unsafe { &*(refcon as *const Made) };
    let r = if status != 0 || sample.is_null() {
        Err(status)
    } else {
        // SAFETY: the sample is the session's, read during the call.
        unsafe {
            let block = CMSampleBufferGetDataBuffer(sample);
            let n = if block.is_null() { 0 } else { CMBlockBufferGetDataLength(block) };
            let mut data = vec![0u8; n];
            if n > 0 {
                CMBlockBufferCopyDataBytes(block, 0, n, data.as_mut_ptr().cast());
            }
            // Parameter sets before every key picture; a picture with none needs none.
            let key = super::is_key(&annex_b(std::ptr::null(), &data));
            Ok(annex_b(if key { CMSampleBufferGetFormatDescription(sample) } else { std::ptr::null() }, &data))
        }
    };
    made.lock().unwrap_or_else(|e| e.into_inner()).push(r);
}

pub fn open(w: usize, h: usize, fps: u8, bitrate: u32) -> Result<Box<dyn Encoder>, String> {
    let made: Box<Made> = Box::new(Mutex::new(Vec::new()));
    let fps = fps.max(1) as i32;
    // SAFETY: VideoToolbox's and Core Foundation's calls with the arguments they take; what is
    // made here is released here, the session at drop.
    unsafe {
        let spec = CFDictionaryCreateMutable(std::ptr::null(), 0, (&raw const kCFTypeDictionaryKeyCallBacks).cast(), (&raw const kCFTypeDictionaryValueCallBacks).cast());
        CFDictionarySetValue(spec, kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder, kCFBooleanTrue);
        // Apple's software encoder is no better than this program's own: the hardware or none
        // (tests take either, on a Mac without the hardware).
        CFDictionarySetValue(spec, kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, if cfg!(test) { kCFBooleanFalse } else { kCFBooleanTrue });
        let mut session: Ref = std::ptr::null();
        let refcon = (&*made as *const Made).cast_mut().cast();
        let r = VTCompressionSessionCreate(std::ptr::null(), w as i32, h as i32, H264, spec, std::ptr::null(), std::ptr::null(), Some(done), refcon, &mut session);
        CFRelease(spec);
        if r != 0 || session.is_null() {
            return Err(format!("VideoToolbox: no H.264 encoder for {w}x{h} ({r})"));
        }
        let number = |v: i32| CFNumberCreate(std::ptr::null(), INT32, (&raw const v).cast());
        let set = |k: Ref, v: Ref| VTSessionSetProperty(session, k, v);
        set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
        set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Baseline_AutoLevel);
        set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
        for (k, v) in [(kVTCompressionPropertyKey_AverageBitRate, bitrate.min(i32::MAX as u32) as i32), (kVTCompressionPropertyKey_ExpectedFrameRate, fps), (kVTCompressionPropertyKey_MaxKeyFrameInterval, fps * 600)] {
            let n = number(v);
            set(k, n);
            CFRelease(n);
        }
        VTCompressionSessionPrepareToEncodeFrames(session);
        let mut used: Ref = std::ptr::null();
        let hardware = VTSessionCopyProperty(session, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder, std::ptr::null(), &mut used) == 0 && !used.is_null() && {
            let b = CFBooleanGetValue(used);
            CFRelease(used);
            b
        };
        Ok(Box::new(Vt { session, made, size: (w, h), pts: 0, fps, hardware }))
    }
}

impl Encoder for Vt {
    fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], strides: (usize, usize, usize)) -> Result<Vec<u8>, String> {
        let (w, h) = self.size;
        // SAFETY: a pixel buffer of the session's pool, written while locked, released after
        // the picture is complete.
        unsafe {
            let pool = VTCompressionSessionGetPixelBufferPool(self.session);
            let mut pb: Ref = std::ptr::null();
            if pool.is_null() || CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut pb) != 0 || pb.is_null() {
                return Err("VideoToolbox: no picture buffer".into());
            }
            CVPixelBufferLockBaseAddress(pb, 0);
            let (luma, chroma) = (CVPixelBufferGetBaseAddressOfPlane(pb, 0), CVPixelBufferGetBaseAddressOfPlane(pb, 1));
            let (lp, cp) = (CVPixelBufferGetBytesPerRowOfPlane(pb, 0), CVPixelBufferGetBytesPerRowOfPlane(pb, 1));
            for r in 0..h {
                std::ptr::copy_nonoverlapping(y[r * strides.0..].as_ptr(), luma.add(r * lp), w);
            }
            for r in 0..h / 2 {
                let row = std::slice::from_raw_parts_mut(chroma.add(r * cp), w);
                for c in 0..w / 2 {
                    row[2 * c] = u[r * strides.1 + c];
                    row[2 * c + 1] = v[r * strides.2 + c];
                }
            }
            CVPixelBufferUnlockBaseAddress(pb, 0);
            let at = CMTime { value: self.pts, timescale: self.fps, flags: 1, epoch: 0 };
            let step = CMTime { value: 1, timescale: self.fps, flags: 1, epoch: 0 };
            self.pts += 1;
            let r = VTCompressionSessionEncodeFrame(self.session, pb, at, step, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
            CFRelease(pb);
            if r != 0 {
                return Err(format!("VideoToolbox: the picture was not taken ({r})"));
            }
            // Every picture handed in comes out before this returns.
            VTCompressionSessionCompleteFrames(self.session, CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 });
        }
        let mut out = Vec::new();
        for r in self.made.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            out.extend(r.map_err(|s| format!("VideoToolbox: the picture failed ({s})"))?);
        }
        Ok(out)
    }

    fn name(&self) -> String {
        if self.hardware { "VideoToolbox (the video hardware)".into() } else { "VideoToolbox (software)".into() }
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        // SAFETY: the session made in `open`, ended once; `made` outlives it.
        unsafe {
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session);
        }
    }
}
