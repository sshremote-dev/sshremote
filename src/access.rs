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

/// Where the settings are: for the whole machine on Windows, so the service (SYSTEM) and the
/// window share them; the user's settings folder elsewhere.
pub fn file() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(d) = TEST_DIR.with(|d| d.borrow().clone()) {
        return Some(d.join("remote-access.json"));
    }
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("ProgramData") {
        return Some(PathBuf::from(p).join("sshremote").join("remote-access.json"));
    }
    crate::hosts::config_dir().map(|d| d.join("remote-access.json"))
}

#[cfg(test)]
thread_local!(pub static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) });

/// The settings, made the first time.
pub fn load() -> Config {
    if let Some(c) = file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| Config::from_json(&v))
        && valid_id(&c.id)
    {
        return c;
    }
    Config { id: new_id(), token: hex(&random(16)), password: new_password(), relay: default_relay(), enabled: false }
}

pub fn save(c: &Config) -> Result<(), String> {
    if c.password.trim().len() < 6 {
        return Err("The password needs at least 6 characters".into());
    }
    let f = file().ok_or("no settings folder")?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let text = serde_json::to_string_pretty(&c.to_json()).map_err(|e| e.to_string())?;
    std::fs::write(&f, text).map_err(|e| format!("{}: {e} (changing it needs administrator rights)", f.display()))?;
    restrict(&f);
    Ok(())
}

/// Only its owner, Administrators and SYSTEM read the file: it holds the password.
fn restrict(f: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("icacls")
            .arg(f)
            .args(["/inheritance:r", "/grant:r", "*S-1-5-32-544:F", "*S-1-5-18:F", "*S-1-3-4:F"])
            .creation_flags(0x0800_0000)
            .output();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600));
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

/// Whether an ID is waiting at a relay now.
pub fn online(id: &str, relay: &str) -> Result<bool, String> {
    let (h, p) = relay_addr(relay);
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&(h.as_str(), p)).map_err(|e| format!("{h}: {e}"))?.next().ok_or("no address")?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("{h}:{p}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    writeln!(s, "status {id}").map_err(|e| e.to_string())?;
    Ok(line(&mut s).map_err(|e| e.to_string())? == "online")
}

/// Keeps this computer reachable by its ID until `keep` says to stop: a line to the relay, a new
/// connection for each viewer, and the agent run as a process of its own for each.
pub fn run(keep: impl Fn() -> Option<Config>) {
    let failures = Arc::new(AtomicU32::new(0));
    let mut wait = 2;
    while let Some(mut cfg) = keep() {
        set_state(State::Connecting);
        let (host, port) = relay_addr(&cfg.relay);
        let mut s = match TcpStream::connect((host.as_str(), port)) {
            Ok(s) => s,
            Err(e) => {
                set_state(State::Failed(format!("The relay {host}:{port} could not be reached: {e}")));
                sleep_while(&keep, wait);
                wait = (wait * 2).min(60);
                continue;
            }
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
        let answer = writeln!(s, "listen {} {}", cfg.id, cfg.token).and_then(|_| line(&mut s));
        match answer.as_deref() {
            Ok("ok") => {}
            Ok("taken") => {
                // Another computer holds that ID: take a new one.
                cfg.id = new_id();
                let _ = save(&cfg);
                continue;
            }
            Ok(other) => {
                set_state(State::Failed(format!("The relay refused: {other}")));
                sleep_while(&keep, wait);
                wait = (wait * 2).min(60);
                continue;
            }
            Err(e) => {
                set_state(State::Failed(format!("The relay did not answer: {e}")));
                sleep_while(&keep, wait);
                wait = (wait * 2).min(60);
                continue;
            }
        }
        wait = 2;
        set_state(State::Online);
        // The relay pings every 30 s; nothing for 75 s means the line is gone. Short reads so a
        // change of the settings (off, a new password) takes effect within a second or two.
        let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
        let mut heard = std::time::Instant::now();
        let mut pending = Vec::new();
        loop {
            if keep().as_ref().is_none_or(|k| k.id != cfg.id || k.password != cfg.password || k.relay != cfg.relay) {
                let _ = s.shutdown(std::net::Shutdown::Both);
                break;
            }
            let mut b = [0u8; 256];
            let l = match s.read(&mut b) {
                Ok(0) => break,
                Ok(n) => {
                    heard = std::time::Instant::now();
                    pending.extend_from_slice(&b[..n]);
                    let Some(at) = pending.iter().position(|c| *c == 10) else {
                        if pending.len() > 1024 {
                            break;
                        }
                        continue;
                    };
                    let l: Vec<u8> = pending.drain(..=at).collect();
                    String::from_utf8_lossy(&l).trim().to_string()
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    if heard.elapsed() > Duration::from_secs(75) {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            };
            // Further whole lines already read wait for the next turn.
            while let Some(at) = pending.iter().position(|c| *c == 10) {
                let more: Vec<u8> = pending.drain(..=at).collect();
                handle_line(&String::from_utf8_lossy(&more), &mut s, &cfg, &failures);
            }
            handle_line(&l, &mut s, &cfg, &failures);
        }
        set_state(State::Connecting);
        sleep_while(&keep, 1);
    }
    set_state(State::Off);
}

/// One line from the relay: a ping to answer, or a viewer's call.
fn handle_line(l: &str, s: &mut TcpStream, cfg: &Config, failures: &Arc<AtomicU32>) {
    let l = l.trim();
    if l == "ping" {
        let _ = writeln!(s, "pong");
    } else if let Some(nonce) = l.strip_prefix("call ") {
        let (cfg, nonce, failures) = (cfg.clone(), nonce.to_string(), failures.clone());
        thread::spawn(move || answer_call(&cfg, &nonce, &failures));
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

/// One viewer: a connection of its own through the relay, the password checked, then the agent.
fn answer_call(cfg: &Config, nonce: &str, failures: &AtomicU32) {
    // Wrong passwords cost more time each, so one cannot be guessed by trying many.
    let f = failures.load(Ordering::Relaxed);
    if f > 0 {
        thread::sleep(Duration::from_secs((2u64 << f.min(6)).min(120)));
    }
    let (host, port) = relay_addr(&cfg.relay);
    let Ok(mut s) = TcpStream::connect((host.as_str(), port)) else { return };
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    if writeln!(s, "accept {} {nonce}", cfg.id).is_err() || !line(&mut s).is_ok_and(|l| l == "ok") {
        return;
    }
    let _ = s.set_read_timeout(None);
    let (r, w) = match crate::net::handshake_agent(s, &cfg.password) {
        Ok(p) => p,
        Err(_) => {
            failures.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    failures.store(0, Ordering::Relaxed);
    VIEWERS.fetch_add(1, Ordering::Relaxed);
    let _ = serve_viewer(r, w);
    VIEWERS.fetch_sub(1, Ordering::Relaxed);
}

/// The agent as a process of its own, its input and output the sealed stream.
fn serve_viewer(mut r: crate::net::SealedReader, mut w: crate::net::SealedWriter) -> Result<(), String> {
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
                (c.enabled && !service_runs()).then_some(c)
            };
            if on().is_some() {
                run(on);
            }
            thread::sleep(Duration::from_secs(2));
        });
    });
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
        return unit_file().and_then(|f| std::fs::read_to_string(f).ok()).is_none_or(|t| std::env::current_exe().is_ok_and(|e| t.contains(&e.display().to_string())));
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
            "[Unit]\nDescription=SSH Remote: remote access by ID\nAfter=network-online.target graphical-session.target\n\n[Service]\nExecStart=\"{}\" remote-access run\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
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
        println!("on:       {}{}", if c.enabled { "yes" } else { "no" }, if service_installed() { " (started with the computer)" } else { "" });
        println!("connect with: sshremote --gui {}", address(&c.id, &c.relay));
    };
    match args.first().map(String::as_str) {
        None | Some("status") => {
            if file().is_none_or(|f| !f.exists()) {
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
        Some("run") => {
            if file().is_none_or(|f| !f.exists()) {
                save(&c)?;
            }
            eprintln!("remote access as {} through {}", spaced(&c.id), c.relay);
            run(|| Some(load()).filter(|c| c.enabled));
            Ok(())
        }
        _ => Err("usage: sshremote remote-access [status | on | off | run | id | password [NEW] | relay HOST | install | uninstall]".into()),
    }
}

/// A relay's side of remote access, for `sshremote --relay`: IDs held by their tokens, a line to
/// each listening agent, and calls passed on.
pub mod relay {
    use super::*;
    use std::collections::HashMap;
    use std::sync::mpsc;

    #[derive(Default)]
    pub struct Hub {
        /// ID → the token's hash, kept on disk so an ID stays its owner's.
        owners: HashMap<String, String>,
        /// ID → a way to call the agent listening under it.
        agents: HashMap<String, (u64, mpsc::Sender<String>)>,
        next: u64,
        /// NONCE → the viewer waiting for that call to be answered.
        calls: HashMap<String, mpsc::Sender<TcpStream>>,
        /// Attempts by address in the current minute.
        tries: HashMap<std::net::IpAddr, (std::time::Instant, u32)>,
        store: Option<PathBuf>,
    }

    fn hash(token: &str) -> String {
        use sha2::Digest;
        hex(&sha2::Sha256::digest(token.as_bytes()))
    }

    impl Hub {
        pub fn new(store: Option<PathBuf>) -> Hub {
            let owners = store.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            Hub { owners, store, ..Default::default() }
        }

        /// Whether an address may try again: 30 attempts a minute.
        pub fn allow(&mut self, ip: std::net::IpAddr) -> bool {
            let now = std::time::Instant::now();
            self.tries.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
            let e = self.tries.entry(ip).or_insert((now, 0));
            e.1 += 1;
            e.1 <= 30
        }

        fn claim(&mut self, id: &str, token: &str) -> bool {
            let h = hash(token);
            match self.owners.get(id) {
                Some(o) => *o == h,
                None => {
                    self.owners.insert(id.to_string(), h);
                    if let Some(f) = &self.store {
                        let _ = std::fs::write(f, serde_json::to_string(&self.owners).unwrap_or_default());
                    }
                    true
                }
            }
        }
    }

    /// Handles one connection whose first line was `first` (the older `agent`/`viewer ID` lines
    /// are handled by the caller).
    pub fn handle(hub: &Arc<Mutex<Hub>>, mut s: TcpStream, first: &str) {
        let lock = || hub.lock().unwrap_or_else(|e| e.into_inner());
        let ip = s.peer_addr().map(|a| a.ip()).ok();
        let words: Vec<&str> = first.split_whitespace().collect();
        match words.as_slice() {
            ["listen", id, token] if valid_id(id) && token.len() >= 16 && token.len() <= 128 => {
                if !lock().claim(id, token) {
                    let _ = writeln!(s, "taken");
                    return;
                }
                let (tx, rx) = mpsc::channel::<String>();
                let generation = {
                    let mut h = lock();
                    h.next += 1;
                    let g = h.next;
                    h.agents.insert(id.to_string(), (g, tx));
                    g
                };
                if writeln!(s, "ok").is_err() {
                    return;
                }
                let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                let mut reader = match s.try_clone() {
                    Ok(r) => r,
                    Err(_) => return,
                };
                let mut since_ping = std::time::Instant::now();
                let mut buf = [0u8; 64];
                loop {
                    while let Ok(nonce) = rx.try_recv() {
                        if writeln!(s, "call {nonce}").is_err() {
                            break;
                        }
                    }
                    if since_ping.elapsed() > Duration::from_secs(30) {
                        if writeln!(s, "ping").is_err() {
                            break;
                        }
                        since_ping = std::time::Instant::now();
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                        Err(_) => break,
                    }
                }
                // Gone, unless a newer line already took its place.
                let mut h = lock();
                if h.agents.get(*id).is_some_and(|(g, _)| *g == generation) {
                    h.agents.remove(*id);
                }
            }
            ["accept", id, nonce] if valid_id(id) && nonce.len() <= 64 => {
                let waiting = lock().calls.remove(*nonce);
                if let Some(v) = waiting {
                    let _ = v.send(s);
                }
            }
            ["viewer", id] | ["status", id] if valid_id(id) => {
                if ip.is_some_and(|ip| !lock().allow(ip)) {
                    let _ = writeln!(s, "too many attempts; wait a minute");
                    return;
                }
                let agent = lock().agents.get(*id).map(|(_, t)| t.clone());
                if words[0] == "status" {
                    let _ = writeln!(s, "{}", if agent.is_some() { "online" } else { "offline" });
                    return;
                }
                let Some(agent) = agent else {
                    let _ = writeln!(s, "offline");
                    return;
                };
                let nonce = hex(&random(12));
                let (tx, rx) = mpsc::channel();
                lock().calls.insert(nonce.clone(), tx);
                if agent.send(nonce.clone()).is_err() {
                    lock().calls.remove(&nonce);
                    let _ = writeln!(s, "offline");
                    return;
                }
                let Ok(mut a) = rx.recv_timeout(Duration::from_secs(20)) else {
                    lock().calls.remove(&nonce);
                    let _ = writeln!(s, "the computer did not answer");
                    return;
                };
                if writeln!(a, "ok").is_err() || writeln!(s, "ok").is_err() {
                    return;
                }
                splice(a, s);
            }
            _ => {}
        }
    }

    /// Copies bytes both ways until either side ends.
    pub fn splice(a: TcpStream, b: TcpStream) {
        let _ = a.set_read_timeout(None);
        let _ = b.set_read_timeout(None);
        let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else { return };
        let (mut a, mut b) = (a, b);
        let t = thread::spawn(move || {
            let _ = io::copy(&mut a2, &mut b2);
            let _ = b2.shutdown(std::net::Shutdown::Both);
        });
        let _ = io::copy(&mut b, &mut a);
        let _ = a.shutdown(std::net::Shutdown::Both);
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
