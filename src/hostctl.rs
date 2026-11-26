//! What a viewer may do to the computer itself, beyond its screen: restart and lock it, keep its
//! own keyboard and mouse out (on Windows and macOS), hide its screen from whoever sits there
//! while the viewer still sees it (on Windows 10 2004 and later, and macOS), and lock it when the
//! viewer leaves.

/// The acts of `ToAgent::Host`.
pub const QUERY: u8 = 0;
pub const RESTART: u8 = 1;
pub const LOCK: u8 = 2;
pub const BLOCK: u8 = 3;
pub const HIDE: u8 = 4;
pub const LOCK_ON_LEAVE: u8 = 5;

#[cfg(windows)]
pub use crate::sys::INJECTED;

/// The bit of an act in `HostState`'s masks.
pub const fn bit(act: u8) -> u8 {
    1 << act
}

/// What can be done here, as `bit`s.
pub fn can() -> u8 {
    let both = bit(RESTART) | bit(LOCK) | bit(LOCK_ON_LEAVE);
    both | (if os::BLOCKS { bit(BLOCK) } else { 0 }) | (if os::hides() { bit(HIDE) } else { 0 })
}

/// The computer's own state for one viewer; what is left on is undone as the viewer goes.
#[derive(Default)]
pub struct HostCtl {
    block: bool,
    hide: bool,
    lock_on_leave: bool,
}

impl HostCtl {
    /// Does `act` (turning it `on` or off for the switches); what to tell the viewer.
    pub fn act(&mut self, act: u8, on: bool) -> Result<String, String> {
        match act {
            RESTART => restart().map(|()| "The computer restarts now; this connection comes back when it is up again, if remote access starts with it.".into()),
            LOCK => lock().map(|()| "The computer is locked.".into()),
            LOCK_ON_LEAVE => {
                self.lock_on_leave = on;
                Ok(if on { "The computer locks when you leave." } else { "The computer stays as it is when you leave." }.into())
            }
            BLOCK => {
                self.block = on;
                self.apply()?;
                Ok(if on { "Its own keyboard and mouse are kept out (Ctrl+P there takes them back)." } else { "Its own keyboard and mouse work again." }.into())
            }
            HIDE => {
                self.hide = on;
                self.apply()?;
                Ok(if on { "Its screen is dark for whoever sits there; you still see it. Its keyboard and mouse are kept out meanwhile." } else { "Its screen shows again." }.into())
            }
            _ => Err("this computer does not know that".into()),
        }
    }

    /// What is on now, as `bit`s.
    pub fn on(&self) -> u8 {
        (if self.block { bit(BLOCK) } else { 0 }) | (if self.hide { bit(HIDE) } else { 0 }) | if self.lock_on_leave { bit(LOCK_ON_LEAVE) } else { 0 }
    }

    /// Whether the person at the computer took its keyboard and mouse back (Ctrl+P) since last asked.
    pub fn taken_back(&mut self) -> bool {
        if !os::taken_back() {
            return false;
        }
        (self.block, self.hide) = (false, false);
        let _ = self.apply();
        true
    }

    /// Hiding the screen keeps the keyboard and mouse out too.
    fn apply(&mut self) -> Result<(), String> {
        let r = os::set(self.block || self.hide, self.hide);
        if r.is_err() {
            (self.block, self.hide) = (false, false);
            let _ = os::set(false, false);
        }
        r
    }
}

impl Drop for HostCtl {
    fn drop(&mut self) {
        if self.block || self.hide {
            let _ = os::set(false, false);
        }
        if self.lock_on_leave {
            let _ = lock();
        }
    }
}

#[cfg(windows)]
fn restart() -> Result<(), String> {
    let out = crate::sys::quiet(std::process::Command::new("shutdown.exe").args(["/r", "/t", "0"])).output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(format!("the computer did not restart: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

/// As the Apple menu's Restart: open programs are asked to quit first.
#[cfg(target_os = "macos")]
fn restart() -> Result<(), String> {
    let out = std::process::Command::new("osascript").args(["-e", "tell application \"loginwindow\" to «event aevtrrst»"]).output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(format!("the computer did not restart: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn restart() -> Result<(), String> {
    let out = std::process::Command::new("systemctl").arg("reboot").output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(format!("the computer did not restart: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

/// Locks the screen of the person signed in there, as them.
#[cfg(windows)]
fn lock() -> Result<(), String> {
    let line = r#""C:\Windows\System32\rundll32.exe" user32.dll,LockWorkStation"#;
    match crate::console_user::acting() {
        crate::console_user::Acting::User(u) => crate::console_user::spawn_piped(&u, line).map(drop),
        crate::console_user::Acting::Nobody => Ok(()),
        crate::console_user::Acting::Myself => crate::sys::quiet(std::process::Command::new(r"C:\Windows\System32\rundll32.exe").arg("user32.dll,LockWorkStation"))
            .status()
            .map_err(|e| e.to_string())
            .and_then(|s| if s.success() { Ok(()) } else { Err("the computer did not lock".into()) }),
    }
}

/// Locks the screen as the display going to sleep does (macOS asks for the password then).
#[cfg(target_os = "macos")]
fn lock() -> Result<(), String> {
    let out = std::process::Command::new("pmset").arg("displaysleepnow").output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err("the screen could not be locked here".into()) }
}

/// Locks the desktop session's screen through its screen saver, as GNOME and others offer it.
#[cfg(all(unix, not(target_os = "macos")))]
fn lock() -> Result<(), String> {
    crate::capture::ensure_session_env();
    let call = |dest: &str, path: &str, method: &str| {
        std::process::Command::new("gdbus")
            .args(["call", "--session", "--dest", dest, "--object-path", path, "--method", method])
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if call("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver.Lock")
        || call("org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver", "org.freedesktop.ScreenSaver.Lock")
    {
        return Ok(());
    }
    let out = std::process::Command::new("loginctl").arg("lock-session").output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err("the screen could not be locked here".into()) }
}

#[cfg(target_os = "macos")]
mod os {
    pub use crate::capture::macprivacy::{hides, set, taken_back};
    pub const BLOCKS: bool = true;
}

#[cfg(not(any(windows, target_os = "macos")))]
mod os {
    pub const BLOCKS: bool = false;

    pub fn hides() -> bool {
        false
    }

    pub fn set(block: bool, hide: bool) -> Result<(), String> {
        if block || hide {
            return Err("not on this computer: its desktop does not let a program keep out its keyboard or hide its screen".into());
        }
        Ok(())
    }

    pub fn taken_back() -> bool {
        false
    }
}

/// One thread with a message loop holds the low-level keyboard and mouse hooks that drop the
/// computer's own input (not what this program injects), and the cover: a black window above all
/// others that lets clicks through and that screen capture leaves out.
#[cfg(windows)]
mod os {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, PAINTSTRUCT,
        SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_LCONTROL, VK_RCONTROL};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW, GetSystemMetrics, HC_ACTION,
        HHOOK, HWND_TOPMOST, KBDLLHOOKSTRUCT, LWA_ALPHA, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowDisplayAffinity,
        SetWindowPos, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_PAINT,
        WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        WS_POPUP,
    };

    use crate::sys::wide;

    pub const BLOCKS: bool = true;

    pub fn hides() -> bool {
        true
    }

    /// Leaves a window out of screen capture (Windows 10 2004 and later).
    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;
    const APPLY: u32 = WM_APP + 1;

    static BLOCK: AtomicBool = AtomicBool::new(false);
    static CTRL: AtomicBool = AtomicBool::new(false);
    static TAKEN_BACK: AtomicBool = AtomicBool::new(false);
    static THREAD: AtomicU32 = AtomicU32::new(0);
    /// The cover's outcome, as the thread reports it after each change.
    static DONE: Mutex<Option<std::sync::mpsc::Sender<Result<(), String>>>> = Mutex::new(None);
    static HIDE: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && BLOCK.load(Ordering::Relaxed) {
            let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            if k.dwExtraInfo != super::INJECTED {
                let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let up = matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP);
                if [VK_CONTROL, VK_LCONTROL, VK_RCONTROL].contains(&(k.vkCode as u16)) && (down || up) {
                    CTRL.store(down, Ordering::Relaxed);
                }
                // Ctrl+P at the computer takes its keyboard and mouse back.
                if down && k.vkCode == 'P' as u32 && CTRL.load(Ordering::Relaxed) {
                    BLOCK.store(false, Ordering::Relaxed);
                    TAKEN_BACK.store(true, Ordering::Relaxed);
                }
                return 1;
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && BLOCK.load(Ordering::Relaxed) {
            let m = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
            if m.dwExtraInfo != super::INJECTED {
                return 1;
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    unsafe extern "system" fn cover_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_PAINT => unsafe {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                let mut r: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut r);
                let black = CreateSolidBrush(0);
                FillRect(dc, &r, black);
                DeleteObject(black as _);
                let face = wide("Segoe UI");
                let font = CreateFontW(28, 0, 0, 0, 400, 0, 0, 0, 0, 0, 0, 5, 0, face.as_ptr());
                let old = SelectObject(dc, font as _);
                SetBkMode(dc, TRANSPARENT as _);
                SetTextColor(dc, 0x0080_8080);
                let mut text = wide("This computer is being used remotely");
                DrawTextW(dc, text.as_mut_ptr(), -1, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                SelectObject(dc, old);
                DeleteObject(font as _);
                EndPaint(hwnd, &ps);
                0
            },
            // Above whatever else asks to be on top since.
            WM_TIMER => unsafe {
                SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, 0x0001 | 0x0002 | SWP_NOACTIVATE);
                0
            },
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// The cover over every screen, or why there is none.
    unsafe fn cover() -> Result<HWND, String> {
        unsafe {
            let class = wide("SshRemoteCover");
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW { lpfnWndProc: Some(cover_proc), hInstance: instance, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
            RegisterClassW(&wc);
            let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
            let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_NOACTIVATE;
            let title = wide("SSH Remote");
            let hwnd = CreateWindowExW(ex, class.as_ptr(), title.as_ptr(), WS_POPUP, x, y, w, h, std::ptr::null_mut(), std::ptr::null_mut(), instance, std::ptr::null());
            if hwnd.is_null() {
                return Err(format!("the cover could not be made: {}", std::io::Error::last_os_error()));
            }
            SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
            // Without being left out of the capture the viewer would see black too: no cover then.
            if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) == 0 {
                DestroyWindow(hwnd);
                return Err("hiding the screen needs Windows 10 version 2004 or later".into());
            }
            SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            SetTimer(hwnd, 1, 1000, None);
            Ok(hwnd)
        }
    }

    /// The thread: hooks while it lives, the cover while asked for.
    fn run() {
        unsafe {
            THREAD.store(GetCurrentThreadId(), Ordering::Release);
            let instance = GetModuleHandleW(std::ptr::null());
            let kb: HHOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), instance, 0);
            let ms: HHOOK = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), instance, 0);
            let mut shown: HWND = std::ptr::null_mut();
            let mut msg: MSG = std::mem::zeroed();
            // A first message makes the queue, then the request waiting for it is answered.
            let mut first = true;
            loop {
                if first {
                    first = false;
                    msg.message = APPLY;
                } else if GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) <= 0 {
                    break;
                }
                if msg.message == APPLY && msg.hwnd.is_null() {
                    let hide = HIDE.load(Ordering::Relaxed);
                    let r = if kb.is_null() || ms.is_null() {
                        Err(format!("the keyboard and mouse could not be kept out: {}", std::io::Error::last_os_error()))
                    } else if hide && shown.is_null() {
                        cover().map(|h| shown = h)
                    } else {
                        if !hide && !shown.is_null() {
                            DestroyWindow(shown);
                            shown = std::ptr::null_mut();
                        }
                        Ok(())
                    };
                    if let Some(tx) = DONE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                        let _ = tx.send(r);
                    }
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if !shown.is_null() {
                DestroyWindow(shown);
            }
            UnhookWindowsHookEx(kb);
            UnhookWindowsHookEx(ms);
            THREAD.store(0, Ordering::Release);
        }
    }

    /// Keeps the computer's own input out (`block`) and its screen covered (`hide`), or lets them be.
    pub fn set(block: bool, hide: bool) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        *DONE.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        HIDE.store(hide, Ordering::Relaxed);
        BLOCK.store(block, Ordering::Relaxed);
        TAKEN_BACK.store(false, Ordering::Relaxed);
        let t = THREAD.load(Ordering::Acquire);
        if !block && !hide {
            if t != 0 {
                unsafe { PostThreadMessageW(t, WM_QUIT, 0, 0) };
                // Gone before anything else is asked of it.
                for _ in 0..100 {
                    if THREAD.load(Ordering::Acquire) == 0 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            return Ok(());
        }
        if t == 0 {
            std::thread::spawn(run);
        } else {
            unsafe { PostThreadMessageW(t, APPLY, 0, 0) };
        }
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_else(|_| Err("the computer did not answer".into()))
    }

    pub fn taken_back() -> bool {
        TAKEN_BACK.swap(false, Ordering::Relaxed)
    }
}
