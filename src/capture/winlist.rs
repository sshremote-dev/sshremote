//! Windows on the Windows desktop: the list a viewer picks from, and for a
//! shared window or program, which parts of the screen show it.

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOTOWNER, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SW_RESTORE, SetForegroundWindow, ShowWindow, WS_CHILD, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_sys::core::BOOL;

use super::WindowInfo;
pub use super::region::R;

/// A visible top-level window, in z-order from the top.
pub struct Win {
    pub hwnd: HWND,
    pub rect: R,
    pub pid: u32,
    /// Has an owner or is a popup (menus, dialogs, tooltips): part of another window.
    pub transient: bool,
    pub title: String,
}

fn text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn class(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The frame as drawn, without the invisible resize borders.
fn bounds(hwnd: HWND) -> R {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut rc as *mut RECT as _,
            size_of::<RECT>() as u32,
        )
    } == 0;
    if !ok {
        unsafe { GetWindowRect(hwnd, &mut rc) };
    }
    R { l: rc.left, t: rc.top, r: rc.right, b: rc.bottom }
}

fn cloaked(hwnd: HWND) -> bool {
    let mut v = 0u32;
    let hr = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut v as *mut u32 as _, 4) };
    hr == 0 && v != 0
}

pub fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

/// The program file of a process, or "" when it cannot be read.
pub fn exe_of(pid: u32) -> String {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) != 0;
        CloseHandle(h);
        if ok { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() }
    }
}

/// "chrome" from "C:\...\chrome.exe".
pub fn app_name(exe: &str) -> String {
    let file = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file).to_string()
}

unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
    let list = unsafe { &mut *(list as *mut Vec<Win>) };
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 || style & WS_CHILD != 0 || cloaked(hwnd) {
            return 1;
        }
        let rect = bounds(hwnd);
        if rect.empty() {
            return 1;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let owned = !GetWindow(hwnd, GW_OWNER).is_null();
        let transient = owned || (style & WS_POPUP != 0 && ex & WS_EX_TOOLWINDOW != 0) || class(hwnd) == "#32768";
        list.push(Win { hwnd, rect, pid: pid_of(hwnd), transient, title: text(hwnd) });
    }
    1
}

/// Visible windows from the top of the z-order down.
fn z_order() -> Vec<Win> {
    let mut list: Vec<Win> = Vec::new();
    unsafe { EnumWindows(Some(collect), &mut list as *mut Vec<Win> as LPARAM) };
    list
}

/// Windows a viewer can choose: titled, unowned, not tool windows or the desktop itself.
pub fn shareable() -> Vec<WindowInfo> {
    let mut out = Vec::new();
    for w in z_order() {
        if w.transient || w.title.trim().is_empty() {
            continue;
        }
        let ex = unsafe { GetWindowLongW(w.hwnd, GWL_EXSTYLE) } as u32;
        let cls = class(w.hwnd);
        if ex & WS_EX_TOOLWINDOW != 0 || matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            continue;
        }
        let (width, height) = ((w.rect.r - w.rect.l) as u16, (w.rect.b - w.rect.t) as u16);
        if width < 80 || height < 40 {
            continue;
        }
        out.push(WindowInfo { id: w.hwnd as u64, title: w.title, app: app_name(&exe_of(w.pid)), w: width, h: height });
    }
    out
}

/// What is shared, resolved to window handles each frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    Screen,
    Window(HWND),
    /// Every window of this program file.
    App(String),
}

/// Does `w` belong to the shared window or program?
fn belongs(scope: &Scope, w: &Win, exe_cache: &mut Vec<(u32, String)>) -> bool {
    match scope {
        Scope::Screen => true,
        Scope::Window(h) => {
            w.hwnd == *h || unsafe { GetAncestor(w.hwnd, GA_ROOTOWNER) } == *h || (w.transient && w.pid == pid_of(*h))
        }
        Scope::App(exe) => {
            let name = match exe_cache.iter().find(|(p, _)| *p == w.pid) {
                Some((_, e)) => e.clone(),
                None => {
                    let e = exe_of(w.pid);
                    exe_cache.push((w.pid, e.clone()));
                    e
                }
            };
            name.eq_ignore_ascii_case(exe)
        }
    }
}

/// The area to capture and, inside it, the visible parts of the shared windows.
pub struct Layout {
    pub area: R,
    pub shown: Vec<R>,
}

/// Where the shared windows are now; `None` when none of them is on screen.
pub fn layout(scope: &Scope, screen: R) -> Option<Layout> {
    let wins = z_order();
    let mut cache = Vec::new();
    let mine: Vec<bool> = wins.iter().map(|w| belongs(scope, w, &mut cache)).collect();
    let stack: Vec<(R, bool, bool)> = wins.iter().zip(&mine).map(|(w, &m)| (w.rect, m, w.transient)).collect();
    arrange(&stack, screen)
}

/// From the windows (top first: where, whether shared, whether a pop-up), the area to capture
/// and the shared parts in it. The area is the shared windows' and their open pop-ups' (menus,
/// dialogs, drop-down lists), so a menu reaching past its window is seen whole; with no shared
/// window on screen, none.
pub fn arrange(stack: &[(R, bool, bool)], screen: R) -> Option<Layout> {
    let mut area: Option<R> = None;
    for &(rect, _, _) in stack.iter().filter(|w| w.1 && !w.2) {
        area = Some(area.map_or(rect, |a| a.union(&rect)));
    }
    let mut area = area?;
    for &(rect, _, _) in stack.iter().filter(|w| w.1 && w.2) {
        area = area.union(&rect);
    }
    let area = area.and(&screen);
    if area.empty() {
        return None;
    }
    let parts: Vec<(R, bool)> = stack.iter().map(|&(r, m, _)| (r, m)).collect();
    Some(Layout { area, shown: super::region::shown_parts(&parts, area) })
}

/// Whether the focused window belongs to what is shared, so keys may go to it.
pub fn focus_inside(scope: &Scope) -> bool {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() {
        return false;
    }
    match scope {
        Scope::Screen => true,
        Scope::Window(h) => fg == *h || unsafe { GetAncestor(fg, GA_ROOTOWNER) } == *h || pid_of(fg) == pid_of(*h),
        Scope::App(exe) => exe_of(pid_of(fg)).eq_ignore_ascii_case(exe),
    }
}

/// Whether the window still exists (minimized counts).
pub fn exists(hwnd: HWND) -> bool {
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(hwnd) != 0 }
}

unsafe extern "system" fn find_visible(hwnd: HWND, found: LPARAM) -> BOOL {
    let found = unsafe { &mut *(found as *mut Vec<u32>) };
    if unsafe { IsWindowVisible(hwnd) } != 0 {
        found.push(pid_of(hwnd));
    }
    1
}

/// Whether the program still has a visible window, minimized ones included.
pub fn app_running(exe: &str) -> bool {
    let mut pids: Vec<u32> = Vec::new();
    unsafe { EnumWindows(Some(find_visible), &mut pids as *mut Vec<u32> as LPARAM) };
    pids.sort_unstable();
    pids.dedup();
    pids.into_iter().any(|p| exe_of(p).eq_ignore_ascii_case(exe))
}

/// Brings a window forward, restoring it when minimized.
pub fn raise(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

pub fn title_of(hwnd: HWND) -> String {
    text(hwnd)
}

/// The program's window nearest the top of the stack.
pub fn top_window_of(exe: &str) -> Option<HWND> {
    z_order().into_iter().find(|w| !w.transient && exe_of(w.pid).eq_ignore_ascii_case(exe)).map(|w| w.hwnd)
}

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shared window's menu or dialog reaching past it widens what is captured, and shows over
    /// the window; other programs' windows stay out, and a pop-up alone is not a shared window.
    #[test]
    fn pop_ups_of_the_shared_window_are_captured_whole() {
        let r = |l, t, r, b| R { l, t, r, b };
        let screen = r(0, 0, 1920, 1080);
        let window = r(100, 100, 900, 700);
        let menu = r(850, 400, 1100, 800);
        let other = r(950, 100, 1500, 600);
        let l = arrange(&[(menu, true, true), (other, false, false), (window, true, false)], screen).unwrap();
        assert_eq!(l.area, r(100, 100, 1100, 800), "the menu widens the area");
        assert!(l.shown.contains(&menu), "the menu shows, whole: {:?}", l.shown);
        let no_menu = arrange(&[(other, false, false), (window, true, false)], screen).unwrap();
        assert_eq!(no_menu.area, window);
        assert!(arrange(&[(menu, true, true)], screen).is_none(), "a pop-up alone shares nothing");
    }

    #[test]
    fn program_names_drop_the_folder_and_extension() {
        assert_eq!(app_name(r"C:\Program Files\Google\Chrome\Application\chrome.exe"), "chrome");
    }
}
