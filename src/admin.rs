//! Administrator rights. On Windows the window asks for them at start (UAC) and keeps working
//! without them; what needs them is shown disabled until it runs elevated.

use std::sync::OnceLock;

/// Whether this process runs with administrator (root) rights.
pub fn elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    *ELEVATED.get_or_init(check)
}

#[cfg(windows)]
fn check() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0;
        let ok = GetTokenInformation(token, TokenElevation, &mut e as *mut _ as _, size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
}

#[cfg(unix)]
fn check() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Whether the window asks for administrator rights when it starts (the default).
pub fn ask_at_start() -> bool {
    cfg!(windows) && crate::hosts::setting("elevate").is_none_or(|v| v != "no")
}

pub fn set_ask_at_start(on: bool) {
    crate::hosts::set_setting("elevate", if on { "yes" } else { "no" });
}

/// Starts this program again with administrator rights through the UAC prompt. Ok(false) when
/// the user declined.
#[cfg(windows)]
pub fn relaunch(args: &[String]) -> Result<bool, String> {
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = wide(&exe.to_string_lossy());
    let params = wide(&args.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(" "));
    let verb = wide("runas");
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut info) } != 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    // ERROR_CANCELLED: the user said no at the prompt.
    if e.raw_os_error() == Some(1223) { Ok(false) } else { Err(e.to_string()) }
}

#[cfg(not(windows))]
pub fn relaunch(_args: &[String]) -> Result<bool, String> {
    Err("run sshremote with sudo for administrator rights".into())
}

/// Whether the sshremote service (sign-in screen, Ctrl+Alt+Del) is installed on this computer.
#[cfg(windows)]
pub fn service_installed() -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("sc")
        .args(["query", crate::service::NAME])
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(not(windows))]
pub fn service_installed() -> bool {
    false
}

/// What administrator rights add, for the tooltip of the badge.
pub const ADDS: &str = "With administrator rights:\n\
    - local terminal tabs run as administrator\n\
    - the sshremote service can be installed here, so this computer can be reached at the sign-in \
    screen and receive Ctrl+Alt+Del\n\
    - windows of elevated programs can be controlled through mcp --here\n\
    Files cannot be dragged in from Explorer while elevated; use the file window instead.";
