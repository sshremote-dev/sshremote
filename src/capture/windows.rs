//! Windows backend: the monitor's picture from Desktop Duplication when it changed (GDI copies
//! when duplication is not available: another session, the secure desktop, a window across
//! monitors), made smaller by area averaging when a smaller target is asked for, and SendInput for
//! keyboard and mouse.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, EnumDisplayMonitors, GdiFlush, GetDC, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR,
    MONITORINFO, MONITORINFOEXW, ReleaseDC, SRCCOPY, SelectObject,
};
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop, SetThreadDesktop};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW,
    SendInput, VIRTUAL_KEY, VK_APPS, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1,
    VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_NEXT, VK_NUMLOCK, VK_PAUSE, VK_PRIOR,
    VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SCROLL, VK_SHIFT, VK_SNAPSHOT, VK_TAB, VK_UP, VkKeyScanW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, ICONINFO,
    MONITORINFOF_PRIMARY, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};
use windows_sys::core::BOOL;

use super::winlist::{self, R, Scope};
use super::{Capture, Image, Input, Monitor, Share, WindowInfo};
use crate::xk;
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
                at: Some((r.left, r.top)),
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
    /// Desktop Duplication of the monitor, and the latest picture it gave (the monitor's rows).
    dup: Option<super::dxgi::Watch>,
    picture: Vec<u8>,
    /// A picture's buffer the agent is done with, for the next (see `Capture::recycle`).
    spare: Vec<u8>,
    /// What the duplication calls as something changes, and the least time between its pictures.
    wake: super::Waker,
    gap: u32,
    /// The whole area, copied from the screen to be made smaller, and what makes it so.
    full: Option<Dib>,
    shrink: super::Shrink,
    /// The duplicated picture is complete (its first frame arrived).
    dup_ready: bool,
    /// When duplication may be tried again after it failed.
    dup_retry: std::time::Instant,
    /// The next grab returns a picture even if the screen did not change (new size or scope).
    force: bool,
    /// The viewer draws the pointer: the pictures leave it out, its shape goes as it changes
    /// (the handle of the one sent last).
    apart: bool,
    sent_cursor: usize,
    /// When the monitors were last looked at for a change of mode or layout; None: look now.
    layout_at: Option<std::time::Instant>,
    /// The input desktop (lock screen, UAC) cannot be followed: when to try again, and whether
    /// the viewer was told.
    shut_out: Option<std::time::Instant>,
    told_shut_out: bool,
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
            // The agent's waits between frames to the millisecond while the screen is shared:
            // with Windows' 15.6 ms tick a frame due could go out up to a tick late.
            timeBeginPeriod(1);
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
                picture: Vec::new(),
                spare: Vec::new(),
                wake: Default::default(),
                gap: gap_for(30),
                full: None,
                shrink: super::Shrink::default(),
                dup_ready: false,
                dup_retry: std::time::Instant::now(),
                force: true,
                apart: false,
                sent_cursor: 0,
                layout_at: Some(std::time::Instant::now()),
                shut_out: None,
                told_shut_out: false,
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
    /// A smaller target is made from the whole area by area averaging: StretchBlt either drops
    /// pixels (strokes of text with them) or is slow.
    fn blit_from(&mut self, src: HDC, at: (i32, i32)) -> bool {
        let (w, h) = (self.target.0 as i32, self.target.1 as i32);
        let (dw, dh) = (self.desk.0 as i32, self.desk.1 as i32);
        let (x, y) = (self.origin.0 - at.0, self.origin.1 - at.1);
        // From the screen, layered windows too: menus, tooltips and other pop-ups are layered,
        // and a plain copy leaves them out.
        let rop = if src == self.screen { SRCCOPY | CAPTUREBLT } else { SRCCOPY };
        unsafe {
            if (w, h) == (dw, dh) {
                return BitBlt(self.mem, 0, 0, w, h, src, x, y, rop) != 0;
            }
            GdiFlush();
            let (sw, sh) = self.desk;
            // The screen is copied whole first.
            if self.full.as_ref().is_none_or(|f| (f.w, f.h) != (sw, sh)) {
                self.full = Dib::new(self.screen, sw, sh);
            }
            let Some(full) = self.full.as_mut() else { return false };
            if BitBlt(full.dc, 0, 0, dw, dh, src, x, y, rop) == 0 {
                return false;
            }
            GdiFlush();
            let out = self.shrink.run(full.pixels(), sw, sh, sw * 4, w as usize, h as usize);
            std::ptr::copy_nonoverlapping(out.as_ptr(), self.bits, out.len());
        }
        true
    }

    /// Where the captured area starts in the duplicated picture, and the picture's row length.
    fn in_picture(&self) -> Option<(usize, usize)> {
        let (mw, mh) = self.dup.as_ref()?.size;
        let ((ml, mt), (sw, sh)) = (self.monitor.0, self.desk);
        if self.origin.0 < ml || self.origin.1 < mt {
            return None;
        }
        let (x, y) = ((self.origin.0 - ml) as usize, (self.origin.1 - mt) as usize);
        let stride = mw * 4;
        (x + sw <= mw && y + sh <= mh && self.picture.len() >= stride * mh).then_some((y * stride + x * 4, stride))
    }

    /// The captured area of the duplicated picture as it is, for a viewer that draws the pointer
    /// itself: copied once, not by way of the target bitmap.
    fn picture_image(&mut self) -> Option<Image> {
        let (at, stride) = self.in_picture()?;
        let ((sw, sh), (w, h)) = (self.desk, self.target);
        let from = &self.picture[at..];
        let mut data = std::mem::take(&mut self.spare);
        data.clear();
        if (w, h) == (sw, sh) {
            for row in 0..h {
                data.extend_from_slice(&from[row * stride..row * stride + w * 4]);
            }
        } else {
            data.extend_from_slice(self.shrink.run(from, sw, sh, stride, w, h));
        }
        Some(Image { w, h, stride: w * 4, data })
    }

    /// Copies the captured area into the target bitmap from the duplicated picture, read where
    /// it is (made smaller by area averaging when asked).
    fn place(&mut self) -> bool {
        let Some((at, stride)) = self.in_picture() else { return false };
        let ((sw, sh), (w, h)) = (self.desk, self.target);
        let from = &self.picture[at..];
        unsafe {
            GdiFlush();
            if (w, h) == (sw, sh) {
                for row in 0..h {
                    std::ptr::copy_nonoverlapping(from[row * stride..].as_ptr(), self.bits.add(row * w * 4), w * 4);
                }
            } else {
                let out = self.shrink.run(from, sw, sh, stride, w, h);
                std::ptr::copy_nonoverlapping(out.as_ptr(), self.bits, out.len());
            }
        }
        true
    }

    fn blit(&mut self) -> bool {
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
            self.dup = super::dxgi::Watch::open(ml, mt, self.wake.clone(), self.gap, follow_input_desktop).ok();
            self.dup_ready = false;
            if self.dup.is_none() {
                self.dup_retry = std::time::Instant::now() + std::time::Duration::from_secs(2);
                return None;
            }
        }
        match self.dup.as_ref()?.take(&mut self.picture) {
            Ok(super::dxgi::Next::Same) => Some(false),
            Ok(super::dxgi::Next::Pointer) => Some(true),
            Ok(super::dxgi::Next::Picture) => {
                self.dup_ready = true;
                Some(true)
            }
            Err(super::dxgi::Lost) => {
                // A desktop switch or mode change: open again on the next frame, GDI meanwhile,
                // and look at the monitors at once.
                (self.dup, self.dup_ready) = (None, false);
                self.layout_at = None;
                None
            }
        }
    }

    /// Follows a change of the monitors (a new mode, a monitor docked, rotated or moved), at
    /// most once a second: the monitor shared is found again by name and takes its new place and
    /// size, which the agent then announces to the viewer.
    fn follow_monitors(&mut self) {
        if self.layout_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
            return;
        }
        self.layout_at = Some(std::time::Instant::now());
        let now = screens();
        let name = self.screens.get(self.current).map(|s| s.info.name.clone());
        let at = name.and_then(|n| now.iter().position(|s| s.info.name == n)).unwrap_or(0);
        let Some(s) = now.get(at) else { return };
        let monitor = ((s.left, s.top), (s.info.w, s.info.h));
        (self.screens, self.current) = (now, at);
        if monitor == self.monitor {
            return;
        }
        self.monitor = monitor;
        if self.scope == Scope::Screen {
            (self.origin, self.desk) = monitor;
        }
        (self.dup, self.dup_ready, self.force) = (None, false, true);
        self.dup_retry = std::time::Instant::now();
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
    // In place on the scaled duplication buffer, unlike region::crop_masked, which crops a full-size frame.
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
    fn finish(&mut self) -> Image {
        if !self.apart {
            self.draw_cursor();
        }
        let (w, h) = self.target;
        let mut data = std::mem::take(&mut self.spare);
        data.clear();
        unsafe {
            GdiFlush();
            data.extend_from_slice(std::slice::from_raw_parts(self.bits, w * h * 4));
        }
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

/// A pointer's picture as RGBA. A monochrome one (the text cursor) and a colour one without
/// alpha may invert what is under them: those pixels come out black, with a white rim around
/// them to stay in sight on dark backgrounds, as other remote desktops draw them.
unsafe fn cursor_shape(hc: windows_sys::Win32::UI::WindowsAndMessaging::HCURSOR) -> Option<super::Pointer> {
    use windows_sys::Win32::Graphics::Gdi::{BITMAP, GetDIBits, GetObjectW};
    unsafe {
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(hc, &mut ii) == 0 {
            return None;
        }
        let mut bm: BITMAP = std::mem::zeroed();
        GetObjectW(ii.hbmMask, size_of::<BITMAP>() as i32, &mut bm as *mut BITMAP as *mut core::ffi::c_void);
        let mono = ii.hbmColor.is_null();
        let w = bm.bmWidth.max(0) as usize;
        let h = if mono { bm.bmHeight.max(0) as usize / 2 } else { bm.bmHeight.max(0) as usize };
        let dc = CreateCompatibleDC(null_mut());
        let read = |hbm: HBITMAP, rows: usize| -> Option<Vec<u8>> {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(rows as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut px = vec![0u8; w * rows * 4];
            let got = GetDIBits(dc, hbm, 0, rows as u32, px.as_mut_ptr() as *mut core::ffi::c_void, &mut bi, DIB_RGB_COLORS);
            (got == rows as i32).then_some(px)
        };
        let mask = read(ii.hbmMask, if mono { h * 2 } else { h });
        let color = if mono { None } else { read(ii.hbmColor, h) };
        DeleteDC(dc);
        DeleteObject(ii.hbmMask);
        if !mono {
            DeleteObject(ii.hbmColor);
        }
        let mask = mask?;
        let n = w * h;
        if n == 0 {
            return None;
        }
        let mut rgba = vec![0u8; n * 4];
        let mut inverted = vec![false; n];
        match color {
            Some(c) => {
                let alpha = c.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
                for i in 0..n {
                    let (b, g, r, a) = (c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]);
                    let shows_through = mask[i * 4] != 0;
                    let px = if alpha {
                        [r, g, b, a]
                    } else if !shows_through {
                        [r, g, b, 255]
                    } else if (r | g | b) != 0 {
                        inverted[i] = true;
                        [0, 0, 0, 255]
                    } else {
                        [0, 0, 0, 0]
                    };
                    rgba[i * 4..i * 4 + 4].copy_from_slice(&px);
                }
            }
            // The AND mask over the XOR one.
            None => {
                for i in 0..n {
                    let px = match (mask[i * 4] != 0, mask[(n + i) * 4] != 0) {
                        (false, false) => [0, 0, 0, 255],
                        (false, true) => [255, 255, 255, 255],
                        (true, false) => [0, 0, 0, 0],
                        (true, true) => {
                            inverted[i] = true;
                            [0, 0, 0, 255]
                        }
                    };
                    rgba[i * 4..i * 4 + 4].copy_from_slice(&px);
                }
            }
        }
        if inverted.iter().any(|&v| v) {
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    if rgba[i * 4 + 3] != 0 {
                        continue;
                    }
                    let near = (y.saturating_sub(1)..(y + 2).min(h)).any(|yy| (x.saturating_sub(1)..(x + 2).min(w)).any(|xx| inverted[yy * w + xx]));
                    if near {
                        rgba[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
                    }
                }
            }
        }
        Some(super::Pointer { w, h, hot: (ii.xHotspot as usize, ii.yHotspot as usize), rgba })
    }
}

/// The least time between two duplicated pictures at the viewer's frame rate: a little less than a
/// frame, so the next is read back by the time the agent may send it.
fn gap_for(fps: u8) -> u32 {
    (1000 / fps.max(1) as u32).saturating_sub(4).max(8)
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
            timeEndPeriod(1);
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

    fn screen_id(&self) -> Option<String> {
        self.screens.get(self.current).map(|s| s.info.name.clone())
    }

    fn screen_changed(&mut self) {
        // Its new size is read now, not within the second the monitors are next looked at.
        self.layout_at = None;
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        self.screens = screens();
        let s = self.screens.get(index).ok_or("no such monitor")?;
        (self.current, self.origin, self.desk) = (index, (s.left, s.top), (s.info.w, s.info.h));
        self.monitor = (self.origin, self.desk);
        self.scope = Scope::Screen;
        self.shown.clear();
        (self.pointer_inside, self.ended) = (true, false);
        (self.dup, self.dup_ready, self.force) = (None, false, true);
        (self.dup_retry, self.layout_at) = (std::time::Instant::now(), Some(std::time::Instant::now()));
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
                self.shown.clear();
                (self.pointer_inside, self.ended) = (true, false);
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

    fn set_fps(&mut self, fps: u8) {
        self.gap = gap_for(fps);
        if let Some(d) = self.dup.as_ref() {
            d.set_gap(self.gap);
        }
    }

    fn recycle(&mut self, img: Image) {
        self.spare = img.data;
    }

    fn set_waker(&mut self, wake: Box<dyn Fn() + Send>) -> bool {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
        true
    }

    // GDI, while duplication is not there, wakes nobody.
    fn wakes(&self) -> bool {
        self.dup.is_some()
    }

    fn pointer_apart(&mut self, on: bool) -> bool {
        (self.apart, self.sent_cursor, self.force) = (on, 0, true);
        true
    }

    fn pointer(&mut self) -> (Option<super::Pointer>, Option<(i32, i32)>) {
        unsafe {
            let mut ci: CURSORINFO = std::mem::zeroed();
            ci.cbSize = size_of::<CURSORINFO>() as u32;
            if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 {
                return (None, None);
            }
            let shape = if ci.hCursor as usize != self.sent_cursor {
                self.sent_cursor = ci.hCursor as usize;
                cursor_shape(ci.hCursor)
            } else {
                None
            };
            (shape, Some((ci.ptScreenPos.x - self.origin.0, ci.ptScreenPos.y - self.origin.1)))
        }
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        self.follow_monitors();
        self.update_layout();
        let force = std::mem::take(&mut self.force) || self.scope != Scope::Screen;
        match self.poll_dup() {
            // Nothing changed on the monitor: no copy, no work further down.
            Some(false) if !force && self.dup_ready => return Ok(None),
            Some(_) if self.dup_ready => {
                // Shown again: a later lock is told again.
                (self.shut_out, self.told_shut_out) = (None, false);
                if self.apart && self.scope == Scope::Screen && let Some(img) = self.picture_image() {
                    return Ok(Some(img));
                }
                if self.place() {
                    return Ok(Some(self.finish()));
                }
            }
            _ => {}
        }
        if self.shut_out.is_some_and(|t| std::time::Instant::now() < t) {
            return Ok(None);
        }
        if !self.blit() {
            // The input desktop changed (lock screen, UAC); follow it and retry once. Without the
            // rights to (not the service), tried again twice a second, not at every turn.
            if !follow_input_desktop() {
                self.shut_out = Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
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
        (self.shut_out, self.told_shut_out) = (None, false);
        Ok(Some(self.finish()))
    }

    fn notice(&mut self) -> Option<String> {
        if self.shut_out.is_none() || std::mem::replace(&mut self.told_shut_out, true) {
            return None;
        }
        Some(if crate::console_user::running_as_system() {
            "The screen there is on a desktop that cannot be shown just now; the picture comes back with it.".into()
        } else {
            "The screen there is locked or asks for permission (UAC), which cannot be shown without SSH Remote's service there; the picture comes back once it is unlocked. To see and use that screen too, install the service there (Remote access settings, or sshremote remote-access install).".into()
        })
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
                Input::MoveBy { .. } => return Err(super::WHOLE_SCREEN_MOUSE.into()),
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
            // Relative: what games that read raw movement take.
            Input::MoveBy { dx, dy } => seq.push(mouse(MOUSEEVENTF_MOVE, dx, dy, 0)),
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
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: crate::hostctl::INJECTED } },
    }
}

fn kbd(vk: VIRTUAL_KEY, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: crate::hostctl::INJECTED } },
    }
}

fn special_vk(keysym: u32) -> Option<(VIRTUAL_KEY, bool)> {
    Some(match keysym {
        xk::BACKSPACE => (VK_BACK, false),
        xk::TAB => (VK_TAB, false),
        xk::RETURN => (VK_RETURN, false),
        xk::ESCAPE => (VK_ESCAPE, false),
        xk::HOME => (VK_HOME, true),
        xk::LEFT => (VK_LEFT, true),
        xk::UP => (VK_UP, true),
        xk::RIGHT => (VK_RIGHT, true),
        xk::DOWN => (VK_DOWN, true),
        xk::PAGE_UP => (VK_PRIOR, true),
        xk::PAGE_DOWN => (VK_NEXT, true),
        xk::END => (VK_END, true),
        xk::INSERT => (VK_INSERT, true),
        xk::DELETE => (VK_DELETE, true),
        // F1 to F24: their virtual keys follow one another as their keysyms do.
        xk::F1..=xk::F24 => (VK_F1 + (keysym - xk::F1) as u16, false),
        xk::SUPER_L => (VK_LWIN, true),
        xk::SUPER_R => (VK_RWIN, true),
        _ => return None,
    })
}

/// Virtual key and extended flag for a keysym pressed on its own, as a physical key.
fn vk_of(keysym: u32) -> Option<(VIRTUAL_KEY, bool)> {
    if let Some(k) = special_vk(keysym) {
        return Some(k);
    }
    Some(match keysym {
        xk::SHIFT_L => (VK_LSHIFT, false),
        xk::SHIFT_R => (VK_RSHIFT, false),
        xk::CONTROL_L => (VK_LCONTROL, false),
        xk::CONTROL_R => (VK_RCONTROL, true),
        xk::ALT_L => (VK_LMENU, false),
        xk::ALT_R => (VK_RMENU, true),
        xk::CAPS_LOCK => (VK_CAPITAL, false),
        xk::NUM_LOCK => (VK_NUMLOCK, true),
        xk::SCROLL_LOCK => (VK_SCROLL, false),
        xk::PAUSE => (VK_PAUSE, false),
        xk::PRINT => (VK_SNAPSHOT, true),
        xk::MENU => (VK_APPS, true),
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
            if let Some(c) = xk::to_char(keysym) {
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
    let ch = if xk::is_unicode(keysym) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The system's own pointers come apart as pictures: the arrow, and the text cursor, which
    /// inverts what is under it and so comes with a white rim. No screen is read.
    /// sshremote-test.exe --ignored windows_pointers_come_as_pictures
    #[test]
    #[ignore]
    fn windows_pointers_come_as_pictures() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IDC_ARROW, IDC_IBEAM, LoadCursorW};
        for (name, id) in [("arrow", IDC_ARROW), ("text", IDC_IBEAM)] {
            let p = unsafe { cursor_shape(LoadCursorW(null_mut(), id)) }.unwrap_or_else(|| panic!("the {name} pointer"));
            assert!(p.w > 0 && p.h > 0 && p.rgba.len() == p.w * p.h * 4, "{name}: {}x{}", p.w, p.h);
            let opaque = p.rgba.chunks(4).filter(|c| c[3] > 128).count();
            assert!(opaque > 10 && opaque < p.w * p.h, "{name}: {opaque} opaque pixels");
            assert!(p.hot.0 < p.w && p.hot.1 < p.h, "{name}: its point inside it");
            let dark = p.rgba.chunks(4).any(|c| c[3] > 128 && c[0] < 64);
            let light = p.rgba.chunks(4).any(|c| c[3] > 128 && c[0] > 192);
            assert!(dark && light, "{name}: dark with a light rim, or the other way round");
        }
    }

    /// What the screen costs the agent while little changes on it, looked at as the agent's loop
    /// looks (woken by the duplication, 30 frames a second), and that no look after the first
    /// (which fills new buffers) holds the loop up. Only counts are printed; no picture is kept.
    /// sshremote-test.exe --ignored --nocapture a_quiet_screen_costs_little
    #[test]
    #[ignore]
    fn a_quiet_screen_costs_little() {
        use std::time::{Duration, Instant};
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let cpu = || unsafe {
            let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut a, mut b, mut k, mut u) = (z, z, z, z);
            GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
            let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
            t(k) + t(u)
        };
        let secs: f64 = std::env::var("SSHREMOTE_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(10.0);
        let mut g = Gdi::open().expect("GDI");
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        assert!(g.set_waker(Box::new(move || {
            let _ = tx.send(());
        })));
        g.set_fps(30);
        g.pointer_apart(true);
        let _ = g.grab();
        let (start, at) = (cpu(), Instant::now());
        let (mut turns, mut pictures, mut longest, mut due) = (0u32, 0u32, Duration::ZERO, Instant::now());
        while at.elapsed().as_secs_f64() < secs {
            let now = Instant::now();
            let shortest = if due <= now { 8 } else { 1 };
            let wait = due.saturating_duration_since(now).clamp(Duration::from_millis(shortest), Duration::from_millis(8));
            let _ = rx.recv_timeout(wait);
            rx.try_iter().for_each(drop);
            turns += 1;
            if Instant::now() < due {
                continue;
            }
            let t = Instant::now();
            let got = g.grab().expect("grab");
            if pictures > 0 {
                longest = longest.max(t.elapsed());
            }
            if let Some(img) = got {
                g.recycle(img);
                pictures += 1;
                due = Instant::now() + Duration::from_millis(33);
            }
        }
        let (used, took) = (cpu() - start, at.elapsed().as_secs_f64());
        println!(
            "{turns} turns ({:.0}/s), {pictures} pictures ({:.1}/s), {:.2}% of a core, longest look {:.1} ms",
            turns as f64 / took,
            pictures as f64 / took,
            used / took * 100.0,
            longest.as_secs_f64() * 1000.0
        );
        assert!(longest < Duration::from_millis(12), "a look held the loop up {longest:?}");
    }

    /// The duplicated picture is the screen, at its size and made smaller: compared with GDI's copy
    /// of the same moment, nearly every pixel is the same (a few may change between the two).
    /// Only the share of equal pixels is printed; no picture is kept.
    /// sshremote-test.exe --ignored --nocapture the_duplicated_picture_is_the_screen
    #[test]
    #[ignore]
    fn the_duplicated_picture_is_the_screen() {
        let mut g = Gdi::open().expect("GDI");
        g.pointer_apart(true);
        let (dw, dh) = g.desk;
        for (w, h) in [(dw, dh), (dw / 2, dh / 2)] {
            g.set_target(w, h).unwrap();
            let mut best = 0.0f64;
            for _ in 0..5 {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                let dup = loop {
                    g.force = true;
                    if let Some(img) = g.grab().unwrap().filter(|_| g.dup_ready) {
                        break img;
                    }
                    assert!(std::time::Instant::now() < deadline, "no duplicated picture");
                    std::thread::sleep(std::time::Duration::from_millis(20));
                };
                assert!(g.blit());
                let gdi = g.finish();
                assert_eq!((dup.w, dup.h, dup.data.len()), (gdi.w, gdi.h, gdi.data.len()));
                let same = dup.data.chunks(4).zip(gdi.data.chunks(4)).filter(|(a, b)| a[..3] == b[..3]).count();
                best = best.max(same as f64 / (w * h) as f64);
            }
            println!("{w}x{h}: {:.2}% of the pixels as GDI copies them", best * 100.0);
            assert!(best > 0.97, "{w}x{h}: only {:.1}% the same", best * 100.0);
        }
    }

    /// A smaller picture is the mean of what it covers: one-pixel lines halved come out grey,
    /// where dropping every other column would leave them all black or all white. Drawn in a
    /// bitmap of its own; no screen is read.
    #[test]
    #[ignore]
    fn a_smaller_picture_is_averaged_on_windows() {
        let mut g = Gdi::open().expect("GDI");
        let (dw, dh) = g.desk;
        let mut lines = Dib::new(g.screen, dw, dh).expect("a bitmap");
        for (i, px) in lines.pixels().chunks_exact_mut(4).enumerate() {
            let v = if i % dw % 2 == 0 { 0 } else { 255 };
            px.copy_from_slice(&[v, v, v, 255]);
        }
        g.set_target(dw / 2, dh / 2).unwrap();
        let at = g.origin;
        assert!(g.blit_from(lines.dc, at), "copied and made smaller");
        let out = unsafe { std::slice::from_raw_parts(g.bits, (dw / 2) * (dh / 2) * 4) };
        assert!(out.chunks(4).take(dw / 2 * 10).all(|c| (120..=135).contains(&c[1])), "grey: {:?}", &out[..16]);
    }
}
