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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::sys::{hex, random};

/// The relay used when none is set.
pub const DEFAULT_RELAY: &str = crate::computers::RELAY;

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
    /// Whether a viewer that came through the relay may move to a straight path through the
    /// routers, once in (`udp`): the two then learn each other's addresses.
    pub punch: bool,
    /// Two-step sign-in: the authenticator app's secret (base32); empty: the password is enough.
    pub totp: String,
    /// Which passwords viewers may give: the lasting one, a one-time one, or either (`Passwords`).
    pub passwords: Passwords,
    /// The one-time password now: shown here, and new a minute after each connection with it ends.
    pub one_time: String,
    /// Whether someone without a password may ask to come in: the person at the screen then
    /// decides, comparing the code both screens show.
    pub requests: bool,
    /// Where viewers may come from: addresses and ranges ("203.0.113.7 192.168.1.0/24");
    /// empty: anywhere. Through the relay, it says where it sees each viewer from.
    pub from: String,
    /// A relay of one's own and its lasting key ("RELAY HEX"), as first seen: the line to it is
    /// sealed with that key from then on (`relaylink`; the public relay's is known).
    pub relay_key: String,
    /// Whether viewers may look at this computer's cameras (listed after its monitors); off
    /// unless chosen, and the person at the screen is told each time.
    pub cameras: bool,
    /// Minutes without input from a viewer after which its session ends; 0: never.
    pub idle: u32,
}

/// Which passwords remote access takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Passwords {
    /// Only the lasting password (the one before one-time passwords, and still the default for it).
    Lasting,
    /// Only the one-time password: nobody comes back later with what they were told once.
    OneTime,
    /// Either.
    Both,
}

impl Passwords {
    pub const ALL: [Passwords; 3] = [Passwords::Lasting, Passwords::OneTime, Passwords::Both];

    pub fn word(self) -> &'static str {
        match self {
            Passwords::Lasting => "lasting",
            Passwords::OneTime => "one-time",
            Passwords::Both => "both",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Passwords::Lasting => "Lasting",
            Passwords::OneTime => "One-time",
            Passwords::Both => "Both",
        }
    }

    fn from_word(w: &str) -> Option<Passwords> {
        Passwords::ALL.into_iter().find(|p| p.word() == w)
    }

    pub fn lasting(self) -> bool {
        self != Passwords::OneTime
    }

    pub fn one_time(self) -> bool {
        self != Passwords::Lasting
    }
}

impl Config {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "id": self.id, "token": self.token, "password": self.password, "relay": self.relay, "enabled": self.enabled, "ask": self.ask, "allow": self.allow, "lan": self.lan, "direct": self.direct, "punch": self.punch, "totp": self.totp, "passwords": self.passwords.word(), "one_time": self.one_time, "requests": self.requests, "from": self.from, "relay_key": self.relay_key, "cameras": self.cameras, "idle": self.idle })
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
            relay: s("relay").filter(|r| !r.trim().is_empty()).map(|r| crate::computers::relay_name(&r)).unwrap_or_else(default_relay),
            enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false),
            ask: v.get("ask").and_then(|x| x.as_bool()).unwrap_or(false),
            allow: v.get("allow").and_then(|x| x.as_u64()).map_or(crate::grant::ALL, |a| a as u8 & crate::grant::ALL),
            lan: v.get("lan").and_then(|x| x.as_bool()).unwrap_or(true),
            // Off unless chosen, as a port open to the network is.
            direct: v.get("direct").and_then(|x| x.as_bool()).unwrap_or(false),
            // On, as with other remote desktops: it moves only sealed traffic, after the password.
            punch: v.get("punch").and_then(|x| x.as_bool()).unwrap_or(true),
            totp: s("totp").unwrap_or_default(),
            // Settings from before one-time passwords keep taking the lasting one alone.
            passwords: s("passwords").and_then(|w| Passwords::from_word(&w)).unwrap_or(Passwords::Lasting),
            one_time: s("one_time").filter(|p| crate::net::looks_one_time(p)).unwrap_or_else(crate::net::new_one_time),
            // Off unless chosen: anyone with the ID could ask.
            requests: v.get("requests").and_then(|x| x.as_bool()).unwrap_or(false),
            from: s("from").unwrap_or_default(),
            relay_key: s("relay_key").unwrap_or_default(),
            cameras: v.get("cameras").and_then(|x| x.as_bool()).unwrap_or(false),
            idle: v.get("idle").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
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
    Config {
        id: new_id(),
        token: hex(&random(16)),
        password: new_password(),
        relay: default_relay(),
        enabled: false,
        ask: false,
        allow: crate::grant::ALL,
        lan: true,
        direct: false,
        punch: true,
        totp: String::new(),
        // As other remote desktops start: a one-time password beside the lasting one.
        passwords: Passwords::Both,
        one_time: crate::net::new_one_time(),
        requests: false,
        from: String::new(),
        relay_key: String::new(),
        cameras: false,
        idle: 0,
    }
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
    format!("sshremote://{}@{}", id.trim().replace(' ', ""), crate::computers::relay_name(relay))
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

/// The viewers being served: a number, what let them in, and their connection, so a change of
/// those settings (off, a new password, ID or relay, requests or that password kind off, their
/// address no longer listed) ends them too.
static SESSIONS: Mutex<Vec<(u64, Welcome, crate::net::Ender)>> = Mutex::new(Vec::new());

/// When each viewer came in, by its number, in seconds since 1970.
static SINCE: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// What remote access shows to anyone at this computer, its secrets left out: for the icon in the
/// notification area, which runs without the rights to read the settings (`service`).
pub fn public_state() -> serde_json::Value {
    let c = load();
    let state = match state() {
        State::Off => "off".to_string(),
        State::Connecting => "connecting".to_string(),
        State::Online => "online".to_string(),
        State::InUse(n) => format!("in use by {n}"),
        State::Failed(why) => why,
    };
    serde_json::json!({
        "enabled": c.enabled, "id": c.id, "relay": c.relay, "ask": c.ask, "allow": c.allow, "punch": c.punch,
        "lan": c.lan, "direct": c.direct, "two_step": !c.totp.is_empty(), "passwords": c.passwords.word(), "requests": c.requests, "state": state, "viewers": viewers().iter().map(Viewer::json).collect::<Vec<_>>(),
    })
}

/// Where remote access run by the service says how it stands (`public_state`), for the icon in
/// the notification area: under ProgramData on Windows (every user reads it, only the service
/// writes it), in the user's runtime folder elsewhere.
pub fn state_file() -> Option<PathBuf> {
    #[cfg(windows)]
    return Some(PathBuf::from(std::env::var_os("ProgramData")?).join("SSH Remote").join("state.json"));
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).or_else(crate::hosts::config_dir)?;
        Some(base.join("sshremote-state.json"))
    }
}

/// Writes `public_state` to `state_file` when it is not what `said` holds.
pub fn publish_state(said: &mut String) {
    let now = public_state().to_string();
    if now == *said {
        return;
    }
    let Some(f) = state_file() else { return };
    #[cfg(windows)]
    if !shared_folder_ready() {
        return;
    }
    #[cfg(not(windows))]
    let _ = f.parent().map(std::fs::create_dir_all);
    let tmp = f.with_extension("new");
    if std::fs::write(&tmp, &now).and_then(|_| std::fs::rename(&tmp, &f)).is_ok() {
        *said = now;
    }
}

/// How many viewers this program serves by ID now (remote access run inside the app).
pub fn viewers_here() -> usize {
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// A viewer connected by the ID, as anyone at this computer may see it: its number, when it came
/// in (seconds since 1970), and where it is when its session went straight past the relay.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewer {
    pub n: u64,
    pub since: u64,
    pub straight: Option<String>,
}

impl Viewer {
    /// "connected 5 min ago, through the relay", or "..., straight from 203.0.113.7".
    pub fn describe(&self) -> String {
        let how = match &self.straight {
            Some(a) => format!("straight from {}", a.rsplit_once(':').map_or(a.as_str(), |(h, _)| h).trim_matches(['[', ']'])),
            None => "through the relay".to_string(),
        };
        format!("connected {}, {how}", crate::hosts::ago(self.since))
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "n": self.n, "since": self.since, "straight": self.straight })
    }

    fn from_json(v: &serde_json::Value) -> Option<Viewer> {
        Some(Viewer { n: v.get("n")?.as_u64()?, since: v.get("since").and_then(|x| x.as_u64()).unwrap_or(0), straight: v.get("straight").and_then(|x| x.as_str()).map(str::to_string) })
    }
}

/// The viewers this program serves by ID now.
pub fn viewers() -> Vec<Viewer> {
    let since = SINCE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(n, _, e)| Viewer { n: *n, since: since.iter().find(|s| s.0 == *n).map_or(0, |s| s.1), straight: e.straight().map(|a| a.to_string()) })
        .collect()
}

/// The viewers the service serves, as it last said (`state_file`).
pub fn service_viewers() -> Vec<Viewer> {
    let v: Option<serde_json::Value> = state_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str(&t).ok());
    v.and_then(|v| v.get("viewers").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(Viewer::from_json).collect())).unwrap_or_default()
}

/// Who came in by the ID, from where, how and for how long, and who was turned away: one JSON
/// object a line. The service keeps its own beside its state, which every user reads and only it
/// writes; remote access run in the window keeps the user's.
fn visits_written() -> Option<PathBuf> {
    #[cfg(windows)]
    if crate::console_user::running_as_system() {
        return state_file().map(|f| f.with_file_name("visits.txt"));
    }
    crate::hosts::config_dir().map(|d| d.join("visits.txt"))
}

fn visit_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    out.extend(state_file().map(|f| f.with_file_name("visits.txt")));
    out.extend(crate::hosts::config_dir().map(|d| d.join("visits.txt")));
    out
}

/// Past this the older half of the record goes.
const VISITS_MOST: u64 = 256 << 10;

fn note_visit(v: serde_json::Value) {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(f) = visits_written() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::metadata(&f).is_ok_and(|m| m.len() > VISITS_MOST)
        && let Ok(t) = std::fs::read_to_string(&f)
    {
        let rest = &t[t.len() / 2..];
        let _ = std::fs::write(&f, rest.find('\n').map_or("", |i| &rest[i + 1..]));
    }
    if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(&f) {
        let _ = writeln!(out, "{v}");
    }
}

/// A viewer by the ID as the record has it: let in (and for how long, once gone), or turned away.
#[derive(Clone, Debug, PartialEq)]
pub struct Visit {
    pub at: u64,
    pub from: Option<String>,
    /// How it was let in, or why it was turned away.
    pub how: String,
    pub let_in: bool,
    /// Through the relay, straight from an address, or on this network; and for how long.
    pub path: String,
    pub secs: Option<u64>,
}

impl Visit {
    /// "12 min ago from 203.0.113.7: let in with the lasting password, straight, for 4 min".
    pub fn describe(&self) -> String {
        let from = self.from.as_deref().map(|f| format!(" from {f}")).unwrap_or_default();
        if !self.let_in {
            return format!("{}{from}: turned away, {}", crate::hosts::ago(self.at), self.how);
        }
        let stay = match self.secs {
            Some(s) if s < 60 => format!(", for {s} s"),
            Some(s) if s < 3600 => format!(", for {} min", s / 60),
            Some(s) => format!(", for {} h {} min", s / 3600, s % 3600 / 60),
            None => ", still here or cut off".into(),
        };
        format!("{}{from}: let in {}, {}{stay}", crate::hosts::ago(self.at), self.how, self.path)
    }
}

/// The latest `most` visits, newest first, from the service's record and the user's.
pub fn visits(most: usize) -> Vec<Visit> {
    let mut out: Vec<Visit> = Vec::new();
    for f in visit_files() {
        if let Ok(text) = std::fs::read_to_string(f) {
            read_visits(&text, &mut out);
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(v.at));
    out.truncate(most);
    out
}

/// The visits in one record: a viewer let in is its "in" line with the "out" line of the same
/// session (number and start; numbers start again with the program), if it ended.
fn read_visits(text: &str, out: &mut Vec<Visit>) {
    {
        let lines: Vec<serde_json::Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        let u = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_u64());
        for v in &lines {
            let at = u(v, "at").unwrap_or(0);
            if let Some(why) = s(v, "refused") {
                out.push(Visit { at, from: s(v, "from"), how: why, let_in: false, path: String::new(), secs: None });
            } else if let Some(n) = u(v, "in") {
                let gone = lines.iter().find(|o| u(o, "out") == Some(n) && u(o, "at") == Some(at));
                let path = gone.and_then(|o| s(o, "path")).or_else(|| s(v, "path")).unwrap_or_default();
                out.push(Visit { at, from: s(v, "from"), how: s(v, "how").unwrap_or_default(), let_in: true, path, secs: gone.and_then(|o| u(o, "secs")) });
            }
        }
    }
}

/// Where the window and the icon ask the service to end a viewer's connection: one number a line.
fn end_file() -> Option<PathBuf> {
    state_file().map(|f| f.with_file_name(if cfg!(windows) { "end.txt" } else { "sshremote-end.txt" }))
}

/// Asks the service to end viewer `n`'s connection (within a second or two).
pub fn ask_to_end(n: u64) -> Result<(), String> {
    let f = end_file().ok_or("no folder for the request")?;
    let failed = |e: io::Error| format!("{}: {e}", f.display());
    // Windows: the service made the file, and lets anyone add to it and do nothing else; while
    // it takes what is there, the file is its alone for a moment.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const APPEND_ONLY: u32 = 0x0010_0004;
        for _ in 0..40 {
            match std::fs::OpenOptions::new().access_mode(APPEND_ONLY).open(&f) {
                Ok(mut o) => return o.write_all(format!("{n}\n").as_bytes()).map_err(failed),
                Err(e) if e.raw_os_error() == Some(32) => thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(failed(e)),
            }
        }
        Err(format!("{}: in use", f.display()))
    }
    #[cfg(not(windows))]
    {
        let mut o = std::fs::OpenOptions::new().create(true).append(true).open(&f).map_err(failed)?;
        writeln!(o, "{n}").map_err(|e| e.to_string())
    }
}

/// Ends the connections the window or the icon asked to end since last time (the service). At
/// most a few kilobytes are read, whatever was written there.
pub fn take_end_requests() {
    let Some(f) = end_file() else { return };
    let mut got = Vec::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE};
        if !shared_folder_ready() {
            return;
        }
        // Others may hold it open to add to it; that must not keep requests from being read.
        let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&f)
        else {
            return;
        };
        if !file.metadata().is_ok_and(|m| m.is_file()) {
            return;
        }
        let _ = (&mut file).take(4096).read_to_end(&mut got);
        if got.is_empty() {
            return;
        }
        let _ = file.set_len(0);
    }
    #[cfg(not(windows))]
    {
        let Ok(file) = std::fs::File::open(&f) else { return };
        let _ = file.take(4096).read_to_end(&mut got);
        let _ = std::fs::remove_file(&f);
    }
    for n in String::from_utf8_lossy(&got).lines().filter_map(|l| l.trim().parse::<u64>().ok()) {
        disconnect(n);
    }
}

/// The service's folder under ProgramData, made safe to share (Windows): made by the service
/// itself, owned by SYSTEM, written by it and administrators alone, read by everyone; and in it
/// `end.txt`, which anyone may add to and nothing more. Without this a user could own the folder,
/// or files or links in it, that SYSTEM then writes, empties and removes. What was there before
/// (made by whoever, or a link elsewhere) is removed first, and nothing goes on if it cannot be.
#[cfg(windows)]
pub fn secure_shared_folder() -> Result<(), String> {
    let f = state_file().ok_or("no ProgramData folder")?;
    let dir = f.parent().ok_or("no folder")?.to_path_buf();
    let gone = |e: io::Error| format!("{} could not be removed: {e}", dir.display());
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => std::fs::remove_dir(&dir).or_else(|_| std::fs::remove_file(&dir)).map_err(gone)?,
        Ok(_) => std::fs::remove_dir_all(&dir).map_err(gone)?,
        Err(_) => {}
    }
    // Made with its owner and permissions at once: nobody else can take it in between.
    win_create(&dir, true, "O:SYG:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)")?;
    if !win_folder_is_safe(&dir) {
        return Err(format!("{} is not the folder just made", dir.display()));
    }
    make_end_file()
}

/// `end.txt` in the safe folder, made anew: anyone may add to it and do nothing else.
#[cfg(windows)]
fn make_end_file() -> Result<(), String> {
    let end = end_file().ok_or("no folder")?;
    win_create(&end, false, "O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x100004;;;BU)")
}

/// Whether the shared folder is safe to use, made so when it is not (tried again on each call
/// until it is): the service touches nothing in it before.
#[cfg(windows)]
fn shared_folder_ready() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static READY: AtomicBool = AtomicBool::new(false);
    if READY.load(Ordering::Acquire) {
        return true;
    }
    let Some(dir) = state_file().and_then(|f| f.parent().map(|d| d.to_path_buf())) else { return false };
    let ok = if win_folder_is_safe(&dir) {
        end_file().is_some_and(|e| e.is_file()) || make_end_file().is_ok()
    } else {
        secure_shared_folder().is_ok()
    };
    READY.store(ok, Ordering::Release);
    ok
}

/// Makes a folder (or an empty file) with this security descriptor; fails if anything is there.
#[cfg(windows)]
fn win_create(path: &std::path::Path, folder: bool, sddl: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL};
    let (sddl, p) = (crate::sys::wide(sddl), crate::sys::wide(&path.to_string_lossy()));
    let mut sd = std::ptr::null_mut();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
        return Err("the permissions could not be made".into());
    }
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
    let ok = unsafe {
        if folder {
            CreateDirectoryW(p.as_ptr(), &sa) != 0
        } else {
            let h = CreateFileW(p.as_ptr(), GENERIC_WRITE, 0, &sa, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
            h != INVALID_HANDLE_VALUE && CloseHandle(h) != 0
        }
    };
    let err = io::Error::last_os_error();
    unsafe { LocalFree(sd as _) };
    if ok { Ok(()) } else { Err(format!("{}: {err}", path.display())) }
}

/// Whether `dir` itself (not where a link would lead) is a folder owned by SYSTEM or the
/// administrators whose permissions are its own (protected, as this service makes them).
#[cfg(windows)]
fn win_folder_is_safe(dir: &std::path::Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, IsWellKnownSid, OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, WinBuiltinAdministratorsSid,
        WinLocalSystemSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileInformationByHandle,
    };
    const READ_CONTROL: u32 = 0x0002_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    let Ok(h) = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)
    else {
        return false;
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h.as_raw_handle(), &mut info) } == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return false;
    }
    let (mut owner, mut dacl, mut sd) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
    let r = unsafe {
        GetSecurityInfo(h.as_raw_handle(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &mut owner, std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd)
    };
    if r != 0 {
        return false;
    }
    let (mut control, mut rev) = (0u16, 0u32);
    let ok = unsafe {
        (IsWellKnownSid(owner, WinLocalSystemSid) != 0 || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0)
            && GetSecurityDescriptorControl(sd, &mut control, &mut rev) != 0
            && control & SE_DACL_PROTECTED != 0
    };
    unsafe { LocalFree(sd as _) };
    ok
}

/// Ends one viewer's connection (the person at this screen chose to).
pub fn disconnect(viewer: u64) {
    for (n, _, s) in SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        if *n == viewer {
            s.end();
        }
    }
}

/// What let a viewer in: the ID at that relay, the password it gave (or that it asked), and
/// where it came from.
struct Welcome {
    id: String,
    relay: String,
    came: Came,
    from: Option<std::net::IpAddr>,
}

enum Came {
    Lasting(String),
    OneTime(String),
    Request,
}

impl Welcome {
    fn new(c: &Config, came: &crate::net::Came, from: Option<std::net::IpAddr>) -> Welcome {
        let came = match came {
            crate::net::Came::Lasting => Came::Lasting(c.password.clone()),
            crate::net::Came::OneTime => Came::OneTime(c.one_time.clone()),
            crate::net::Came::Request(_) => Came::Request,
        };
        Welcome { id: c.id.clone(), relay: c.relay.clone(), came, from }
    }

    /// Whether the settings `c` would still let it in.
    fn still(&self, c: &Config) -> bool {
        c.enabled
            && c.id == self.id
            && c.relay == self.relay
            && comes_from(&c.from, self.from)
            && match &self.came {
                Came::Lasting(p) => c.passwords.lasting() && *p == c.password,
                Came::OneTime(p) => c.passwords.one_time() && *p == c.one_time,
                Came::Request => c.requests,
            }
    }
}

/// Ends every viewer the settings `now` would not let in; all of them when it is off.
fn end_sessions_but(now: Option<&Config>) {
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(_, w, s)| {
        let stays = now.is_some_and(|c| w.still(c));
        if !stays {
            s.end();
        }
        stays
    });
}

pub(crate) fn line(s: &mut impl Read) -> io::Result<String> {
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
        let mut s = match sealed_line(&mut cfg) {
            Ok(s) => s,
            Err(e) => {
                set_state(State::Failed(e));
                sleep_while(&keep, wait);
                wait = (wait * 2).min(60);
                continue;
            }
        };
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
        let mut seen = std::collections::HashMap::new();
        'line: loop {
            let now = keep();
            // Who is asked, what viewers may do and whether they may go straight apply to the next
            // call, with no new line.
            if let Some(k) = &now {
                (cfg.ask, cfg.allow, cfg.punch, cfg.passwords, cfg.requests) = (k.ask, k.allow, k.punch, k.passwords, k.requests);
                cfg.totp.clone_from(&k.totp);
                cfg.one_time.clone_from(&k.one_time);
                cfg.from.clone_from(&k.from);
                // Those the new settings would not let in go now.
                end_sessions_but(Some(k));
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
                } else if let Some((nonce, ip)) = l.strip_prefix("from ").and_then(|r| r.split_once(' ')) {
                    // Where the relay sees the viewer of the next call from.
                    if seen.len() >= 64 {
                        seen.clear();
                    }
                    if let Ok(ip) = ip.parse::<std::net::IpAddr>() {
                        seen.insert(nonce.to_string(), ip);
                    }
                } else if let Some(nonce) = l.strip_prefix("call ") {
                    let from = seen.remove(nonce);
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
                        answer_call(&cfg, &nonce, &guard, from);
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

/// The relay's lasting key this computer knows it by: the public relay's, or one of its own's as
/// first seen.
fn known_key(cfg: &Config) -> Option<[u8; 32]> {
    let relay = crate::computers::relay_name(&cfg.relay);
    if relay.eq_ignore_ascii_case(crate::computers::RELAY) {
        return crate::relaylink::unhex(crate::relaylink::PUBLIC_RELAY_KEY);
    }
    let (r, k) = cfg.relay_key.split_once(' ')?;
    (r == relay).then(|| crate::relaylink::unhex(k)).flatten()
}

/// The line to the relay, sealed: with the key it is known by, or (a relay of one's own seen
/// for the first time) the key it shows, kept. A relay before 1.3 gets the plain line, unless
/// it was known sealed: then someone in between is refusing the seal, and nothing is said.
fn sealed_line(cfg: &mut Config) -> Result<crate::relaylink::Link, String> {
    use crate::relaylink::{Link, Outcome};
    let reach = |relay: &str| -> Result<TcpStream, String> {
        let s = dial(relay).map_err(|e| format!("Could not reach {e}"))?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
        Ok(s)
    };
    let known = known_key(cfg);
    match Link::seal(reach(&cfg.relay)?, known) {
        Ok(Outcome::Sealed(link, lasting)) => {
            if known.is_none() {
                cfg.relay_key = format!("{} {}", crate::computers::relay_name(&cfg.relay), hex(&lasting));
                let _ = save(cfg);
            }
            Ok(link)
        }
        Ok(Outcome::Older) if known.is_none() => Ok(Link::plain(reach(&cfg.relay)?)),
        Ok(Outcome::Older) => Err("The relay did not seal the line as it always has: someone between may be in the way".into()),
        Err(e) => Err(format!("The relay could not be trusted: {e}")),
    }
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
                        let from = s.peer_addr().ok().map(|a| a.ip());
                        serve_call(s, &cfg, &guard, false, from);
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
fn answer_call(cfg: &Config, nonce: &str, guard: &Guard, from: Option<std::net::IpAddr>) {
    let Ok(mut s) = dial(&cfg.relay) else { return };
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    if writeln!(s, "accept {} {nonce}", cfg.id).is_err() || !line(&mut s).is_ok_and(|l| l == "ok") {
        return;
    }
    serve_call(s, cfg, guard, true, from);
}

/// A viewer's connection, by the relay or straight: the password checked in turn, then the agent.
/// Through the relay, it may move to a straight path through the routers. `from`: where the
/// viewer is, as far as is known.
fn serve_call(mut s: TcpStream, cfg: &Config, guard: &Guard, relayed: bool, from: Option<std::net::IpAddr>) {
    let from_text = from.map(|a| a.to_canonical().to_string());
    if !comes_from(&cfg.from, from) {
        note_visit(serde_json::json!({ "refused": "its address is not on the list", "at": crate::sys::now_secs(), "from": from_text }));
        crate::net::turn_away_address(&mut s);
        return;
    }
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
        if right {
            guard.passed()
        } else {
            guard.failed();
            note_visit(serde_json::json!({ "refused": "a wrong password", "at": crate::sys::now_secs(), "from": from_text }));
        }
        Ok(right)
    };
    let totp = (!cfg.totp.is_empty()).then_some(cfg.totp.as_str());
    let punch = (relayed && cfg.punch).then(|| crate::net::Punch { viewer: false, relay: cfg.relay.clone(), id: String::new() });
    let lasting = cfg.passwords.lasting().then_some(cfg.password.as_str());
    let one_time = cfg.passwords.one_time().then_some(cfg.one_time.as_str()).filter(|p| !p.is_empty());
    // A request without a password: one at a time, and a breath after the last, so nobody can
    // fill the screen with them.
    let took = std::cell::Cell::new(None);
    let take_request = || {
        let quiet = REQUEST_ENDED.lock().unwrap_or_else(|e| e.into_inner()).is_none_or(|t| t.elapsed() > REQUEST_BREATH);
        let mut slot = REQUEST_IN.lock().unwrap_or_else(|e| e.into_inner());
        let ok = quiet && slot.is_none_or(|t| t.elapsed() > REQUEST_HOLD);
        if ok {
            let now = std::time::Instant::now();
            *slot = Some(now);
            took.set(Some(now));
        }
        ok
    };
    let requests: Option<&dyn Fn() -> bool> = cfg.requests.then_some(&take_request as &dyn Fn() -> bool);
    let Ok((r, w, came)) = crate::net::handshake_agent_passwords(s, lasting, one_time, requests, totp, &judge, punch) else {
        // Taken, then lost before the end of the exchange (or stopped short of showing its
        // part): the next request may come, after the same breath.
        request_done(took.get());
        return;
    };
    let once = came == crate::net::Came::OneTime;
    if once {
        ONE_TIME_IN.fetch_add(1, Ordering::AcqRel);
    }
    // Someone who asked is always let in by a click, which the code backs.
    let code = if let crate::net::Came::Request(c) = &came { Some(c.replace(' ', "")) } else { None };
    struct Request(Option<std::time::Instant>);
    impl Drop for Request {
        fn drop(&mut self) {
            request_done(self.0);
        }
    }
    let _request = Request(took.get());
    let grant = if code.is_some() { cfg.grant() | crate::grant::ASK } else { cfg.grant() };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let how = match &came {
        crate::net::Came::Lasting => "with the lasting password",
        crate::net::Came::OneTime => "with a one-time password",
        crate::net::Came::Request(_) => "by asking, at the screen",
    };
    let how = if cfg.totp.is_empty() { how.to_string() } else { format!("{how} and the two-step code") };
    let (at, ender) = (crate::sys::now_secs(), w.ender());
    note_visit(serde_json::json!({ "in": n, "at": at, "from": from_text, "how": how, "path": if relayed { "through the relay" } else { "on this network" } }));
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).push((n, Welcome::new(cfg, &came, from), w.ender()));
    SINCE.lock().unwrap_or_else(|e| e.into_inner()).push((n, at));
    VIEWERS.fetch_add(1, Ordering::Relaxed);
    let _ = serve_viewer(r, w, grant, code.as_deref(), cfg.cameras, cfg.idle);
    VIEWERS.fetch_sub(1, Ordering::Relaxed);
    let path = match ender.straight() {
        Some(a) => format!("straight from {}", a.ip()),
        None if relayed => "through the relay".into(),
        None => "on this network".into(),
    };
    note_visit(serde_json::json!({ "out": n, "at": at, "secs": crate::sys::now_secs().saturating_sub(at), "path": path }));
    if once {
        one_time_used(&cfg.one_time);
    }
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _, _)| *k != n);
    SINCE.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _)| *k != n);
}

/// One address or range of `Config::from`: the address and how many of its first bits count.
fn range(word: &str) -> Option<(std::net::IpAddr, u32)> {
    let (a, bits) = match word.split_once('/') {
        Some((a, b)) => (a, Some(b.parse::<u32>().ok()?)),
        None => (word, None),
    };
    let a = a.parse::<std::net::IpAddr>().ok()?.to_canonical();
    let most = if a.is_ipv4() { 32 } else { 128 };
    let bits = bits.unwrap_or(most);
    (bits <= most).then_some((a, bits))
}

fn words(from: &str) -> impl Iterator<Item = &str> {
    from.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|w| !w.is_empty())
}

/// The words of an address list that are not an address or a range.
pub fn bad_ranges(from: &str) -> Vec<String> {
    words(from).filter(|w| range(w).is_none()).map(str::to_string).collect()
}

/// An address list as kept: one space between its words.
pub fn tidy_ranges(from: &str) -> String {
    words(from).collect::<Vec<_>>().join(" ")
}

/// Whether a viewer at `ip` may come in, by the list (empty: anyone). Where it is not known (an
/// older relay does not say), nobody comes in past a list.
pub fn comes_from(from: &str, ip: Option<std::net::IpAddr>) -> bool {
    use std::net::IpAddr;
    let mut list = words(from).peekable();
    if list.peek().is_none() {
        return true;
    }
    let Some(ip) = ip.map(|i| i.to_canonical()) else { return false };
    list.filter_map(range).any(|(a, bits)| match (a, ip) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let m = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(a) & m == u32::from(b) & m
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let m = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(a) & m == u128::from(b) & m
        }
        _ => false,
    })
}

/// When the request without a password being answered was taken (it holds the turn while the
/// person at the screen decides, at most `REQUEST_HOLD`); when the last one ended, and how long
/// after it the next is taken.
static REQUEST_IN: Mutex<Option<std::time::Instant>> = Mutex::new(None);
static REQUEST_ENDED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const REQUEST_BREATH: Duration = Duration::from_secs(5);
/// Longer than a request can take until it is answered: the exchange (10 s), the helper that asks
/// starting on Windows (20 s) and the question on the screen (60 s).
const REQUEST_HOLD: Duration = Duration::from_secs(100);

/// The request taken at `taken` is over: its turn is free (unless another has it by now).
fn request_done(taken: Option<std::time::Instant>) {
    let Some(taken) = taken else { return };
    let mut slot = REQUEST_IN.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some_and(|t| t == taken) {
        *slot = None;
        *REQUEST_ENDED.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }
}

/// Sessions in now with the one-time password.
static ONE_TIME_IN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// How long a one-time password still works after the connection with it ended: a viewer whose
/// network dropped comes back in with it.
const ONE_TIME_GRACE: Duration = Duration::from_secs(60);

/// A session with the one-time password `used` ended: a minute later, with nobody in with it
/// again, it is replaced.
fn one_time_used(used: &str) {
    ONE_TIME_IN.fetch_sub(1, Ordering::AcqRel);
    let used = used.to_string();
    thread::spawn(move || {
        thread::sleep(ONE_TIME_GRACE);
        if ONE_TIME_IN.load(Ordering::Acquire) > 0 {
            return;
        }
        let mut c = load();
        if c.one_time == used {
            c.one_time = crate::net::new_one_time();
            let _ = save(&c);
        }
    });
}

/// The agent as a process of its own, its input and output the sealed stream.
/// `code`: someone who asked without a password, and the code to show the person at the screen.
/// Where the viewer's stream stands: within a message's head or its payload (messages are a kind,
/// a length and that many bytes), to tell the person's doing from the app's own messages and to
/// put a message of this side's in between two.
#[derive(Default)]
struct Frames {
    head: [u8; 5],
    have: usize,
    left: usize,
}

/// What a viewer sends by itself: settings, acknowledgements, pings, pausing, refreshing,
/// accessibility state and density.
const AUTOMATIC: [u8; 8] = [1, 2, 7, 8, 18, 22, 36, 46];

impl Frames {
    /// Follows `b`; true when a message it begins is the person's doing.
    fn feed(&mut self, mut b: &[u8]) -> bool {
        let mut person = false;
        while !b.is_empty() {
            if self.left > 0 {
                let n = self.left.min(b.len());
                (self.left, b) = (self.left - n, &b[n..]);
                continue;
            }
            let n = (5 - self.have).min(b.len());
            self.head[self.have..self.have + n].copy_from_slice(&b[..n]);
            (self.have, b) = (self.have + n, &b[n..]);
            if self.have == 5 {
                self.have = 0;
                self.left = u32::from_le_bytes([self.head[1], self.head[2], self.head[3], self.head[4]]) as usize;
                person |= !AUTOMATIC.contains(&self.head[0]);
            }
        }
        person
    }

    fn between(&self) -> bool {
        self.have == 0 && self.left == 0
    }
}

fn serve_viewer(mut r: crate::net::SealedReader, mut w: crate::net::SealedWriter, grant: u8, code: Option<&str>, cameras: bool, idle: u32) -> Result<(), String> {
    // The session's own pings come every second; two minutes of nothing means it is gone.
    let ender = w.ender();
    ender.set_idle_limit(Duration::from_secs(120));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(["--agent", "--capture", "auto", "--by-id", "--grant", &grant.to_string()]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(c) = code {
        cmd.args(["--code", c]);
    }
    if cameras {
        cmd.arg("--cameras");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    crate::sys::tie(&child);
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    // The viewer's stream as it goes in, followed message by message; the last time the person
    // did something there.
    let input = Arc::new(Mutex::new((stdin, Frames::default(), std::time::Instant::now())));
    if idle > 0 {
        let input = Arc::downgrade(&input);
        let limit = Duration::from_secs(idle as u64 * 60);
        thread::spawn(move || {
            while let Some(input) = input.upgrade() {
                {
                    let mut i = input.lock().unwrap_or_else(|e| e.into_inner());
                    // Said between two of the viewer's messages, never inside one.
                    if i.2.elapsed() > limit && i.1.between() {
                        let why = format!("This session ended after {idle} minutes without input from you, as the computer is set to.");
                        let (k, p) = crate::proto::ToAgent::End(why).encode();
                        let _ = crate::proto::write_msg(&mut i.0, k, &p).and_then(|_| i.0.flush());
                        return;
                    }
                }
                drop(input);
                thread::sleep(Duration::from_secs(5));
            }
        });
    }
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
        // The agent is gone: so is the session, and the viewer's side ends with it, once its
        // last words (why) are through.
        ender.finish();
    });
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut i = input.lock().unwrap_or_else(|e| e.into_inner());
                if i.1.feed(&buf[..n]) {
                    i.2 = std::time::Instant::now();
                }
                if i.0.write_all(&buf[..n]).and_then(|_| i.0.flush()).is_err() {
                    break;
                }
            }
        }
    }
    drop(input);
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
        return Command::new("systemctl").args(["--user", "is-active", "--quiet", UNIT]).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(unix)]
const UNIT: &str = "sshremote-access.service";

/// The sign-in entry that starts the icon in the top bar.
#[cfg(unix)]
fn autostart_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join("sshremote-tray.desktop"))
}

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
        // Its icon in the top bar, with every sign-in and now.
        if let Some(a) = autostart_file() {
            let _ = std::fs::create_dir_all(a.parent().ok_or("bad path")?);
            let entry = format!(
                "[Desktop Entry]\nType=Application\nName=SSH Remote\nComment=Remote access by ID: its icon in the top bar\nExec=\"{}\" --tray\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
                exe.display()
            );
            if std::fs::write(&a, entry).is_ok() {
                let _ = Command::new(&exe).arg("--tray").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            }
        }
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
        // The icon notices the service went and goes too.
        if let Some(a) = autostart_file() {
            let _ = std::fs::remove_file(a);
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
        if c.passwords.lasting() {
            println!("password: {}", c.password);
        }
        if c.passwords.one_time() {
            println!("one-time: {} (a minute after a connection with it ends, a new one)", c.one_time);
        }
        println!("relay:    {}", c.relay);
        println!("on:       {}", if c.enabled { "yes" } else { "no" });
        if c.requests {
            println!("requests: anyone with the ID may ask without a password; you decide, comparing the code you both see");
        }
        if !c.from.is_empty() {
            println!("from:     only {}", c.from);
        }
        println!("asks:     {}", if c.ask { "yes, the person at this computer lets each viewer in" } else { "no, the password is enough" });
        println!("viewers:  may use {}", crate::grant::words(c.allow));
        println!("nearby:   {}", match (c.lan, c.direct) {
            (true, true) => "found by computers on this network, which may come straight in",
            (true, false) => "found by computers on this network",
            (false, true) => "not found, but computers on this network may come straight in",
            (false, false) => "not found by computers on this network",
        });
        println!("idle:     {}", if c.idle == 0 { "a session goes on however long the viewer does nothing".to_string() } else { format!("a session ends after {} minutes without input from its viewer", c.idle) });
        println!("straight: {}", if c.punch { "yes, viewers by ID move to a straight path through the routers when one is found" } else { "no, always through the relay" });
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
            // A viewer giving it would be taken to give a one-time password.
            if crate::net::looks_one_time(&c.password) {
                return Err("that looks like a one-time password (four, a dash, four): choose another".into());
            }
            save(&c)?;
            println!("password: {}", c.password);
            Ok(())
        }
        // Which passwords viewers may give, and a new one-time password.
        Some("passwords") => {
            c.passwords = args.get(1).and_then(|w| Passwords::from_word(w)).ok_or("usage: sshremote remote-access passwords lasting|one-time|both")?;
            save(&c)?;
            show(&c);
            Ok(())
        }
        // Minutes without input from a viewer after which its session ends.
        Some("idle") => {
            c.idle = match args.get(1).map(String::as_str) {
                Some("off" | "0") => 0,
                Some(n) => n.parse().ok().filter(|&m| m > 0).ok_or("usage: sshremote remote-access idle MINUTES|off")?,
                None => return Err("usage: sshremote remote-access idle MINUTES|off".into()),
            };
            save(&c)?;
            show(&c);
            Ok(())
        }
        // Who came in by the ID and who was turned away, newest first.
        Some("log") => {
            let all = visits(args.get(1).and_then(|n| n.parse().ok()).unwrap_or(30));
            if all.is_empty() {
                println!("nobody has come by the ID here yet");
            }
            for v in all {
                println!("{}", v.describe());
            }
            Ok(())
        }
        Some("one-time") => {
            c.one_time = crate::net::new_one_time();
            save(&c)?;
            println!("one-time: {}", c.one_time);
            Ok(())
        }
        Some("id") => {
            c.id = new_id();
            save(&c)?;
            println!("ID: {}", crate::computers::spaced(&c.id));
            Ok(())
        }
        // Switches, each `on` or `off`.
        Some(what @ ("straight" | "lan" | "direct" | "requests" | "cameras")) => {
            let on = match args.get(1).map(String::as_str) {
                Some("on") => true,
                Some("off") => false,
                _ => return Err(format!("usage: sshremote remote-access {what} on|off")),
            };
            match what {
                "straight" => c.punch = on,
                "lan" => c.lan = on,
                "requests" => c.requests = on,
                "cameras" => c.cameras = on,
                _ => c.direct = on,
            }
            save(&c)?;
            show(&c);
            Ok(())
        }
        // Where viewers may come from.
        Some("from") => {
            let list = args[1..].join(" ");
            if list.trim().is_empty() {
                return Err("usage: sshremote remote-access from ADDRESS|RANGE ... | anywhere   (e.g. from 203.0.113.7 192.168.1.0/24)".into());
            }
            c.from = if list.trim() == "anywhere" { String::new() } else { tidy_ranges(&list) };
            let bad = bad_ranges(&c.from);
            if !bad.is_empty() {
                return Err(format!("not an address or a range: {} (like 203.0.113.7, 192.168.1.0/24 or 2001:db8::/32)", bad.join(", ")));
            }
            save(&c)?;
            println!("{}", if c.from.is_empty() { "viewers may come from anywhere".to_string() } else { format!("viewers come in only from {}", c.from) });
            Ok(())
        }
        // Who is connected through the service now, and ending one of them.
        Some("viewers") => {
            let list = service_viewers();
            if list.is_empty() {
                println!("nobody is connected");
            }
            for v in list {
                println!("{}  {}", v.n, v.describe());
            }
            Ok(())
        }
        Some("end") => {
            let n = args.get(1).and_then(|n| n.parse::<u64>().ok()).ok_or("usage: sshremote remote-access end N (N from: sshremote remote-access viewers)")?;
            ask_to_end(n)?;
            println!("asked: viewer {n}'s connection ends within a second or two");
            Ok(())
        }
        Some("service") => match args.get(1).map(String::as_str) {
            Some("start") => service_control(true),
            Some("stop") => service_control(false),
            _ => Err("usage: sshremote remote-access service start|stop".into()),
        },
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
            c.relay = args.get(1).map(|r| crate::computers::relay_name(r)).unwrap_or_else(default_relay);
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
            // The service says how it stands, for the icon in the notification area.
            if service {
                thread::spawn(|| {
                    let mut said = String::new();
                    loop {
                        take_end_requests();
                        publish_state(&mut said);
                        thread::sleep(Duration::from_secs(1));
                    }
                });
            }
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
        _ => Err("usage: sshremote remote-access [status | log | idle MINUTES|off | on | off | run | id | password [NEW] | relay HOST | ask on|off | allow RIGHTS | straight|lan|direct|requests on|off | viewers | end N | passwords lasting|one-time|both | one-time | install | uninstall | service start|stop]".into()),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_viewers_stream_is_followed_message_by_message() {
        use crate::proto::ToAgent;
        let msg = |m: ToAgent| {
            let (k, p) = m.encode();
            let mut b = Vec::new();
            crate::proto::write_msg(&mut b, k, &p).unwrap();
            b
        };
        let (ack, ping, key) = (msg(ToAgent::Ack(7)), msg(ToAgent::Ping(0)), msg(ToAgent::Key { keysym: 0x61, mods: 0 }));
        let mut f = Frames::default();
        assert!(!f.feed(&[ack.clone(), ping.clone()].concat()), "the app's own messages are no input");
        assert!(f.between());
        // A key cut in three, across reads: the acknowledgement's last two bytes come with the
        // key's five-byte head.
        let all = [ack, key, ping].concat();
        let (a, rest) = all.split_at(7);
        let (b, c) = rest.split_at(7);
        assert!(!f.feed(a) && !f.between());
        assert!(f.feed(b), "the key's head is whole here");
        assert!(!f.between());
        assert!(!f.feed(c));
        assert!(f.between());
    }

    #[test]
    fn visits_pair_each_coming_with_its_going() {
        let text = [
            r#"{"at":100,"from":"203.0.113.7","refused":"a wrong password"}"#,
            r#"{"at":200,"from":"203.0.113.7","how":"with the lasting password","in":1,"path":"through the relay"}"#,
            // The program started again: session 1 once more, another start.
            r#"{"at":300,"from":"198.51.100.2","how":"with a one-time password","in":1,"path":"on this network"}"#,
            r#"{"at":200,"out":1,"path":"straight from 203.0.113.7","secs":240}"#,
            "not json",
        ]
        .join("\n");
        let mut v = Vec::new();
        read_visits(&text, &mut v);
        assert_eq!(v.len(), 3);
        assert!(!v[0].let_in && v[0].how == "a wrong password" && v[0].from.as_deref() == Some("203.0.113.7"));
        assert_eq!((v[1].secs, v[1].path.as_str()), (Some(240), "straight from 203.0.113.7"));
        assert_eq!((v[2].secs, v[2].path.as_str()), (None, "on this network"));
        assert!(v[1].describe().ends_with("let in with the lasting password, straight from 203.0.113.7, for 4 min"), "{}", v[1].describe());
        assert!(v[2].describe().contains("still here or cut off"));
    }

    #[test]
    fn viewers_come_only_from_the_addresses_listed() {
        let ip = |a: &str| Some(a.parse::<std::net::IpAddr>().unwrap());
        assert!(comes_from("", None) && comes_from("  ", ip("8.8.8.8")), "no list: anywhere, known or not");
        let list = "203.0.113.7, 192.168.1.0/24 2001:db8::/32";
        assert!(comes_from(list, ip("203.0.113.7")) && !comes_from(list, ip("203.0.113.8")));
        assert!(comes_from(list, ip("192.168.1.250")) && !comes_from(list, ip("192.168.2.1")));
        assert!(comes_from(list, ip("2001:db8:1::5")) && !comes_from(list, ip("2001:db9::5")));
        assert!(comes_from(list, ip("::ffff:192.168.1.9")), "an IPv4 address written as IPv6");
        assert!(!comes_from(list, None), "where it is not known: not past a list");
        assert!(comes_from("0.0.0.0/0", ip("1.2.3.4")) && !comes_from("0.0.0.0/0", ip("::1")));
        assert_eq!(bad_ranges("10.0.0.0/8 10.0.0.0/33 host.example 1.2.3"), ["10.0.0.0/33", "host.example", "1.2.3"]);
        assert_eq!(tidy_ranges(" 10.0.0.1,,  10.0.0.2 ;"), "10.0.0.1 10.0.0.2");
    }
    use super::*;

    /// The line to a relay of one's own is sealed and its key kept; a relay showing another key
    /// later is refused, and so is one that no longer seals once it did; an older relay gets
    /// the plain line the first time.
    #[test]
    fn the_line_to_the_relay_is_sealed_with_the_key_it_is_known_by() {
        use std::net::TcpListener;
        // A relay of 1.3 with `lasting` (None: one before, which drops a line it does not know).
        let relay = |lasting: Option<([u8; 32], [u8; 32])>| -> String {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = l.local_addr().unwrap().to_string();
            thread::spawn(move || {
                for s in l.incoming().take(2) {
                    let mut s = s.unwrap();
                    let first = line(&mut s).unwrap();
                    let Some(lasting) = lasting else {
                        if let Some(rest) = first.strip_prefix("listen ") {
                            let _ = rest;
                            writeln!(s, "ok").unwrap();
                        }
                        continue;
                    };
                    let e = crate::relaylink::unhex(first.strip_prefix("secure ").unwrap()).unwrap();
                    let (answer, seal) = crate::relaylink::answer(&e, &lasting, [4; 32]).unwrap();
                    writeln!(s, "{answer}").unwrap();
                    let crate::relaylink::Sealed { mut out, mut inn } = seal;
                    let mut raw = Vec::new();
                    let mut b = [0u8; 512];
                    // As the relay reads it: frames until a line is whole, three words.
                    let mut words = Vec::new();
                    while !words.ends_with(b"\n") {
                        if let Some(w) = inn.open(&mut raw).unwrap() {
                            words.extend(w);
                            continue;
                        }
                        let n = s.read(&mut b).unwrap();
                        raw.extend_from_slice(&b[..n]);
                    }
                    let text = String::from_utf8(words).unwrap();
                    assert_eq!(text.split_whitespace().count(), 3, "{text:?}");
                    assert!(text.starts_with("listen "));
                    s.write_all(&out.seal(b"ok\n")).unwrap();
                }
            });
            addr
        };
        let mut cfg = fresh();
        cfg.relay = relay(Some(crate::relaylink::keypair([1; 32])));
        let mut link = sealed_line(&mut cfg).unwrap();
        writeln!(link, "listen {} {}", cfg.id, cfg.token).unwrap();
        assert_eq!(line(&mut link).unwrap(), "ok");
        assert!(cfg.relay_key.starts_with(&format!("{} ", cfg.relay)), "kept: {}", cfg.relay_key);
        // The same address with another key: refused.
        let other = relay(Some(crate::relaylink::keypair([2; 32])));
        cfg.relay_key = cfg.relay_key.replace(&cfg.relay, &other);
        cfg.relay = other;
        assert!(sealed_line(&mut cfg).err().unwrap().contains("another key"));
        // One before 1.3: the plain line, the first time only.
        let mut fresh_cfg = fresh();
        fresh_cfg.relay = relay(None);
        let mut plain = sealed_line(&mut fresh_cfg).unwrap();
        writeln!(plain, "listen {} {}", fresh_cfg.id, fresh_cfg.token).unwrap();
        assert_eq!(line(&mut plain).unwrap(), "ok");
        assert!(fresh_cfg.relay_key.is_empty());
        let old = relay(None);
        cfg.relay_key = format!("{old} {}", "ab".repeat(32));
        cfg.relay = old;
        assert!(sealed_line(&mut cfg).err().unwrap().contains("as it always has"));
        // The public relay is known by the key shipped.
        cfg.relay = crate::computers::OLD_RELAY.into();
        assert_eq!(known_key(&cfg), crate::relaylink::unhex(crate::relaylink::PUBLIC_RELAY_KEY));
    }

    /// The public relay seals the line with the key shipped: cargo test -- --ignored public_relay_seals
    #[test]
    #[ignore]
    fn public_relay_seals() {
        let mut cfg = fresh();
        cfg.relay = crate::computers::RELAY.into();
        let mut link = sealed_line(&mut cfg).unwrap();
        writeln!(link, "listen {} {}", cfg.id, cfg.token).unwrap();
        assert_eq!(line(&mut link).unwrap(), "ok");
    }

    /// The service never takes a link (a junction elsewhere) for its folder, and never makes its
    /// folder over something already there.
    #[cfg(windows)]
    #[test]
    fn a_link_is_never_taken_for_the_shared_folder() {
        let base = std::env::temp_dir().join(format!("sshr-acl-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join("real");
        win_create(&dir, true, "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)").unwrap();
        assert!(win_create(&dir, true, "D:P(A;;FA;;;SY)").is_err());
        assert!(win_create(&dir.join("end.txt"), false, "D:P(A;;FA;;;SY)(A;;FA;;;OW)").is_ok());
        assert!(win_create(&dir.join("end.txt"), false, "D:P(A;;FA;;;SY)").is_err());
        let link = base.join("link");
        let made = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&link).arg(&dir).output().unwrap();
        assert!(made.status.success() && link.join("end.txt").is_file());
        assert!(!win_folder_is_safe(&link));
        assert!(!win_folder_is_safe(&dir.join("end.txt")));
        std::fs::remove_dir(&link).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
    }

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
