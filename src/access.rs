//! Remote access by ID: this computer keeps a line open to a relay under a lasting ID, and a
//! viewer anywhere reaches it with that ID and the password, as RustDesk does; for PCs on Wi-Fi
//! or behind NAT with no fixed address. SSH stays the way for one host reached directly.
//!
//! Relay lines (beside the older `agent ID` / `viewer ID`):
//!   agent:  `listen ID TOKEN` → `ok` | `taken`; then the relay sends `call NONCE` for each viewer
//!           and the agent answers with a new connection `accept ID NONCE` → `ok`.
//!   viewer: `viewer ID` → `ok` | `offline` | `busy`; `status ID` → `online` | `offline`.
//! The relay never learns the password: both ends run SPAKE2 through it and seal every byte.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// The relay used when none is set.
pub const DEFAULT_RELAY: &str = "sshremote.dev";

/// What remote access by ID keeps: its ID, the secret that holds the ID at the relay, the
/// password viewers give, the relay, and whether it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub id: String,
    pub token: String,
    pub password: String,
    pub relay: String,
    pub enabled: bool,
}

impl Config {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "id": self.id, "token": self.token, "password": self.password, "relay": self.relay, "enabled": self.enabled })
    }

    fn from_json(v: &serde_json::Value) -> Option<Config> {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        Some(Config {
            id: s("id")?,
            token: s("token")?,
            password: s("password")?,
            relay: s("relay").filter(|r| !r.trim().is_empty()).unwrap_or_else(default_relay),
            enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false),
        })
    }
}

fn default_relay() -> String {
    DEFAULT_RELAY.into()
}

fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("system random source");
    b
}

pub fn new_id() -> String {
    // Nine digits, not starting with 0, to read out and type.
    let b = random(9);
    let mut s: String = b.iter().map(|x| char::from(b'0' + x % 10)).collect();
    if s.starts_with('0') {
        s.replace_range(0..1, "1");
    }
    s
}

/// Eight letters and digits without the ones that are read wrong (0 O 1 l I).
pub fn new_password() -> String {
    const SET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    random(8).iter().map(|x| SET[*x as usize % SET.len()] as char).collect()
}

/// Where the settings are on Unix: the user's settings folder, readable by the user only.
#[cfg(any(not(windows), test))]
pub fn file() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(d) = TEST_DIR.with(|d| d.borrow().clone()) {
        return Some(d.join("remote-access.json"));
    }
    crate::hosts::config_dir().map(|d| d.join("remote-access.json"))
}

#[cfg(test)]
thread_local!(pub static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) });

/// On Windows the settings are a value under HKLM\SOFTWARE\sshremote: only administrators can
/// write there, so a standard user cannot plant settings the service (SYSTEM) would trust.
#[cfg(windows)]
mod machine {
    use crate::sys::wide;
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_SZ, RegGetValueW};

    const KEY: &str = r"SOFTWARE\sshremote";
    const VALUE: &str = "RemoteAccess";

    /// The stored text; `Ok(None)` when there is none yet.
    pub fn read() -> Result<Option<String>, String> {
        let (key, value) = (wide(KEY), wide(VALUE));
        let mut len: u32 = 0;
        let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut len) };
        if r == 2 {
            return Ok(None);
        }
        if r == 5 {
            return Err("only administrators can see them (they hold the password)".into());
        }
        if r != 0 {
            return Err(format!("the remote access settings could not be read (error {r})"));
        }
        let mut buf = vec![0u16; len as usize / 2 + 1];
        let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut len) };
        if r != 0 {
            return Err(format!("the remote access settings could not be read (error {r})"));
        }
        let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Ok(Some(String::from_utf16_lossy(&buf[..n])))
    }

    /// Writes the settings under a key only Administrators and SYSTEM may open: it holds the
    /// password, which no standard user should read.
    pub fn write(text: &str) -> Result<(), String> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION};
        use windows_sys::Win32::System::Registry::{HKEY, KEY_ALL_ACCESS, REG_OPTION_NON_VOLATILE, RegCloseKey, RegCreateKeyExW, RegSetKeySecurity, RegSetValueExW};
        let (key, value, data) = (wide(KEY), wide(VALUE), wide(text));
        let sddl = wide("D:P(A;OICI;KA;;;BA)(A;OICI;KA;;;SY)");
        let mut sd = std::ptr::null_mut();
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err("the settings' permissions could not be made".into());
        }
        let mut h: HKEY = std::ptr::null_mut();
        let r = unsafe {
            RegCreateKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_ALL_ACCESS, std::ptr::null(), &mut h, std::ptr::null_mut())
        };
        let result = if r != 0 {
            Err(r)
        } else {
            // Also for a key an earlier version made with the permissions HKLM\SOFTWARE hands down.
            let r = unsafe { RegSetKeySecurity(h, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, sd) };
            let r = if r == 0 { unsafe { RegSetValueExW(h, value.as_ptr(), 0, REG_SZ, data.as_ptr() as _, (data.len() * 2) as u32) } } else { r };
            unsafe { RegCloseKey(h) };
            if r == 0 { Ok(()) } else { Err(r) }
        };
        unsafe { LocalFree(sd as _) };
        result.map_err(|e| match e {
            5 => "Changing remote access needs administrator rights".to_string(),
            e => format!("the remote access settings could not be saved (error {e})"),
        })
    }
}

/// The stored settings: `Ok(None)` when there are none yet, `Err` when they cannot be read.
pub fn stored() -> Result<Option<Config>, String> {
    #[cfg(all(windows, not(test)))]
    let text = machine::read()?;
    #[cfg(any(not(windows), test))]
    let text = match file() {
        Some(f) if f.exists() => Some(std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?),
        _ => None,
    };
    let Some(text) = text else { return Ok(None) };
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("the remote access settings are damaged: {e}"))?;
    Config::from_json(&v).filter(|c| valid_id(&c.id)).map(Some).ok_or_else(|| "the remote access settings are damaged".into())
}

/// New settings, not yet kept.
pub fn fresh() -> Config {
    Config { id: new_id(), token: hex(&random(16)), password: new_password(), relay: default_relay(), enabled: false }
}

/// The stored settings, or new ones (off) when there are none or they cannot be read.
pub fn load() -> Config {
    stored().ok().flatten().unwrap_or_else(fresh)
}

pub fn save(c: &Config) -> Result<(), String> {
    if c.password.trim().chars().count() < 8 {
        return Err("The password needs at least 8 characters".into());
    }
    let text = serde_json::to_string_pretty(&c.to_json()).map_err(|e| e.to_string())?;
    #[cfg(all(windows, not(test)))]
    return machine::write(&text);
    #[cfg(any(not(windows), test))]
    {
        let f = file().ok_or("no settings folder")?;
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        // Readable by its user only from the start: it holds the password.
        let tmp = f.with_extension("json.new");
        // A leftover could keep wider permissions: a new file gets the mode.
        let _ = std::fs::remove_file(&tmp);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(&tmp)
            .and_then(|mut h| {
                h.write_all(text.as_bytes())?;
                h.sync_all()
            })
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &f).map_err(|e| format!("{}: {e}", f.display()))
    }
}

pub fn valid_id(id: &str) -> bool {
    (6..=12).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `relay.example` or `relay.example:7071` as host and port.
pub fn relay_addr(relay: &str) -> (String, u16) {
    match relay.trim().rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap_or(crate::net::RELAY_PORT)),
        _ => (relay.trim().to_string(), crate::net::RELAY_PORT),
    }
}

/// A viewer's address for an ID: `sshremote://ID@relay`.
pub fn address(id: &str, relay: &str) -> String {
    format!("sshremote://{}@{}", id.trim().replace(' ', ""), relay.trim())
}

/// "123 456 789", to read out.
pub fn spaced(id: &str) -> String {
    id.as_bytes().chunks(3).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// How remote access is doing, for the window.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Off,
    Connecting,
    Online,
    /// A viewer is connected now (how many).
    InUse(u32),
    Failed(String),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static VIEWERS: AtomicU32 = AtomicU32::new(0);

fn set_state(s: State) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

/// This process's own remote access, when it runs one.
pub fn state() -> State {
    let n = VIEWERS.load(Ordering::Relaxed);
    let s = STATE.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    if n > 0 && s == State::Online { State::InUse(n) } else { s }
}

fn line(s: &mut TcpStream) -> io::Result<String> {
    // Byte by byte: what follows the line belongs to the handshake.
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    while out.len() < 256 {
        if s.read(&mut b)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if b[0] == b'\n' {
            break;
        }
        out.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// The way that last reached each relay: its own port, or HTTP on port 80.
static WAY: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

/// A connection to a relay. A port given is used as it is; without one, the relay's own port
/// (7071) is tried first and then port 80 with an HTTP upgrade, which networks that let only
/// web traffic out (and relays behind a web server) still pass.
pub fn dial(relay: &str) -> io::Result<TcpStream> {
    let explicit = relay.trim().rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok());
    let (host, port) = relay_addr(relay);
    let direct = |port: u16| -> io::Result<TcpStream> {
        let mut last = io::Error::other("no address");
        for a in std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))? {
            match TcpStream::connect_timeout(&a, Duration::from_secs(4)) {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
        Err(last)
    };
    if explicit {
        return direct(port);
    }
    let known = WAY.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(h, _)| *h == host).map(|(_, http)| *http);
    let http = |host: &str| -> io::Result<TcpStream> {
        let mut s = direct(80)?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        write!(s, "GET /relay HTTP/1.1\r\nHost: {host}\r\nUpgrade: sshremote-relay\r\nConnection: Upgrade\r\n\r\n")?;
        let status = line(&mut s)?;
        // The headers end with an empty line.
        while !line(&mut s)?.is_empty() {}
        if !status.contains(" 101") {
            return Err(io::Error::other(format!("{host}:80 did not pass the relay through: {status}")));
        }
        s.set_read_timeout(None)?;
        Ok(s)
    };
    let remember = |via_http: bool| {
        let mut w = WAY.lock().unwrap_or_else(|e| e.into_inner());
        w.retain(|(h, _)| *h != host);
        w.push((host.clone(), via_http));
    };
    let order = if known == Some(true) { [true, false] } else { [false, true] };
    let mut err = io::Error::other("unreachable");
    for via_http in order {
        match if via_http { http(&host) } else { direct(port) } {
            Ok(s) => {
                remember(via_http);
                return Ok(s);
            }
            Err(e) => err = e,
        }
    }
    Err(io::Error::new(err.kind(), format!("the relay {host} (port {port}, and 80): {err}")))
}

/// Whether an ID is waiting at a relay now.
pub fn online(id: &str, relay: &str) -> Result<bool, String> {
    let mut s = dial(relay).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    writeln!(s, "status {id}").map_err(|e| e.to_string())?;
    match line(&mut s).map_err(|e| e.to_string())?.as_str() {
        "online" => Ok(true),
        "offline" => Ok(false),
        other => Err(other.to_string()),
    }
}

/// Wrong passwords seen lately, and one password check at a time: guesses wait their turn and
/// cost more time each, and the count fades after a quiet quarter of an hour, so wrong guesses
/// slow an attacker without locking the owner out.
#[derive(Default)]
struct Guard {
    failures: Mutex<(u32, Option<std::time::Instant>)>,
    turn: Mutex<()>,
    /// Calls waiting for the turn: past two, more are turned away at once, so a queue of guesses
    /// cannot keep the owner's own call waiting until it gives up.
    waiting: std::sync::atomic::AtomicUsize,
}

impl Guard {
    fn delay(&self) -> Duration {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if f.1.is_some_and(|t| t.elapsed() > Duration::from_secs(15 * 60)) {
            *f = (0, None);
        }
        if f.0 == 0 { Duration::ZERO } else { Duration::from_secs((1u64 << f.0.min(5)).min(30)) }
    }

    fn failed(&self) {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        *f = (f.0 + 1, Some(std::time::Instant::now()));
    }

    fn passed(&self) {
        *self.failures.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
    }
}

/// Keeps this computer reachable by its ID until `keep` says to stop: a line to the relay, a new
/// connection for each viewer, and the agent run as a process of its own for each.
pub fn run(keep: impl Fn() -> Option<Config>) {
    let guard = Arc::new(Guard::default());
    let mut wait = 2;
    while let Some(mut cfg) = keep() {
        set_state(State::Connecting);
        let mut s = match dial(&cfg.relay) {
            Ok(s) => s,
            Err(e) => {
                set_state(State::Failed(format!("Could not reach {e}")));
                sleep_while(&keep, wait);
                wait = (wait * 2).min(60);
                continue;
            }
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
        let answer = writeln!(s, "listen {} {}", cfg.id, cfg.token).and_then(|_| line(&mut s));
        let refused = match answer.as_deref() {
            Ok("ok") => None,
            Ok("taken") => {
                // Another computer holds that ID: take a new one, if it can be kept.
                cfg.id = new_id();
                match save(&cfg) {
                    Ok(()) => continue,
                    Err(e) => Some(format!("The ID is taken and a new one could not be kept: {e}")),
                }
            }
            Ok(other) => Some(format!("The relay refused: {other}")),
            Err(e) => Some(format!("The relay did not answer: {e}")),
        };
        if let Some(why) = refused {
            set_state(State::Failed(why));
            sleep_while(&keep, wait);
            wait = (wait * 2).min(60);
            continue;
        }
        wait = 2;
        set_state(State::Online);
        // The relay pings every 30 s; nothing for 75 s means the line is gone. Short reads so a
        // change of the settings (off, a new password) takes effect within a second or two.
        let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
        let mut heard = std::time::Instant::now();
        let mut pending = Vec::new();
        'line: loop {
            if keep().as_ref().is_none_or(|k| k.id != cfg.id || k.password != cfg.password || k.relay != cfg.relay) {
                let _ = s.shutdown(std::net::Shutdown::Both);
                break;
            }
            let mut b = [0u8; 256];
            match s.read(&mut b) {
                Ok(0) => break,
                Ok(n) => {
                    heard = std::time::Instant::now();
                    pending.extend_from_slice(&b[..n]);
                    if pending.len() > 4096 {
                        break;
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    if heard.elapsed() > Duration::from_secs(75) {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            }
            while let Some(at) = pending.iter().position(|c| *c == 10) {
                let l: Vec<u8> = pending.drain(..=at).collect();
                let l = String::from_utf8_lossy(&l).trim().to_string();
                if l == "ping" {
                    if writeln!(s, "pong").is_err() {
                        break 'line;
                    }
                } else if let Some(nonce) = l.strip_prefix("call ") {
                    let (cfg, nonce, guard) = (cfg.clone(), nonce.to_string(), guard.clone());
                    thread::spawn(move || answer_call(&cfg, &nonce, &guard));
                }
            }
        }
        set_state(State::Connecting);
        sleep_while(&keep, 1);
    }
    set_state(State::Off);
}

fn sleep_while(keep: &impl Fn() -> Option<Config>, secs: u64) {
    for _ in 0..secs * 4 {
        if keep().is_none() {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// One viewer: a connection of its own through the relay at once (the relay waits only so
/// long), then the password checked in turn, then the agent.
fn answer_call(cfg: &Config, nonce: &str, guard: &Guard) {
    let Ok(mut s) = dial(&cfg.relay) else { return };
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    if writeln!(s, "accept {} {nonce}", cfg.id).is_err() || !line(&mut s).is_ok_and(|l| l == "ok") {
        return;
    }
    if guard.waiting.fetch_add(1, Ordering::AcqRel) >= 2 {
        guard.waiting.fetch_sub(1, Ordering::AcqRel);
        return;
    }
    let checked = {
        let _turn = guard.turn.lock().unwrap_or_else(|e| e.into_inner());
        guard.waiting.fetch_sub(1, Ordering::AcqRel);
        thread::sleep(guard.delay());
        // A viewer that goes quiet in the middle of the check does not hold the turn.
        let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
        crate::net::handshake_agent(s, &cfg.password)
    };
    let (r, w) = match checked {
        Ok(p) => p,
        Err(e) => {
            if e.kind() == io::ErrorKind::PermissionDenied || e.to_string().contains("key exchange") {
                guard.failed();
            }
            return;
        }
    };
    guard.passed();
    VIEWERS.fetch_add(1, Ordering::Relaxed);
    let _ = serve_viewer(r, w);
    VIEWERS.fetch_sub(1, Ordering::Relaxed);
}

/// The agent as a process of its own, its input and output the sealed stream.
fn serve_viewer(mut r: crate::net::SealedReader, mut w: crate::net::SealedWriter) -> Result<(), String> {
    // The session's own pings come every second; two minutes of nothing means it is gone.
    let socket = w.stream().map_err(|e| e.to_string())?;
    let _ = socket.set_read_timeout(Some(Duration::from_secs(120)));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(["--agent", "--capture", "auto"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let up = thread::spawn(move || {
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if w.write_all(&buf[..n]).and_then(|_| w.flush()).is_err() {
                        break;
                    }
                }
            }
        }
        // The agent is gone: so is the session, and the viewer's side ends with it.
        let _ = socket.shutdown(std::net::Shutdown::Both);
    });
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdin.write_all(&buf[..n]).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        }
    }
    drop(stdin);
    // The agent ends within a few seconds of its viewer; past that it is ended.
    for _ in 0..40 {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = up.join();
    Ok(())
}

/// Remote access run by this window while it is open, when no service runs it.
pub fn start_in_app() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        thread::spawn(|| loop {
            let on = || {
                let c = load();
                (c.enabled && !service_runs_cached()).then_some(c)
            };
            if on().is_some() {
                run(on);
            }
            thread::sleep(Duration::from_secs(2));
        });
    });
}

/// [`service_runs`], asked again at most every 15 s.
pub fn service_runs_cached() -> bool {
    static LAST: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match *l {
        Some((t, v)) if t.elapsed() < Duration::from_secs(15) => v,
        _ => {
            let v = service_runs();
            *l = Some((std::time::Instant::now(), v));
            v
        }
    }
}

/// Whether a process still runs.
fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = unsafe { GetExitCodeProcess(h, &mut code) };
        unsafe { CloseHandle(h) };
        return ok != 0 && code == 259;
    }
    #[cfg(unix)]
    {
        return unsafe { libc::kill(pid as i32, 0) } == 0;
    }
    #[allow(unreachable_code)]
    true
}

/// Whether a service (Windows) or a user unit (Linux) keeps remote access on at start.
pub fn service_installed() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        return Command::new("sc").args(["query", crate::service::NAME]).creation_flags(0x0800_0000).output().is_ok_and(|o| o.status.success());
    }
    #[cfg(unix)]
    {
        return unit_file().is_some_and(|f| f.exists());
    }
    #[allow(unreachable_code)]
    false
}

/// Whether the installed service runs this very program file (an older copy lacks what this has).
pub fn service_is_this() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let Ok(o) = Command::new("sc").args(["qc", crate::service::NAME]).creation_flags(0x0800_0000).output() else { return true };
        let text = String::from_utf8_lossy(&o.stdout).to_lowercase();
        return std::env::current_exe().is_ok_and(|e| text.contains(&e.display().to_string().to_lowercase()));
    }
    #[cfg(unix)]
    {
        return unit_file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .is_none_or(|t| t.contains("--service") && std::env::current_exe().is_ok_and(|e| t.contains(&e.display().to_string())));
    }
    #[allow(unreachable_code)]
    true
}

/// Whether the service is running now, so this window leaves remote access to it.
pub fn service_runs() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        return Command::new("sc")
            .args(["query", crate::service::NAME])
            .creation_flags(0x0800_0000)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("RUNNING"));
    }
    #[cfg(unix)]
    {
        return Command::new("systemctl").args(["--user", "is-active", "--quiet", UNIT]).status().is_ok_and(|s| s.success());
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(unix)]
const UNIT: &str = "sshremote-access.service";

#[cfg(unix)]
fn unit_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("systemd").join("user").join(UNIT))
}

/// Starts remote access with the computer: the Windows service (elevated), or a systemd user
/// unit that also runs before anyone opens the window (with lingering, before sign-in).
pub fn install_service() -> Result<(), String> {
    #[cfg(windows)]
    return crate::service::install();
    #[cfg(unix)]
    {
        let f = unit_file().ok_or("no home folder")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(f.parent().ok_or("bad path")?).map_err(|e| e.to_string())?;
        let unit = format!(
            "[Unit]\nDescription=SSH Remote: remote access by ID\nAfter=network-online.target graphical-session.target\n\n[Service]\nExecStart=\"{}\" remote-access run --service\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
            exe.display()
        );
        std::fs::write(&f, unit).map_err(|e| e.to_string())?;
        let ok = |args: &[&str]| Command::new("systemctl").args(args).status().map_err(|e| e.to_string()).and_then(|s| if s.success() { Ok(()) } else { Err(format!("systemctl {}", args.join(" "))) });
        ok(&["--user", "daemon-reload"])?;
        ok(&["--user", "enable", "--now", UNIT])?;
        // Lingering starts the user's units at boot, before anyone signs in; it may need rights.
        let _ = Command::new("loginctl").args(["enable-linger"]).status();
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("not on this system".into())
}

pub fn uninstall_service() -> Result<(), String> {
    #[cfg(windows)]
    return crate::service::uninstall();
    #[cfg(unix)]
    {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", UNIT]).status();
        if let Some(f) = unit_file() {
            let _ = std::fs::remove_file(f);
        }
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("not on this system".into())
}

/// `sshremote remote-access [status | on | off | run | id | password [NEW] | relay HOST | install | uninstall]`
pub fn cli(args: &[String]) -> Result<(), String> {
    let mut c = load();
    let show = |c: &Config| {
        println!("ID:       {}", spaced(&c.id));
        println!("password: {}", c.password);
        println!("relay:    {}", c.relay);
        println!("on:       {}", if c.enabled { "yes" } else { "no" });
        println!("service:  {}", if service_installed() { "installed (starts it with the computer)" } else { "not installed (on while sshremote's window is open)" });
        println!("connect with: sshremote --gui {}", address(&c.id, &c.relay));
    };
    match args.first().map(String::as_str) {
        // For a window that runs remote access through this program (the Terminal edition).
        Some("json") => {
            let c = stored()?.ok_or("not set up yet")?;
            println!("{}", serde_json::json!({ "id": c.id, "password": c.password, "relay": c.relay, "enabled": c.enabled, "service": service_installed() }));
            Ok(())
        }
        None | Some("status") => {
            if stored()?.is_none() {
                save(&c)?;
            }
            show(&c);
            Ok(())
        }
        Some("on") | Some("off") => {
            c.enabled = args[0] == "on";
            save(&c)?;
            show(&c);
            Ok(())
        }
        Some("password") => {
            c.password = args.get(1).cloned().unwrap_or_else(new_password);
            save(&c)?;
            println!("password: {}", c.password);
            Ok(())
        }
        Some("id") => {
            c.id = new_id();
            save(&c)?;
            println!("ID: {}", spaced(&c.id));
            Ok(())
        }
        Some("relay") => {
            c.relay = args.get(1).cloned().unwrap_or_else(default_relay);
            save(&c)?;
            println!("relay: {}", c.relay);
            Ok(())
        }
        Some("install") => {
            c.enabled = true;
            save(&c)?;
            install_service()?;
            println!("remote access starts with the computer");
            Ok(())
        }
        Some("uninstall") => uninstall_service(),
        // `--service`: run by the service or unit itself; otherwise it leaves remote access to a
        // service that runs it. `--parent PID`: ends with that process (a window that started it).
        Some("run") => {
            if stored()?.is_none() {
                save(&c)?;
            }
            // systemd sets INVOCATION_ID for what its units start, older units included.
            let service = args.iter().any(|a| a == "--service") || std::env::var_os("INVOCATION_ID").is_some();
            let parent: Option<u32> = args.iter().position(|a| a == "--parent").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok());
            eprintln!("remote access as {} through {}", spaced(&c.id), c.relay);
            let keep = || {
                if parent.is_some_and(|p| !alive(p)) {
                    std::process::exit(0);
                }
                Some(load()).filter(|c| c.enabled && (service || !service_runs_cached()))
            };
            // Until the parent goes, it waits for remote access to be turned on again.
            loop {
                run(keep);
                if parent.is_none() {
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(2));
                let _ = keep();
            }
        }
        _ => Err("usage: sshremote remote-access [status | on | off | run | id | password [NEW] | relay HOST | install | uninstall]".into()),
    }
}

/// The relay (`sshremote --relay`): IDs held by their tokens, a line to each listening agent,
/// calls passed on, and the bytes of each pair copied. It is open to the internet, so every
/// connection is bounded: in time, in size, and by how many one address may hold or try.
pub mod relay {
    use super::*;
    use std::collections::HashMap;
    use std::net::{IpAddr, TcpListener};
    use std::sync::mpsc;
    use std::time::Instant;

    /// Connections one address (an IPv6 /64) may hold at once once it has said what it wants,
    /// and before that (a slow first line); lines it may send a minute, and status questions.
    const PER_ADDRESS: usize = 32;
    const HELLO_PER_ADDRESS: usize = 16;
    const TRIES_A_MINUTE: u32 = 60;
    const STATUS_A_MINUTE: u32 = 600;
    /// Calls to one ID a minute, and calls waiting for it at once.
    const CALLS_A_MINUTE: u32 = 60;
    const CALLS_FROM_ONE_A_MINUTE: u32 = 5;
    const CALLS_WAITING: usize = 4;
    /// New IDs one address may take a day; an ID is kept only after its agent stayed a minute.
    const NEW_IDS_A_DAY: u32 = 50;
    const SETTLE: Duration = Duration::from_secs(60);
    /// Connections in all, so a flood cannot take the machine's threads.
    const ALL: usize = 4000;
    /// The first line (and an HTTP upgrade's headers) must come within this.
    const HELLO: Duration = Duration::from_secs(10);
    /// A paired session with nothing either way for this long is over (sessions ping every second).
    const IDLE: Duration = Duration::from_secs(120);
    /// IDs not seen for this long go back to the pool.
    const KEEP_ID: u64 = 180 * 24 * 3600;
    /// Agents of the older protocol (`sshremote --serve --via`) waiting at once, in all and by address.
    const OLD_WAITING: usize = 1000;
    const OLD_PER_ADDRESS: usize = 4;

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }

    /// The part of an address that limits count by: all of IPv4, the /64 of IPv6.
    fn key(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V6(v6) => {
                let mut s = v6.segments();
                s[4..].fill(0);
                IpAddr::V6(std::net::Ipv6Addr::from(s))
            }
            v4 => v4,
        }
    }

    fn hash(token: &str) -> String {
        use sha2::Digest;
        hex(&sha2::Sha256::digest(token.as_bytes()))
    }

    /// A count in a window of time: whether one more is within `max`.
    fn within<K: std::hash::Hash + Eq>(map: &mut HashMap<K, (Instant, u32)>, k: K, window: Duration, max: u32) -> bool {
        let e = map.entry(k).or_insert((Instant::now(), 0));
        if e.0.elapsed() > window {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        e.1 <= max
    }

    fn up(map: &mut HashMap<IpAddr, usize>, k: IpAddr, max: usize) -> bool {
        let n = map.entry(k).or_insert(0);
        if *n >= max {
            return false;
        }
        *n += 1;
        true
    }

    fn down(map: &mut HashMap<IpAddr, usize>, k: IpAddr) {
        if let Some(n) = map.get_mut(&k) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&k);
            }
        }
    }

    #[derive(Default)]
    pub struct Hub {
        /// ID → the token's hash and when it was last seen, kept on disk so an ID stays its owner's.
        owners: HashMap<String, (String, u64)>,
        dirty: bool,
        /// ID → its listening line's number, a way to call the agent, and its token's hash (which
        /// holds the ID while it is not yet kept).
        agents: HashMap<String, (u64, mpsc::Sender<String>, String)>,
        next: u64,
        /// NONCE → the viewer waiting for that call to be answered.
        calls: HashMap<String, mpsc::Sender<TcpStream>>,
        /// Calls by ID in the current minute, and waiting now.
        calls_by_id: HashMap<String, (Instant, u32)>,
        calls_by_source: HashMap<(String, IpAddr), (Instant, u32)>,
        waiting_by_id: HashMap<String, usize>,
        /// Lines and status questions by address in the current minute; new IDs by address today.
        tries: HashMap<IpAddr, (Instant, u32)>,
        statuses: HashMap<IpAddr, (Instant, u32)>,
        new_ids: HashMap<IpAddr, (Instant, u32)>,
        /// Connections held by address: before their first line, and after it.
        hello: HashMap<IpAddr, usize>,
        held: HashMap<IpAddr, usize>,
        all: usize,
        /// The older protocol's agents, by ID, with the address that left each.
        old: HashMap<String, (TcpStream, Instant, IpAddr)>,
    }

    impl Hub {
        /// The owners kept in `store`; a file that is there but cannot be read stops the relay
        /// rather than handing every ID to whoever asks first.
        pub fn new(store: Option<&std::path::Path>) -> Result<Hub, String> {
            let mut h = Hub::default();
            if let Some(f) = store.filter(|f| f.exists()) {
                let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
                let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", f.display()))?;
                for (id, o) in v.as_object().into_iter().flatten() {
                    match o {
                        // The first version kept only the hash.
                        serde_json::Value::String(h0) => {
                            h.owners.insert(id.clone(), (h0.clone(), now()));
                        }
                        o => {
                            if let (Some(hh), Some(t)) = (o.get(0).and_then(|x| x.as_str()), o.get(1).and_then(|x| x.as_u64())) {
                                h.owners.insert(id.clone(), (hh.to_string(), t));
                            }
                        }
                    }
                }
            }
            Ok(h)
        }

        /// Whether a token may use an ID now: its owner's, or one nobody holds.
        fn may_use(&self, id: &str, token: &str) -> bool {
            let h = hash(token);
            match self.owners.get(id) {
                Some((o, _)) => *o == h,
                None => self.agents.get(id).is_none_or(|(_, _, a)| *a == h),
            }
        }

        /// The ID kept as this token's, or its last-seen time brought up to date.
        fn keep(&mut self, id: &str, token: &str) {
            self.owners.insert(id.to_string(), (hash(token), now()));
            self.dirty = true;
        }

        /// Forgets what has gone stale: minutes of tries, IDs long unseen.
        fn prune(&mut self) {
            let minute = Duration::from_secs(60);
            self.tries.retain(|_, (t, _)| t.elapsed() < minute);
            self.statuses.retain(|_, (t, _)| t.elapsed() < minute);
            self.calls_by_id.retain(|_, (t, _)| t.elapsed() < minute);
            self.calls_by_source.retain(|_, (t, _)| t.elapsed() < minute);
            self.new_ids.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(24 * 3600));
            let cut = now().saturating_sub(KEEP_ID);
            let before = self.owners.len();
            self.owners.retain(|_, (_, seen)| *seen >= cut);
            self.dirty |= self.owners.len() != before;
            self.old.retain(|_, (_, since, _)| since.elapsed() < Duration::from_secs(24 * 3600));
        }
    }

    type Shared = Arc<Mutex<Hub>>;

    fn lock(hub: &Shared) -> std::sync::MutexGuard<'_, Hub> {
        hub.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs the relay until the listener fails.
    pub fn serve(listen: &str, store: Option<&str>) -> Result<(), String> {
        let store = store.map(PathBuf::from);
        let hub: Shared = Arc::new(Mutex::new(Hub::new(store.as_deref())?));
        let listener = TcpListener::bind(listen).map_err(|e| format!("{listen}: {e}"))?;
        println!("sshremote relay listening on {listen}");
        // Every 30 s: stale entries out, and the owners written (outside the lock, whole or not at all).
        {
            let hub = hub.clone();
            thread::spawn(move || {
                loop {
                    thread::sleep(Duration::from_secs(30));
                    let snapshot = {
                        let mut h = lock(&hub);
                        h.prune();
                        if !std::mem::take(&mut h.dirty) {
                            continue;
                        }
                        let map: serde_json::Map<String, serde_json::Value> = h.owners.iter().map(|(id, (hh, t))| (id.clone(), serde_json::json!([hh, t]))).collect();
                        serde_json::Value::Object(map).to_string()
                    };
                    if let Some(f) = &store {
                        let tmp = f.with_extension("new");
                        let done = std::fs::File::create(&tmp).and_then(|mut o| {
                            o.write_all(snapshot.as_bytes())?;
                            o.sync_all()
                        });
                        let done = done.and_then(|_| std::fs::rename(&tmp, f));
                        // The folder too, so the rename itself survives a power cut.
                        #[cfg(unix)]
                        if let Some(d) = f.parent().and_then(|d| std::fs::File::open(d).ok()) {
                            let _ = d.sync_all();
                        }
                        if done.is_err() {
                            lock(&hub).dirty = true;
                        }
                    }
                }
            });
        }
        for s in listener.incoming().flatten() {
            let Ok(peer) = s.peer_addr().map(|a| a.ip()) else { continue };
            {
                // A web server on this machine speaks for many: its clients are counted once it
                // names them.
                let mut h = lock(&hub);
                if h.all >= ALL || (!peer.is_loopback() && !up(&mut h.hello, key(peer), HELLO_PER_ADDRESS)) {
                    continue;
                }
                h.all += 1;
            }
            let hub = hub.clone();
            thread::spawn(move || {
                struct All(Shared);
                impl Drop for All {
                    fn drop(&mut self) {
                        lock(&self.0).all -= 1;
                    }
                }
                let _all = All(hub.clone());
                connection(&hub, s, peer);
            });
        }
        Ok(())
    }

    /// Reads one line within `deadline`, byte by byte (what follows belongs to the handshake).
    fn hello_line(s: &mut TcpStream, deadline: Instant) -> Option<String> {
        let mut raw = Vec::new();
        let mut b = [0u8; 1];
        while raw.len() < 1024 {
            let left = deadline.checked_duration_since(Instant::now())?;
            s.set_read_timeout(Some(left.max(Duration::from_millis(1)))).ok()?;
            match s.read(&mut b) {
                Ok(1) if b[0] == 10 => {
                    let l = String::from_utf8_lossy(&raw).to_string();
                    return Some(l.trim_end_matches('\r').to_string());
                }
                Ok(1) => raw.push(b[0]),
                _ => return None,
            }
        }
        None
    }

    /// What a connection asks, read within [`HELLO`]: the line and the address it counts for.
    fn hello(hub: &Shared, s: &mut TcpStream, peer: IpAddr, counted: &mut Option<IpAddr>) -> Option<(String, IpAddr)> {
        let deadline = Instant::now() + HELLO;
        let mut first = hello_line(s, deadline)?;
        let mut ip = peer;
        // Through a web server: an HTTP upgrade, then the same lines. Only a web server on this
        // machine may say where its client is, and it must.
        if first.starts_with("GET ") {
            let mut real = None;
            for _ in 0..32 {
                let h = hello_line(s, deadline)?;
                if h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.trim().eq_ignore_ascii_case("x-real-ip")
                {
                    real = v.trim().parse::<IpAddr>().ok();
                }
            }
            if peer.is_loopback() {
                ip = real?;
                // Counted as its own address from here, as a direct connection is from the start.
                if !up(&mut lock(hub).hello, key(ip), HELLO_PER_ADDRESS) {
                    return None;
                }
                *counted = Some(ip);
            }
            s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: sshremote-relay\r\nConnection: Upgrade\r\n\r\n").ok()?;
            first = hello_line(s, deadline)?;
        }
        let _ = s.set_read_timeout(None);
        Some((first, ip))
    }

    fn connection(hub: &Shared, mut s: TcpStream, peer: IpAddr) {
        let mut counted = None;
        let asked = hello(hub, &mut s, peer, &mut counted);
        {
            let mut h = lock(hub);
            if !peer.is_loopback() {
                down(&mut h.hello, key(peer));
            }
            if let Some(c) = counted {
                down(&mut h.hello, key(c));
            }
        }
        let Some((first, ip)) = asked else { return };
        let words: Vec<String> = first.split_whitespace().map(str::to_string).collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            // `accept` answers a call the relay made, with an unguessable nonce of its own: it
            // counts against nothing, or a queue of calls could keep the agent from answering.
            ["accept", id, nonce] if valid_id(id) && nonce.len() <= 64 => {
                let waiting = lock(hub).calls.remove(*nonce);
                if let Some(v) = waiting {
                    let _ = v.send(s);
                }
                return;
            }
            // Status questions come from every open window every few seconds: a budget of their own.
            ["status", id] if valid_id(id) => {
                let (ok, on) = {
                    let mut h = lock(hub);
                    (within(&mut h.statuses, key(ip), Duration::from_secs(60), STATUS_A_MINUTE), h.agents.contains_key(*id))
                };
                let _ = writeln!(s, "{}", if !ok { "too many attempts; wait a minute" } else if on { "online" } else { "offline" });
                return;
            }
            _ => {}
        }
        {
            let mut h = lock(hub);
            if !within(&mut h.tries, key(ip), Duration::from_secs(60), TRIES_A_MINUTE) || !up(&mut h.held, key(ip), PER_ADDRESS) {
                drop(h);
                let _ = writeln!(s, "too many attempts; wait a minute");
                return;
            }
        }
        struct Held(Shared, IpAddr);
        impl Drop for Held {
            fn drop(&mut self) {
                down(&mut lock(&self.0).held, key(self.1));
            }
        }
        let _held = Held(hub.clone(), ip);
        match words.as_slice() {
            ["listen", id, token] if valid_id(id) && (16..=128).contains(&token.len()) => listen(hub, s, id, token, ip),
            ["viewer", id] if id.len() <= 32 => viewer(hub, s, id, ip),
            // The older one-off protocol of `sshremote --serve --via`: never for an ID remote
            // access holds or another waiter has, and a few waiters an address.
            ["agent", id] if id.len() <= 32 => {
                let mut h = lock(hub);
                let mine = h.old.values().filter(|(_, _, a)| key(*a) == key(ip)).count();
                if h.owners.contains_key(*id) || h.agents.contains_key(*id) || h.old.contains_key(*id) || h.old.len() >= OLD_WAITING || mine >= OLD_PER_ADDRESS {
                    drop(h);
                    let _ = writeln!(s, "taken");
                    return;
                }
                h.old.insert(id.to_string(), (s, Instant::now(), ip));
            }
            _ => {}
        }
    }

    /// An agent's line: calls passed on, pings, and the line given up when it goes quiet or a
    /// newer line for the same ID replaces it. An ID nobody held becomes this token's once its
    /// agent has stayed a minute, a few new ones an address a day.
    fn listen(hub: &Shared, mut s: TcpStream, id: &str, token: &str, ip: IpAddr) {
        let (tx, rx) = mpsc::channel::<String>();
        let (number, mut kept) = {
            let mut h = lock(hub);
            if !h.may_use(id, token) {
                drop(h);
                let _ = writeln!(s, "taken");
                return;
            }
            // New IDs are counted when kept; an address past its day's allowance waits.
            let known = h.owners.contains_key(id);
            if !known && h.new_ids.get(&key(ip)).is_some_and(|(t, n)| t.elapsed() < Duration::from_secs(24 * 3600) && *n >= NEW_IDS_A_DAY) {
                drop(h);
                let _ = writeln!(s, "busy: too many new IDs from this address today");
                return;
            }
            h.next += 1;
            let n = h.next;
            h.agents.insert(id.to_string(), (n, tx, hash(token)));
            (n, None::<Instant>)
        };
        let since = Instant::now();
        if writeln!(s, "ok").is_ok() && s.set_read_timeout(Some(Duration::from_millis(200))).is_ok() {
            let mut heard = Instant::now();
            let mut pinged = Instant::now();
            let mut buf = [0u8; 64];
            'line: loop {
                // Kept (or seen again) after a minute, then every hour while it stays.
                if since.elapsed() > SETTLE && kept.is_none_or(|t| t.elapsed() > Duration::from_secs(3600)) {
                    let mut h = lock(hub);
                    if !h.may_use(id, token) {
                        break;
                    }
                    if !h.owners.contains_key(id) {
                        within(&mut h.new_ids, key(ip), Duration::from_secs(24 * 3600), u32::MAX);
                    }
                    h.keep(id, token);
                    kept = Some(Instant::now());
                }
                loop {
                    match rx.try_recv() {
                        Ok(nonce) => {
                            if writeln!(s, "call {nonce}").is_err() {
                                break 'line;
                            }
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        // Replaced by a newer line for this ID.
                        Err(mpsc::TryRecvError::Disconnected) => break 'line,
                    }
                }
                if pinged.elapsed() > Duration::from_secs(30) {
                    if writeln!(s, "ping").is_err() {
                        break;
                    }
                    pinged = Instant::now();
                }
                match s.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => heard = Instant::now(),
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                        if heard.elapsed() > Duration::from_secs(75) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let mut h = lock(hub);
        if h.agents.get(id).is_some_and(|(n, _, _)| *n == number) {
            h.agents.remove(id);
        }
    }

    /// A viewer: its call passed to the agent, and the agent's answering connection paired with it.
    fn viewer(hub: &Shared, mut s: TcpStream, id: &str, ip: IpAddr) {
        let agent = lock(hub).agents.get(id).map(|(_, t, _)| t.clone());
        let Some(agent) = agent else {
            // An agent of the older protocol, if one waits under that ID.
            let old = lock(hub).old.remove(id).map(|(a, _, _)| a);
            match old {
                Some(mut a) => {
                    if writeln!(a, "ok").is_ok() && writeln!(s, "ok").is_ok() {
                        splice(a, s);
                    }
                }
                None => {
                    let _ = writeln!(s, "offline");
                }
            }
            return;
        };
        let nonce = hex(&random(16));
        let (tx, rx) = mpsc::channel();
        {
            let mut h = lock(hub);
            let waiting = h.waiting_by_id.get(id).copied().unwrap_or(0);
            // A few calls a minute from any one address, so one cannot keep the owner out alone.
            let minute = Duration::from_secs(60);
            if h.calls.len() >= 10_000
                || waiting >= CALLS_WAITING
                || !within(&mut h.calls_by_source, (id.to_string(), key(ip)), minute, CALLS_FROM_ONE_A_MINUTE)
                || !within(&mut h.calls_by_id, id.to_string(), minute, CALLS_A_MINUTE)
            {
                drop(h);
                let _ = writeln!(s, "too many attempts; wait a minute");
                return;
            }
            h.calls.insert(nonce.clone(), tx);
            *h.waiting_by_id.entry(id.to_string()).or_insert(0) += 1;
        }
        let answered = if agent.send(nonce.clone()).is_ok() { rx.recv_timeout(Duration::from_secs(20)).ok() } else { None };
        {
            let mut h = lock(hub);
            h.calls.remove(&nonce);
            if let Some(n) = h.waiting_by_id.get_mut(id) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    h.waiting_by_id.remove(id);
                }
            }
        }
        let Some(mut a) = answered else {
            let _ = writeln!(s, "the computer did not answer");
            return;
        };
        if writeln!(a, "ok").is_ok() && writeln!(s, "ok").is_ok() {
            splice(a, s);
        }
    }

    /// Copies bytes both ways until either side ends or both go quiet for [`IDLE`].
    pub fn splice(a: TcpStream, b: TcpStream) {
        let _ = a.set_read_timeout(Some(IDLE));
        let _ = b.set_read_timeout(Some(IDLE));
        let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else { return };
        let (mut a, mut b) = (a, b);
        let t = thread::spawn(move || {
            let _ = io::copy(&mut a2, &mut b2);
            let _ = a2.shutdown(std::net::Shutdown::Both);
            let _ = b2.shutdown(std::net::Shutdown::Both);
        });
        let _ = io::copy(&mut b, &mut a);
        let _ = a.shutdown(std::net::Shutdown::Both);
        let _ = b.shutdown(std::net::Shutdown::Both);
        let _ = t.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_passwords_and_addresses() {
        let id = new_id();
        assert!(valid_id(&id) && id.len() == 9 && !id.starts_with('0'));
        assert_eq!(spaced("123456789"), "123 456 789");
        assert_eq!(new_password().len(), 8);
        assert_eq!(relay_addr("r.example"), ("r.example".into(), crate::net::RELAY_PORT));
        assert_eq!(relay_addr("r.example:9000"), ("r.example".into(), 9000));
        assert_eq!(address("123 456 789", "r.example"), "sshremote://123456789@r.example");
        assert!(!valid_id("12a456789") && !valid_id("123"));
    }

    /// The relay's side: an ID claimed by its token, its status, a viewer's call answered by a
    /// new connection, bytes both ways, and the ID refused to another token.
    #[test]
    fn the_relay_pairs_by_id() {
        let port = 20000 + (std::process::id() % 20000) as u16;
        let addr = format!("127.0.0.1:{port}");
        let a2 = addr.clone();
        thread::spawn(move || crate::net::relay(Some(&a2), None));
        thread::sleep(Duration::from_millis(300));
        let id = "987654321";
        let token = "0123456789abcdef0123";
        let mut agent = TcpStream::connect(&addr).unwrap();
        agent.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(agent, "listen {id} {token}").unwrap();
        assert_eq!(line(&mut agent).unwrap(), "ok");
        assert!(online(id, &addr).unwrap(), "online once listening");
        assert!(!online("111222333", &addr).unwrap());
        // The same through a web server's HTTP upgrade, as port 80 brings it.
        let mut up = TcpStream::connect(&addr).unwrap();
        up.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(up, "GET /relay HTTP/1.1\r\nHost: x\r\nUpgrade: sshremote-relay\r\nConnection: Upgrade\r\nX-Real-IP: 10.1.2.3\r\n\r\nstatus {id}\n").unwrap();
        assert!(line(&mut up).unwrap().contains(" 101"), "from a web server with its client's address");
        while !line(&mut up).unwrap().is_empty() {}
        assert_eq!(line(&mut up).unwrap(), "online");
        let mut thief = TcpStream::connect(&addr).unwrap();
        thief.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(thief, "listen {id} ffffffffffffffffffff").unwrap();
        assert_eq!(line(&mut thief).unwrap(), "taken");
        let a3 = addr.clone();
        let viewer = thread::spawn(move || {
            let mut v = TcpStream::connect(&a3).unwrap();
            v.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            writeln!(v, "viewer {id}").unwrap();
            assert_eq!(line(&mut v).unwrap(), "ok");
            v.write_all(b"ping!").unwrap();
            let mut got = [0u8; 5];
            v.read_exact(&mut got).unwrap();
            got
        });
        let call = line(&mut agent).unwrap();
        let nonce = call.strip_prefix("call ").expect("a call").to_string();
        let mut a = TcpStream::connect(&addr).unwrap();
        a.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(a, "accept {id} {nonce}").unwrap();
        assert_eq!(line(&mut a).unwrap(), "ok");
        let mut got = [0u8; 5];
        a.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping!");
        a.write_all(b"pong!").unwrap();
        assert_eq!(&viewer.join().unwrap(), b"pong!");
        // The older protocol cannot take an ID remote access holds.
        let mut old = TcpStream::connect(&addr).unwrap();
        old.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(old, "agent {id}").unwrap();
        assert_eq!(line(&mut old).unwrap(), "taken");
        // Once its line ends the ID is offline again.
        drop(agent);
        thread::sleep(Duration::from_millis(600));
        assert!(!online(id, &addr).unwrap());
    }

    #[test]
    fn settings_are_kept() {
        let d = std::env::temp_dir().join(format!("sshremote-access-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        TEST_DIR.with(|t| *t.borrow_mut() = Some(d.clone()));
        let mut c = load();
        c.enabled = true;
        save(&c).unwrap();
        assert_eq!(load(), c);
        c.password = "abc".into();
        assert!(save(&c).is_err(), "too short");
        TEST_DIR.with(|t| *t.borrow_mut() = None);
    }
}
