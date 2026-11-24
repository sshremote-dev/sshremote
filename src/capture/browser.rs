//! A web page as a screen (`browser:URL`): an installed Chromium-family browser (Chrome, Edge,
//! Chromium) run with no window, its page streamed as pictures by DevTools' screencast and the
//! viewer's mouse and keys given to it, so a web app (a translator, a chat, a mail) opens in a
//! pane as a remote screen does, with no window of its own on the desktop. Each site keeps its own
//! profile (and so its sign-ins) under the settings folder.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Capture, Image, Input};
use crate::xk;

/// Where DevTools' messages go: the pipe the browser reads (`--remote-debugging-pipe`).
type Out = Arc<Mutex<Box<dyn Write + Send>>>;

/// What the reader thread shares: replies awaited, the newest picture (as it came, decoded when
/// taken), why it ended, and a window the page opened that is to be shown, or one that closed.
struct Shared {
    replies: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
    frame: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
    popup: Mutex<Option<String>>,
    popup_gone: Mutex<bool>,
}

pub struct Browser {
    out: Out,
    ids: Arc<AtomicU64>,
    shared: Arc<Shared>,
    /// The page shown (the site's, or a window it opened, such as a sign-in) and the site's own.
    session: String,
    target: String,
    main: (String, String),
    /// The shown page's session and target, as the reader thread knows them.
    cells: (Arc<Mutex<String>>, Arc<Mutex<String>>),
    /// The picture's size in pixels, and the screen's pixels to a page's (2 on a HiDPI screen).
    size: (usize, usize),
    density: f64,
    proc: Proc,
    /// The browser's last words on its error output, to say why it would not start.
    errors: Arc<Mutex<String>>,
    /// Held while the page is open: no other pane runs a browser on this profile.
    _lock: std::fs::File,
    /// What is held now: the pointer, its buttons (DevTools' mask), the modifier keys (its bits).
    at: (f64, f64),
    buttons: u32,
    mods: u32,
    /// The last press, to tell a double click: when, which button, where, and how many so far.
    last_press: Option<(Instant, u8, (f64, f64), u32)>,
}

/// The browser to run: SSHREMOTE_BROWSER, else Chrome, Edge or Chromium where they install.
fn find_browser() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SSHREMOTE_BROWSER").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    let mut list: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for base in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(b) = std::env::var_os(base).map(PathBuf::from) {
                list.push(b.join(r"Google\Chrome\Application\chrome.exe"));
                list.push(b.join(r"Microsoft\Edge\Application\msedge.exe"));
                list.push(b.join(r"Chromium\Application\chrome.exe"));
            }
        }
    } else {
        let path = std::env::var_os("PATH").unwrap_or_default();
        for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "microsoft-edge-stable"] {
            for dir in std::env::split_paths(&path) {
                list.push(dir.join(name));
            }
            list.push(PathBuf::from("/snap/bin").join(name));
        }
    }
    list.into_iter().find(|p| p.is_file())
}

/// The snap a browser runs in (Ubuntu's Chromium), whose confinement keeps it out of hidden
/// folders such as ~/.config: its profiles go in the snap's own folder instead.
fn snap_of(exe: &Path) -> Option<String> {
    let name = exe.file_name()?.to_string_lossy().into_owned();
    if exe.starts_with("/snap/bin") {
        return Some(name);
    }
    // /usr/bin/chromium-browser on Ubuntu is a script that runs the snap.
    let text = std::fs::read(exe).ok().filter(|t| t.len() < 64 << 10)?;
    String::from_utf8_lossy(&text).contains("/snap/bin/chromium").then(|| "chromium".to_string())
}

/// A site's profile (its sign-ins and settings, apart from the user's own browser), locked by
/// this page while it is open. A second pane of a site already open gets a profile of its own
/// beside the first, so two browsers never share one.
fn profile(url: &str, snap: Option<&str>) -> Result<(PathBuf, std::fs::File), String> {
    let host = url.split_once("://").map_or("page", |(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""));
    let host = host.rsplit('@').next().unwrap_or(host);
    let mut name: String = host.chars().take(100).map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    if name.trim_matches('.').is_empty() {
        name = "site".into();
    }
    // Windows' device names are no folder names (con, nul, com1...).
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    if ["con", "prn", "aux", "nul"].contains(&stem.as_str()) || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit()) {
        name.insert(0, '_');
    }
    let base = match snap {
        Some(s) => std::env::var_os("HOME").map(PathBuf::from).ok_or("no home folder")?.join("snap").join(s).join("common").join("sshremote-browser"),
        None => crate::hosts::config_dir().ok_or("no settings folder")?.join("browser"),
    };
    for n in 1..=8 {
        let dir = if n == 1 { base.join(&name) } else { base.join(format!("{name}~{n}")) };
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // Its sign-ins are the user's alone.
        #[cfg(unix)]
        for d in [&base, &dir] {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("sshremote.lock")).map_err(|e| e.to_string())?;
        if lock.try_lock().is_ok() {
            return Ok((dir, lock));
        }
    }
    Err("this site is open in too many panes".into())
}

/// One DevTools message: JSON, then a NUL.
fn send_msg(out: &Out, text: &str) -> std::io::Result<()> {
    let mut w = out.lock().unwrap_or_else(|e| e.into_inner());
    w.write_all(text.as_bytes())?;
    w.write_all(&[0])?;
    w.flush()
}

/// The next whole DevTools message.
fn read_msg(r: &mut impl BufRead) -> std::io::Result<String> {
    let mut buf = Vec::new();
    r.take(64 << 20).read_until(0, &mut buf)?;
    match buf.pop() {
        Some(0) => Ok(String::from_utf8_lossy(&buf).into_owned()),
        None => Err(std::io::Error::other("the browser closed DevTools")),
        Some(_) => Err(std::io::Error::other("an oversized or cut DevTools message")),
    }
}

/// The browser's process, ended with the page.
enum Proc {
    #[cfg(unix)]
    Child(std::process::Child),
    #[cfg(windows)]
    Handle(std::os::windows::io::OwnedHandle),
}

impl Proc {
    fn ended(&mut self) -> bool {
        match self {
            #[cfg(unix)]
            Proc::Child(c) => c.try_wait().ok().flatten().is_some(),
            #[cfg(windows)]
            Proc::Handle(h) => {
                use std::os::windows::io::AsRawHandle;
                unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(h.as_raw_handle(), 0) == 0 }
            }
        }
    }

    fn kill(&mut self) {
        match self {
            #[cfg(unix)]
            Proc::Child(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            #[cfg(windows)]
            Proc::Handle(h) => {
                use std::os::windows::io::AsRawHandle;
                unsafe { windows_sys::Win32::System::Threading::TerminateProcess(h.as_raw_handle(), 1) };
            }
        }
    }
}

/// The browser started, its DevTools on two pipes only this process holds: no port that
/// another user or program on this computer could reach.
struct Started {
    to: Box<dyn Write + Send>,
    from: Box<dyn Read + Send>,
    proc: Proc,
    errors: Arc<Mutex<String>>,
}

/// Keeps the last of what `r` says (the browser's error output).
#[cfg_attr(windows, allow(dead_code))]
fn keep_tail(r: Option<impl Read + Send + 'static>) -> Arc<Mutex<String>> {
    let tail = Arc::new(Mutex::new(String::new()));
    if let Some(mut r) = r {
        let t = tail.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 4096];
            while let Ok(n @ 1..) = r.read(&mut b) {
                let mut s = t.lock().unwrap_or_else(|e| e.into_inner());
                s.push_str(&String::from_utf8_lossy(&b[..n]));
                if s.len() > 4000 {
                    let cut = s.ceil_char_boundary(s.len() - 2000);
                    s.drain(..cut);
                }
            }
        });
    }
    tail
}

/// The browser on descriptors 3 (what it reads) and 4 (what it writes), ended by SIGTERM (which
/// it takes as a close, keeping its sign-ins) when this thread or process ends.
#[cfg(unix)]
fn spawn(exe: &Path, args: &[String]) -> std::io::Result<Started> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let pipe = || -> std::io::Result<(OwnedFd, OwnedFd)> {
        let mut p = [0; 2];
        #[cfg(target_os = "linux")]
        let made = unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) };
        // No pipe2 there (macOS): closed on exec once made.
        #[cfg(not(target_os = "linux"))]
        let made = unsafe { libc::pipe(p.as_mut_ptr()) };
        if made != 0 {
            return Err(std::io::Error::last_os_error());
        }
        #[cfg(not(target_os = "linux"))]
        for fd in p {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        Ok(unsafe { (OwnedFd::from_raw_fd(p[0]), OwnedFd::from_raw_fd(p[1])) })
    };
    // Its ends at 10 and up, so that moving them to 3 and 4 overwrites neither.
    let high = |fd: OwnedFd| -> std::io::Result<OwnedFd> {
        let n = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
        if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(unsafe { OwnedFd::from_raw_fd(n) }) }
    };
    let (its_in, ours_out) = pipe()?;
    let (ours_in, its_out) = pipe()?;
    let (r, w) = (high(its_in)?, high(its_out)?);
    let (rfd, wfd) = (r.as_raw_fd(), w.as_raw_fd());
    #[cfg(target_os = "linux")]
    let parent = unsafe { libc::getpid() };
    let mut c = std::process::Command::new(exe);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    unsafe {
        c.pre_exec(move || {
            if libc::dup2(rfd, 3) < 0 || libc::dup2(wfd, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
            }
            Ok(())
        })
    };
    let mut child = c.spawn()?;
    drop((r, w));
    let errors = keep_tail(child.stderr.take());
    Ok(Started { to: Box::new(std::fs::File::from(ours_out)), from: Box::new(std::fs::File::from(ours_in)), proc: Proc::Child(child), errors })
}

/// The browser with its C runtime's descriptors 3 and 4 on two pipes (passed as the runtime
/// reads them at start, as Node does), and no other handle of this process; ended with it.
#[cfg(windows)]
fn spawn(exe: &Path, args: &[String]) -> std::io::Result<Started> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    };
    let last = std::io::Error::last_os_error;
    let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: std::ptr::null_mut(), bInheritHandle: 1 };
    let (mut its_in, mut ours_out, mut ours_in, mut its_out): (HANDLE, HANDLE, HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
    unsafe {
        if CreatePipe(&mut its_in, &mut ours_out, &sa, 0) == 0 {
            return Err(last());
        }
        if CreatePipe(&mut ours_in, &mut its_out, &sa, 0) == 0 {
            let e = last();
            CloseHandle(its_in);
            CloseHandle(ours_out);
            return Err(e);
        }
        SetHandleInformation(ours_out, HANDLE_FLAG_INHERIT, 0);
        SetHandleInformation(ours_in, HANDLE_FLAG_INHERIT, 0);
    }
    let nul = unsafe { CreateFileW(crate::sys::wide("NUL").as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, std::ptr::null_mut()) };
    // The runtime's inherited descriptors: none for 0 to 2, then 3 and 4 open on pipes.
    let mut crt: Vec<u8> = 5i32.to_le_bytes().to_vec();
    crt.extend([0, 0, 0, 0x09, 0x09]);
    for h in [INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE, its_in, its_out] {
        crt.extend((h as usize).to_le_bytes());
    }
    let handles: [HANDLE; 3] = [its_in, its_out, nul];
    let mut line = crate::sys::wide(&std::iter::once(exe.to_string_lossy().into_owned()).chain(args.iter().cloned()).map(|a| win_quote(&a)).collect::<Vec<_>>().join(" "));
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        let mut size = 0;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut attrs = vec![0u8; size];
        let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        let mut ok = InitializeProcThreadAttributeList(list, 1, 0, &mut size) != 0
            && UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, handles.as_ptr() as _, size_of_val(&handles), std::ptr::null_mut(), std::ptr::null()) != 0;
        if ok {
            let mut si: STARTUPINFOEXW = std::mem::zeroed();
            si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            (si.StartupInfo.hStdInput, si.StartupInfo.hStdOutput, si.StartupInfo.hStdError) = (nul, nul, nul);
            si.StartupInfo.cbReserved2 = crt.len() as u16;
            si.StartupInfo.lpReserved2 = crt.as_mut_ptr();
            si.lpAttributeList = list;
            ok = CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(),
                std::ptr::null(),
                &si as *const STARTUPINFOEXW as *const _,
                &mut pi,
            ) != 0;
        }
        let e = last();
        DeleteProcThreadAttributeList(list);
        for h in [its_in, its_out, nul] {
            CloseHandle(h);
        }
        if !ok {
            CloseHandle(ours_out);
            CloseHandle(ours_in);
            return Err(e);
        }
        CloseHandle(pi.hThread);
        ok
    };
    debug_assert!(ok);
    crate::sys::tie_handle(pi.hProcess);
    let (to, from) = unsafe { (std::fs::File::from_raw_handle(ours_out as _), std::fs::File::from_raw_handle(ours_in as _)) };
    Ok(Started { to: Box::new(to), from: Box::new(from), proc: Proc::Handle(unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) }), errors: Arc::new(Mutex::new(String::new())) })
}

/// One argument on a Windows command line, as programs split it again.
#[cfg_attr(not(windows), allow(dead_code))]
fn win_quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// A JPEG frame as the screen's pixels: BGRx rows.
fn decode(data: &str) -> Option<Image> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let rgba = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let mut out = rgba.into_raw();
    for px in out.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(Image { w, h, stride: w * 4, data: out })
}

impl Browser {
    pub fn open(url: &str) -> Result<Browser, String> {
        // An address with a scheme (https:, http:, data:, file:) as it is; a bare name is a site.
        let scheme = url.split_once(':').is_some_and(|(s, rest)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()) && !rest.starts_with(|c: char| c.is_ascii_digit()));
        let url = if scheme { url.to_string() } else { format!("https://{url}") };
        let exe = find_browser().ok_or("this needs Chrome, Edge or Chromium installed (or SSHREMOTE_BROWSER naming one)")?;
        let (dir, lock) = profile(&url, snap_of(&exe).as_deref())?;
        let mut args: Vec<String> = vec![
            "--headless=new".into(),
            "--remote-debugging-pipe".into(),
            format!("--user-data-dir={}", dir.display()),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-background-networking".into(),
            "--window-size=1280,800".into(),
        ];
        // Chrome will not run its sandbox as root.
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 {
            args.push("--no-sandbox".into());
        }
        args.push("about:blank".into());
        let started = spawn(&exe, &args).map_err(|e| format!("the browser did not start: {e}"))?;
        let Started { to, from, proc, errors } = started;
        let out: Out = Arc::new(Mutex::new(to));
        let shared = Arc::new(Shared { replies: Mutex::new(HashMap::new()), frame: Mutex::new(None), error: Mutex::new(None), popup: Mutex::new(None), popup_gone: Mutex::new(false) });
        let ids = Arc::new(AtomicU64::new(1));
        let cells = (Arc::new(Mutex::new(String::new())), Arc::new(Mutex::new(String::new())));
        let main_target = Arc::new(Mutex::new(String::new()));
        {
            let (shared, out, ids, cells, main_target) = (shared.clone(), out.clone(), ids.clone(), cells.clone(), main_target.clone());
            let mut reader = BufReader::with_capacity(1 << 16, from);
            std::thread::spawn(move || {
                let r = read_loop(&mut reader, &out, &shared, &ids, &cells, &main_target);
                let why = r.err().map_or_else(|| "the page closed".to_string(), |e| e.to_string());
                shared.error.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert(why);
                // Whoever waits for a reply hears the end too.
                shared.replies.lock().unwrap_or_else(|e| e.into_inner()).clear();
            });
        }
        let mut b = Browser {
            out,
            ids,
            shared,
            session: String::new(),
            target: String::new(),
            main: (String::new(), String::new()),
            cells,
            size: (1280, 800),
            density: 1.0,
            proc,
            errors,
            _lock: lock,
            at: (0.0, 0.0),
            buttons: 0,
            mods: 0,
            last_press: None,
        };
        let why = |b: &Browser, e: String| -> String {
            let tail = b.errors.lock().unwrap_or_else(|e| e.into_inner()).clone();
            match tail.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
                Some(l) => format!("{e} (the browser said: {})", l.chars().take(200).collect::<String>()),
                None => e,
            }
        };
        let created = b.call("Target.createTarget", json!({ "url": url }), false).map_err(|e| why(&b, e))?;
        let target = created["targetId"].as_str().unwrap_or_default().to_string();
        *main_target.lock().unwrap_or_else(|e| e.into_inner()) = target.clone();
        let _ = b.call("Target.setDiscoverTargets", json!({ "discover": true }), false);
        b.show(&target).map_err(|e| why(&b, e))?;
        b.main = (b.session.clone(), b.target.clone());
        Ok(b)
    }

    /// Shows `target` (the site's page, or a window it opened): its pictures, and the input goes
    /// to it.
    fn show(&mut self, target: &str) -> Result<(), String> {
        self.session = if target == self.main.1 {
            self.main.0.clone()
        } else {
            let attached = self.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), false)?;
            attached["sessionId"].as_str().unwrap_or_default().to_string()
        };
        self.target = target.to_string();
        *self.cells.0.lock().unwrap_or_else(|e| e.into_inner()) = self.session.clone();
        *self.cells.1.lock().unwrap_or_else(|e| e.into_inner()) = self.target.clone();
        self.call("Page.enable", json!({}), true)?;
        let (w, h) = self.size;
        self.resize(w, h)
    }

    /// A command, waited for: its result, or the browser's error.
    fn call(&self, method: &str, params: Value, on_page: bool) -> Result<Value, String> {
        let id = self.ids.fetch_add(1, Ordering::AcqRel);
        let (tx, rx) = mpsc::channel();
        {
            // Not waited for once the reader has ended (it empties the list as it goes).
            let mut replies = self.shared.replies.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(e) = self.ended() {
                return Err(e);
            }
            replies.insert(id, tx);
        }
        self.post(id, method, params, on_page).map_err(|e| e.to_string())?;
        let v = rx.recv_timeout(Duration::from_secs(15)).map_err(|_| self.ended().unwrap_or_else(|| format!("the browser did not answer {method}")))?;
        match v.get("error") {
            Some(e) => Err(e["message"].as_str().unwrap_or("the browser refused it").to_string()),
            None => Ok(v["result"].clone()),
        }
    }

    /// A command not waited for (input, as it comes).
    fn tell(&self, method: &str, params: Value) -> Result<(), String> {
        let id = self.ids.fetch_add(1, Ordering::AcqRel);
        self.post(id, method, params, true).map_err(|e| e.to_string())
    }

    fn post(&self, id: u64, method: &str, params: Value, on_page: bool) -> std::io::Result<()> {
        let mut m = json!({ "id": id, "method": method, "params": params });
        if on_page {
            m["sessionId"] = json!(self.session);
        }
        if std::env::var_os("SSHREMOTE_CDP_DEBUG").is_some() {
            eprintln!("cdp> {}", m.to_string().chars().take(300).collect::<String>());
        }
        send_msg(&self.out, &m.to_string())
    }

    /// A browser-wide command not waited for.
    fn tell_browser(&self, method: &str) -> Result<(), String> {
        let id = self.ids.fetch_add(1, Ordering::AcqRel);
        self.post(id, method, json!({}), false).map_err(|e| e.to_string())
    }

    fn ended(&self) -> Option<String> {
        self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The page laid out at this size (in the screen's pixels; its own are `density` of them), and
    /// its pictures sent at it.
    fn resize(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.size = (w, h);
        let (cw, ch) = ((w as f64 / self.density).round() as u32, (h as f64 / self.density).round() as u32);
        self.call("Emulation.setDeviceMetricsOverride", json!({ "width": cw, "height": ch, "deviceScaleFactor": self.density, "mobile": false }), true)?;
        let _ = self.call("Page.stopScreencast", json!({}), true);
        self.call("Page.startScreencast", json!({ "format": "jpeg", "quality": 92, "maxWidth": w, "maxHeight": h, "everyNthFrame": 1 }), true)?;
        Ok(())
    }

    fn mouse(&self, kind: &str, button: &str, clicks: u32) -> Result<(), String> {
        self.tell(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": self.at.0 / self.density, "y": self.at.1 / self.density, "button": button, "buttons": self.buttons, "clickCount": clicks, "modifiers": self.mods }),
        )
    }

    /// A browser's own shortcuts, which a page does not get: Alt+Left and Alt+Right go back and
    /// forward, F5 reloads. Whether it was one.
    fn shortcut(&self, keysym: u32, mods: u32) -> Result<bool, String> {
        let script = match (keysym, mods) {
            (xk::LEFT, 1) => "history.back()",
            (xk::RIGHT, 1) => "history.forward()",
            (k, 0) if k == xk::F1 + 4 => {
                self.tell("Page.reload", json!({}))?;
                return Ok(true);
            }
            _ => return Ok(false),
        };
        self.tell("Runtime.evaluate", json!({ "expression": script }))?;
        Ok(true)
    }

    /// A key down or up: a character's (typed, or with Ctrl a shortcut), or a named key's.
    fn key(&self, keysym: u32, down: bool, mods: u32) -> Result<(), String> {
        if down && self.shortcut(keysym, mods)? {
            return Ok(());
        }
        if !down && matches!((keysym, mods), (xk::LEFT | xk::RIGHT, 1)) || (!down && keysym == xk::F1 + 4 && mods == 0) {
            return Ok(());
        }
        // A character typed whole (as this keyboard's layout or an input method made it): text.
        if xk::is_unicode(keysym) {
            let Some(c) = char::from_u32(keysym & 0x00ff_ffff).filter(|_| down) else { return Ok(()) };
            return self.tell("Input.insertText", json!({ "text": c.to_string() }));
        }
        let ch = if (0x20..0x100).contains(&keysym) { char::from_u32(keysym) } else { None };
        let kind = if down { "keyDown" } else { "keyUp" };
        if let Some(c) = ch {
            // The viewer sends keys as they are on the keyboard; Shift makes the character, as a
            // US layout does (an input method's characters come whole).
            let c = if mods & 8 != 0 && mods & (1 | 2 | 4) == 0 { shifted(c) } else { c };
            let upper = c.to_ascii_uppercase();
            let vk = if upper.is_ascii_alphanumeric() { upper as u32 } else if c == ' ' { 32 } else { 0 };
            let code = if upper.is_ascii_alphabetic() { format!("Key{upper}") } else if c.is_ascii_digit() { format!("Digit{c}") } else if c == ' ' { "Space".into() } else { String::new() };
            // With Ctrl, Alt or Meta held a letter is a shortcut, not text.
            let typing = mods & (1 | 2 | 4) == 0;
            // Pasting brings this computer's clipboard into the page (the page's own is apart).
            if down && mods & 2 != 0 && mods & 1 == 0 && upper == 'V' {
                if let Some(text) = arboard::Clipboard::new().ok().and_then(|mut b| b.get_text().ok()).filter(|t| !t.is_empty()) {
                    return self.tell("Input.insertText", json!({ "text": text }));
                }
                return Ok(());
            }
            let mut p = json!({ "type": kind, "key": c.to_string(), "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk, "modifiers": mods });
            if down && typing {
                p["text"] = json!(c.to_string());
                p["unmodifiedText"] = json!(c.to_string());
            } else if down {
                p["type"] = json!("rawKeyDown");
                // Editing shortcuts, as the browser's own menus run them on every system.
                let command = match (upper, mods & 8 != 0) {
                    ('A', false) => Some("selectAll"),
                    ('C', false) => Some("copy"),
                    ('X', false) => Some("cut"),
                    ('Z', false) => Some("undo"),
                    ('Z', true) | ('Y', false) => Some("redo"),
                    _ => None,
                };
                if mods & 2 != 0
                    && let Some(cmd) = command
                {
                    p["commands"] = json!([cmd]);
                    // What is copied goes on this computer's clipboard too.
                    if matches!(cmd, "copy" | "cut")
                        && let Ok(v) = self.call(
                            "Runtime.evaluate",
                            json!({ "expression": "(() => { const e = document.activeElement; if (e && (e.tagName === 'INPUT' || e.tagName === 'TEXTAREA')) return e.value.substring(e.selectionStart, e.selectionEnd); return String(getSelection()); })()", "returnByValue": true }),
                            true,
                        )
                        && let Some(text) = v["result"]["value"].as_str().filter(|t| !t.is_empty())
                        && let Ok(mut b) = arboard::Clipboard::new()
                    {
                        let _ = b.set_text(text.to_string());
                    }
                }
            }
            return self.tell("Input.dispatchKeyEvent", p);
        }
        let (key, vk, text): (&str, u32, &str) = match keysym {
            xk::BACKSPACE => ("Backspace", 8, ""),
            xk::TAB => ("Tab", 9, ""),
            xk::RETURN => ("Enter", 13, "\r"),
            xk::ESCAPE => ("Escape", 27, ""),
            xk::DELETE => ("Delete", 46, ""),
            xk::INSERT => ("Insert", 45, ""),
            xk::HOME => ("Home", 36, ""),
            xk::END => ("End", 35, ""),
            xk::PAGE_UP => ("PageUp", 33, ""),
            xk::PAGE_DOWN => ("PageDown", 34, ""),
            xk::LEFT => ("ArrowLeft", 37, ""),
            xk::UP => ("ArrowUp", 38, ""),
            xk::RIGHT => ("ArrowRight", 39, ""),
            xk::DOWN => ("ArrowDown", 40, ""),
            k if (xk::F1..xk::F1 + 12).contains(&k) => {
                let n = k - xk::F1 + 1;
                let name = format!("F{n}");
                return self.tell("Input.dispatchKeyEvent", json!({ "type": if down { "rawKeyDown" } else { "keyUp" }, "key": name, "code": name, "windowsVirtualKeyCode": 111 + n, "modifiers": mods }));
            }
            _ => return Ok(()),
        };
        let mut p = json!({ "type": if down { "rawKeyDown" } else { "keyUp" }, "key": key, "code": key, "windowsVirtualKeyCode": vk, "modifiers": mods });
        if down && !text.is_empty() && mods & (2 | 4) == 0 {
            p["type"] = json!("keyDown");
            p["text"] = json!(text);
        }
        self.tell("Input.dispatchKeyEvent", p)
    }
}

/// A key's character with Shift held, on a US keyboard.
fn shifted(c: char) -> char {
    const PAIRS: &[(char, char)] = &[
        ('1', '!'), ('2', '@'), ('3', '#'), ('4', '$'), ('5', '%'), ('6', '^'), ('7', '&'), ('8', '*'), ('9', '('), ('0', ')'),
        ('-', '_'), ('=', '+'), ('[', '{'), (']', '}'), ('\\', '|'), (';', ':'), ('\'', '"'), (',', '<'), ('.', '>'), ('/', '?'), ('`', '~'),
    ];
    if c.is_ascii_lowercase() {
        return c.to_ascii_uppercase();
    }
    PAIRS.iter().find(|(a, _)| *a == c).map_or(c, |(_, b)| *b)
}

/// DevTools' bit for a modifier key, and its name there.
fn modifier(keysym: u32) -> Option<(u32, &'static str, u32)> {
    Some(match keysym {
        xk::ALT_L | xk::ALT_R => (1, "Alt", 18),
        xk::CONTROL_L | xk::CONTROL_R => (2, "Control", 17),
        xk::SUPER_L | xk::SUPER_R => (4, "Meta", 91),
        xk::SHIFT_L | xk::SHIFT_R => (8, "Shift", 16),
        _ => return None,
    })
}

/// The viewer's modifier bits (`proto::MOD_*`) as DevTools' (Alt 1, Ctrl 2, Meta 4, Shift 8).
fn devtools_mods(m: u8) -> u32 {
    use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
    (if m & MOD_ALT != 0 { 1 } else { 0 }) | (if m & MOD_CTRL != 0 { 2 } else { 0 }) | (if m & MOD_SUPER != 0 { 4 } else { 0 }) | (if m & MOD_SHIFT != 0 { 8 } else { 0 })
}

/// DevTools' messages, as they come: replies to whoever waits, frames of the page shown kept (and
/// acknowledged, so the next is sent), and a window the site's page opens (a sign-in, a link to a
/// new tab) told, to be shown in its place until it closes.
fn read_loop(r: &mut impl BufRead, out: &Out, shared: &Shared, ids: &AtomicU64, cells: &(Arc<Mutex<String>>, Arc<Mutex<String>>), main: &Mutex<String>) -> std::io::Result<()> {
    let mut opened: Vec<String> = Vec::new();
    loop {
        let text = read_msg(r)?;
        if std::env::var_os("SSHREMOTE_CDP_DEBUG").is_some() {
            eprintln!("cdp< {}", text.chars().take(300).collect::<String>());
        }
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            if let Some(tx) = shared.replies.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
                let _ = tx.send(v);
            }
            continue;
        }
        let shown = cells.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mine = main.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let p = &v["params"];
        match v["method"].as_str().unwrap_or("") {
            "Page.screencastFrame" if v["sessionId"].as_str() == Some(shown.as_str()) => {
                let ack = json!({ "id": ids.fetch_add(1, Ordering::AcqRel), "method": "Page.screencastFrameAck", "params": { "sessionId": p["sessionId"] }, "sessionId": shown });
                send_msg(out, &ack.to_string())?;
                if let Some(data) = p["data"].as_str() {
                    *shared.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(data.to_string());
                }
            }
            "Target.targetCreated" => {
                let info = &p["targetInfo"];
                let id = info["targetId"].as_str().unwrap_or("");
                if info["type"] == "page" && info["openerId"].as_str() == Some(mine.as_str()) && id != mine && !opened.iter().any(|o| o == id) {
                    opened.push(id.to_string());
                    *shared.popup.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
                }
            }
            "Target.targetDestroyed" | "Target.targetCrashed" => {
                let id = p["targetId"].as_str().unwrap_or("");
                if id == mine {
                    return Err(std::io::Error::other(if v["method"] == "Target.targetCrashed" { "the page crashed" } else { "the page was closed" }));
                }
                if let Some(at) = opened.iter().position(|o| o == id) {
                    opened.remove(at);
                    let mut popup = shared.popup.lock().unwrap_or_else(|e| e.into_inner());
                    if popup.as_deref() == Some(id) {
                        *popup = None;
                    }
                    if id == cells.1.lock().unwrap_or_else(|e| e.into_inner()).as_str() {
                        *shared.popup_gone.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    }
                }
            }
            // The site's page let go of while shown (a window it opened going is no end).
            "Inspector.detached" if v["sessionId"].as_str().is_none_or(|s| s == shown) && *cells.1.lock().unwrap_or_else(|e| e.into_inner()) == mine => {
                return Err(std::io::Error::other("the browser let go of the page"));
            }
            _ => {}
        }
    }
}

impl Capture for Browser {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.size
    }

    fn resizable(&self) -> bool {
        true
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        let (w, h) = (w.clamp(200, 3840), h.clamp(150, 2160));
        if (w, h) != self.size {
            self.resize(w, h)?;
        }
        Ok(())
    }

    fn set_density(&mut self, d: f64) {
        let d = if d.is_finite() { d.clamp(0.5, 4.0) } else { 1.0 };
        if (d - self.density).abs() > 0.01 {
            self.density = d;
            let (w, h) = self.size;
            let _ = self.resize(w, h);
        }
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        if let Some(e) = self.ended() {
            return Err(e);
        }
        // A window the page opened shows in its place; when it closes, the page again.
        let popup = self.shared.popup.lock().unwrap_or_else(|e| e.into_inner()).take();
        let gone = std::mem::take(&mut *self.shared.popup_gone.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(id) = popup {
            self.show(&id)?;
        } else if gone && self.target != self.main.1 {
            let main = self.main.1.clone();
            self.show(&main)?;
        }
        let data = self.shared.frame.lock().unwrap_or_else(|e| e.into_inner()).take();
        Ok(data.and_then(|d| decode(&d)))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        if let Some(e) = self.ended() {
            return Err(e);
        }
        match ev {
            Input::Move { x, y } => {
                self.at = (x, y);
                self.mouse("mouseMoved", "none", 0)
            }
            Input::Button { button, pressed } => {
                let (name, bit) = match button {
                    1 => ("left", 1),
                    2 => ("middle", 4),
                    3 => ("right", 2),
                    8 => ("back", 8),
                    9 => ("forward", 16),
                    _ => return Ok(()),
                };
                if pressed {
                    let now = Instant::now();
                    let clicks = match self.last_press {
                        Some((t, b, at, n)) if b == button && now - t < Duration::from_millis(500) && (at.0 - self.at.0).abs() < 5.0 && (at.1 - self.at.1).abs() < 5.0 => n + 1,
                        _ => 1,
                    };
                    self.last_press = Some((now, button, self.at, clicks));
                    self.buttons |= bit;
                    self.mouse("mousePressed", name, clicks)
                } else {
                    self.buttons &= !bit;
                    let clicks = self.last_press.map_or(1, |p| p.3);
                    self.mouse("mouseReleased", name, clicks)
                }
            }
            Input::Scroll { steps } | Input::HScroll { steps } => {
                let horizontal = matches!(ev, Input::HScroll { .. });
                let d = steps as f64 * 100.0;
                self.tell(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseWheel", "x": self.at.0 / self.density, "y": self.at.1 / self.density, "deltaX": if horizontal { d } else { 0.0 }, "deltaY": if horizontal { 0.0 } else { d }, "modifiers": self.mods }),
                )
            }
            Input::KeyState { keysym, down } => {
                if let Some((bit, name, vk)) = modifier(keysym) {
                    if down { self.mods |= bit } else { self.mods &= !bit }
                    return self.tell("Input.dispatchKeyEvent", json!({ "type": if down { "rawKeyDown" } else { "keyUp" }, "key": name, "code": name, "windowsVirtualKeyCode": vk, "modifiers": self.mods }));
                }
                self.key(keysym, down, self.mods)
            }
            Input::Key { keysym, mods } => {
                let mods = self.mods | devtools_mods(mods);
                self.key(keysym, true, mods)?;
                self.key(keysym, false, mods)
            }
            Input::SecureAttention => Ok(()),
            // A page has no pointer of its own to move by so much.
            Input::MoveBy { .. } => Ok(()),
        }
    }
}

impl Browser {
    #[cfg(test)]
    fn proc_id(&self) -> u32 {
        match &self.proc {
            #[cfg(unix)]
            Proc::Child(c) => c.id(),
            #[cfg(windows)]
            Proc::Handle(h) => {
                use std::os::windows::io::AsRawHandle;
                unsafe { windows_sys::Win32::System::Threading::GetProcessId(h.as_raw_handle()) }
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // Closed as a window is, so its sign-ins are written out; ended if it does not.
        let _ = self.tell_browser("Browser.close");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !self.proc.ended() {
            std::thread::sleep(Duration::from_millis(50));
        }
        self.proc.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page in a real browser (Chrome, Edge or Chromium installed): it draws, takes typing and
    /// follows a resize. cargo test -- --ignored a_web_page_is_a_screen
    #[test]
    #[ignore]
    fn a_web_page_is_a_screen() {
        let page = "data:text/html,<input id=i autofocus style='font-size:40px'><script>document.title='ready'</script>";
        let mut b = Browser::open(page).expect("a browser");
        let started = Instant::now();
        let mut got = None;
        while got.is_none() && started.elapsed() < Duration::from_secs(15) {
            got = b.grab().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
        let img = got.expect("a picture of the page");
        assert_eq!((img.w, img.h), (1280, 800));
        // Letters, and what an input method gives (Korean) as characters.
        for c in "hi 한글".chars() {
            b.input(Input::Key { keysym: xk::of_char(c), mods: 0 }).unwrap();
        }
        // What another layout makes (an AZERTY é, a German ß) comes as text.
        for c in "éß".chars() {
            b.input(Input::Key { keysym: xk::unicode(c as u32), mods: 0 }).unwrap();
        }
        let typed = b.call("Runtime.evaluate", json!({ "expression": "document.getElementById('i').value", "returnByValue": true }), true).unwrap();
        assert_eq!(typed["result"]["value"], "hi 한글éß");
        // Shift held makes capitals and the symbols over the digits.
        b.input(Input::Key { keysym: 'a' as u32, mods: crate::proto::MOD_CTRL }).unwrap();
        b.input(Input::KeyState { keysym: xk::SHIFT_L, down: true }).unwrap();
        for c in "h1".chars() {
            b.input(Input::KeyState { keysym: xk::of_char(c), down: true }).unwrap();
            b.input(Input::KeyState { keysym: xk::of_char(c), down: false }).unwrap();
        }
        b.input(Input::KeyState { keysym: xk::SHIFT_L, down: false }).unwrap();
        let typed = b.call("Runtime.evaluate", json!({ "expression": "document.getElementById('i').value", "returnByValue": true }), true).unwrap();
        assert_eq!(typed["result"]["value"], "H!");
        // Ctrl+A then Backspace: a shortcut, then a named key.
        b.input(Input::Key { keysym: 'a' as u32, mods: crate::proto::MOD_CTRL }).unwrap();
        b.input(Input::Key { keysym: xk::BACKSPACE, mods: 0 }).unwrap();
        let typed = b.call("Runtime.evaluate", json!({ "expression": "document.getElementById('i').value", "returnByValue": true }), true).unwrap();
        assert_eq!(typed["result"]["value"], "");
        b.set_target(800, 600).unwrap();
        let started = Instant::now();
        let mut resized = None;
        while started.elapsed() < Duration::from_secs(10) {
            if let Some(i) = b.grab().unwrap()
                && (i.w, i.h) == (800, 600)
            {
                resized = Some(i);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(resized.is_some(), "a picture at the new size");
        // A tall page scrolls under the wheel, and a click lands where it is.
        b.call("Runtime.evaluate", json!({ "expression": "document.body.style.height='5000px'; document.onclick = e => document.title = 'clicked ' + e.clientX + ' ' + e.clientY;" }), true).unwrap();
        b.input(Input::Move { x: 300.0, y: 300.0 }).unwrap();
        b.input(Input::Scroll { steps: 3 }).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let y = b.call("Runtime.evaluate", json!({ "expression": "window.scrollY", "returnByValue": true }), true).unwrap();
        assert!(y["result"]["value"].as_f64().unwrap_or(0.0) > 100.0, "scrolled down: {y}");
        b.input(Input::Button { button: 1, pressed: true }).unwrap();
        b.input(Input::Button { button: 1, pressed: false }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let t = b.call("Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true }), true).unwrap();
        assert_eq!(t["result"]["value"], "clicked 300 300");
        // On a HiDPI screen the page lays itself out in its own pixels, half as many.
        b.set_density(2.0);
        let w = b.call("Runtime.evaluate", json!({ "expression": "innerWidth + 'x' + devicePixelRatio", "returnByValue": true }), true).unwrap();
        assert_eq!(w["result"]["value"], "400x2");
        b.set_density(1.0);
        // A window the page opens (a sign-in) shows in its place, its opener kept; closed, the page is back.
        let main = b.target.clone();
        b.call("Runtime.evaluate", json!({ "expression": "window.open('data:text/html,<title>popup</title><body style=background:red>', 'w', 'popup'); 1", "userGesture": true }), true).unwrap();
        let started = Instant::now();
        while b.target == main && started.elapsed() < Duration::from_secs(10) {
            let _ = b.grab().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_ne!(b.target, main, "the popup shown");
        let opener = b.call("Runtime.evaluate", json!({ "expression": "!!window.opener", "returnByValue": true }), true).unwrap();
        assert_eq!(opener["result"]["value"], true);
        b.call("Runtime.evaluate", json!({ "expression": "window.close()" }), true).unwrap();
        let started = Instant::now();
        while b.target != main && started.elapsed() < Duration::from_secs(10) {
            let _ = b.grab().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(b.target, main, "the page again");
        // The same site in a second pane runs on a profile of its own.
        let other = Browser::open(page).expect("a second browser");
        assert_ne!(other.proc_id(), b.proc_id());
        drop(other);
    }
}
