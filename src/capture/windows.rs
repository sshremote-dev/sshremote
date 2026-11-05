//! Windows backend: GDI copy of the primary screen (scaled by StretchBlt when a
//! smaller target is asked for) and SendInput for keyboard and mouse.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, COLORONCOLOR, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
    SetStretchBltMode, StretchBlt,
};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop, SetThreadDesktop};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
    VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LEFT,
    VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP, VkKeyScanW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, ICONINFO,
    SM_CXSCREEN, SM_CYSCREEN,
};

use super::{Capture, Image, Input};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

const DESKTOP_ALL: u32 = 0x01ff;

pub struct Gdi {
    desk: (usize, usize),
    target: (usize, usize),
    screen: HDC,
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
}

impl Gdi {
    pub fn open() -> Result<Gdi, String> {
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            follow_input_desktop();
            let desk = (GetSystemMetrics(SM_CXSCREEN) as usize, GetSystemMetrics(SM_CYSCREEN) as usize);
            let screen = GetDC(null_mut());
            if screen.is_null() || desk.0 == 0 {
                return Err("no interactive desktop in this session".into());
            }
            let mem = CreateCompatibleDC(screen);
            let mut g = Gdi { desk, target: (0, 0), screen, mem, bmp: null_mut(), old: null_mut(), bits: null_mut() };
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

    fn blit(&self) -> bool {
        let (w, h) = (self.target.0 as i32, self.target.1 as i32);
        let (dw, dh) = (self.desk.0 as i32, self.desk.1 as i32);
        unsafe {
            if (w, h) == (dw, dh) {
                BitBlt(self.mem, 0, 0, w, h, self.screen, 0, 0, SRCCOPY) != 0
            } else {
                SetStretchBltMode(self.mem, COLORONCOLOR);
                StretchBlt(self.mem, 0, 0, w, h, self.screen, 0, 0, dw, dh, SRCCOPY) != 0
            }
        }
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
            let x = ci.ptScreenPos.x * self.target.0 as i32 / self.desk.0 as i32 - ii.xHotspot as i32;
            let y = ci.ptScreenPos.y * self.target.1 as i32 / self.desk.1 as i32 - ii.yHotspot as i32;
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

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
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
        self.draw_cursor();
        let (w, h) = self.target;
        let data = unsafe {
            GdiFlush();
            std::slice::from_raw_parts(self.bits, w * h * 4).to_vec()
        };
        Ok(Some(Image { w, h, stride: w * 4, data }))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        let mut seq = Vec::new();
        match ev {
            Input::Move { x, y } => {
                let nx = (x * 65535.0 / (self.desk.0 - 1).max(1) as f64) as i32;
                let ny = (y * 65535.0 / (self.desk.1 - 1).max(1) as f64) as i32;
                seq.push(mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, nx, ny, 0));
            }
            Input::Button { button, pressed } => {
                let flags = match (button, pressed) {
                    (1, true) => MOUSEEVENTF_LEFTDOWN,
                    (1, false) => MOUSEEVENTF_LEFTUP,
                    (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (2, false) => MOUSEEVENTF_MIDDLEUP,
                    (3, true) => MOUSEEVENTF_RIGHTDOWN,
                    (3, false) => MOUSEEVENTF_RIGHTUP,
                    _ => return Ok(()),
                };
                seq.push(mouse(flags, 0, 0, 0));
            }
            Input::Scroll { steps } => seq.push(mouse(MOUSEEVENTF_WHEEL, 0, 0, (-steps * 120) as u32)),
            Input::Key { keysym, mods } => key_events(keysym, mods, &mut seq),
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
        _ => return None,
    })
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
