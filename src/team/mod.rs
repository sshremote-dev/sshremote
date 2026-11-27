//! Teams (docs/team.md): an organization's own server that knows its computers and its people.
//! What both sides share is here: tickets, policies, events, how a device signs its requests,
//! passwords kept as PBKDF2 hashes, and the app's own team settings with the calls it makes.

use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

pub mod client;
pub mod server;

type HmacSha256 = Hmac<Sha256>;

/// How long a ticket lets its viewer in.
pub const TICKET_SECS: u64 = 300;
/// How far a device's clock may be from the server's.
pub const CLOCK_SLACK: u64 = 300;

pub fn now() -> u64 {
    crate::sys::now_secs()
}

fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Two byte strings compared in a time that does not tell where they differ.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn b64url(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn unb64url(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim()).ok()
}

/// What a ticket lets one person do on one computer, for a few minutes.
#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    pub device: String,
    pub user: String,
    pub display: String,
    pub rights: u8,
    pub ask: bool,
    /// The viewer's device key (X25519, public), proved in the handshake.
    pub key: [u8; 32],
    pub viewer: String,
    pub expires: u64,
    pub nonce: String,
}

impl Ticket {
    /// The ticket as sent: base64url of its JSON.
    pub fn encode(&self) -> String {
        let v = json!({"v": 1, "d": self.device, "u": self.user, "n": self.display, "r": self.rights, "a": self.ask,
            "k": crate::sys::hex(&self.key), "i": self.viewer, "e": self.expires, "x": self.nonce});
        b64url(v.to_string().as_bytes())
    }

    pub fn decode(s: &str) -> Option<Ticket> {
        let v: Value = serde_json::from_slice(&unb64url(s)?).ok()?;
        if v["v"].as_u64() != Some(1) {
            return None;
        }
        let key: [u8; 32] = unhex(v["k"].as_str()?)?.try_into().ok()?;
        Some(Ticket {
            device: v["d"].as_str()?.to_string(),
            user: v["u"].as_str()?.to_string(),
            display: v["n"].as_str().unwrap_or("").to_string(),
            rights: u8::try_from(v["r"].as_u64()?).ok()?,
            ask: v["a"].as_bool().unwrap_or(false),
            key,
            viewer: v["i"].as_str().unwrap_or("").to_string(),
            expires: v["e"].as_u64()?,
            nonce: v["x"].as_str().unwrap_or("").to_string(),
        })
    }
}

/// The tag that lets a ticket in: only the team server and that computer can make it.
pub fn ticket_tag(team_key: &[u8], ticket: &str) -> String {
    crate::sys::hex(&mac(team_key, &[b"sshremote team ticket\n", ticket.as_bytes()]))
}

/// A ticket as a computer checks it: for this computer, not expired, not made for too long.
pub fn check_ticket(ticket: &str, own_id: &str) -> Result<Ticket, &'static str> {
    let t = Ticket::decode(ticket).ok_or(n!("the team ticket does not read"))?;
    let now = now();
    if t.device != own_id {
        return Err(n!("the team ticket is for another computer"));
    }
    if t.expires < now || t.expires > now + TICKET_SECS + CLOCK_SLACK {
        return Err(n!("the team ticket is out of date (check both computers' clocks)"));
    }
    Ok(t)
}

/// What a device signs its requests with (docs/team.md, Protocol). `stamp` is the time in seconds
/// and a random part ("1800000000.9f3a..."), so no two requests sign alike.
pub fn request_sign(team_key: &[u8], method: &str, path: &str, stamp: &str, body: &[u8]) -> String {
    let head = format!("sshremote team request\n{method}\n{path}\n{stamp}\n");
    crate::sys::hex(&mac(team_key, &[head.as_bytes(), body]))
}

/// The settings a device group keeps; None leaves the computer's own.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    /// False: only team tickets let anyone in (no password, one-time password or request).
    pub password: bool,
    pub ask: Option<bool>,
    /// What anyone may do there at most (`crate::grant` bits).
    pub rights: u8,
    pub record: Option<bool>,
    pub idle: Option<u32>,
    pub direct: Option<bool>,
    pub lan: Option<bool>,
    /// The app there shows this computer alone (its ID and password), for people who are
    /// helped, not who help: no computers of their own to open.
    pub incoming_only: bool,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy { password: true, ask: None, rights: 0x7f, record: None, idle: None, direct: None, lan: None, incoming_only: false }
    }
}

impl Policy {
    pub fn json(&self) -> Value {
        json!({"password": self.password, "ask": self.ask, "rights": self.rights, "record": self.record, "idle": self.idle, "direct": self.direct, "lan": self.lan, "incoming_only": self.incoming_only})
    }

    pub fn from_json(v: &Value) -> Policy {
        let d = Policy::default();
        Policy {
            password: v["password"].as_bool().unwrap_or(d.password),
            ask: v["ask"].as_bool(),
            rights: v["rights"].as_u64().map_or(d.rights, |r| (r & 0x7f) as u8),
            record: v["record"].as_bool(),
            idle: v["idle"].as_u64().map(|m| m.min(24 * 60) as u32),
            direct: v["direct"].as_bool(),
            lan: v["lan"].as_bool(),
            incoming_only: v["incoming_only"].as_bool().unwrap_or(false),
        }
    }
}

/// A password kept as PBKDF2-HMAC-SHA256: "pbkdf2$ROUNDS$SALT$HASH", in hex.
pub fn password_hash(password: &str) -> String {
    let salt = crate::sys::random(16);
    let rounds = if cfg!(test) { 1000 } else { 210_000 };
    format!("pbkdf2${rounds}${}${}", crate::sys::hex(&salt), crate::sys::hex(&pbkdf2(password.as_bytes(), &salt, rounds)))
}

pub fn password_ok(password: &str, stored: &str) -> bool {
    let mut p = stored.split('$');
    let (Some("pbkdf2"), Some(rounds), Some(salt), Some(hash)) = (p.next(), p.next(), p.next(), p.next()) else { return false };
    let (Ok(rounds), Some(salt), Some(hash)) = (rounds.parse::<u32>(), unhex(salt), unhex(hash)) else { return false };
    (1..=2_000_000).contains(&rounds) && same(&pbkdf2(password.as_bytes(), &salt, rounds), &hash)
}

/// PBKDF2-HMAC-SHA256, one block of 32 bytes.
pub fn pbkdf2(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let prf = HmacSha256::new_from_slice(password).expect("any key length");
    let mut u = {
        let mut m = prf.clone();
        m.update(salt);
        m.update(&1u32.to_be_bytes());
        <[u8; 32]>::from(m.finalize().into_bytes())
    };
    let mut out = u;
    for _ in 1..rounds {
        let mut m = prf.clone();
        m.update(&u);
        u = m.finalize().into_bytes().into();
        for (o, x) in out.iter_mut().zip(u) {
            *o ^= x;
        }
    }
    out
}

/// A session or enrollment secret, and what is kept of it (its SHA-256).
pub fn new_secret() -> String {
    crate::sys::hex(&crate::sys::random(32))
}

pub fn secret_hash(s: &str) -> String {
    use sha2::Digest;
    crate::sys::hex(&Sha256::digest(format!("sshremote team secret\n{s}").as_bytes()))
}

/// A line typed in the terminal without showing it.
pub(crate) fn read_secret(prompt: &str) -> std::io::Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    use std::io::{IsTerminal, Write};
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    // From a script or a pipe: a line as it comes.
    if !std::io::stdin().is_terminal() {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        eprintln!();
        return Ok(line.trim_end_matches(['\r', '\n']).to_string());
    }
    crossterm::terminal::enable_raw_mode()?;
    let mut line = String::new();
    let got = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(line),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break Err(std::io::Error::other("stopped")),
                KeyCode::Backspace => {
                    line.pop();
                }
                KeyCode::Char(c) => line.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    got
}

/// Opens an address in the system's browser.
pub(crate) fn open_url(url: &str) {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// `sshremote team ...` (docs/team.md).
pub fn cli(args: &[String]) -> Result<(), String> {
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        Some("serve") => server::serve(&args[1..]),
        Some("admin") => server::admin_cli(&args[1..]),
        Some("join") => match (arg(1), arg(2)) {
            (Some(url), Some(token)) => client::join(url, token).map(|t| println!("joined {}", t["name"].as_str().unwrap_or(url))),
            _ => Err("usage: sshremote team join URL TOKEN".into()),
        },
        Some("leave") => client::leave().map(|()| println!("left the team")),
        Some("login") => {
            let url = arg(1).ok_or("usage: sshremote team login URL [NAME]")?;
            match arg(2) {
                // In the terminal: name, password and, when asked for, the authenticator's code.
                Some(name) => {
                    let password = read_secret("team password: ").map_err(|e| e.to_string())?;
                    let user = match client::sign_in_password(url, name, &password, None) {
                        Err(e) if e.contains("code") => {
                            let code = read_secret("authenticator code: ").map_err(|e| e.to_string())?;
                            client::sign_in_password(url, name, &password, Some(code.trim()))?
                        }
                        r => r?,
                    };
                    println!("signed in as {}", user["name"].as_str().unwrap_or(name));
                    Ok(())
                }
                // In the browser, as the window does: single sign-on works there.
                None => {
                    let (open, nonce) = client::start_sign_in(url);
                    println!("sign in at: {open}");
                    open_url(&open);
                    let until = std::time::Instant::now() + std::time::Duration::from_secs(600);
                    while std::time::Instant::now() < until {
                        if let Some(user) = client::poll_sign_in(url, &nonce)? {
                            println!("signed in as {}", user["name"].as_str().unwrap_or(""));
                            return Ok(());
                        }
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    Err("the sign-in was not finished in 10 minutes".into())
                }
            }
        }
        Some("logout") => client::sign_out().map(|()| println!("signed out")),
        Some("devices") => {
            for d in client::devices()? {
                println!("{}  {}  {}{}", crate::computers::spaced(&d.id), d.name, d.group, if d.online { "  (online)" } else { "" });
            }
            Ok(())
        }
        None | Some("status") => {
            let s = client::state();
            match &s.joined {
                Some(j) => println!("this computer: in {} ({}), ID {}", j.team["name"].as_str().unwrap_or("a team"), j.url, crate::computers::spaced(&j.id)),
                None => println!("this computer: in no team"),
            }
            match &s.person {
                Some(p) => println!("signed in: {} at {}", p.user["name"].as_str().unwrap_or("?"), p.url),
                None => println!("signed in: nobody"),
            }
            Ok(())
        }
        _ => Err("usage: sshremote team [status | join URL TOKEN | leave | login URL [NAME] | logout | devices | serve --listen ADDR --data DIR --url URL | admin NAME]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_reads_back_and_its_tag_needs_the_key() {
        let t = Ticket { device: "123456789".into(), user: "ana".into(), display: "Ana".into(), rights: 5, ask: true, key: [7; 32], viewer: "987654321".into(), expires: now() + 60, nonce: "ab".into() };
        let s = t.encode();
        assert_eq!(Ticket::decode(&s), Some(t.clone()));
        assert_eq!(check_ticket(&s, "123456789"), Ok(t));
        assert!(check_ticket(&s, "111111111").is_err());
        assert_ne!(ticket_tag(b"key one", &s), ticket_tag(b"key two", &s));
        let old = Ticket { device: "1".into(), user: "a".into(), display: String::new(), rights: 1, ask: false, key: [0; 32], viewer: String::new(), expires: now() - 1, nonce: String::new() };
        assert!(check_ticket(&old.encode(), "1").is_err());
    }

    #[test]
    fn pbkdf2_is_the_standard_one() {
        // RFC 7914's test vector for PBKDF2-HMAC-SHA256 (first 32 bytes).
        let got = pbkdf2(b"passwd", b"salt", 1);
        assert_eq!(crate::sys::hex(&got), "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc");
        let h = password_hash("correct horse");
        assert!(password_ok("correct horse", &h));
        assert!(!password_ok("wrong horse", &h));
        assert!(!password_ok("x", "pbkdf2$0$00$00"));
    }

    #[test]
    fn a_policy_keeps_what_is_set_and_leaves_the_rest() {
        let p = Policy::from_json(&json!({"password": false, "ask": true, "rights": 3}));
        assert_eq!(p, Policy { password: false, ask: Some(true), rights: 3, ..Policy::default() });
        assert_eq!(Policy::from_json(&p.json()), p);
        assert_eq!(Policy::from_json(&json!({})), Policy::default());
    }
}
