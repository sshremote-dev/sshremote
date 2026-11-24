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

/// This computer's monitors as the system places them, in pixels (left, top, width, height):
/// the primary first, then from left to right, as the agent numbers a host's. Windows only (a
/// Wayland session does not let a program place its windows); empty elsewhere.
pub fn monitors() -> Vec<(i32, i32, i32, i32)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{LPARAM, RECT};
        use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
        use windows_sys::core::BOOL;
        unsafe extern "system" fn add(m: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
            // SAFETY: `list` is the vector handed to EnumDisplayMonitors below, alive for its call.
            let list = unsafe { &mut *(list as *mut Vec<(bool, i32, i32, i32, i32)>) };
            let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(m, &mut mi) } != 0 {
                let r = mi.rcMonitor;
                list.push((mi.dwFlags & 1 != 0, r.left, r.top, r.right - r.left, r.bottom - r.top));
            }
            1
        }
        let mut list: Vec<(bool, i32, i32, i32, i32)> = Vec::new();
        // SAFETY: the callback only writes into `list`, which outlives the call.
        unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(add), &mut list as *mut _ as LPARAM) };
        list.sort_by_key(|m| (!m.0, m.1, m.2));
        list.into_iter().map(|m| (m.1, m.2, m.3, m.4)).collect()
    }
    #[cfg(not(windows))]
    Vec::new()
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
