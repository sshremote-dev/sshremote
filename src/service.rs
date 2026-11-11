//! Optional Windows service (LocalSystem) for what a user-level agent cannot do:
//! start the screen helper in the console session even when nobody is logged
//! on (the sign-in and lock screens), and raise Ctrl+Alt+Del. Agents ask it
//! over a named pipe that only Administrators and SYSTEM may open.

use std::ffi::c_void;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::process::Command;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SECURITY_ATTRIBUTES, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TokenPrimary,
    TokenSessionId,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_STATUS,
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

/// Installs and starts the service, and lets services raise Ctrl+Alt+Del. Needs an elevated prompt.
pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin = format!("\"{}\" --service", exe.display());
    let _ = run("sc", &["stop", NAME]);
    let _ = run("sc", &["delete", NAME]);
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
    run("sc", &["start", NAME])?;
    println!("sshremote service installed and started ({bin})");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let _ = run("sc", &["stop", NAME]);
    run("sc", &["delete", NAME]).map_err(|e| format!("{e}\n(run this from an elevated prompt)"))?;
    let _ = run("reg", &["delete", POLICY_KEY, "/v", "SoftwareSASGeneration", "/f"]);
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
    // Remote access by ID, from start-up and at the sign-in screen, while it is turned on.
    std::thread::spawn(|| {
        loop {
            let on = || Some(crate::access::load()).filter(|c| c.enabled);
            if on().is_some() {
                crate::access::run(on);
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
    serve();
    report(SERVICE_STOPPED);
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
    loop {
        let h = unsafe {
            CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 4, 4096, 4096, 0, &sa)
        };
        if h == INVALID_HANDLE_VALUE {
            break;
        }
        unsafe { ConnectNamedPipe(h, null_mut()) };
        let mut pipe = unsafe { File::from_raw_handle(h) };
        let mut line = String::new();
        let _ = BufReader::new(&pipe).read_line(&mut line);
        let answer = match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["sas"] => send_sas(),
            ["spawn", base, capture] => spawn_helper(base, capture),
            _ => Err("unknown request".into()),
        };
        let _ = writeln!(pipe, "{}", answer.err().unwrap_or_else(|| "ok".into()));
        let _ = pipe.flush();
        unsafe { DisconnectNamedPipe(h) };
    }
    unsafe { LocalFree(sd as _) };
}

fn send_sas() -> Result<(), String> {
    let lib = wide("sas.dll");
    unsafe {
        let m = LoadLibraryW(lib.as_ptr());
        let f = GetProcAddress(m, c"SendSAS".as_ptr() as *const u8).ok_or("SendSAS is not available")?;
        let send: extern "system" fn(i32) = std::mem::transmute(f);
        send(0);
    }
    Ok(())
}

/// Starts `sshremote --agent --pipe BASE` as SYSTEM in the active console session.
fn spawn_helper(base: &str, capture: &str) -> Result<(), String> {
    if !base.starts_with(r"\\.\pipe\sshremote-") || !capture.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("bad request".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = wide(&format!("\"{}\" --agent --capture {capture} --pipe {base}", exe.display()));
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

/// Sends one request to the service; `None` when no service is installed or reachable.
pub fn ask(request: &str) -> Option<Result<(), String>> {
    let name = wide(PIPE);
    let h = unsafe {
        CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null_mut(), OPEN_EXISTING, 0, null_mut())
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut pipe = unsafe { File::from_raw_handle(h) };
    if writeln!(pipe, "{request}").and_then(|_| pipe.flush()).is_err() {
        return None;
    }
    let mut line = String::new();
    let _ = BufReader::new(&pipe).read_line(&mut line);
    Some(if line.trim() == "ok" { Ok(()) } else { Err(line.trim().to_string()) })
}
