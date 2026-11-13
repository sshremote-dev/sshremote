//! Remote access by ID: this computer keeps a line open to a relay under a lasting ID, and a
//! viewer anywhere reaches it with that ID and the password, as RustDesk does; for PCs on Wi-Fi
//! or behind NAT with no fixed address. SSH stays the way for one host reached directly.
//!
//! Relay lines:
//!   agent:  `listen ID TOKEN` → `ok` | `taken`; then the relay sends `call NONCE` for each viewer
//!           and the agent answers with a new connection `accept ID NONCE` → `ok`.
//!   viewer: `viewer ID` → `ok` | `offline` | `busy`; `status ID` → `online` | `offline`.
//! The relay never learns the password: both ends run SPAKE2 through it and seal every byte.

use std::io::{self, Read, Write};
use std::net::TcpStream;
#[cfg(any(not(windows), test))]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::sys::{hex, random};

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
    /// Whether the person at the screen is asked before a viewer comes in.
    pub ask: bool,
    /// What a viewer may do (`grant`), until the person at the screen changes it.
    pub allow: u8,
    /// Whether computers on this network may find this one by asking (`lan`).
    pub lan: bool,
    /// Whether viewers on this network may come straight in (`net::DIRECT_PORT`), not through the relay.
    pub direct: bool,
    /// Two-step sign-in: the authenticator app's secret (base32); empty: the password is enough.
    pub totp: String,
}

impl Config {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "id": self.id, "token": self.token, "password": self.password, "relay": self.relay, "enabled": self.enabled, "ask": self.ask, "allow": self.allow, "lan": self.lan, "direct": self.direct, "totp": self.totp })
    }

    /// The rights and whether to ask, as the agent takes them (`--grant`).
    pub fn grant(&self) -> u8 {
        (self.allow & crate::grant::ALL) | if self.ask { crate::grant::ASK } else { 0 }
    }

    fn from_json(v: &serde_json::Value) -> Option<Config> {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        Some(Config {
            id: s("id")?,
            token: s("token")?,
            password: s("password")?,
            relay: s("relay").filter(|r| !r.trim().is_empty()).unwrap_or_else(default_relay),
            enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false),
            ask: v.get("ask").and_then(|x| x.as_bool()).unwrap_or(false),
            allow: v.get("allow").and_then(|x| x.as_u64()).map_or(crate::grant::ALL, |a| a as u8 & crate::grant::ALL),
            lan: v.get("lan").and_then(|x| x.as_bool()).unwrap_or(true),
            direct: v.get("direct").and_then(|x| x.as_bool()).unwrap_or(true),
            totp: s("totp").unwrap_or_default(),
        })
    }
}

fn default_relay() -> String {
    DEFAULT_RELAY.into()
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
#[cfg(all(windows, not(test)))]
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
    Config { id: new_id(), token: hex(&random(16)), password: new_password(), relay: default_relay(), enabled: false, ask: false, allow: crate::grant::ALL, lan: true, direct: true, totp: String::new() }
}

/// The stored settings, or new ones (off) when there are none or they cannot be read.
pub fn load() -> Config {
    stored().ok().flatten().unwrap_or_else(fresh)
}

/// The relay this computer uses, to reach others by ID through it too.
pub fn relay() -> String {
    stored().ok().flatten().map_or_else(default_relay, |c| c.relay)
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

/// This process's own remote access, when it runs one: in use for as long as a viewer is served,
/// whatever the line to the relay is doing.
pub fn state() -> State {
    let n = VIEWERS.load(Ordering::Relaxed);
    let s = STATE.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    if n > 0 { State::InUse(n) } else { s }
}

/// The viewers being served: a number, the settings they came in by, and their connection, so a
/// change of those settings (off, a new password, ID or relay) ends them too.
static SESSIONS: Mutex<Vec<(u64, String, TcpStream)>> = Mutex::new(Vec::new());

/// When each viewer came in, by its number, in seconds since 1970.
static SINCE: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// How many viewers this program serves by ID now (remote access run inside the app).
pub fn viewers_here() -> usize {
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// The viewers this program serves by ID now: each one's number and when it came in.
pub fn viewers() -> Vec<(u64, u64)> {
    let since = SINCE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(n, _, _)| (*n, since.iter().find(|s| s.0 == *n).map_or(0, |s| s.1))).collect()
}

/// Ends one viewer's connection (the person at this screen chose to).
pub fn disconnect(viewer: u64) {
    for (n, _, s) in SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        if *n == viewer {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// What a viewer had to know to come in: the ID and password at that relay.
fn credentials(c: &Config) -> String {
    format!("{}\n{}\n{}", c.id, c.password, c.relay)
}

/// Ends every viewer that came in by other settings than `now`; all of them when it is off.
fn end_sessions_but(now: Option<&Config>) {
    let keep = now.map(credentials);
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(_, k, s)| {
        let stays = keep.as_ref() == Some(k);
        if !stays {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        stays
    });
}

pub(crate) fn line(s: &mut TcpStream) -> io::Result<String> {
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

/// Wrong passwords in a row past which no password is checked for a day, or until the password
/// is changed: a guesser gets this many tries, not thousands.
const LOCK_AFTER: u32 = 20;
const LOCK_FOR: Duration = Duration::from_secs(24 * 3600);
/// Calls answered at once; more (a flood from a relay or someone on the path) are dropped.
const MOST_CALLS: usize = 8;

/// Wrong passwords seen lately, and one password check at a time: guesses wait their turn and
/// cost more time each, and the count fades after a quiet quarter of an hour, so wrong guesses
/// slow an attacker without locking the owner out. Many in a row stop the checking altogether.
#[derive(Default)]
struct Guard {
    failures: Mutex<(u32, Option<std::time::Instant>)>,
    /// Wrong passwords since the last right one, and when the last was.
    strikes: Mutex<(u32, Option<std::time::Instant>)>,
    turn: Mutex<()>,
    /// Calls waiting for the turn: past two, more are turned away at once, so a queue of guesses
    /// cannot keep the owner's own call waiting until it gives up.
    waiting: std::sync::atomic::AtomicUsize,
    /// Calls being answered now.
    calls: std::sync::atomic::AtomicUsize,
}

/// Wrong passwords in a row in this process's remote access, for the window.
static STRIKES: AtomicU32 = AtomicU32::new(0);

/// Whether this process's remote access stopped taking passwords after too many wrong ones.
pub fn locked() -> bool {
    STRIKES.load(Ordering::Relaxed) >= LOCK_AFTER
}

impl Guard {
    /// No password is checked: too many wrong ones in a row, not long ago.
    fn locked(&self) -> bool {
        let mut s = self.strikes.lock().unwrap_or_else(|e| e.into_inner());
        if s.1.is_some_and(|t| t.elapsed() > LOCK_FOR) {
            *s = (0, None);
            STRIKES.store(0, Ordering::Relaxed);
        }
        s.0 >= LOCK_AFTER
    }

    /// A new password: what was guessed against the old one no longer counts.
    fn reset(&self) {
        *self.strikes.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        *self.failures.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        STRIKES.store(0, Ordering::Relaxed);
    }

    fn delay(&self) -> Duration {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if f.1.is_some_and(|t| t.elapsed() > Duration::from_secs(15 * 60)) {
            *f = (0, None);
        }
        if f.0 == 0 { Duration::ZERO } else { Duration::from_secs((1u64 << f.0.min(5)).min(30)) }
    }

    fn failed(&self) {
        let now = Some(std::time::Instant::now());
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        *f = (f.0 + 1, now);
        let mut s = self.strikes.lock().unwrap_or_else(|e| e.into_inner());
        *s = (s.0 + 1, now);
        STRIKES.store(s.0, Ordering::Relaxed);
    }

    fn passed(&self) {
        *self.failures.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        *self.strikes.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        STRIKES.store(0, Ordering::Relaxed);
    }
}

/// Keeps this computer reachable by its ID until `keep` says to stop: a line to the relay, a new
/// connection for each viewer, and the agent run as a process of its own for each.
pub fn run(keep: impl Fn() -> Option<Config>) {
    // Computers on this network that ask are told this one's ID, while remote access is on and
    // that is allowed (one process answers: another that runs it too finds the port taken).
    thread::spawn(|| crate::lan::answer(|| stored().ok().flatten().filter(|c| c.enabled && c.lan).map(|c| (c.id, c.relay, c.direct))));
    let guard = Arc::new(Guard::default());
    {
        let guard = guard.clone();
        thread::spawn(move || listen_direct(guard));
    }
    let mut wait = 2;
    let mut password = None;
    while let Some(mut cfg) = keep() {
        if password.as_ref().is_some_and(|p| *p != cfg.password) {
            guard.reset();
        }
        password = Some(cfg.password.clone());
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
            let now = keep();
            // Who is asked and what viewers may do apply to the next call, with no new line.
            if let Some(k) = &now {
                (cfg.ask, cfg.allow) = (k.ask, k.allow);
                cfg.totp.clone_from(&k.totp);
            }
            if now.as_ref().is_none_or(|k| k.id != cfg.id || k.password != cfg.password || k.relay != cfg.relay) {
                let _ = s.shutdown(std::net::Shutdown::Both);
                // Viewers let in by the settings that changed go too.
                end_sessions_but(now.as_ref());
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
                    // A flood of calls is not answered past a handful at once, nor at all while
                    // no password is taken.
                    if guard.locked() {
                        continue;
                    }
                    if guard.calls.fetch_add(1, Ordering::AcqRel) >= MOST_CALLS {
                        guard.calls.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    let (cfg, nonce, guard) = (cfg.clone(), nonce.to_string(), guard.clone());
                    thread::spawn(move || {
                        answer_call(&cfg, &nonce, &guard);
                        guard.calls.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            }
        }
        set_state(State::Connecting);
        sleep_while(&keep, 1);
    }
    set_state(State::Off);
    // Off: nobody stays in.
    end_sessions_but(None);
}

fn sleep_while(keep: &impl Fn() -> Option<Config>, secs: u64) {
    for _ in 0..secs * 4 {
        if keep().is_none() {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Viewers on this network straight in, while remote access and that are on: the same password
/// check, turn and limits as through the relay.
fn listen_direct(guard: Arc<Guard>) {
    // Another process running remote access holds it already.
    let Ok(l) = std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, crate::net::DIRECT_PORT)) else { return };
    let _ = l.set_nonblocking(true);
    while let Some(cfg) = stored().ok().flatten().filter(|c| c.enabled && c.direct) {
        match l.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(false);
                if guard.locked() || guard.calls.fetch_add(1, Ordering::AcqRel) >= MOST_CALLS {
                    if !guard.locked() {
                        guard.calls.fetch_sub(1, Ordering::AcqRel);
                    }
                    continue;
                }
                let guard = guard.clone();
                thread::spawn(move || {
                    if crate::net::accept_direct(&mut s, &cfg.id) {
                        serve_call(s, &cfg, &guard);
                    }
                    guard.calls.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
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
    serve_call(s, cfg, guard);
}

/// A viewer's connection, by the relay or straight: the password checked in turn, then the agent.
fn serve_call(s: TcpStream, cfg: &Config, guard: &Guard) {
    // The exchange runs at the viewer's pace with nobody waiting on it; only checking its proof
    // of the password takes the turn, after the delay wrong ones have earned.
    let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
    let judge = |check: &mut dyn FnMut() -> bool| -> io::Result<bool> {
        if guard.locked() {
            return Err(io::Error::other("no passwords taken now"));
        }
        if guard.waiting.fetch_add(1, Ordering::AcqRel) >= 2 {
            guard.waiting.fetch_sub(1, Ordering::AcqRel);
            return Err(io::Error::other("busy"));
        }
        let _turn = guard.turn.lock().unwrap_or_else(|e| e.into_inner());
        guard.waiting.fetch_sub(1, Ordering::AcqRel);
        thread::sleep(guard.delay());
        let right = check();
        if right { guard.passed() } else { guard.failed() }
        Ok(right)
    };
    let totp = (!cfg.totp.is_empty()).then_some(cfg.totp.as_str());
    let Ok((r, w)) = crate::net::handshake_agent_2fa(s, &cfg.password, totp, &judge) else { return };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    if let Ok(socket) = w.stream() {
        SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).push((n, credentials(cfg), socket));
        SINCE.lock().unwrap_or_else(|e| e.into_inner()).push((n, crate::sys::now_secs()));
    }
    VIEWERS.fetch_add(1, Ordering::Relaxed);
    let _ = serve_viewer(r, w, cfg.grant());
    VIEWERS.fetch_sub(1, Ordering::Relaxed);
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _, _)| *k != n);
    SINCE.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _)| *k != n);
}

/// The agent as a process of its own, its input and output the sealed stream.
fn serve_viewer(mut r: crate::net::SealedReader, mut w: crate::net::SealedWriter, grant: u8) -> Result<(), String> {
    // The session's own pings come every second; two minutes of nothing means it is gone.
    let socket = w.stream().map_err(|e| e.to_string())?;
    let _ = socket.set_read_timeout(Some(Duration::from_secs(120)));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(["--agent", "--capture", "auto", "--by-id", "--grant", &grant.to_string()]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    crate::sys::tie(&child);
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
        return crate::admin::service_installed();
    }
    #[cfg(unix)]
    {
        return unit_file().is_some_and(|f| f.exists());
    }
    #[allow(unreachable_code)]
    false
}

/// Whether the installed service runs this very program (an older copy lacks what this has). On
/// Windows it runs its own copy under Program Files, the same as this one when size and time match.
pub fn service_is_this() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let Ok(o) = Command::new("sc").args(["qc", crate::service::NAME]).creation_flags(0x0800_0000).output() else { return true };
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        // BINARY_PATH_NAME   : "C:\Program Files\SSH Remote\sshremote.exe" --service
        let Some(path) = text.lines().find(|l| l.contains("BINARY_PATH_NAME")).and_then(|l| l.split('"').nth(1)) else { return false };
        let stamp = |p: &std::path::Path| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok()));
        return std::env::current_exe().ok().and_then(|e| stamp(&e)).is_some_and(|here| stamp(std::path::Path::new(path)) == Some(here));
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

/// Starts or stops the service that runs remote access with the computer, without installing or
/// removing it (on Windows through the UAC prompt, unless the rights are held).
pub fn service_control(start: bool) -> Result<(), String> {
    let verb = if start { "start" } else { "stop" };
    #[cfg(windows)]
    {
        let args = [verb.to_string(), crate::service::NAME.to_string()];
        if crate::admin::elevated() {
            let s = Command::new("sc.exe").args(&args).output().map_err(|e| e.to_string())?;
            // 1056: already running; 1062: not running. Either way it is as asked.
            let code = s.status.code().unwrap_or(-1);
            return if s.status.success() || code == 1056 || code == 1062 { Ok(()) } else { Err(format!("sc {verb}: {}", String::from_utf8_lossy(&s.stdout).trim())) };
        }
        return match crate::admin::run_elevated("sc.exe", &args, true)? {
            true => Ok(()),
            false => Err("administrator rights were not given".into()),
        };
    }
    #[cfg(unix)]
    {
        let ok = Command::new("systemctl").args(["--user", verb, UNIT]).status().map_err(|e| e.to_string())?;
        return if ok.success() { Ok(()) } else { Err(format!("systemctl --user {verb} {UNIT} failed")) };
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
        println!("ID:       {}", crate::computers::spaced(&c.id));
        println!("password: {}", c.password);
        println!("relay:    {}", c.relay);
        println!("on:       {}", if c.enabled { "yes" } else { "no" });
        println!("asks:     {}", if c.ask { "yes, the person at this computer lets each viewer in" } else { "no, the password is enough" });
        println!("viewers:  may use {}", crate::grant::words(c.allow));
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
            println!("ID: {}", crate::computers::spaced(&c.id));
            Ok(())
        }
        Some("ask") => {
            c.ask = match args.get(1).map(String::as_str) {
                Some("on") => true,
                Some("off") => false,
                _ => return Err("usage: sshremote remote-access ask on|off".into()),
            };
            save(&c)?;
            show(&c);
            Ok(())
        }
        // The rights by their short names, or "all" or "none".
        Some("allow") => {
            let names: Vec<&str> = crate::grant::RIGHTS.iter().map(|(.., w)| w.split_whitespace().next().unwrap_or("")).collect();
            let mut bits = 0;
            for a in args[1..].iter().flat_map(|a| a.split(',')).map(str::trim).filter(|a| !a.is_empty()) {
                bits |= match a {
                    "all" => crate::grant::ALL,
                    "none" => 0,
                    _ => match names.iter().position(|n| *n == a) {
                        Some(i) => crate::grant::RIGHTS[i].0,
                        None => return Err(format!("usage: sshremote remote-access allow [all | none | {}]...", names.join(" | "))),
                    },
                };
            }
            c.allow = bits;
            save(&c)?;
            show(&c);
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
            // Older units lack `--service`: systemd names the process it started itself (248+).
            let unit_started = std::env::var("SYSTEMD_EXEC_PID").is_ok_and(|p| p == std::process::id().to_string());
            let service = args.iter().any(|a| a == "--service") || unit_started;
            let parent: Option<u32> = args.iter().position(|a| a == "--parent").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok());
            eprintln!("remote access as {} through {}", crate::computers::spaced(&c.id), c.relay);
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
        _ => Err("usage: sshremote remote-access [status | on | off | run | id | password [NEW] | relay HOST | ask on|off | allow RIGHTS | install | uninstall]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twenty_wrong_passwords_in_a_row_stop_the_checking() {
        let g = Guard::default();
        for _ in 0..LOCK_AFTER - 1 {
            g.failed();
        }
        assert!(!g.locked(), "not yet");
        g.passed();
        for _ in 0..LOCK_AFTER - 1 {
            g.failed();
        }
        assert!(!g.locked(), "a right password starts the count again");
        g.failed();
        assert!(g.locked() && locked(), "{LOCK_AFTER} in a row");
        // A day later, or a new password, opens it again.
        // (A day back exists only on a computer up for longer than that.)
        if let Some(day) = std::time::Instant::now().checked_sub(LOCK_FOR + Duration::from_secs(1)) {
            g.strikes.lock().unwrap().1 = Some(day);
            assert!(!g.locked(), "a day on");
        }
        for _ in 0..LOCK_AFTER {
            g.failed();
        }
        g.reset();
        assert!(!g.locked() && !locked(), "a new password");
    }

    #[test]
    fn ids_passwords_and_addresses() {
        let id = new_id();
        assert!(valid_id(&id) && id.len() == 9 && !id.starts_with('0'));
        assert_eq!(crate::computers::spaced("123456789"), "123 456 789");
        assert_eq!(new_password().len(), 8);
        assert_eq!(relay_addr("r.example"), ("r.example".into(), crate::net::RELAY_PORT));
        assert_eq!(relay_addr("r.example:9000"), ("r.example".into(), 9000));
        assert_eq!(address("123 456 789", "r.example"), "sshremote://123456789@r.example");
        assert!(!valid_id("12a456789") && !valid_id("123"));
    }

    #[test]
    fn settings_are_kept() {
        let d = std::env::temp_dir().join(format!("sshremote-access-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        TEST_DIR.with(|t| *t.borrow_mut() = Some(d.clone()));
        let mut c = load();
        assert!(!c.ask && c.allow == crate::grant::ALL, "the password is enough, for everything, until set otherwise");
        c.enabled = true;
        (c.ask, c.allow) = (true, crate::grant::CONTROL | crate::grant::FILES);
        save(&c).unwrap();
        assert_eq!(load(), c);
        assert_eq!(c.grant(), crate::grant::ASK | crate::grant::CONTROL | crate::grant::FILES);
        c.password = "abc".into();
        assert!(save(&c).is_err(), "too short");
        TEST_DIR.with(|t| *t.borrow_mut() = None);
    }
}
