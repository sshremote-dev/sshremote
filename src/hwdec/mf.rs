//! Media Foundation's H.265 decoder (Windows' HEVC Video Extensions bring it): a synchronous
//! transform asked for low latency, one access unit in, its NV12 picture out.

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::Interface;

use super::Decoder;

fn e(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |x| format!("H.265 decoder: {what}: {x}")
}

fn listed() -> Vec<IMFActivate> {
    // SAFETY: plain calls; already set up on this thread (in another way) is as good. The list
    // and its count are written by the call, each entry taken over once, the list freed.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if MFStartup(MF_VERSION, MFSTARTUP_FULL).is_err() {
            return Vec::new();
        }
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_HEVC };
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let (mut list, mut count) = (std::ptr::null_mut(), 0u32);
        if MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER, Some(&input), Some(&output), &mut list, &mut count).is_err() || list.is_null() {
            return Vec::new();
        }
        let acts = (0..count as usize).filter_map(|i| std::ptr::read(list.add(i))).collect();
        CoTaskMemFree(Some(list as _));
        acts
    }
}

pub fn has() -> bool {
    !listed().is_empty()
}

pub struct Mf {
    mft: IMFTransform,
    provides: bool,
    out_size: u32,
    /// The picture's pitch and rows as the decoder lays NV12 out (its rows may be padded).
    layout: (usize, usize),
}

// SAFETY: the transform lives in the process's multithreaded apartment.
unsafe impl Send for Mf {}

/// Made at the first picture, whose size the decoder is told.
pub struct Later(Option<Mf>);

pub fn open() -> Result<Box<dyn Decoder>, String> {
    Ok(Box::new(Later(None)))
}

impl Decoder for Later {
    fn decode(&mut self, au: &[u8], w: usize, h: usize, rgb: &mut Vec<u8>) -> Result<bool, String> {
        if self.0.is_none() {
            self.0 = Some(Mf::new(w, h)?);
        }
        self.0.as_mut().unwrap().decode(au, w, h, rgb)
    }
}

impl Mf {
    fn new(w: usize, h: usize) -> Result<Mf, String> {
        let act = listed().into_iter().next().ok_or("this computer has no H.265 decoder (the HEVC Video Extensions bring one)")?;
        // SAFETY: Media Foundation's calls with the arguments they take.
        unsafe {
            let mft: IMFTransform = act.ActivateObject().map_err(e("open"))?;
            if let Ok(a) = mft.GetAttributes() {
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            // Each picture out as its unit goes in, not one behind.
            if let Ok(api) = mft.cast::<ICodecAPI>() {
                let mut v = windows::Win32::System::Variant::VARIANT::default();
                (*v.Anonymous.Anonymous).vt = windows::Win32::System::Variant::VT_BOOL;
                (*v.Anonymous.Anonymous).Anonymous.boolVal = windows::Win32::Foundation::VARIANT_TRUE;
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &v);
            }
            let inp = MFCreateMediaType().map_err(e("type"))?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("type"))?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC).map_err(e("type"))?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64).map_err(e("type"))?;
            mft.SetInputType(0, &inp, 0).map_err(e("its input"))?;
            let mut m = Mf { mft, provides: false, out_size: 0, layout: (w, h) };
            m.pick_output()?;
            m.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(e("start"))?;
            m.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(e("start"))?;
            Ok(m)
        }
    }

    /// NV12 out, among what the decoder offers now.
    fn pick_output(&mut self) -> Result<(), String> {
        // SAFETY: Media Foundation's calls with the arguments they take.
        unsafe {
            for i in 0.. {
                let Ok(t) = self.mft.GetOutputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|g| g == MFVideoFormat_NV12) {
                    self.mft.SetOutputType(0, &t, 0).map_err(e("its output"))?;
                    let size = t.GetUINT64(&MF_MT_FRAME_SIZE).ok();
                    let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32 as isize).unwrap_or(0);
                    let (w, h) = size.map_or(self.layout, |s| ((s >> 32) as usize, (s & 0xffff_ffff) as usize));
                    self.layout = (if stride > 0 { stride as usize } else { w }, h);
                    let info = self.mft.GetOutputStreamInfo(0).map_err(e("output"))?;
                    self.provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 != 0;
                    self.out_size = info.cbSize;
                    return Ok(());
                }
            }
        }
        Err("H.265 decoder: it gives no NV12".into())
    }

    /// The decoder's next picture as RGB, if it has one.
    fn output(&mut self, w: usize, h: usize, rgb: &mut Vec<u8>) -> Result<bool, String> {
        for _ in 0..3 {
            // SAFETY: Media Foundation's calls; the sample handed back is taken out and let go.
            unsafe {
                let sample = if self.provides {
                    None
                } else {
                    let s = MFCreateSample().map_err(e("sample"))?;
                    s.AddBuffer(&MFCreateMemoryBuffer(self.out_size.max((self.layout.0 * self.layout.1 * 3 / 2) as u32)).map_err(e("buffer"))?).map_err(e("buffer"))?;
                    Some(s)
                };
                let mut out = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: std::mem::ManuallyDrop::new(sample), dwStatus: 0, pEvents: std::mem::ManuallyDrop::new(None) }];
                let mut status = 0u32;
                let r = self.mft.ProcessOutput(0, &mut out, &mut status);
                let sample = std::mem::ManuallyDrop::take(&mut out[0].pSample);
                drop(std::mem::ManuallyDrop::take(&mut out[0].pEvents));
                match r {
                    Err(x) if x.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
                    // The stream's size is known now: its picture's layout with it.
                    Err(x) if x.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.pick_output()?;
                        continue;
                    }
                    Err(x) => return Err(e("output")(x)),
                    Ok(()) => {}
                }
                let Some(sample) = sample else { return Ok(false) };
                let buf = sample.ConvertToContiguousBuffer().map_err(e("output"))?;
                let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
                buf.Lock(&mut p, None, Some(&mut n)).map_err(e("output"))?;
                let data = std::slice::from_raw_parts(p, n as usize);
                let (pitch, rows) = (self.layout.0.max(w), self.layout.1.max(h));
                let ok = data.len() >= pitch * rows + pitch * (h / 2);
                if ok {
                    super::nv12_to_rgb(data, pitch, &data[pitch * rows..], pitch, w, h, rgb);
                }
                let _ = buf.Unlock();
                return Ok(ok);
            }
        }
        Err("H.265 decoder: its output kept changing".into())
    }
}

impl Mf {
    fn decode(&mut self, au: &[u8], w: usize, h: usize, rgb: &mut Vec<u8>) -> Result<bool, String> {
        // An access unit delimiter after it: the decoder knows the picture is whole and gives
        // it at once, rather than with the next one.
        let au = [au, &[0, 0, 0, 1, 0x46, 0x01, 0x50][..]].concat();
        // SAFETY: Media Foundation's calls; the access unit is copied into a buffer of its own.
        unsafe {
            let buf = MFCreateMemoryBuffer(au.len() as u32).map_err(e("buffer"))?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(e("buffer"))?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), p, au.len());
            let _ = buf.Unlock();
            buf.SetCurrentLength(au.len() as u32).map_err(e("buffer"))?;
            let s = MFCreateSample().map_err(e("sample"))?;
            s.AddBuffer(&buf).map_err(e("sample"))?;
            self.mft.ProcessInput(0, &s, 0).map_err(e("input"))?;
        }
        self.output(w, h, rgb)
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        // SAFETY: plain calls.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}
