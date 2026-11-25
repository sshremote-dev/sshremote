//! Which graphics interface a window draws with on Windows, and the way back from a graphics
//! driver that dies in it. Direct3D 12 is the first choice; some drivers crash in it as the first
//! frame is made (an Intel UHD 630 driver of 2020 did, and in Vulkan too, while its OpenGL works).
//! A crash before the window's first frames starts the program again on OpenGL, which it keeps
//! from then on (the `renderer` setting).

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

/// What the program starting again after such a crash is told, so it keeps OpenGL.
#[cfg(windows)]
const FELL_BACK: &str = "SSHREMOTE_GPU_FELL_BACK";

/// The window has drawn its first frames: a crash from now on is not the driver refusing to start.
#[cfg(windows)]
static DRAWN: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static FRAMES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Chooses the interface (an explicit WGPU_BACKEND wins) and watches the first frames for a crash.
pub(crate) fn choose() {
    #[cfg(windows)]
    {
        if std::env::var_os(FELL_BACK).is_some() {
            crate::hosts::set_setting("renderer", "gl");
            // SAFETY: set before any thread of the window starts.
            unsafe { std::env::remove_var(FELL_BACK) };
        }
        if std::env::var_os("WGPU_BACKEND").is_none() {
            let gl = crate::hosts::setting("renderer").is_some_and(|r| r == "gl");
            // SAFETY: as above.
            unsafe { std::env::set_var("WGPU_BACKEND", if gl { "gl" } else { "dx12" }) };
        }
        if std::env::var("WGPU_BACKEND").is_ok_and(|b| b != "gl") {
            // SAFETY: a plain call; the filter is a function for the life of the program.
            unsafe { windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(crashed)) };
        }
    }
}

/// A frame is being made: after a few, the interface has shown it works.
pub(crate) fn frame() {
    #[cfg(windows)]
    if FRAMES.fetch_add(1, Ordering::Relaxed) >= 3 {
        DRAWN.store(true, Ordering::Relaxed);
    }
}

/// A crash before the first frames, in a graphics interface other than OpenGL: the same command
/// line again, on OpenGL, and this one ends without the system's error report.
#[cfg(windows)]
unsafe extern "system" fn crashed(_: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    use windows_sys::Win32::System::Environment::{GetCommandLineW, SetEnvironmentVariableW};
    use windows_sys::Win32::System::Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW};
    const CONTINUE_SEARCH: i32 = 0;
    const EXECUTE_HANDLER: i32 = 1;
    if DRAWN.load(Ordering::Relaxed) {
        return CONTINUE_SEARCH;
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    // SAFETY: plain calls with NUL-terminated strings that live through them; the command line
    // is copied, as CreateProcessW may write into it.
    unsafe {
        SetEnvironmentVariableW(wide("WGPU_BACKEND").as_ptr(), wide("gl").as_ptr());
        SetEnvironmentVariableW(wide(FELL_BACK).as_ptr(), wide("1").as_ptr());
        let line = GetCommandLineW();
        let mut n = 0;
        while *line.add(n) != 0 {
            n += 1;
        }
        let mut copy: Vec<u16> = std::slice::from_raw_parts(line, n).iter().copied().chain(Some(0)).collect();
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        CreateProcessW(std::ptr::null(), copy.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, 0, std::ptr::null(), std::ptr::null(), &si, &mut pi);
    }
    EXECUTE_HANDLER
}
