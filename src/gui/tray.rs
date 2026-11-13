//! The icon in the notification area. It stands for SSH Remote while its window is closed and
//! remote access by ID or tunnels go on (a click brings the window back), and, as a program of its
//! own (`sshremote --tray`, started with every sign-in once the service is installed), for the
//! service: its menu turns remote access on or off, copies the ID, sets who comes in and how,
//! shows the window of whoever is connected, works the service and quits.
//!
//! Its thread does all of that itself: a hidden window gets no frames, so nothing may wait for one.
//! Windows only: a Wayland session lets no program hide its window and bring it back, and on
//! Linux the systemd user service keeps remote access up with no window.

/// What the menu shows, as the window (or the service) last told.
#[derive(Clone, Default)]
pub(super) struct Status {
    /// Remote access by ID: whether it is on, and the ID.
    pub access: Option<(bool, String)>,
    /// The service: installed, running.
    pub service: Option<(bool, bool)>,
    /// Who comes in and how: asked first, straight through the routers, found on this network,
    /// straight in from it.
    pub settings: Option<[bool; 4]>,
    /// Viewers connected by the ID now.
    pub viewers: usize,
    /// The icon's own program: Quit hides the icon, and changes go through the administrator
    /// prompt (the settings are the administrators').
    #[cfg_attr(not(windows), allow(dead_code))]
    pub alone: bool,
}

pub(super) type Shared = std::sync::Arc<std::sync::Mutex<Status>>;

/// Set when the menu's Quit was chosen: the window then closes for good.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) static QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Set when the menu changed remote access's settings: the window reads them again.
pub(super) static CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
pub(super) use win::Tray;

/// The menu's choices, by their item numbers.
#[cfg_attr(not(windows), allow(dead_code))]
mod item {
    pub const OPEN: usize = 1;
    pub const ACCESS: usize = 2;
    pub const COPY_ID: usize = 3;
    pub const ASK: usize = 4;
    pub const STRAIGHT: usize = 5;
    pub const NEARBY: usize = 6;
    pub const DIRECT: usize = 7;
    pub const SETTINGS: usize = 8;
    pub const SHOW: usize = 9;
    pub const INSTALL: usize = 10;
    pub const START: usize = 11;
    pub const STOP: usize = 12;
    pub const REMOVE: usize = 13;
    pub const QUIT: usize = 99;
}

/// Does what the menu's item `n` asks; says what went wrong, if anything did.
#[cfg_attr(not(windows), allow(dead_code))]
fn act(n: usize, status: &Status) -> Result<(), String> {
    #[cfg(feature = "remote")]
    {
        use crate::access;
        // A change of the settings or the service: here when this program has the rights (the
        // window asks for them at start), else by the command line through the prompt for them.
        let as_admin = |args: &[&str], here: &dyn Fn() -> Result<(), String>| -> Result<(), String> {
            if crate::admin::elevated() {
                return here();
            }
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let args: Vec<String> = ["--no-console", "remote-access"].into_iter().chain(args.iter().copied()).map(String::from).collect();
            #[cfg(windows)]
            let given = crate::admin::run_elevated(&exe.to_string_lossy(), &args, true)?;
            #[cfg(not(windows))]
            let given = crate::admin::relaunch(&args)?;
            let _ = exe;
            if given { Ok(()) } else { Err("administrator rights were not given".into()) }
        };
        let change = |f: &dyn Fn(&mut access::Config)| -> Result<(), String> {
            let mut c = access::stored()?.ok_or("remote access is not set up")?;
            f(&mut c);
            access::save(&c)
        };
        let set = status.settings.unwrap_or_default();
        let word = |on: bool| if on { "on" } else { "off" };
        match n {
            item::ACCESS => {
                let on = !status.access.as_ref().is_some_and(|a| a.0);
                as_admin(&[word(on)], &|| change(&|c| c.enabled = on))?;
            }
            item::ASK => as_admin(&["ask", word(!set[0])], &|| change(&|c| c.ask = !set[0]))?,
            item::STRAIGHT => as_admin(&["straight", word(!set[1])], &|| change(&|c| c.punch = !set[1]))?,
            item::NEARBY => as_admin(&["lan", word(!set[2])], &|| change(&|c| c.lan = !set[2]))?,
            item::DIRECT => as_admin(&["direct", word(!set[3])], &|| change(&|c| c.direct = !set[3]))?,
            item::COPY_ID => {
                if let Some((_, id)) = &status.access {
                    arboard::Clipboard::new().and_then(|mut c| c.set_text(id.clone())).map_err(|e| e.to_string())?;
                }
            }
            item::INSTALL => as_admin(&["install"], &access::install_service)?,
            item::REMOVE => as_admin(&["uninstall"], &access::uninstall_service)?,
            item::START => as_admin(&["service", "start"], &|| access::service_control(true))?,
            item::STOP => as_admin(&["service", "stop"], &|| access::service_control(false))?,
            _ => {}
        }
        CHANGED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(not(feature = "remote"))]
    let _ = (n, status);
    Ok(())
}

/// The icon's own program's view: the service's state file, and the service manager.
#[cfg(all(windows, feature = "remote"))]
fn read_status() -> Status {
    let mut st = Status { alone: true, ..Default::default() };
    st.service = Some((crate::access::service_installed(), crate::access::service_runs()));
    let v: Option<serde_json::Value> =
        crate::service::state_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str(&t).ok());
    if let Some(v) = v {
        let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if !id.is_empty() {
            st.access = Some((b("enabled"), id));
            st.settings = Some([b("ask"), b("punch"), b("lan"), b("direct")]);
        }
        st.viewers = v.get("viewers").and_then(|x| x.as_array()).map_or(0, |a| a.len());
    }
    st
}

/// `sshremote --tray`: the icon alone, for the service, until it is removed or the icon hidden.
/// One for each signed-in user.
#[cfg(all(windows, feature = "remote"))]
pub fn run_alone() -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = crate::sys::wide(win::ALONE);
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(());
    }
    let status: Shared = std::sync::Arc::new(std::sync::Mutex::new(read_status()));
    let tray = Tray::start(0, status.clone(), super::app_icon().as_ref()).ok_or("the notification area is not there")?;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if QUIT.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let now = read_status();
        // The service was removed: its program file is let go with the icon.
        if now.service.is_some_and(|s| !s.0) {
            break;
        }
        *status.lock().unwrap_or_else(|e| e.into_inner()) = now;
        tray.refresh();
    }
    drop(tray);
    unsafe { CloseHandle(mutex) };
    Ok(())
}

/// Whether the icon's own program runs in this sign-in (the window then makes no second icon).
#[cfg(windows)]
pub(super) fn alone_runs() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let name = crate::sys::wide(win::ALONE);
    let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if h.is_null() {
        return false;
    }
    unsafe { CloseHandle(h) };
    true
}

#[cfg(windows)]
mod win {
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, HICON, ICONINFO, MF_CHECKED,
        MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
        RegisterWindowMessageW, SW_HIDE, SW_RESTORE, SW_SHOW, SendMessageW, SetForegroundWindow, ShowWindow, TPM_NONOTIFY,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
        WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    };

    use super::{QUIT, Shared, act, item};
    use crate::sys::wide;

    const CALLBACK: u32 = WM_APP + 1;
    const SAY: u32 = WM_APP + 2;
    /// The tip read again (the icon's own program, every few seconds).
    const REFRESH: u32 = WM_APP + 3;
    /// The icon's own program, one in each sign-in.
    pub(in crate::gui) const ALONE: &str = "Local\\SshRemoteTray";
    /// The windows of the person connected by the ID: what "show" brings to the front.
    const CONNECTED_WINDOWS: &str = "SSH Remote: someone";

    /// What the window procedure works with: the window shown again, and the menu's status.
    struct Inner {
        main: isize,
        status: Shared,
        icon: isize,
        /// The message Explorer broadcasts when it starts again (its icons are gone then).
        taskbar: u32,
        said: Option<String>,
    }

    static INNER: Mutex<Option<Inner>> = Mutex::new(None);

    /// The icon in the notification area, on a thread of its own; it goes when dropped.
    pub(in crate::gui) struct Tray {
        hwnd: isize,
    }

    impl Tray {
        /// Shows the icon for the window `main` (a top-level window of this program), hidden now.
        pub(in crate::gui) fn start(main: isize, status: Shared, icon: Option<&eframe::egui::IconData>) -> Option<Tray> {
            let hicon = icon.map_or(0, make_icon);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let class = wide("SshRemoteTray");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc_),
                    hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
                    lpszClassName: class.as_ptr(),
                    ..unsafe { std::mem::zeroed() }
                };
                unsafe { RegisterClassW(&wc) };
                // A hidden top-level window: Explorer's broadcast reaches it, unlike a message-only one.
                let title = wide("SSH Remote tray");
                let hwnd = unsafe {
                    CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), wc.hInstance, std::ptr::null())
                };
                if hwnd.is_null() {
                    let _ = tx.send(0);
                    return;
                }
                let taskbar = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
                *INNER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Inner { main, status, icon: hicon, taskbar, said: None });
                add(hwnd as isize, NIM_ADD);
                let _ = tx.send(hwnd as isize);
                let mut msg: MSG = unsafe { std::mem::zeroed() };
                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                if let Some(i) = INNER.lock().unwrap_or_else(|e| e.into_inner()).take()
                    && i.icon != 0
                {
                    unsafe { DestroyIcon(i.icon as HICON) };
                }
            });
            let hwnd = rx.recv().ok().filter(|h| *h != 0)?;
            Some(Tray { hwnd })
        }

        /// The tip and menu read the status again.
        #[cfg_attr(not(feature = "remote"), allow(dead_code))]
        pub(in crate::gui) fn refresh(&self) {
            unsafe { PostMessageW(self.hwnd as HWND, REFRESH, 0, 0) };
        }

        /// A balloon from the icon, once: where the program went.
        pub(in crate::gui) fn say(&self, text: &str) {
            if let Some(i) = INNER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                i.said = Some(text.to_string());
            }
            unsafe { PostMessageW(self.hwnd as HWND, SAY, 0, 0) };
        }

    }

    impl Drop for Tray {
        fn drop(&mut self) {
            // Waited for, so the icon is gone before the program is.
            unsafe { SendMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        }
    }

    /// The window of this program in front now (the one whose close button was just used).
    pub(in crate::gui) fn front_window() -> Option<isize> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        let h = unsafe { GetForegroundWindow() };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, &mut pid) };
        (!h.is_null() && pid == std::process::id()).then_some(h as isize)
    }

    /// Adds, changes or removes the icon.
    fn add(hwnd: isize, what: u32) {
        let inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd as HWND;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
        nid.uCallbackMessage = CALLBACK;
        nid.hIcon = inner.as_ref().map_or(std::ptr::null_mut(), |i| i.icon as HICON);
        let status = inner.as_ref().and_then(|i| i.status.lock().ok().map(|s| s.clone())).unwrap_or_default();
        let mut tip = match &status.access {
            Some((true, id)) => format!("SSH Remote: reachable by ID {}", crate::computers::spaced(id)),
            Some((false, _)) => "SSH Remote: remote access is off".to_string(),
            None => "SSH Remote".to_string(),
        };
        if status.viewers > 0 {
            tip.push_str(&format!("; {} connected now", status.viewers));
        }
        copy(&mut nid.szTip, &tip);
        let mut inner = inner;
        if what == NIM_MODIFY
            && let Some(text) = inner.as_mut().and_then(|i| i.said.take())
        {
            nid.uFlags |= NIF_INFO;
            copy(&mut nid.szInfoTitle, "SSH Remote");
            copy(&mut nid.szInfo, &text);
        }
        drop(inner);
        unsafe { Shell_NotifyIconW(what, &nid) };
    }

    fn copy(to: &mut [u16], text: &str) {
        let w: Vec<u16> = text.encode_utf16().take(to.len() - 1).collect();
        to[..w.len()].copy_from_slice(&w);
        to[w.len()] = 0;
    }

    /// A balloon from the icon now.
    fn tell(hwnd: HWND, text: &str) {
        if let Some(i) = INNER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            i.said = Some(text.to_string());
        }
        add(hwnd as isize, NIM_MODIFY);
    }

    /// The window: the one hidden (the window's icon), else SSH Remote's open window brought to the
    /// front or, with none, started (the icon's own program).
    fn open(main: isize) {
        if main != 0 {
            return show(main);
        }
        use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
        let title = wide("SSH Remote");
        let w = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if !w.is_null() {
            return show(w as isize);
        }
        if let Ok(exe) = std::env::current_exe() {
            let mut c = std::process::Command::new(exe);
            use std::os::windows::process::CommandExt;
            // DETACHED_PROCESS: the window lives on its own, with no console.
            c.creation_flags(0x0000_0008);
            let _ = c.spawn();
        }
    }

    /// Brings the windows of whoever is connected by the ID to the front; how many there were.
    fn show_connected() -> usize {
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW};
        unsafe extern "system" fn each(h: HWND, found: LPARAM) -> windows_sys::core::BOOL {
            let mut buf = [0u16; 128];
            let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
            if String::from_utf16_lossy(&buf[..n]).starts_with(CONNECTED_WINDOWS) {
                show(h as isize);
                unsafe { *(found as *mut usize) += 1 };
            }
            1
        }
        let mut found = 0usize;
        unsafe { EnumWindows(Some(each), &mut found as *mut usize as LPARAM) };
        found
    }

    /// Shows the window again, in front.
    fn show(main: isize) {
        unsafe {
            ShowWindow(main as HWND, SW_SHOW);
            ShowWindow(main as HWND, SW_RESTORE);
            SetForegroundWindow(main as HWND);
        }
    }

    /// Hides `main`, leaving the icon to stand for it.
    pub(in crate::gui) fn hide(main: isize) {
        unsafe { ShowWindow(main as HWND, SW_HIDE) };
    }

    unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let (main, taskbar, status) = match INNER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(i) => (i.main, i.taskbar, i.status.lock().map(|s| s.clone()).unwrap_or_default()),
            None => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        };
        match msg {
            CALLBACK => match lp as u32 {
                WM_LBUTTONUP => open(main),
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let chosen = menu(hwnd, &status);
                    match chosen {
                        0 => {}
                        item::OPEN | item::SETTINGS => open(main),
                        item::SHOW => {
                            if show_connected() == 0 {
                                tell(hwnd, "Nobody connected by this computer's ID has a window open here now.");
                            }
                        }
                        // The icon's own program only goes; the window's quits with it.
                        item::QUIT if main == 0 => QUIT.store(true, Ordering::Relaxed),
                        item::QUIT => {
                            QUIT.store(true, Ordering::Relaxed);
                            show(main);
                            unsafe { PostMessageW(main as HWND, WM_CLOSE, 0, 0) };
                        }
                        n => {
                            if let Err(e) = act(n, &status) {
                                tell(hwnd, &e);
                            }
                        }
                    }
                }
                _ => {}
            },
            SAY | REFRESH => add(hwnd as isize, NIM_MODIFY),
            WM_CLOSE => {
                add(hwnd as isize, NIM_DELETE);
                unsafe {
                    DestroyWindow(hwnd);
                    PostQuitMessage(0);
                }
            }
            m if m == taskbar => add(hwnd as isize, NIM_ADD),
            _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
        0
    }

    /// The menu at the pointer; the item chosen, 0 for none.
    fn menu(hwnd: HWND, s: &super::Status) -> usize {
        let text = |t: &str| wide(t);
        unsafe {
            let m = CreatePopupMenu();
            let add = |m, flags, id: usize, t: &str| {
                let w = text(t);
                AppendMenuW(m, flags, id, w.as_ptr());
            };
            add(m, MF_STRING, item::OPEN, "Open SSH Remote");
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            let check = |on: bool| if on { MF_CHECKED } else { 0 };
            match &s.access {
                Some((on, id)) => {
                    add(m, MF_STRING | check(*on), item::ACCESS, "Remote access by ID");
                    add(m, MF_STRING | if *on { 0 } else { MF_GRAYED }, item::COPY_ID, &format!("Copy the ID {}", crate::computers::spaced(id)));
                }
                None => add(m, MF_STRING | MF_GRAYED, 0, "Remote access by ID: not set up"),
            }
            // Who comes in and how: the switches of the remote access settings.
            let who = CreatePopupMenu();
            match s.settings {
                Some([ask, straight, nearby, direct]) => {
                    // As the remote access settings have them, in their order.
                    add(who, MF_STRING | check(ask), item::ASK, "Ask the person at this computer before letting anyone in");
                    add(who, MF_STRING | check(nearby), item::NEARBY, "Let computers on this network find this one");
                    add(who, MF_STRING | check(direct), item::DIRECT, "Let viewers on this network come straight in");
                    add(who, MF_STRING | check(straight), item::STRAIGHT, "Let viewers by ID go straight through the routers");
                }
                None => add(who, MF_STRING | MF_GRAYED, 0, "Not set up"),
            }
            AppendMenuW(who, MF_SEPARATOR, 0, std::ptr::null());
            add(who, MF_STRING, item::SETTINGS, "All remote access settings\u{2026}");
            add(m, MF_STRING | MF_POPUP, who as usize, "Who comes in");
            let connected = match s.viewers {
                0 => "Nobody is connected".to_string(),
                1 => "1 connected: show the window".to_string(),
                n => format!("{n} connected: show their windows"),
            };
            add(m, MF_STRING | if s.viewers == 0 { MF_GRAYED } else { 0 }, item::SHOW, &connected);
            let svc = CreatePopupMenu();
            match s.service {
                Some((true, runs)) => {
                    add(svc, MF_STRING | if runs { MF_GRAYED } else { 0 }, item::START, "Start");
                    add(svc, MF_STRING | if runs { 0 } else { MF_GRAYED }, item::STOP, "Stop");
                    add(svc, MF_STRING, item::REMOVE, "Remove");
                }
                Some((false, _)) => add(svc, MF_STRING, item::INSTALL, "Install: start remote access with the computer"),
                None => add(svc, MF_STRING | MF_GRAYED, 0, "Looking\u{2026}"),
            }
            let service_label = match s.service {
                Some((true, true)) => "Service: running",
                Some((true, false)) => "Service: stopped",
                _ => "Service",
            };
            add(m, MF_STRING | MF_POPUP, svc as usize, service_label);
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            add(m, MF_STRING, item::QUIT, if s.alone { "Hide this icon until the next sign-in" } else { "Quit" });
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            // The menu closes when clicked away only if its window is in front.
            SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, 0, hwnd, std::ptr::null());
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(m);
            chosen.max(0) as usize
        }
    }

    /// The program's icon at the notification area's size.
    fn make_icon(icon: &eframe::egui::IconData) -> isize {
        const S: usize = 32;
        let (w, h) = (icon.width as usize, icon.height as usize);
        if w == 0 || h == 0 {
            return 0;
        }
        // Box-averaged down to 32x32, as BGRA.
        let mut bgra = vec![0u8; S * S * 4];
        for y in 0..S {
            for x in 0..S {
                let (x0, x1, y0, y1) = (x * w / S, ((x + 1) * w / S).max(x * w / S + 1), y * h / S, ((y + 1) * h / S).max(y * h / S + 1));
                let mut sum = [0u32; 4];
                let mut n = 0;
                for sy in y0..y1.min(h) {
                    for sx in x0..x1.min(w) {
                        let o = (sy * w + sx) * 4;
                        for c in 0..4 {
                            sum[c] += icon.rgba[o + c] as u32;
                        }
                        n += 1;
                    }
                }
                let o = (y * S + x) * 4;
                let n = n.max(1);
                bgra[o..o + 4].copy_from_slice(&[(sum[2] / n) as u8, (sum[1] / n) as u8, (sum[0] / n) as u8, (sum[3] / n) as u8]);
            }
        }
        unsafe {
            let color = CreateBitmap(S as i32, S as i32, 1, 32, bgra.as_ptr().cast());
            let mask_bits = vec![0u8; S * S / 8];
            let mask = CreateBitmap(S as i32, S as i32, 1, 1, mask_bits.as_ptr().cast());
            let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
            let icon = CreateIconIndirect(&info);
            DeleteObject(color);
            DeleteObject(mask);
            icon as isize
        }
    }
}

#[cfg(windows)]
pub(super) use win::{front_window, hide};
