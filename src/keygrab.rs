//! Windows viewer: while the remote window is in front, keys that Windows
//! would act on itself (Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Print Screen) are
//! taken with a low-level keyboard hook and handed to the session instead.
//! Ctrl+Alt+Del cannot be hooked by any program.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_RWIN, VK_SNAPSHOT, VK_TAB,
};
use crate::xk;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_INJECTED, LLKHF_UP, MSG, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP,
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The hook's thread, told to set the hook again (`WM_APP`) as grabbing starts: Windows removes a
/// low-level hook that once took too long, silently, and it would take no key after that.
static HOOK_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Keys whose press was taken: their release is taken too, whatever changed meanwhile (Alt let
/// go before Tab, another window in front), or the host would keep them down.
static TAKEN: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
static SINK: Mutex<Option<Sender<(u32, bool)>>> = Mutex::new(None);
/// Wakes the window for a key taken: Windows never tells it of the key, so without a frame the
/// key would wait, and Tab would reach the host after Alt was let go. A thread of its own does
/// it, as waking takes the window's lock, which a frame may hold for longer than a hook may take.
static WAKE: Mutex<Option<Sender<()>>> = Mutex::new(None);

fn ours_in_front() -> bool {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid == unsafe { GetCurrentProcessId() }
}

/// The key a taken virtual key is at the host.
fn keysym_of(vk: u16) -> Option<u32> {
    match vk {
        VK_LWIN => Some(xk::SUPER_L),
        VK_RWIN => Some(xk::SUPER_R),
        VK_TAB => Some(xk::TAB),
        VK_ESCAPE => Some(xk::ESCAPE),
        VK_SNAPSHOT => Some(xk::PRINT),
        _ => None,
    }
}

/// Hands a key to the session; false if there is none to take it.
fn forward(sym: u32, down: bool) -> bool {
    let sent = SINK.lock().unwrap().as_ref().is_some_and(|tx| tx.send((sym, down)).is_ok());
    if sent && let Some(wake) = WAKE.lock().unwrap().as_ref() {
        let _ = wake.send(());
    }
    sent
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    // Keys a program typed (this computer's own agent among them, which would come straight
    // back) are Windows', never taken.
    if code == HC_ACTION as i32 && k.flags & LLKHF_INJECTED == 0 {
        let down = k.flags & LLKHF_UP == 0;
        let vk = k.vkCode as u16;
        let taken = &TAKEN[(vk & 0xff) as usize];
        if !down && taken.swap(false, Ordering::Relaxed) && keysym_of(vk).is_some_and(|sym| forward(sym, false)) {
            return 1;
        }
        if down && ACTIVE.load(Ordering::Relaxed) && ours_in_front() {
            let alt = k.flags & LLKHF_ALTDOWN != 0;
            let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL as i32) } < 0;
            let wanted = match vk {
                VK_LWIN | VK_RWIN | VK_SNAPSHOT => true,
                VK_TAB => alt,
                VK_ESCAPE => alt || ctrl,
                _ => false,
            };
            if wanted && keysym_of(vk).is_some_and(|sym| forward(sym, true)) {
                // Swallowed: Windows never sees it, the remote host does.
                taken.store(true, Ordering::Relaxed);
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Installs the hook on its own thread: Windows skips a low-level hook whose thread is
/// busy (drawing a frame, say), so that thread does nothing but pump messages.
pub fn install(ctx: eframe::egui::Context) -> Option<Receiver<(u32, bool)>> {
    let (tx, rx) = mpsc::channel();
    *SINK.lock().unwrap() = Some(tx);
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    *WAKE.lock().unwrap() = Some(wake_tx);
    std::thread::spawn(move || {
        while wake_rx.recv().is_ok() {
            ctx.request_repaint();
        }
    });
    let (ok_tx, ok_rx) = mpsc::channel();
    std::thread::spawn(move || unsafe {
        // A global hook needs a module handle even though a low-level hook runs in this process.
        let set = || SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), GetModuleHandleW(std::ptr::null()), 0);
        let mut h: HHOOK = set();
        HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
        let _ = ok_tx.send(!h.is_null());
        if h.is_null() {
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.hwnd.is_null() && msg.message == WM_APP {
                UnhookWindowsHookEx(h);
                h = set();
                continue;
            }
            DispatchMessageW(&msg);
        }
    });
    ok_rx.recv().unwrap_or(false).then_some(rx)
}

/// Turns grabbing on while a session is shown and the user wants it; the hook is set again each
/// time it starts.
pub fn set_active(on: bool) {
    if on && !ACTIVE.swap(true, Ordering::Relaxed) {
        let t = HOOK_THREAD.load(Ordering::Relaxed);
        if t != 0 {
            unsafe { PostThreadMessageW(t, WM_APP, 0, 0) };
        }
    }
    ACTIVE.store(on, Ordering::Relaxed);
}
