//! The team server (docs/team.md): `sshremote team serve`, an organization's own record of its
//! computers and people, and the console it is run from. Everything is in one JSON file,
//! `DATA/team.json`, written whole (and read again when `sshremote team admin` changes it); the
//! audit log is appended to `DATA/audit.jsonl`, and each person's sealed address book is a file in
//! `DATA/books`. Sessions and enrollment tokens are kept as hashes only; the computers' team keys
//! are kept as they are, since every ticket is tagged with one.
//!
//! The administration lists come in an object named for what they hold: `{"users": [..]}`,
//! `{"groups": [..]}` (user groups and device groups both), `{"devices": [..]}`, `{"rules": [..]}`,
//! `{"tokens": [..]}`, `{"events": [..]}`; a change answers with what it made (`{"user": ..}`,
//! `{"group": ..}`, `{"device": ..}`, a token, the rules, the settings), a removal with `{}`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use sha2::Digest;

use super::{CLOCK_SLACK, Policy, TICKET_SECS, Ticket, now, secret_hash};

const CONSOLE_HTML: &str = include_str!("../../assets/team/console.html");
const CONSOLE_JS: &str = include_str!("../../assets/team/console.js");
const CONSOLE_CSS: &str = include_str!("../../assets/team/console.css");
const FAVICON: &str = include_str!("../../site/favicon.svg");

/// How long a sign-in lasts: in the console, and in the app (which signs in through the browser).
const CONSOLE_SECS: u64 = 12 * 3600;
const APP_SECS: u64 = 30 * 86_400;
/// How long a browser sign-in waits for its app to take it, and a single sign-on for its provider.
const WAIT_SECS: u64 = 600;
/// How long an app has to take its session after the browser handed it the code.
const APP_CODE_SECS: u64 = 120;
/// A computer seen this recently is online.
const ONLINE_SECS: u64 = 180;
/// How long a device's signature is remembered, so the same request is never taken twice.
const REPLAY_SECS: u64 = 600;
/// Wrong sign-ins (or enrollment tokens) from one address in a quarter of an hour before it waits.
const MOST_TRIES: usize = 5;
/// Wrong tries at one account from anywhere, in the same time: past them it waits too.
const MOST_NAME_TRIES: usize = 20;
const TRIES_SECS: u64 = 900;
const MOST_BODY: usize = 1 << 20;
/// An address book's request: the book (at most 1 MB) in JSON.
const MOST_BOOK_BODY: usize = 2 << 20;
const MOST_BOOK: usize = 1 << 20;
const MOST_LOGO: usize = 256 << 10;
const MOST_EVENTS: usize = 1000;
/// Connections served at once: enough for every computer's look, not for a flood.
const MOST_CONNECTIONS: usize = 512;
/// From one address (an IPv6 one by its /64) at a time, unless through a proxy on this computer
/// (which takes its clients' requests whole before it passes them on).
const MOST_FROM_ONE: usize = 16;
/// The time a request has to come whole, however slowly its bytes come.
const REQUEST_SECS: u64 = 30;
/// A request or header line longer than this is no client's.
const MOST_LINE: u64 = 8192;

static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn st(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Text as kept: no control characters, no spaces around, at most `most` characters.
fn clean(s: &str, most: usize) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(most).collect()
}

/// A name of a person or group: 1 to 64 characters, no control characters, no spaces around.
fn name_ok(s: &str) -> bool {
    (1..=64).contains(&s.chars().count()) && !s.chars().any(char::is_control) && s.trim() == s
}

/// The nonce an app signs in with: 32 to 128 hex digits.
/// An app's sign-in in the browser (RFC 8252): the app waits on this computer's loopback `port`
/// and holds the verifier whose SHA-256 is `challenge`. Only that page gets the one-time code and
/// only that app can take the session for it, so a link someone else made gives them nothing.
#[derive(Clone, Debug, PartialEq)]
struct AppAsk {
    challenge: String,
    port: u16,
}

impl AppAsk {
    fn from(app: &str, port: &str) -> Result<Option<AppAsk>, &'static str> {
        if app.is_empty() {
            return Ok(None);
        }
        let port = port.parse::<u16>().ok().filter(|p| *p >= 1024);
        match port {
            Some(port) if app.len() == 64 && app.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(Some(AppAsk { challenge: app.to_ascii_lowercase(), port })),
            _ => Err("this sign-in link from the app is broken: start it again in the app"),
        }
    }

    /// The app's own page on this computer, with the one-time code.
    fn back(&self, code: &str) -> String {
        format!("http://127.0.0.1:{}/?code={code}", self.port)
    }

    /// This sign-in's page again, for its words.
    fn query(&self) -> String {
        format!("app={}&port={}", self.challenge, self.port)
    }
}

/// A team's own address (`--url`): http:// or https://, then only characters safe in a shell's or
/// PowerShell's quotes, an HTML attribute and a header.
fn url_ok(u: &str) -> bool {
    u.strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .is_some_and(|r| !r.is_empty() && !r.starts_with('/') && r.chars().all(|c| c.is_ascii_alphanumeric() || "-._~:/%[]".contains(c)))
}

// ---------------------------------------------------------------------------------------------
// What the server keeps.

#[derive(Clone, Debug, Default, PartialEq)]
struct User {
    name: String,
    display: String,
    email: String,
    admin: bool,
    groups: Vec<String>,
    /// A PBKDF2 hash, or empty: signs in through LDAP or single sign-on only.
    password: String,
    /// The authenticator's secret (base32), or empty.
    totp: String,
    disabled: bool,
    /// The identity provider's issuer and its subject for this person (`ISSUER#SUB`), linked at
    /// the first single sign-on.
    oidc: String,
    /// For the address book's key, made once.
    salt: String,
}

impl User {
    fn stored(&self) -> Value {
        json!({"name": self.name, "display": self.display, "email": self.email, "admin": self.admin, "groups": self.groups,
            "password": self.password, "totp": self.totp, "disabled": self.disabled, "oidc": self.oidc, "salt": self.salt})
    }

    fn read(v: &Value) -> Option<User> {
        Some(User {
            name: v["name"].as_str()?.to_string(),
            display: st(v, "display"),
            email: st(v, "email"),
            admin: v["admin"].as_bool().unwrap_or(false),
            groups: strings(&v["groups"]),
            password: st(v, "password"),
            totp: st(v, "totp"),
            disabled: v["disabled"].as_bool().unwrap_or(false),
            oidc: st(v, "oidc"),
            salt: st(v, "salt"),
        })
    }

    /// USER of docs/team.md.
    fn public(&self) -> Value {
        json!({"name": self.name, "display": self.display, "email": self.email, "admin": self.admin, "groups": self.groups, "totp": !self.totp.is_empty()})
    }

    /// As the console lists it: also whether it is disabled, has a password, is linked to single sign-on.
    fn listed(&self) -> Value {
        let mut v = self.public();
        v["disabled"] = json!(self.disabled);
        v["password"] = json!(!self.password.is_empty());
        v["oidc"] = json!(!self.oidc.is_empty());
        v
    }

    fn shown(&self) -> &str {
        if self.display.is_empty() { &self.name } else { &self.display }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Device {
    id: String,
    name: String,
    os: String,
    version: String,
    /// Its device group, or empty.
    group: String,
    key: Vec<u8>,
    joined: u64,
    seen: u64,
    /// The console asked for its sessions to end: said at its next look.
    end: bool,
}

impl Device {
    fn stored(&self) -> Value {
        json!({"id": self.id, "name": self.name, "os": self.os, "version": self.version, "group": self.group, "key": crate::sys::hex(&self.key),
            "joined": self.joined, "seen": self.seen, "end": self.end})
    }

    fn read(v: &Value) -> Option<Device> {
        Some(Device {
            id: v["id"].as_str()?.to_string(),
            name: st(v, "name"),
            os: st(v, "os"),
            version: st(v, "version"),
            group: st(v, "group"),
            key: crate::sys::unhex(v["key"].as_str()?).filter(|k| k.len() == 32)?,
            joined: v["joined"].as_u64().unwrap_or(0),
            seen: v["seen"].as_u64().unwrap_or(0),
            end: v["end"].as_bool().unwrap_or(false),
        })
    }

    fn online(&self) -> bool {
        now().saturating_sub(self.seen) <= ONLINE_SECS
    }

    fn listed(&self) -> Value {
        json!({"id": self.id, "name": self.name, "os": self.os, "version": self.version, "group": self.group, "online": self.online(),
            "seen": self.seen, "joined": self.joined, "end": self.end})
    }
}

/// Who may reach which computers, with what rights (`access` says how rules add up).
#[derive(Clone, Debug, PartialEq)]
struct Rule {
    /// `user:NAME`, `group:NAME` or `all`.
    who: String,
    /// `group:NAME` (a device group), `device:ID` or `all`.
    devices: String,
    rights: u8,
    ask: bool,
}

impl Rule {
    fn json(&self) -> Value {
        json!({"who": self.who, "devices": self.devices, "rights": self.rights, "ask": self.ask})
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    /// What the console calls it by; the token itself is shown once, when made.
    id: String,
    hash: String,
    group: String,
    /// 0: never.
    expires: u64,
    /// 0: any number.
    uses: u64,
    used: u64,
    created: u64,
}

impl Token {
    fn stored(&self) -> Value {
        json!({"id": self.id, "hash": self.hash, "group": self.group, "expires": self.expires, "uses": self.uses, "used": self.used, "created": self.created})
    }

    fn read(v: &Value) -> Option<Token> {
        Some(Token {
            id: v["id"].as_str()?.to_string(),
            hash: v["hash"].as_str()?.to_string(),
            group: st(v, "group"),
            expires: v["expires"].as_u64().unwrap_or(0),
            uses: v["uses"].as_u64().unwrap_or(0),
            used: v["used"].as_u64().unwrap_or(0),
            created: v["created"].as_u64().unwrap_or(0),
        })
    }

    fn listed(&self) -> Value {
        json!({"id": self.id, "group": self.group, "expires": self.expires, "uses": self.uses, "used": self.used, "created": self.created})
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Session {
    user: String,
    expires: u64,
    app: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Oidc {
    issuer: String,
    client_id: String,
    client_secret: String,
    /// The email domains whose people may be found by email or made.
    domains: Vec<String>,
    make_users: bool,
    /// The provider's email addresses are the organization's own (Microsoft Entra ID, say, which
    /// says nothing of verifying them): taken as verified unless it says they are not.
    trust_email: bool,
}

/// Where the team's email goes out: `smtps://HOST[:PORT]`, or `smtp://HOST[:PORT]` with STARTTLS
/// (plain only to this computer's own mail server).
#[derive(Clone, Debug, Default, PartialEq)]
struct Smtp {
    url: String,
    user: String,
    password: String,
    /// The sender's address.
    from: String,
}

/// A link sent by email to set a password: an invitation, or a new password for a forgotten one.
#[derive(Clone, Debug, Default, PartialEq)]
struct Link {
    user: String,
    expires: u64,
    reset: bool,
}

/// How long an invitation's link works; a new password's.
const INVITE_SECS: u64 = 7 * 86_400;
const RESET_SECS: u64 = 3600;

/// An email address as one goes in a mail's envelope and headers: nothing that could add a line.
fn mail_ok(a: &str) -> bool {
    let a = a.trim();
    a.len() <= 254 && a.split_once('@').is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.')) && a.chars().all(|c| c.is_ascii_graphic() && !"<>()[],;:\\\"".contains(c))
}

/// A header's words, in UTF-8 as RFC 2047 says.
fn mail_words(s: &str) -> String {
    use base64::Engine;
    if s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        s.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(s))
    }
}

/// A time as a mail's Date header has it (RFC 5322), in UTC.
fn mail_date(secs: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let t = utc(secs);
    let (y, m, d) = (&t[0..4], t[5..7].parse::<usize>().unwrap_or(1), t[8..10].trim_start_matches('0'));
    format!("{}, {d} {} {y} {} +0000", DAYS[(secs / 86_400 % 7) as usize], MONTHS[m.saturating_sub(1).min(11)], &t[11..])
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Ldap {
    url: String,
    /// The DN a name binds as, `{}` standing for the name.
    user_dn: String,
    make_users: bool,
    /// The certificate (PEM, on this server) of the authority that signed the directory's own,
    /// for one the system does not know (an organization's own); empty: the system's.
    ca_file: String,
}

#[derive(Clone, Debug, Default)]
struct Store {
    name: String,
    logo: String,
    relay: String,
    /// The team's computers show its name and logo as the app's own (in the window's title and
    /// the task bar), as an app made for the team would.
    brand: bool,
    oidc: Option<Oidc>,
    ldap: Option<Ldap>,
    smtp: Option<Smtp>,
    /// Links sent by email to set a password, by the hash of their token.
    links: BTreeMap<String, Link>,
    users: BTreeMap<String, User>,
    groups: BTreeSet<String>,
    device_groups: BTreeMap<String, Policy>,
    devices: BTreeMap<String, Device>,
    rules: Vec<Rule>,
    tokens: BTreeMap<String, Token>,
    /// By the hash of their token.
    sessions: HashMap<String, Session>,
}

impl Store {
    fn stored(&self) -> Value {
        let now = now();
        json!({
            "version": 1,
            "name": self.name,
            "logo": self.logo,
            "relay": self.relay,
            "brand": self.brand,
            "oidc": self.oidc.as_ref().map(|o| json!({"issuer": o.issuer, "client_id": o.client_id, "client_secret": o.client_secret, "domains": o.domains, "make_users": o.make_users, "trust_email": o.trust_email})),
            "ldap": self.ldap.as_ref().map(|l| json!({"url": l.url, "user_dn": l.user_dn, "make_users": l.make_users, "ca_file": l.ca_file})),
            "smtp": self.smtp.as_ref().map(|m| json!({"url": m.url, "user": m.user, "password": m.password, "from": m.from})),
            "links": self.links.iter().filter(|(_, l)| l.expires > now).map(|(h, l)| json!({"hash": h, "user": l.user, "expires": l.expires, "reset": l.reset})).collect::<Vec<_>>(),
            "users": self.users.values().map(User::stored).collect::<Vec<_>>(),
            "groups": self.groups,
            "device_groups": self.device_groups.iter().map(|(n, p)| json!({"name": n, "policy": p.json()})).collect::<Vec<_>>(),
            "devices": self.devices.values().map(Device::stored).collect::<Vec<_>>(),
            "rules": self.rules.iter().map(Rule::json).collect::<Vec<_>>(),
            "tokens": self.tokens.values().map(Token::stored).collect::<Vec<_>>(),
            "sessions": self.sessions.iter().filter(|(_, s)| s.expires > now).map(|(h, s)| json!({"hash": h, "user": s.user, "expires": s.expires, "app": s.app})).collect::<Vec<_>>(),
        })
    }

    fn read(v: &Value) -> Store {
        let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
        Store {
            name: v["name"].as_str().unwrap_or("Team").to_string(),
            logo: st(v, "logo"),
            relay: st(v, "relay"),
            brand: v["brand"].as_bool().unwrap_or(false),
            oidc: v["oidc"].as_object().map(|_| {
                let o = &v["oidc"];
                Oidc { issuer: st(o, "issuer"), client_id: st(o, "client_id"), client_secret: st(o, "client_secret"), domains: strings(&o["domains"]), make_users: o["make_users"].as_bool().unwrap_or(false), trust_email: o["trust_email"].as_bool().unwrap_or(false) }
            }),
            ldap: v["ldap"].as_object().map(|_| {
                let l = &v["ldap"];
                Ldap { url: st(l, "url"), user_dn: st(l, "user_dn"), make_users: l["make_users"].as_bool().unwrap_or(false), ca_file: st(l, "ca_file") }
            }),
            smtp: v["smtp"].as_object().map(|_| {
                let m = &v["smtp"];
                Smtp { url: st(m, "url"), user: st(m, "user"), password: st(m, "password"), from: st(m, "from") }
            }),
            links: list("links")
                .iter()
                .filter_map(|l| Some((l["hash"].as_str()?.to_string(), Link { user: st(l, "user"), expires: l["expires"].as_u64()?, reset: l["reset"].as_bool().unwrap_or(false) })))
                .collect(),
            users: list("users").iter().filter_map(User::read).map(|u| (u.name.clone(), u)).collect(),
            groups: strings(&v["groups"]).into_iter().collect(),
            device_groups: list("device_groups").iter().filter_map(|g| Some((g["name"].as_str()?.to_string(), Policy::from_json(&g["policy"])))).collect(),
            devices: list("devices").iter().filter_map(Device::read).map(|d| (d.id.clone(), d)).collect(),
            rules: list("rules")
                .iter()
                .map(|r| Rule { who: st(r, "who"), devices: st(r, "devices"), rights: (r["rights"].as_u64().unwrap_or(0) & 0x7f) as u8, ask: r["ask"].as_bool().unwrap_or(false) })
                .collect(),
            tokens: list("tokens").iter().filter_map(Token::read).map(|t| (t.id.clone(), t)).collect(),
            sessions: list("sessions")
                .iter()
                .filter_map(|s| Some((s["hash"].as_str()?.to_string(), Session { user: st(s, "user"), expires: s["expires"].as_u64()?, app: s["app"].as_bool().unwrap_or(false) })))
                .collect(),
        }
    }

    /// A person by name, as typed: the same letters in another case find them too.
    fn user(&self, name: &str) -> Option<&User> {
        self.users.get(name).or_else(|| self.users.values().find(|u| u.name.eq_ignore_ascii_case(name)))
    }

    fn policy(&self, d: &Device) -> Policy {
        self.device_groups.get(&d.group).cloned().unwrap_or_default()
    }

    /// What `user` may do on `d`: what the rules give, within what its device group's policy lets anyone do.
    fn reach(&self, user: &User, d: &Device) -> Option<(u8, bool)> {
        access(&self.rules, user, d).map(|(rights, ask)| (rights & self.policy(d).rights, ask))
    }

    fn has_admin(&self) -> bool {
        self.users.values().any(|u| u.admin && !u.disabled)
    }
}

/// What the rules let `user` do on `device`: None when no rule lets them reach it. A rule matches
/// when its `who` names the person (`user:NAME`), a group they are in (`group:NAME`) or anyone
/// (`all`), and its `devices` name the computer (`device:ID`), its device group (`group:NAME`) or
/// every computer (`all`). Rules only give, never take away: the rights are those of all the
/// matching rules together, and the person at the screen is asked first when any matching rule
/// says so (the careful rule wins over the trusting one).
fn access(rules: &[Rule], user: &User, device: &Device) -> Option<(u8, bool)> {
    let mut got: Option<(u8, bool)> = None;
    for r in rules {
        let who = r.who == "all" || r.who.strip_prefix("user:") == Some(user.name.as_str()) || r.who.strip_prefix("group:").is_some_and(|g| user.groups.iter().any(|x| x == g));
        let what = r.devices == "all" || r.devices.strip_prefix("device:") == Some(device.id.as_str()) || r.devices.strip_prefix("group:").is_some_and(|g| !device.group.is_empty() && g == device.group);
        if who && what {
            let (rights, ask) = got.unwrap_or((0, false));
            got = Some((rights | r.rights, ask || r.ask));
        }
    }
    got
}

/// A rule as the console sends it, checked against what there is.
fn rule_from(v: &Value, s: &Store) -> Result<Rule, String> {
    let who = v["who"].as_str().unwrap_or("").trim().to_string();
    let who_ok = who == "all"
        || match who.split_once(':') {
            Some(("user", n)) => s.users.contains_key(n),
            Some(("group", g)) => s.groups.contains(g),
            _ => false,
        };
    if !who_ok {
        return Err(format!("a rule's \"who\" is user:NAME, group:NAME or all, of a user or group there is (not \"{}\")", who));
    }
    let devices = v["devices"].as_str().unwrap_or("").trim().to_string();
    let devices_ok = devices == "all"
        || match devices.split_once(':') {
            Some(("group", g)) => s.device_groups.contains_key(g),
            Some(("device", id)) => crate::access::valid_id(id),
            _ => false,
        };
    if !devices_ok {
        return Err(format!("a rule's \"devices\" is group:NAME of a device group there is, device:ID or all (not \"{}\")", devices));
    }
    Ok(Rule { who, devices, rights: (v["rights"].as_u64().unwrap_or(0) & 0x7f) as u8, ask: v["ask"].as_bool().unwrap_or(false) })
}

fn new_session(s: &mut Store, user: &str, app: bool) -> (String, u64) {
    let now = now();
    s.sessions.retain(|_, x| x.expires > now);
    let token = super::new_secret();
    let expires = now + if app { APP_SECS } else { CONSOLE_SECS };
    s.sessions.insert(secret_hash(&token), Session { user: user.to_string(), expires, app });
    (token, expires)
}

fn load(file: &Path) -> Result<Store, String> {
    match std::fs::read(file) {
        Ok(b) => serde_json::from_slice::<Value>(&b).map(|v| Store::read(&v)).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::read(&Value::Null)),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// When a file was last changed, and its length: how a change made elsewhere is noticed.
fn stamp(file: &Path) -> Option<(SystemTime, u64)> {
    std::fs::metadata(file).ok().and_then(|m| Some((m.modified().ok()?, m.len())))
}

/// A folder for this user alone.
fn make_dir(dir: &Path) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    // ProgramData lets every user read what is made in it: this folder (and all in it, every
    // computer's team key among them) opens to SYSTEM, the administrators and its owner alone.
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW};
        use windows_sys::Win32::Security::{ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION};
        let sddl = crate::sys::wide("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)");
        let path = crate::sys::wide(&dir.to_string_lossy());
        let mut sd = std::ptr::null_mut();
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(format!("{}: the permissions could not be made", dir.display()));
        }
        let (mut present, mut dacl, mut defaulted) = (0, std::ptr::null_mut::<ACL>(), 0);
        let r = if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) } == 0 {
            1
        } else {
            unsafe { SetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), dacl, std::ptr::null()) }
        };
        unsafe { LocalFree(sd as _) };
        if r != 0 {
            return Err(format!("{}: its permissions could not be set (error {r})", dir.display()));
        }
    }
    Ok(())
}

fn default_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from).join("sshremote-team")
    } else {
        PathBuf::from("/var/lib/sshremote-team")
    }
}

fn flag(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}

// ---------------------------------------------------------------------------------------------
// The server.

/// A browser sign-in for an app: its answer until the app takes it (then None, so a nonce is used once).
/// A single sign-on under way: the PKCE verifier, the app's sign-in, when it began, and the hash
/// of the secret the browser that began it holds in a cookie.
type Flow = (String, Option<AppAsk>, u64, String);

/// The cookie that ties a single sign-on to the browser that began it.
const SSO_COOKIE: &str = "sshremote_sso";

/// Where that cookie goes: the single sign-on's path as the browser sees it, under the team's
/// own path (a proxy may serve it below one).
fn sso_path(url: &str) -> String {
    let below = url.split_once("://").and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].trim_end_matches('/'))).unwrap_or("");
    format!("{below}/api/oidc/")
}

/// An app's sign-in done in the browser, by its one-time code: the challenge, the session, when.
type AppLogin = (String, Value, u64);

struct Team {
    dir: PathBuf,
    /// The address people and computers reach it at (`--url`), without a trailing slash.
    url: String,
    store: Mutex<Store>,
    /// team.json as last read or written here.
    stamp: Mutex<Option<(SystemTime, u64)>>,
    /// Something only worth saving soon (when computers were seen) changed.
    dirty: AtomicBool,
    /// Wrong tries by address, when each was.
    tries: Mutex<HashMap<String, Vec<u64>>>,
    /// The devices' signatures of the last minutes, and when each came.
    signs: Mutex<HashMap<String, u64>>,
    app_logins: Mutex<HashMap<String, AppLogin>>,
    /// Single sign-ons under way, by their state: the PKCE verifier, the app's sign-in, when.
    flows: Mutex<HashMap<String, Flow>>,
    /// The identity provider's settings: its issuer, what it said, when.
    discovery: Mutex<Option<(String, Value, u64)>>,
    /// The authenticator codes each person signed in with lately, so a code works once.
    codes: Mutex<HashMap<(String, String), u64>>,
    audit_file: Mutex<()>,
}

impl Team {
    fn open(dir: &Path, url: &str) -> Result<Team, String> {
        make_dir(dir)?;
        let file = dir.join("team.json");
        let store = load(&file)?;
        Ok(Team {
            dir: dir.to_path_buf(),
            url: url.to_string(),
            stamp: Mutex::new(stamp(&file)),
            store: Mutex::new(store),
            dirty: AtomicBool::new(false),
            tries: Mutex::default(),
            signs: Mutex::default(),
            app_logins: Mutex::default(),
            flows: Mutex::default(),
            discovery: Mutex::default(),
            codes: Mutex::default(),
            audit_file: Mutex::default(),
        })
    }

    fn file(&self) -> PathBuf {
        self.dir.join("team.json")
    }

    /// The record, read again first when something else (`sshremote team admin`) changed the file.
    fn store(&self) -> MutexGuard<'_, Store> {
        let mut s = lock(&self.store);
        let file = self.file();
        let on_disk = stamp(&file);
        let mut known = lock(&self.stamp);
        if on_disk.is_some() && on_disk != *known {
            match load(&file) {
                Ok(fresh) => *s = fresh,
                Err(e) => eprintln!("team: {e}"),
            }
            *known = on_disk;
        }
        drop(known);
        s
    }

    fn save(&self, s: &Store) -> Result<(), String> {
        let file = self.file();
        let text = serde_json::to_vec_pretty(&s.stored()).map_err(|e| e.to_string())?;
        crate::sys::write_whole(&file, &text, true, true).map_err(|e| format!("{}: {e}", file.display()))?;
        *lock(&self.stamp) = stamp(&file);
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn saved(&self, s: &Store) -> Result<(), Answer> {
        self.save(s).map_err(|e| {
            eprintln!("team: {e}");
            fail(500, "the team server could not save its record")
        })
    }

    fn team_json(&self, s: &Store) -> Value {
        json!({"name": s.name, "logo": s.logo, "relay": s.relay, "url": self.url, "brand": s.brand})
    }

    fn redirect_uri(&self) -> String {
        format!("{}/api/oidc/callback", self.url)
    }

    /// One email out through the team's mail server (curl's SMTP: its password in the config
    /// on stdin, the message in a file of this server's own).
    fn send_mail(&self, m: &Smtp, team: &str, to: &str, subject: &str, body: &str) -> Result<(), String> {
        if !mail_ok(to) || !mail_ok(&m.from) {
            return Err("an email address is not valid".into());
        }
        let host = m.url.split_once("://").map_or("", |(_, r)| r.split([':', '/']).next().unwrap_or(""));
        let local = host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
        let id = format!("<{}@{}>", super::new_secret(), m.from.rsplit_once('@').map_or("team", |(_, d)| d));
        let message = format!(
            "From: {} <{}>\r\nTo: <{to}>\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: {id}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n",
            mail_words(team),
            m.from,
            mail_words(subject),
            mail_date(now()),
            body.replace("\r\n", "\n").replace('\n', "\r\n")
        );
        let file = self.dir.join(format!("mail-{}.eml", super::new_secret()));
        crate::sys::write_whole(&file, message.as_bytes(), true, false).map_err(|e| e.to_string())?;
        let bad = || "a control character in the mail settings".to_string();
        let mut config = format!(
            "url = {}\nmail-from = {}\nmail-rcpt = {}\nupload-file = {}\n",
            curl_text(&m.url).ok_or_else(bad)?,
            curl_text(&m.from).ok_or_else(bad)?,
            curl_text(to).ok_or_else(bad)?,
            curl_text(&file.to_string_lossy()).ok_or_else(bad)?
        );
        if !m.user.is_empty() {
            config.push_str(&format!("user = {}\n", curl_text(&format!("{}:{}", m.user, m.password)).ok_or_else(bad)?));
        }
        // STARTTLS on smtp://, but for the mail server on this computer.
        if m.url.starts_with("smtp://") && !local {
            config.push_str("ssl-reqd\n");
        }
        let out = run_curl(&["-sS", "--max-time", "30", "--proto", "=smtp,smtps"], &config);
        let _ = std::fs::remove_file(&file);
        let out = out?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").trim().to_string())
        }
    }

    /// A link to set `user`'s password, made and sent to their email address.
    fn mail_link(&self, user: &str, reset: bool, by: Option<&str>) -> Result<(), Answer> {
        let (m, u, team) = {
            let s = self.store();
            let Some(m) = s.smtp.clone() else { return Err(fail(400, "the team has no mail server set up (Settings, Email)")) };
            let Some(u) = s.users.get(user).cloned() else { return Err(fail(404, "there is no such user")) };
            (m, u, s.name.clone())
        };
        if !mail_ok(&u.email) {
            return Err(fail(400, "this person has no email address"));
        }
        let token = super::new_secret();
        let link = format!("{}/#link={token}", self.url);
        let (subject, body) = if reset {
            (
                format!("A new password for {team}"),
                format!(
                    "Hello {},\n\nSomeone asked for a new password for your account {} in {team}. If it was you, set it here (the link works for an hour):\n\n{link}\n\nIf not, there is nothing to do: your password stays as it is.\n",
                    u.display, u.name
                ),
            )
        } else {
            (
                format!("You are invited to {team}"),
                format!(
                    "Hello {},\n\n{} invited you to {team}, the team for SSH Remote. Set your password here (the link works for a week):\n\n{link}\n\nThen sign in as {} in SSH Remote: Settings, Team, with the team's address {}.\n",
                    u.display,
                    by.unwrap_or("Its administrator"),
                    u.name,
                    self.url
                ),
            )
        };
        {
            let mut s = self.store();
            let now = now();
            s.links.retain(|_, l| l.expires > now && !(l.user == u.name && l.reset == reset));
            s.links.insert(secret_hash(&token), Link { user: u.name.clone(), expires: now + if reset { RESET_SECS } else { INVITE_SECS }, reset });
            self.saved(&s)?;
        }
        self.send_mail(&m, &team, &u.email, &subject, &body).map_err(|e| {
            eprintln!("team: mail to {}: {e}", u.email);
            fail(502, "the mail server did not take the email: see the team server's log")
        })?;
        self.audit(&[json!({"time": now(), "kind": if reset { "reset" } else { "invite" }, "user": u.name, "words": by})]);
        Ok(())
    }

    /// `POST /api/reset {"name"}` (a name or an email address): a link to set a new password,
    /// if that account has a password and an email address. The answer is the same either way.
    fn reset(&self, req: &Req) -> Reply {
        let v = req.json()?;
        let asked = v["name"].as_str().unwrap_or("").trim().to_string();
        let user = {
            let s = self.store();
            s.user(&asked).or_else(|| s.users.values().find(|u| !u.email.is_empty() && u.email.eq_ignore_ascii_case(&asked))).filter(|u| !u.password.is_empty() && !u.disabled && s.smtp.is_some()).map(|u| u.name.clone())
        };
        // Asks counted apart from sign-ins (a few here do not lock an office out of signing in):
        // from one address, and at one account however it was named.
        if !self.try_as(&format!("reset:{}", address_key(req.who)), MOST_TRIES) {
            return Err(fail(429, "too many tries from this address: try again in 15 minutes"));
        }
        let account = format!("mail:{}", user.as_deref().unwrap_or(&asked).to_lowercase());
        if !self.try_as(&account, 3) {
            return Err(fail(429, "too many asks for this account: try again in 15 minutes"));
        }
        if let Some(name) = user {
            let _ = self.mail_link(&name, true, None);
        }
        Ok(ok(json!({"sent": true})))
    }

    /// `GET /api/link?token=T`: whose a link is, while it works; `POST /api/link {"token",
    /// "password"}`: their new password, the link used up.
    fn link(&self, req: &Req, method: &str) -> Reply {
        let (token, password) = if method == "POST" {
            let v = req.json()?;
            (v["token"].as_str().unwrap_or("").to_string(), Some(v["password"].as_str().unwrap_or("").to_string()))
        } else {
            (req.q("token").to_string(), None)
        };
        let hash = secret_hash(&token);
        let now = now();
        let gone = || fail(404, "this link has run out or was used already: ask for another");
        let Some(l) = self.store().links.get(&hash).filter(|l| l.expires > now).cloned() else { return Err(gone()) };
        let Some(password) = password else { return Ok(ok(json!({"user": l.user, "reset": l.reset}))) };
        if password.chars().count() < 8 {
            return Err(fail(400, "a password has at least 8 characters"));
        }
        let hashed = super::password_hash(&password);
        let mut s = self.store();
        if s.links.remove(&hash).is_none() {
            return Err(gone());
        }
        let Some(u) = s.users.get_mut(&l.user) else { return Err(gone()) };
        u.password = hashed;
        // Who knew the old one is out.
        s.sessions.retain(|_, x| x.user != l.user);
        self.saved(&s)?;
        drop(s);
        self.audit(&[json!({"time": now, "kind": "password", "user": l.user, "from": req.who.to_string()})]);
        Ok(ok(json!({"user": l.user})))
    }

    fn book_file(&self, name: &str) -> PathBuf {
        self.dir.join("books").join(crate::sys::hex(&sha2::Sha256::digest(format!("sshremote team book\n{name}").as_bytes())))
    }

    fn audit(&self, events: &[Value]) {
        let mut text = String::new();
        for e in events {
            text.push_str(&e.to_string());
            text.push('\n');
        }
        let _one = lock(&self.audit_file);
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let file = self.dir.join("audit.jsonl");
        if let Err(e) = o.open(&file).and_then(|mut f| f.write_all(text.as_bytes())) {
            eprintln!("team: {}: {e}", file.display());
        }
    }

    /// The audit log's events that pass the console's filter, newest first.
    fn audit_events(&self, q: &HashMap<String, String>, most: usize) -> Vec<Value> {
        let from = q.get("from").and_then(|v| when(v, false)).unwrap_or(0);
        let to = q.get("to").and_then(|v| when(v, true)).unwrap_or(u64::MAX);
        let limit = q.get("limit").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(most).clamp(1, 1_000_000);
        let want = |k: &str, e: &Value| q.get(k).map(|w| w.trim()).filter(|w| !w.is_empty()).is_none_or(|w| e[k].as_str() == Some(w));
        let text = {
            let _one = lock(&self.audit_file);
            std::fs::read_to_string(self.dir.join("audit.jsonl")).unwrap_or_default()
        };
        text.lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| (from..=to).contains(&e["time"].as_u64().unwrap_or(0)) && want("device", e) && want("user", e) && want("kind", e))
            .take(limit)
            .collect()
    }

    /// Counts a try from `who` before it is checked, under one lock, so tries made at once all
    /// count and none slips past the limit; false when the address must wait.
    fn try_from(&self, who: IpAddr) -> bool {
        self.try_as(&address_key(who), MOST_TRIES)
    }

    /// A try at `key` (an address, or an account's name), unless it has had `most` in the last
    /// 15 minutes.
    fn try_as(&self, key: &str, most: usize) -> bool {
        let now = now();
        let mut t = lock(&self.tries);
        t.retain(|_, at| {
            at.retain(|x| now.saturating_sub(*x) < TRIES_SECS);
            !at.is_empty()
        });
        // However many addresses someone has, the record of them stays small.
        if t.len() >= 100_000 && !t.contains_key(key) {
            return false;
        }
        let at = t.entry(key.to_string()).or_default();
        if at.len() >= most {
            return false;
        }
        at.push(now);
        true
    }

    /// A try that was no wrong guess. Only its own: a right sign-in to one account does not clear
    /// the wrong ones at another.
    fn untry(&self, who: IpAddr) {
        self.untry_as(&address_key(who));
    }

    fn untry_as(&self, key: &str) {
        let mut t = lock(&self.tries);
        if let Some(at) = t.get_mut(key) {
            at.pop();
            if at.is_empty() {
                t.remove(key);
            }
        }
    }

    /// The signed-in person a request's bearer token stands for, and the token's hash.
    fn person(&self, req: &Req) -> Result<(String, User), Answer> {
        let auth = req.header("authorization");
        let token = auth.split_once(' ').filter(|(k, _)| k.eq_ignore_ascii_case("bearer")).map(|(_, t)| t.trim()).filter(|t| !t.is_empty());
        let Some(token) = token else { return Err(fail(401, "sign in first")) };
        let hash = secret_hash(token);
        let s = self.store();
        let user = s.sessions.get(&hash).filter(|x| x.expires > now()).and_then(|x| s.users.get(&x.user)).filter(|u| !u.disabled);
        match user {
            Some(u) => Ok((hash, u.clone())),
            None => Err(fail(401, "the session has ended: sign in again")),
        }
    }

    /// The computer that signed this request (docs/team.md, Protocol).
    fn device(&self, req: &Req) -> Result<Device, Answer> {
        let id = req.header("x-team-device");
        let stamp = req.header("x-team-time");
        let d = self.store().devices.get(id).cloned().ok_or_else(|| fail(401, "this computer is not in the team"))?;
        check_sign(&d.key, &req.method, &req.target, &req.body, (stamp, req.header("x-team-sign")), now(), &mut lock(&self.signs)).map_err(|e| fail(401, e))?;
        Ok(d)
    }

    /// The app's session kept for its one-time code; the address its page goes on to.
    fn hand_to_app(&self, app: &AppAsk, u: &User, token: &str, expires: u64) -> String {
        let now = now();
        let code = super::new_secret();
        let mut a = lock(&self.app_logins);
        a.retain(|_, (_, _, at)| now.saturating_sub(*at) < APP_CODE_SECS);
        a.insert(code.clone(), (app.challenge.clone(), json!({"token": token, "user": u.public(), "expires": expires}), now));
        app.back(&code)
    }
}

/// A device's signature checked: made with its key over this request, at a time near ours, and
/// not seen before (the same request sent again by someone who saw it is refused).
fn check_sign(key: &[u8], method: &str, target: &str, body: &[u8], (stamp, sign): (&str, &str), now: u64, seen: &mut HashMap<String, u64>) -> Result<(), &'static str> {
    let time: u64 = stamp.split('.').next().and_then(|t| t.parse().ok()).ok_or("the request is not signed")?;
    if time.abs_diff(now) > CLOCK_SLACK {
        return Err("the computer's clock is more than 5 minutes off");
    }
    let want = super::request_sign(key, method, target, stamp, body);
    if !super::same(want.as_bytes(), sign.trim().to_ascii_lowercase().as_bytes()) {
        return Err("the request's signature is wrong");
    }
    seen.retain(|_, at| now.saturating_sub(*at) < REPLAY_SECS);
    if seen.insert(want, now).is_some() {
        return Err("this request was taken already");
    }
    Ok(())
}

/// `sshremote team serve --listen ADDR --data DIR --url PUBLIC_URL`
pub fn serve(args: &[String]) -> Result<(), String> {
    let listen_on = flag(args, "--listen").unwrap_or_else(|| "127.0.0.1:8023".into());
    let dir = flag(args, "--data").map_or_else(default_dir, PathBuf::from);
    let url = flag(args, "--url").map_or_else(|| format!("http://{listen_on}"), |u| u.trim().trim_end_matches('/').to_string());
    if !url_ok(&url) {
        return Err(format!("--url is the address people and computers reach this server at (http:// or https://, a host, a port and a path), not {url}"));
    }
    let team = Arc::new(Team::open(&dir, &url)?);
    if !team.store().has_admin() {
        eprintln!("sshremote team: no administrator yet; make one with: sshremote team admin NAME --data {}", dir.display());
    }
    let server = TcpListener::bind(&listen_on).map_err(|e| format!("{listen_on}: {e}"))?;
    eprintln!("sshremote team: {url} on {listen_on}, its record in {}", dir.display());
    listen(server, team);
    Ok(())
}

fn listen(server: TcpListener, team: Arc<Team>) {
    // When computers were last seen is kept every half minute, not at every look.
    {
        let team = team.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(30));
                if team.dirty.load(Ordering::Relaxed) {
                    let s = team.store();
                    if let Err(e) = team.save(&s) {
                        eprintln!("team: {e}");
                    }
                }
            }
        });
    }
    static FROM: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
    for conn in server.incoming().flatten() {
        // Over the limit the connection is closed at once, before a thread is spent on it.
        if CONNECTIONS.fetch_add(1, Ordering::Relaxed) >= MOST_CONNECTIONS {
            CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
            continue;
        }
        let from = conn.peer_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_loopback()).map(address_key);
        if let Some(k) = &from {
            let mut f = lock(&FROM);
            let n = f.get_or_insert_default().entry(k.clone()).or_default();
            if *n >= MOST_FROM_ONE {
                drop(f);
                CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                continue;
            }
            *n += 1;
        }
        let team = team.clone();
        std::thread::spawn(move || {
            struct Served(Option<String>);
            impl Drop for Served {
                fn drop(&mut self) {
                    CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                    if let Some(k) = &self.0 {
                        let mut f = lock(&FROM);
                        if let Some(m) = f.as_mut()
                            && let Some(n) = m.get_mut(k)
                        {
                            *n -= 1;
                            if *n == 0 {
                                m.remove(k);
                            }
                        }
                    }
                }
            }
            let _served = Served(from);
            handle(&conn, &team);
        });
    }
}

fn handle(conn: &TcpStream, team: &Team) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = conn.set_write_timeout(Some(Duration::from_secs(30)));
    let Ok(peer) = conn.peer_addr() else { return };
    let req = match read_request(conn, peer.ip()) {
        Ok(r) => r,
        Err((code, words)) => {
            let _ = send(conn, &fail(code, words), true, false);
            return;
        }
    };
    let answer = route(team, &req);
    let no_store = req.path.starts_with("/api/") || req.path.starts_with("/install.");
    let _ = send(conn, &answer, no_store, req.method == "HEAD");
}

// ---------------------------------------------------------------------------------------------
// HTTP: one request a connection, its body by Content-Length.

struct Req {
    method: String,
    /// The path and query as sent: what a device signs.
    target: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    /// The client's address (the proxy's word for it when it comes through one on this computer).
    who: IpAddr,
}

impl Req {
    fn header(&self, k: &str) -> &str {
        self.headers.get(k).map_or("", String::as_str)
    }

    fn q(&self, k: &str) -> &str {
        self.query.get(k).map_or("", String::as_str)
    }

    fn json(&self) -> Result<Value, Answer> {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(json!({}));
        }
        serde_json::from_slice(&self.body).map_err(|_| fail(400, "the request is not JSON"))
    }
}

fn read_line(r: &mut impl BufRead, line: &mut String) -> Result<(), (u16, &'static str)> {
    line.clear();
    let n = r.by_ref().take(MOST_LINE).read_line(line).map_err(|_| (400, "the request did not come"))?;
    if n == 0 {
        return Err((400, "the request ended early"));
    }
    if n as u64 == MOST_LINE && !line.ends_with('\n') {
        return Err((431, "a request line is too long"));
    }
    Ok(())
}

/// Reads from a connection until a time, each wait no longer than what is left of it.
struct Until<'a> {
    conn: &'a TcpStream,
    until: std::time::Instant,
}

impl Read for Until<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the request came too slowly"));
        }
        self.conn.set_read_timeout(Some(left))?;
        let mut c = self.conn;
        c.read(buf)
    }
}

fn read_request(conn: &TcpStream, peer: IpAddr) -> Result<Req, (u16, &'static str)> {
    let mut r = BufReader::new(Until { conn, until: std::time::Instant::now() + Duration::from_secs(REQUEST_SECS) });
    let mut line = String::new();
    read_line(&mut r, &mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    if !target.starts_with('/') {
        return Err((400, "not a request"));
    }
    let mut headers = HashMap::new();
    let mut ended = false;
    for _ in 0..100 {
        read_line(&mut r, &mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            ended = true;
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    if !ended {
        return Err((431, "too many headers"));
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let (path, query) = (path.to_string(), parse_query(query));
    if headers.contains_key("transfer-encoding") {
        return Err((411, "send the body with a Content-Length"));
    }
    let len: usize = match headers.get("content-length") {
        Some(v) => v.parse().map_err(|_| (400, "the Content-Length does not read"))?,
        None => 0,
    };
    if len > if path == "/api/book" { MOST_BOOK_BODY } else { MOST_BODY } {
        return Err((413, "the request is too large"));
    }
    // curl waits for this before a body of more than a kilobyte.
    if len > 0 && headers.get("expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
        let mut w = conn;
        let _ = w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(|_| (400, "the request's body did not all come"))?;
    let who = client_address(peer, &headers);
    Ok(Req { method, target, path, query, headers, body, who })
}

/// Behind a proxy on this computer the client's address comes in its header: the last of
/// X-Forwarded-For, the one the proxy saw (nginx's `$proxy_add_x_forwarded_for` and Caddy put it
/// there; those before it, and X-Real-IP, which a proxy may pass on as the client sent it, are
/// the client's own word).
fn client_address(peer: IpAddr, headers: &HashMap<String, String>) -> IpAddr {
    if !peer.is_loopback() {
        return peer;
    }
    headers.get("x-forwarded-for").and_then(|v| v.rsplit(',').next()).and_then(|v| v.trim().parse().ok()).unwrap_or(peer)
}

/// Wrong tries are counted by address: an IPv6 one by its /64, which one network has whole.
fn address_key(who: IpAddr) -> String {
    match who {
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(|| who.to_string(), |v4| v4.to_string()),
        IpAddr::V4(_) => who.to_string(),
    }
}

/// `%XX` decoded (and `+` as a space, in a query).
fn decode(s: &str, plus: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let escaped = if b[i] == b'%' { s.get(i + 1..i + 3).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit())).and_then(|h| u8::from_str_radix(h, 16).ok()) } else { None };
        match b[i] {
            _ if escaped.is_some() => {
                out.extend(escaped);
                i += 3;
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k, true), decode(v, true))
        })
        .collect()
}

/// Percent-encoded for a URL's query: all but the unreserved characters.
fn esc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

struct Answer {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
    headers: Vec<(&'static str, String)>,
}

type Reply = Result<Answer, Answer>;

fn ok(v: Value) -> Answer {
    Answer { status: 200, kind: "application/json", body: v.to_string().into_bytes(), headers: Vec::new() }
}

fn fail(status: u16, words: &str) -> Answer {
    Answer { status, kind: "application/json", body: json!({"error": words}).to_string().into_bytes(), headers: Vec::new() }
}

fn file(kind: &'static str, body: &str) -> Answer {
    Answer { status: 200, kind, body: body.as_bytes().to_vec(), headers: Vec::new() }
}

fn redirect(to: &str) -> Answer {
    Answer { status: 302, kind: "text/plain; charset=utf-8", body: Vec::new(), headers: vec![("Location", to.to_string())] }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

fn send(mut conn: &TcpStream, a: &Answer, no_store: bool, head: bool) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n", a.status, reason(a.status), a.kind, a.body.len());
    for (k, v) in &a.headers {
        // Never a header broken into two by what it carries.
        if !v.chars().any(char::is_control) {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    h.push_str(
        "X-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'\r\n\
         X-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n",
    );
    h.push_str(if no_store { "Cache-Control: no-store\r\n" } else { "Cache-Control: no-cache\r\n" });
    h.push_str("Connection: close\r\n\r\n");
    conn.write_all(h.as_bytes())?;
    if !head {
        conn.write_all(&a.body)?;
    }
    conn.flush()
}

fn route(team: &Team, req: &Req) -> Answer {
    let method = if req.method == "HEAD" { "GET" } else { req.method.as_str() };
    let p = req.path.as_str();
    let r = match (method, p) {
        ("GET", "/" | "/index.html" | "/login") => Ok(file("text/html; charset=utf-8", CONSOLE_HTML)),
        ("GET", "/console.js") => Ok(file("text/javascript; charset=utf-8", CONSOLE_JS)),
        ("GET", "/console.css") => Ok(file("text/css; charset=utf-8", CONSOLE_CSS)),
        ("GET", "/favicon.svg") => Ok(file("image/svg+xml", FAVICON)),
        ("GET", "/install.sh" | "/install.ps1") => team.installer(req),
        // The language tables the console's words come from, as the web gateway serves them.
        ("GET", _) if p.starts_with("/lang/") => match p.strip_prefix("/lang/").and_then(|f| f.strip_suffix(".json")).and_then(crate::lang::table_json) {
            Some(table) => Ok(file("application/json", table)),
            None => Err(fail(404, "not here")),
        },
        // What the sign-in page offers, before anyone is signed in.
        ("GET", "/api/login-options") => {
            let s = team.store();
            Ok(ok(json!({"oidc": s.oidc.is_some(), "ldap": s.ldap.is_some(), "mail": s.smtp.is_some(), "team": team.team_json(&s)})))
        }
        ("POST", "/api/login") => team.login(req),
        ("POST", "/api/reset") => team.reset(req),
        ("GET" | "POST", "/api/link") => team.link(req, method),
        ("POST", "/api/logout") => team.person(req).map(|(hash, _)| {
            let mut s = team.store();
            s.sessions.remove(&hash);
            let _ = team.save(&s);
            ok(json!({}))
        }),
        ("GET", "/api/me") => team.person(req).map(|(_, u)| ok(json!({"user": u.public(), "team": team.team_json(&team.store())}))),
        ("GET", "/api/devices") => team.devices(req),
        ("POST", "/api/ticket") => team.ticket(req),
        ("GET" | "PUT", "/api/book") => team.book(req, method),
        ("GET", "/api/oidc/start") => {
            let app = AppAsk::from(req.q("app"), req.q("port")).ok().flatten();
            Ok(team.oidc_start(req).unwrap_or_else(|w| team.to_page(app.as_ref(), &format!("error={}", esc(&w)))))
        }
        ("GET", "/api/oidc/callback") => Ok(team.oidc_callback(req)),
        ("POST", "/api/app-login") => team.app_login(req),
        ("POST", "/api/device/enroll") => team.enroll(req),
        ("GET", "/api/device/policy") => team.policy(req),
        ("POST", "/api/device/events") => team.events(req),
        ("POST", "/api/device/leave") => team.leave(req),
        _ if p.starts_with("/api/admin/") => team.admin(req, method),
        _ => Err(fail(404, "not here")),
    };
    r.unwrap_or_else(|e| e)
}

// ---------------------------------------------------------------------------------------------
// People.

/// Why a sign-in did not take.
enum Wrong {
    /// A wrong name, password or code: counted against the address.
    Guess,
    /// The password may be right; the authenticator's code is needed too.
    Code,
    Disabled,
    /// Known to the directory but not to the team, which makes no accounts.
    Outsider,
    /// The directory did not answer.
    Unreachable,
}

/// A hash of no password, checked against when there is no such person, so the time taken does
/// not tell who is one.
fn dummy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| super::password_hash(&super::new_secret()))
}

impl Team {
    fn login(&self, req: &Req) -> Reply {
        let v = req.json()?;
        let name = v["name"].as_str().unwrap_or("").trim().to_string();
        let password = v["password"].as_str().unwrap_or("");
        let code = v["code"].as_str().unwrap_or("").trim();
        let port = v["port"].as_u64().map_or_else(|| v["port"].as_str().unwrap_or("").to_string(), |p| p.to_string());
        let app = AppAsk::from(v["app"].as_str().unwrap_or(""), &port).map_err(|w| fail(400, w))?;
        if !self.try_from(req.who) {
            return Err(fail(429, "too many wrong tries from this address: try again in 15 minutes"));
        }
        let account = format!("name:{}", name.to_lowercase());
        if !self.try_as(&account, MOST_NAME_TRIES) {
            self.untry(req.who);
            return Err(fail(429, "too many wrong tries at this account: try again in 15 minutes"));
        }
        let (user, how) = match self.check(&name, password, code) {
            Ok(got) => {
                self.untry(req.who);
                self.untry_as(&account);
                got
            }
            Err(Wrong::Guess) => return Err(fail(401, "wrong name, password or code")),
            Err(w) => {
                self.untry(req.who);
                self.untry_as(&account);
                return Err(match w {
                    Wrong::Code => Answer { status: 401, kind: "application/json", body: br#"{"error":"code needed","code":true}"#.to_vec(), headers: Vec::new() },
                    Wrong::Disabled => fail(403, "this account is disabled"),
                    Wrong::Outsider => fail(403, "this account is not in the team: ask its administrator to add it"),
                    _ => fail(503, "the directory could not be reached: try again later"),
                });
            }
        };
        let mut s = self.store();
        // For the app, its session alone: the page that signed in for it keeps none.
        let (token, expires) = if app.is_some() { (String::new(), 0) } else { new_session(&mut s, &user.name, false) };
        let for_app = app.as_ref().map(|_| new_session(&mut s, &user.name, true));
        self.saved(&s)?;
        drop(s);
        // The page goes on to the app's own address with the code; it keeps no session itself.
        let to = match (&app, for_app) {
            (Some(a), Some((t, e))) => Some(self.hand_to_app(a, &user, &t, e)),
            _ => None,
        };
        self.audit(&[json!({"time": now(), "kind": "login", "how": how, "user": user.name, "from": req.who.to_string()})]);
        Ok(ok(json!({"token": (!token.is_empty()).then_some(token), "user": user.public(), "expires": expires, "app": app.is_some(), "to": to})))
    }

    /// A name and password (and code) checked: a local password first, else the directory.
    fn check(&self, name: &str, password: &str, code: &str) -> Result<(User, &'static str), Wrong> {
        let (found, ldap) = {
            let s = self.store();
            (s.user(name).cloned(), s.ldap.clone())
        };
        let (user, how) = match found {
            Some(u) if !u.password.is_empty() => {
                if !super::password_ok(password, &u.password) {
                    return Err(Wrong::Guess);
                }
                (u, "password")
            }
            found => {
                let Some(l) = ldap.filter(|_| name_ok(name)) else {
                    let _ = super::password_ok(password, dummy_hash());
                    return Err(Wrong::Guess);
                };
                let as_name = found.as_ref().map_or(name, |u| u.name.as_str()).to_string();
                let (display, email) = match ldap_check(&l, &as_name, password) {
                    Ok(Some(said)) => said,
                    Ok(None) => return Err(Wrong::Guess),
                    Err(e) => {
                        eprintln!("team: LDAP: {e}");
                        return Err(Wrong::Unreachable);
                    }
                };
                match found {
                    Some(u) => (u, "ldap"),
                    None if l.make_users => {
                        let mut s = self.store();
                        let u = match s.user(&as_name) {
                            Some(u) => u.clone(),
                            None => {
                                // Its name and email as the directory has them.
                                let display = if display.is_empty() { as_name.clone() } else { display };
                                let email = if email.contains('@') { email } else { String::new() };
                                let u = User { name: as_name.clone(), display, email, ..User::default() };
                                s.users.insert(as_name, u.clone());
                                self.save(&s).map_err(|_| Wrong::Unreachable)?;
                                u
                            }
                        };
                        (u, "ldap")
                    }
                    None => return Err(Wrong::Outsider),
                }
            }
        };
        // The code is asked for once the password is right: the answer says nothing of who
        // has an account, or an authenticator, to someone who does not know their password.
        if !user.totp.is_empty() {
            if code.is_empty() {
                return Err(Wrong::Code);
            }
            if !self.code_ok(&user, code) {
                return Err(Wrong::Guess);
            }
        }
        if user.disabled {
            return Err(Wrong::Disabled);
        }
        Ok((user, how))
    }

    fn code_ok(&self, u: &User, code: &str) -> bool {
        // As the check reads it: digits alone.
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if !crate::totp::check(&u.totp, &code) {
            return false;
        }
        // A code works once: one seen over someone's shoulder is no good after, for as long as
        // any code is taken.
        let now = now();
        let mut c = lock(&self.codes);
        c.retain(|(_, _), at| now.saturating_sub(*at) < 120);
        let key = (u.name.clone(), code);
        if c.contains_key(&key) {
            return false;
        }
        c.insert(key, now);
        true
    }

    /// `POST /api/app-login {"code", "verifier"}`: the app takes the session its page signed in
    /// for, once, with the verifier only it holds.
    fn app_login(&self, req: &Req) -> Reply {
        let v = req.json()?;
        let (code, verifier) = (v["code"].as_str().unwrap_or(""), v["verifier"].as_str().unwrap_or(""));
        let now = now();
        let taken = {
            let mut a = lock(&self.app_logins);
            a.retain(|_, (_, _, at)| now.saturating_sub(*at) < APP_CODE_SECS);
            // Taken out at the first try, right or wrong.
            a.remove(code)
        };
        let Some((challenge, session, _)) = taken else { return Err(fail(404, "this sign-in has run out or was taken already: sign in again")) };
        let got = crate::sys::hex(&sha2::Sha256::digest(verifier.as_bytes()));
        if !super::same(got.as_bytes(), challenge.as_bytes()) {
            return Err(fail(403, "this sign-in was made for another app"));
        }
        Ok(ok(session))
    }

    fn devices(&self, req: &Req) -> Reply {
        let (_, me) = self.person(req)?;
        let s = self.store();
        let mut list: Vec<(String, Value)> = s
            .devices
            .values()
            .filter_map(|d| {
                let (rights, ask) = s.reach(&me, d)?;
                Some((d.name.to_lowercase(), json!({"id": d.id, "name": d.name, "os": d.os, "group": d.group, "online": d.online(), "rights": rights, "ask": ask})))
            })
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(ok(json!({"devices": list.into_iter().map(|(_, v)| v).collect::<Vec<_>>()})))
    }

    fn ticket(&self, req: &Req) -> Reply {
        let (_, me) = self.person(req)?;
        let v = req.json()?;
        let id: String = v["device"].as_str().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        let key: [u8; 32] = v["key"].as_str().and_then(crate::sys::unhex).and_then(|k| k.try_into().ok()).ok_or_else(|| fail(400, "\"key\" is the viewer's device key: 64 hex digits"))?;
        let viewer: String = v["viewer"].as_str().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        if !viewer.is_empty() && !crate::access::valid_id(&viewer) {
            return Err(fail(400, "\"viewer\" is the viewer's remote access ID"));
        }
        let s = self.store();
        let d = s.devices.get(&id).cloned().ok_or_else(|| fail(404, "that computer is not in the team (any more): it may have left it"))?;
        let got = s.reach(&me, &d);
        drop(s);
        let from = if viewer.is_empty() { req.who.to_string() } else { viewer.clone() };
        let Some((rights, ask)) = got else {
            self.audit(&[json!({"time": now(), "kind": "refused", "how": "team", "user": me.name, "from": from, "device": d.id, "device_name": d.name, "words": "no access rule"})]);
            return Err(fail(403, "no rule lets you reach this computer"));
        };
        let t = Ticket {
            device: d.id.clone(),
            user: me.name.clone(),
            display: me.shown().to_string(),
            rights,
            ask,
            key,
            viewer,
            expires: now() + TICKET_SECS,
            nonce: crate::sys::hex(&crate::sys::random(16)),
        };
        let ticket = t.encode();
        let tag = super::ticket_tag(&d.key, &ticket);
        self.audit(&[json!({"time": now(), "kind": "ticket", "how": "team", "user": me.name, "from": from, "device": d.id, "device_name": d.name, "rights": rights, "ask": ask})]);
        Ok(ok(json!({"ticket": ticket, "tag": tag, "expires": t.expires})))
    }

    fn book(&self, req: &Req, method: &str) -> Reply {
        let (_, me) = self.person(req)?;
        let file = self.book_file(&me.name);
        if method == "PUT" {
            let v = req.json()?;
            match &v["book"] {
                Value::Null => {
                    let _ = std::fs::remove_file(&file);
                }
                Value::String(b) => {
                    use base64::Engine;
                    if b.len() > MOST_BOOK {
                        return Err(fail(413, "an address book is at most 1 MB"));
                    }
                    if base64::engine::general_purpose::STANDARD.decode(b.trim()).is_err() {
                        return Err(fail(400, "\"book\" is base64"));
                    }
                    let dir = self.dir.join("books");
                    make_dir(&dir).map_err(|e| fail(500, &e))?;
                    crate::sys::write_whole(&file, b.trim().as_bytes(), true, true).map_err(|e| fail(500, &e.to_string()))?;
                }
                _ => return Err(fail(400, "\"book\" is base64, or null")),
            }
            return Ok(ok(json!({})));
        }
        let mut s = self.store();
        let made = match s.users.get_mut(&me.name) {
            Some(u) if u.salt.is_empty() => {
                u.salt = crate::sys::hex(&crate::sys::random(16));
                true
            }
            Some(_) => false,
            None => return Err(fail(401, "the session has ended: sign in again")),
        };
        let salt = s.users[&me.name].salt.clone();
        if made {
            self.saved(&s)?;
        }
        drop(s);
        let book = std::fs::read_to_string(&file).ok();
        Ok(ok(json!({"book": book, "salt": salt})))
    }
}

// ---------------------------------------------------------------------------------------------
// The directory (LDAP) and the identity provider (OpenID Connect), both through curl.

/// Text as a quoted string of a curl config; None when it holds a control character, which a
/// config line cannot carry (and no address, header or form here needs).
fn curl_text(s: &str) -> Option<String> {
    if s.chars().any(char::is_control) {
        return None;
    }
    Some(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// curl with its config on stdin: what it is given there (a password, a client secret, a token)
/// is not on its command line, which `ps` shows anyone.
fn run_curl(args: &[&str], config: &str) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    let mut cmd = std::process::Command::new("curl");
    cmd.args(args).args(["-K", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::sys::quiet(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("curl is needed: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(config.as_bytes());
    }
    child.wait_with_output().map_err(|e| e.to_string())
}

/// One HTTP request (a form POSTed when `form` is given): the status and the answer.
fn curl(url: &str, headers: &[(&str, &str)], form: Option<&[(&str, &str)]>) -> Result<(u16, Vec<u8>), String> {
    let bad = || "a control character in a request to the identity provider".to_string();
    let mut config = format!("url = {}\n", curl_text(url).ok_or_else(bad)?);
    for (k, v) in headers.iter().chain(&[("Accept", "application/json")]) {
        config.push_str(&format!("header = {}\n", curl_text(&format!("{k}: {v}")).ok_or_else(bad)?));
    }
    if let Some(f) = form {
        let data = f.iter().map(|(k, v)| format!("{}={}", esc(k), esc(v))).collect::<Vec<_>>().join("&");
        config.push_str(&format!("data-raw = {}\n", curl_text(&data).ok_or_else(bad)?));
    }
    let out = run_curl(&["-sS", "--max-time", "15", "--proto", "=https,http", "-w", "\n%{http_code}"], &config)?;
    let at = out.stdout.iter().rposition(|&b| b == b'\n').unwrap_or(0);
    let code: u16 = String::from_utf8_lossy(&out.stdout[at..]).trim().parse().unwrap_or(0);
    if code == 0 {
        return Err(format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok((code, out.stdout[..at].to_vec()))
}

/// A name as one value of a DN (RFC 4514), and with no colon, which would end curl's user there.
fn ldap_escape(name: &str) -> String {
    let n = name.chars().count();
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        match c {
            ',' | '+' | '"' | '\\' | '<' | '>' | ';' | '=' => {
                out.push('\\');
                out.push(c);
            }
            '\0' => out.push_str("\\00"),
            ':' => out.push_str("\\3a"),
            '#' if i == 0 => out.push_str("\\#"),
            ' ' if i == 0 || i + 1 == n => out.push_str("\\ "),
            _ => out.push(c),
        }
    }
    out
}

/// Binds to the directory as `name` (and searches its base, as curl does with an LDAP address):
/// whether it let them in, or an error when it did not answer.
/// Binds to the directory as `name` with `password`: Some(its display name and email, as far as
/// its own entry says them) when that works, None when not.
fn ldap_check(l: &Ldap, name: &str, password: &str) -> Result<Option<(String, String)>, String> {
    // An empty password binds as nobody on many directories, and that would let anyone in.
    if password.is_empty() {
        return Ok(None);
    }
    let dn = l.user_dn.replace("{}", &ldap_escape(name));
    let (Some(url), Some(user)) = (curl_text(&ldap_self_url(&l.url, &dn)), curl_text(&format!("{dn}:{password}"))) else { return Ok(None) };
    let mut config = format!("url = {url}\nuser = {user}\n");
    if let Some(ca) = Some(l.ca_file.as_str()).filter(|c| !c.is_empty()).and_then(curl_text) {
        config.push_str(&format!("cacert = {ca}\n"));
    }
    let out = run_curl(&["-sS", "--max-time", "15", "--proto", "=ldap,ldaps"], &config)?;
    match out.status.code() {
        // Bound as that person, and their own entry read (curl's lines: "\tcn: Ana Lee").
        Some(0) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let attr = |k: &str| text.lines().find_map(|l| l.trim_start().strip_prefix(k)?.strip_prefix(": ").map(|v| clean(v, 128))).unwrap_or_default();
            let display = Some(attr("displayName")).filter(|d| !d.is_empty()).unwrap_or_else(|| attr("cn"));
            Ok(Some((display, attr("mail"))))
        }
        // Bound, though not let read even that: many directories let few read, and the bind
        // alone is the proof.
        Some(39) => Ok(Some((String::new(), String::new()))),
        // Not resolved, not connected, timed out, TLS: nothing was said about the password.
        Some(6 | 7 | 28 | 35 | 60) => Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").trim().to_string()),
        // Login denied (67), could not bind (38), and the rest.
        _ => Ok(None),
    }
}

/// The directory's address with the person's own entry as what is looked up, for their names:
/// what a bound person may read even where the rest of the directory is closed to them.
fn ldap_self_url(url: &str, dn: &str) -> String {
    let server = url.split_once("://").map_or(url.len(), |(scheme, rest)| scheme.len() + 3 + rest.find('/').unwrap_or(rest.len()));
    let mut enc = String::new();
    for b in dn.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~,=".contains(&b) {
            enc.push(b as char);
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    format!("{}/{enc}?cn,displayName,mail?base", &url[..server])
}

fn endpoint(d: &Value, k: &str) -> Result<String, String> {
    d[k].as_str()
        .filter(|u| (u.starts_with("https://") || u.starts_with("http://")) && !u.chars().any(char::is_control))
        .map(str::to_string)
        .ok_or_else(|| format!("The identity provider's settings have no {k}."))
}

/// Whether the provider vouches for the email address: `email_verified` true (or "true", as some
/// send it); with `trust_email`, also when it says nothing of it. Never when it says false.
fn email_verified(o: &Oidc, info: &Value) -> bool {
    match &info["email_verified"] {
        Value::Bool(b) => *b,
        Value::String(t) => t.eq_ignore_ascii_case("true"),
        Value::Null => o.trust_email,
        _ => false,
    }
}

/// The person a single sign-on is: the one linked to this issuer's subject; else, with a verified
/// email of an allowed domain, the account made for single sign-on with that email (one with no
/// password, authenticator or administrator's rights: those sign in as they were made to, and
/// an email the provider vouches for does not take them over), or a new one.
fn oidc_user(s: &mut Store, o: &Oidc, sub: &str, email: &str, verified: bool, display: &str) -> Result<String, String> {
    let link = format!("{}#{sub}", o.issuer.trim_end_matches('/'));
    if let Some(u) = s.users.values().find(|u| u.oidc == link) {
        return Ok(u.name.clone());
    }
    let email = email.trim().to_lowercase();
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_default();
    let allowed = verified && !domain.is_empty() && o.domains.iter().any(|d| d.trim().trim_start_matches('@').eq_ignore_ascii_case(&domain));
    if !allowed {
        return Err("This account is not in the team: ask its administrator to add it.".into());
    }
    if let Some(u) = s.users.values_mut().find(|u| u.email.eq_ignore_ascii_case(&email) || u.name.eq_ignore_ascii_case(&email)) {
        if !u.oidc.is_empty() {
            return Err("This person is linked to another account of the identity provider.".into());
        }
        if !u.password.is_empty() || !u.totp.is_empty() || u.admin {
            return Err("Single sign-on does not take over this account by its email: it has a password, two-step sign-in or an administrator's rights.".into());
        }
        u.oidc = link;
        return Ok(u.name.clone());
    }
    if !o.make_users || !name_ok(&email) {
        return Err("This account is not in the team: ask its administrator to add it.".into());
    }
    let display = clean(display, 128);
    let u = User { name: email.clone(), display: if display.is_empty() { email.clone() } else { display }, email: email.clone(), oidc: link, ..User::default() };
    s.users.insert(email.clone(), u);
    Ok(email)
}

impl Team {
    /// The identity provider's settings, fetched again after an hour.
    fn discover(&self, o: &Oidc) -> Result<Value, String> {
        let cached = lock(&self.discovery).clone();
        if let Some((issuer, v, at)) = cached
            && issuer == o.issuer
            && now().saturating_sub(at) < 3600
        {
            return Ok(v);
        }
        let url = format!("{}/.well-known/openid-configuration", o.issuer.trim_end_matches('/'));
        let (status, body) = curl(&url, &[], None).map_err(|e| format!("The identity provider could not be reached ({e})."))?;
        let v = serde_json::from_slice::<Value>(&body).ok().filter(|v| status == 200 && v.is_object()).ok_or_else(|| format!("The identity provider's settings did not read ({url}: {status})."))?;
        *lock(&self.discovery) = Some((o.issuer.clone(), v.clone(), now()));
        Ok(v)
    }

    /// On to the identity provider: the code flow with PKCE (S256) and a state of our own.
    fn oidc_start(&self, req: &Req) -> Result<Answer, String> {
        let o = self.store().oidc.clone().ok_or("Single sign-on is not set up on this team server.")?;
        let app = AppAsk::from(req.q("app"), req.q("port")).map_err(str::to_string)?;
        let d = self.discover(&o)?;
        let auth = endpoint(&d, "authorization_endpoint")?;
        let state = super::new_secret();
        let verifier = super::b64url(&crate::sys::random(32));
        let challenge = super::b64url(&sha2::Sha256::digest(verifier.as_bytes()));
        // The browser that begins it holds a secret the end must come back with: a link someone
        // else made, to the end of their own sign-in, signs nobody in here as them.
        let binder = super::new_secret();
        {
            let now = now();
            let mut f = lock(&self.flows);
            f.retain(|_, x| now.saturating_sub(x.2) < WAIT_SECS);
            // However many are begun and left, the oldest give way.
            while f.len() >= 10_000 {
                let Some(oldest) = f.iter().min_by_key(|(_, x)| x.2).map(|(k, _)| k.clone()) else { break };
                f.remove(&oldest);
            }
            f.insert(state.clone(), (verifier, app, now, secret_hash(&binder)));
        }
        let sep = if auth.contains('?') { '&' } else { '?' };
        let mut to = redirect(&format!(
            "{auth}{sep}response_type=code&client_id={}&redirect_uri={}&scope=openid%20email%20profile&state={state}&code_challenge={challenge}&code_challenge_method=S256",
            esc(&o.client_id),
            esc(&self.redirect_uri())
        ));
        let secure = if self.url.starts_with("https://") { "; Secure" } else { "" };
        to.headers.push(("Set-Cookie", format!("{SSO_COOKIE}={binder}; Path={}; Max-Age={WAIT_SECS}; HttpOnly; SameSite=Lax{secure}", sso_path(&self.url))));
        Ok(to)
    }

    /// Back to the console's page with what came of it in the address's fragment, which no
    /// server sees: `#session=TOKEN` or `#error=WORDS`, or for an app's sign-in its own page with
    /// `#error=WORDS`.
    fn to_page(&self, app: Option<&AppAsk>, fragment: &str) -> Answer {
        match app {
            Some(a) => redirect(&format!("{}/login?{}#{fragment}", self.url, a.query())),
            None => redirect(&format!("{}/#{fragment}", self.url)),
        }
    }

    /// Back from the identity provider, with the state that says which sign-in this is (used once).
    fn oidc_callback(&self, req: &Req) -> Answer {
        let flow = {
            let now = now();
            let mut f = lock(&self.flows);
            f.retain(|_, x| now.saturating_sub(x.2) < WAIT_SECS);
            f.remove(req.q("state"))
        };
        let Some((verifier, app, _, binder)) = flow else { return self.to_page(None, &format!("error={}", esc("This sign-in has run out or was finished already: start it again."))) };
        let held = req.header("cookie").split(';').filter_map(|c| c.trim().strip_prefix(SSO_COOKIE)?.strip_prefix('=')).next().unwrap_or("");
        if !super::same(secret_hash(held).as_bytes(), binder.as_bytes()) {
            return self.to_page(app.as_ref(), &format!("error={}", esc("This sign-in was begun in another browser: start it again here.")));
        }
        let done = match req.query.get("error") {
            Some(e) => Err(format!("The identity provider did not sign you in ({}).", clean(e, 200))),
            None => self.oidc_finish(req, &verifier, app.as_ref()),
        };
        match done {
            Ok(Ok(fragment)) => self.to_page(None, &fragment),
            // Signed in for the app: its sign-in page hands the code on to the app on this
            // computer at a click (not at once: a link someone else made would hand it to theirs).
            Ok(Err(back)) => match (&app, back.rsplit_once("?code=")) {
                (Some(a), Some((_, code))) => redirect(&format!("{}/login?{}#handoff={code}", self.url, a.query())),
                _ => self.to_page(None, &format!("error={}", esc("This sign-in has run out or was finished already: start it again."))),
            },
            Err(w) => self.to_page(app.as_ref(), &format!("error={}", esc(&w))),
        }
    }

    /// The provider's code for a token (client_secret_post), the token for who it is (TLS to the
    /// provider vouches for the answer), and the person signed in: the fragment for the page.
    /// Ok(Ok(fragment)) for the console, Ok(Err(address)) for the app's page.
    fn oidc_finish(&self, req: &Req, verifier: &str, app: Option<&AppAsk>) -> Result<Result<String, String>, String> {
        let code = req.q("code");
        if code.is_empty() {
            return Err("The identity provider gave no code.".into());
        }
        let o = self.store().oidc.clone().ok_or("Single sign-on is not set up on this team server.")?;
        let d = self.discover(&o)?;
        let (token_url, info_url) = (endpoint(&d, "token_endpoint")?, endpoint(&d, "userinfo_endpoint")?);
        let back = self.redirect_uri();
        let mut form = vec![("grant_type", "authorization_code"), ("code", code), ("redirect_uri", back.as_str()), ("client_id", o.client_id.as_str()), ("code_verifier", verifier)];
        if !o.client_secret.is_empty() {
            form.push(("client_secret", o.client_secret.as_str()));
        }
        let (status, body) = curl(&token_url, &[], Some(&form))?;
        let v: Value = serde_json::from_slice(&body).unwrap_or_default();
        let Some(access) = v["access_token"].as_str().filter(|_| status == 200) else {
            return Err(format!("The identity provider gave no token ({}).", clean(v["error"].as_str().unwrap_or(&status.to_string()), 200)));
        };
        let bearer = format!("Bearer {access}");
        let (status, body) = curl(&info_url, &[("Authorization", &bearer)], None)?;
        let info: Value = serde_json::from_slice(&body).ok().filter(|_| status == 200).unwrap_or_default();
        let sub = info["sub"].as_str().filter(|s| !s.is_empty()).ok_or("The identity provider did not say who you are.")?;
        let mut s = self.store();
        let display = info["name"].as_str().or(info["preferred_username"].as_str()).unwrap_or("");
        let name = oidc_user(&mut s, &o, sub, info["email"].as_str().unwrap_or(""), email_verified(&o, &info), display)?;
        let u = s.users[&name].clone();
        if u.disabled {
            let _ = self.save(&s);
            return Err("This account is disabled.".into());
        }
        let (token, expires) = new_session(&mut s, &u.name, app.is_some());
        self.save(&s)?;
        drop(s);
        self.audit(&[json!({"time": now(), "kind": "login", "how": "oidc", "user": u.name, "from": req.who.to_string()})]);
        Ok(match app {
            Some(a) => Err(self.hand_to_app(a, &u, &token, expires)),
            None => Ok(format!("session={token}")),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Devices.

impl Team {
    fn enroll(&self, req: &Req) -> Reply {
        let v = req.json()?;
        let token = v["token"].as_str().unwrap_or("").trim();
        let id: String = v["id"].as_str().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        if id.len() != 9 || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(fail(400, "a computer's ID is 9 digits"));
        }
        if !self.try_from(req.who) {
            return Err(fail(429, "too many wrong tokens from this address: try again in 15 minutes"));
        }
        // A computer in the team already joins again only by its own key: a token alone does not
        // take its place (and with it, its tickets and its record).
        let by_itself = self.device(req).is_ok_and(|d| d.id == id);
        let hash = secret_hash(token);
        let now = now();
        let mut s = self.store();
        let Some(t) = s.tokens.values().find(|t| t.hash == hash) else { return Err(fail(403, "this enrollment token is not known here")) };
        self.untry(req.who);
        if t.expires != 0 && t.expires <= now {
            return Err(fail(403, "this enrollment token has run out"));
        }
        if t.uses != 0 && t.used >= t.uses {
            return Err(fail(403, "this enrollment token has been used up"));
        }
        if s.devices.contains_key(&id) && !by_itself {
            return Err(fail(409, "a computer with this ID is in the team already: remove it in the console first"));
        }
        let Some(t) = s.tokens.values_mut().find(|t| t.hash == hash) else { return Err(fail(403, "this enrollment token is not known here")) };
        t.used += 1;
        let group = t.group.clone();
        let old = s.devices.get(&id);
        let group = if group.is_empty() { old.map(|d| d.group.clone()).unwrap_or_default() } else { group };
        let group = if s.device_groups.contains_key(&group) { group } else { String::new() };
        let name = clean(v["name"].as_str().unwrap_or(""), 64);
        let name = if name.is_empty() { format!("Computer {}", crate::computers::spaced(&id)) } else { name };
        let key = crate::sys::random(32);
        let d = Device { id: id.clone(), name, os: clean(v["os"].as_str().unwrap_or(""), 32), version: clean(v["version"].as_str().unwrap_or(""), 32), group, key: key.clone(), joined: now, seen: now, end: false };
        s.devices.insert(id.clone(), d.clone());
        self.saved(&s)?;
        let team = self.team_json(&s);
        drop(s);
        self.audit(&[json!({"time": now, "kind": "join", "device": d.id, "device_name": d.name, "from": req.who.to_string()})]);
        Ok(ok(json!({"key": crate::sys::hex(&key), "team": team})))
    }

    fn policy(&self, req: &Req) -> Reply {
        let d = self.device(req)?;
        let mut s = self.store();
        let Some(dev) = s.devices.get_mut(&d.id) else { return Err(fail(401, "this computer is not in the team")) };
        dev.seen = now();
        let version = clean(req.header("x-team-version"), 32);
        if !version.is_empty() {
            dev.version = version;
        }
        let end = std::mem::take(&mut dev.end);
        let dev = dev.clone();
        let mut out = json!({"policy": s.policy(&dev).json(), "team": self.team_json(&s)});
        if end {
            out["end"] = json!(true);
            self.saved(&s)?;
        } else {
            self.dirty.store(true, Ordering::Relaxed);
        }
        Ok(ok(out))
    }

    fn events(&self, req: &Req) -> Reply {
        let d = self.device(req)?;
        let v = req.json()?;
        let now = now();
        let text = |e: &Value, k: &str, most: usize| e[k].as_str().map(|s| clean(s, most)).filter(|s| !s.is_empty());
        let events: Vec<Value> = v["events"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .take(MOST_EVENTS)
            .filter_map(|e| {
                // What a computer may say of itself; the server's own kinds (sign-ins, tickets,
                // joining) it cannot.
                let kind = text(e, "kind", 16).filter(|k| ["in", "out", "refused", "file"].contains(&k.as_str()))?;
                // Kept while the computer could not reach the server, so it may be old; not from the future.
                let time = e["time"].as_u64().filter(|t| *t <= now + CLOCK_SLACK).unwrap_or(now);
                let mut out = json!({"time": time, "kind": kind, "device": d.id, "device_name": d.name});
                if let Some(how) = text(e, "how", 16).filter(|h| ["team", "password", "one-time", "request"].contains(&h.as_str())) {
                    out["how"] = json!(how);
                }
                for (k, most) in [("user", 64), ("from", 128), ("words", 1000)] {
                    if let Some(t) = text(e, k, most) {
                        out[k] = json!(t);
                    }
                }
                if let Some(r) = e["rights"].as_u64() {
                    out["rights"] = json!(r & 0xff);
                }
                if let Some(secs) = e["secs"].as_u64() {
                    out["secs"] = json!(secs);
                }
                Some(out)
            })
            .collect();
        self.audit(&events);
        Ok(ok(json!({})))
    }

    fn leave(&self, req: &Req) -> Reply {
        let d = self.device(req)?;
        let mut s = self.store();
        s.devices.remove(&d.id);
        self.saved(&s)?;
        drop(s);
        self.audit(&[json!({"time": now(), "kind": "leave", "device": d.id, "device_name": d.name})]);
        Ok(ok(json!({})))
    }

    /// The installers that join a computer with a token: the official one, then `team join` and
    /// remote access turned on with the service.
    fn installer(&self, req: &Req) -> Reply {
        let token = req.q("token");
        if !(32..=128).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(fail(400, "an enrollment token is 64 hex digits: copy the line from the console again"));
        }
        if req.path.ends_with(".ps1") { Ok(file("text/plain; charset=utf-8", &install_ps1(&self.url, token))) } else { Ok(file("text/plain; charset=utf-8", &install_sh(&self.url, token))) }
    }
}

fn install_sh(url: &str, token: &str) -> String {
    let q = format!("'{}'", url.replace('\'', "'\\''"));
    format!(
        r#"#!/bin/sh
# Joins this computer to the team at {url}:
#   curl -fsSL '{url}/install.sh?token=...' | sh
# Installs SSH Remote for this user with the official installer (https://sshremote.dev/install.sh),
# joins the team with the enrollment token, and turns remote access on with its service.
set -eu
curl -fsSL https://sshremote.dev/install.sh | sh
# Where that installer puts it.
if [ "$(uname -s)" = Darwin ]; then
  apps=${{SSHREMOTE_INSTALL_DIR:-/Applications}}
  [ -w "$apps" ] || apps="$HOME/Applications"
  bin="$apps/SSH Remote.app/Contents/MacOS/sshremote"
else
  bin="${{SSHREMOTE_INSTALL_DIR:-$HOME/.local/bin}}/sshremote"
fi
"$bin" team join {q} {token}
"$bin" remote-access on
"$bin" remote-access install
echo "This computer is in the team at "{q}
"#
    )
}

fn install_ps1(url: &str, token: &str) -> String {
    let q = format!("'{}'", url.replace('\'', "''"));
    format!(
        r#"# Joins this computer to the team at {url}. In PowerShell:
#   irm '{url}/install.ps1?token=...' | iex
# Installs SSH Remote for this user with the official installer (https://sshremote.dev/install.ps1),
# joins the team with the enrollment token, and turns remote access on with its service.

& {{
  $ErrorActionPreference = 'Stop'
  irm https://sshremote.dev/install.ps1 | iex
  # Where that installer puts it.
  $dir = if ($env:SSHREMOTE_INSTALL_DIR) {{ $env:SSHREMOTE_INSTALL_DIR }} else {{ Join-Path $env:LOCALAPPDATA 'Programs\SSH Remote' }}
  $exe = Join-Path $dir 'sshremote.exe'
  # Joining and remote access's service change settings only administrators may change: the
  # three steps run together, with one prompt for the rights when this window lacks them.
  $e = $exe -replace "'", "''"
  $fail = "{{ Write-Host 'Joining the team did not succeed.'; Read-Host 'Press Enter to close'; exit 1 }}"
  $inner = "& '$e' team join {q} {token}; if (`$LASTEXITCODE) $fail; & '$e' remote-access on; if (`$LASTEXITCODE) $fail; & '$e' remote-access install; if (`$LASTEXITCODE) $fail"
  $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($inner))
  $me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
  $how = @{{ FilePath = 'powershell.exe'; ArgumentList = @('-NoProfile', '-EncodedCommand', $enc); Wait = $true; PassThru = $true }}
  if ($me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {{ $how.NoNewWindow = $true }} else {{ $how.Verb = 'RunAs' }}
  $p = Start-Process @how
  if ($p.ExitCode -ne 0) {{ throw 'Joining the team did not succeed.' }}
  Write-Host ('This computer is in the team at ' + {q})
}}
"#
    )
}

/// `text` as a QR code to scan with a phone: an SVG picture as a data: URL (the console's CSP
/// takes those), dark on white with the quiet border cameras look for.
#[cfg(feature = "gui")]
fn qr_svg(text: &str) -> Option<String> {
    use base64::Engine;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let n = code.width();
    let mut path = String::new();
    for (i, c) in code.to_colors().iter().enumerate() {
        if *c == qrcode::Color::Dark {
            path.push_str(&format!("M{} {}h1v1h-1z", i % n + 4, i / n + 4));
        }
    }
    let side = n + 8;
    let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {side} {side}\" shape-rendering=\"crispEdges\"><rect width=\"{side}\" height=\"{side}\" fill=\"#fff\"/><path fill=\"#000\" d=\"{path}\"/></svg>");
    Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)))
}

#[cfg(not(feature = "gui"))]
fn qr_svg(_: &str) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------------------------
// Administration.

/// A time as the console may send it: UNIX seconds (or milliseconds, as JavaScript counts),
/// or a date `YYYY-MM-DD` with a time `THH:MM[:SS]` or not, in UTC; `end`: a date alone means its last second.
fn when(s: &str, end: bool) -> Option<u64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, ""));
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from 1970-01-01 to the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let mut t = time.trim_end_matches('Z').split(':').map(|p| p.parse::<i64>().ok());
    let secs = match (t.next().flatten(), t.next().flatten(), t.next().flatten()) {
        (Some(h), Some(min), sec) => h * 3600 + min * 60 + sec.unwrap_or(0),
        _ if end => 86_399,
        _ => 0,
    };
    u64::try_from(days * 86_400 + secs).ok()
}

/// A UNIX time as `YYYY-MM-DD HH:MM:SS`, UTC.
fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let t = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

const CSV_COLUMNS: [&str; 10] = ["time", "device", "device_name", "kind", "how", "user", "from", "rights", "secs", "words"];

/// The audit log as a spreadsheet takes it (RFC 4180), with no cell it would run as a formula.
fn audit_csv(events: &[Value]) -> String {
    let cell = |s: String| {
        let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s };
        // Every cell quoted: a spreadsheet that splits at ';' (as in many languages) splits none.
        format!("\"{}\"", s.replace('"', "\"\""))
    };
    let mut out = CSV_COLUMNS.join(",");
    out.push_str("\r\n");
    for e in events {
        let cells: Vec<String> = CSV_COLUMNS
            .iter()
            .map(|k| match (*k, &e[*k]) {
                ("time", Value::Number(n)) => utc(n.as_u64().unwrap_or(0)),
                (_, Value::String(s)) => s.clone(),
                (_, Value::Number(n)) => n.to_string(),
                (_, Value::Bool(b)) => b.to_string(),
                _ => String::new(),
            })
            .map(cell)
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

/// A logo the app and the console show: a data: URL of an image of at most 256 KB.
fn logo_ok(l: &str) -> bool {
    use base64::Engine;
    let Some((kind, data)) = l.strip_prefix("data:image/").and_then(|r| r.split_once(";base64,")) else { return false };
    ["png", "jpeg", "gif", "webp", "svg+xml"].contains(&kind) && base64::engine::general_purpose::STANDARD.decode(data.trim()).is_ok_and(|b| !b.is_empty() && b.len() <= MOST_LOGO)
}

fn relay_ok(r: &str) -> bool {
    r.is_empty() || (r.len() <= 255 && r.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:[]".contains(c)))
}

impl Team {
    fn admin(&self, req: &Req, method: &str) -> Reply {
        let (own, me) = self.person(req)?;
        if !me.admin {
            return Err(fail(403, "only the team's administrators may do this"));
        }
        let rest = &req.path["/api/admin/".len()..];
        let (what, arg) = match rest.split_once('/') {
            Some((w, a)) => (w, Some(decode(a, false))),
            None => (rest, None),
        };
        let arg = arg.as_deref();
        match (method, what, arg) {
            ("GET", "users", None) => Ok(ok(json!({"users": self.store().users.values().map(User::listed).collect::<Vec<_>>()}))),
            ("POST", "users", None) => self.put_user(req, None, &own),
            ("PUT", "users", Some(name)) => self.put_user(req, Some(name), &own),
            ("DELETE", "users", Some(name)) => {
                let mut s = self.store();
                if !s.users.contains_key(name) {
                    return Err(fail(404, "there is no such user"));
                }
                if !s.users.values().any(|u| u.name != name && u.admin && !u.disabled) {
                    return Err(fail(400, "the team needs an administrator: make another one first"));
                }
                s.users.remove(name);
                s.sessions.retain(|_, x| x.user != name);
                let who = format!("user:{name}");
                s.rules.retain(|r| r.who != who);
                self.saved(&s)?;
                drop(s);
                let _ = std::fs::remove_file(self.book_file(name));
                Ok(ok(json!({})))
            }
            ("GET", "groups", None) => Ok(ok(json!({"groups": groups_list(&self.store())}))),
            ("POST", "groups", None) => {
                let v = req.json()?;
                let name = v["name"].as_str().unwrap_or("").trim().to_string();
                if !name_ok(&name) {
                    return Err(fail(400, "a group's name is 1 to 64 characters"));
                }
                let mut s = self.store();
                if !s.groups.insert(name.clone()) {
                    return Err(fail(409, "there is a group of that name already"));
                }
                self.saved(&s)?;
                Ok(ok(json!({"group": {"name": name, "users": []}})))
            }
            ("DELETE", "groups", Some(g)) => {
                let mut s = self.store();
                if !s.groups.remove(g) {
                    return Err(fail(404, "there is no such group"));
                }
                for u in s.users.values_mut() {
                    u.groups.retain(|x| x != g);
                }
                let who = format!("group:{g}");
                s.rules.retain(|r| r.who != who);
                self.saved(&s)?;
                Ok(ok(json!({})))
            }
            ("GET", "device-groups", None) => Ok(ok(json!({"groups": device_groups_list(&self.store())}))),
            ("POST", "device-groups", None) => self.put_device_group(req, None),
            ("PUT", "device-groups", Some(g)) => self.put_device_group(req, Some(g)),
            ("DELETE", "device-groups", Some(g)) => {
                let mut s = self.store();
                if s.device_groups.remove(g).is_none() {
                    return Err(fail(404, "there is no such device group"));
                }
                for d in s.devices.values_mut().filter(|d| d.group == g) {
                    d.group.clear();
                }
                for t in s.tokens.values_mut().filter(|t| t.group == g) {
                    t.group.clear();
                }
                let devices = format!("group:{g}");
                s.rules.retain(|r| r.devices != devices);
                self.saved(&s)?;
                Ok(ok(json!({})))
            }
            ("GET", "devices", None) => Ok(ok(json!({"devices": self.store().devices.values().map(Device::listed).collect::<Vec<_>>()}))),
            ("PUT", "devices", Some(id)) => {
                let v = req.json()?;
                let mut s = self.store();
                let group = v["group"].as_str().map(str::trim);
                if group.is_some_and(|g| !g.is_empty() && !s.device_groups.contains_key(g)) {
                    return Err(fail(400, "there is no such device group"));
                }
                let name = v["name"].as_str().map(|n| n.trim().to_string());
                if name.as_deref().is_some_and(|n| !name_ok(n)) {
                    return Err(fail(400, "a computer's name is 1 to 64 characters"));
                }
                let d = s.devices.get_mut(id).ok_or_else(|| fail(404, "that computer is not in the team"))?;
                if let Some(n) = name {
                    d.name = n;
                }
                if let Some(g) = group {
                    d.group = g.to_string();
                }
                if v["end"].as_bool() == Some(true) {
                    d.end = true;
                }
                let out = d.listed();
                self.saved(&s)?;
                Ok(ok(json!({"device": out})))
            }
            ("DELETE", "devices", Some(id)) => {
                let mut s = self.store();
                let d = s.devices.remove(id).ok_or_else(|| fail(404, "that computer is not in the team"))?;
                self.saved(&s)?;
                drop(s);
                self.audit(&[json!({"time": now(), "kind": "removed", "user": me.name, "device": d.id, "device_name": d.name})]);
                Ok(ok(json!({})))
            }
            ("GET", "rules", None) => Ok(ok(json!({"rules": self.store().rules.iter().map(Rule::json).collect::<Vec<_>>()}))),
            ("PUT", "rules", None) => {
                let v = req.json()?;
                let list = if v.is_array() { &v } else { &v["rules"] };
                let list = list.as_array().ok_or_else(|| fail(400, "the rules are a list"))?;
                if list.len() > 10_000 {
                    return Err(fail(400, "at most 10000 rules"));
                }
                let mut s = self.store();
                let rules = list.iter().map(|r| rule_from(r, &s)).collect::<Result<Vec<_>, _>>().map_err(|e| fail(400, &e))?;
                s.rules = rules;
                self.saved(&s)?;
                Ok(ok(json!({"rules": s.rules.iter().map(Rule::json).collect::<Vec<_>>()})))
            }
            ("GET", "tokens", None) => Ok(ok(json!({"tokens": self.store().tokens.values().map(Token::listed).collect::<Vec<_>>()}))),
            ("POST", "tokens", None) => {
                let v = req.json()?;
                let now = now();
                let group = v["group"].as_str().unwrap_or("").trim().to_string();
                let expires = match &v["expires"] {
                    Value::Null => 0,
                    Value::Number(n) if n.as_u64() == Some(0) => 0,
                    Value::Number(n) => n.as_u64().and_then(|n| when(&n.to_string(), true)).ok_or_else(|| fail(400, "\"expires\" is a time"))?,
                    Value::String(t) if t.trim().is_empty() => 0,
                    Value::String(t) => when(t, true).ok_or_else(|| fail(400, "\"expires\" is a time"))?,
                    _ => return Err(fail(400, "\"expires\" is a time")),
                };
                if expires != 0 && expires <= now {
                    return Err(fail(400, "the token would have run out already"));
                }
                let uses = v["uses"].as_u64().unwrap_or(0);
                let mut s = self.store();
                if !group.is_empty() && !s.device_groups.contains_key(&group) {
                    return Err(fail(400, "there is no such device group"));
                }
                let token = super::new_secret();
                let t = Token { id: crate::sys::hex(&crate::sys::random(8)), hash: secret_hash(&token), group, expires, uses, used: 0, created: now };
                s.tokens.insert(t.id.clone(), t.clone());
                self.saved(&s)?;
                let mut out = t.listed();
                out["token"] = json!(token);
                out["install"] = json!({
                    "sh": format!("curl -fsSL '{}/install.sh?token={token}' | sh", self.url),
                    "ps1": format!("irm '{}/install.ps1?token={token}' | iex", self.url),
                    "join": format!("sshremote team join {} {token}", self.url),
                });
                Ok(ok(out))
            }
            ("DELETE", "tokens", Some(id)) => {
                let mut s = self.store();
                if s.tokens.remove(id).is_none() {
                    return Err(fail(404, "there is no such token"));
                }
                self.saved(&s)?;
                Ok(ok(json!({})))
            }
            ("GET", "audit", None) => Ok(ok(json!({"events": self.audit_events(&req.query, 1000)}))),
            ("GET", "audit.csv", None) => {
                let mut a = file("text/csv; charset=utf-8", &audit_csv(&self.audit_events(&req.query, 1_000_000)));
                a.headers.push(("Content-Disposition", "attachment; filename=\"audit.csv\"".into()));
                Ok(a)
            }
            ("GET", "settings", None) => Ok(ok(self.settings(&self.store()))),
            ("PUT", "settings", None) => self.put_settings(req),
            // An invitation to set a password, by email.
            ("POST", "invite", None) => {
                let v = req.json()?;
                self.mail_link(v["name"].as_str().unwrap_or(""), false, Some(if me.display.is_empty() { &me.name } else { &me.display }))?;
                Ok(ok(json!({"sent": true})))
            }
            ("POST", "mail-test", None) => {
                let v = req.json()?;
                let to = v["to"].as_str().filter(|t| !t.is_empty()).unwrap_or(&me.email).trim().to_string();
                let s = self.store();
                let (Some(m), team) = (s.smtp.clone(), s.name.clone()) else { return Err(fail(400, "the team has no mail server set up (Settings, Email)")) };
                drop(s);
                if !mail_ok(&to) {
                    return Err(fail(400, "that is not an email address"));
                }
                let body = format!("This email came from the team server at {}: its email works.\n", self.url);
                self.send_mail(&m, &team, &to, &format!("A test email from {team}"), &body).map_err(|e| fail(502, &format!("the mail server did not take the email ({})", clean(&e, 200))))?;
                Ok(ok(json!({"sent": true})))
            }
            _ => Err(fail(404, "no such request")),
        }
    }

    /// A user made (`old` None) or changed, as the console sends it. `"totp": true` gives them an
    /// authenticator, whose secret comes back with the answer to be set up once; `"totp_reset": true`
    /// (or `"totp": false`) takes it away.
    fn put_user(&self, req: &Req, old: Option<&str>, own: &str) -> Reply {
        let v = req.json()?;
        // The password's hash takes a while: not under the lock.
        let password = match v["password"].as_str().filter(|p| !p.is_empty()) {
            Some(p) if p.chars().count() < 8 => return Err(fail(400, "a password has at least 8 characters")),
            Some(p) => Some(super::password_hash(p)),
            None => None,
        };
        let mut s = self.store();
        let before = match old {
            Some(n) => Some(s.users.get(n).cloned().ok_or_else(|| fail(404, "there is no such user"))?),
            None => None,
        };
        let name = match (v["name"].as_str(), &before) {
            (Some(n), _) => n.trim().to_string(),
            (None, Some(b)) => b.name.clone(),
            (None, None) => String::new(),
        };
        if !name_ok(&name) {
            return Err(fail(400, "a name is 1 to 64 characters, with no control characters"));
        }
        if s.users.keys().any(|k| Some(k.as_str()) != old && k.eq_ignore_ascii_case(&name)) {
            return Err(fail(409, "there is a user of that name already"));
        }
        let mut u = before.clone().unwrap_or_else(|| User { display: name.clone(), ..User::default() });
        u.name = name.clone();
        if let Some(d) = v["display"].as_str() {
            u.display = clean(d, 128);
        }
        if let Some(e) = v["email"].as_str() {
            let e = e.trim();
            if e.len() > 254 || e.chars().any(|c| c.is_control() || c.is_whitespace()) || (!e.is_empty() && !e.contains('@')) {
                return Err(fail(400, "that is not an email address"));
            }
            u.email = e.to_string();
        }
        if let Some(a) = v["admin"].as_bool() {
            u.admin = a;
        }
        if let Some(d) = v["disabled"].as_bool() {
            u.disabled = d;
        }
        if v.get("groups").is_some_and(|g| !g.is_null()) {
            let mut groups = strings(&v["groups"]);
            if let Some(g) = groups.iter().find(|g| !s.groups.contains(*g)) {
                return Err(fail(400, &format!("there is no group \"{}\"", g)));
            }
            groups.sort();
            groups.dedup();
            u.groups = groups;
        }
        // No password any more: single sign-on or the directory only.
        let cleared = v["password_clear"].as_bool() == Some(true) && password.is_none() && !u.password.is_empty();
        if cleared {
            u.password.clear();
        }
        let new_password = password.is_some() || cleared;
        if let Some(h) = password {
            u.password = h;
        }
        let mut secret = None;
        if v["totp_reset"].as_bool() == Some(true) || v["totp"].as_bool() == Some(false) {
            // A lost or new phone: the password alone signs in again, until another is set up.
            u.totp.clear();
        } else if v["totp"].as_bool() == Some(true) && u.totp.is_empty() {
            let t = crate::totp::new_secret();
            u.totp = t.clone();
            secret = Some(t);
        }
        let mut users = s.users.clone();
        if let Some(o) = old {
            users.remove(o);
        }
        users.insert(name.clone(), u.clone());
        if !users.values().any(|x| x.admin && !x.disabled) {
            return Err(fail(400, "the team needs an administrator: make another one first"));
        }
        s.users = users;
        if let Some(o) = old.filter(|o| *o != name) {
            let (was, now) = (format!("user:{o}"), format!("user:{name}"));
            for r in s.rules.iter_mut().filter(|r| r.who == was) {
                r.who = now.clone();
            }
            for x in s.sessions.values_mut().filter(|x| x.user == o) {
                x.user = name.clone();
            }
            let _ = std::fs::rename(self.book_file(o), self.book_file(&name));
        }
        // A new password or a disabled account ends its sessions (but not the one making the change).
        if before.is_some() && (new_password || u.disabled) {
            s.sessions.retain(|h, x| x.user != name || h == own);
        }
        self.saved(&s)?;
        let uri = secret.as_ref().map(|t| crate::totp::uri(t, &esc(&format!("{}:{name}", s.name))));
        let qr = uri.as_deref().and_then(qr_svg);
        Ok(ok(json!({"user": u.listed(), "totp_secret": secret, "totp_uri": uri, "totp_qr": qr})))
    }

    fn put_device_group(&self, req: &Req, old: Option<&str>) -> Reply {
        let v = req.json()?;
        let name = old.map(str::to_string).unwrap_or_else(|| v["name"].as_str().unwrap_or("").trim().to_string());
        if !name_ok(&name) {
            return Err(fail(400, "a device group's name is 1 to 64 characters"));
        }
        let mut s = self.store();
        if old.is_some() && !s.device_groups.contains_key(&name) {
            return Err(fail(404, "there is no such device group"));
        }
        let policy = if v["policy"].is_object() { Policy::from_json(&v["policy"]) } else { s.device_groups.get(&name).cloned().unwrap_or_default() };
        s.device_groups.insert(name.clone(), policy.clone());
        self.saved(&s)?;
        let devices = s.devices.values().filter(|d| d.group == name).count();
        Ok(ok(json!({"group": {"name": name, "policy": policy.json(), "devices": devices}})))
    }

    fn settings(&self, s: &Store) -> Value {
        json!({
            "name": s.name,
            "logo": s.logo,
            "relay": s.relay,
            "brand": s.brand,
            "url": self.url,
            "redirect_uri": self.redirect_uri(),
            "oidc": s.oidc.as_ref().map(|o| json!({"issuer": o.issuer, "client_id": o.client_id, "client_secret_set": !o.client_secret.is_empty(), "domains": o.domains, "make_users": o.make_users, "trust_email": o.trust_email})),
            "ldap": s.ldap.as_ref().map(|l| json!({"url": l.url, "user_dn": l.user_dn, "make_users": l.make_users, "ca_file": l.ca_file})),
            "smtp": s.smtp.as_ref().map(|m| json!({"url": m.url, "user": m.user, "password_set": !m.password.is_empty(), "from": m.from})),
        })
    }

    fn put_settings(&self, req: &Req) -> Reply {
        let v = req.json()?;
        let mut kept = self.store();
        // Changed on a copy: a setting refused leaves the others as they were too.
        let mut s = kept.clone();
        if let Some(n) = v["name"].as_str() {
            let n = n.trim();
            if !name_ok(n) {
                return Err(fail(400, "the team's name is 1 to 64 characters"));
            }
            s.name = n.to_string();
        }
        if let Some(l) = v["logo"].as_str() {
            if !l.is_empty() && !logo_ok(l) {
                return Err(fail(400, "the logo is a PNG, JPEG, GIF, WebP or SVG picture of at most 256 KB, as a data: URL"));
            }
            s.logo = l.to_string();
        }
        if let Some(r) = v["relay"].as_str() {
            let r = r.trim();
            if !relay_ok(r) {
                return Err(fail(400, "the relay is HOST or HOST:PORT"));
            }
            s.relay = r.to_string();
        }
        if let Some(b) = v["brand"].as_bool() {
            s.brand = b;
        }
        match v.get("oidc") {
            None => {}
            Some(Value::Null) => s.oidc = None,
            Some(o) => {
                let issuer = st(o, "issuer").trim().trim_end_matches('/').to_string();
                if !(issuer.starts_with("https://") || issuer.starts_with("http://")) || issuer.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err(fail(400, "the identity provider's issuer is its https:// address"));
                }
                let client_id = st(o, "client_id").trim().to_string();
                if client_id.is_empty() || client_id.len() > 512 || client_id.chars().any(char::is_control) {
                    return Err(fail(400, "single sign-on needs the client ID the identity provider gave"));
                }
                // Never sent back: an empty one keeps the one there is.
                let secret = st(o, "client_secret");
                let client_secret = if secret.is_empty() { s.oidc.as_ref().map(|o| o.client_secret.clone()).unwrap_or_default() } else { secret };
                if client_secret.chars().any(char::is_control) {
                    return Err(fail(400, "the client secret has a control character"));
                }
                let domains: Vec<String> = strings(&o["domains"]).iter().map(|d| d.trim().trim_start_matches('@').to_lowercase()).filter(|d| !d.is_empty()).collect();
                if domains.iter().any(|d| !d.chars().all(|c| c.is_alphanumeric() || ".-".contains(c))) {
                    return Err(fail(400, "a domain is like example.com"));
                }
                s.oidc = Some(Oidc { issuer, client_id, client_secret, domains, make_users: o["make_users"].as_bool().unwrap_or(false), trust_email: o["trust_email"].as_bool().unwrap_or(false) });
            }
        }
        match v.get("smtp") {
            None => {}
            Some(Value::Null) => s.smtp = None,
            Some(m) => {
                let url = st(m, "url").trim().trim_end_matches('/').to_string();
                if !(url.starts_with("smtps://") || url.starts_with("smtp://")) || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err(fail(400, "the mail server's address is smtps://HOST or smtp://HOST:587"));
                }
                let from = st(m, "from").trim().to_string();
                if !mail_ok(&from) {
                    return Err(fail(400, "the sender is an email address, like team@example.com"));
                }
                let user = st(m, "user").trim().to_string();
                // Never sent back: an empty one keeps the one there is.
                let given = st(m, "password");
                let password = if given.is_empty() { s.smtp.as_ref().map(|m| m.password.clone()).unwrap_or_default() } else { given };
                if user.chars().chain(password.chars()).any(char::is_control) {
                    return Err(fail(400, "the mail server's name or password has a control character"));
                }
                s.smtp = Some(Smtp { url, user, password, from });
            }
        }
        match v.get("ldap") {
            None => {}
            Some(Value::Null) => s.ldap = None,
            Some(l) => {
                let url = st(l, "url").trim().to_string();
                if !(url.starts_with("ldaps://") || url.starts_with("ldap://")) || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err(fail(400, "the directory's address is ldaps://HOST/BASE (or ldap://)"));
                }
                let user_dn = st(l, "user_dn").trim().to_string();
                if !user_dn.contains("{}") || user_dn.contains(':') || user_dn.chars().any(char::is_control) {
                    return Err(fail(400, "the user DN has {} where the name goes, like uid={},ou=people,dc=example,dc=com"));
                }
                let ca_file = st(l, "ca_file").trim().to_string();
                if !ca_file.is_empty() && (!Path::new(&ca_file).is_absolute() || ca_file.chars().any(char::is_control) || !Path::new(&ca_file).is_file()) {
                    return Err(fail(400, "the authority's certificate is a file on the team server: give its full path"));
                }
                s.ldap = Some(Ldap { url, user_dn, make_users: l["make_users"].as_bool().unwrap_or(false), ca_file });
            }
        }
        self.saved(&s)?;
        if s.oidc != kept.oidc {
            *lock(&self.discovery) = None;
        }
        *kept = s;
        Ok(ok(self.settings(&kept)))
    }
}

fn groups_list(s: &Store) -> Vec<Value> {
    s.groups.iter().map(|g| json!({"name": g, "users": s.users.values().filter(|u| u.groups.contains(g)).map(|u| u.name.clone()).collect::<Vec<_>>()})).collect()
}

fn device_groups_list(s: &Store) -> Vec<Value> {
    s.device_groups.iter().map(|(g, p)| json!({"name": g, "policy": p.json(), "devices": s.devices.values().filter(|d| &d.group == g).count()})).collect()
}

/// `sshremote team admin NAME [--data DIR] [--totp]`: makes NAME an administrator (or gives one a
/// new password), on the server; `--totp` gives them a new authenticator too.
pub fn admin_cli(args: &[String]) -> Result<(), String> {
    let usage = "usage: sshremote team admin NAME [--data DIR] [--totp]";
    let mut name = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data" => i += 1,
            "--totp" => {}
            a if a.starts_with("--") => return Err(usage.into()),
            a => name = Some(a.trim().to_string()),
        }
        i += 1;
    }
    let name = name.ok_or(usage)?;
    if !name_ok(&name) {
        return Err("a name is 1 to 64 characters, with no control characters".into());
    }
    let dir = flag(args, "--data").map_or_else(default_dir, PathBuf::from);
    make_dir(&dir)?;
    let file = dir.join("team.json");
    // Read first: a record that does not read is not written over.
    load(&file)?;
    let password = super::read_secret(&format!("password for {name}: ")).map_err(|e| e.to_string())?;
    if password.chars().count() < 8 {
        return Err("a password has at least 8 characters".into());
    }
    if super::read_secret("the same again: ").map_err(|e| e.to_string())? != password {
        return Err("the two passwords are not the same".into());
    }
    let hash = super::password_hash(&password);
    let mut s = load(&file)?;
    let key = s.user(&name).map_or_else(|| name.clone(), |u| u.name.clone());
    let u = s.users.entry(key).or_insert_with(|| User { name: name.clone(), display: name.clone(), ..User::default() });
    u.password = hash;
    u.admin = true;
    u.disabled = false;
    let totp = args.iter().any(|a| a == "--totp").then(crate::totp::new_secret);
    if let Some(t) = &totp {
        u.totp = t.clone();
    }
    let shown = u.name.clone();
    let team = s.name.clone();
    let text = serde_json::to_vec_pretty(&s.stored()).map_err(|e| e.to_string())?;
    crate::sys::write_whole(&file, &text, true, true).map_err(|e| format!("{}: {e}", file.display()))?;
    println!("{shown} is an administrator of the team in {} (a running server takes it at its next request)", dir.display());
    if let Some(t) = totp {
        println!("Authenticator app: add the key {} (or open {})", crate::totp::grouped(&t), crate::totp::uri(&t, &esc(&format!("{team}:{shown}"))));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::client::http;

    fn temp_dir(what: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sshremote-team-{what}-{}-{}", std::process::id(), crate::sys::hex(&crate::sys::random(4))))
    }

    /// A test's record, removed when the test is done.
    struct Gone(PathBuf);

    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A team server on a free port of this computer, with one administrator: ana.
    /// A mail server of the test's own: each mail it takes (its envelope's recipient and its
    /// text) comes out of the channel.
    fn mail_server() -> (String, std::sync::mpsc::Receiver<(String, String)>) {
        use std::io::BufRead;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("smtp://{}", l.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for conn in l.incoming().flatten() {
                let mut w = conn.try_clone().unwrap();
                let mut r = BufReader::new(conn);
                let _ = w.write_all(b"220 test\r\n");
                let (mut to, mut line) = (String::new(), String::new());
                while r.read_line(&mut line).unwrap_or(0) > 0 {
                    let up = line.to_ascii_uppercase();
                    let answer: &[u8] = if up.starts_with("EHLO") {
                        b"250-test\r\n250 OK\r\n"
                    } else if up.starts_with("RCPT TO:") {
                        to = line[8..].trim().trim_matches(['<', '>']).to_string();
                        b"250 OK\r\n"
                    } else if up.starts_with("DATA") {
                        let _ = w.write_all(b"354 go on\r\n");
                        let mut text = String::new();
                        let mut l2 = String::new();
                        while r.read_line(&mut l2).unwrap_or(0) > 0 && l2 != ".\r\n" {
                            text.push_str(&l2);
                            l2.clear();
                        }
                        let _ = tx.send((to.clone(), text));
                        b"250 taken\r\n"
                    } else if up.starts_with("QUIT") {
                        let _ = w.write_all(b"221 bye\r\n");
                        break;
                    } else {
                        b"250 OK\r\n"
                    };
                    let _ = w.write_all(answer);
                    line.clear();
                }
            }
        });
        (url, rx)
    }

    #[test]
    fn invitations_and_new_passwords_go_by_email() {
        let (url, team, _gone) = start("mail");
        let (smtp, mails) = mail_server();
        let admin = login(&url, "ana", "ana's password");
        let api = |p: &str| format!("{url}{p}");
        assert_eq!(call("POST", &api("/api/admin/invite"), Some(&admin), Some(json!({"name": "ana"}))).0, 400, "no mail server yet");
        let (code, v) = call("PUT", &api("/api/admin/settings"), Some(&admin), Some(json!({"smtp": {"url": smtp, "from": "team@example.com", "user": "", "password": ""}})));
        assert_eq!(code, 200, "{v}");
        assert_eq!(call("PUT", &api("/api/admin/settings"), Some(&admin), Some(json!({"smtp": {"url": smtp, "from": "x\r\nBcc: y@z.com"}}))).0, 400);
        let (code, v) = call("POST", &api("/api/admin/users"), Some(&admin), Some(json!({"name": "mia", "display": "Mia Kova\u{10d}", "email": "mia@example.com"})));
        assert_eq!(code, 200, "{v}");
        assert_eq!(call("POST", &api("/api/admin/invite"), Some(&admin), Some(json!({"name": "mia"}))).0, 200);
        let (to, text) = mails.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(to, "mia@example.com");
        assert!(text.contains("Subject: You are invited to Team\r\n") && text.contains("From: Team <team@example.com>\r\n") && text.contains("Hello Mia Kova\u{10d},"), "{text}");
        let token = text.split("#link=").nth(1).unwrap()[..64].to_string();
        // The link: whose it is, a password set once, and signing in with it.
        assert_eq!(call("GET", &api(&format!("/api/link?token={token}")), None, None).1["user"], json!("mia"));
        assert_eq!(call("POST", &api("/api/link"), None, Some(json!({"token": token, "password": "short"}))).0, 400);
        assert_eq!(call("POST", &api("/api/link"), None, Some(json!({"token": token, "password": "mia's password"}))).0, 200);
        assert_eq!(call("POST", &api("/api/link"), None, Some(json!({"token": token, "password": "another one"}))).0, 404);
        login(&url, "mia", "mia's password");
        // A forgotten one: the same answer for anyone, a mail only for an account that has an email.
        assert_eq!(call("POST", &api("/api/reset"), None, Some(json!({"name": "nobody"}))).1, json!({"sent": true}));
        assert_eq!(call("POST", &api("/api/reset"), None, Some(json!({"name": "MIA@example.com"}))).1, json!({"sent": true}));
        let (to, text) = mails.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(to == "mia@example.com" && text.contains("Subject: A new password for Team\r\n"), "{text}");
        assert!(mails.try_recv().is_err(), "nothing for nobody");
        assert_eq!((mail_ok("a@b.co"), mail_ok("a b@c.com"), mail_ok("a@b"), mail_ok("<a@b.co>")), (true, false, false, false));
        assert_eq!(mail_date(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 +0000");
        assert_eq!((mail_words("Team"), mail_words("\u{c9}quipe")), ("Team".to_string(), "=?UTF-8?B?w4lxdWlwZQ==?=".to_string()));
        drop(team);
    }

    fn start(what: &str) -> (String, Arc<Team>, Gone) {
        // Where the client keeps a request's body for curl.
        let _ = std::fs::create_dir_all(crate::hosts::config_dir().unwrap());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let team = Arc::new(Team::open(&temp_dir(what), &url).unwrap());
        {
            let mut s = team.store();
            s.users.insert("ana".into(), User { name: "ana".into(), display: "Ana".into(), admin: true, password: crate::team::password_hash("ana's password"), ..User::default() });
            team.save(&s).unwrap();
        }
        let t = team.clone();
        std::thread::spawn(move || listen(l, t));
        let gone = Gone(team.dir.clone());
        (url, team, gone)
    }

    fn call(method: &str, url: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
        let headers: Vec<(String, String)> = token.map(|t| vec![("Authorization".to_string(), format!("Bearer {t}"))]).unwrap_or_default();
        let body = body.map(|b| b.to_string().into_bytes());
        http(method, url, &headers, body.as_deref()).unwrap()
    }

    fn login(url: &str, name: &str, password: &str) -> String {
        let (code, v) = call("POST", &format!("{url}/api/login"), None, Some(json!({"name": name, "password": password})));
        assert_eq!(code, 200, "{v}");
        v["token"].as_str().unwrap().to_string()
    }

    /// What an app keeps (the verifier) and what it shows (the challenge).
    fn app_secret() -> (String, String) {
        let verifier = crate::sys::hex(&crate::sys::random(32));
        let challenge = crate::sys::hex(&sha2::Sha256::digest(verifier.as_bytes()));
        (verifier, challenge)
    }

    fn signed(id: &str, key: &[u8], method: &str, path: &str, body: &str, time: u64) -> Vec<(String, String)> {
        let stamp = format!("{time}.{}", crate::sys::hex(&crate::sys::random(8)));
        vec![
            ("X-Team-Device".into(), id.into()),
            ("X-Team-Time".into(), stamp.clone()),
            ("X-Team-Sign".into(), crate::team::request_sign(key, method, path, &stamp, body.as_bytes())),
            ("X-Team-Version".into(), "9.9.9".into()),
        ]
    }

    /// A GET by curl itself: the status, and where it sends one on.
    fn fetch(url: &str) -> (u16, String) {
        fetch_as("browser", url)
    }

    /// The same from one browser of this test's own (`who`): its cookies kept between calls.
    fn fetch_as(who: &str, url: &str) -> (u16, String) {
        let jar = std::env::temp_dir().join(format!("sshremote-team-jar-{}-{:?}-{who}", std::process::id(), std::thread::current().id()));
        let jar = jar.to_string_lossy().into_owned();
        let out = std::process::Command::new("curl").args(["-sS", "-b", &jar, "-c", &jar, "-w", "\n@@%{http_code} %{redirect_url}", url]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (_, tail) = text.rsplit_once("\n@@").unwrap();
        let (code, to) = tail.split_once(' ').unwrap_or((tail, ""));
        (code.parse().unwrap(), to.to_string())
    }

    #[test]
    fn rules_give_every_matching_rights_and_ask_if_any_asks() {
        let bo = User { name: "bo".into(), groups: vec!["staff".into()], ..User::default() };
        let desk = Device { id: "123456789".into(), group: "office".into(), key: vec![0; 32], ..Device::default() };
        let rule = |who: &str, devices: &str, rights: u8, ask: bool| Rule { who: who.into(), devices: devices.into(), rights, ask };
        assert_eq!(access(&[], &bo, &desk), None);
        assert_eq!(access(&[rule("user:cy", "all", 1, false), rule("all", "group:lab", 1, false), rule("group:staff", "device:111111111", 1, false)], &bo, &desk), None);
        let rules = [rule("group:staff", "group:office", 1, false), rule("user:bo", "device:123456789", 4, true), rule("all", "device:111111111", 64, false)];
        assert_eq!(access(&rules, &bo, &desk), Some((5, true)));
        // A rule of no rights still reaches it (to watch the screen).
        assert_eq!(access(&[rule("all", "all", 0, false)], &bo, &desk), Some((0, false)));
        // A computer of no group is reached only by its ID or by every computer.
        let loose = Device { group: String::new(), ..desk.clone() };
        assert_eq!(access(&[rule("all", "group:", 1, false)], &bo, &loose), None);
        // What the device group's policy lets anyone do caps it.
        let mut s = Store::read(&Value::Null);
        s.users.insert("bo".into(), bo.clone());
        s.groups.insert("staff".into());
        s.device_groups.insert("office".into(), Policy { rights: 1, ..Policy::default() });
        s.rules = rules.to_vec();
        assert_eq!(s.reach(&bo, &desk), Some((1, true)));
        // Rules name what there is.
        assert!(rule_from(&json!({"who": "group:nobody", "devices": "all"}), &s).is_err());
        assert!(rule_from(&json!({"who": "user:bo", "devices": "device:12"}), &s).is_err());
        assert!(rule_from(&json!({"who": "user:bo", "devices": "group:lab"}), &s).is_err());
        assert!(rule_from(&json!({"who": "everyone", "devices": "all"}), &s).is_err());
        assert_eq!(rule_from(&json!({"who": "user:bo", "devices": "group:office", "rights": 0xff, "ask": true}), &s), Ok(rule("user:bo", "group:office", 0x7f, true)));
    }

    #[test]
    fn a_device_signature_needs_its_key_its_time_and_to_be_new() {
        let (key, t, path) = ([3u8; 32], 1_800_000_000u64, "/api/device/events");
        let stamp = format!("{t}.0a1b");
        let sign = crate::team::request_sign(&key, "POST", path, &stamp, b"{}");
        let mut seen = HashMap::new();
        assert!(check_sign(&[4u8; 32], "POST", path, b"{}", (&stamp, &sign), t, &mut seen).is_err());
        assert!(check_sign(&key, "POST", path, b"{ }", (&stamp, &sign), t, &mut seen).is_err());
        assert!(check_sign(&key, "PUT", path, b"{}", (&stamp, &sign), t, &mut seen).is_err());
        assert!(check_sign(&key, "POST", path, b"{}", (&stamp, "00"), t, &mut seen).is_err());
        assert!(check_sign(&key, "POST", path, b"{}", (&stamp, &sign), t + 301, &mut seen).is_err());
        assert!(check_sign(&key, "POST", path, b"{}", (&stamp, &sign), t - 301, &mut seen).is_err());
        assert!(check_sign(&key, "POST", path, b"{}", ("nonsense", &sign), t, &mut seen).is_err());
        assert!(check_sign(&key, "POST", path, b"{}", (&stamp, &sign.to_uppercase()), t + 300, &mut seen).is_ok());
        assert_eq!(check_sign(&key, "POST", path, b"{}", (&stamp, &sign), t + 1, &mut seen), Err("this request was taken already"));
        // The same request in the same second, with its own random part, is another one.
        let again = format!("{t}.0a1c");
        let sign2 = crate::team::request_sign(&key, "POST", path, &again, b"{}");
        assert!(check_sign(&key, "POST", path, b"{}", (&again, &sign2), t + 1, &mut seen).is_ok());
    }

    #[test]
    fn a_computer_joins_and_a_person_gets_a_ticket_it_takes() {
        let (url, team, _gone) = start("join");
        let api = |p: &str| format!("{url}{p}");
        let admin = login(&url, "ana", "ana's password");
        let (code, v) = call("POST", &api("/api/admin/device-groups"), Some(&admin), Some(json!({"name": "office", "policy": {"password": false, "rights": 0x15}})));
        assert_eq!(code, 200, "{v}");
        assert_eq!(call("POST", &api("/api/admin/groups"), Some(&admin), Some(json!({"name": "staff"}))).0, 200);
        let (code, v) = call("POST", &api("/api/admin/users"), Some(&admin), Some(json!({"name": "bo", "display": "Bo", "password": "bo's password", "groups": ["staff"]})));
        assert_eq!(code, 200, "{v}");
        assert_eq!(call("POST", &api("/api/admin/users"), Some(&admin), Some(json!({"name": "bo", "password": "another one"}))).0, 409);
        assert_eq!(call("POST", &api("/api/admin/users"), Some(&admin), Some(json!({"name": "cy", "groups": ["nobody"]}))).0, 400);

        // Joining: a token for the office, used once.
        let (code, v) = call("POST", &api("/api/admin/tokens"), Some(&admin), Some(json!({"group": "office", "uses": 1})));
        assert_eq!(code, 200, "{v}");
        let token = v["token"].as_str().unwrap().to_string();
        assert!(v["install"]["sh"].as_str().unwrap().contains(&format!("{url}/install.sh?token={token}")));
        let enroll = |id: &str| call("POST", &api("/api/device/enroll"), None, Some(json!({"token": token, "id": id, "name": "Front desk", "os": "linux", "version": "1.0"})));
        assert_eq!(enroll("12345").0, 400);
        let (code, v) = enroll("123456789");
        assert_eq!(code, 200, "{v}");
        let key = crate::sys::unhex(v["key"].as_str().unwrap()).unwrap();
        assert_eq!((key.len(), v["team"]["url"].as_str()), (32, Some(url.as_str())));
        assert_eq!(enroll("987654321").0, 403);
        assert!(team.store().tokens.values().all(|t| t.hash != token && t.used == 1));
        // Another token does not take a joined computer's place; its own key joins it again.
        let (_, v) = call("POST", &api("/api/admin/tokens"), Some(&admin), Some(json!({"group": "office", "uses": 5})));
        let other = v["token"].as_str().unwrap().to_string();
        let body = json!({"token": other, "id": "123456789", "name": "Front desk", "os": "linux", "version": "1.0"}).to_string();
        assert_eq!(http("POST", &api("/api/device/enroll"), &[], Some(body.as_bytes())).unwrap().0, 409);
        assert_eq!(http("POST", &api("/api/device/enroll"), &signed("123456789", &[1; 32], "POST", "/api/device/enroll", &body, now()), Some(body.as_bytes())).unwrap().0, 409);
        let (code, v) = http("POST", &api("/api/device/enroll"), &signed("123456789", &key, "POST", "/api/device/enroll", &body, now()), Some(body.as_bytes())).unwrap();
        assert_eq!(code, 200, "{v}");
        let key = crate::sys::unhex(v["key"].as_str().unwrap()).unwrap();

        // Its look: signed, once.
        let look = signed("123456789", &key, "GET", "/api/device/policy", "", now());
        let (code, v) = http("GET", &api("/api/device/policy"), &look, None).unwrap();
        assert_eq!(code, 200, "{v}");
        assert_eq!((v["policy"]["password"].as_bool(), v["policy"]["rights"].as_u64(), v["end"].as_bool()), (Some(false), Some(0x15), None));
        assert_eq!(v["team"]["url"].as_str(), Some(url.as_str()));
        assert_eq!(http("GET", &api("/api/device/policy"), &look, None).unwrap().0, 401);
        assert_eq!(http("GET", &api("/api/device/policy"), &signed("123456789", &[1; 32], "GET", "/api/device/policy", "", now()), None).unwrap().0, 401);
        assert_eq!(http("GET", &api("/api/device/policy"), &signed("123456789", &key, "GET", "/api/device/policy", "", now() - 400), None).unwrap().0, 401);
        assert_eq!(team.store().devices["123456789"].version, "9.9.9");

        // No rule lets bo in yet.
        let bo = login(&url, "bo", "bo's password");
        let viewer_key = crate::sys::hex(&[9u8; 32]);
        assert_eq!(call("POST", &api("/api/ticket"), Some(&bo), Some(json!({"device": "123456789", "key": viewer_key}))).0, 403);
        assert_eq!(call("GET", &api("/api/devices"), Some(&bo), None).1["devices"], json!([]));
        assert_eq!(call("GET", &api("/api/admin/users"), Some(&bo), None).0, 403);
        assert_eq!(call("PUT", &api("/api/admin/rules"), Some(&admin), Some(json!([{"who": "group:nobody", "devices": "all"}]))).0, 400);
        let (code, v) = call("PUT", &api("/api/admin/rules"), Some(&admin), Some(json!([{"who": "group:staff", "devices": "group:office", "rights": 0x07, "ask": true}])));
        assert_eq!(code, 200, "{v}");
        let (_, v) = call("GET", &api("/api/devices"), Some(&bo), None);
        assert_eq!(v["devices"], json!([{"id": "123456789", "name": "Front desk", "os": "linux", "group": "office", "online": true, "rights": 0x05, "ask": true}]));

        // The ticket, as the computer checks it: its tag made with the key it was given.
        assert_eq!(call("POST", &api("/api/ticket"), Some(&bo), Some(json!({"device": "123456789", "key": "abc"}))).0, 400);
        let (code, v) = call("POST", &api("/api/ticket"), Some(&bo), Some(json!({"device": "123456789", "key": viewer_key, "viewer": "555666777"})));
        assert_eq!(code, 200, "{v}");
        let (ticket, tag) = (v["ticket"].as_str().unwrap(), v["tag"].as_str().unwrap());
        let t = crate::team::check_ticket(ticket, "123456789").unwrap();
        assert_eq!(crate::team::ticket_tag(&key, ticket), tag);
        assert_ne!(crate::team::ticket_tag(&[1; 32], ticket), tag);
        assert_eq!((t.user.as_str(), t.display.as_str(), t.rights, t.ask, t.key, t.viewer.as_str()), ("bo", "Bo", 0x05, true, [9; 32], "555666777"));
        assert_eq!(v["expires"].as_u64(), Some(t.expires));

        // What the computer records reaches the audit log, with its name.
        let body = json!({"events": [{"time": now(), "kind": "in", "how": "team", "user": "bo", "from": "555666777", "rights": 5, "extra": "dropped"}]}).to_string();
        let (code, _) = http("POST", &api("/api/device/events"), &signed("123456789", &key, "POST", "/api/device/events", &body, now()), Some(body.as_bytes())).unwrap();
        assert_eq!(code, 200);
        let (_, v) = call("GET", &api("/api/admin/audit?device=123456789&kind=in"), Some(&admin), None);
        assert_eq!(v["events"].as_array().unwrap().len(), 1);
        assert_eq!((v["events"][0]["device_name"].as_str(), v["events"][0]["user"].as_str(), v["events"][0].get("extra")), (Some("Front desk"), Some("bo"), None));
        let (_, v) = call("GET", &api("/api/admin/audit?kind=ticket"), Some(&admin), None);
        assert_eq!(v["events"][0]["user"].as_str(), Some("bo"));

        // The console ends its sessions at its next look, once.
        assert_eq!(call("PUT", &api("/api/admin/devices/123456789"), Some(&admin), Some(json!({"end": true}))).0, 200);
        let (_, v) = http("GET", &api("/api/device/policy"), &signed("123456789", &key, "GET", "/api/device/policy", "", now() + 1), None).unwrap();
        assert_eq!(v["end"].as_bool(), Some(true));
        let (_, v) = http("GET", &api("/api/device/policy"), &signed("123456789", &key, "GET", "/api/device/policy", "", now() + 2), None).unwrap();
        assert_eq!(v["end"].as_bool(), None);

        // The address book: a salt made once, and what was put.
        let (_, v) = call("GET", &api("/api/book"), Some(&bo), None);
        let salt = v["salt"].as_str().unwrap().to_string();
        assert_eq!((v["book"].clone(), salt.len()), (Value::Null, 32));
        assert_eq!(call("PUT", &api("/api/book"), Some(&bo), Some(json!({"book": "c2VhbGVk"}))).0, 200);
        assert_eq!(call("PUT", &api("/api/book"), Some(&bo), Some(json!({"book": "not base64!"}))).0, 400);
        let (_, v) = call("GET", &api("/api/book"), Some(&bo), None);
        assert_eq!((v["book"].as_str(), v["salt"].as_str()), (Some("c2VhbGVk"), Some(salt.as_str())));

        // Leaving: the computer is gone.
        let (code, _) = http("POST", &api("/api/device/leave"), &signed("123456789", &key, "POST", "/api/device/leave", "{}", now()), Some(b"{}")).unwrap();
        assert_eq!(code, 200);
        assert_eq!(call("POST", &api("/api/ticket"), Some(&bo), Some(json!({"device": "123456789", "key": viewer_key}))).0, 404);

        // An authenticator given (its secret shown once), and taken away again for a lost phone.
        let (code, v) = call("PUT", &api("/api/admin/users/bo"), Some(&admin), Some(json!({"totp": true})));
        assert_eq!(code, 200, "{v}");
        assert!(v["totp_uri"].as_str().unwrap().starts_with("otpauth://totp/") && v["user"]["totp"] == json!(true));
        #[cfg(feature = "gui")]
        assert!(v["totp_qr"].as_str().unwrap().starts_with("data:image/svg+xml;base64,"));
        let secret = v["totp_secret"].as_str().unwrap().to_string();
        assert_eq!(team.store().users["bo"].totp, secret);
        assert_eq!(call("POST", &api("/api/login"), None, Some(json!({"name": "bo", "password": "bo's password"}))).1["code"], json!(true));
        assert_eq!(call("PUT", &api("/api/admin/users/bo"), Some(&admin), Some(json!({"totp_reset": true}))).1["user"]["totp"], json!(false));
        login(&url, "bo", "bo's password");

        // The last administrator stays one; a signed-out session is no more.
        assert_eq!(call("PUT", &api("/api/admin/users/ana"), Some(&admin), Some(json!({"admin": false}))).0, 400);
        assert_eq!(call("DELETE", &api("/api/admin/users/ana"), Some(&admin), None).0, 400);
        assert_eq!(call("POST", &api("/api/logout"), Some(&bo), Some(json!({}))).0, 200);
        assert_eq!(call("GET", &api("/api/me"), Some(&bo), None).0, 401);
    }

    #[test]
    fn a_code_is_asked_for_and_wrong_tries_lock_the_address_out() {
        let (url, team, _gone) = start("login");
        let secret = crate::totp::new_secret();
        {
            let mut s = team.store();
            s.users.get_mut("ana").unwrap().totp = secret.clone();
            team.save(&s).unwrap();
        }
        let api = format!("{url}/api/login");
        let (code, v) = call("POST", &api, None, Some(json!({"name": "ana", "password": "ana's password"})));
        assert_eq!((code, v["code"].as_bool(), v["error"].as_str()), (401, Some(true), Some("code needed")));
        // Signed in from the page an app opened: the page goes on to the app's own port with a
        // code, which only the app's verifier turns into its session, once.
        let (verifier, challenge) = app_secret();
        let right = crate::totp::now(&secret);
        let (code, v) = call("POST", &api, None, Some(json!({"name": "Ana", "password": "ana's password", "code": right, "app": challenge, "port": 40123})));
        assert_eq!(code, 200, "{v}");
        assert_eq!((v["user"]["name"].as_str(), v["user"]["totp"].as_bool()), (Some("ana"), Some(true)));
        assert!(v["expires"].as_u64().unwrap() <= now() + CONSOLE_SECS);
        let handed = v["to"].as_str().unwrap().strip_prefix("http://127.0.0.1:40123/?code=").unwrap().to_string();
        let take = |code: &str, verifier: &str| call("POST", &format!("{url}/api/app-login"), None, Some(json!({"code": code, "verifier": verifier})));
        let (code, v) = take(&handed, &verifier);
        assert_eq!(code, 200, "{v}");
        assert!(v["expires"].as_u64().unwrap() > now() + 29 * 86_400);
        let app = v["token"].as_str().unwrap().to_string();
        assert_eq!(take(&handed, &verifier).0, 404, "once");
        assert_eq!(call("GET", &format!("{url}/api/me"), Some(&app), None).1["user"]["name"].as_str(), Some("ana"));
        assert_eq!(call("POST", &api, None, Some(json!({"name": "ana", "password": "x", "app": challenge, "port": 80}))).0, 400, "a port below 1024");
        assert_eq!(call("POST", &api, None, Some(json!({"name": "ana", "password": "x", "app": "12", "port": 40123}))).0, 400);
        // A code works once (a wrong try), spaced out too, and three wrong passwords more lock
        // this address out.
        assert_eq!(call("POST", &api, None, Some(json!({"name": "ana", "password": "ana's password", "code": right}))).0, 401);
        let spaced = format!("{} {}", &right[..3], &right[3..]);
        assert_eq!(call("POST", &api, None, Some(json!({"name": "ana", "password": "ana's password", "code": spaced}))).0, 401);
        for _ in 0..3 {
            assert_eq!(call("POST", &api, None, Some(json!({"name": "ana", "password": "wrong", "code": "123456"}))).0, 401);
        }
        let (code, v) = call("POST", &api, None, Some(json!({"name": "ana", "password": "ana's password", "code": crate::totp::now(&secret)})));
        assert_eq!(code, 429, "{v}");
        // Another address, as a proxy on this computer says it (the last of X-Forwarded-For), is
        // not; one the client says itself is no other.
        let from = |headers: &[(&str, &str)], body: Value| {
            let headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            http("POST", &api, &headers, Some(body.to_string().as_bytes())).unwrap()
        };
        assert_eq!(from(&[("X-Real-IP", "192.0.2.7")], json!({"name": "ana", "password": "x"})).0, 429);
        assert_eq!(from(&[("X-Forwarded-For", "192.0.2.7, 127.0.0.1")], json!({"name": "ana", "password": "x"})).0, 429);
        let (code, v) = from(&[("X-Forwarded-For", "127.0.0.1, 192.0.2.7")], json!({"name": "ana", "password": "ana's password"}));
        assert_eq!((code, v["code"].as_bool()), (401, Some(true)));
        // The code is asked for only once the password is right.
        let (code, v) = from(&[("X-Forwarded-For", "192.0.2.7")], json!({"name": "ana", "password": "x"}));
        assert_eq!((code, v["code"].as_bool()), (401, None));
        assert_eq!(from(&[("X-Forwarded-For", "192.0.2.8")], json!({"name": "nobody", "password": "x"})).0, 401);
        assert_eq!((address_key("2001:db8:1:2:3::9".parse().unwrap()), address_key("::ffff:192.0.2.1".parse().unwrap())), ("2001:db8:1:2::/64".to_string(), "192.0.2.1".to_string()));
    }

    #[test]
    fn single_sign_on_finds_or_makes_the_person() {
        let (url, team, _gone) = start("oidc");
        // An identity provider of our own: its settings, the code for a token (with the PKCE
        // verifier and the client secret), and who the token is.
        let idp = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", idp.local_addr().unwrap());
        let challenge: Arc<Mutex<String>> = Arc::default();
        // Who the provider says the person is.
        let said = Arc::new(Mutex::new(json!({"sub": "sub-1", "email": "Cy@Example.com", "name": "Cy", "email_verified": true})));
        {
            let (issuer, challenge, said) = (issuer.clone(), challenge.clone(), said.clone());
            std::thread::spawn(move || {
                for conn in idp.incoming().flatten() {
                    let Ok(req) = read_request(&conn, IpAddr::from([127, 0, 0, 1])) else { continue };
                    let a = match req.path.as_str() {
                        "/.well-known/openid-configuration" => ok(json!({"issuer": issuer, "authorization_endpoint": format!("{issuer}/auth"), "token_endpoint": format!("{issuer}/token"), "userinfo_endpoint": format!("{issuer}/me")})),
                        "/token" => {
                            let form = parse_query(&String::from_utf8_lossy(&req.body));
                            let pkce = crate::team::b64url(&sha2::Sha256::digest(form.get("code_verifier").map_or("", String::as_str).as_bytes()));
                            let good = form.get("code").map(String::as_str) == Some("good") && form.get("client_secret").map(String::as_str) == Some("s3cret") && pkce == *lock(&challenge);
                            if good { ok(json!({"access_token": "at", "token_type": "Bearer"})) } else { fail(400, "invalid_grant") }
                        }
                        "/me" if req.header("authorization") == "Bearer at" => ok(lock(&said).clone()),
                        _ => fail(404, "no"),
                    };
                    let _ = send(&conn, &a, true, false);
                }
            });
        }
        // Back to the console's page, the words in the fragment.
        let refused = |path: &str, app: Option<&str>| {
            let (code, to) = fetch(&format!("{url}{path}"));
            let page = app.map_or_else(|| format!("{url}/#error="), |n| format!("{url}/login?app={n}#error="));
            assert!(code == 302 && to.starts_with(&page), "{code} {to}");
            decode(&to[page.len()..], false)
        };
        assert_eq!(refused("/api/oidc/start", None), "Single sign-on is not set up on this team server.");
        {
            let mut s = team.store();
            s.oidc = Some(Oidc { issuer: issuer.clone(), client_id: "the team".into(), client_secret: "s3cret".into(), domains: vec!["example.com".into()], make_users: true, trust_email: false });
            team.save(&s).unwrap();
        }
        // From the console: on to the provider, back with a code, and into the console.
        let begin = |start: &str| {
            let (code, to) = fetch(&format!("{url}{start}"));
            assert_eq!(code, 302);
            assert!(to.starts_with(&format!("{issuer}/auth?")), "{to}");
            let q = parse_query(to.split_once('?').unwrap().1);
            assert_eq!((q["client_id"].as_str(), q["redirect_uri"].clone(), q["code_challenge_method"].as_str()), ("the team", format!("{url}/api/oidc/callback"), "S256"));
            *lock(&challenge) = q["code_challenge"].clone();
            q["state"].clone()
        };
        let state = begin("/api/oidc/start");
        assert!(refused("/api/oidc/callback?code=good&state=nope", None).contains("run out"));
        let (code, to) = fetch(&format!("{url}/api/oidc/callback?code=good&state={state}"));
        assert_eq!(code, 302);
        let token = to.strip_prefix(&format!("{url}/#session=")).unwrap();
        let (_, v) = call("GET", &format!("{url}/api/me"), Some(token), None);
        assert_eq!((v["user"]["name"].as_str(), v["user"]["display"].as_str()), (Some("cy@example.com"), Some("Cy")));
        // A state is used once; a wrong code is refused.
        assert!(refused(&format!("/api/oidc/callback?code=good&state={state}"), None).contains("run out"));
        let state = begin("/api/oidc/start");
        assert_eq!(refused(&format!("/api/oidc/callback?code=bad&state={state}"), None), "The identity provider gave no token (invalid_grant).");
        let state = begin("/api/oidc/start");
        assert!(refused(&format!("/api/oidc/callback?error=access_denied&state={state}"), None).contains("access_denied"));
        // From the app: on to its own port with a code, which it takes with its verifier; still one person.
        let (verifier, app) = app_secret();
        let state = begin(&format!("/api/oidc/start?app={app}&port=40124"));
        let (code, to) = fetch(&format!("{url}/api/oidc/callback?code=good&state={state}"));
        assert_eq!(code, 302);
        let handed = to.strip_prefix(&format!("{url}/login?app={app}&port=40124#handoff=")).unwrap();
        let (code, v) = call("POST", &format!("{url}/api/app-login"), None, Some(json!({"code": handed, "verifier": verifier})));
        assert_eq!((code, v["user"]["name"].as_str()), (200, Some("cy@example.com")));
        // A link someone else made: the code reaches only the app's port, and without its
        // verifier is worth nothing (and gone after one try).
        let state = begin(&format!("/api/oidc/start?app={app}&port=40124"));
        let (_, to) = fetch(&format!("{url}/api/oidc/callback?code=good&state={state}"));
        let handed = to.rsplit('=').next().unwrap();
        let take = |verifier: &str| call("POST", &format!("{url}/api/app-login"), None, Some(json!({"code": handed, "verifier": verifier}))).0;
        assert_eq!((take("a guess"), take(&verifier)), (403, 404));
        // The end of a sign-in begun in another browser (a link someone made to the end of
        // theirs) signs nobody in.
        let state = begin("/api/oidc/start");
        let (code, to) = fetch_as("someone else", &format!("{url}/api/oidc/callback?code=good&state={state}"));
        assert!(code == 302 && to.contains("#error=") && to.contains("another%20browser"), "{to}");
        // A failed one goes back to the app's sign-in page, with the words.
        let state = begin(&format!("/api/oidc/start?app={app}&port=40124"));
        let (_, to) = fetch(&format!("{url}/api/oidc/callback?error=access_denied&state={state}"));
        assert!(to.starts_with(&format!("{url}/login?app={app}&port=40124#error=")), "{to}");
        assert_eq!(team.store().users.len(), 2);
        // An email the provider does not say it verified finds and makes nobody; a verified one
        // does not take over an account with a password (here an administrator's).
        let words = |who: Value| {
            *lock(&said) = who;
            let state = begin("/api/oidc/start");
            let (_, to) = fetch(&format!("{url}/api/oidc/callback?code=good&state={state}"));
            to.split_once("#error=").map(|(_, w)| decode(w, false)).unwrap_or(to)
        };
        team.store().users.get_mut("ana").unwrap().email = "ana@example.com".into();
        assert!(words(json!({"sub": "x-2", "email": "dee@example.com"})).contains("not in the team"));
        assert!(words(json!({"sub": "x-3", "email": "dee@example.com", "email_verified": "false"})).contains("not in the team"));
        assert!(words(json!({"sub": "x-4", "email": "ana@example.com", "email_verified": "true"})).contains("does not take over this account"));
        assert_eq!(team.store().users.len(), 2);
        // A provider whose addresses are the organization's own, as set: taken unless it says no.
        team.store().oidc.as_mut().unwrap().trust_email = true;
        assert!(words(json!({"sub": "x-5", "email": "dee@example.com"})).starts_with(&format!("{url}/#session=")));
        assert!(words(json!({"sub": "x-6", "email": "eve@example.com", "email_verified": false})).contains("not in the team"));
        assert_eq!(team.store().users["dee@example.com"].oidc, format!("{issuer}#x-5"));
        team.store().oidc.as_mut().unwrap().trust_email = false;
        // The client secret is never shown.
        let admin = login(&url, "ana", "ana's password");
        let (_, v) = call("GET", &format!("{url}/api/login-options"), None, None);
        assert_eq!((v["oidc"].as_bool(), v["ldap"].as_bool(), v["team"]["url"].as_str()), (Some(true), Some(false), Some(url.as_str())));
        let (_, v) = call("GET", &format!("{url}/api/admin/settings"), Some(&admin), None);
        assert_eq!((v["oidc"]["client_secret_set"].as_bool(), v["oidc"].get("client_secret")), (Some(true), None));
        assert!(!v.to_string().contains("s3cret"));
        // Settings kept, the secret too when none is sent.
        let (code, v) = call("PUT", &format!("{url}/api/admin/settings"), Some(&admin), Some(json!({"name": "Acme", "oidc": {"issuer": issuer, "client_id": "the team", "domains": ["@Example.com"], "make_users": false}, "logo": "data:image/png;base64,iVBORw0KGgo="})));
        assert_eq!(code, 200, "{v}");
        assert_eq!(team.store().oidc.as_ref().map(|o| (o.client_secret.as_str(), o.domains.clone())), Some(("s3cret", vec!["example.com".to_string()])));
        assert_eq!(call("PUT", &format!("{url}/api/admin/settings"), Some(&admin), Some(json!({"logo": "data:text/html;base64,PGI+"}))).0, 400);
        assert_eq!(call("GET", &format!("{url}/api/me"), Some(&admin), None).1["team"]["name"].as_str(), Some("Acme"));
    }

    #[test]
    fn the_audit_log_reads_as_csv() {
        let events = [
            json!({"time": 1_700_000_000u64, "device": "123456789", "device_name": "Desk, front", "kind": "in", "how": "team", "user": "=cmd()", "from": "555666777", "rights": 5, "secs": 60, "words": "said \"hi\"\nthen left"}),
            json!({"time": 0, "kind": "join"}),
        ];
        let csv = audit_csv(&events);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], "time,device,device_name,kind,how,user,from,rights,secs,words");
        assert_eq!(lines[1], "\"2023-11-14 22:13:20\",\"123456789\",\"Desk, front\",\"in\",\"team\",\"'=cmd()\",\"555666777\",\"5\",\"60\",\"said \"\"hi\"\"\nthen left\"");
        assert_eq!(lines[2], "\"1970-01-01 00:00:00\",\"\",\"\",\"join\",\"\",\"\",\"\",\"\",\"\",\"\"");
        assert_eq!(lines[3], "");
        assert_eq!(when("2023-11-14", false), Some(1_699_920_000));
        assert_eq!(when("2023-11-14", true), Some(1_699_920_000 + 86_399));
        assert_eq!(when("2023-11-14T22:13", false), Some(1_700_000_000 - 20));
        assert_eq!(when("1700000000000", false), Some(1_700_000_000));
        assert_eq!(when("yesterday", false), None);
    }

    #[test]
    fn installers_join_with_a_token_they_check() {
        let token = "ab".repeat(32);
        let sh = install_sh("https://team.example.com", &token);
        assert!(sh.contains(&format!("\"$bin\" team join 'https://team.example.com' {token}\n")) && sh.contains("remote-access install"));
        let f = temp_dir("sh");
        std::fs::write(&f, &sh).unwrap();
        assert!(std::process::Command::new("sh").arg("-n").arg(&f).status().unwrap().success());
        let _ = std::fs::remove_file(f);
        let ps1 = install_ps1("https://team.example.com", &token);
        assert!(ps1.contains(&format!("team join 'https://team.example.com' {token}; if")) && ps1.contains("Verb = 'RunAs'"), "{ps1}");
        assert!(url_ok("https://team.example.com/sub") && url_ok("http://127.0.0.1:8023"));
        assert_eq!((sso_path("https://team.example.com"), sso_path("https://example.com/team/")), ("/api/oidc/".to_string(), "/team/api/oidc/".to_string()));
        assert!(!url_ok("https://x'; rm -rf ~") && !url_ok("ftp://x") && !url_ok("https://"));
        assert_eq!(ldap_escape(" a,b+c:d#"), "\\ a\\,b\\+c\\3ad#");
        assert_eq!(ldap_self_url("ldaps://dir.example.com:636/dc=example,dc=com", "uid=o'neil,ou=people,dc=example,dc=com"), "ldaps://dir.example.com:636/uid=o%27neil,ou=people,dc=example,dc=com?cn,displayName,mail?base");
        assert_eq!(ldap_self_url("ldap://dir", "uid=a\\2cb"), "ldap://dir/uid=a%5C2cb?cn,displayName,mail?base");
        assert_eq!(ldap_escape("#x "), "\\#x\\ ");
    }
}
