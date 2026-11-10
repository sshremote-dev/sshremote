//! Shells in pseudo-terminals (a pty on Unix, a pseudo console on Windows):
//! on the SSH host for the viewer's terminal (`Terms`, driven by the agent),
//! and on this computer for local shells and `ssh` sessions (`Local`).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::mpsc;

use crate::proto::ToClient;

enum Out {
    Data(Vec<u8>),
    Exit(String),
}

struct Shell {
    input: File,
    out: mpsc::Receiver<Out>,
    sys: sys::Handle,
}

/// The shells opened by one viewer.
#[derive(Default)]
pub struct Terms {
    open: HashMap<u32, Shell>,
}

impl Terms {
    /// Starts a shell; returns a message to send if it failed.
    pub fn open(&mut self, id: u32, cols: u16, rows: u16) -> Option<ToClient> {
        match sys::spawn(cols.max(2), rows.max(1), None, None) {
            Ok((input, output, handle)) => {
                // Bounded, so a flood of output waits for the reader instead of filling memory.
                let (tx, rx) = mpsc::sync_channel(64);
                std::thread::spawn(move || read_loop(output, tx));
                self.open.insert(id, Shell { input, out: rx, sys: handle });
                None
            }
            Err(e) => Some(ToClient::TermExit { id, text: format!("cannot start a shell: {e}") }),
        }
    }

    pub fn input(&mut self, id: u32, data: &[u8]) {
        if let Some(s) = self.open.get_mut(&id) {
            let _ = s.input.write_all(data);
            let _ = s.input.flush();
        }
    }

    pub fn resize(&mut self, id: u32, cols: u16, rows: u16) {
        if let Some(s) = self.open.get(&id) {
            s.sys.resize(cols.max(2), rows.max(1));
        }
    }

    pub fn close(&mut self, id: u32) {
        self.open.remove(&id);
    }

    /// Output and exits since the last call.
    pub fn poll(&mut self) -> Vec<ToClient> {
        let mut msgs = Vec::new();
        let mut ended = Vec::new();
        for (&id, s) in &self.open {
            let mut data = Vec::new();
            for o in s.out.try_iter() {
                match o {
                    Out::Data(d) => data.extend_from_slice(&d),
                    Out::Exit(text) => {
                        ended.push(id);
                        if !data.is_empty() {
                            msgs.push(ToClient::TermOutput { id, data: std::mem::take(&mut data) });
                        }
                        let text = s.sys.exit_text().unwrap_or(text);
                        msgs.push(ToClient::TermExit { id, text });
                        break;
                    }
                }
            }
            if !data.is_empty() {
                msgs.push(ToClient::TermOutput { id, data });
            }
        }
        for id in ended {
            self.open.remove(&id);
        }
        msgs
    }
}

fn read_loop(output: File, tx: mpsc::SyncSender<Out>) {
    read_loop_waking(output, tx, Box::new(|| {}));
}

fn read_loop_waking(mut output: File, tx: mpsc::SyncSender<Out>, wake: Box<dyn Fn() + Send>) {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match output.read(&mut buf) {
            Ok(0) | Err(_) => {
                let _ = tx.send(Out::Exit("the shell ended".into()));
                wake();
                return;
            }
            Ok(n) => {
                if tx.send(Out::Data(buf[..n].to_vec())).is_err() {
                    return;
                }
                wake();
            }
        }
    }
}

/// A program in a pseudo-terminal on this computer: a local shell, or `ssh HOST`.
pub struct Local {
    input: File,
    out: mpsc::Receiver<Out>,
    sys: sys::Handle,
}

impl Local {
    /// Starts `argv` (the login shell when empty); `wake` is called whenever output arrives.
    pub fn spawn(argv: &[String], cols: u16, rows: u16, wake: Box<dyn Fn() + Send>) -> Result<Local, String> {
        Self::spawn_in(argv, None, cols, rows, wake)
    }

    /// The same, starting in `dir` (when it is a folder there) rather than the home folder.
    pub fn spawn_in(argv: &[String], dir: Option<&str>, cols: u16, rows: u16, wake: Box<dyn Fn() + Send>) -> Result<Local, String> {
        let dir = dir.filter(|d| std::path::Path::new(d).is_dir());
        let (input, output, sys) = sys::spawn(cols.max(2), rows.max(1), (!argv.is_empty()).then_some(argv), dir)?;
        let (tx, out) = mpsc::sync_channel(64);
        std::thread::spawn(move || read_loop_waking(output, tx, wake));
        Ok(Local { input, out, sys })
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.input.write_all(data);
        let _ = self.input.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.sys.resize(cols.max(2), rows.max(1));
    }

    /// The folder the program is in, where the system tells it (Linux).
    pub fn cwd(&self) -> Option<String> {
        self.sys.cwd()
    }

    /// Output since the last call, and why the program ended once it has.
    pub fn drain(&mut self) -> (Vec<u8>, Option<String>) {
        let mut data = Vec::new();
        for o in self.out.try_iter() {
            match o {
                Out::Data(d) => data.extend_from_slice(&d),
                Out::Exit(text) => return (data, Some(self.sys.exit_text().unwrap_or(text))),
            }
        }
        (data, None)
    }
}

#[cfg(unix)]
mod sys {
    use std::ffi::CStr;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::Mutex;

    pub struct Handle {
        master: File,
        child: Mutex<Option<Child>>,
    }

    fn winsize(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
    }

    fn os_err(what: &str) -> String {
        format!("{what}: {}", std::io::Error::last_os_error())
    }

    /// The user's shell started as a login shell on a new pseudo-terminal.
    pub fn spawn(cols: u16, rows: u16, argv: Option<&[String]>, dir: Option<&str>) -> Result<(File, File, Handle), String> {
        unsafe {
            let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if m < 0 {
                return Err(os_err("posix_openpt"));
            }
            let master = File::from_raw_fd(m);
            libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
            if libc::grantpt(m) != 0 || libc::unlockpt(m) != 0 {
                return Err(os_err("grantpt"));
            }
            let name = libc::ptsname(m);
            if name.is_null() {
                return Err(os_err("ptsname"));
            }
            let name = CStr::from_ptr(name).to_owned();
            let s = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
            if s < 0 {
                return Err(os_err("open the terminal"));
            }
            let slave = File::from_raw_fd(s);
            let ws = winsize(cols, rows);
            libc::ioctl(m, libc::TIOCSWINSZ, &ws);
            let login = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
            let home: std::ffi::OsString = dir.map_or_else(|| std::env::var_os("HOME").unwrap_or_else(|| "/".into()), Into::into);
            let (shell, args): (String, Vec<String>) = match argv {
                Some([first, rest @ ..]) => (first.clone(), rest.to_vec()),
                _ => (login, vec!["-l".into()]),
            };
            let mut cmd = Command::new(&shell);
            cmd.args(&args)
                .current_dir(home)
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .stdin(slave.try_clone().map_err(|e| e.to_string())?)
                .stdout(slave.try_clone().map_err(|e| e.to_string())?)
                .stderr(slave);
            cmd.pre_exec(|| {
                // A session of its own, with this terminal as the controlling one.
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
            let child = cmd.spawn().map_err(|e| format!("{shell}: {e}"))?;
            let reader = master.try_clone().map_err(|e| e.to_string())?;
            let writer = master.try_clone().map_err(|e| e.to_string())?;
            Ok((writer, reader, Handle { master, child: Mutex::new(Some(child)) }))
        }
    }

    impl Handle {
        pub fn cwd(&self) -> Option<String> {
            let pid = self.child.lock().ok()?.as_ref()?.id();
            std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
        }

        pub fn resize(&self, cols: u16, rows: u16) {
            let ws = winsize(cols, rows);
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
        }

        pub fn exit_text(&self) -> Option<String> {
            // Taken, so the drop does not signal a process id that may be reused.
            let status = self.child.lock().ok()?.take()?.wait().ok()?;
            Some(match status.code() {
                Some(c) => format!("the shell exited with code {c}"),
                None => "the shell was ended by a signal".into(),
            })
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            let Some(mut c) = self.child.lock().ok().and_then(|mut c| c.take()) else { return };
            // The whole session gets the hangup, as when a terminal window closes.
            let group = -(c.id() as i32);
            unsafe { libc::kill(group, libc::SIGHUP) };
            // Reaped on the side; a shell that ignores the hangup is killed after two seconds.
            std::thread::spawn(move || {
                for _ in 0..20 {
                    if matches!(c.try_wait(), Ok(Some(_))) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                unsafe { libc::kill(group, libc::SIGKILL) };
                let _ = c.wait();
            });
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    const PSEUDOCONSOLE_ATTRIBUTE: usize = 0x20016;

    pub struct Handle {
        console: usize,
        process: usize,
    }

    fn os_err(what: &str) -> String {
        format!("{what}: {}", std::io::Error::last_os_error())
    }

    use crate::sys::wide;

    use crate::sys::command_line;

    /// PowerShell (or cmd) attached to a new pseudo console.
    pub fn spawn(cols: u16, rows: u16, argv: Option<&[String]>, dir: Option<&str>) -> Result<(File, File, Handle), String> {
        let user = std::env::var("USERNAME").unwrap_or_default();
        if user.eq_ignore_ascii_case("SYSTEM") || user.ends_with('$') {
            return Err("the agent runs as the Windows service; connect over SSH for a terminal".into());
        }
        unsafe {
            let (mut in_r, mut in_w, mut out_r, mut out_w): (HANDLE, HANDLE, HANDLE, HANDLE) =
                (null_mut(), null_mut(), null_mut(), null_mut());
            if CreatePipe(&mut in_r, &mut in_w, null(), 0) == 0 || CreatePipe(&mut out_r, &mut out_w, null(), 0) == 0 {
                return Err(os_err("CreatePipe"));
            }
            let mut console = std::mem::zeroed();
            let size = COORD { X: cols as i16, Y: rows as i16 };
            let hr = CreatePseudoConsole(size, in_r, out_w, 0, &mut console);
            // The pseudo console holds its own copies of these ends.
            CloseHandle(in_r);
            CloseHandle(out_w);
            if hr != 0 {
                CloseHandle(in_w);
                CloseHandle(out_r);
                return Err(format!("CreatePseudoConsole: {hr:#x}"));
            }
            let mut bytes = 0usize;
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes);
            let mut list = vec![0u8; bytes];
            let attrs = list.as_mut_ptr() as _;
            if InitializeProcThreadAttributeList(attrs, 1, 0, &mut bytes) == 0
                || UpdateProcThreadAttribute(
                    attrs,
                    0,
                    PSEUDOCONSOLE_ATTRIBUTE,
                    console as *const c_void,
                    size_of_val(&console),
                    null_mut(),
                    null(),
                ) == 0
            {
                ClosePseudoConsole(console);
                return Err(os_err("UpdateProcThreadAttribute"));
            }
            let mut si: STARTUPINFOEXW = std::mem::zeroed();
            si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            // Without this the shell can inherit the agent's redirected standard handles.
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
            si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
            si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
            si.lpAttributeList = attrs;
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into());
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let mut started = false;
            let wanted: Vec<String> = match argv {
                Some(a) => vec![command_line(a)],
                None => vec!["powershell.exe -NoLogo".into(), "cmd.exe".into()],
            };
            for cmd in &wanted {
                let mut line = wide(cmd);
                let dir = wide(dir.unwrap_or(&home));
                if CreateProcessW(
                    null(),
                    line.as_mut_ptr(),
                    null(),
                    null(),
                    0,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                    null(),
                    dir.as_ptr(),
                    &si.StartupInfo,
                    &mut pi,
                ) != 0
                {
                    started = true;
                    break;
                }
            }
            DeleteProcThreadAttributeList(attrs);
            if !started {
                ClosePseudoConsole(console);
                CloseHandle(in_w);
                CloseHandle(out_r);
                return Err(os_err("CreateProcess"));
            }
            CloseHandle(pi.hThread);
            let (console, process) = (console as usize, pi.hProcess as usize);
            // The output pipe stays open until the pseudo console is closed, so close it when the shell exits.
            std::thread::spawn(move || {
                if WaitForSingleObject(process as HANDLE, INFINITE) == WAIT_OBJECT_0 {
                    ClosePseudoConsole(console as _);
                }
            });
            Ok((File::from_raw_handle(in_w), File::from_raw_handle(out_r), Handle { console, process }))
        }
    }

    impl Handle {
        /// Windows does not tell another program's folder; the shell's own word is used.
        pub fn cwd(&self) -> Option<String> {
            None
        }

        pub fn resize(&self, cols: u16, rows: u16) {
            unsafe { ResizePseudoConsole(self.console as _, COORD { X: cols as i16, Y: rows as i16 }) };
        }

        pub fn exit_text(&self) -> Option<String> {
            let mut code = 0u32;
            unsafe { GetExitCodeProcess(self.process as HANDLE, &mut code) };
            Some(format!("the shell exited with code {code}"))
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // Ending the shell lets the waiting thread close the pseudo console.
            unsafe {
                TerminateProcess(self.process as HANDLE, 0);
                WaitForSingleObject(self.process as HANDLE, 2000);
                CloseHandle(self.process as HANDLE);
            }
        }
    }
}
