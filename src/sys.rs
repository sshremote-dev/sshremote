//! Small things every part of the program needs from the system, in one place: the home folder,
//! starting helper programs without a console window, and Windows strings and command lines.

use std::path::PathBuf;
use std::process::Command;

/// Marks input this program injects (Windows' dwExtraInfo), so the keyboard and mouse it keeps
/// out, and the keys the viewer takes from Windows, are only others'.
#[cfg_attr(not(windows), allow(dead_code))]
pub const INJECTED: usize = 0x5352_4d54;

/// The user's home folder (USERPROFILE on Windows, HOME elsewhere).
pub fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default()
}

/// `~/.ssh`.
pub fn ssh_dir() -> PathBuf {
    home_dir().join(".ssh")
}

/// A helper program started from a window: on Windows it gets no console window of its own.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    cmd
}

/// Whether Insert is down now: Ctrl+Insert and Shift+Insert reach the windows as a copy and a
/// paste, and only this tells them from Ctrl+C and Ctrl+V.
pub fn insert_held() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_INSERT};
        // SAFETY: a plain query of the key's state.
        unsafe { GetAsyncKeyState(VK_INSERT as i32) < 0 }
    }
    #[cfg(not(windows))]
    false
}

/// Where the mouse pointer is on the screen, in pixels, where the system says (Windows; None
/// elsewhere). Files dragged over a window move no pointer events there, so this is how a drop
/// knows where it landed.
pub fn pointer_on_screen() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT { x: 0, y: 0 };
        // SAFETY: a plain query into a local.
        (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
    }
    #[cfg(not(windows))]
    None
}

/// One of this computer's monitors as the system places it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    /// Its number in the windowing library's own order: what `ViewportCommand::SetMonitor` takes.
    pub index: usize,
    /// Its top left and size: pixels, points on macOS, the compositor's units under Wayland.
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub primary: bool,
}

/// This computer's monitors: the primary first, then from left to right, as the agent numbers a
/// host's. Empty where they cannot be read.
pub fn monitors() -> Vec<Local> {
    #[cfg(test)]
    if let Some(list) = TEST_MONITORS.with(|t| t.borrow().clone()) {
        return list;
    }
    // Read again at most every two seconds: a menu asks each frame it is open.
    static LAST: std::sync::Mutex<Option<(std::time::Instant, Vec<Local>)>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = last.as_ref()
        && at.elapsed() < std::time::Duration::from_secs(2)
    {
        return list.clone();
    }
    let mut list = monitors_listed();
    list.sort_by_key(|m| (!m.primary, m.x, m.y));
    *last = Some((std::time::Instant::now(), list.clone()));
    list
}

#[cfg(test)]
thread_local! {
    /// Tests: the monitors this computer seems to have.
    pub static TEST_MONITORS: std::cell::RefCell<Option<Vec<Local>>> = const { std::cell::RefCell::new(None) };
}

/// The monitors in the order winit lists them (`available_monitors`), so `index` is its own.
fn monitors_listed() -> Vec<Local> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{LPARAM, RECT};
        use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
        use windows_sys::core::BOOL;
        unsafe extern "system" fn add(m: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
            // SAFETY: `list` is the vector handed to EnumDisplayMonitors below, alive for its call.
            let list = unsafe { &mut *(list as *mut Vec<Local>) };
            let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(m, &mut mi) } != 0 {
                let r = mi.rcMonitor;
                let index = list.len();
                list.push(Local { index, x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top, primary: mi.dwFlags & 1 != 0 });
            }
            1
        }
        let mut list: Vec<Local> = Vec::new();
        // SAFETY: the callback only writes into `list`, which outlives the call.
        unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(add), &mut list as *mut _ as LPARAM) };
        list
    }
    #[cfg(all(target_os = "macos", feature = "remote"))]
    {
        use objc2_core_graphics::{CGDisplayBounds, CGGetActiveDisplayList, CGMainDisplayID};
        let (mut ids, mut n) = ([0u32; 16], 0u32);
        // SAFETY: room for 16 displays; their count is written back.
        unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) };
        let main = CGMainDisplayID();
        ids[..(n as usize).min(16)]
            .iter()
            .enumerate()
            .map(|(index, &id)| {
                let b = CGDisplayBounds(id);
                let (x, y, w, h) = (b.origin.x as i32, b.origin.y as i32, b.size.width as i32, b.size.height as i32);
                Local { index, x, y, w, h, primary: id == main }
            })
            .collect()
    }
    #[cfg(all(target_os = "linux", feature = "remote", feature = "gui"))]
    {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty()) || std::env::var_os("WAYLAND_SOCKET").is_some();
        if wayland && let Some(list) = wayland_outputs() {
            return list;
        }
        x11_monitors().unwrap_or_default()
    }
    #[cfg(not(any(windows, all(target_os = "macos", feature = "remote"), all(target_os = "linux", feature = "remote", feature = "gui"))))]
    Vec::new()
}

/// X11: the CRTCs that show something, as winit lists them; the primary is the one whose first
/// output RandR names primary, else the first.
#[cfg(all(target_os = "linux", feature = "remote", feature = "gui"))]
fn x11_monitors() -> Option<Vec<Local>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt;
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let res = conn.randr_get_screen_resources_current(root).ok()?.reply().ok()?;
    let primary = conn.randr_get_output_primary(root).ok()?.reply().ok()?.output;
    let mut list = Vec::new();
    for crtc in res.crtcs {
        let Some(c) = conn.randr_get_crtc_info(crtc, x11rb::CURRENT_TIME).ok().and_then(|c| c.reply().ok()) else { continue };
        if c.width == 0 || c.height == 0 || c.outputs.is_empty() {
            continue;
        }
        let index = list.len();
        list.push(Local { index, x: c.x as i32, y: c.y as i32, w: c.width as i32, h: c.height as i32, primary: c.outputs[0] == primary });
    }
    if !list.iter().any(|m| m.primary)
        && let Some(first) = list.first_mut()
    {
        first.primary = true;
    }
    Some(list)
}

/// Wayland: the outputs in the order the compositor announces them, as winit binds them, placed
/// where xdg-output says (wl_output's own place is 0,0 under wlroots); none is primary there, so
/// the one at the far left takes its place.
#[cfg(all(target_os = "linux", feature = "remote", feature = "gui"))]
fn wayland_outputs() -> Option<Vec<Local>> {
    use wayland_client::protocol::wl_output::{self, WlOutput};
    use wayland_client::protocol::wl_registry::{self, WlRegistry};
    use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
    use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
    use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
    #[derive(Default)]
    struct Outs {
        list: Vec<(WlOutput, Local)>,
        manager: Option<ZxdgOutputManagerV1>,
    }
    impl Dispatch<WlRegistry, ()> for Outs {
        fn event(s: &mut Self, reg: &WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { name, interface, version } = ev {
                if interface == "wl_output" {
                    let index = s.list.len();
                    let out = reg.bind::<WlOutput, usize, Self>(name, version.min(4), qh, index);
                    s.list.push((out, Local { index, x: 0, y: 0, w: 0, h: 0, primary: false }));
                } else if interface == "zxdg_output_manager_v1" {
                    s.manager = Some(reg.bind::<ZxdgOutputManagerV1, (), Self>(name, version.min(3), qh, ()));
                }
            }
        }
    }
    impl Dispatch<WlOutput, usize> for Outs {
        fn event(s: &mut Self, _: &WlOutput, ev: wl_output::Event, index: &usize, _: &Connection, _: &QueueHandle<Self>) {
            let Some((_, m)) = s.list.get_mut(*index) else { return };
            match ev {
                wl_output::Event::Geometry { x, y, .. } => (m.x, m.y) = (x, y),
                wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. } if f.contains(wl_output::Mode::Current) => (m.w, m.h) = (width, height),
                _ => {}
            }
        }
    }
    impl Dispatch<ZxdgOutputManagerV1, ()> for Outs {
        fn event(_: &mut Self, _: &ZxdgOutputManagerV1, _: <ZxdgOutputManagerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<ZxdgOutputV1, usize> for Outs {
        fn event(s: &mut Self, _: &ZxdgOutputV1, ev: zxdg_output_v1::Event, index: &usize, _: &Connection, _: &QueueHandle<Self>) {
            let Some((_, m)) = s.list.get_mut(*index) else { return };
            match ev {
                zxdg_output_v1::Event::LogicalPosition { x, y } => (m.x, m.y) = (x, y),
                zxdg_output_v1::Event::LogicalSize { width, height } => (m.w, m.h) = (width, height),
                _ => {}
            }
        }
    }
    let conn = Connection::connect_to_env().ok()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut outs = Outs::default();
    queue.roundtrip(&mut outs).ok()?;
    queue.roundtrip(&mut outs).ok()?;
    if let Some(manager) = outs.manager.clone() {
        for (i, (out, _)) in outs.list.iter().enumerate() {
            manager.get_xdg_output(out, &qh, i);
        }
        queue.roundtrip(&mut outs).ok()?;
    }
    let mut list: Vec<Local> = outs.list.into_iter().map(|(_, m)| m).collect();
    if let Some(left) = list.iter().map(|m| (m.x, m.y, m.index)).min().map(|l| l.2) {
        list[left].primary = true;
    }
    Some(list)
}

/// Now, in seconds since 1970.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Bytes as lowercase hex.
#[cfg(any(feature = "remote", test))]
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Bytes from hex digits as [`hex`] writes them; None unless the text is pairs of hex digits.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// `n` bytes from the system's random source.
#[cfg(feature = "remote")]
pub fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("system random source");
    b
}

/// A string as Windows wants it: UTF-16 ending in a zero.
#[cfg(windows)]
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[cfg(any(windows, test))]
/// One argument quoted the way the C runtime splits a command line: backslashes before a quote,
/// and at the end before the closing quote, are doubled.
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut q = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                q.push_str(&"\\".repeat(slashes * 2 + 1));
                q.push('"');
                slashes = 0;
            }
            c => {
                q.push_str(&"\\".repeat(slashes));
                q.push(c);
                slashes = 0;
            }
        }
    }
    q.push_str(&"\\".repeat(slashes * 2));
    q.push('"');
    q
}

/// A command line from separate arguments.
#[cfg(windows)]
pub fn command_line(argv: &[String]) -> String {
    argv.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")
}

/// A modification time as local "YYYY-MM-DD HH:MM".
pub fn local_time(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let (y, mo, d, h, mi) = local_parts(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

#[cfg(unix)]
fn local_parts(secs: u64) -> (i64, u32, u32, u32, u32) {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    (tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32)
}

#[cfg(windows)]
fn local_parts(secs: u64) -> (i64, u32, u32, u32, u32) {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ticks = (secs + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc);
        if SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            local = utc;
        }
    }
    (local.wYear as i64, local.wMonth as u32, local.wDay as u32, local.wHour as u32, local.wMinute as u32)
}

/// A size for people: 1.5 MB.
#[cfg_attr(not(feature = "remote"), allow(dead_code))]
pub fn human(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

/// Room left for this user on the disk that holds `path` (an existing file or folder), in bytes.
#[cfg_attr(not(feature = "remote"), allow(dead_code))]
pub fn free_space(path: &std::path::Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let dir = if path.is_dir() { path } else { path.parent()? };
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: a zero-ended path and valid out-pointers.
        (unsafe { GetDiskFreeSpaceExW(wide(&dir.to_string_lossy()).as_ptr(), &mut avail, &mut total, &mut free) } != 0).then_some(avail)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: a zero-ended path and a zeroed statvfs to fill.
        unsafe {
            let mut st: libc::statvfs = std::mem::zeroed();
            (libc::statvfs(c.as_ptr(), &mut st) == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
        }
    }
}

/// Writes a file only this user may read, made so from the start (as ssh insists on for its
/// configuration and keys, and as secrets need).
pub fn write_private(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path).and_then(|mut f| std::io::Write::write_all(&mut f, data)).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Keeps this computer awake, its screen on, for as long as it lives: during a session, on the
/// computer shown (its picture would go black) and on the one watching (nobody types there).
pub struct KeepAwake {
    #[cfg(windows)]
    stop: Option<std::sync::mpsc::Sender<()>>,
    #[cfg(not(windows))]
    child: Option<std::process::Child>,
    /// Linux: the desktop portal's inhibition, held while this connection to the session bus
    /// lives.
    #[cfg(all(unix, not(target_os = "macos"), feature = "remote"))]
    _bus: Option<zbus::Connection>,
}

pub fn keep_awake() -> KeepAwake {
    // Windows: the state of the thread that sets it, so a thread of its own holds it.
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            // SAFETY: plain flags for this thread.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED) };
            let _ = rx.recv();
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        });
        KeepAwake { stop: Some(tx) }
    }
    // macOS: caffeinate, for as long as this program runs (or until let go).
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("/usr/bin/caffeinate");
        cmd.args(["-di", "-w", &std::process::id().to_string()]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        KeepAwake { child: cmd.spawn().ok() }
    }
    // Linux: the desktop's own (the portal's Inhibit, which a service of the person's session may
    // ask for too); else logind's lock, held by a child that waits for nothing (polkit lets only
    // the session at the screen have it).
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        #[cfg(feature = "remote")]
        if let Some(bus) = portal_inhibit() {
            return KeepAwake { child: None, _bus: Some(bus) };
        }
        let mut cmd = Command::new("systemd-inhibit");
        cmd.args(["--what=idle:sleep", "--who=SSH Remote", "--why=A remote session", "--mode=block", "sleep", "infinity"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        KeepAwake {
            child: cmd.spawn().ok(),
            #[cfg(feature = "remote")]
            _bus: None,
        }
    }
}

/// The desktop's picture taken away for as long as it lives (a plain color to send instead), and
/// put back after. Windows: in memory only, so signing out brings it back too; GNOME, Cinnamon
/// and MATE: what it was is written down first, put back by the next session if this one could
/// not.
pub struct PlainDesktop {
    #[cfg(windows)]
    was: Option<Vec<u16>>,
    #[cfg(all(unix, not(target_os = "macos")))]
    was: Option<(&'static str, String)>,
}

#[cfg(all(unix, not(target_os = "macos")))]
fn plain_note() -> Option<std::path::PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("desktop-picture-was"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn gsettings(args: &[&str]) -> Option<String> {
    let out = Command::new("gsettings").args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn plain_desktop() -> PlainDesktop {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SystemParametersInfoW};
        let mut buf = vec![0u16; 1024];
        // SAFETY: a buffer of the size given; the empty name is NUL-terminated.
        let ok = unsafe { SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, buf.as_mut_ptr().cast(), 0) };
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        if ok == 0 || len == 0 {
            return PlainDesktop { was: None };
        }
        buf.truncate(len + 1);
        let mut none = [0u16; 1];
        unsafe { SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, none.as_mut_ptr().cast(), SPIF_SENDCHANGE) };
        PlainDesktop { was: Some(buf) }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let desk = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
        let schema = match () {
            _ if desk.contains("cinnamon") => "org.cinnamon.desktop.background",
            _ if desk.contains("mate") => "org.mate.background",
            _ if ["gnome", "unity", "budgie", "pop"].iter().any(|d| desk.contains(d)) => "org.gnome.desktop.background",
            _ => return PlainDesktop { was: None },
        };
        // One a session before did not put back (it ended at once) is what it was.
        let note = plain_note();
        let left = note.as_ref().and_then(|n| std::fs::read_to_string(n).ok()).and_then(|t| t.split_once(' ').map(|(s, v)| (s.to_string(), v.trim().to_string())));
        let was = match left {
            Some((s, v)) if s == schema => v,
            _ => match gsettings(&["get", schema, "picture-options"]) {
                Some(v) => v,
                None => return PlainDesktop { was: None },
            },
        };
        if was.trim_matches('\'') == "none" || !was.trim_matches('\'').chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return PlainDesktop { was: None };
        }
        if let Some(n) = &note {
            let _ = n.parent().map(std::fs::create_dir_all);
            let _ = write_private(n, format!("{schema} {was}\n").as_bytes());
        }
        let _ = gsettings(&["set", schema, "picture-options", "none"]);
        PlainDesktop { was: Some((schema, was)) }
    }
    #[cfg(target_os = "macos")]
    PlainDesktop {}
}

impl Drop for PlainDesktop {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(mut was) = self.was.take() {
            use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SystemParametersInfoW};
            // SAFETY: the NUL-terminated name taken before.
            unsafe { SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, was.as_mut_ptr().cast(), SPIF_SENDCHANGE) };
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some((schema, was)) = self.was.take()
            && gsettings(&["set", schema, "picture-options", was.trim_matches('\'')]).is_some()
            && let Some(n) = plain_note()
        {
            let _ = std::fs::remove_file(n);
        }
    }
}

/// The portal's Inhibit of suspending and going idle (4 | 8), for as long as the connection
/// asking lives.
#[cfg(all(unix, not(target_os = "macos"), feature = "remote"))]
fn portal_inhibit() -> Option<zbus::Connection> {
    use std::collections::HashMap;
    zbus::block_on(async {
        let conn = zbus::Connection::session().await.ok()?;
        let p = zbus::Proxy::new(&conn, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Inhibit").await.ok()?;
        let options: HashMap<&str, zbus::Variant> = HashMap::from([("reason", zbus::Variant::from("A remote session"))]);
        let _: zbus::OwnedObjectPath = p.call("Inhibit", &("", 4u32 | 8u32, options)).await.ok()?;
        Some(conn)
    })
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        #[cfg(not(windows))]
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// `path` opened to read, or to append to (made for its user alone if need be), as a file of its
/// own: never through a link, nor one with another name (or nothing is opened). Root writing in a
/// person's folder (the login screen's remote access) is so not led to another file by a link
/// they put there.
pub fn open_own(path: &std::path::Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    if append {
        o.create(true).append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = o.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = f.metadata()?;
        if !m.is_file() || m.nlink() != 1 {
            return Err(std::io::Error::other("not a file of its own"));
        }
    }
    Ok(f)
}

/// Writes `data` to `path` whole or not at all: beside it first, then renamed over it, so a
/// reader never meets half a file. `private`: readable by its user only, from the start;
/// `durable`: on the disk before it takes the old one's place, so not even a crash loses both.
pub fn write_whole(path: &std::path::Path, data: &[u8], private: bool, durable: bool) -> std::io::Result<()> {
    // One of its own for each write, so two at once (threads of this program too) never share it.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.{}.{n}.new", std::process::id()));
    // A leftover could keep wider permissions: a new file gets the mode.
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let done = o
        .open(&tmp)
        .and_then(|mut h| {
            std::io::Write::write_all(&mut h, data)?;
            if durable { h.sync_all() } else { Ok(()) }
        })
        .and_then(|_| std::fs::rename(&tmp, path));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Today in UTC as YYYY-MM-DD.
pub fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86400) as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Ends `child` together with this program, however this program ends (closed, crashed or
/// killed): on Windows it joins a job whose processes the system ends when its last handle, ours,
/// closes. Otherwise a helper started from this program could keep running, and keep its file
/// from being deleted.
pub fn tie(child: &std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        tie_handle(child.as_raw_handle() as _);
    }
    #[cfg(not(windows))]
    let _ = child;
}

/// `tie` for a process made otherwise (its handle).
#[cfg(windows)]
pub fn tie_handle(process: windows_sys::Win32::Foundation::HANDLE) {
    {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        };
        static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let job = *JOB.get_or_init(|| unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            job as usize
        });
        if job != 0 {
            unsafe { AssignProcessToJobObject(job as _, process) };
        }
    }
}

/// Starts `cmd` tied to this program (see `tie`). On Unix it leads a process group of its own,
/// so `end_tree` ends what it starts as well.
pub fn spawn_tied(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    tie(&child);
    Ok(child)
}

/// Ends a child started with `spawn_tied`, with its process group on Unix, and waits for it.
pub fn end_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(group) = i32::try_from(child.id()) {
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    /// Run in a signed-in session of a test machine (it takes the desktop's picture away).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn the_desktop_picture_comes_back_on_windows() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETDESKWALLPAPER, SystemParametersInfoW};
        let now = || {
            let mut buf = vec![0u16; 1024];
            unsafe { SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, buf.as_mut_ptr().cast(), 0) };
            String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)])
        };
        let before = now();
        assert!(!before.is_empty(), "a picture to take away");
        let plain = super::plain_desktop();
        assert_eq!(now(), "");
        drop(plain);
        assert_eq!(now(), before);
    }

    /// Run alone with gsettings' file backend in a home of its own (`GSETTINGS_BACKEND=keyfile
    /// XDG_CONFIG_HOME=... XDG_CURRENT_DESKTOP=GNOME`), never with the person's own settings.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    #[ignore]
    fn the_desktop_picture_comes_back_after_a_session_even_one_that_died() {
        assert_eq!(std::env::var("GSETTINGS_BACKEND").as_deref(), Ok("keyfile"), "a settings file of its own only");
        let get = || super::gsettings(&["get", "org.gnome.desktop.background", "picture-options"]).unwrap();
        super::gsettings(&["set", "org.gnome.desktop.background", "picture-options", "zoom"]).unwrap();
        let plain = super::plain_desktop();
        assert_eq!(get(), "'none'");
        drop(plain);
        assert_eq!(get(), "'zoom'");
        assert!(!super::plain_note().unwrap().exists());
        // A session that ended before it could put it back: the next one knows what it was.
        let plain = super::plain_desktop();
        std::mem::forget(plain);
        assert_eq!(get(), "'none'");
        let plain = super::plain_desktop();
        drop(plain);
        assert_eq!(get(), "'zoom'");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_of_its_own_opens_and_a_link_does_not() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("sshremote-own-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (own, target) = (dir.join("own"), dir.join("target"));
        std::fs::write(&target, "kept").unwrap();
        super::open_own(&own, true).unwrap().write_all(b"a line\n").unwrap();
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "a line\n");
        // Put in its place: a link, or another name of a file elsewhere.
        std::fs::remove_file(&own).unwrap();
        std::os::unix::fs::symlink(&target, &own).unwrap();
        assert!(super::open_own(&own, true).is_err() && super::open_own(&own, false).is_err());
        std::fs::remove_file(&own).unwrap();
        std::fs::hard_link(&target, &own).unwrap();
        assert!(super::open_own(&own, true).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arguments_are_quoted_for_the_c_runtime() {
        assert_eq!(super::quote_arg("plain"), "plain");
        assert_eq!(super::quote_arg("two words"), "\"two words\"");
        assert_eq!(super::quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(super::quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(super::quote_arg(""), "\"\"");
    }

    #[test]
    fn hex_comes_back_as_bytes_and_nothing_else_does() {
        assert_eq!(super::unhex(&super::hex(&[0, 0xab, 0xff, 16])), Some(vec![0, 0xab, 0xff, 16]));
        assert_eq!(super::unhex("ABcd"), Some(vec![0xab, 0xcd]));
        assert_eq!(super::unhex(""), Some(vec![]));
        for bad in ["0", "abc", "zz", "+f", "-1", "0x", " 1"] {
            assert_eq!(super::unhex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_file_is_written_whole_and_private_ones_stay_private() {
        let dir = std::env::temp_dir().join(format!("sshremote-whole-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("settings.json");
        std::fs::write(&f, "old").unwrap();
        super::write_whole(&f, b"new", true, true).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600, "a file that was readable by all is not any more");
        }
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("settings.json")], "nothing is left beside it");
        assert!(super::write_whole(&dir.join("no-such-folder").join("x"), b"x", false, false).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
