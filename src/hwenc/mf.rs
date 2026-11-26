//! Media Foundation's hardware H.264 and H.265 encoders (Intel Quick Sync, NVIDIA NVENC, AMD AMF): an
//! asynchronous transform given a Direct3D 11 device of its own graphics card and NV12 pictures
//! in textures there (from memory, a picture took Quick Sync 25 ms at any size), asked for low
//! latency, no B pictures, and H.264's constrained baseline (what every viewer's decoder reads)
//! or H.265's Main.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::{Interface, PWSTR};

use super::{Codec, Encoder};

/// The encoder's graphics card: its device, and textures the pictures go up into in turn (the
/// transform may still read the one before).
struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    _manager: IMFDXGIDeviceManager,
    textures: Vec<ID3D11Texture2D>,
    next: usize,
}

/// A device on the card of the adapter `luid` (the first card without one), shared with the
/// transform through a device manager.
fn gpu(luid: Option<u64>) -> Result<Gpu, String> {
    // SAFETY: Direct3D's and Media Foundation's calls with the arguments they take.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(e("graphics"))?;
        let mut chosen = None;
        for i in 0.. {
            let Ok(a) = factory.EnumAdapters1(i) else { break };
            let d = a.GetDesc1().map_err(e("graphics"))?;
            let id = ((d.AdapterLuid.HighPart as u32 as u64) << 32) | d.AdapterLuid.LowPart as u64;
            if chosen.is_none() || luid == Some(id) {
                chosen = Some(a);
            }
            if luid == Some(id) {
                break;
            }
        }
        let adapter = chosen.ok_or("hardware encoder: no graphics card")?;
        let (mut device, mut ctx) = (None, None);
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(e("graphics"))?;
        let (device, ctx) = (device.ok_or("hardware encoder: no device")?, ctx.ok_or("hardware encoder: no device")?);
        // The transform uses the device from its own threads.
        let _ = device.cast::<ID3D11Multithread>().map_err(e("graphics"))?.SetMultithreadProtected(true);
        let (mut token, mut manager) = (0u32, None);
        MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(e("device manager"))?;
        let manager = manager.ok_or("hardware encoder: no device manager")?;
        manager.ResetDevice(&device, token).map_err(e("device manager"))?;
        Ok(Gpu { device, ctx, _manager: manager, textures: Vec::new(), next: 0 })
    }
}

pub struct Mf {
    mft: IMFTransform,
    codec: Codec,
    gpu: Option<Gpu>,
    /// The transform's events (hardware transforms run asynchronously); None for one that does not.
    events: Option<IMFMediaEventGenerator>,
    name: String,
    size: (usize, usize),
    nv12: Vec<u8>,
    /// Pictures the transform asked for and did not get yet.
    wanted: u32,
    pts: i64,
    step: i64,
    provides: bool,
    out_size: u32,
    /// The parameter sets, for an encoder that leaves them out of the first picture.
    header: Vec<u8>,
    first: bool,
    /// What came out after its picture had gone, sent with the next one.
    late: Vec<u8>,
}

// SAFETY: the transform lives in the process's multithreaded apartment.
unsafe impl Send for Mf {}

fn e(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |x| format!("hardware encoder: {what}: {x}")
}

fn ui4(n: u32) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: the variant's tag and its matching member.
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_UI4;
        (*v.Anonymous.Anonymous).Anonymous.ulVal = n;
    }
    v
}

fn yes() -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: as above; VARIANT_TRUE.
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_BOOL;
        (*v.Anonymous.Anonymous).Anonymous.boolVal = windows::Win32::Foundation::VARIANT_TRUE;
    }
    v
}

fn subtype(codec: Codec) -> windows::core::GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::H265 => MFVideoFormat_HEVC,
    }
}

/// The hardware encoders of `codec` the system lists, best first.
fn listed(codec: Codec) -> Result<Vec<IMFActivate>, String> {
    // SAFETY: plain calls; already set up on this thread (in another way) is as good.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e("start"))?;
    }
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
    let (mut list, mut count) = (std::ptr::null_mut(), 0u32);
    // SAFETY: the list and its count are written by the call, freed below.
    unsafe { MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, Some(&input), Some(&output), &mut list, &mut count) }
        .map_err(e("list"))?;
    Ok(if list.is_null() {
        Vec::new()
    } else {
        // SAFETY: `count` entries, each taken over once, then the list itself freed.
        let acts = (0..count as usize).filter_map(|i| unsafe { std::ptr::read(list.add(i)) }).collect();
        unsafe { CoTaskMemFree(Some(list as _)) };
        acts
    })
}

pub fn has(codec: Codec) -> bool {
    listed(codec).is_ok_and(|l| !l.is_empty())
}

pub fn open(codec: Codec, w: usize, h: usize, fps: u8, bitrate: u32) -> Result<Box<dyn Encoder>, String> {
    let acts = listed(codec)?;
    let mut why = Vec::new();
    for act in &acts {
        match Mf::new(act, codec, w, h, fps, bitrate) {
            Ok(m) => return Ok(Box::new(m)),
            Err(x) => why.push(x),
        }
    }
    Err(if why.is_empty() { format!("no video hardware here encodes {codec:?}") } else { why.join("; ") })
}

impl Mf {
    fn new(act: &IMFActivate, codec: Codec, w: usize, h: usize, fps: u8, bitrate: u32) -> Result<Mf, String> {
        let name = {
            let (mut p, mut n) = (PWSTR::null(), 0u32);
            // SAFETY: the string is allocated by the call and freed after it is read.
            unsafe {
                if act.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut n).is_ok() {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as _));
                    s
                } else {
                    "a hardware encoder".into()
                }
            }
        };
        let fps = fps.max(1) as u32;
        // SAFETY: Media Foundation's calls with the arguments they take.
        unsafe {
            let mft: IMFTransform = act.ActivateObject().map_err(e("open"))?;
            let attrs = mft.GetAttributes().map_err(e("attributes"))?;
            // A hardware transform answers only once unlocked for asynchronous use.
            let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            // Without its card's device it takes pictures from memory, slowly; with none, so be it.
            let gpu = gpu(act.GetUINT64(&MFT_ENUM_ADAPTER_LUID).ok())
                .and_then(|g| mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, Interface::as_raw(&g._manager) as usize).map(|()| g).map_err(e("device manager")))
                .inspect_err(|x| eprintln!("sshremote: {x}"))
                .ok();
            let events = mft.cast::<IMFMediaEventGenerator>().ok();
            if let Ok(api) = mft.cast::<ICodecAPI>() {
                let _ = api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &ui4(eAVEncCommonRateControlMode_CBR.0 as u32));
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &ui4(bitrate));
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &yes());
                let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &ui4(0));
                // Key pictures only when asked for (a new encoder): the viewer asks after a loss.
                let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &ui4(fps * 600));
            }
            let size = ((w as u64) << 32) | h as u64;
            let rate = ((fps as u64) << 32) | 1;
            let out = MFCreateMediaType().map_err(e("type"))?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("type"))?;
            out.SetGUID(&MF_MT_SUBTYPE, &subtype(codec)).map_err(e("type"))?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(e("type"))?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(e("type"))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(e("type"))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1).map_err(e("type"))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e("type"))?;
            let profile = match codec {
                Codec::H264 => eAVEncH264VProfile_Base.0 as u32,
                Codec::H265 => eAVEncH265VProfile_Main_420_8.0 as u32,
            };
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, profile).map_err(e("type"))?;
            mft.SetOutputType(0, &out, 0).map_err(e("its output"))?;
            let inp = MFCreateMediaType().map_err(e("type"))?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("type"))?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(e("type"))?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(e("type"))?;
            inp.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(e("type"))?;
            inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1).map_err(e("type"))?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e("type"))?;
            mft.SetInputType(0, &inp, 0).map_err(e("its input"))?;
            let info = mft.GetOutputStreamInfo(0).map_err(e("output"))?;
            let mut header = Vec::new();
            if let Ok(t) = mft.GetOutputCurrentType(0) {
                let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
                if t.GetAllocatedBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut p, &mut n).is_ok() && !p.is_null() {
                    header = std::slice::from_raw_parts(p, n as usize).to_vec();
                    CoTaskMemFree(Some(p as _));
                }
            }
            mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0).map_err(e("start"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(e("start"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(e("start"))?;
            // The driver's own waits follow the system's timer: at its 15.6 ms a picture took 25 ms.
            windows_sys::Win32::Media::timeBeginPeriod(1);
            Ok(Mf {
                mft,
                codec,
                gpu,
                events,
                name,
                size: (w, h),
                nv12: vec![0; w * h * 3 / 2],
                wanted: 0,
                pts: 0,
                step: 10_000_000 / fps as i64,
                provides: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                out_size: info.cbSize.max((w * h) as u32),
                header,
                first: true,
                late: Vec::new(),
            })
        }
    }

    /// The next event of the asynchronous transform, waiting until `until`.
    fn event(&self, until: Instant) -> Result<Option<MF_EVENT_TYPE>, String> {
        let Some(ev) = self.events.as_ref() else { return Ok(None) };
        loop {
            // SAFETY: a call that returns at once.
            match unsafe { ev.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(m) => return Ok(Some(MF_EVENT_TYPE(unsafe { m.GetType() }.map_err(e("event"))? as i32))),
                Err(x) if x.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= until {
                        return Err("hardware encoder: no answer in time".into());
                    }
                    std::thread::sleep(Duration::from_micros(500));
                }
                Err(x) => return Err(e("event")(x)),
            }
        }
    }

    /// What the transform has made, if anything; a change of its output's form is taken first.
    fn output(&mut self) -> Result<Option<Vec<u8>>, String> {
        for _ in 0..3 {
            match self.output_once() {
                // An asynchronous transform says again when the picture is there.
                Err(Some(x)) if x.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.renegotiate()?;
                    if self.events.is_some() {
                        return Ok(None);
                    }
                }
                Err(Some(x)) => return Err(e("output")(x)),
                Err(None) => return Err("hardware encoder: output".into()),
                Ok(d) => return Ok(d),
            }
        }
        Err("hardware encoder: its output kept changing".into())
    }

    /// The output form the transform offers now, taken.
    fn renegotiate(&mut self) -> Result<(), String> {
        // SAFETY: Media Foundation's calls with the arguments they take.
        unsafe {
            let t = self.mft.GetOutputAvailableType(0, 0).map_err(e("its new output"))?;
            self.mft.SetOutputType(0, &t, 0).map_err(e("its new output"))?;
            let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
            if t.GetAllocatedBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut p, &mut n).is_ok() && !p.is_null() {
                self.header = std::slice::from_raw_parts(p, n as usize).to_vec();
                CoTaskMemFree(Some(p as _));
            }
            let info = self.mft.GetOutputStreamInfo(0).map_err(e("output"))?;
            self.provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            self.out_size = info.cbSize.max((self.size.0 * self.size.1) as u32);
        }
        Ok(())
    }

    fn output_once(&mut self) -> Result<Option<Vec<u8>>, Option<windows::core::Error>> {
        let e = |_: windows::core::Error| None;
        // SAFETY: Media Foundation's calls; the sample handed back is taken out and let go here.
        unsafe {
            let sample = if self.provides {
                None
            } else {
                let s = MFCreateSample().map_err(e)?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_size).map_err(e)?).map_err(e)?;
                Some(s)
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: std::mem::ManuallyDrop::new(sample), dwStatus: 0, pEvents: std::mem::ManuallyDrop::new(None) }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut out, &mut status);
            let sample = std::mem::ManuallyDrop::take(&mut out[0].pSample);
            drop(std::mem::ManuallyDrop::take(&mut out[0].pEvents));
            match r {
                Err(x) if x.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(x) => return Err(Some(x)),
                Ok(()) => {}
            }
            let Some(sample) = sample else { return Ok(None) };
            let buf = sample.ConvertToContiguousBuffer().map_err(Some)?;
            let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
            buf.Lock(&mut p, None, Some(&mut n)).map_err(Some)?;
            let data = std::slice::from_raw_parts(p, n as usize).to_vec();
            let _ = buf.Unlock();
            Ok(Some(data))
        }
    }

    /// The picture in a texture on the card, as a buffer of a sample.
    fn on_card(&mut self) -> Result<Option<IMFMediaBuffer>, String> {
        let (w, h) = self.size;
        let Some(g) = self.gpu.as_mut() else { return Ok(None) };
        // SAFETY: Direct3D's and Media Foundation's calls; the picture is copied up as it is read.
        unsafe {
            if g.textures.len() < 4 {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w as u32,
                    Height: h as u32,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut t = None;
                g.device.CreateTexture2D(&desc, None, Some(&mut t)).map_err(e("texture"))?;
                g.textures.push(t.ok_or("hardware encoder: no texture")?);
            }
            let t = g.textures[g.next % g.textures.len()].clone();
            g.next += 1;
            // Both planes at once: the chroma rows follow the luma rows at the same pitch.
            g.ctx.UpdateSubresource(&t, 0, None, self.nv12.as_ptr().cast(), w as u32, 0);
            let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &t, 0, false).map_err(e("buffer"))?;
            let len = buf.cast::<IMF2DBuffer>().and_then(|b| b.GetContiguousLength()).unwrap_or(self.nv12.len() as u32);
            buf.SetCurrentLength(len).map_err(e("buffer"))?;
            Ok(Some(buf))
        }
    }

    fn feed(&mut self) -> Result<(), String> {
        let card = self.on_card()?;
        // SAFETY: Media Foundation's calls; the picture is copied into a buffer of its own.
        unsafe {
            let buf = match card {
                Some(b) => b,
                None => {
                    let buf = MFCreateMemoryBuffer(self.nv12.len() as u32).map_err(e("buffer"))?;
                    let mut p = std::ptr::null_mut();
                    buf.Lock(&mut p, None, None).map_err(e("buffer"))?;
                    std::ptr::copy_nonoverlapping(self.nv12.as_ptr(), p, self.nv12.len());
                    let _ = buf.Unlock();
                    buf.SetCurrentLength(self.nv12.len() as u32).map_err(e("buffer"))?;
                    buf
                }
            };
            let s = MFCreateSample().map_err(e("sample"))?;
            s.AddBuffer(&buf).map_err(e("sample"))?;
            s.SetSampleTime(self.pts).map_err(e("sample"))?;
            s.SetSampleDuration(self.step).map_err(e("sample"))?;
            self.pts += self.step;
            self.mft.ProcessInput(0, &s, 0).map_err(e("input"))
        }
    }
}

impl Encoder for Mf {
    fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], strides: (usize, usize, usize)) -> Result<Vec<u8>, String> {
        let (w, h) = self.size;
        for r in 0..h {
            self.nv12[r * w..(r + 1) * w].copy_from_slice(&y[r * strides.0..r * strides.0 + w]);
        }
        let uv = &mut self.nv12[w * h..];
        for r in 0..h / 2 {
            for c in 0..w / 2 {
                uv[r * w + 2 * c] = u[r * strides.1 + c];
                uv[r * w + 2 * c + 1] = v[r * strides.2 + c];
            }
        }
        // A first picture may take the hardware a while to set up.
        let until = Instant::now() + if self.first { Duration::from_secs(5) } else { Duration::from_millis(250) };
        let mut data = if self.events.is_none() {
            self.feed()?;
            self.output()?.unwrap_or_default()
        } else {
            while self.wanted == 0 {
                match self.event(until)? {
                    Some(t) if t == METransformNeedInput => self.wanted += 1,
                    Some(t) if t == METransformHaveOutput => {
                        if let Some(d) = self.output()? {
                            self.late.extend_from_slice(&d);
                        }
                    }
                    _ => {}
                }
            }
            self.feed()?;
            self.wanted -= 1;
            loop {
                match self.event(until)? {
                    Some(t) if t == METransformNeedInput => self.wanted += 1,
                    Some(t) if t == METransformHaveOutput => {
                        if let Some(d) = self.output()? {
                            break d;
                        }
                    }
                    _ => {}
                }
            }
        };
        if !self.late.is_empty() {
            data.splice(0..0, std::mem::take(&mut self.late));
        }
        if self.first && !super::has_sps(self.codec, &data) && !self.header.is_empty() {
            data.splice(0..0, self.header.iter().copied());
        }
        self.first = false;
        Ok(data)
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        // SAFETY: plain calls; an asynchronous transform is shut down so its threads end.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
            windows_sys::Win32::Media::timeEndPeriod(1);
        }
    }
}
