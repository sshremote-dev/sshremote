//! The app's side of a team: this computer joined to one (its team key, the policy it keeps, the
//! record it sends), and a person signed in to one (their session, the computers they may reach,
//! tickets, their own address book sealed with their passphrase). The computer's team is kept
//! with its remote access settings (`access::set_team`: where only who may change remote access
//! writes, and the service reads); the person's session in `team.json` of their own settings
//! folder, readable by them alone.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::{Policy, Ticket};

/// This computer in a team.
#[derive(Clone, Debug, PartialEq)]
pub struct Joined {
    pub url: String,
    pub id: String,
    pub key: Vec<u8>,
    pub team: Value,
    pub policy: Policy,
    /// When the policy was last read from the team server.
    pub policy_at: u64,
}

/// A person signed in to a team on this device.
#[derive(Clone, Debug, PartialEq)]
pub struct SignedIn {
    pub url: String,
    pub token: String,
    pub user: Value,
    pub team: Value,
    pub expires: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub joined: Option<Joined>,
    pub person: Option<SignedIn>,
}

fn file() -> Option<PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("team.json"))
}

static CACHE: Mutex<Option<(std::time::Instant, State)>> = Mutex::new(None);

#[cfg(test)]
thread_local!(static TEST_STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) });

/// A made-up state in place of what is kept, for tests (the window's pictures): None puts it back.
#[cfg(test)]
pub fn set_test_state(s: Option<State>) {
    TEST_STATE.with(|t| *t.borrow_mut() = s);
}

/// What is kept, read again at most once a second (the service and the window share it).
pub fn state() -> State {
    #[cfg(test)]
    if let Some(s) = TEST_STATE.with(|t| t.borrow().clone()) {
        return s;
    }
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, s)) = c.as_ref()
        && at.elapsed() < std::time::Duration::from_secs(1)
    {
        return s.clone();
    }
    let s = read();
    *c = Some((std::time::Instant::now(), s.clone()));
    s
}

fn joined_of(j: &Value) -> Option<Joined> {
    Some(Joined {
        url: j["url"].as_str()?.to_string(),
        id: j["id"].as_str()?.to_string(),
        key: super::unhex(j["key"].as_str()?).filter(|k| k.len() == 32)?,
        team: j["team"].clone(),
        policy: Policy::from_json(&j["policy"]),
        policy_at: j["policy_at"].as_u64().unwrap_or(0),
    })
}

fn joined_json(j: &Joined) -> Value {
    json!({"url": j.url, "id": j.id, "key": crate::sys::hex(&j.key), "team": j.team, "policy": j.policy.json(), "policy_at": j.policy_at})
}

/// The team as anyone at this computer may see it (in `access::public_state`): no key, no logo.
pub fn public(team: &Value) -> Value {
    match joined_of(team) {
        Some(j) => json!({"url": j.url, "id": j.id, "name": j.team["name"], "policy": j.policy.json(), "policy_at": j.policy_at}),
        None => Value::Null,
    }
}

fn read() -> State {
    let joined = match crate::access::stored() {
        Ok(c) => c.and_then(|c| joined_of(&c.team)),
        // Settings only administrators open (Windows): what the service says of it, without
        // the key (nothing here signs with it then).
        Err(_) => crate::access::state_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| {
            let t = &v["team"];
            Some(Joined {
                url: t["url"].as_str()?.to_string(),
                id: t["id"].as_str()?.to_string(),
                key: Vec::new(),
                team: json!({"name": t["name"], "url": t["url"]}),
                policy: Policy::from_json(&t["policy"]),
                policy_at: t["policy_at"].as_u64().unwrap_or(0),
            })
        }),
    };
    let v = file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or_default();
    let person = v.get("person").filter(|p| p.is_object()).and_then(|p| {
        Some(SignedIn {
            url: p["url"].as_str()?.to_string(),
            token: p["token"].as_str()?.to_string(),
            user: p["user"].clone(),
            team: p["team"].clone(),
            expires: p["expires"].as_u64().unwrap_or(0),
        })
    });
    State { joined, person }
}

fn write(s: &State, was: &State) -> Result<(), String> {
    if s.joined != was.joined {
        crate::access::set_team(s.joined.as_ref().map_or(Value::Null, joined_json))?;
    }
    if s.person != was.person {
        let f = file().ok_or(n!("no settings folder"))?;
        let v = json!({
            "person": s.person.as_ref().map(|p| json!({"url": p.url, "token": p.token, "user": p.user, "team": p.team, "expires": p.expires})),
        });
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        crate::sys::write_whole(&f, v.to_string().as_bytes(), true, true).map_err(|e| format!("{}: {e}", f.display()))?;
    }
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), s.clone()));
    Ok(())
}

fn change(f: impl FnOnce(&mut State)) -> Result<(), String> {
    let was = read();
    let mut s = was.clone();
    f(&mut s);
    write(&s, &was)
}

/// A team server's address as typed: https:// unless said otherwise, no trailing slash.
pub fn tidy_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("https://") || u.starts_with("http://") { u.to_string() } else { format!("https://{u}") }
}

/// One request to a team server through curl (which knows the system's certificates and
/// proxies): the status and the JSON answer.
pub fn http(method: &str, url: &str, headers: &[(String, String)], body: Option<&[u8]>) -> Result<(u16, Value), String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "--max-time", "30", "--proto", "=https,http", "-X", method, "-w", "\n%{http_code}"]);
    if let Some(p) = crate::proxy::current()? {
        for v in ["https_proxy", "HTTPS_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            cmd.env(v, p.url());
        }
        cmd.env_remove("no_proxy").env_remove("NO_PROXY");
    }
    // Headers through a config on stdin: a session token on the command line is seen by `ps`.
    let mut config = String::new();
    for (k, v) in headers {
        if k.contains(['\n', '"']) || v.contains(['\n', '"']) {
            return Err("bad header".into());
        }
        config.push_str(&format!("header = \"{k}: {v}\"\n"));
    }
    config.push_str("header = \"Accept: application/json\"\n");
    let data = match body {
        Some(b) => {
            config.push_str("header = \"Content-Type: application/json\"\n");
            let dir = crate::hosts::config_dir().ok_or(n!("no settings folder"))?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let tmp = dir.join(format!("team-body-{}-{:?}", std::process::id(), std::thread::current().id()));
            crate::sys::write_whole(&tmp, b, true, true).map_err(|e| e.to_string())?;
            config.push_str(&format!("data-binary = \"@{}\"\n", tmp.display().to_string().replace('\\', "/")));
            Some(tmp)
        }
        None => None,
    };
    cmd.args(["-K", "-"]).arg(url).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    crate::sys::quiet(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("curl is needed to reach the team server: {e}"))?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        stdin.write_all(config.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string());
    if let Some(t) = data {
        let _ = std::fs::remove_file(t);
    }
    let out = out?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if code == 0 {
        return Err(crate::lang::fill(n!("the team server did not answer: {}"), &[&String::from_utf8_lossy(&out.stderr).trim()]));
    }
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    Ok((code, v))
}

/// The server's words for a refusal, or its status.
fn refused(code: u16, v: &Value) -> String {
    // The server's words are English; in the app's language when known.
    v["error"].as_str().map(|e| crate::lang::tr_msg(e).into_owned()).unwrap_or_else(|| crate::lang::fill(n!("the team server answered {}"), &[&code]))
}

fn as_person(p: &SignedIn) -> Vec<(String, String)> {
    vec![("Authorization".into(), format!("Bearer {}", p.token))]
}

fn person_call(method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let p = state().person.ok_or(n!("not signed in to a team"))?;
    let body = body.map(|b| b.to_string().into_bytes());
    let (code, v) = http(method, &format!("{}{path}", p.url), &as_person(&p), body.as_deref())?;
    if code == 401 {
        let _ = change(|s| s.person = None);
        return Err(n!("signed out of the team: sign in again").into());
    }
    if !(200..300).contains(&code) {
        return Err(refused(code, &v));
    }
    Ok(v)
}

/// A device's signed request (docs/team.md, Protocol).
/// The headers that sign a device's request with its team key.
fn signed(j: &Joined, method: &str, path: &str, body: &str) -> Vec<(String, String)> {
    let stamp = format!("{}.{}", super::now(), crate::sys::hex(&crate::sys::random(8)));
    let sign = super::request_sign(&j.key, method, path, &stamp, body.as_bytes());
    vec![
        ("X-Team-Device".to_string(), j.id.clone()),
        ("X-Team-Time".to_string(), stamp),
        ("X-Team-Sign".to_string(), sign),
        ("X-Team-Version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
    ]
}

fn device_call(j: &Joined, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let headers = signed(j, method, path, &body);
    let (code, v) = http(method, &format!("{}{path}", j.url), &headers, (!body.is_empty()).then_some(body.as_bytes()))?;
    if !(200..300).contains(&code) {
        return Err(refused(code, &v));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------------------------
// This computer in a team.

/// Joins this computer to the team at `url` with an enrollment token: it is listed there by its
/// remote access ID, which it gets now if it has none.
#[cfg(feature = "remote")]
pub fn join(url: &str, token: &str) -> Result<Value, String> {
    let url = tidy_url(url);
    let mut cfg = crate::access::load();
    if crate::access::stored()?.is_none() {
        crate::access::save(&cfg)?;
    }
    cfg = crate::access::load();
    let name = crate::here::look().name;
    let body = json!({"token": token.trim(), "id": cfg.id, "name": name, "os": std::env::consts::OS, "version": env!("CARGO_PKG_VERSION")}).to_string();
    // In this team already: signed with the key it has, which only this computer holds.
    let headers = match state().joined {
        Some(j) if j.url == url && j.id == cfg.id && j.key.len() == 32 => signed(&j, "POST", "/api/device/enroll", &body),
        _ => Vec::new(),
    };
    let (code, v) = http("POST", &format!("{url}/api/device/enroll"), &headers, Some(body.as_bytes()))?;
    if !(200..300).contains(&code) {
        return Err(refused(code, &v));
    }
    let key = v["key"].as_str().and_then(super::unhex).filter(|k| k.len() == 32).ok_or(n!("the team server gave no key"))?;
    let team = v["team"].clone();
    // The team's own relay, if it has one.
    if let Some(relay) = team["relay"].as_str().filter(|r| !r.trim().is_empty())
        && relay != cfg.relay
    {
        cfg.relay = relay.to_string();
        crate::access::save(&cfg)?;
    }
    change(|s| s.joined = Some(Joined { url: url.clone(), id: cfg.id.clone(), key, team: team.clone(), policy: Policy::default(), policy_at: 0 }))?;
    let _ = refresh();
    Ok(team)
}

/// Leaves the team this computer is in (the server forgets it too, if it can be reached).
pub fn leave() -> Result<(), String> {
    if let Some(j) = state().joined {
        let _ = device_call(&j, "POST", "/api/device/leave", Some(&json!({})));
    }
    change(|s| s.joined = None)
}

/// The policy this computer keeps now (its team's, as last fetched), if it is in a team.
pub fn policy() -> Option<Policy> {
    state().joined.map(|j| j.policy)
}

/// Asks the team server for this computer's policy (and tells it this computer is there), and
/// sends what the record holds. Returns whether the server wants every session ended.
pub fn refresh() -> Result<bool, String> {
    let Some(j) = state().joined else { return Ok(false) };
    let v = device_call(&j, "GET", "/api/device/policy", None)?;
    let policy = Policy::from_json(&v["policy"]);
    let team = if v["team"].is_object() { v["team"].clone() } else { j.team.clone() };
    // Written when something changed, else every few minutes (when it was read, for the window).
    if policy != j.policy || team != j.team || super::now().saturating_sub(j.policy_at) >= 300 {
        change(|s| {
            if let Some(k) = s.joined.as_mut() {
                k.policy = policy.clone();
                k.team = team.clone();
                k.policy_at = super::now();
            }
        })?;
    }
    flush_events(&j);
    Ok(v["end"].as_bool().unwrap_or(false))
}

fn events_file() -> Option<PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("team-events.jsonl"))
}

/// Records an event for the team's audit log: kept here until the server has it.
pub fn report(event: Value) {
    if state().joined.is_none() {
        return;
    }
    let Some(f) = events_file() else { return };
    append(&f, &format!("{event}\n"));
    // Sent soon, not on the session's own thread.
    std::thread::spawn(|| {
        if let Some(j) = state().joined {
            flush_events(&j);
        }
    });
}

static FLUSHING: Mutex<()> = Mutex::new(());

/// Lines put back at the end of the record, its file left to its owner.
fn append(f: &std::path::Path, lines: &str) {
    use std::io::Write;
    if lines.is_empty() {
        return;
    }
    // Who came in from where: for this user (or the service) alone, and opened as itself only
    // (the login screen's copy writes as root in its person's folder).
    if let Ok(mut file) = crate::sys::open_own(f, true) {
        let _ = file.write_all(lines.as_bytes());
    }
    // The login screen's service (root) writes in its person's folder: theirs still.
    crate::access::hand_back(f);
}

fn flush_events(j: &Joined) {
    let Ok(_one) = FLUSHING.try_lock() else { return };
    let Some(f) = events_file() else { return };
    // Taken out whole first: of two services sending (the person's own and the login screen's),
    // one has them, and none is sent twice.
    let taking = f.with_extension(format!("sending-{}", std::process::id()));
    if std::fs::rename(&f, &taking).is_err() {
        return;
    }
    let mut text = String::new();
    if let Ok(mut t) = crate::sys::open_own(&taking, false) {
        use std::io::Read;
        let _ = t.read_to_string(&mut text);
    }
    let _ = std::fs::remove_file(&taking);
    let events: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    // At most a thousand at a time; the rest at the next look, as all of them if the server
    // could not be reached.
    let batch: Vec<Value> = events.iter().take(1000).cloned().collect();
    let sent = !batch.is_empty() && device_call(j, "POST", "/api/device/events", Some(&json!({"events": batch}))).is_ok();
    let keep = if sent { batch.len() } else { 0 };
    append(&f, &events.iter().skip(keep).map(|e| format!("{e}\n")).collect::<String>());
}

/// A ticket as this computer checks it in the handshake: the tag it must be given, and what it
/// lets in.
pub fn check_ticket(ticket: &str, own_id: &str) -> Option<(String, Ticket)> {
    let j = state().joined?;
    // Without its key (settings this process cannot read) nothing is taken.
    if j.id != own_id || j.key.len() != 32 {
        return None;
    }
    let t = super::check_ticket(ticket, own_id).ok()?;
    Some((super::ticket_tag(&j.key, ticket), t))
}

// ---------------------------------------------------------------------------------------------
// A person signed in to a team.

/// Sign-ins under way in the browser, by their nonce: where this app waits for the page to come
/// back (a loopback port), the verifier only it holds, and when it began.
static WAITING: Mutex<Vec<(String, std::net::TcpListener, String, std::time::Instant)>> = Mutex::new(Vec::new());

/// Starts signing in at `url` in the browser: the address to open, and the nonce `poll_sign_in`
/// waits on. The page comes back to this computer alone (`127.0.0.1`), with a code that only the
/// verifier kept here turns into a session (RFC 8252), so a sign-in link someone else made and
/// sent gives them nothing.
pub fn start_sign_in(url: &str) -> (String, String) {
    let url = tidy_url(url);
    let nonce = crate::sys::hex(&crate::sys::random(16));
    let verifier = crate::sys::hex(&crate::sys::random(32));
    let challenge = crate::sys::hex(&<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()));
    let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") else { return (format!("{url}/login"), nonce) };
    let port = listener.local_addr().map_or(0, |a| a.port());
    let _ = listener.set_nonblocking(true);
    let mut w = WAITING.lock().unwrap_or_else(|e| e.into_inner());
    w.retain(|(_, _, _, at)| at.elapsed() < std::time::Duration::from_secs(900));
    w.push((nonce.clone(), listener, verifier, std::time::Instant::now()));
    (format!("{url}/login?app={challenge}&port={port}"), nonce)
}

/// The sign-in that `start_sign_in` began, once the person is in (None while not yet): the
/// browser's page brought to this app's port, its code taken for the session.
pub fn poll_sign_in(url: &str, nonce: &str) -> Result<Option<Value>, String> {
    use std::io::{Read, Write};
    let url = tidy_url(url);
    let (stream, verifier) = {
        let w = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        let Some((_, listener, verifier, _)) = w.iter().find(|(n, ..)| n == nonce) else { return Err(n!("the sign-in was not started here: start it again").into()) };
        match listener.accept() {
            Ok((s, _)) => (s, verifier.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut stream = stream;
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let line = String::from_utf8_lossy(&head).lines().next().unwrap_or("").to_string();
    let code = line.split(' ').nth(1).and_then(|t| t.strip_prefix("/?code=")).map(|c| c.split('&').next().unwrap_or(c).to_string()).filter(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_alphanumeric()));
    // Something else asked (the browser's icon): still waiting.
    let Some(code) = code else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Ok(None);
    };
    WAITING.lock().unwrap_or_else(|e| e.into_inner()).retain(|(n, ..)| n != nonce);
    let mut page = |words: &str| {
        let body = format!("<!doctype html><meta charset=utf-8><title>SSH Remote</title><body style=\"font:16px system-ui,sans-serif;margin:3em;max-width:36em\"><p>{}</p>", words.replace('&', "&amp;").replace('<', "&lt;"));
        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}", body.len());
    };

    let body = json!({"code": code, "verifier": verifier});
    let got = http("POST", &format!("{url}/api/app-login"), &[], Some(body.to_string().as_bytes())).and_then(|(status, v)| if (200..300).contains(&status) { Ok(v) } else { Err(refused(status, &v)) });
    let v = match got {
        Ok(v) => v,
        Err(e) => {
            page(&e);
            return Err(e);
        }
    };
    page(t!("You are signed in. You can close this window and go back to SSH Remote."));
    let token = v["token"].as_str().ok_or(n!("the team server gave no session"))?.to_string();
    let p = SignedIn { url: url.clone(), token, user: v["user"].clone(), team: Value::Null, expires: v["expires"].as_u64().unwrap_or(0) };
    change(|s| s.person = Some(p.clone()))?;
    // The team's name and look, for the window.
    if let Ok(me) = person_call("GET", "/api/me", None) {
        let _ = change(|s| {
            if let Some(p) = s.person.as_mut() {
                p.team = me["team"].clone();
                p.user = me["user"].clone();
            }
        });
    }
    Ok(Some(v["user"].clone()))
}

/// Signs in with a name and password (and the authenticator's code, if the account has one),
/// as the command line does.
pub fn sign_in_password(url: &str, name: &str, password: &str, code: Option<&str>) -> Result<Value, String> {
    let url = tidy_url(url);
    let body = json!({"name": name, "password": password, "code": code});
    let (status, v) = http("POST", &format!("{url}/api/login"), &[], Some(body.to_string().as_bytes()))?;
    if !(200..300).contains(&status) {
        return Err(refused(status, &v));
    }
    let token = v["token"].as_str().ok_or(n!("the team server gave no session"))?.to_string();
    change(|s| s.person = Some(SignedIn { url: url.clone(), token, user: v["user"].clone(), team: Value::Null, expires: v["expires"].as_u64().unwrap_or(0) }))?;
    if let Ok(me) = person_call("GET", "/api/me", None) {
        let _ = change(|s| {
            if let Some(p) = s.person.as_mut() {
                p.team = me["team"].clone();
            }
        });
    }
    Ok(v["user"].clone())
}

pub fn sign_out() -> Result<(), String> {
    let _ = person_call("POST", "/api/logout", Some(&json!({})));
    change(|s| s.person = None)
}

/// A computer of the team the person may reach.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub os: String,
    pub group: String,
    pub online: bool,
    pub rights: u8,
    pub ask: bool,
}

pub fn devices() -> Result<Vec<Device>, String> {
    let v = person_call("GET", "/api/devices", None)?;
    Ok(v["devices"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    Some(Device {
                        id: d["id"].as_str()?.to_string(),
                        name: d["name"].as_str().unwrap_or("").to_string(),
                        os: d["os"].as_str().unwrap_or("").to_string(),
                        group: d["group"].as_str().unwrap_or("").to_string(),
                        online: d["online"].as_bool().unwrap_or(false),
                        rights: d["rights"].as_u64().unwrap_or(0) as u8,
                        ask: d["ask"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// What a viewer gives in place of the password to reach `device` with a fresh ticket: the
/// handshake's team mark, the ticket and its tag.
#[cfg(feature = "remote")]
pub fn ticket_password(device: &str) -> Result<String, String> {
    let (_, key) = crate::relaylink::keypair(crate::access::device_key());
    let viewer = crate::access::own_id().unwrap_or_default();
    let v = person_call("POST", "/api/ticket", Some(&json!({"device": device, "key": crate::sys::hex(&key), "viewer": viewer})))?;
    let (ticket, tag) = (v["ticket"].as_str().ok_or(n!("the team server gave no ticket"))?, v["tag"].as_str().ok_or(n!("the team server gave no ticket"))?);
    let mark = crate::net::TEAM_MARK;
    Ok(format!("{mark}{ticket}{mark}{tag}"))
}

// The person's own address book, sealed on the device with a key from their passphrase.

fn book_key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    super::pbkdf2(passphrase.as_bytes(), &[b"sshremote team book\n".as_slice(), salt].concat(), if cfg!(test) { 1000 } else { 210_000 })
}

pub fn seal_book(book: &Value, passphrase: &str, salt: &[u8]) -> Result<String, String> {
    use base64::Engine;
    use chacha20poly1305::aead::{Aead, KeyInit};
    let cipher = chacha20poly1305::ChaCha20Poly1305::new((&book_key(passphrase, salt)).into());
    let nonce = crate::sys::random(12);
    let sealed = cipher.encrypt(nonce.as_slice().into(), book.to_string().as_bytes()).map_err(|_| "seal")?;
    Ok(base64::engine::general_purpose::STANDARD.encode([nonce, sealed].concat()))
}

pub fn open_book(sealed: &str, passphrase: &str, salt: &[u8]) -> Result<Value, String> {
    use base64::Engine;
    use chacha20poly1305::aead::{Aead, KeyInit};
    let b = base64::engine::general_purpose::STANDARD.decode(sealed.trim()).map_err(|_| n!("the address book does not read"))?;
    if b.len() < 12 + 16 {
        return Err(n!("the address book does not read").into());
    }
    let cipher = chacha20poly1305::ChaCha20Poly1305::new((&book_key(passphrase, salt)).into());
    let plain = cipher.decrypt(b[..12].into(), &b[12..]).map_err(|_| n!("wrong address book passphrase"))?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

/// The person's address book from the server, opened with their passphrase (None when there is
/// none yet).
pub fn book_get(passphrase: &str) -> Result<Option<Value>, String> {
    let v = person_call("GET", "/api/book", None)?;
    let salt = v["salt"].as_str().and_then(super::unhex).ok_or(n!("the team server gave no salt"))?;
    match v["book"].as_str() {
        Some(b) => open_book(b, passphrase, &salt).map(Some),
        None => Ok(None),
    }
}

pub fn book_put(book: &Value, passphrase: &str) -> Result<(), String> {
    let v = person_call("GET", "/api/book", None)?;
    let salt = v["salt"].as_str().and_then(super::unhex).ok_or(n!("the team server gave no salt"))?;
    person_call("PUT", "/api/book", Some(&json!({"book": seal_book(book, passphrase, &salt)?}))).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_book_opens_only_with_its_passphrase() {
        let book = json!({"computers": [{"address": "dev@build", "name": "Build"}]});
        let sealed = seal_book(&book, "river stone", b"salt").unwrap();
        assert_eq!(open_book(&sealed, "river stone", b"salt").unwrap(), book);
        assert!(open_book(&sealed, "river stones", b"salt").is_err());
        assert!(open_book(&sealed, "river stone", b"pepper").is_err());
    }

    #[test]
    fn team_addresses_are_tidied() {
        assert_eq!(tidy_url("team.example.com/"), "https://team.example.com");
        assert_eq!(tidy_url("http://127.0.0.1:8023"), "http://127.0.0.1:8023");
    }
}
