//! Desktop Duplication (DXGI): the system hands over a monitor's picture only when something on it
//! changed, so an idle screen costs nothing, where GDI copies the whole screen for every frame.
//! The picture lands in a buffer of the monitor's size, from which the GDI path scales, crops and
//! draws the pointer as before. A thread of its own waits for each change and reads it back
//! (reading back waits most of a frame for the graphics card), so the agent's loop is not held up
//! and need not look every millisecond. An HDR desktop is asked for as it is, in floats, and
//! turned into what an SDR screen shows (`hdr`).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_MODE_ROTATION_IDENTITY};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1,
    IDXGIFactory1, IDXGIOutput1, IDXGIOutput5, IDXGIOutput6, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

/// What the next look at the monitor found.
pub enum Next {
    /// Nothing changed since the last look.
    Same,
    /// Only the pointer moved or changed; the picture is as before.
    Pointer,
    /// A new picture was copied into the buffer.
    Picture,
}

/// A monitor's duplication on a thread of its own.
pub struct Watch {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The monitor's size in pixels.
    pub size: (usize, usize),
}

struct Shared {
    state: Mutex<State>,
    wake: super::Waker,
    /// The least time between two pictures read back, in milliseconds: the viewer's pace.
    gap: AtomicU32,
}

#[derive(Default)]
struct State {
    /// A picture read back and not yet taken, and a buffer the next may be read into.
    ready: Option<Vec<u8>>,
    free: Option<Vec<u8>>,
    pointer: bool,
    lost: bool,
    stop: bool,
}

/// How long the thread waits for a change at a time, before seeing whether it is to stop.
const LOOK: u32 = 25;

impl Watch {
    /// Duplicates the monitor at (`left`, `top`) on a new thread, which first takes the input
    /// desktop with `desktop` (duplication works only there).
    pub fn open(left: i32, top: i32, wake: super::Waker, gap_ms: u32, desktop: fn() -> bool) -> Result<Watch, String> {
        let shared = Arc::new(Shared { state: Mutex::default(), wake, gap: AtomicU32::new(gap_ms) });
        let (tx, rx) = mpsc::sync_channel(1);
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("duplication".into())
            .spawn(move || {
                desktop();
                match Dup::open(left, top) {
                    Ok(mut dup) => {
                        let _ = tx.send(Ok(dup.size));
                        s.run(&mut dup);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        match rx.recv().unwrap_or_else(|_| Err("the duplication thread ended".into())) {
            Ok(size) => Ok(Watch { shared, thread: Some(thread), size }),
            Err(e) => {
                let _ = thread.join();
                Err(e)
            }
        }
    }

    /// What came since the last look: a new picture is swapped into `picture` (the monitor's
    /// rows, `size.0 * 4` bytes each), which then holds the latest one.
    pub fn take(&self, picture: &mut Vec<u8>) -> Result<Next, Lost> {
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.lost {
            return Err(Lost);
        }
        let pointer = std::mem::take(&mut st.pointer);
        if let Some(new) = st.ready.take() {
            st.free = Some(std::mem::replace(picture, new));
            return Ok(Next::Picture);
        }
        Ok(if pointer { Next::Pointer } else { Next::Same })
    }

    pub fn set_gap(&self, ms: u32) {
        self.shared.gap.store(ms, Ordering::Relaxed);
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        // Within one look: a duplication of the monitor still open would keep a new one from opening.
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Shared {
    fn run(&self, dup: &mut Dup) {
        let len = dup.size.0 * dup.size.1 * 4;
        let mut spare = vec![0u8; len];
        loop {
            if self.state.lock().unwrap_or_else(|e| e.into_inner()).stop {
                return;
            }
            let got = dup.next(LOOK, &mut spare, dup.size.0 * 4);
            {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match got {
                    Ok(Next::Same) => continue,
                    Ok(Next::Pointer) => st.pointer = true,
                    Ok(Next::Picture) => {
                        let old = st.ready.replace(spare);
                        spare = old.or_else(|| st.free.take()).unwrap_or_default();
                        spare.resize(len, 0);
                    }
                    Err(Lost) => st.lost = true,
                }
            }
            super::wake(&self.wake);
            if matches!(got, Err(Lost)) {
                return;
            }
            // Changes meanwhile gather into the next picture: none is lost by waiting. Slept in
            // short steps, so a Watch dropped meanwhile is not kept waiting for a slow viewer's gap.
            if matches!(got, Ok(Next::Picture)) {
                let until = dup.got + Duration::from_millis(self.gap.load(Ordering::Relaxed) as u64);
                while let Some(left) = until.checked_duration_since(Instant::now()).filter(|l| !l.is_zero()) {
                    if self.state.lock().unwrap_or_else(|e| e.into_inner()).stop {
                        return;
                    }
                    std::thread::sleep(left.min(Duration::from_millis(10)));
                }
            }
        }
    }
}

/// The duplication of one monitor.
struct Dup {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32, DXGI_FORMAT)>,
    size: (usize, usize),
    /// When the last change was handed over.
    got: Instant,
    /// The monitor's name ("\\.\DISPLAY1"), which its SDR white is asked by.
    name: [u16; 32],
    /// An HDR desktop's conversion on the graphics card (an error: done on the CPU), its white
    /// and when that was last looked at (the person may move the slider meanwhile).
    gpu: Option<Result<super::hdr::Gpu, String>>,
    cpu: Option<super::hdr::Cpu>,
    white: Option<(f32, Instant)>,
}

/// The duplication stopped (desktop switch, mode change, another program took it): open again.
pub struct Lost;

impl Dup {
    /// Duplicates the monitor whose top-left corner is at (`left`, `top`) on the virtual desktop.
    fn open(left: i32, top: i32) -> Result<Dup, String> {
        Dup::open_as(left, top, true)
    }

    /// `floats`: an HDR desktop is asked for as it is (else Windows clips it to 8 bits).
    fn open_as(left: i32, top: i32, floats: bool) -> Result<Dup, String> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
            let mut a = 0;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                a += 1;
                let mut o = 0;
                while let Ok(output) = adapter.EnumOutputs(o) {
                    o += 1;
                    let Ok(desc) = output.GetDesc() else { continue };
                    let r = desc.DesktopCoordinates;
                    if (r.left, r.top) != (left, top) || desc.Rotation != DXGI_MODE_ROTATION_IDENTITY {
                        continue;
                    }
                    let (device, ctx) = device_for(&adapter)?;
                    // In floats where the desktop is (HDR), as Windows 10 1703 and later can;
                    // else in 8 bits as before.
                    let floats = (floats && output.cast::<IDXGIOutput6>().is_ok()).then(|| output.cast::<IDXGIOutput5>().ok()).flatten();
                    let dup = match floats.and_then(|o| o.DuplicateOutput1(&device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM]).ok()) {
                        Some(d) => d,
                        None => {
                            let output1: IDXGIOutput1 = output.cast().map_err(|e| format!("DXGI output: {e}"))?;
                            output1.DuplicateOutput(&device).map_err(|e| format!("Desktop Duplication: {e}"))?
                        }
                    };
                    let size = ((r.right - r.left) as usize, (r.bottom - r.top) as usize);
                    return Ok(Dup { device, ctx, dup, staging: None, size, got: Instant::now(), name: desc.DeviceName, gpu: None, cpu: None, white: None });
                }
            }
            Err("no DXGI output at that place".into())
        }
    }

    /// Waits up to `wait` ms for a change and, when the picture changed, copies it into `dst`
    /// (BGRA rows of `stride` bytes, at least the monitor's size).
    fn next(&mut self, wait: u32, dst: &mut [u8], stride: usize) -> Result<Next, Lost> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match self.dup.AcquireNextFrame(wait, &mut info, &mut res) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Next::Same),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Err(Lost),
                Err(_) => return Err(Lost),
            }
            self.got = Instant::now();
            // No new picture, only the pointer.
            if info.LastPresentTime == 0 {
                let _ = self.dup.ReleaseFrame();
                return Ok(if info.LastMouseUpdateTime != 0 { Next::Pointer } else { Next::Same });
            }
            let copied = self.copy(res, dst, stride);
            let _ = self.dup.ReleaseFrame();
            copied.map(|_| Next::Picture).map_err(|_| Lost)
        }
    }

    /// Where SDR white sits on an HDR desktop now, looked at again each second.
    fn white(&mut self) -> f32 {
        match self.white {
            Some((w, at)) if at.elapsed() < Duration::from_secs(1) => w,
            _ => {
                let w = super::hdr::white(&self.name);
                self.white = Some((w, Instant::now()));
                w
            }
        }
    }

    unsafe fn copy(&mut self, res: Option<IDXGIResource>, dst: &mut [u8], stride: usize) -> Result<(), String> {
        unsafe {
            let mut tex: ID3D11Texture2D = res.ok_or("no frame")?.cast().map_err(|e| e.to_string())?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            // An HDR desktop: turned to 8 bits on the graphics card, else read in floats and
            // turned here.
            if desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT {
                let white = self.white();
                let gpu = self.gpu.get_or_insert_with(|| super::hdr::Gpu::new(&self.device));
                if let Ok(g) = gpu {
                    match g.convert(&self.device, &self.ctx, &tex, desc.Width, desc.Height, white) {
                        Ok(t) => {
                            tex = t;
                            tex.GetDesc(&mut desc);
                        }
                        Err(e) => *gpu = Err(e),
                    }
                }
                if desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT && self.cpu.as_ref().is_none_or(|c| c.white() != white) {
                    self.cpu = Some(super::hdr::Cpu::new(white));
                }
            }
            if self.staging.as_ref().is_none_or(|s| (s.1, s.2, s.3) != (desc.Width, desc.Height, desc.Format)) {
                let mut sd = desc;
                sd.Usage = D3D11_USAGE_STAGING;
                sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                sd.BindFlags = 0;
                sd.MiscFlags = 0;
                sd.MipLevels = 1;
                sd.ArraySize = 1;
                let mut t = None;
                self.device.CreateTexture2D(&sd, None, Some(&mut t)).map_err(|e| e.to_string())?;
                self.staging = Some((t.ok_or("no staging texture")?, desc.Width, desc.Height, desc.Format));
            }
            let (staging, w, h, format) = self.staging.as_ref().expect("just made");
            self.ctx.CopyResource(staging, &tex);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut map)).map_err(|e| e.to_string())?;
            let (w, h) = ((*w as usize).min(self.size.0), (*h as usize).min(self.size.1));
            let src = map.pData as *const u8;
            let floats = (*format == DXGI_FORMAT_R16G16B16A16_FLOAT).then_some(self.cpu.as_ref()).flatten();
            for y in 0..h.min(dst.len() / stride.max(1)) {
                let to = &mut dst[y * stride..y * stride + w * 4];
                match floats {
                    Some(cpu) => cpu.row(std::slice::from_raw_parts(src.add(y * map.RowPitch as usize) as *const u16, w * 4), to),
                    None => to.copy_from_slice(std::slice::from_raw_parts(src.add(y * map.RowPitch as usize), w * 4)),
                }
            }
            self.ctx.Unmap(staging, 0);
            Ok(())
        }
    }
}

unsafe fn device_for(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    unsafe {
        let (mut device, mut ctx) = (None, None);
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(|e| format!("Direct3D 11: {e}"))?;
        Ok((device.ok_or("no Direct3D device")?, ctx.ok_or("no Direct3D context")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A window of one colour on the screen at (`x`, `y`), moved a pixel to and fro until
    /// `stop`, so each look finds a new picture.
    fn painted(x: i32, y: i32, rgb: [i32; 3], stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
        use windows_sys::Win32::Graphics::Gdi::CreateSolidBrush;
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        std::thread::spawn(move || {
            // SAFETY: a window of this thread's, its messages handled here until it goes.
            unsafe {
                let name: Vec<u16> = "sshremote-hdr-test\0".encode_utf16().collect();
                let class = WNDCLASSW { lpfnWndProc: Some(DefWindowProcW), lpszClassName: name.as_ptr(), hbrBackground: CreateSolidBrush((rgb[0] | rgb[1] << 8 | rgb[2] << 16) as u32), ..std::mem::zeroed() };
                RegisterClassW(&class);
                let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, name.as_ptr(), name.as_ptr(), WS_POPUP | WS_VISIBLE, x, y, 400, 300, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
                let mut n = 0;
                while !stop.load(Ordering::Relaxed) {
                    let mut msg = std::mem::zeroed();
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    n += 1;
                    SetWindowPos(hwnd, HWND_TOPMOST, x + n % 2, y, 400, 300, SWP_NOACTIVATE);
                    std::thread::sleep(Duration::from_millis(50));
                }
                DestroyWindow(hwnd);
            }
        })
    }

    /// On a real HDR desktop (SSHREMOTE_TEST_HDR_AT "X,Y": the monitor's corner), SDR content in
    /// a colour comes out in that colour, on the graphics card and on the CPU alike; asked for
    /// the old way, it does not.
    #[test]
    #[ignore]
    fn an_hdr_desktop_comes_out_as_it_looks() {
        let at: Vec<i32> = std::env::var("SSHREMOTE_TEST_HDR_AT").expect("SSHREMOTE_TEST_HDR_AT").split(',').map(|n| n.trim().parse().unwrap()).collect();
        let rgb = [128, 64, 32];
        let middle = |floats: bool, cpu: bool| {
            let mut dup = Dup::open_as(at[0], at[1], floats).unwrap();
            if cpu {
                dup.gpu = Some(Err("on the CPU".into()));
            }
            let (w, h) = dup.size;
            let stop = Arc::new(AtomicBool::new(false));
            let window = painted(at[0] + w as i32 / 2 - 200, at[1] + h as i32 / 2 - 150, rgb, stop.clone());
            let mut px = vec![0u8; w * h * 4];
            let (t, mut pictures, mut seen) = (Instant::now(), 0, [0; 4]);
            // The last of a second's pictures, the window shown by then.
            while t.elapsed() < Duration::from_millis(1500) || pictures == 0 {
                let got = dup.next(100, &mut px, w * 4);
                seen[match got { Ok(Next::Same) => 0, Ok(Next::Pointer) => 1, Ok(Next::Picture) => 2, Err(Lost) => 3 }] += 1;
                if matches!(got, Ok(Next::Picture)) && t.elapsed() > Duration::from_millis(500) {
                    pictures += 1;
                }
                assert!(t.elapsed() < Duration::from_secs(10), "no picture: same, pointer, picture, lost {seen:?}");
            }
            stop.store(true, Ordering::Relaxed);
            window.join().unwrap();
            let i = (h / 2 * w + w / 2) * 4;
            let got = [px[i + 2], px[i + 1], px[i]];
            eprintln!("floats {floats} cpu {cpu}: {got:?} after {pictures} pictures; white {:?}, card {}", dup.white.map(|w| w.0), match &dup.gpu { None => "unused".to_string(), Some(Ok(_)) => "used".into(), Some(Err(e)) => e.clone() });
            (got, dup.white.map(|w| w.0))
        };
        let near = |got: [u8; 3]| got.iter().zip(&rgb).all(|(g, w)| (*g as i32 - w).abs() <= 2);
        let ((gpu, white), (cpu, _), (old, _)) = (middle(true, false), middle(true, true), middle(false, false));
        assert!(white.is_some_and(|w| w > 1.0), "the desktop came in floats, with its SDR white");
        assert!(near(gpu), "{gpu:?}, not {rgb:?}");
        assert!(near(cpu), "{cpu:?}, not {rgb:?}");
        assert!(!near(old), "the old way clips it: {old:?}");
    }
}
