//! HDR desktops on Windows. With HDR on, Windows composes the desktop as linear scRGB in half
//! floats, where SDR white sits at the brightness the person set for SDR content (about 200 nits:
//! 2.5), not at 1.0. Desktop Duplication hands such a desktop over as it is when asked
//! (`DuplicateOutput1`); asked the old way it clips it to 8 bits, and everything brighter than
//! about 40% turns white. Here it becomes the picture an SDR screen would show: divided by that
//! white, a colour brighter than white scaled down whole (its hue kept, where clamping each
//! channel turns bright orange yellow), the sRGB curve put back. On the graphics card with a small
//! compute shader, else on the CPU.

/// The sRGB curve, from linear light in 0..=1.
fn srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// A half float's value.
pub fn half(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (e, m) = ((h >> 10) & 0x1f, (h & 0x3ff) as f32);
    match e {
        0 => sign * m * (2.0f32).powi(-24),
        31 if m == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + m / 1024.0) * (2.0f32).powi(e as i32 - 15),
    }
}

/// Linear scRGB light, SDR white at `white`, to the light an SDR screen shows (0..=1 each).
#[cfg(test)]
fn light(rgb: [f32; 3], white: f32) -> [f32; 3] {
    // Colours outside sRGB come negative: their channel is left at none.
    let c = rgb.map(|v| if v.is_nan() { 0.0 } else { (v.min(65504.0) / white).max(0.0) });
    let top = c[0].max(c[1]).max(c[2]);
    if top > 1.0 { c.map(|v| v / top) } else { c }
}

/// One pixel as an SDR screen shows it: R, G, B in 8 bits.
#[cfg(test)]
pub fn pixel(rgb: [f32; 3], white: f32) -> [u8; 3] {
    light(rgb, white).map(|v| (srgb(v) * 255.0 + 0.5) as u8)
}

/// The same for rows of pixels on the CPU, with tables made for one white.
pub struct Cpu {
    #[cfg_attr(not(windows), allow(dead_code))]
    white: f32,
    /// Each half float's linear value over white, never below none.
    linear: Vec<f32>,
    /// Linear light in `ENC` steps to its 8-bit sRGB value.
    encode: Vec<u8>,
}

const ENC: usize = 16384;

impl Cpu {
    pub fn new(white: f32) -> Cpu {
        let linear = (0..=u16::MAX)
            .map(|h| {
                let v = half(h);
                if v.is_nan() { 0.0 } else { (v.min(65504.0) / white).max(0.0) }
            })
            .collect();
        let encode = (0..ENC).map(|i| (srgb(i as f32 / (ENC - 1) as f32) * 255.0 + 0.5) as u8).collect();
        Cpu { white, linear, encode }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn white(&self) -> f32 {
        self.white
    }

    /// Pixels of four half floats (R, G, B, A) into BGRA bytes.
    pub fn row(&self, src: &[u16], dst: &mut [u8]) {
        for (p, out) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
            let c = [self.linear[p[0] as usize], self.linear[p[1] as usize], self.linear[p[2] as usize]];
            let top = c[0].max(c[1]).max(c[2]);
            let k = if top > 1.0 { (ENC - 1) as f32 / top } else { (ENC - 1) as f32 };
            let e = |v: f32| self.encode[((v * k + 0.5) as usize).min(ENC - 1)];
            *out = [e(c[2]), e(c[1]), e(c[0]), 255];
        }
    }
}

#[cfg(windows)]
pub use gpu::{Gpu, white};

#[cfg(windows)]
mod gpu {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
    use windows::core::Interface;

    /// The conversion, as `super::pixel` does it, a thread for each pixel; the output's channels
    /// are written in BGRA order, so its bytes read as the desktop's 8-bit pictures do.
    const SHADER: &str = r"
Texture2D<float4> src : register(t0);
RWTexture2D<unorm float4> dst : register(u0);
cbuffer Light : register(b0) { float4 white; };
float3 enc(float3 c) { return c <= 0.0031308 ? c * 12.92 : 1.055 * pow(c, 1.0 / 2.4) - 0.055; }
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    float3 c = max(min(src[id.xy].rgb, 65504.0) / white.x, 0.0);
    c /= max(1.0, max(c.r, max(c.g, c.b)));
    c = enc(c);
    dst[id.xy] = float4(c.b, c.g, c.r, 1.0);
}
";

    type Compile = unsafe extern "system" fn(*const c_void, usize, *const u8, *const c_void, *const c_void, *const u8, *const u8, u32, u32, *mut *mut c_void, *mut *mut c_void) -> i32;

    /// The shader compiled once a process, with the system's compiler loaded when first needed
    /// (so the program starts where it is missing).
    fn bytecode() -> Result<&'static [u8], String> {
        static CODE: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
        CODE.get_or_init(|| {
            use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
            let name: Vec<u16> = "d3dcompiler_47.dll\0".encode_utf16().collect();
            // SAFETY: the system's own library, from System32 alone; the function's type is its
            // documented one; the blobs it hands back are taken over once.
            unsafe {
                let lib = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
                if lib.is_null() {
                    return Err("no shader compiler (d3dcompiler_47.dll)".into());
                }
                let f = GetProcAddress(lib, c"D3DCompile".as_ptr().cast()).ok_or("no D3DCompile")?;
                let compile: Compile = std::mem::transmute(f);
                let (mut code, mut errors) = (std::ptr::null_mut(), std::ptr::null_mut());
                let r = compile(SHADER.as_ptr().cast(), SHADER.len(), c"hdr".as_ptr().cast(), std::ptr::null(), std::ptr::null(), c"main".as_ptr().cast(), c"cs_5_0".as_ptr().cast(), 1 << 15, 0, &mut code, &mut errors);
                let blob = |p: *mut c_void| (!p.is_null()).then(|| ID3DBlob::from_raw(p));
                let (code, errors) = (blob(code), blob(errors));
                let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec();
                match code {
                    Some(c) if r >= 0 => Ok(bytes(&c)),
                    _ => Err(format!("the HDR shader did not compile: {}", errors.map(|e| String::from_utf8_lossy(&bytes(&e)).into_owned()).unwrap_or_default())),
                }
            }
        })
        .as_ref()
        .map(|v| v.as_slice())
        .map_err(Clone::clone)
    }

    /// The conversion on one device, with the textures for one size.
    pub struct Gpu {
        cs: ID3D11ComputeShader,
        light: ID3D11Buffer,
        white: f32,
        /// The float picture copied where the shader reads it, and the 8-bit one it writes.
        at: Option<(u32, u32, ID3D11Texture2D, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11UnorderedAccessView)>,
    }

    impl Gpu {
        pub fn new(device: &ID3D11Device) -> Result<Gpu, String> {
            let code = bytecode()?;
            // SAFETY: Direct3D's calls with what they take.
            unsafe {
                let mut cs = None;
                device.CreateComputeShader(code, None, Some(&mut cs)).map_err(|e| format!("HDR shader: {e}"))?;
                let desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
                let mut light = None;
                device.CreateBuffer(&desc, None, Some(&mut light)).map_err(|e| format!("HDR shader: {e}"))?;
                Ok(Gpu { cs: cs.ok_or("no shader")?, light: light.ok_or("no buffer")?, white: 0.0, at: None })
            }
        }

        /// `tex` (half floats, `w`x`h`) as 8-bit BGRA in a texture of this device's.
        pub fn convert(&mut self, device: &ID3D11Device, ctx: &ID3D11DeviceContext, tex: &ID3D11Texture2D, w: u32, h: u32, white: f32) -> Result<ID3D11Texture2D, String> {
            let e = |x: windows::core::Error| format!("HDR conversion: {x}");
            // SAFETY: Direct3D's calls with what they take; the views are let go of the pipeline
            // before the textures are used otherwise.
            unsafe {
                if self.at.as_ref().is_none_or(|a| (a.0, a.1) != (w, h)) {
                    let base = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, ..Default::default() };
                    let (mut src, mut out, mut srv, mut uav) = (None, None, None, None);
                    let sd = D3D11_TEXTURE2D_DESC { Format: DXGI_FORMAT_R16G16B16A16_FLOAT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..base };
                    device.CreateTexture2D(&sd, None, Some(&mut src)).map_err(e)?;
                    let od = D3D11_TEXTURE2D_DESC { Format: DXGI_FORMAT_R8G8B8A8_UNORM, BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32, ..base };
                    device.CreateTexture2D(&od, None, Some(&mut out)).map_err(e)?;
                    let (src, out) = (src.ok_or("no texture")?, out.ok_or("no texture")?);
                    device.CreateShaderResourceView(&src, None, Some(&mut srv)).map_err(e)?;
                    device.CreateUnorderedAccessView(&out, None, Some(&mut uav)).map_err(e)?;
                    self.at = Some((w, h, src, srv.ok_or("no view")?, out, uav.ok_or("no view")?));
                }
                if white != self.white {
                    let v = [white, 0.0, 0.0, 0.0];
                    ctx.UpdateSubresource(&self.light, 0, None, v.as_ptr().cast(), 0, 0);
                    self.white = white;
                }
                let (_, _, src, srv, out, uav) = self.at.as_ref().expect("just made");
                ctx.CopyResource(src, tex);
                ctx.CSSetShader(&self.cs, None);
                ctx.CSSetShaderResources(0, Some(&[Some(srv.clone())]));
                let uavs = [Some(uav.clone())];
                ctx.CSSetUnorderedAccessViews(0, 1, Some(uavs.as_ptr()), None);
                ctx.CSSetConstantBuffers(0, Some(&[Some(self.light.clone())]));
                ctx.Dispatch(w.div_ceil(8), h.div_ceil(8), 1);
                ctx.CSSetShaderResources(0, Some(&[None]));
                let none = [None];
                ctx.CSSetUnorderedAccessViews(0, 1, Some(none.as_ptr()), None);
                ctx.CSSetShader(None::<&ID3D11ComputeShader>, None);
                Ok(out.clone())
            }
        }
    }

    /// Where SDR white sits on the monitor named `name` ("\\.\DISPLAY1"), in scRGB: the SDR
    /// brightness the person set (80 nits a unit) on an HDR desktop; 1.0 on a desktop in
    /// floats without HDR (Windows' Advanced Color on an SDR screen), and where it is not told.
    pub fn white(name: &[u16; 32]) -> f32 {
        if hdr(name) == Some(false) {
            return 1.0;
        }
        sdr_white(name).unwrap_or(1.0)
    }

    /// Whether the monitor shows HDR now, as a fresh look at the outputs says (an old one keeps
    /// what was so when it was made).
    fn hdr(name: &[u16; 32]) -> Option<bool> {
        // SAFETY: plain calls; the outputs are looked at, not kept.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
            let mut a = 0;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                a += 1;
                let mut o = 0;
                while let Ok(output) = adapter.EnumOutputs(o) {
                    o += 1;
                    let Ok(d) = output.cast::<IDXGIOutput6>().and_then(|o| o.GetDesc1()) else { continue };
                    if d.DeviceName == *name {
                        return Some(d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
                    }
                }
            }
            None
        }
    }

    /// The SDR white level Windows keeps for the monitor, in scRGB.
    fn sdr_white(name: &[u16; 32]) -> Option<f32> {
        use windows_sys::Win32::Devices::Display::*;
        // SAFETY: the path and mode lists are as long as the system said, and each question is
        // a packet of the size and kind it says.
        unsafe {
            let (mut np, mut nm) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != 0 {
                return None;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), std::ptr::null_mut()) != 0 {
                return None;
            }
            for p in paths.iter().take(np as usize) {
                let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                    header: DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32, adapterId: p.sourceInfo.adapterId, id: p.sourceInfo.id },
                    ..Default::default()
                };
                if DisplayConfigGetDeviceInfo(&mut src.header) != 0 || src.viewGdiDeviceName != *name {
                    continue;
                }
                let mut sw = DISPLAYCONFIG_SDR_WHITE_LEVEL { header: DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, size: size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32, adapterId: p.targetInfo.adapterId, id: p.targetInfo.id }, SDRWhiteLevel: 0 };
                if DisplayConfigGetDeviceInfo(&mut sw.header) != 0 || sw.SDRWhiteLevel == 0 {
                    return None;
                }
                return Some(sw.SDRWhiteLevel as f32 / 1000.0);
            }
            None
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};

        fn to_half(v: f32) -> u16 {
            // Exact for the values used here (and nearest below 2^-14 is not needed).
            let b = v.to_bits();
            let sign = ((b >> 16) & 0x8000) as u16;
            let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
            if v == 0.0 {
                return sign;
            }
            if e >= 31 {
                return sign | 0x7c00;
            }
            if e <= 0 {
                return sign;
            }
            sign | ((e as u16) << 10) | (((b >> 13) & 0x3ff) as u16)
        }

        /// The graphics card's conversion matches the CPU's, pixel for pixel, at a desktop's size,
        /// and says how long it takes.
        #[test]
        #[ignore]
        fn an_hdr_desktop_turns_out_as_on_an_sdr_screen() {
            for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
                let (mut device, mut ctx) = (None, None);
                // SAFETY: Direct3D's calls with what they take.
                let made = unsafe { D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx)) };
                if made.is_err() {
                    eprintln!("{driver:?}: no device");
                    continue;
                }
                let (device, ctx) = (device.unwrap(), ctx.unwrap());
                let mut gpu = Gpu::new(&device).unwrap();
                for (w, h, white) in [(1920u32, 1080u32, 2.5f32), (3840, 2160, 2.5), (1280, 720, 1.0)] {
                    // Every shade of SDR content, highlights, colours outside sRGB, nothing.
                    let value = |x: u32, y: u32, c: u32| -> f32 {
                        let t = ((x * 7 + y * 13 + c * 101) % 1024) as f32 / 1023.0;
                        match y % 4 {
                            0 => white * t,
                            1 => white * t * 6.0,
                            2 => white * (t - 0.3),
                            _ => 0.0,
                        }
                    };
                    let px: Vec<u16> = (0..h).flat_map(|y| (0..w).flat_map(move |x| (0..4).map(move |c| if c == 3 { to_half(1.0) } else { to_half(value(x, y, c)) }))).collect();
                    // SAFETY: Direct3D's calls with what they take; the data outlives the call.
                    let (tex, staging) = unsafe {
                        let desc = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R16G16B16A16_FLOAT, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
                        let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr().cast(), SysMemPitch: w * 8, SysMemSlicePitch: 0 };
                        let mut t = None;
                        device.CreateTexture2D(&desc, Some(&init), Some(&mut t)).unwrap();
                        let sd = D3D11_TEXTURE2D_DESC { Format: DXGI_FORMAT_R8G8B8A8_UNORM, Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..desc };
                        let mut s = None;
                        device.CreateTexture2D(&sd, None, Some(&mut s)).unwrap();
                        (t.unwrap(), s.unwrap())
                    };
                    let mut times = Vec::new();
                    let mut got = vec![0u8; (w * h * 4) as usize];
                    for _ in 0..8 {
                        let t = std::time::Instant::now();
                        let out = gpu.convert(&device, &ctx, &tex, w, h, white).unwrap();
                        // SAFETY: the staging texture is mapped for reading and let go after.
                        unsafe {
                            ctx.CopyResource(&staging, &out);
                            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
                            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map)).unwrap();
                            for y in 0..h as usize {
                                let row = std::slice::from_raw_parts((map.pData as *const u8).add(y * map.RowPitch as usize), w as usize * 4);
                                got[y * w as usize * 4..][..w as usize * 4].copy_from_slice(row);
                            }
                            ctx.Unmap(&staging, 0);
                        }
                        times.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let cpu = super::super::Cpu::new(white);
                    let mut by_cpu = vec![0u8; got.len()];
                    let t = std::time::Instant::now();
                    for y in 0..h as usize {
                        cpu.row(&px[y * w as usize * 4..][..w as usize * 4], &mut by_cpu[y * w as usize * 4..][..w as usize * 4]);
                    }
                    let cpu_ms = t.elapsed().as_secs_f64() * 1000.0;
                    let mut worst = 0i32;
                    for (i, p) in got.as_chunks::<4>().0.iter().enumerate() {
                        let (x, y) = (i as u32 % w, i as u32 / w);
                        let want = super::super::pixel([value(x, y, 0), value(x, y, 1), value(x, y, 2)].map(|v| super::super::half(to_half(v))), white);
                        let want = [want[2], want[1], want[0], 255];
                        for c in 0..4 {
                            worst = worst.max((p[c] as i32 - want[c] as i32).abs()).max((by_cpu[i * 4 + c] as i32 - want[c] as i32).abs());
                        }
                    }
                    eprintln!("{driver:?} {w}x{h} white {white}: card {:.2} ms (with reading back), CPU {cpu_ms:.1} ms; at most {worst} off", times[times.len() / 2]);
                    assert!(worst <= 1, "{worst}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdr_content_comes_out_as_it_was() {
        // A shade drawn on the desktop in sRGB, as an HDR desktop holds it at SDR white 2.5,
        // comes back as it was.
        for v in [0u8, 1, 17, 64, 128, 200, 254, 255] {
            let s = v as f32 / 255.0;
            let lin = if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
            assert_eq!(pixel([lin * 2.5; 3], 2.5), [v; 3]);
            let cpu = Cpu::new(2.5);
            let h = to_half(lin * 2.5);
            let mut out = [0u8; 4];
            cpu.row(&[h, h, h, 0x3c00], &mut out);
            assert!(out[..3].iter().all(|&o| (o as i32 - v as i32).abs() <= 1), "{v}: {out:?}");
        }
    }

    #[test]
    fn a_highlight_keeps_its_hue() {
        // Bright orange, four times SDR white: still orange, at full brightness.
        let [r, g, b] = pixel([4.0 * 2.5, 2.0 * 2.5, 0.0], 2.5);
        assert_eq!((r, b), (255, 0));
        assert!((180..=190).contains(&g), "{g}");
        // Clamped channel by channel it would have turned yellow.
        assert_ne!(g, 255);
        // Outside sRGB and not a number: nothing in that channel.
        assert_eq!(pixel([-0.5, f32::NAN, 2.5], 2.5), [0, 0, 255]);
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(half(0x7bff), 65504.0);
    }

    fn to_half(v: f32) -> u16 {
        // Nearest half float for 0 <= v < 65504.
        (0..=0x7bffu16).min_by(|&a, &b| (half(a) - v).abs().partial_cmp(&(half(b) - v).abs()).unwrap()).unwrap()
    }
}
