//! Windows viewer: while the remote window is in front, keys that Windows
//! would act on itself (Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Print Screen) are
//! taken with a low-level keyboard hook and handed to the session instead.
//! Ctrl+Alt+Del cannot be hooked by any program.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_RWIN, VK_SNAPSHOT, VK_TAB,
};
use crate::xk;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION,
    KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_UP, MSG, SetWindowsHookExW, WH_KEYBOARD_LL,
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<Sender<(u32, bool)>>> = Mutex::new(None);
/// The window to wake for a key taken: Windows never tells it of the key, so without a
/// frame the key would wait, and Tab would reach the host after Alt was let go.
static WAKE: Mutex<Option<eframe::egui::Context>> = Mutex::new(None);

fn ours_in_front() -> bool {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid == unsafe { GetCurrentProcessId() }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && ACTIVE.load(Ordering::Relaxed) && ours_in_front() {
        let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = k.flags & LLKHF_UP == 0;
        let alt = k.flags & LLKHF_ALTDOWN != 0;
        let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL as i32) } < 0;
        let vk = k.vkCode as u16;
        let keysym = match vk {
            VK_LWIN => Some(xk::SUPER_L),
            VK_RWIN => Some(xk::SUPER_R),
            VK_TAB if alt => Some(xk::TAB),
            VK_ESCAPE if alt || ctrl => Some(xk::ESCAPE),
            VK_SNAPSHOT => Some(xk::PRINT),
            _ => None,
        };
        if let Some(sym) = keysym
            && let Some(tx) = SINK.lock().unwrap().as_ref()
            && tx.send((sym, down)).is_ok()
        {
            if let Some(ctx) = WAKE.lock().unwrap().as_ref() {
                ctx.request_repaint();
            }
            // Swallowed: Windows never sees it, the remote host does.
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Installs the hook on its own thread: Windows skips a low-level hook whose thread is
/// busy (drawing a frame, say), so that thread does nothing but pump messages.
pub fn install(ctx: eframe::egui::Context) -> Option<Receiver<(u32, bool)>> {
    let (tx, rx) = mpsc::channel();
    *SINK.lock().unwrap() = Some(tx);
    *WAKE.lock().unwrap() = Some(ctx);
    let (ok_tx, ok_rx) = mpsc::channel();
    std::thread::spawn(move || unsafe {
        // A global hook needs a module handle even though a low-level hook runs in this process.
        let h = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), GetModuleHandleW(std::ptr::null()), 0);
        let _ = ok_tx.send(!h.is_null());
        if h.is_null() {
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    });
    ok_rx.recv().unwrap_or(false).then_some(rx)
}

/// Turns grabbing on while a session is shown and the user wants it.
pub fn set_active(on: bool) {
    ACTIVE.store(on, Ordering::Relaxed);
}
