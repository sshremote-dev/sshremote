//! Optional Windows service (LocalSystem) for what a user-level agent cannot do:
//! start the screen helper in the console session even when nobody is logged
//! on (the sign-in and lock screens), and raise Ctrl+Alt+Del. Agents ask it
//! over a named pipe that only Administrators and SYSTEM may open. It also runs
//! remote access by ID, so the computer is reachable before anyone signs in.

use std::ffi::c_void;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::process::Command;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SECURITY_ATTRIBUTES, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TokenPrimary,
    TokenSessionId,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
    SERVICE_QUERY_STATUS, SERVICE_STATUS_PROCESS, RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_STATUS,
    SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
};

pub const NAME: &str = "sshremote";
const PIPE: &str = r"\\.\pipe\sshremote-service";
const POLICY_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";

use crate::sys::wide;

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let out =
        Command::new(cmd).args(args).creation_flags(CREATE_NO_WINDOW).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let text =
            String::from_utf8_lossy(&out.stdout).trim().to_string() + String::from_utf8_lossy(&out.stderr).trim();
        Err(format!("{cmd} {}: {text}", args.first().unwrap_or(&"")))
    }
}

/// The service's own copy of the program: under Program Files, which only administrators can
/// change, so nobody else can swap the program that SYSTEM runs (as they could in a folder of
/// their own, or Downloads).
pub fn installed_exe() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("ProgramFiles")?;
    Some(std::path::PathBuf::from(base).join("SSH Remote").join("sshremote.exe"))
}

/// The icon in the notification area starts with every sign-in, from here.
const RUN_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "SSH Remote";

/// Whether the service is stopped (or not installed).
fn stopped() -> bool {
    use std::os::windows::process::CommandExt;
    Command::new("sc")
        .args(["query", NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|o| !String::from_utf8_lossy(&o.stdout).contains("_PENDING") && !String::from_utf8_lossy(&o.stdout).contains("RUNNING"))
}

/// Installs and starts the service, and lets services raise Ctrl+Alt+Del. Needs an elevated
/// prompt. The service runs a copy of this program kept under Program Files.
/// The firewall rule for questions from computers on this network.
const FIREWALL: &str = "SSH Remote (found on this network)";

pub fn install() -> Result<(), String> {
    let here = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = installed_exe().ok_or("Program Files was not found")?;
    let _ = run("sc", &["stop", NAME]);
    // Its program file is free once it has stopped.
    for _ in 0..100 {
        if stopped() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = run("sc", &["delete", NAME]);
    let same = std::fs::canonicalize(&here).ok().zip(std::fs::canonicalize(&exe).ok()).is_some_and(|(a, b)| a == b);
    let mut failed = None;
    if !same {
        let dir = exe.parent().ok_or("bad path")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", exe.display()))?;
        // Copies moved aside by earlier updates, now that their programs may have ended.
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.starts_with("sshremote") && name.ends_with(".old.exe") {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let mut copied = std::fs::copy(&here, &exe);
        for _ in 0..30 {
            if copied.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            copied = std::fs::copy(&here, &exe);
        }
        // Still in use (this app's own window started from there, say): a running program's
        // file can be renamed though not overwritten, so it moves aside and the new one goes in.
        if copied.is_err() {
            let aside = (0..10).map(|n| dir.join(if n == 0 { "sshremote.old.exe".to_string() } else { format!("sshremote-{n}.old.exe") })).find(|p| !p.exists());
            if let Some(aside) = aside
                && std::fs::rename(&exe, &aside).is_ok()
            {
                copied = std::fs::copy(&here, &exe);
            }
        }
        if let Err(e) = copied {
            // The service comes back with the program it had rather than not at all.
            if !exe.exists() {
                return Err(format!("{}: {e}", exe.display()));
            }
            failed = Some(format!("{}: {e}; the service runs the version it had", exe.display()));
        }
    }
    let bin = format!("\"{}\" --service", exe.display());
    run(
        "sc",
        &[
            "create",
            NAME,
            "binPath=",
            &bin,
            "start=",
            "auto",
            "obj=",
            "LocalSystem",
            "DisplayName=",
            "SSH Remote",
        ],
    )
    .map_err(|e| format!("{e}\n(run this from an elevated prompt)"))?;
    let _ = run(
        "sc",
        &["description", NAME, "SSH Remote: remote access by ID, the screen helper at the sign-in screen, and Ctrl+Alt+Del"],
    );
    // SoftwareSASGeneration = 1: services may simulate Ctrl+Alt+Del.
    run("reg", &["add", POLICY_KEY, "/v", "SoftwareSASGeneration", "/t", "REG_DWORD", "/d", "1", "/f"])?;
    // Computers on this network may ask for it (`lan`); the firewall lets their question in, to it alone.
    let _ = run("netsh", &["advfirewall", "firewall", "delete", "rule", &format!("name={FIREWALL}")]);
    let _ = run(
        "netsh",
        &["advfirewall", "firewall", "add", "rule", &format!("name={FIREWALL}"), "dir=in", "action=allow", "protocol=UDP", &format!("localport={}", crate::lan::PORT), &format!("program={}", exe.display())],
    );
    // And viewers on this network coming straight in.
    let _ = run(
        "netsh",
        &["advfirewall", "firewall", "add", "rule", &format!("name={FIREWALL}"), "dir=in", "action=allow", "protocol=TCP", &format!("localport={}", crate::net::DIRECT_PORT), &format!("program={}", exe.display())],
    );
    // The service shows its icon at each sign-in; the start at sign-in older versions set is gone.
    let _ = run("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
    // The virtual display driver, for screens a viewer adds; the rest works without it.
    if let Err(e) = crate::display::amyuni::install() {
        eprintln!("sshremote: {e}");
    }
    // SSH Remote's printer, whose jobs print on the viewer; the rest works without it.
    if let Err(e) = crate::print::set_up(true) {
        eprintln!("sshremote: {e}");
    }
    run("sc", &["start", NAME])?;
    if let Some(e) = failed {
        return Err(e);
    }
    println!("sshremote service installed and started ({bin})");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let _ = run("sc", &["stop", NAME]);
    run("sc", &["delete", NAME]).map_err(|e| format!("{e}\n(run this from an elevated prompt)"))?;
    // The virtual display driver goes with it, before the folder its tool is in.
    crate::display::amyuni::uninstall();
    let _ = crate::print::set_up(false);
    let _ = run("reg", &["delete", POLICY_KEY, "/v", "SoftwareSASGeneration", "/f"]);
    let _ = run("netsh", &["advfirewall", "firewall", "delete", "rule", &format!("name={FIREWALL}")]);
    let _ = run("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
    if let Some(f) = crate::access::state_file() {
        let _ = f.parent().map(std::fs::remove_dir_all);
    }
    // Its copy goes too, once its processes have let go of it (the icon's notices the service
    // went within a few seconds).
    if let Some(exe) = installed_exe() {
        for _ in 0..100 {
            if !exe.exists() || std::fs::remove_file(&exe).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if let Some(dir) = exe.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    println!("sshremote service removed");
    Ok(())
}

static mut STATUS: SERVICE_STATUS_HANDLE = null_mut();

fn report(state: u32) {
    let st = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP } else { 0 },
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    unsafe { SetServiceStatus(STATUS, &st) };
}

unsafe extern "system" fn control(code: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    if code == SERVICE_CONTROL_STOP {
        report(SERVICE_STOPPED);
        std::process::exit(0);
    }
    0
}

unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let name = wide(NAME);
    unsafe { STATUS = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), null_mut()) };
    report(SERVICE_RUNNING);
    // How remote access stands, for the icon in the notification area; and the icon itself for
    // whoever is signed in now (a sign-in later starts it by itself).
    std::thread::spawn(|| {
        let mut said = String::new();
        // The sessions given the icon, while they are in use. Started from here with no console
        // (a start at sign-in would flash one on Windows before 11 24H2); one already there keeps
        // to itself (`tray::alone_runs`).
        let mut shown = std::collections::HashSet::new();
        loop {
            crate::access::take_end_requests();
            crate::access::publish_state(&mut said);
            let now = crate::console_user::active_sessions();
            shown.retain(|s| now.contains(s));
            for id in now {
                if !shown.contains(&id)
                    && let Some(u) = crate::console_user::User::in_session(id)
                    && let Some(exe) = installed_exe()
                    && crate::console_user::spawn_piped(&u, &format!("\"{}\" --tray", exe.display())).is_ok()
                {
                    shown.insert(id);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    // Newer releases put in place by themselves, between sessions.
    std::thread::spawn(|| crate::update::unattended(|| crate::access::viewers_here() == 0));
    // Remote access by ID, from start-up and at the sign-in screen, while it is turned on.
    std::thread::spawn(|| {
        loop {
            let on = || crate::access::recent().filter(|c| c.enabled);
            if on().is_some() {
                crate::access::run(on);
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
    serve();
    report(SERVICE_STOPPED);
}

/// After an update: a helper (the new program) starts the service again once this one has
/// stopped, as the service manager does not by itself.
pub fn start_again() {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--start-service-again");
        crate::sys::quiet(&mut cmd);
        let _ = cmd.spawn();
    }
    report(SERVICE_STOPPED);
}

/// What the MSI package runs once its files are in place (`--msi installed SERVICE AUTOUPDATE`,
/// each "1", "0" or empty as its properties are), and before it takes them away (`--msi
/// removing`): the service installed, or started again on the new program if it was there.
pub fn msi(args: &[String]) -> Result<(), String> {
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    match arg(0) {
        "installed" => {
            if let w @ ("0" | "1") = arg(2) {
                crate::access::cli(&["auto-update".into(), if w == "1" { "on" } else { "off" }.into()])?;
            }
            let r = if arg(1) == "1" {
                crate::access::cli(&["install".into()])
            } else if crate::admin::service_installed() {
                let _ = run("sc.exe", &["stop", NAME]);
                start_when_stopped()
            } else {
                Ok(())
            };
            // Copies the package moved aside, once nothing runs them.
            if let Some(dir) = installed_exe().as_deref().and_then(std::path::Path::parent) {
                for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                    let name = e.file_name().to_string_lossy().to_lowercase();
                    if name.starts_with("sshremote") && name.ends_with(".old.exe") {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            r
        }
        "removing" if crate::admin::service_installed() => uninstall(),
        "removing" => Ok(()),
        _ => Err("usage: sshremote --msi installed SERVICE AUTOUPDATE | removing".into()),
    }
}

/// `--start-service-again`: waits for the service to stop, then starts it (on the new program).
pub fn start_when_stopped() -> Result<(), String> {
    for _ in 0..60 {
        if stopped() {
            return run("sc.exe", &["start", NAME]);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    run("sc.exe", &["start", NAME])
}

/// Entry point for `sshremote --service`, run by the service control manager.
pub fn dispatch() -> Result<(), String> {
    let mut name = wide(NAME);
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lpServiceName: null_mut(), lpServiceProc: None },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(format!("not started by the service manager: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Answers requests one connection at a time: "spawn PIPE_BASE CAPTURE" or "sas".
fn serve() {
    // Administrators and SYSTEM only: the service can show and control the console as SYSTEM.
    let sddl = wide("D:P(A;;GA;;;BA)(A;;GA;;;SY)");
    let mut sd = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
    } == 0
    {
        return;
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let name = wide(PIPE);
    // One instance, made first and kept: the name is never free between requests for another
    // program to take. One already there (someone else's) is waited out rather than shared.
    let h = loop {
        let h = unsafe {
            CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, &sa)
        };
        if h != INVALID_HANDLE_VALUE {
            break h;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    };
    // The handle stays the pipe's; reads and writes borrow it.
    let pipe = std::mem::ManuallyDrop::new(unsafe { File::from_raw_handle(h) });
    loop {
        // A client that connected before this call is connected all the same.
        if unsafe { ConnectNamedPipe(h, null_mut()) } == 0
            && std::io::Error::last_os_error().raw_os_error() != Some(535)
        {
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        let mut line = String::new();
        let _ = BufReader::new(&*pipe).read_line(&mut line);
        let answer = match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["sas"] => send_sas(),
            ["spawn", base, capture] => spawn_helper(base, capture, None, None, false, ""),
            // By ID: what the viewer may do, the code to show with the question when it asked
            // without a password, and whether it may see the cameras, each when given.
            // Then the session's other settings, as words of `agent::helper_words`.
            ["spawn", base, capture, "id", all @ ..] => {
                let at = all.iter().position(|w| w.starts_with("way=")).unwrap_or(all.len());
                let (rest, words) = all.split_at(at);
                let cameras = rest.last() == Some(&"cameras");
                match (&rest[..rest.len() - cameras as usize], crate::agent::helper_args(words)) {
                    (_, Err(e)) => Err(e),
                    ([], Ok(more)) => spawn_helper(base, capture, Some(crate::grant::ALL), None, cameras, &more),
                    ([grant], Ok(more)) => match grant.parse() {
                        Ok(g) => spawn_helper(base, capture, Some(g), None, cameras, &more),
                        Err(_) => Err("bad request".into()),
                    },
                    ([grant, code], Ok(more)) => match grant.parse() {
                        Ok(g) if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) => spawn_helper(base, capture, Some(g), Some(code), cameras, &more),
                        _ => Err("bad request".into()),
                    },
                    _ => Err("bad request".into()),
                }
            }
            _ => Err("unknown request".into()),
        };
        let _ = writeln!(&*pipe, "{}", answer.err().unwrap_or_else(|| "ok".into()));
        unsafe {
            FlushFileBuffers(h);
            DisconnectNamedPipe(h);
        }
    }
}

fn send_sas() -> Result<(), String> {
    let lib = wide("sas.dll");
    unsafe {
        // From System32 only: never a sas.dll put beside the program.
        let m = LoadLibraryExW(lib.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        let f = GetProcAddress(m, c"SendSAS".as_ptr() as *const u8).ok_or("SendSAS is not available")?;
        let send: extern "system" fn(i32) = std::mem::transmute(f);
        send(0);
    }
    Ok(())
}

/// Starts `sshremote --agent --pipe BASE` as SYSTEM in the active console session.
fn spawn_helper(base: &str, capture: &str, by_id: Option<u8>, code: Option<&str>, cameras: bool, more: &str) -> Result<(), String> {
    let name = base.strip_prefix(r"\\.\pipe\sshremote-").unwrap_or("");
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || !capture.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("bad request".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let code = code.map(|c| format!(" --code {c}")).unwrap_or_default();
    let cameras = if cameras { " --cameras" } else { "" };
    let by_id = by_id.map(|g| format!(" --by-id --grant {g}{code}{cameras}{more}")).unwrap_or_default();
    let mut cmd = wide(&format!("\"{}\" --agent --capture {capture} --pipe {base}{by_id}", exe.display()));
    let mut desktop = wide(r"winsta0\default");
    unsafe {
        let (mut own, mut dup): (HANDLE, HANDLE) = (null_mut(), null_mut());
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own) == 0 {
            return Err(format!("OpenProcessToken: {}", std::io::Error::last_os_error()));
        }
        let ok = DuplicateTokenEx(own, TOKEN_ALL_ACCESS, null_mut(), SecurityImpersonation, TokenPrimary, &mut dup);
        CloseHandle(own);
        if ok == 0 {
            return Err(format!("DuplicateTokenEx: {}", std::io::Error::last_os_error()));
        }
        let mut session = WTSGetActiveConsoleSessionId();
        SetTokenInformation(dup, TokenSessionId, &mut session as *mut u32 as _, 4);
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop.as_mut_ptr();
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessAsUserW(
            dup,
            null_mut(),
            cmd.as_mut_ptr(),
            null_mut(),
            null_mut(),
            0,
            CREATE_NO_WINDOW,
            null_mut(),
            null_mut(),
            &si,
            &mut pi,
        );
        CloseHandle(dup);
        if ok == 0 {
            return Err(format!("CreateProcessAsUserW: {}", std::io::Error::last_os_error()));
        }
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// The running service's process id, from the service manager.
fn service_pid() -> Option<u32> {
    let name = wide(NAME);
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return None;
        }
        let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
        let mut st: SERVICE_STATUS_PROCESS = std::mem::zeroed();
        let mut needed = 0u32;
        let ok = !svc.is_null()
            && QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, &mut st as *mut _ as *mut u8, size_of::<SERVICE_STATUS_PROCESS>() as u32, &mut needed) != 0;
        if !svc.is_null() {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        (ok && st.dwProcessId != 0).then_some(st.dwProcessId)
    }
}

/// Sends one request to the service; `None` when no service is installed or reachable.
pub fn ask(request: &str) -> Option<Result<(), String>> {
    let name = wide(PIPE);
    // Identification only: whoever answers cannot act as the caller.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null_mut(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut pipe = unsafe { File::from_raw_handle(h) };
    // The pipe must be the service's own, not one another program made under its name.
    let mut server = 0u32;
    if unsafe { GetNamedPipeServerProcessId(h, &mut server) } == 0 || service_pid() != Some(server) {
        return Some(Err("the service's pipe is held by another program".into()));
    }
    if writeln!(pipe, "{request}").and_then(|_| pipe.flush()).is_err() {
        return None;
    }
    let mut line = String::new();
    let _ = BufReader::new(&pipe).read_line(&mut line);
    Some(if line.trim() == "ok" { Ok(()) } else { Err(line.trim().to_string()) })
}
