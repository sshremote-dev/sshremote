//! The person signed in at this computer's screen, for an agent the service started as SYSTEM:
//! its shells start as them, with their environment and home, and file access goes through their
//! rights, so a viewer gets what that person has, never SYSTEM.

use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_TIMEOUT};
use windows_sys::Win32::Security::{
    GetTokenInformation, ImpersonateLoggedOnUser, IsWellKnownSid, RevertToSelf, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WinLocalSystemSid,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::GetUserProfileDirectoryW;

/// Whether this process runs as SYSTEM (an agent the service started).
pub fn running_as_system() -> bool {
    static SYSTEM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SYSTEM.get_or_init(|| unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut buf = vec![0u8; 256];
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as _, buf.len() as u32, &mut len) != 0;
        CloseHandle(token);
        ok && IsWellKnownSid((*(buf.as_ptr() as *const TOKEN_USER)).User.Sid, WinLocalSystemSid) != 0
    })
}

/// Who a SYSTEM agent acts as for shells and files.
pub enum Acting {
    /// Not SYSTEM: as itself, which is already a person's.
    Myself,
    /// SYSTEM, with someone signed in at the screen: as them.
    User(User),
    /// SYSTEM, and nobody signed in: nobody's shells or files are handed out.
    Nobody,
}

/// Who a shell or a file operation is done as, now (people sign in and out).
pub fn acting() -> Acting {
    if !running_as_system() {
        return Acting::Myself;
    }
    User::at_console().map_or(Acting::Nobody, Acting::User)
}

/// What a viewer is told when nobody is signed in at a computer whose agent runs as SYSTEM.
pub const NOBODY: &str = "Nobody is signed in at that computer's screen: its terminals and files open as the person signed in there.";

/// The user signed in at the console, by their token.
pub struct User {
    token: HANDLE,
    /// Their profile folder (C:\Users\NAME).
    pub profile: PathBuf,
}

// The token is a kernel handle, usable from any thread.
unsafe impl Send for User {}

impl Drop for User {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.token) };
    }
}

/// The sessions someone is signed in to and using now: the console's, and those by Remote Desktop.
pub fn active_sessions() -> Vec<u32> {
    use windows_sys::Win32::System::RemoteDesktop::{WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW, WTSActive, WTSEnumerateSessionsW, WTSFreeMemory};
    let (mut info, mut count): (*mut WTS_SESSION_INFOW, u32) = (null_mut(), 0);
    if unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut info, &mut count) } == 0 || info.is_null() {
        return Vec::new();
    }
    let list = unsafe { std::slice::from_raw_parts(info, count as usize) };
    let ids = list.iter().filter(|s| s.State == WTSActive && s.SessionId != 0).map(|s| s.SessionId).collect();
    unsafe { WTSFreeMemory(info as _) };
    ids
}

impl User {
    /// The user signed in at the console now; only SYSTEM may ask.
    pub fn at_console() -> Option<User> {
        User::in_session(unsafe { WTSGetActiveConsoleSessionId() })
    }

    /// The user signed in to session `id` (the console's, or one by Remote Desktop).
    pub fn in_session(id: u32) -> Option<User> {
        unsafe {
            let mut token: HANDLE = null_mut();
            if WTSQueryUserToken(id, &mut token) == 0 {
                return None;
            }
            let mut buf = vec![0u16; 260];
            let mut len = buf.len() as u32;
            let profile = if GetUserProfileDirectoryW(token, buf.as_mut_ptr(), &mut len) != 0 {
                PathBuf::from(String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]))
            } else {
                PathBuf::from(r"C:\Users\Default")
            };
            Some(User { token, profile })
        }
    }

    pub fn token(&self) -> HANDLE {
        self.token
    }

    /// Their environment, as Windows makes it for their own programs.
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        unsafe {
            let mut block: *mut core::ffi::c_void = null_mut();
            if CreateEnvironmentBlock(&mut block, self.token, 0) == 0 || block.is_null() {
                return out;
            }
            let mut p = block as *const u16;
            loop {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                if n == 0 {
                    break;
                }
                let entry = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                // "=C:=C:\..." entries name drives' folders; they are not variables.
                if let Some((k, v)) = entry.split_once('=').filter(|(k, _)| !k.is_empty()) {
                    out.push((k.to_string(), v.to_string()));
                }
                p = p.add(n + 1);
            }
            DestroyEnvironmentBlock(block);
        }
        out
    }

    /// Acts as them on this thread until what it returns goes; it keeps their token open
    /// meanwhile.
    pub fn impersonate(self) -> Option<AsUser> {
        (unsafe { ImpersonateLoggedOnUser(self.token) } != 0).then(|| AsUser { _user: self, _thread: std::marker::PhantomData })
    }
}

/// This thread acts as the user while it lives; it stays on the thread it began on.
pub struct AsUser {
    _user: User,
    _thread: std::marker::PhantomData<*const ()>,
}

impl Drop for AsUser {
    fn drop(&mut self) {
        // Back to SYSTEM before the user's token is closed.
        unsafe { RevertToSelf() };
    }
}

/// A program started as the user, on their desktop.
pub struct Process(HANDLE);

unsafe impl Send for Process {}

impl Process {
    pub fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT }
    }

    pub fn kill(&self) {
        unsafe { TerminateProcess(self.0, 1) };
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Starts `command` (a whole command line) as `user` on their desktop with no console, its input
/// and output joined to the pipes returned: what to write to it and what to read from it.
pub fn spawn_piped(user: &User, command: &str) -> Result<(Process, std::fs::File, std::fs::File), String> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;
    let err = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    unsafe {
        // The child's ends are inherited, this process's are not.
        let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: null_mut(), bInheritHandle: 1 };
        let (mut in_r, mut in_w, mut out_r, mut out_w): (HANDLE, HANDLE, HANDLE, HANDLE) = (null_mut(), null_mut(), null_mut(), null_mut());
        if CreatePipe(&mut in_r, &mut in_w, &sa, 0) == 0 {
            return Err(err("CreatePipe"));
        }
        if CreatePipe(&mut out_r, &mut out_w, &sa, 0) == 0 {
            CloseHandle(in_r);
            CloseHandle(in_w);
            return Err(err("CreatePipe"));
        }
        SetHandleInformation(in_w, HANDLE_FLAG_INHERIT, 0);
        SetHandleInformation(out_r, HANDLE_FLAG_INHERIT, 0);
        let mut desktop = crate::sys::wide(r"winsta0\default");
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop.as_mut_ptr();
        si.dwFlags = STARTF_USESTDHANDLES;
        (si.hStdInput, si.hStdOutput, si.hStdError) = (in_r, out_w, out_w);
        let mut env: Vec<u16> = Vec::new();
        for (k, v) in user.environment() {
            env.extend(format!("{k}={v}").encode_utf16());
            env.push(0);
        }
        env.push(0);
        let mut line = crate::sys::wide(command);
        let dir = crate::sys::wide(&user.profile.display().to_string());
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let made = CreateProcessAsUserW(
            user.token,
            std::ptr::null(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            env.as_ptr() as *const core::ffi::c_void,
            dir.as_ptr(),
            &si,
            &mut pi,
        );
        // The child holds its own copies now.
        CloseHandle(in_r);
        CloseHandle(out_w);
        if made == 0 {
            CloseHandle(in_w);
            CloseHandle(out_r);
            return Err(err("CreateProcessAsUserW"));
        }
        CloseHandle(pi.hThread);
        Ok((Process(pi.hProcess), std::fs::File::from_raw_handle(in_w), std::fs::File::from_raw_handle(out_r)))
    }
}
