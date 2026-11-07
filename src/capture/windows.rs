//! Windows backend: the monitor's picture from Desktop Duplication when it changed (GDI copies
//! when duplication is not available: another session, the secure desktop, a window across
//! monitors), scaled by StretchBlt when a smaller target is asked for, and SendInput for
//! keyboard and mouse.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, COLORONCOLOR, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, EnumDisplayMonitors, GdiFlush, GetDC, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR,
    MONITORINFO, MONITORINFOEXW, ReleaseDC, SRCCOPY, SelectObject, SetStretchBltMode, StretchBlt,
};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop, SetThreadDesktop};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW,
    SendInput, VIRTUAL_KEY, VK_APPS, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1,
    VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PAUSE, VK_PRIOR,
    VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SNAPSHOT, VK_TAB, VK_UP, VkKeyScanW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, ICONINFO,
    MONITORINFOF_PRIMARY, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};
use windows_sys::core::BOOL;

use super::winlist::{self, R, Scope};
use super::{Capture, Image, Input, Monitor, Share, WindowInfo};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

const DESKTOP_ALL: u32 = 0x01ff;

/// A monitor as Windows places it on the virtual desktop.
struct Screen {
    info: Monitor,
    left: i32,
    top: i32,
}

unsafe extern "system" fn add_monitor(m: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
    let list = unsafe { &mut *(list as *mut Vec<Screen>) };
    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(m, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) } != 0 {
        let r = mi.monitorInfo.rcMonitor;
        let n = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
        let name = String::from_utf16_lossy(&mi.szDevice[..n]).trim_start_matches(r"\\.\").to_string();
        list.push(Screen {
            info: Monitor {
                name,
                w: (r.right - r.left) as usize,
                h: (r.bottom - r.top) as usize,
                primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            },
            left: r.left,
            top: r.top,
        });
    }
    1
}

fn screens() -> Vec<Screen> {
    let mut list: Vec<Screen> = Vec::new();
    unsafe { EnumDisplayMonitors(null_mut(), null_mut(), Some(add_monitor), &mut list as *mut Vec<Screen> as LPARAM) };
    // Primary first, then left to right, so index 0 is what a single-screen viewer expects.
    list.sort_by_key(|s| (!s.info.primary, s.left, s.top));
    list
}

pub struct Gdi {
    screens: Vec<Screen>,
    current: usize,
    origin: (i32, i32),
    desk: (usize, usize),
    target: (usize, usize),
    screen: HDC,
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    /// The whole monitor, or a window or program on it.
    scope: Scope,
    /// The monitor's place, kept while a window decides `origin` and `sshremote`.
    monitor: ((i32, i32), (usize, usize)),
    /// Where the shared windows show, in captured pixels; empty when hidden.
    shown: Vec<R>,
    /// Whether the pointer was last moved over a shared window.
    pointer_inside: bool,
    /// The shared window closed and the whole screen came back; the viewer is told once.
    ended: bool,
    /// Desktop Duplication of the monitor, and the monitor-sized picture it fills.
    dup: Option<super::dxgi::Dup>,
    base: Option<Dib>,
    /// The duplicated picture is complete (its first frame arrived).
    dup_ready: bool,
    /// When duplication may be tried again after it failed.
    dup_retry: std::time::Instant,
    /// The next grab returns a picture even if the screen did not change (new size or scope).
    force: bool,
}

/// A device-independent bitmap selected into its own memory DC.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: usize,
    h: usize,
}

impl Dib {
    fn new(screen: HDC, w: usize, h: usize) -> Option<Dib> {
        unsafe {
            let dc = CreateCompatibleDC(screen);
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits = null_mut();
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bmp);
            Some(Dib { dc, bmp, old, bits: bits as *mut u8, w, h })
        }
    }

    fn pixels(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.w * self.h * 4) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

impl Gdi {
    pub fn open() -> Result<Gdi, String> {
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            follow_input_desktop();
            let screens = screens();
            let desk = match screens.first() {
                Some(s) => (s.info.w, s.info.h),
                None => (GetSystemMetrics(SM_CXSCREEN) as usize, GetSystemMetrics(SM_CYSCREEN) as usize),
            };
            let origin = screens.first().map_or((0, 0), |s| (s.left, s.top));
            let screen = GetDC(null_mut());
            if screen.is_null() || desk.0 == 0 {
                return Err("no interactive desktop in this session".into());
            }
            let mem = CreateCompatibleDC(screen);
            let mut g = Gdi {
                screens,
                current: 0,
                origin,
                desk,
                target: (0, 0),
                screen,
                mem,
                bmp: null_mut(),
                old: null_mut(),
                bits: null_mut(),
                scope: Scope::Screen,
                monitor: (origin, desk),
                shown: Vec::new(),
                pointer_inside: true,
                ended: false,
                dup: None,
                base: None,
                dup_ready: false,
                dup_retry: std::time::Instant::now(),
                force: true,
            };
            g.set_target(desk.0, desk.1)?;
            Ok(g)
        }
    }

    fn free_bitmap(&mut self) {
        unsafe {
            if !self.bmp.is_null() {
                SelectObject(self.mem, self.old);
                DeleteObject(self.bmp);
                self.bmp = null_mut();
            }
        }
    }

    /// Copies the captured area into the target bitmap, from the screen or from the duplicated
    /// picture (`src` with its top-left at `at` on the virtual desktop).
    fn blit_from(&self, src: HDC, at: (i32, i32)) -> bool {
        let (w, h) = (self.target.0 as i32, self.target.1 as i32);
        let (dw, dh) = (self.desk.0 as i32, self.desk.1 as i32);
        let (x, y) = (self.origin.0 - at.0, self.origin.1 - at.1);
        unsafe {
            if (w, h) == (dw, dh) {
                BitBlt(self.mem, 0, 0, w, h, src, x, y, SRCCOPY) != 0
            } else {
                SetStretchBltMode(self.mem, COLORONCOLOR);
                StretchBlt(self.mem, 0, 0, w, h, src, x, y, dw, dh, SRCCOPY) != 0
            }
        }
    }

    fn blit(&self) -> bool {
        self.blit_from(self.screen, (0, 0))
    }

    /// Whether the captured area lies on the monitor being duplicated.
    fn on_monitor(&self) -> bool {
        let ((ml, mt), (mw, mh)) = self.monitor;
        self.origin.0 >= ml
            && self.origin.1 >= mt
            && self.origin.0 + self.desk.0 as i32 <= ml + mw as i32
            && self.origin.1 + self.desk.1 as i32 <= mt + mh as i32
    }

    /// Looks at the duplicated monitor: Some(true) when its picture or pointer changed, Some(false)
    /// when nothing did, None when duplication cannot serve this frame (GDI does instead).
    fn poll_dup(&mut self) -> Option<bool> {
        if !self.on_monitor() {
            return None;
        }
        if self.dup.is_none() {
            if std::time::Instant::now() < self.dup_retry {
                return None;
            }
            let ((ml, mt), _) = self.monitor;
            match super::dxgi::Dup::open(ml, mt) {
                Ok(d) => {
                    self.base = Dib::new(self.screen, d.size.0, d.size.1);
                    self.dup = self.base.is_some().then_some(d);
                    self.dup_ready = false;
                }
                Err(_) => {}
            }
            if self.dup.is_none() {
                self.dup_retry = std::time::Instant::now() + std::time::Duration::from_secs(2);
                return None;
            }
        }
        let (dup, base) = (self.dup.as_mut()?, self.base.as_mut()?);
        let stride = base.w * 4;
        match dup.next(base.pixels(), stride) {
            Ok(super::dxgi::Next::Same) => Some(false),
            Ok(super::dxgi::Next::Pointer) => Some(true),
            Ok(super::dxgi::Next::Picture) => {
                self.dup_ready = true;
                Some(true)
            }
            Err(super::dxgi::Lost) => {
                // A desktop switch or mode change: open again on the next frame, GDI meanwhile.
                (self.dup, self.base, self.dup_ready) = (None, None, false);
                None
            }
        }
    }

    /// Follows the shared windows: the captured area and the parts that show them.
    fn update_layout(&mut self) {
        if self.scope == Scope::Screen {
            return;
        }
        let screen = unsafe {
            let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            R { l: x, t: y, r: x + GetSystemMetrics(SM_CXVIRTUALSCREEN), b: y + GetSystemMetrics(SM_CYVIRTUALSCREEN) }
        };
        match winlist::layout(&self.scope, screen) {
            Some(l) => {
                self.origin = (l.area.l, l.area.t);
                self.desk = ((l.area.r - l.area.l) as usize, (l.area.b - l.area.t) as usize);
                self.shown = l
                    .shown
                    .iter()
                    .map(|r| R { l: r.l - l.area.l, t: r.t - l.area.t, r: r.r - l.area.l, b: r.b - l.area.t })
                    .collect();
            }
            None => {
                let gone = match &self.scope {
                    Scope::Window(h) => !winlist::exists(*h),
                    Scope::App(exe) => !winlist::app_running(exe),
                    Scope::Screen => false,
                };
                if gone {
                    // Closed: back to the whole screen rather than a frozen last picture.
                    self.scope = Scope::Screen;
                    (self.origin, self.desk) = self.monitor;
                    self.pointer_inside = true;
                    self.shown.clear();
                    self.ended = true;
                } else {
                    // Minimized: the area stays, all of it hidden, until it comes back.
                    self.shown.clear();
                }
            }
        }
    }

    /// Greys out what is not part of the shared windows.
    fn mask(&self, data: &mut [u8]) {
        let (tw, th) = self.target;
        let (dw, dh) = (self.desk.0.max(1), self.desk.1.max(1));
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for y in 0..th {
            let yd = (y * dh / th) as i32;
            spans.clear();
            for r in self.shown.iter().filter(|r| yd >= r.t && yd < r.b) {
                let x0 = (r.l.max(0) as usize * tw / dw).min(tw);
                let x1 = ((r.r.max(0) as usize * tw).div_ceil(dw)).min(tw);
                spans.push((x0, x1));
            }
            spans.sort_unstable();
            let row = &mut data[y * tw * 4..(y + 1) * tw * 4];
            let mut x = 0;
            for &(a, b) in spans.iter().chain(std::iter::once(&(tw, tw))) {
                for px in row[x * 4..a.max(x) * 4].as_chunks_mut::<4>().0 {
                    px.copy_from_slice(&super::region::HIDDEN);
                }
                x = x.max(b);
            }
        }
    }

    /// The target bitmap with the pointer drawn on it and, while a window is shared, the rest
    /// greyed out.
    fn finish(&self) -> Image {
        self.draw_cursor();
        let (w, h) = self.target;
        let mut data = unsafe {
            GdiFlush();
            std::slice::from_raw_parts(self.bits, w * h * 4).to_vec()
        };
        if self.scope != Scope::Screen {
            self.mask(&mut data);
        }
        Image { w, h, stride: w * 4, data }
    }

    fn draw_cursor(&self) {
        unsafe {
            let mut ci: CURSORINFO = std::mem::zeroed();
            ci.cbSize = size_of::<CURSORINFO>() as u32;
            if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 {
                return;
            }
            let mut ii: ICONINFO = std::mem::zeroed();
            if GetIconInfo(ci.hCursor, &mut ii) == 0 {
                return;
            }
            let x = (ci.ptScreenPos.x - self.origin.0) * self.target.0 as i32 / self.desk.0 as i32 - ii.xHotspot as i32;
            let y = (ci.ptScreenPos.y - self.origin.1) * self.target.1 as i32 / self.desk.1 as i32 - ii.yHotspot as i32;
            DrawIconEx(self.mem, x, y, ci.hCursor, 0, 0, 0, null_mut(), DI_NORMAL);
            DeleteObject(ii.hbmMask);
            DeleteObject(ii.hbmColor);
        }
    }
}

/// Moves this thread to the desktop that receives input, so the lock screen and
/// UAC prompts keep working; fails harmlessly without the rights to do so.
fn follow_input_desktop() -> bool {
    unsafe {
        let d = OpenInputDesktop(0, 0, DESKTOP_ALL);
        if d.is_null() {
            return false;
        }
        let ok = SetThreadDesktop(d) != 0;
        if !ok {
            CloseDesktop(d);
        }
        ok
    }
}

impl Drop for Gdi {
    fn drop(&mut self) {
        self.free_bitmap();
        unsafe {
            DeleteDC(self.mem);
            ReleaseDC(null_mut::<core::ffi::c_void>() as HWND, self.screen);
        }
    }
}

impl Capture for Gdi {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.desk
    }

    fn origin(&self) -> (i32, i32) {
        self.origin
    }

    fn shared_window(&self) -> Option<u64> {
        match self.scope {
            Scope::Window(h) => Some(h as u64),
            _ => None,
        }
    }

    fn shared_app(&self) -> Option<String> {
        match &self.scope {
            Scope::App(exe) => Some(exe.clone()),
            _ => None,
        }
    }

    fn monitors(&self) -> Vec<Monitor> {
        self.screens.iter().map(|s| s.info.clone()).collect()
    }

    fn current(&self) -> usize {
        self.current
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        self.screens = screens();
        let s = self.screens.get(index).ok_or("no such monitor")?;
        (self.current, self.origin, self.desk) = (index, (s.left, s.top), (s.info.w, s.info.h));
        self.monitor = (self.origin, self.desk);
        self.scope = Scope::Screen;
        (self.dup, self.base, self.dup_ready, self.force) = (None, None, false, true);
        self.dup_retry = std::time::Instant::now();
        Ok(())
    }

    fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        Ok(winlist::shareable())
    }

    fn poll_share(&mut self) -> Option<Result<(u8, String), String>> {
        std::mem::take(&mut self.ended).then(|| Ok((0, "the whole screen, as the shared window closed".into())))
    }

    fn share(&mut self, scope: Share) -> Result<String, String> {
        let hwnd = match scope {
            Share::Screen => {
                self.scope = Scope::Screen;
                (self.origin, self.desk) = self.monitor;
                self.pointer_inside = true;
                self.force = true;
                return Ok("the whole screen".into());
            }
            Share::Window(id) | Share::App(id) => id as HWND,
            Share::ActiveWindow | Share::ActiveApp => winlist::foreground(),
        };
        if hwnd.is_null() || winlist::pid_of(hwnd) == 0 {
            return Err("that window is gone".into());
        }
        let title = winlist::title_of(hwnd);
        let text = if matches!(scope, Share::App(_) | Share::ActiveApp) {
            let exe = winlist::exe_of(winlist::pid_of(hwnd));
            if exe.is_empty() {
                return Err("cannot tell which program that window belongs to".into());
            }
            let text = format!("every window of {}", winlist::app_name(&exe));
            self.scope = Scope::App(exe);
            text
        } else {
            self.scope = Scope::Window(hwnd);
            format!("the window \"{title}\"")
        };
        winlist::raise(hwnd);
        self.update_layout();
        Ok(text)
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.force = true;
        self.free_bitmap();
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits = null_mut();
            let bmp = CreateDIBSection(self.mem, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() {
                return Err(format!("CreateDIBSection {w}x{h} failed"));
            }
            self.old = SelectObject(self.mem, bmp);
            (self.bmp, self.bits, self.target) = (bmp, bits as *mut u8, (w, h));
        }
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        self.update_layout();
        let force = std::mem::take(&mut self.force) || self.scope != Scope::Screen;
        match self.poll_dup() {
            // Nothing changed on the monitor: no copy, no work further down.
            Some(false) if !force && self.dup_ready => return Ok(None),
            Some(_) if self.dup_ready => {
                let at = self.monitor.0;
                let dc = self.base.as_ref().map(|b| b.dc).unwrap_or(self.screen);
                if self.blit_from(dc, at) {
                    return Ok(Some(self.finish()));
                }
            }
            _ => {}
        }
        if !self.blit() {
            // The input desktop changed (lock screen, UAC); follow it and retry once.
            if !follow_input_desktop() {
                return Ok(None);
            }
            unsafe {
                ReleaseDC(null_mut::<core::ffi::c_void>() as HWND, self.screen);
                self.screen = GetDC(null_mut());
            }
            if !self.blit() {
                return Ok(None);
            }
        }
        Ok(Some(self.finish()))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        let mut seq = Vec::new();
        if self.scope != Scope::Screen {
            // Only the shared windows take clicks and keys.
            match &ev {
                Input::Move { x, y } => {
                    self.pointer_inside = self.shown.iter().any(|r| r.contains(*x as i32, *y as i32));
                }
                Input::Button { pressed: true, .. } if !self.pointer_inside => return Err(super::OUTSIDE.into()),
                Input::Scroll { .. } | Input::HScroll { .. } if !self.pointer_inside => return Ok(()),
                Input::Key { .. } | Input::KeyState { down: true, .. } if !winlist::focus_inside(&self.scope) => {
                    return Err("keys go only to the shared window; click it first".into());
                }
                _ => {}
            }
        }
        match ev {
            Input::Move { x, y } => {
                // Absolute coordinates span the whole virtual desktop, every monitor included.
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN) as f64,
                        GetSystemMetrics(SM_YVIRTUALSCREEN) as f64,
                        GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2) as f64,
                        GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2) as f64,
                    )
                };
                let nx = ((self.origin.0 as f64 + x - vx) * 65535.0 / (vw - 1.0)) as i32;
                let ny = ((self.origin.1 as f64 + y - vy) * 65535.0 / (vh - 1.0)) as i32;
                seq.push(mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0));
            }
            Input::Button { button, pressed } => {
                let flags = match (button, pressed) {
                    (1, true) => MOUSEEVENTF_LEFTDOWN,
                    (1, false) => MOUSEEVENTF_LEFTUP,
                    (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (2, false) => MOUSEEVENTF_MIDDLEUP,
                    (3, true) => MOUSEEVENTF_RIGHTDOWN,
                    (3, false) => MOUSEEVENTF_RIGHTUP,
                    (8 | 9, true) => MOUSEEVENTF_XDOWN,
                    (8 | 9, false) => MOUSEEVENTF_XUP,
                    _ => return Ok(()),
                };
                // XBUTTON1 is back, XBUTTON2 forward.
                let x = match button {
                    8 => 1,
                    9 => 2,
                    _ => 0,
                };
                seq.push(mouse(flags, 0, 0, x));
            }
            Input::Scroll { steps } => seq.push(mouse(MOUSEEVENTF_WHEEL, 0, 0, (-steps * 120) as u32)),
            Input::HScroll { steps } => seq.push(mouse(MOUSEEVENTF_HWHEEL, 0, 0, (steps * 120) as u32)),
            Input::Key { keysym, mods } => key_events(keysym, mods, &mut seq),
            Input::KeyState { keysym, down } => key_state(keysym, down, &mut seq),
            Input::SecureAttention => {
                return match crate::service::ask("sas") {
                    Some(r) => r,
                    None => Err(
                        "Ctrl+Alt+Del needs the sshremote service: run `sshremote --install-service` as administrator"
                            .into(),
                    ),
                };
            }
        }
        if seq.is_empty() {
            return Ok(());
        }
        let sent = unsafe { SendInput(seq.len() as u32, seq.as_ptr(), size_of::<INPUT>() as i32) };
        if sent as usize != seq.len() {
            return Err("SendInput was blocked (another desktop or a higher-integrity window has focus)".into());
        }
        Ok(())
    }
}

fn mouse(flags: u32, dx: i32, dy: i32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn kbd(vk: VIRTUAL_KEY, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn special_vk(keysym: u32) -> Option<(VIRTUAL_KEY, bool)> {
    Some(match keysym {
        0xff08 => (VK_BACK, false),
        0xff09 => (VK_TAB, false),
        0xff0d => (VK_RETURN, false),
        0xff1b => (VK_ESCAPE, false),
        0xff50 => (VK_HOME, true),
        0xff51 => (VK_LEFT, true),
        0xff52 => (VK_UP, true),
        0xff53 => (VK_RIGHT, true),
        0xff54 => (VK_DOWN, true),
        0xff55 => (VK_PRIOR, true),
        0xff56 => (VK_NEXT, true),
        0xff57 => (VK_END, true),
        0xff63 => (VK_INSERT, true),
        0xffff => (VK_DELETE, true),
        0xffbe..=0xffc9 => (VK_F1 + (keysym - 0xffbe) as u16, false),
        0xffeb => (VK_LWIN, true),
        0xffec => (VK_RWIN, true),
        _ => return None,
    })
}

/// Virtual key and extended flag for a keysym pressed on its own, as a physical key.
fn vk_of(keysym: u32) -> Option<(VIRTUAL_KEY, bool)> {
    if let Some(k) = special_vk(keysym) {
        return Some(k);
    }
    Some(match keysym {
        0xffe1 => (VK_LSHIFT, false),
        0xffe2 => (VK_RSHIFT, false),
        0xffe3 => (VK_LCONTROL, false),
        0xffe4 => (VK_RCONTROL, true),
        0xffe9 => (VK_LMENU, false),
        0xffea => (VK_RMENU, true),
        0xffe5 => (VK_CAPITAL, false),
        0xff13 => (VK_PAUSE, false),
        0xff61 => (VK_SNAPSHOT, true),
        0xff67 => (VK_APPS, true),
        0x20..=0x7e => {
            let scan = unsafe { VkKeyScanW(keysym as u16) };
            if scan == -1 {
                return None;
            }
            ((scan & 0xff) as u16, false)
        }
        _ => return None,
    })
}

/// One key down or up with its scan code, so held keys and games behave like a keyboard.
/// Characters without a key on this layout go in as Unicode.
fn key_state(keysym: u32, down: bool, seq: &mut Vec<INPUT>) {
    let up = if down { 0 } else { KEYEVENTF_KEYUP };
    match vk_of(keysym) {
        Some((vk, ext)) => {
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
            seq.push(kbd(vk, scan, up | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 }));
        }
        None => {
            let c = if keysym & 0xff00_0000 == 0x0100_0000 { keysym & 0xff_ffff } else { keysym };
            if let Some(c) = char::from_u32(c) {
                let mut buf = [0u16; 2];
                for &unit in c.encode_utf16(&mut buf).iter() {
                    seq.push(kbd(0, unit, KEYEVENTF_UNICODE | up));
                }
            }
        }
    }
}

/// Text goes in as Unicode; shortcuts and special keys go in as virtual keys
/// wrapped in their modifiers.
fn key_events(keysym: u32, mods: u8, seq: &mut Vec<INPUT>) {
    let ch = if keysym & 0xff00_0000 == 0x0100_0000 {
        char::from_u32(keysym & 0x00ff_ffff)
    } else if keysym <= 0xff {
        char::from_u32(keysym)
    } else {
        None
    };
    let (vk, ext, mods) = if let Some((vk, ext)) = special_vk(keysym) {
        (vk, ext, mods)
    } else if let Some(c) = ch {
        let shortcut = mods & (MOD_CTRL | MOD_ALT | MOD_SUPER) != 0;
        let scan = if shortcut && c.is_ascii() { unsafe { VkKeyScanW(c as u16) } } else { -1 };
        if scan == -1 {
            let mut buf = [0u16; 2];
            for &unit in c.encode_utf16(&mut buf).iter() {
                seq.push(kbd(0, unit, KEYEVENTF_UNICODE));
                seq.push(kbd(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
            }
            return;
        }
        let extra = if (scan >> 8) & 1 != 0 { MOD_SHIFT } else { 0 };
        ((scan & 0xff) as u16, false, mods | extra)
    } else {
        return;
    };
    let held: Vec<VIRTUAL_KEY> =
        [(MOD_CTRL, VK_CONTROL), (MOD_ALT, VK_MENU), (MOD_SHIFT, VK_SHIFT), (MOD_SUPER, VK_LWIN)]
            .iter()
            .filter(|(bit, _)| mods & bit != 0)
            .map(|&(_, vk)| vk)
            .collect();
    for &m in &held {
        seq.push(kbd(m, 0, 0));
    }
    let ext = if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
    seq.push(kbd(vk, 0, ext));
    seq.push(kbd(vk, 0, ext | KEYEVENTF_KEYUP));
    for &m in held.iter().rev() {
        seq.push(kbd(m, 0, KEYEVENTF_KEYUP));
    }
}

/// True when this process cannot see the interactive desktop, as under the
/// OpenSSH service, which runs sessions outside the console session.
pub fn needs_relay() -> bool {
    use windows_sys::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    let mut mine = 0;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut mine) != 0 && mine != WTSGetActiveConsoleSessionId() }
}
