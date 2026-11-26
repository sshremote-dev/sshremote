//! VideoToolbox's H.265 decoder on macOS: a decompression session made from the stream's
//! parameter sets (again when they change), each access unit decoded before the call returns
//! into an NV12 pixel buffer, turned to RGB here as the encoders' pictures were made (BT.601; the
//! system's own turn would take the stream for BT.709).

use std::ffi::c_void;

use super::Decoder;

type Ref = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

type Done = unsafe extern "C" fn(refcon: *mut c_void, frame: *mut c_void, status: i32, flags: u32, image: Ref, pts: CMTime, duration: CMTime);

#[repr(C)]
struct Callback {
    done: Option<Done>,
    refcon: *mut c_void,
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTDecompressionSessionCreate(alloc: Ref, desc: Ref, spec: Ref, attrs: Ref, callback: *const Callback, session: *mut Ref) -> i32;
    fn VTDecompressionSessionDecodeFrame(session: Ref, sample: Ref, flags: u32, frame: *mut c_void, info: *mut u32) -> i32;
    fn VTDecompressionSessionInvalidate(session: Ref);
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(alloc: Ref, count: usize, sets: *const *const u8, sizes: *const usize, header: i32, extensions: Ref, desc: *mut Ref) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(alloc: Ref, block: *mut c_void, len: usize, block_alloc: Ref, custom: Ref, offset: usize, data_len: usize, flags: u32, out: *mut Ref) -> i32;
    fn CMBlockBufferReplaceDataBytes(src: *const c_void, dest: Ref, offset: usize, len: usize) -> i32;
    fn CMSampleBufferCreateReady(alloc: Ref, data: Ref, desc: Ref, samples: isize, timings: isize, timing: Ref, sizes_count: isize, sizes: *const usize, out: *mut Ref) -> i32;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferLockBaseAddress(buffer: Ref, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: Ref, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: Ref, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: Ref, plane: usize) -> usize;
    fn CVPixelBufferGetWidth(buffer: Ref) -> usize;
    fn CVPixelBufferGetHeight(buffer: Ref) -> usize;
    static kCVPixelBufferPixelFormatTypeKey: Ref;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: Ref);
    fn CFNumberCreate(alloc: Ref, kind: isize, value: *const c_void) -> Ref;
    fn CFDictionaryCreateMutable(alloc: Ref, capacity: isize, keys: Ref, values: Ref) -> Ref;
    fn CFDictionarySetValue(dict: Ref, key: Ref, value: Ref);
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

/// macOS decodes H.265 (in hardware where it can) since 10.13.
pub fn has() -> bool {
    true
}

/// Where the session's callback puts the picture: RGB, width and height.
#[derive(Default)]
struct Got {
    picture: Option<(Vec<u8>, usize, usize)>,
}

pub struct Vt {
    session: Ref,
    desc: Ref,
    sets: Vec<Vec<u8>>,
    got: Box<Got>,
}

// SAFETY: the session is used from one thread at a time.
unsafe impl Send for Vt {}

pub fn open() -> Result<Box<dyn Decoder>, String> {
    Ok(Box::new(Vt { session: std::ptr::null(), desc: std::ptr::null(), sets: Vec::new(), got: Box::default() }))
}

unsafe extern "C" fn done(refcon: *mut c_void, _: *mut c_void, status: i32, _: u32, image: Ref, _: CMTime, _: CMTime) {
    if status != 0 || image.is_null() {
        return;
    }
    // SAFETY: `refcon` is the decoder's `got`, which outlives the session; the image is read
    // while locked.
    unsafe {
        let got = &mut *(refcon as *mut Got);
        CVPixelBufferLockBaseAddress(image, 1);
        let (w, h) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        let (luma, chroma) = (CVPixelBufferGetBaseAddressOfPlane(image, 0), CVPixelBufferGetBaseAddressOfPlane(image, 1));
        let (lp, cp) = (CVPixelBufferGetBytesPerRowOfPlane(image, 0), CVPixelBufferGetBytesPerRowOfPlane(image, 1));
        if !luma.is_null() && !chroma.is_null() {
            let y = std::slice::from_raw_parts(luma, lp * h);
            let uv = std::slice::from_raw_parts(chroma, cp * h.div_ceil(2));
            let mut rgb = Vec::new();
            super::nv12_to_rgb(y, lp, uv, cp, w, h, &mut rgb);
            got.picture = Some((rgb, w, h));
        }
        CVPixelBufferUnlockBaseAddress(image, 1);
    }
}

/// The units of an Annex B access unit.
fn units(au: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= au.len() {
        if au[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let mut end = starts.get(k + 1).map_or(au.len(), |&n| n - 3);
            // A four-byte start code leaves a zero behind the unit before it.
            while end > s && au[end - 1] == 0 && k + 1 < starts.len() {
                end -= 1;
            }
            &au[s..end]
        })
        .filter(|u| !u.is_empty())
        .collect()
}

impl Vt {
    /// A session for these parameter sets (VPS, SPS, PPS).
    fn start(&mut self, sets: Vec<Vec<u8>>) -> Result<(), String> {
        self.stop();
        let ptrs: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
        let sizes: Vec<usize> = sets.iter().map(Vec::len).collect();
        // SAFETY: Core Media's and VideoToolbox's calls with what they take; what is made here
        // is released at `stop`.
        unsafe {
            let mut desc: Ref = std::ptr::null();
            let r = CMVideoFormatDescriptionCreateFromHEVCParameterSets(std::ptr::null(), sets.len(), ptrs.as_ptr(), sizes.as_ptr(), 4, std::ptr::null(), &mut desc);
            if r != 0 {
                return Err(format!("H.265 decoder: the stream's parameter sets were not taken ({r})"));
            }
            let attrs = CFDictionaryCreateMutable(std::ptr::null(), 0, (&raw const kCFTypeDictionaryKeyCallBacks).cast(), (&raw const kCFTypeDictionaryValueCallBacks).cast());
            // NV12, video range.
            let nv12 = u32::from_be_bytes(*b"420v") as i32;
            let n = CFNumberCreate(std::ptr::null(), 3, (&raw const nv12).cast());
            CFDictionarySetValue(attrs, kCVPixelBufferPixelFormatTypeKey, n);
            CFRelease(n);
            let cb = Callback { done: Some(done), refcon: (&mut *self.got as *mut Got).cast() };
            let mut session: Ref = std::ptr::null();
            let r = VTDecompressionSessionCreate(std::ptr::null(), desc, std::ptr::null(), attrs, &cb, &mut session);
            CFRelease(attrs);
            if r != 0 {
                CFRelease(desc);
                return Err(format!("H.265 decoder: no session ({r})"));
            }
            (self.desc, self.session) = (desc, session);
        }
        self.sets = sets;
        Ok(())
    }

    fn stop(&mut self) {
        // SAFETY: made in `start`, let go once.
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
            }
            if !self.desc.is_null() {
                CFRelease(self.desc);
            }
        }
        (self.session, self.desc) = (std::ptr::null(), std::ptr::null());
    }
}

impl Decoder for Vt {
    fn decode(&mut self, au: &[u8], w: usize, h: usize, rgb: &mut Vec<u8>) -> Result<bool, String> {
        let all = units(au);
        let kind = |u: &[u8]| (u[0] >> 1) & 0x3f;
        let sets: Vec<Vec<u8>> = [32, 33, 34].iter().filter_map(|&t| all.iter().find(|u| kind(u) == t).map(|u| u.to_vec())).collect();
        if sets.len() == 3 && sets != self.sets {
            self.start(sets)?;
        }
        if self.session.is_null() {
            return Ok(false);
        }
        let mut data = Vec::with_capacity(au.len() + 16);
        for u in all.iter().filter(|u| !(32..=34).contains(&kind(u))) {
            data.extend_from_slice(&(u.len() as u32).to_be_bytes());
            data.extend_from_slice(u);
        }
        if data.is_empty() {
            return Ok(false);
        }
        self.got.picture = None;
        // SAFETY: a block and a sample made from `data` and released after the call, which
        // decodes before it returns (no asynchronous flag).
        unsafe {
            let mut block: Ref = std::ptr::null();
            if CMBlockBufferCreateWithMemoryBlock(std::ptr::null(), std::ptr::null_mut(), data.len(), std::ptr::null(), std::ptr::null(), 0, data.len(), 0, &mut block) != 0 {
                return Err("H.265 decoder: no block".into());
            }
            CMBlockBufferReplaceDataBytes(data.as_ptr().cast(), block, 0, data.len());
            let mut sample: Ref = std::ptr::null();
            let size = data.len();
            let r = CMSampleBufferCreateReady(std::ptr::null(), block, self.desc, 1, 0, std::ptr::null(), 1, &size, &mut sample);
            CFRelease(block);
            if r != 0 {
                return Err(format!("H.265 decoder: no sample ({r})"));
            }
            let mut info = 0u32;
            let r = VTDecompressionSessionDecodeFrame(self.session, sample, 0, std::ptr::null_mut(), &mut info);
            CFRelease(sample);
            if r != 0 {
                return Err(format!("H.265 decoder: the picture failed ({r})"));
            }
        }
        let Some((px, pw, ph)) = self.got.picture.take() else { return Ok(false) };
        if (pw, ph) != (w, h) {
            return Err(format!("H.265 picture is {pw}x{ph}, expected {w}x{h}"));
        }
        *rgb = px;
        Ok(true)
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        self.stop();
    }
}
