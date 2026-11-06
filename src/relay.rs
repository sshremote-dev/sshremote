//! Windows OpenSSH runs sessions outside the console session, where the screen
//! cannot be read. The agent there starts a helper in the logged-on user's
//! console session through a one-shot scheduled task and relays bytes to it
//! over two named pipes.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// SDDL granting the pipe only to this user and SYSTEM. The default DACL would
/// leave a non-elevated helper of an elevated session read-only access.
fn user_only_sddl() -> Result<Vec<u16>, String> {
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(format!("OpenProcessToken: {}", io::Error::last_os_error()));
        }
        let mut buf = vec![0u8; 256];
        let mut len = 0;
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as _, buf.len() as u32, &mut len);
        CloseHandle(token);
        if ok == 0 {
            return Err(format!("GetTokenInformation: {}", io::Error::last_os_error()));
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(format!("ConvertSidToStringSidW: {}", io::Error::last_os_error()));
        }
        let n = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
        LocalFree(text as _);
        Ok(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)").encode_utf16().chain([0]).collect())
    }
}

fn pipe(name: &str, access: u32, sddl: &[u16]) -> Result<File, String> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut sd = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!("security descriptor: {}", io::Error::last_os_error()));
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let h = unsafe { CreateNamedPipeW(wide.as_ptr(), access, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 1 << 16, 1 << 16, 0, &sa) };
    unsafe { LocalFree(sd as _) };
    if h == INVALID_HANDLE_VALUE {
        return Err(format!("could not create {name}: {}", io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_handle(h) })
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let out =
        Command::new("schtasks").args(args).stdin(Stdio::null()).output().map_err(|e| format!("schtasks: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("schtasks {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()))
}

/// Names of the pipes the helper reads from and writes to.
pub fn pipe_names(base: &str) -> (String, String) {
    (format!("{base}-down"), format!("{base}-up"))
}

pub fn run(backend: &str) -> Result<(), String> {
    let base = format!(r"\\.\pipe\sshremote-{}", std::process::id());
    let (down_name, up_name) = pipe_names(&base);
    let sddl = user_only_sddl()?;
    let mut down = pipe(&down_name, PIPE_ACCESS_OUTBOUND, &sddl)?;
    let mut up = pipe(&up_name, PIPE_ACCESS_INBOUND, &sddl)?;

    // The sshremote service starts the helper as SYSTEM, which also works at the sign-in and lock screens;
    // without it a one-shot scheduled task starts it as the logged-on user.
    match crate::service::ask(&format!("spawn {base} {backend}")) {
        Some(Ok(())) => {}
        service => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            // A headless console host keeps a console window from appearing on the remote screen.
            let cmd = format!("conhost.exe --headless \"{}\" --agent --capture {backend} --pipe {base}", exe.display());
            let task = format!("sshremote-agent-{}", std::process::id());
            schtasks(&["/create", "/tn", &task, "/tr", &cmd, "/sc", "once", "/st", "00:00", "/it", "/f"])?;
            let started = schtasks(&["/run", "/tn", &task]);
            let _ = schtasks(&["/delete", "/tn", &task, "/f"]);
            started.map_err(|e| match service {
                Some(Err(s)) => format!("{e}; sshremote service: {s}"),
                _ => format!("{e} (nobody logged on at the console? the sshremote service covers that)"),
            })?;
        }
    }

    let (tx, rx) = mpsc::channel();
    let handles = [&down, &up].map(|f| {
        use std::os::windows::io::AsRawHandle;
        f.as_raw_handle() as usize
    });
    thread::spawn(move || {
        for h in handles {
            // ERROR_PIPE_CONNECTED also means success.
            unsafe { ConnectNamedPipe(h as _, std::ptr::null_mut()) };
        }
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(20))
        .map_err(|_| "the console-session helper did not start (no user logged on at the console?)".to_string())?;

    thread::spawn(move || pump(&mut io::stdin().lock(), &mut down));
    pump(&mut up, &mut io::stdout().lock());
    Ok(())
}

/// Copies with a flush after every read; stdout would otherwise hold bytes until a newline.
fn pump(from: &mut impl Read, to: &mut impl Write) {
    let mut buf = vec![0u8; 1 << 16];
    while let Ok(n @ 1..) = from.read(&mut buf) {
        if to.write_all(&buf[..n]).and_then(|_| to.flush()).is_err() {
            break;
        }
    }
}
