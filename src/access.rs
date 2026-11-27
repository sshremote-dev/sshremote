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
    /// Devices let in without the two-step code (each for `TRUST_DAYS` from when it was trusted).
    pub trusted: Vec<Trusted>,
    /// The proxy the relay is reached through (`proxy::Proxy`'s URL); empty: straight. Kept
    /// here for a service that runs without the person's own settings.
    pub proxy: String,
    /// Whether sessions by ID are recorded here, as their viewers see the screen
    /// (`recordings_dir`); off unless chosen, and each viewer is told.
    pub record: bool,
    /// Which viewers' IDs come in ("123456789, 98765*"; `tidy_ids`); empty: any. Each shows its
    /// ID with its device's key, and the first device an ID came from is the only one it may
    /// come from after (`id_keys`).
    pub ids: String,
    /// The device each listed ID first came from: the ID, its key (hex), and when.
    pub id_keys: Vec<(String, String, u64)>,
    /// Remote access run as a service puts newer releases in place by itself, between sessions
    /// (`update::unattended`): nobody sits at such a computer to click "Update".
    pub auto_update: bool,
    /// Which way the clipboard and files go for viewers by ID (`agent::WAY_*`): both ways, from
    /// this computer only, or to it only.
    pub clip_way: u8,
    pub files_way: u8,
    /// The desktop's picture taken away during sessions by ID (less to send).
    pub plain_wallpaper: bool,
    /// The team this computer is in (`team::client::Joined` as JSON), or null: kept with the rest,
    /// so that only who may change remote access can join a team, and the service reads it.
    pub team: serde_json::Value,
}

/// Where sessions by ID are recorded: on Windows where the service runs them (as SYSTEM), a
/// folder of its own that only administrators open; else "SSH Remote sessions" in Videos (or
/// Downloads).
pub fn recordings_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if crate::console_user::running_as_system() || service_installed() {
        return state_file().and_then(|f| f.parent().map(|d| d.join("Session recordings")));
    }
    let downloads = crate::files::downloads();
    let base = downloads.parent().map(|h| h.join("Videos")).filter(|p| p.is_dir()).unwrap_or(downloads);
    Some(base.join("SSH Remote sessions"))
}

/// The same, made if need be, for none but its owner (Windows' service: SYSTEM and the
/// administrators) to open; one that others could open is not recorded in.
pub fn recordings_made() -> Result<PathBuf, String> {
    let dir = recordings_dir().ok_or("no folder for recordings")?;
    #[cfg(windows)]
    if crate::console_user::running_as_system() {
        if !shared_folder_ready() {
            return Err("the service's folder is not safe to use".into());
        }
        if !dir.exists() {
            win_create(&dir, true, "O:SYG:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")?;
        }
        if !win_folder_is_safe(&dir) {
            return Err(format!("{} is not the service's own folder", dir.display()));
        }
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(dir)
}

/// A device a viewer came in from with the two-step code and asked to be trusted on.
#[derive(Clone, Debug, PartialEq)]
pub struct Trusted {
    /// The checksum of the token it was given (the token itself stays with the device).
    pub hash: String,
    /// When it was trusted, and when it last came in by its token (0: not yet, so it may never
    /// have got it).
    pub at: u64,
    pub used: u64,
    /// Where it came from then.
    pub from: String,
}

/// How long a trusted device comes in without the two-step code.
pub const TRUST_DAYS: u64 = 90;
/// Devices trusted at most; for a new one, the oldest that never came back goes, else the oldest.
const TRUST_MOST: usize = 32;
/// How long a device trusted waits to come back by its token before it is taken for one that
/// never got it.
const TRUST_PENDING: u64 = 86_400;

impl Trusted {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "hash": self.hash, "at": self.at, "used": self.used, "from": self.from })
    }

    fn from_json(v: &serde_json::Value) -> Option<Trusted> {
        let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        Some(Trusted { hash: v.get("hash")?.as_str()?.to_string(), at: n("at"), used: n("used"), from: v.get("from").and_then(|x| x.as_str()).unwrap_or("").to_string() })
    }

    /// Still trusted: not past `TRUST_DAYS`, nor a day past being trusted without coming back.
    pub fn current(&self) -> bool {
        let age = crate::sys::now_secs().saturating_sub(self.at);
        age < TRUST_DAYS * 86_400 && (self.used != 0 || age < TRUST_PENDING)
    }
}

/// A token's checksum with the two-step secret: a new secret trusts no device of the old one.
fn token_hash(secret: &str, token: &str) -> String {
    use sha2::Digest;
    crate::sys::hex(&sha2::Sha256::digest(format!("{secret}\n{}", token.trim()).as_bytes()))
}

impl Config {
    /// The devices still trusted.
    pub fn trusted_now(&self) -> impl Iterator<Item = &Trusted> {
        self.trusted.iter().filter(|t| t.current())
    }

    /// Whether `token` is a trusted device's.
    fn trusts(&self, token: &str) -> bool {
        let h = token_hash(&self.totp, token);
        !self.totp.is_empty() && !token.trim().is_empty() && self.trusted_now().any(|t| t.hash == h)
    }
}

/// Changes the stored settings as they are now, one change at a time in this process (the
/// sessions' threads each write some), keeping them when `change` says so.
fn edit(change: impl FnOnce(&mut Config) -> bool) {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let mut c = load();
    if change(&mut c) {
        let _ = save(&c);
    }
}

/// Trusts the device given `token` from now on (it came in with the two-step code).
fn trust_device(token: &str, from: &str) {
    edit(|c| {
        if c.totp.is_empty() {
            return false;
        }
        c.trusted.retain(Trusted::current);
        while c.trusted.len() >= TRUST_MOST {
            let i = c.trusted.iter().position(|t| t.used == 0).unwrap_or(0);
            c.trusted.remove(i);
        }
        c.trusted.push(Trusted { hash: token_hash(&c.totp, token), at: crate::sys::now_secs(), used: 0, from: from.to_string() });
        true
    });
}

/// Notes that the device with `token` came in now.
fn trusted_came(token: &str) {
    edit(|c| {
        let h = token_hash(&c.totp, token);
        c.trusted.iter_mut().find(|t| t.hash == h).map(|t| t.used = crate::sys::now_secs()).is_some()
    });
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
            Passwords::Lasting => n!("Lasting"),
            Passwords::OneTime => n!("One-time"),
            Passwords::Both => n!("Both"),
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
        serde_json::json!({ "id": self.id, "token": self.token, "password": self.password, "relay": self.relay, "enabled": self.enabled, "ask": self.ask, "allow": self.allow, "lan": self.lan, "direct": self.direct, "punch": self.punch, "totp": self.totp, "passwords": self.passwords.word(), "one_time": self.one_time, "requests": self.requests, "from": self.from, "relay_key": self.relay_key, "cameras": self.cameras, "idle": self.idle, "trusted": self.trusted.iter().map(Trusted::json).collect::<Vec<_>>(), "proxy": self.proxy, "record": self.record, "ids": self.ids, "id_keys": self.id_keys.iter().map(|(i, k, at)| serde_json::json!({ "id": i, "key": k, "at": at })).collect::<Vec<_>>(), "auto_update": self.auto_update, "clip_way": self.clip_way, "files_way": self.files_way, "plain_wallpaper": self.plain_wallpaper, "team": self.team })
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
            trusted: v.get("trusted").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(Trusted::from_json).collect()).unwrap_or_default(),
            proxy: s("proxy").unwrap_or_default(),
            // Off unless chosen.
            record: v.get("record").and_then(|x| x.as_bool()).unwrap_or(false),
            ids: s("ids").unwrap_or_default(),
            id_keys: v
                .get("id_keys")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|k| Some((k.get("id")?.as_str()?.to_string(), k.get("key")?.as_str()?.to_string(), k.get("at").and_then(|x| x.as_u64()).unwrap_or(0))))
                        .collect()
                })
                .unwrap_or_default(),
            team: v.get("team").filter(|t| t.is_object()).cloned().unwrap_or_default(),
            // On unless turned off: a computer nobody sits at would stay on an old release.
            auto_update: v.get("auto_update").and_then(|x| x.as_bool()).unwrap_or(true),
            clip_way: v.get("clip_way").and_then(|x| x.as_u64()).filter(|w| *w <= 2).unwrap_or(0) as u8,
            files_way: v.get("files_way").and_then(|x| x.as_u64()).filter(|w| *w <= 2).unwrap_or(0) as u8,
            plain_wallpaper: v.get("plain_wallpaper").and_then(|x| x.as_bool()).unwrap_or(false),
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
            return Err(n!("only administrators can see them (they hold the password)").into());
        }
        if r != 0 {
            return Err(crate::lang::fill(n!("the remote access settings could not be read (error {})"), &[&r]));
        }
        let mut buf = vec![0u16; len as usize / 2 + 1];
        let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut len) };
        if r != 0 {
            return Err(crate::lang::fill(n!("the remote access settings could not be read (error {})"), &[&r]));
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
            return Err(n!("the settings' permissions could not be made").into());
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
            5 => n!("Changing remote access needs administrator rights").to_string(),
            e => crate::lang::fill(n!("the remote access settings could not be saved (error {})"), &[&e]),
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
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| crate::lang::fill(n!("the remote access settings are damaged: {}"), &[&e]))?;
    Config::from_json(&v).filter(|c| valid_id(&c.id)).map(Some).ok_or_else(|| n!("the remote access settings are damaged").into())
}

/// When [`recent`] last read the settings, from where, and what it found.
static RECENT: Mutex<Option<(std::time::Instant, Option<PathBuf>, Option<Config>)>> = Mutex::new(None);

/// [`recent`] with the team's policy over it: what remote access acts on (the window shows and
/// keeps the computer's own settings).
pub fn effective() -> Option<Config> {
    recent().map(with_policy)
}

/// [`stored`] for the loops that wait on it: read again at most once a second, however often
/// they ask (a burst of calls or of network questions included).
pub fn recent() -> Option<Config> {
    // Windows keeps them in one place for the whole machine.
    #[cfg(all(windows, not(test)))]
    let path: Option<PathBuf> = None;
    #[cfg(any(not(windows), test))]
    let path = file();
    let mut last = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, from, c)) = &*last
        && at.elapsed() < Duration::from_secs(1)
        && *from == path
    {
        return c.clone();
    }
    let c = stored().ok().flatten();
    *last = Some((std::time::Instant::now(), path, c.clone()));
    c
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
        trusted: Vec::new(),
        proxy: String::new(),
        record: false,
        ids: String::new(),
        id_keys: Vec::new(),
        auto_update: true,
        clip_way: 0,
        files_way: 0,
        plain_wallpaper: false,
        team: serde_json::Value::Null,
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

/// macOS: what the login window's copy of remote access (run as root there, with the settings of
/// the person who set it up) writes is handed to that person, who could not read or change it
/// otherwise.
pub(crate) fn hand_back(p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        // The file itself, opened as itself: a link put in its place is not followed.
        if let Some(owner) = crate::hosts::config_dir().and_then(|d| std::fs::metadata(d).ok())
            && owner.uid() != 0
            && let Ok(f) = crate::sys::open_own(p, false)
        {
            let _ = std::os::unix::fs::fchown(&f, Some(owner.uid()), Some(owner.gid()));
        }
    }
    let _ = p;
}

pub fn save(c: &Config) -> Result<(), String> {
    // The team is changed by `set_team` alone: a window's older copy of the settings keeps it.
    let mut c = c.clone();
    if let Ok(Some(now)) = stored() {
        c.team = now.team;
    }
    write_config(&c)
}

/// Joins this computer's settings to a team (`team::client`), or out of one with null.
pub fn set_team(team: serde_json::Value) -> Result<(), String> {
    let mut c = stored()?.unwrap_or_else(fresh);
    c.team = team;
    write_config(&c)
}

fn write_config(c: &Config) -> Result<(), String> {
    if c.password.trim().chars().count() < 8 {
        return Err(n!("The password needs at least 8 characters").into());
    }
    let text = serde_json::to_string_pretty(&c.to_json()).map_err(|e| e.to_string())?;
    let done = write_settings(&text);
    // The loops that wait on the settings see these at once.
    *RECENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    done
}

fn write_settings(text: &str) -> Result<(), String> {
    #[cfg(all(windows, not(test)))]
    return machine::write(text);
    #[cfg(any(not(windows), test))]
    {
        let f = file().ok_or(n!("no settings folder"))?;
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        // Readable by its user only: it holds the password.
        crate::sys::write_whole(&f, text.as_bytes(), true, true).map_err(|e| format!("{}: {e}", f.display()))?;
        hand_back(&f);
        Ok(())
    }
}

/// An ID a person chose: as `valid_id`, and not starting with 0 (as the relay keeps it). The relay
/// says whether another computer has it already.
pub fn own_id_ok(id: &str) -> bool {
    valid_id(id) && !id.starts_with('0')
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

/// The relay, reached for someone else (a web gateway's visitor at `whom`): a relay on this
/// computer takes their address from a web server here, so their calls count as theirs, not as
/// all the gateway's visitors' together. Elsewhere it is dialled as for this person.
pub fn dial_for(relay: &str, whom: std::net::IpAddr) -> io::Result<TcpStream> {
    let (host, port) = relay_addr(relay);
    let here = host.eq_ignore_ascii_case("localhost") || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|a| a.is_loopback());
    if !here {
        return dial(relay);
    }
    let mut s = TcpStream::connect_timeout(&(host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().unwrap_or(std::net::Ipv4Addr::LOCALHOST.into()), port).into(), Duration::from_secs(4))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(s, "GET /relay HTTP/1.1\r\nHost: {host}\r\nUpgrade: sshremote-relay\r\nConnection: Upgrade\r\nX-Real-IP: {}\r\n\r\n", whom.to_canonical())?;
    let status = line(&mut s)?;
    while !line(&mut s)?.is_empty() {}
    if !status.contains(" 101") {
        return Err(io::Error::other(format!("the relay did not take the visitor's address: {status}")));
    }
    s.set_read_timeout(None)?;
    Ok(s)
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
        "team": crate::team::client::public(&c.team),
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
    if crate::sys::write_whole(&f, now.as_bytes(), false, false).is_ok() {
        hand_back(&f);
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
        let ago = crate::hosts::ago(self.since);
        match &self.straight {
            Some(a) => t!("connected {}, straight from {}", ago, a.rsplit_once(':').map_or(a.as_str(), |(h, _)| h).trim_matches(['[', ']'])),
            None => t!("connected {}, through the relay", ago),
        }
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
    // Opened as itself only (see `sys::open_own`): the login screen's copy runs as root.
    if let Ok(mut t) = crate::sys::open_own(&f, false)
        && t.metadata().is_ok_and(|m| m.len() > VISITS_MOST)
    {
        let mut text = String::new();
        if t.read_to_string(&mut text).is_ok() {
            let rest = &text[text.len() / 2..];
            let _ = crate::sys::write_whole(&f, rest.find('\n').map_or("", |i| &rest[i + 1..]).as_bytes(), true, false);
        }
    }
    if let Ok(mut out) = crate::sys::open_own(&f, true) {
        let _ = writeln!(out, "{v}");
        hand_back(&f);
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
        // The record keeps English words; the window shows them in the reader's language.
        let (ago, how, path) = (crate::hosts::ago(self.at), crate::lang::tr_msg(&self.how), crate::lang::tr_msg(&self.path));
        match (&self.from, self.let_in, self.secs) {
            (Some(f), false, _) => t!("{} from {}: turned away, {}", ago, f, how),
            (None, false, _) => t!("{}: turned away, {}", ago, how),
            (Some(f), true, Some(s)) if s < 60 => t!("{} from {}: let in {}, {}, for {} s", ago, f, how, path, s),
            (None, true, Some(s)) if s < 60 => t!("{}: let in {}, {}, for {} s", ago, how, path, s),
            (Some(f), true, Some(s)) if s < 3600 => t!("{} from {}: let in {}, {}, for {} min", ago, f, how, path, s / 60),
            (None, true, Some(s)) if s < 3600 => t!("{}: let in {}, {}, for {} min", ago, how, path, s / 60),
            (Some(f), true, Some(s)) => t!("{} from {}: let in {}, {}, for {} h {} min", ago, f, how, path, s / 3600, s % 3600 / 60),
            (None, true, Some(s)) => t!("{}: let in {}, {}, for {} h {} min", ago, how, path, s / 3600, s % 3600 / 60),
            (Some(f), true, None) => t!("{} from {}: let in {}, {}, still here or cut off", ago, f, how, path),
            (None, true, None) => t!("{}: let in {}, {}, still here or cut off", ago, how, path),
        }
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
    let f = end_file().ok_or(n!("no folder for the request"))?;
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
        Err(crate::lang::fill(n!("{}: in use"), &[&f.display()]))
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
    /// The token of the trusted device it came in from, in place of the two-step code.
    token: Option<String>,
    /// Its ID and its device's key, where only some IDs come in.
    who: Option<(String, String)>,
}

enum Came {
    Lasting(String),
    OneTime(String),
    Request,
    /// A person of the computer's team, by a ticket.
    Team,
}

impl Welcome {
    fn new(c: &Config, came: &crate::net::Came, from: Option<std::net::IpAddr>, token: Option<String>, who: Option<(String, String)>) -> Welcome {
        let came = match came {
            crate::net::Came::Lasting => Came::Lasting(c.password.clone()),
            crate::net::Came::OneTime => Came::OneTime(c.one_time.clone()),
            crate::net::Came::Request(_) => Came::Request,
            crate::net::Came::Team(_) => Came::Team,
        };
        Welcome { id: c.id.clone(), relay: c.relay.clone(), came, from, token, who }
    }

    /// Whether the settings `c` would still let it in.
    fn still(&self, c: &Config) -> bool {
        c.enabled
            && c.id == self.id
            && c.relay == self.relay
            && comes_from(&c.from, self.from)
            // One in by a trusted device's token goes when it is no longer trusted (two-step
            // sign-in off, nothing to be trusted for, it stays).
            && self.token.as_ref().is_none_or(|t| c.totp.is_empty() || c.trusts(t))
            // A list of IDs set or changed: one not on it goes, as one from another device than
            // the one its ID is kept with.
            && (c.ids.trim().is_empty() || self.who.as_ref().is_some_and(|(id, key)| id_listed(&c.ids, id) && id_key(c, id).is_none_or(|k| k == *key)))
            && match &self.came {
                Came::Lasting(p) => c.passwords.lasting() && *p == c.password,
                Came::OneTime(p) => c.passwords.one_time() && *p == c.one_time,
                Came::Request => c.requests,
                // While the computer stays in its team.
                Came::Team => crate::team::client::state().joined.is_some(),
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
    // Through the proxy, when one is set.
    let direct = |port: u16| crate::proxy::connect(&host, port, Duration::from_secs(4));
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
            return Err(io::Error::other(crate::lang::fill(n!("{}:80 did not pass the relay through: {}"), &[&host, &status])));
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
    Err(io::Error::new(err.kind(), crate::lang::fill(n!("the relay {} (port {}, and 80): {}"), &[&host, &port, &err])))
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

/// Wrong passwords in a row past which a place they come from (an address, or an IPv6 network's
/// /64) is not checked for a day, or until the password is changed: a guesser gets this many
/// tries there, not thousands. `LOCK_ALL_AFTER` in a row from places not let in lately (more
/// than one place makes alone) stop the checking of all such places the same way; those a right
/// password came from lately are still checked, so a guesser who knows the ID cannot lock its
/// owner out.
const LOCK_AFTER: u32 = 20;
const LOCK_ALL_AFTER: u32 = 100;
const LOCK_FOR: Duration = Duration::from_secs(24 * 3600);
/// Calls answered at once; more (a flood from a relay or someone on the path) are dropped. Places
/// let in lately have a few more of their own, which a flood from elsewhere cannot take.
const MOST_CALLS: usize = 8;
const KNOWN_CALLS: usize = 4;
/// Places let in lately that are remembered, and places with wrong passwords counted.
const KNOWN_MOST: usize = 32;
const COUNTED_MOST: usize = 4096;

/// Where guesses are counted from: an address, or the /64 an IPv6 one is in (one network's).
fn place(from: Option<std::net::IpAddr>) -> String {
    match from.map(|a| a.to_canonical()) {
        Some(std::net::IpAddr::V4(a)) => a.to_string(),
        Some(std::net::IpAddr::V6(a)) => {
            let s = a.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        None => "?".into(),
    }
}

/// Wrong passwords seen lately, and one password check at a time: guesses wait their turn and
/// cost more time each, and the count fades after a quiet quarter of an hour, so wrong guesses
/// slow an attacker without locking the owner out. Many in a row stop the checking (see
/// `LOCK_AFTER`); places a right password came from lately keep a way of their own in.
#[derive(Default)]
struct Guard {
    /// Wrong passwords from places not let in lately, for their delay, and those in a row.
    failures: Mutex<(u32, Option<std::time::Instant>)>,
    strikes: Mutex<(u32, Option<std::time::Instant>)>,
    /// Each place's wrong passwords in a row, and when the last was.
    places: Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>,
    /// Places a right password came from lately, the latest last.
    known: Mutex<std::collections::VecDeque<String>>,
    turn: Mutex<()>,
    /// Calls waiting for the turn: past two, more are turned away at once, so a queue of guesses
    /// cannot keep the owner's own call waiting until it gives up.
    waiting: std::sync::atomic::AtomicUsize,
    /// Calls being answered now.
    calls: std::sync::atomic::AtomicUsize,
}

/// Wrong passwords in a row in this process's remote access from places not let in lately, for
/// the window.
static STRIKES: AtomicU32 = AtomicU32::new(0);

/// Whether this process's remote access stopped taking passwords from places not let in lately,
/// after too many wrong ones.
pub fn locked() -> bool {
    STRIKES.load(Ordering::Relaxed) >= LOCK_ALL_AFTER
}

impl Guard {
    /// A right password came from there lately.
    fn known(&self, from: &str) -> bool {
        self.known.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|k| k == from)
    }

    /// No password is checked from there: too many wrong ones in a row, not long ago, from there
    /// (or, for a place not let in lately, from all such places).
    fn locked(&self, from: &str) -> bool {
        let mut places = self.places.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((n, at)) = places.get(from).copied() {
            if at.elapsed() > LOCK_FOR {
                places.remove(from);
            } else if n >= LOCK_AFTER {
                return true;
            }
        }
        drop(places);
        if self.known(from) {
            return false;
        }
        let mut s = self.strikes.lock().unwrap_or_else(|e| e.into_inner());
        if s.1.is_some_and(|t| t.elapsed() > LOCK_FOR) {
            *s = (0, None);
            STRIKES.store(0, Ordering::Relaxed);
        }
        s.0 >= LOCK_ALL_AFTER
    }

    /// A new password: what was guessed against the old one no longer counts.
    fn reset(&self) {
        *self.strikes.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        *self.failures.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        self.places.lock().unwrap_or_else(|e| e.into_inner()).clear();
        STRIKES.store(0, Ordering::Relaxed);
    }

    /// How long a check from there waits first: more for each wrong one lately, from there or,
    /// for a place not let in lately, from all such places.
    fn delay(&self, from: &str) -> Duration {
        let wait = |n: u32| if n == 0 { Duration::ZERO } else { Duration::from_secs((1u64 << n.min(5)).min(30)) };
        let quiet = Duration::from_secs(15 * 60);
        let own = self.places.lock().unwrap_or_else(|e| e.into_inner()).get(from).filter(|(_, at)| at.elapsed() < quiet).map_or(0, |p| p.0);
        if self.known(from) {
            return wait(own);
        }
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if f.1.is_some_and(|t| t.elapsed() > quiet) {
            *f = (0, None);
        }
        wait(own.max(f.0))
    }

    fn failed(&self, from: &str) {
        let now = std::time::Instant::now();
        let mut places = self.places.lock().unwrap_or_else(|e| e.into_inner());
        if places.len() >= COUNTED_MOST {
            // Addresses made up by the thousand: only those locked out are kept.
            places.retain(|_, (n, at)| *n >= LOCK_AFTER && at.elapsed() < LOCK_FOR);
        }
        let p = places.entry(from.to_string()).or_insert((0, now));
        *p = (p.0 + 1, now);
        drop(places);
        if self.known(from) {
            return;
        }
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        *f = (f.0 + 1, Some(now));
        let mut s = self.strikes.lock().unwrap_or_else(|e| e.into_inner());
        *s = (s.0 + 1, Some(now));
        STRIKES.store(s.0, Ordering::Relaxed);
    }

    fn passed(&self, from: &str) {
        self.places.lock().unwrap_or_else(|e| e.into_inner()).remove(from);
        let mut known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        known.retain(|k| k != from);
        if known.len() >= KNOWN_MOST {
            known.pop_front();
        }
        known.push_back(from.to_string());
        drop(known);
        *self.failures.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        *self.strikes.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
        STRIKES.store(0, Ordering::Relaxed);
    }

    /// Takes one of the calls answered at once, if there is one for a call from there.
    /// A place for one call more from `from`, if it may have one: given back as the slot goes,
    /// also when its call ends in a panic.
    fn take_call(self: &Arc<Self>, from: &str) -> Option<CallSlot> {
        if self.locked(from) {
            return None;
        }
        let most = if self.known(from) { MOST_CALLS + KNOWN_CALLS } else { MOST_CALLS };
        if self.calls.fetch_add(1, Ordering::AcqRel) >= most {
            self.calls.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(CallSlot(self.clone()))
    }
}

/// One of the calls taken at once (`Guard::take_call`).
struct CallSlot(Arc<Guard>);

impl Drop for CallSlot {
    fn drop(&mut self) {
        self.0.calls.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Linux, with remote access at the login screen set up: which of its two services this is.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// The only one.
    Plain,
    /// Root's, at the login screen and on a locked screen, around the desktop of `owner`.
    LoginScreen(u32),
    /// The person's own, while their desktop is open on the screen.
    Owner(u32),
}

#[cfg(target_os = "linux")]
static ROLE: std::sync::OnceLock<Role> = std::sync::OnceLock::new();

#[cfg(target_os = "linux")]
fn role() -> Role {
    match *ROLE.get().unwrap_or(&Role::Plain) {
        // The person's own service learns of root's as soon as it is set up (after it, often).
        Role::Owner(_) if !crate::loginscreen::installed() => Role::Plain,
        r => r,
    }
}

/// This process is root's remote access at the login screen.
fn at_login_screen() -> bool {
    #[cfg(target_os = "linux")]
    return matches!(role(), Role::LoginScreen(_));
    #[allow(unreachable_code)]
    false
}

/// Whether the other of the two services answers for this computer now (Linux, with remote
/// access at the login screen): root's while the person's desktop is not open on the screen,
/// theirs while it is. Root's also waits while the person wants to let each viewer in, as
/// nobody at a login screen can.
fn standby() -> bool {
    #[cfg(target_os = "linux")]
    {
        use crate::console::Console;
        static SEEN: Mutex<Option<(std::time::Instant, Option<Console>)>> = Mutex::new(None);
        let r = role();
        if r == Role::Plain {
            return false;
        }
        let console = {
            let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            match *seen {
                Some((at, c)) if at.elapsed() < Duration::from_millis(900) => c,
                _ => {
                    let c = crate::console::now();
                    *seen = Some((std::time::Instant::now(), c));
                    c
                }
            }
        };
        return match r {
            Role::LoginScreen(owner) => console.is_none_or(|c| c.open_for(owner)) || effective().is_some_and(|c| c.ask),
            Role::Owner(uid) => console.is_some_and(|c| !c.open_for(uid)),
            Role::Plain => false,
        };
    }
    #[allow(unreachable_code)]
    false
}

/// The settings with the team's policy over them, for a computer in a team (docs/team.md).
pub fn with_policy(mut c: Config) -> Config {
    if let Some(p) = crate::team::client::policy() {
        c.allow &= p.rights;
        if let Some(a) = p.ask {
            c.ask = a;
        }
        if let Some(r) = p.record {
            c.record = r;
        }
        if let Some(i) = p.idle {
            c.idle = i;
        }
        if let Some(d) = p.direct {
            c.punch = d;
        }
        if let Some(l) = p.lan {
            c.lan = l;
            c.direct &= l;
        }
        if !p.password {
            c.requests = false;
        }
    }
    c
}

/// Keeps this computer reachable by its ID until `keep` says to stop: a line to the relay, a new
/// connection for each viewer, and the agent run as a process of its own for each.
pub fn run(keep: impl Fn() -> Option<Config>) {
    // In a team, its policy over the settings; looked up each minute, with the record sent.
    let keep = || keep().map(with_policy);
    thread::spawn(|| {
        loop {
            if crate::team::client::state().joined.is_some() {
                match crate::team::client::refresh() {
                    // The console ended this computer's sessions.
                    Ok(true) => end_sessions_but(None),
                    Ok(false) => {}
                    Err(e) => eprintln!("team: {e}"),
                }
            }
            thread::sleep(Duration::from_secs(60));
        }
    });
    // Computers on this network that ask are told this one's ID, while remote access is on and
    // that is allowed (one process answers: another that runs it too finds the port taken).
    thread::spawn(|| crate::lan::answer(|| effective().filter(|c| c.enabled && c.lan).map(|c| (c.id, c.relay, c.direct))));
    let guard = Arc::new(Guard::default());
    {
        let guard = guard.clone();
        thread::spawn(move || listen_direct(guard));
    }
    let mut wait = 2;
    let mut password = None;
    while let Some(mut cfg) = keep() {
        // The other service answers meanwhile; for the person's own the computer is reachable.
        if standby() {
            set_state(if at_login_screen() { State::Connecting } else { State::Online });
            sleep_while(&keep, 1);
            continue;
        }
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
                    Err(e) => Some(crate::lang::fill(n!("The ID is taken and a new one could not be kept: {}"), &[&e])),
                }
            }
            Ok(other) => Some(crate::lang::fill(n!("The relay refused: {}"), &[&other])),
            Err(e) => Some(crate::lang::fill(n!("The relay did not answer: {}"), &[&e])),
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
                cfg.trusted.clone_from(&k.trusted);
                cfg.one_time.clone_from(&k.one_time);
                cfg.from.clone_from(&k.from);
                // Those the new settings would not let in go now.
                end_sessions_but(Some(k));
            }
            // At the login screen nobody can let a viewer in on request.
            if at_login_screen() {
                cfg.requests = false;
            }
            if standby() {
                let _ = s.shutdown(std::net::Shutdown::Both);
                // The desktop opened: the login screen's viewers go on to the person's own service
                // (they connect again by themselves); the person's own keep theirs.
                if at_login_screen() {
                    end_sessions_but(None);
                }
                break;
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
                    // no password is taken from there.
                    let Some(slot) = guard.take_call(&place(from)) else { continue };
                    let (cfg, nonce, guard) = (cfg.clone(), nonce.to_string(), guard.clone());
                    thread::spawn(move || {
                        let _slot = slot;
                        answer_call(&cfg, &nonce, &guard, from);
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
        return crate::relaylink::key_from_hex(crate::relaylink::PUBLIC_RELAY_KEY);
    }
    let (r, k) = cfg.relay_key.split_once(' ')?;
    (r == relay).then(|| crate::relaylink::key_from_hex(k)).flatten()
}

/// The line to the relay, sealed: with the key it is known by, or (a relay of one's own seen
/// for the first time) the key it shows, kept. A relay before 1.3 gets the plain line, unless
/// it was known sealed: then someone in between is refusing the seal, and nothing is said.
fn sealed_line(cfg: &mut Config) -> Result<crate::relaylink::Link, String> {
    use crate::relaylink::{Link, Outcome};
    let reach = |relay: &str| -> Result<TcpStream, String> {
        let s = dial(relay).map_err(|e| crate::lang::fill(n!("Could not reach {}"), &[&e]))?;
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
        Ok(Outcome::Older) => Err(n!("The relay did not seal the line as it always has: someone between may be in the way").into()),
        Err(e) => Err(crate::lang::fill(n!("The relay could not be trusted: {}"), &[&e])),
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
    while let Some(cfg) = effective().filter(|c| c.enabled && c.direct) {
        match l.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(false);
                let Some(slot) = guard.take_call(&place(s.peer_addr().ok().map(|a| a.ip()))) else { continue };
                let guard = guard.clone();
                thread::spawn(move || {
                    let _slot = slot;
                    if crate::net::accept_direct(&mut s, &cfg.id) {
                        let from = s.peer_addr().ok().map(|a| a.ip());
                        serve_call(s, &cfg, &guard, false, from);
                    }
                });
            }
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// One viewer: a connection of its own through the relay at once (the relay waits only so
/// long), then the password checked in turn, then the agent.
fn answer_call(cfg: &Config, nonce: &str, guard: &Guard, from: Option<std::net::IpAddr>) {
    // The other service answers now (the screen just locked, or the desktop just opened): the
    // viewer tries again in a moment and reaches it.
    if standby() {
        return;
    }
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
        note_visit(serde_json::json!({ "refused": n!("its address is not on the list"), "at": crate::sys::now_secs(), "from": from_text }));
        crate::net::turn_away_address(&mut s);
        return;
    }
    // The exchange runs at the viewer's pace with nobody waiting on it; only checking its proof
    // of the password takes the turn, after the delay wrong ones have earned.
    let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
    // The first check is of the password, the next of the two-step code.
    let checks = std::cell::Cell::new(0);
    let at = place(from);
    let judge = |check: &mut dyn FnMut() -> bool| -> io::Result<bool> {
        let what = if checks.replace(checks.get() + 1) == 0 { n!("a wrong password") } else { n!("a wrong two-step code") };
        if guard.locked(&at) {
            return Err(io::Error::other("no passwords taken now"));
        }
        // A place let in lately waits only for its own wrong ones, never behind others' guesses.
        let _turn = if guard.known(&at) {
            None
        } else {
            if guard.waiting.fetch_add(1, Ordering::AcqRel) >= 2 {
                guard.waiting.fetch_sub(1, Ordering::AcqRel);
                return Err(io::Error::other("busy"));
            }
            let turn = guard.turn.lock().unwrap_or_else(|e| e.into_inner());
            guard.waiting.fetch_sub(1, Ordering::AcqRel);
            Some(turn)
        };
        thread::sleep(guard.delay(&at));
        let right = check();
        if right {
            guard.passed(&at)
        } else {
            guard.failed(&at);
            note_visit(serde_json::json!({ "refused": what, "at": crate::sys::now_secs(), "from": from_text }));
            crate::team::client::report(serde_json::json!({ "time": crate::sys::now_secs(), "kind": "refused", "words": what, "from": from.map(|a| a.to_string()) }));
        }
        Ok(right)
    };
    // The two-step code, or the token of a device trusted to come in without it.
    let by_device = std::cell::RefCell::new(None::<String>);
    let two_step = |code: &str| match code.strip_prefix(crate::net::TRUST_MARK) {
        Some(token) if cfg.trusts(token) => {
            *by_device.borrow_mut() = Some(token.to_string());
            true
        }
        Some(_) => false,
        None => crate::totp::check(&cfg.totp, code),
    };
    let totp = (!cfg.totp.is_empty()).then_some(&two_step as crate::net::TwoStep);
    // Through a proxy everything goes its way: no straight path around it.
    let punch = (relayed && cfg.punch && crate::proxy::none()).then(|| crate::net::Punch { viewer: false, relay: cfg.relay.clone(), id: String::new() });
    // A team's policy may let in its people alone (their tickets), no password at all.
    let policy = crate::team::client::policy();
    let passwords = policy.as_ref().is_none_or(|p| p.password);
    let lasting = cfg.passwords.lasting().then_some(cfg.password.as_str()).filter(|_| passwords);
    let one_time = cfg.passwords.one_time().then_some(cfg.one_time.as_str()).filter(|p| !p.is_empty() && passwords);
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
    let requests: Option<&dyn Fn() -> bool> = (cfg.requests && passwords).then_some(&take_request as &dyn Fn() -> bool);
    let team_check = |ticket: &str| crate::team::client::check_ticket(ticket, &cfg.id);
    let team = policy.is_some().then_some(&team_check as crate::net::TeamCheck);
    // Only some IDs, each from the device it first came from.
    let who = std::cell::RefCell::new(None::<(String, String)>);
    let check_id = |id: &str, key: &[u8; 32]| {
        let key = crate::sys::hex(key);
        let known = stored().ok().flatten().and_then(|c| id_key(&c, id)).or_else(|| id_key(cfg, id));
        let verdict = if !id_listed(&cfg.ids, id) {
            Err(crate::net::IdRefused::NotListed)
        } else if known.as_ref().is_some_and(|k| *k != key) {
            Err(crate::net::IdRefused::OtherDevice)
        } else {
            if known.is_none() {
                pin_device(id, &key);
            }
            *who.borrow_mut() = Some((id.to_string(), key.clone()));
            Ok(())
        };
        if let Err(why) = verdict {
            let spaced = crate::computers::spaced(id);
            let what = crate::lang::fill(if why == crate::net::IdRefused::OtherDevice { n!("ID {} from another device than before") } else { n!("ID {}, not on the list") }, &[&spaced]);
            note_visit(serde_json::json!({ "refused": what, "at": crate::sys::now_secs(), "from": from_text }));
        }
        verdict
    };
    let ids = (!cfg.ids.trim().is_empty()).then_some(&check_id as crate::net::Ids);
    let Ok((r, w, came)) = crate::net::handshake_agent_passwords(s, lasting, one_time, requests, totp, &judge, punch, ids, team) else {
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
    let grant = match &came {
        // A team ticket says what this person may do here, within the team's policy.
        crate::net::Came::Team(t) => {
            let cap = policy.as_ref().map_or(crate::grant::ALL, |p| p.rights);
            (t.rights & cap & crate::grant::ALL) | if t.ask || cfg.ask { crate::grant::ASK } else { 0 }
        }
        _ if code.is_some() => cfg.grant() | crate::grant::ASK,
        _ => cfg.grant(),
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let by_device = by_device.into_inner();
    // 0: no two-step code asked, 1: a trusted device instead of it, 2: with it.
    let step = match &by_device {
        _ if cfg.totp.is_empty() => 0,
        Some(token) => {
            trusted_came(token);
            1
        }
        None => 2,
    };
    // One who gave the code may ask to be trusted for next time: the agent has a token to give.
    let token = (!cfg.totp.is_empty() && by_device.is_none() && code.is_none()).then(|| crate::sys::hex(&crate::sys::random(32)));
    let trust_offer = token.map(|t| (t, from_text.clone().unwrap_or_default()));
    let by_device = by_device.filter(|_| !cfg.totp.is_empty());
    let (at, ender) = (crate::sys::now_secs(), w.ender());
    let who = who.into_inner();
    let how = came_how(&came, step, who.as_ref().map(|(id, _)| crate::computers::spaced(id)));
    note_visit(serde_json::json!({ "in": n, "at": at, "from": from_text, "how": how, "path": if relayed { n!("through the relay") } else { n!("on this network") } }));
    let (team_how, team_user) = match &came {
        crate::net::Came::Team(t) => ("team", Some(t.user.clone())),
        crate::net::Came::OneTime => ("one-time", None),
        crate::net::Came::Request(_) => ("request", None),
        crate::net::Came::Lasting => ("password", None),
    };
    crate::team::client::report(serde_json::json!({ "time": at, "kind": "in", "session": n, "how": team_how, "user": team_user, "from": from.map(|a| a.to_string()), "rights": grant & crate::grant::ALL }));
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).push((n, Welcome::new(cfg, &came, from, by_device, who), w.ender()));
    SINCE.lock().unwrap_or_else(|e| e.into_inner()).push((n, at));
    VIEWERS.fetch_add(1, Ordering::Relaxed);
    // Files moved or changed in the session go to the team's audit log too, as who did it.
    let team_base = serde_json::json!({ "kind": "file", "session": n, "how": team_how, "user": team_user, "from": from.map(|a| a.to_string()) });
    let asking = Asking { code: code.as_deref(), how: &how, from: from_text.as_deref().unwrap_or("") };
    let _ = serve_viewer(r, w, grant, asking, cfg, trust_offer, team_base);
    VIEWERS.fetch_sub(1, Ordering::Relaxed);
    let path = match ender.straight() {
        Some(a) => crate::lang::fill(n!("straight from {}"), &[&a.ip()]),
        None if relayed => n!("through the relay").into(),
        None => n!("on this network").into(),
    };
    note_visit(serde_json::json!({ "out": n, "at": at, "secs": crate::sys::now_secs().saturating_sub(at), "path": path }));
    crate::team::client::report(serde_json::json!({ "time": crate::sys::now_secs(), "kind": "out", "session": n, "how": team_how, "user": team_user, "from": from.map(|a| a.to_string()), "secs": crate::sys::now_secs().saturating_sub(at) }));
    if once {
        one_time_used(&cfg.one_time);
    }
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _, _)| *k != n);
    SINCE.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _)| *k != n);
}

/// How a viewer came in, for the record: whole phrases, so the window can show each in the
/// reader's language. `step` as in `serve_call`; `id` when only some IDs may come in.
fn came_how(came: &crate::net::Came, step: u8, id: Option<String>) -> String {
    use crate::net::Came;
    let phrase = match (came, step, id.is_some()) {
        (Came::Lasting, 0, false) => n!("with the lasting password"),
        (Came::Lasting, 0, true) => n!("with the lasting password, as ID {}"),
        (Came::Lasting, 1, false) => n!("with the lasting password, from a trusted device"),
        (Came::Lasting, 1, true) => n!("with the lasting password, from a trusted device, as ID {}"),
        (Came::Lasting, _, false) => n!("with the lasting password and the two-step code"),
        (Came::Lasting, _, true) => n!("with the lasting password and the two-step code, as ID {}"),
        (Came::OneTime, 0, false) => n!("with a one-time password"),
        (Came::OneTime, 0, true) => n!("with a one-time password, as ID {}"),
        (Came::OneTime, 1, false) => n!("with a one-time password, from a trusted device"),
        (Came::OneTime, 1, true) => n!("with a one-time password, from a trusted device, as ID {}"),
        (Came::OneTime, _, false) => n!("with a one-time password and the two-step code"),
        (Came::OneTime, _, true) => n!("with a one-time password and the two-step code, as ID {}"),
        (Came::Request(_), 0, false) => n!("by asking, at the screen"),
        (Came::Request(_), 0, true) => n!("by asking, at the screen, as ID {}"),
        (Came::Request(_), 1, false) => n!("by asking, at the screen, from a trusted device"),
        (Came::Request(_), 1, true) => n!("by asking, at the screen, from a trusted device, as ID {}"),
        (Came::Request(_), _, false) => n!("by asking, at the screen and the two-step code"),
        (Came::Request(_), _, true) => n!("by asking, at the screen and the two-step code, as ID {}"),
        (Came::Team(t), ..) => return crate::lang::fill(n!("with a team ticket, as {}"), &[&if t.display.is_empty() { t.user.clone() } else { format!("{} ({})", t.display, t.user) }]),
    };
    crate::lang::fill(phrase, &[&id.unwrap_or_default()])
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

/// The IDs of a list as typed: split at commas, semicolons and new lines, and at spaces too
/// where the part is no ID with its digits spaced ("123 456 789").
fn id_words(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in list.split([',', ';', '\n']).map(str::trim).filter(|p| !p.is_empty()) {
        let joined: String = part.chars().filter(|c| !c.is_whitespace()).collect();
        if (6..=12).contains(&joined.len()) || joined.contains('*') && !part.contains(char::is_whitespace) {
            out.push(joined);
        } else {
            out.extend(part.split_whitespace().map(str::to_string));
        }
    }
    out
}

/// An ID of a list: 6 to 12 digits, or some digits and stars standing for any.
fn id_pattern(w: &str) -> bool {
    w.bytes().all(|b| b.is_ascii_digit() || b == b'*') && w.bytes().any(|b| b.is_ascii_digit()) && (w.contains('*') || valid_id(w)) && w.len() <= 12
}

/// What in an ID list is none.
pub fn bad_ids(list: &str) -> Vec<String> {
    id_words(list).into_iter().filter(|w| !id_pattern(w)).collect()
}

/// An ID list as kept: its IDs, a comma between.
pub fn tidy_ids(list: &str) -> String {
    id_words(list).join(", ")
}

/// Whether `id` is on the list (a star standing for any digits).
pub fn id_listed(list: &str, id: &str) -> bool {
    fn fits(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((b'*', rest)) => (0..=s.len()).any(|i| fits(rest, &s[i..])),
            Some((c, rest)) => s.first() == Some(c) && fits(rest, &s[1..]),
        }
    }
    id_words(list).iter().filter(|w| id_pattern(w)).any(|w| fits(w.as_bytes(), id.as_bytes()))
}

/// This computer's own ID, as a viewer shows it to a computer that lets in only some: its remote
/// access ID, whether remote access is on or not (as the service has it, on Windows).
pub fn own_id() -> Option<String> {
    stored()
        .ok()
        .flatten()
        .map(|c| c.id)
        .or_else(|| {
            // (Not in tests: that file is the running app's.)
            if cfg!(test) {
                return None;
            }
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(state_file()?).ok()?).ok()?;
            v.get("id")?.as_str().map(str::to_string)
        })
        .filter(|i| valid_id(i))
}

/// This device's key (its secret half), made the first time and kept in the user's settings
/// folder for the user alone.
pub fn device_key() -> [u8; 32] {
    static KEY: Mutex<Option<[u8; 32]>> = Mutex::new(None);
    let mut k = KEY.lock().unwrap_or_else(|e| e.into_inner());
    *k.get_or_insert_with(|| {
        let file = crate::hosts::config_dir().map(|d| d.join("device-key"));
        if let Some(key) = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()) {
            return key;
        }
        let key: [u8; 32] = random(32).try_into().expect("32 bytes");
        if let Some(f) = &file {
            let _ = f.parent().map(std::fs::create_dir_all);
            let _ = crate::sys::write_whole(f, &key, true, true);
        }
        key
    })
}

/// The device `id` came from first, as these settings know it.
fn id_key(c: &Config, id: &str) -> Option<String> {
    c.id_keys.iter().find(|(i, _, _)| i == id).map(|(_, k, _)| k.clone())
}

/// Keeps `key` as the device `id` comes from (its first).
fn pin_device(id: &str, key: &str) {
    if let Ok(Some(mut c)) = stored()
        && id_key(&c, id).is_none()
    {
        c.id_keys.push((id.to_string(), key.to_string(), crate::sys::now_secs()));
        if let Err(e) = save(&c) {
            eprintln!("remote access: the device of ID {id} was not kept: {e}");
        }
    }
}

/// The devices of IDs no longer on the list are forgotten with them.
pub fn keep_listed_devices(c: &mut Config) {
    let list = c.ids.clone();
    c.id_keys.retain(|(i, _, _)| id_listed(&list, i));
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
        edit(|c| {
            let new = c.one_time == used;
            if new {
                c.one_time = crate::net::new_one_time();
            }
            new
        });
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
    /// The viewer asked to be trusted (`ToAgent::TrustMe`).
    trust_me: bool,
    /// The message being kept whole (`NOTED`), as far as it came.
    body: Option<Vec<u8>>,
    /// Uploads, downloads and changes to files the viewer asked for, for the record (None: one
    /// that did not read).
    files: Vec<Option<crate::proto::ToAgent>>,
}

/// A viewer asked for administrator rights for remote access run in the app (Windows): the
/// window asks its person (`take_elevation_ask`).
#[cfg(windows)]
static ELEVATION_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a viewer asked for administrator rights since the last look.
#[cfg(windows)]
pub fn take_elevation_ask() -> bool {
    ELEVATION_ASKED.swap(false, Ordering::AcqRel)
}

/// What of a viewer's messages goes to a team's audit log: uploads (13, 24, and 52 going on
/// with one), downloads (17, 26, and 53) and folders made, names changed and files deleted (25).
const NOTED: [u8; 7] = [13, 17, 24, 25, 26, 52, 53];
/// What of such a message is kept to read: names and paths come first, and are short.
const NOTED_MOST: usize = 64 * 1024;

/// A file event in English words, which the console and the app translate where they show it.
fn file_words(m: &Option<crate::proto::ToAgent>) -> Option<String> {
    use crate::proto::{FileOp, ToAgent};
    let fill = crate::lang::fill;
    let into = |dir: &str, name: &str| if dir.is_empty() { format!("Downloads/{name}") } else { format!("{}/{name}", dir.trim_end_matches(['/', '\\'])) };
    let Some(m) = m else { return Some(n!("Did something with files that the record could not read").into()) };
    Some(match m {
        ToAgent::FileStart { size, dir, name, .. } => fill(n!("Uploaded {} ({})"), &[&into(dir, name), &crate::sys::human(*size)]),
        // Going on after a broken connection: its own line too, whatever it says it has already.
        ToAgent::FileResume { size, dir, name, .. } => fill(n!("Uploaded {} ({})"), &[&into(dir, name), &crate::sys::human(*size)]),
        ToAgent::Fetch { path, .. } | ToAgent::FetchFrom { path, .. } => fill(n!("Downloaded {}"), &[path]),
        ToAgent::FileOp { op: FileOp::Mkdir, path, .. } => fill(n!("Made the folder {}"), &[path]),
        ToAgent::FileOp { op: FileOp::Rename, path, arg } => fill(n!("Renamed {} to {}"), &[path, arg]),
        ToAgent::FileOp { op: FileOp::Delete, path, .. } => fill(n!("Deleted {}"), &[path]),
        _ => return None,
    })
}

/// What a viewer sends by itself: settings, acknowledgements, pings, pausing, refreshing,
/// accessibility state, density, transfers it resumes after a broken connection, and asking to
/// be trusted.
const AUTOMATIC: [u8; 12] = [1, 2, 7, 8, 18, 22, 36, 46, 51, 52, 53, 54];

impl Frames {
    /// Follows `b`; true when a message it begins is the person's doing.
    fn feed(&mut self, mut b: &[u8]) -> bool {
        let mut person = false;
        while !b.is_empty() {
            if self.left > 0 {
                let n = self.left.min(b.len());
                if let Some(body) = self.body.as_mut() {
                    let room = NOTED_MOST.saturating_sub(body.len()).min(n);
                    body.extend_from_slice(&b[..room]);
                }
                (self.left, b) = (self.left - n, &b[n..]);
                if self.left == 0 {
                    self.noted();
                }
                continue;
            }
            let n = (5 - self.have).min(b.len());
            self.head[self.have..self.have + n].copy_from_slice(&b[..n]);
            (self.have, b) = (self.have + n, &b[n..]);
            if self.have == 5 {
                self.have = 0;
                self.left = u32::from_le_bytes([self.head[1], self.head[2], self.head[3], self.head[4]]) as usize;
                person |= !AUTOMATIC.contains(&self.head[0]);
                self.trust_me |= self.head[0] == crate::proto::KIND_TRUST_ME;
                #[cfg(windows)]
                if self.head[0] == crate::proto::KIND_ASK_ELEVATION {
                    ELEVATION_ASKED.store(true, Ordering::Release);
                }
                self.body = NOTED.contains(&self.head[0]).then(Vec::new);
                if self.left == 0 {
                    self.noted();
                }
            }
        }
        person
    }

    fn noted(&mut self) {
        if let Some(body) = self.body.take() {
            self.files.push(crate::proto::ToAgent::decode(self.head[0], &body).ok());
        }
    }

    fn between(&self) -> bool {
        self.have == 0 && self.left == 0
    }
}

/// `trust`: a token for the agent to give the viewer if it asks to be trusted, and where it is;
/// `audit`: the team's record of this session, for its file events.
/// What the person at the screen is told of a viewer who asks to come in: the two-step code both
/// screens show (a request), how they came (a team's person by name, an ID proven), from where.
struct Asking<'a> {
    code: Option<&'a str>,
    how: &'a str,
    from: &'a str,
}

fn serve_viewer(
    mut r: crate::net::SealedReader,
    mut w: crate::net::SealedWriter,
    grant: u8,
    asking: Asking,
    cfg: &Config,
    mut trust: Option<(String, String)>,
    audit: serde_json::Value,
) -> Result<(), String> {
    let (cameras, idle) = (cfg.cameras, cfg.idle);
    // The session's own pings come every second; two minutes of nothing means it is gone.
    let ender = w.ender();
    ender.set_idle_limit(Duration::from_secs(120));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    // Root's at the login screen: the screen as the display hardware shows it, and nothing but
    // keyboard and mouse (no terminal, files or clipboard as root).
    let (capture, grant) = if at_login_screen() { ("kms", grant & crate::grant::CONTROL) } else { ("auto", grant) };
    cmd.args(["--agent", "--capture", capture, "--by-id", "--grant", &grant.to_string()]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(c) = asking.code {
        cmd.args(["--code", c]);
    }
    cmd.args(["--who-how", asking.how, "--who-from", asking.from]);
    cmd.args(["--clip-way", &cfg.clip_way.to_string(), "--files-way", &cfg.files_way.to_string()]);
    if cfg.plain_wallpaper {
        cmd.arg("--plain-wallpaper");
    }
    if cameras {
        cmd.arg("--cameras");
    }
    crate::sys::quiet(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    crate::sys::tie(&child);
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    // Before anything of the viewer's, always (so the viewer's own can never be taken for it):
    // the token the agent may hand out, if any.
    let (k, p) = crate::proto::ToAgent::TrustToken(trust.as_ref().map(|t| t.0.clone()).unwrap_or_default()).encode();
    crate::proto::write_msg(&mut stdin, k, &p).map_err(|e| e.to_string())?;

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
                for m in std::mem::take(&mut i.1.files) {
                    if let Some(words) = file_words(&m) {
                        let mut e = audit.clone();
                        e["time"] = crate::sys::now_secs().into();
                        e["words"] = words.into();
                        crate::team::client::report(e);
                    }
                }
                // Trusted, pending until it comes back by the token the agent gives it.
                if std::mem::take(&mut i.1.trust_me)
                    && let Some((token, from)) = trust.take()
                {
                    trust_device(&token, &from);
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
            let on = || recent().filter(|c| c.enabled && !service_runs_cached());
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
        let Ok(o) = Command::new("sc").args(["qc", crate::service::NAME]).creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW).output() else { return true };
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
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("RUNNING"));
    }
    #[cfg(target_os = "macos")]
    {
        return Command::new("launchctl")
            .args(["print", &launchd_target()])
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("state = running"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return Command::new("systemctl").args(["--user", "is-active", "--quiet", UNIT]).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(all(unix, not(target_os = "macos")))]
const UNIT: &str = "sshremote-access.service";

/// macOS: the launchd agent that runs remote access in the signed-in session, and where it is
/// kept.
#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "dev.sshremote.access";

#[cfg(target_os = "macos")]
fn launchd_target() -> String {
    // SAFETY: getuid cannot fail.
    format!("gui/{}/{LAUNCHD_LABEL}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<(), String> {
    let o = Command::new("launchctl").args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(()) } else { Err(format!("launchctl {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim())) }
}

/// The sign-in entry that starts the icon in the top bar.
#[cfg(all(unix, not(target_os = "macos")))]
fn autostart_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join("sshremote-tray.desktop"))
}

#[cfg(target_os = "macos")]
fn unit_file() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist")))
}

/// macOS: the system's launch agent that runs remote access at the login window, as root, with
/// the settings of the person who set it up there.
#[cfg(target_os = "macos")]
const LOGIN_WINDOW_PLIST: &str = "/Library/LaunchAgents/dev.sshremote.loginwindow.plist";

/// Puts the login window's launch agent in place or takes it away, as root: directly when this
/// runs as root, else through macOS's own question for an administrator's password.
#[cfg(target_os = "macos")]
pub fn login_window(on: bool) -> Result<(), String> {
    let f = std::path::Path::new(LOGIN_WINDOW_PLIST);
    let script = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let base = crate::hosts::config_dir().and_then(|d| d.parent().map(PathBuf::from)).ok_or("no settings folder")?;
        let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>dev.sshremote.loginwindow</string>\n<key>ProgramArguments</key><array><string>{}</string><string>remote-access</string><string>run</string><string>--service</string></array>\n<key>EnvironmentVariables</key><dict><key>XDG_CONFIG_HOME</key><string>{}</string></dict>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>LimitLoadToSessionType</key><string>LoginWindow</string>\n<key>ProcessType</key><string>Interactive</string>\n</dict></plist>\n",
            xml(&exe.display().to_string()),
            xml(&base.display().to_string())
        );
        // Already so (an update, a second install): nothing to ask for.
        if std::fs::read_to_string(f).is_ok_and(|t| t == plist) {
            return Ok(());
        }
        let tmp = std::env::temp_dir().join(format!("dev.sshremote.loginwindow-{}.plist", std::process::id()));
        // It goes into a shell line between single quotes (and that into AppleScript's quotes).
        if tmp.display().to_string().contains(['\'', '"', '\\', '\n']) {
            return Err(crate::lang::fill(n!("{}: no place for the setting that can be passed on safely"), &[&tmp.display()]));
        }
        std::fs::write(&tmp, plist).map_err(|e| format!("{}: {e}", tmp.display()))?;
        format!("/usr/bin/install -o root -g wheel -m 644 '{}' '{}'", tmp.display(), f.display())
    } else {
        if !f.exists() {
            return Ok(());
        }
        format!("/bin/rm -f '{}'", f.display())
    };
    // SAFETY: geteuid cannot fail.
    let out = if unsafe { libc::geteuid() } == 0 {
        Command::new("/bin/sh").args(["-c", &script]).output()
    } else {
        let what = if on { "be reachable at the login window, before anyone signs in" } else { "no longer be reachable at the login window" };
        Command::new("osascript").args(["-e", &format!("do shell script \"{script}\" with prompt \"SSH Remote wants to {what}.\" with administrator privileges")]).output()
    }
    .map_err(|e| e.to_string())?;
    if on {
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("dev.sshremote.loginwindow-{}.plist", std::process::id())));
    }
    if out.status.success() {
        Ok(())
    } else {
        Err(crate::lang::fill(n!("the login window was not set up: {}"), &[&String::from_utf8_lossy(&out.stderr).trim()]))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unit_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("systemd").join("user").join(UNIT))
}

/// Starts remote access with the computer: the Windows service (elevated), a systemd user unit
/// that also runs before anyone opens the window (with lingering, before sign-in), or on macOS a
/// launchd agent of the signed-in session (where the screen is, and the app's permissions count).
pub fn install_service() -> Result<(), String> {
    #[cfg(windows)]
    return crate::service::install();
    #[cfg(target_os = "macos")]
    {
        let f = unit_file().ok_or(n!("no home folder"))?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(f.parent().ok_or(n!("bad path"))?).map_err(|e| e.to_string())?;
        let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LAUNCHD_LABEL}</string>\n<key>ProgramArguments</key><array><string>{}</string><string>remote-access</string><string>run</string><string>--service</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>LimitLoadToSessionType</key><string>Aqua</string>\n<key>ProcessType</key><string>Interactive</string>\n</dict></plist>\n",
            xml(&exe.display().to_string())
        );
        std::fs::write(&f, plist).map_err(|e| e.to_string())?;
        let _ = launchctl(&["bootout", &launchd_target()]);
        // SAFETY: getuid cannot fail.
        launchctl(&["bootstrap", &format!("gui/{}", unsafe { libc::getuid() }), &f.display().to_string()])?;
        // Before anyone signs in too, where an administrator lets it (the rest works without).
        if let Err(e) = login_window(true) {
            eprintln!("{e}");
        }
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let f = unit_file().ok_or(n!("no home folder"))?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(f.parent().ok_or(n!("bad path"))?).map_err(|e| e.to_string())?;
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
        // At the login screen and on a locked screen too, where an administrator lets it (the
        // rest works without).
        #[cfg(target_os = "linux")]
        if let Err(e) = crate::loginscreen::set(true) {
            eprintln!("{e}");
        }
        // Its icon in the top bar, with every sign-in and now.
        if let Some(a) = autostart_file() {
            let _ = std::fs::create_dir_all(a.parent().ok_or(n!("bad path"))?);
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
    Err(n!("not on this system").into())
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
            false => Err(n!("administrator rights were not given").into()),
        };
    }
    #[cfg(target_os = "macos")]
    {
        let _ = verb;
        let f = unit_file().ok_or(n!("no home folder"))?;
        // Kept alive by launchd while loaded: stopping is unloading it, starting loading it again.
        return if start {
            // SAFETY: getuid cannot fail.
            launchctl(&["bootstrap", &format!("gui/{}", unsafe { libc::getuid() }), &f.display().to_string()]).or_else(|_| launchctl(&["kickstart", &launchd_target()]))
        } else {
            launchctl(&["bootout", &launchd_target()])
        };
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let ok = Command::new("systemctl").args(["--user", verb, UNIT]).status().map_err(|e| e.to_string())?;
        return if ok.success() { Ok(()) } else { Err(format!("systemctl --user {verb} {UNIT} failed")) };
    }
    #[allow(unreachable_code)]
    Err(n!("not on this system").into())
}

pub fn uninstall_service() -> Result<(), String> {
    #[cfg(windows)]
    return crate::service::uninstall();
    #[cfg(target_os = "macos")]
    {
        let _ = launchctl(&["bootout", &launchd_target()]);
        if let Some(f) = unit_file() {
            let _ = std::fs::remove_file(f);
        }
        return login_window(false);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", UNIT]).status();
        if let Some(f) = unit_file() {
            let _ = std::fs::remove_file(f);
        }
        #[cfg(target_os = "linux")]
        if crate::loginscreen::installed()
            && let Err(e) = crate::loginscreen::set(false)
        {
            eprintln!("{e}");
        }
        // The icon notices the service went and goes too.
        if let Some(a) = autostart_file() {
            let _ = std::fs::remove_file(a);
        }
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err(n!("not on this system").into())
}

/// `sshremote remote-access [status | on | off | run | id [NUMBER] | clipboard|files both|out|in | wallpaper on|off | password [NEW] | relay HOST | install | uninstall]`
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
        if !c.ids.is_empty() {
            println!("ids:      only {}", c.ids);
            for (id, key, at) in &c.id_keys {
                println!("          {} from its device {}.. since {}", crate::computers::spaced(id), &key[..key.len().min(12)], crate::hosts::ago(*at));
            }
        }
        println!("asks:     {}", if c.ask { "yes, the person at this computer lets each viewer in" } else { "no, the password is enough" });
        println!("viewers:  may use {}", crate::grant::words(c.allow));
        println!("nearby:   {}", match (c.lan, c.direct) {
            (true, true) => "found by computers on this network, which may come straight in",
            (true, false) => "found by computers on this network",
            (false, true) => "not found, but computers on this network may come straight in",
            (false, false) => "not found by computers on this network",
        });
        let trusted = c.trusted_now().count();
        println!("two-step: {}", match (c.totp.is_empty(), trusted) {
            (true, _) => "off, the password is enough".to_string(),
            (false, 0) => "on, viewers give the code of the authenticator app with the password".to_string(),
            (false, n) => format!("on, viewers give the code with the password, but for {n} trusted {} (remote-access trusted)", if n == 1 { "device" } else { "devices" }),
        });
        match crate::proxy::current() {
            Ok(Some(p)) => println!("proxy:    the relay is reached through {}", p.shown()),
            Ok(None) => {}
            Err(e) => println!("proxy:    {e}: the relay is not reached"),
        }
        if c.record {
            println!("record:   sessions by ID are recorded in {}", recordings_dir().map(|d| d.display().to_string()).unwrap_or_default());
        }
        let way = |w: u8| ["both ways", "from this computer only", "to this computer only"][w.min(2) as usize];
        if c.plain_wallpaper {
            println!("desktop:  its picture is taken away during sessions by ID");
        }
        if c.clip_way != 0 || c.files_way != 0 {
            println!("one way:  clipboard {}, files {}", way(c.clip_way), way(c.files_way));
        }
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
        // The proxy the relay is reached through, for remote access and this person's viewers.
        Some("proxy") => {
            let url = match args.get(1).map(String::as_str) {
                Some("off" | "none") => String::new(),
                Some(u) => crate::proxy::Proxy::parse(u).map(|p| p.url())?,
                None => return Err("usage: sshremote remote-access proxy socks5://HOST:PORT | http://[NAME:PASSWORD@]HOST:PORT | off".into()),
            };
            c.proxy = url.clone();
            save(&c)?;
            crate::hosts::set_setting("proxy", &url);
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
        // Which way the clipboard and files go for viewers by ID.
        Some(what @ ("clipboard" | "files")) => {
            let way = match args.get(1).map(String::as_str) {
                Some("both") => 0,
                Some("out") => 1,
                Some("in") => 2,
                _ => return Err(format!("usage: sshremote remote-access {what} both|out|in   (out: from this computer only, in: to it only)")),
            };
            if what == "clipboard" {
                c.clip_way = way;
            } else {
                c.files_way = way;
            }
            save(&c)?;
            show(&c);
            Ok(())
        }
        // Devices let in without the two-step code, and forgetting them.
        Some("trusted") => {
            if args.get(1).map(String::as_str) == Some("forget") {
                c.trusted.clear();
                save(&c)?;
                println!("no device is trusted now: each gives the code again");
                return Ok(());
            }
            let now: Vec<&Trusted> = c.trusted_now().collect();
            if now.is_empty() {
                println!("no trusted devices: every viewer gives the two-step code");
            }
            for t in now {
                let last = if t.used == 0 { "not back by it yet".to_string() } else { format!("last in {}", crate::sys::local_time(t.used)) };
                println!("from {} since {}, {last} (until {})", if t.from.is_empty() { "?" } else { &t.from }, crate::sys::local_time(t.at), crate::sys::local_time(t.at + TRUST_DAYS * 86_400));
            }
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
            if c.team.is_object() {
                return Err("this computer is in a team, which knows it by its ID: leave the team first (sshremote team leave)".into());
            }
            // One of one's own choosing, or a new one at random.
            c.id = match args.get(1) {
                Some(id) => {
                    let id: String = id.chars().filter(|c| !c.is_whitespace()).collect();
                    if !own_id_ok(&id) {
                        return Err("usage: sshremote remote-access id [6 to 12 digits, not starting with 0]".into());
                    }
                    id
                }
                None => new_id(),
            };
            save(&c)?;
            println!("ID: {}", crate::computers::spaced(&c.id));
            Ok(())
        }
        // Switches, each `on` or `off`.
        Some(what @ ("straight" | "lan" | "direct" | "requests" | "cameras" | "record" | "wallpaper")) => {
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
                "record" => c.record = on,
                "wallpaper" => c.plain_wallpaper = !on,
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
        Some("ids") => {
            let list = args[1..].join(" ");
            if list.trim().is_empty() {
                return Err("usage: sshremote remote-access ids ID ... | anyone   (e.g. ids 123456789, 98765*)".into());
            }
            c.ids = if list.trim() == "anyone" { String::new() } else { tidy_ids(&list) };
            let bad = bad_ids(&c.ids);
            if !bad.is_empty() {
                return Err(format!("not an ID: {} (6 to 12 digits, a star standing for any)", bad.join(", ")));
            }
            keep_listed_devices(&mut c);
            save(&c)?;
            println!("{}", if c.ids.is_empty() { "viewers may come from any ID".to_string() } else { format!("viewers come in only as {}, each from the device it first came from", c.ids) });
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
        // SSH Remote's printer: what is printed to it prints on the computer viewing this one.
        Some("printer") => match args.get(1).map(String::as_str) {
            Some(w @ ("on" | "off")) => crate::print::set_up(w == "on").map(|t| println!("{t}")),
            None => {
                println!("printer:  {}", if crate::print::installed() { format!("\"{}\" prints on the computer viewing this one", crate::print::NAME) } else { "none (remote-access printer on, as an administrator)".into() });
                Ok(())
            }
            _ => Err("usage: sshremote remote-access printer [on|off]".into()),
        },
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Some("login-window") => match args.get(1).map(String::as_str) {
            #[cfg(target_os = "macos")]
            Some(w @ ("on" | "off")) => login_window(w == "on").map(|()| println!("at the login window: {w}")),
            #[cfg(target_os = "linux")]
            Some(w @ ("on" | "off")) => crate::loginscreen::set(w == "on").map(|()| println!("at the login screen and on a locked screen: {w}")),
            _ => Err("usage: sshremote remote-access login-window on|off".into()),
        },
        // `--service`: run by the service or unit itself; otherwise it leaves remote access to a
        // service that runs it. `--parent PID`: ends with that process (a window that started it).
        Some("run") => {
            if stored()?.is_none() {
                save(&c)?;
            }
            // Older units lack `--service`: systemd names the process it started itself (248+).
            let unit_started = std::env::var("SYSTEMD_EXEC_PID").is_ok_and(|p| p == std::process::id().to_string());
            let service = args.iter().any(|a| a == "--service") || unit_started;
            // At a Mac's login window just after start-up: launchd starts it again, by then inside.
            #[cfg(target_os = "macos")]
            if service && crate::capture::outside_the_login_window() {
                return Err("the login window's screen is not up yet".into());
            }
            let parent: Option<u32> = args.iter().position(|a| a == "--parent").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok());
            // The person's own agent (not root's at the login window) keeps itself up to date.
            // SAFETY: getuid cannot fail.
            #[cfg(target_os = "macos")]
            if service && unsafe { libc::getuid() } != 0 {
                thread::spawn(|| crate::update::unattended(|| viewers_here() == 0));
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: getuid cannot fail.
                let uid = unsafe { libc::getuid() };
                let r = if args.iter().any(|a| a == "--login-screen") {
                    let owner: u32 = std::env::var(crate::loginscreen::OWNER_VAR).ok().and_then(|u| u.parse().ok()).ok_or("--login-screen: whose desktop it serves around was not said")?;
                    // Root's copy follows the person's program to newer releases.
                    thread::spawn(|| {
                        loop {
                            if crate::loginscreen::follow() {
                                std::process::exit(0);
                            }
                            thread::sleep(Duration::from_secs(6 * 3600));
                        }
                    });
                    Role::LoginScreen(owner)
                } else if service && uid != 0 {
                    thread::spawn(|| crate::update::unattended(|| viewers_here() == 0));
                    Role::Owner(uid)
                } else {
                    Role::Plain
                };
                let _ = ROLE.set(r);
            }
            eprintln!("remote access as {} through {}", crate::computers::spaced(&c.id), c.relay);
            // Run by hand: the line to the relay as it changes, so a refusal (the relay full, the
            // ID taken) is not left unsaid while the computer cannot be reached.
            if !service {
                thread::spawn(|| {
                    let mut said: Option<State> = None;
                    loop {
                        let now = STATE.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        if now != said {
                            match &now {
                                Some(State::Online) => eprintln!("remote access: online"),
                                Some(State::Failed(why)) => eprintln!("remote access: not reachable: {why}"),
                                _ => {}
                            }
                            said = now;
                        }
                        thread::sleep(Duration::from_secs(1));
                    }
                });
            }
            // The service says how it stands, for the icon in the notification area (root's at the
            // login screen leaves that to the person's own).
            if service && !at_login_screen() {
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
                recent().filter(|c| c.enabled && (service || !service_runs_cached()))
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
        _ => Err("usage: sshremote remote-access [status | log | idle MINUTES|off | trusted [forget] | proxy URL|off | record on|off | on | off | run | id | password [NEW] | relay HOST | ask on|off | allow RIGHTS | straight|lan|direct|requests on|off | viewers | end N | passwords lasting|one-time|both | one-time | install | uninstall | printer [on|off] | login-window on|off (macOS, Linux) | service start|stop]".into()),
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
        assert!(!f.trust_me);
        f.feed(&msg(ToAgent::TrustMe));
        assert!(f.trust_me, "asking to be trusted is seen");
        // Files moved or changed are kept for the record, a byte at a time too; their data not.
        let up = msg(ToAgent::FileStart { id: 1, size: 2048, dir: "/srv/share/".into(), name: "a b.txt".into() });
        let data = msg(ToAgent::FileData { id: 1, data: vec![7; 2048] });
        let down = msg(ToAgent::Fetch { tag: 3, path: "C:\\Users\\ana\\report.pdf".into() });
        let gone = msg(ToAgent::FileOp { op: crate::proto::FileOp::Delete, path: "/tmp/x".into(), arg: String::new() });
        for b in [up, data, down, gone].concat() {
            f.feed(&[b]);
        }
        assert!(f.between());
        let words: Vec<String> = std::mem::take(&mut f.files).iter().filter_map(file_words).collect();
        assert_eq!(words, ["Uploaded /srv/share/a b.txt (2.0 KB)", "Downloaded C:\\Users\\ana\\report.pdf", "Deleted /tmp/x"]);
        // Going on with a transfer is recorded too; one padded past what is kept, all the same.
        let on = msg(ToAgent::FetchFrom { tag: 1, path: "/etc/hosts".into(), name: "hosts".into(), from: 0, tail: [0; 32] });
        let (k, mut p) = ToAgent::FileOp { op: crate::proto::FileOp::Mkdir, path: "/srv/new".into(), arg: String::new() }.encode();
        p.extend(std::iter::repeat_n(0u8, 100_000));
        let mut padded = Vec::new();
        crate::proto::write_msg(&mut padded, k, &p).unwrap();
        f.feed(&[on, padded].concat());
        let words: Vec<String> = std::mem::take(&mut f.files).iter().filter_map(file_words).collect();
        assert_eq!(words, ["Downloaded /etc/hosts", "Made the folder /srv/new"]);
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
    fn id_lists_take_spaced_ids_and_stars() {
        assert_eq!(tidy_ids("123 456 789, 98765*"), "123456789, 98765*");
        assert_eq!(tidy_ids("123456789 987654321"), "123456789, 987654321", "two IDs a space apart");
        assert_eq!(tidy_ids("123456789\n111 222 333;"), "123456789, 111222333");
        assert!(bad_ids("123456789, 98765*").is_empty());
        assert_eq!(bad_ids("12345, abc, 1234567890123, *"), vec!["12345", "abc", "1234567890123", "*"]);
        let list = "123456789, 98765*, 4*9";
        assert!(id_listed(list, "123456789") && id_listed(list, "987650000") && id_listed(list, "409") && id_listed(list, "4123459"));
        assert!(!id_listed(list, "123456780") && !id_listed(list, "98764") && !id_listed(list, "4123450") && !id_listed("", "123456789"));
        let mut c = fresh();
        c.ids = "123456789".into();
        c.id_keys = vec![("123456789".into(), "aa".into(), 1), ("555555555".into(), "bb".into(), 2)];
        keep_listed_devices(&mut c);
        assert_eq!(c.id_keys.len(), 1, "a device goes with its ID off the list");
        let back = Config::from_json(&c.to_json()).unwrap();
        assert_eq!((back.ids, back.id_keys), (c.ids, c.id_keys), "kept as they were");
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
                    let e = crate::relaylink::key_from_hex(first.strip_prefix("secure ").unwrap()).unwrap();
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
        assert_eq!(known_key(&cfg), crate::relaylink::key_from_hex(crate::relaylink::PUBLIC_RELAY_KEY));
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
    fn wrong_passwords_lock_out_where_they_come_from_never_the_owner() {
        let g = Arc::new(Guard::default());
        let (owner, guesser, other) = ("192.0.2.1", "198.51.100.7", "2001:db8:1:2::/64");
        g.passed(owner);
        for _ in 0..LOCK_AFTER - 1 {
            g.failed(guesser);
        }
        assert!(!g.locked(guesser), "not yet");
        assert!(g.delay(guesser) >= Duration::from_secs(30) && g.delay(owner) == Duration::ZERO, "a wait for guessers, none for the owner");
        g.failed(guesser);
        assert!(g.locked(guesser) && !g.locked(other) && !g.locked(owner), "{LOCK_AFTER} in a row lock out only their place");
        assert!(g.take_call(guesser).is_none() && g.take_call(owner).is_some());
        assert_eq!(g.calls.load(Ordering::Acquire), 0, "the slot given back as it went");
        // Guesses from places by the hundred: all places not let in lately are locked, not the owner's.
        for i in 0..LOCK_ALL_AFTER {
            g.failed(&format!("203.0.{}.{}", i / 200, i % 200));
        }
        assert!(g.locked(other) && locked() && !g.locked(owner), "{LOCK_ALL_AFTER} from many places");
        // The owner keeps calls of its own while a flood takes all the others.
        g.calls.store(MOST_CALLS, Ordering::Release);
        assert!(g.take_call("203.0.9.9").is_none() && g.take_call(owner).is_some());
        g.calls.store(0, Ordering::Release);
        // The owner's own mistakes count against the owner's place alone.
        let before = g.strikes.lock().unwrap().0;
        for _ in 0..LOCK_AFTER {
            g.failed(owner);
        }
        assert!(g.locked(owner) && g.strikes.lock().unwrap().0 == before, "its own place only");
        // A day later, or a new password, opens it all again.
        if let Some(day) = std::time::Instant::now().checked_sub(LOCK_FOR + Duration::from_secs(1)) {
            g.strikes.lock().unwrap().1 = Some(day);
            g.places.lock().unwrap().values_mut().for_each(|p| p.1 = day);
            assert!(!g.locked(guesser) && !g.locked(other), "a day on");
        }
        for _ in 0..LOCK_ALL_AFTER {
            g.failed(guesser);
        }
        g.reset();
        assert!(!g.locked(guesser) && !g.locked(other) && !locked(), "a new password");
    }

    /// A web gateway's visitor is named to a relay on this computer, through the upgrade it
    /// takes from a web server here, and the line goes on after it.
    #[test]
    fn a_visitor_is_named_to_a_relay_here() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = l.local_addr().unwrap().to_string();
        let t = thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = std::io::BufReader::new(s.try_clone().unwrap());
            let mut head = Vec::new();
            loop {
                let mut line = String::new();
                std::io::BufRead::read_line(&mut r, &mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                head.push(line.trim().to_string());
            }
            (&s).write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: sshremote-relay\r\n\r\n").unwrap();
            let mut next = String::new();
            std::io::BufRead::read_line(&mut r, &mut next).unwrap();
            (head, next)
        });
        let mut s = dial_for(&relay, "2001:db8::7".parse().unwrap()).unwrap();
        writeln!(s, "viewer 123456789").unwrap();
        let (head, next) = t.join().unwrap();
        assert!(head[0].starts_with("GET ") && head.iter().any(|h| h == "X-Real-IP: 2001:db8::7"), "{head:?}");
        assert_eq!(next.trim(), "viewer 123456789");
    }

    #[test]
    fn guesses_count_by_address_or_ipv6_network() {
        let p = |s: &str| place(Some(s.parse().unwrap()));
        assert_eq!(p("198.51.100.7"), "198.51.100.7");
        assert_eq!(p("::ffff:198.51.100.7"), "198.51.100.7");
        assert_eq!(p("2001:db8:1:2:aaaa::1"), p("2001:db8:1:2:bbbb::9"));
        assert_ne!(p("2001:db8:1:2::1"), p("2001:db8:1:3::1"));
        assert_eq!(place(None), "?");
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

    /// A device trusted with the two-step code comes in by its token until its days are up, or
    /// until two-step sign-in is set up anew; the list is kept with the settings.
    #[test]
    fn trusted_devices_come_in_until_their_time_or_a_new_secret() {
        let d = std::env::temp_dir().join(format!("sshremote-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        TEST_DIR.with(|t| *t.borrow_mut() = Some(d.clone()));
        let mut c = load();
        c.totp = crate::totp::new_secret();
        save(&c).unwrap();
        trust_device("abc123", "203.0.113.7");
        let c = load();
        assert!(c.trusts("abc123") && !c.trusts("abc124") && !c.trusts(""));
        assert_eq!(c.trusted_now().map(|t| t.from.as_str()).collect::<Vec<_>>(), ["203.0.113.7"]);
        assert!(!c.trusted[0].hash.contains("abc123"), "only its checksum is kept");
        // Past its days.
        let mut old = c.clone();
        old.trusted[0].at -= TRUST_DAYS * 86_400;
        assert!(!old.trusts("abc123"));
        // Set up anew, or off.
        let mut anew = c.clone();
        anew.totp = crate::totp::new_secret();
        assert!(!anew.trusts("abc123"));
        anew.totp.clear();
        assert!(!anew.trusts("abc123"));
        // One that never came back by its token is let go after a day.
        let mut gone = c.clone();
        gone.trusted[0].at -= TRUST_PENDING;
        assert!(!gone.trusts("abc123"));
        trusted_came("abc123");
        let mut back = load();
        assert!(back.trusted[0].used > 0);
        back.trusted[0].at -= TRUST_PENDING;
        assert!(back.trusts("abc123"), "one that came back stays");
        // Never more than so many: those that never came back go first.
        for n in 0..TRUST_MOST + 3 {
            trust_device(&format!("t{n}"), "");
        }
        let c = load();
        assert_eq!(c.trusted.len(), TRUST_MOST);
        assert!(c.trusts(&format!("t{}", TRUST_MOST + 2)) && c.trusts("abc123") && !c.trusts("t0"));
        let _ = std::fs::remove_dir_all(&d);
        TEST_DIR.with(|t| *t.borrow_mut() = None);
    }
}
