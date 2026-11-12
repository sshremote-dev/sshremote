//! The web gateway: `sshremote web` serves a page to browsers and, over a WebSocket, joins it to
//! a computer as the app does: its terminal, screen and files on one session. A computer by its
//! remote access ID needs only its own password (it is reached through the relay); SSH hosts and
//! this computer's own shells need the gateway's password, or a sign-in it gave the browser to
//! remember. Put it behind HTTPS (nginx, Caddy) when it faces the internet.
//!
//! On the socket, text frames carry JSON (sign-in, what to open, the views, files, the end) and
//! binary frames start with a type byte: 1 terminal bytes, 2 a JPEG picture, 3 a download chunk
//! and 4 an upload chunk (both after a u32 LE id).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("../web/index.html");
const XTERM_JS: &str = include_str!("../web/xterm.js");
const XTERM_CSS: &str = include_str!("../web/xterm.css");
const FIT_JS: &str = include_str!("../web/addon-fit.js");
// The page's own script, in a file: the page's policy runs no inline script.
const APP_JS: &str = include_str!("../web/app.js");
const LOGO: &str = include_str!("../site/favicon.svg");

/// The name the browser picks for a shell on the gateway's own computer.
const HERE: &str = "This computer";

struct Gate {
    password: String,
    /// Only computers by ID: no shells or hosts of the gateway's own (a public gateway).
    id_only: bool,
    /// The relay ID connections go through.
    relay: String,
    /// For trying the page: ID connections open an agent on this computer with this capture.
    local: Option<String>,
    /// Failed sign-ins by address: how many, and when the last was.
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    /// Open sockets by address, so one browser (or one attacker) cannot take them all.
    sockets: Mutex<HashMap<IpAddr, usize>>,
}

/// Connections served at once, and sockets from one address: enough for many tabs, not for a flood.
const MOST_CONNECTIONS: usize = 512;
const MOST_SOCKETS_EACH: usize = 32;
/// A request or header line longer than this is no browser's.
const MOST_LINE: u64 = 8192;

static CONNECTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One connection served; counted down however it ends.
struct Served;

impl Drop for Served {
    fn drop(&mut self) {
        CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn password_file() -> Option<PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("web-password"))
}

/// `sshremote web [--listen ADDR] [--password-file FILE] [--id-only] [--relay HOST[:PORT]]`
pub fn run(args: &[String]) -> Result<(), String> {
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let listen = get("--listen").unwrap_or_else(|| "127.0.0.1:8022".into());
    let file = get("--password-file").map(PathBuf::from).or_else(password_file).ok_or("no settings folder")?;
    let password = match std::fs::read_to_string(&file) {
        Ok(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => {
            // A new one, shown once; it stays in the file for later starts.
            let mut b = [0u8; 18];
            getrandom::getrandom(&mut b).map_err(|e| e.to_string())?;
            use base64::Engine;
            let p = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
            if let Some(d) = file.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(&file, &p).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
            }
            eprintln!("sshremote web: the password is {p} (kept in {})", file.display());
            p
        }
    };
    let id_only = args.iter().any(|a| a == "--id-only");
    let relay = get("--relay").unwrap_or_else(|| crate::access::DEFAULT_RELAY.into());
    // Hidden, for testing: never on an address other computers can reach.
    let local = get("--local-agent");
    if local.is_some() && !listen.parse::<std::net::SocketAddr>().is_ok_and(|a| a.ip().is_loopback()) {
        return Err("--local-agent only listens on 127.0.0.1".into());
    }
    let gate = Arc::new(Gate {
        password,
        id_only,
        relay,
        local,
        failures: Mutex::new(HashMap::new()),
        sockets: Mutex::new(HashMap::new()),
    });
    let server = TcpListener::bind(&listen).map_err(|e| format!("{listen}: {e}"))?;
    eprintln!("sshremote web: on http://{listen}/");
    for conn in server.incoming().flatten() {
        // Over the limit the connection is closed at once, before a thread is spent on it.
        if CONNECTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= MOST_CONNECTIONS {
            CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        let served = Served;
        let gate = gate.clone();
        std::thread::spawn(move || {
            let _served = served;
            let _ = conn.set_read_timeout(Some(Duration::from_secs(30)));
            if let Err(e) = serve(conn, &gate) {
                eprintln!("web: {e}");
            }
        });
    }
    Ok(())
}

struct Request {
    path: String,
    headers: HashMap<String, String>,
}

/// One line of the request, at most `MOST_LINE` long.
fn read_line(r: &mut BufReader<TcpStream>, line: &mut String) -> Result<(), String> {
    line.clear();
    let n = r.by_ref().take(MOST_LINE).read_line(line).map_err(|e| e.to_string())?;
    if n as u64 == MOST_LINE && !line.ends_with('\n') {
        return Err("request line too long".into());
    }
    Ok(())
}

fn read_request(r: &mut BufReader<TcpStream>) -> Result<Request, String> {
    let mut line = String::new();
    read_line(r, &mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/").to_string());
    if method != "GET" {
        return Err("only GET".into());
    }
    let mut headers = HashMap::new();
    for _ in 0..100 {
        read_line(r, &mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            return Ok(Request { path, headers });
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Err("too many headers".into())
}

fn respond(mut s: &TcpStream, status: &str, kind: &str, body: &[u8]) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:\r\n\
         Referrer-Policy: no-referrer\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).and_then(|_| s.write_all(body)).map_err(|e| e.to_string())
}

fn serve(conn: TcpStream, gate: &Gate) -> Result<(), String> {
    let peer = conn.peer_addr().map_err(|e| e.to_string())?.ip();
    let mut r = BufReader::new(conn.try_clone().map_err(|e| e.to_string())?);
    let req = read_request(&mut r)?;
    // Behind a proxy on this computer the browser's address comes in its header.
    let who = if peer.is_loopback() {
        req.headers.get("x-real-ip").and_then(|v| v.parse().ok()).unwrap_or(peer)
    } else {
        peer
    };
    // Served at the root or under /web, as the proxy places it.
    let path = req.path.split('?').next().unwrap_or("/");
    let path = path.strip_prefix("/web").unwrap_or(path);
    match path {
        "" | "/" | "/index.html" => respond(&conn, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes()),
        "/app.js" => respond(&conn, "200 OK", "text/javascript", APP_JS.as_bytes()),
        "/xterm.js" => respond(&conn, "200 OK", "text/javascript", XTERM_JS.as_bytes()),
        "/addon-fit.js" => respond(&conn, "200 OK", "text/javascript", FIT_JS.as_bytes()),
        "/favicon.svg" => respond(&conn, "200 OK", "image/svg+xml", LOGO.as_bytes()),
        "/xterm.css" => respond(&conn, "200 OK", "text/css", XTERM_CSS.as_bytes()),
        "/ws" => {
            let key = req.headers.get("sec-websocket-key").ok_or("not a WebSocket")?;
            // Another site's page may not use a signed-in browser's socket.
            if let (Some(origin), Some(host)) = (req.headers.get("origin"), req.headers.get("host"))
                && origin.split("://").nth(1) != Some(host.as_str())
            {
                return respond(&conn, "403 Forbidden", "text/plain", b"wrong origin");
            }
            // A socket is held for as long as the page is open: a few per address.
            let mut sockets = gate.sockets.lock().map_err(|e| e.to_string())?;
            let open = sockets.entry(who).or_insert(0);
            if *open >= MOST_SOCKETS_EACH {
                drop(sockets);
                return respond(&conn, "429 Too Many Requests", "text/plain", b"too many connections");
            }
            *open += 1;
            drop(sockets);
            struct Held<'a>(&'a Gate, IpAddr);
            impl Drop for Held<'_> {
                fn drop(&mut self) {
                    if let Ok(mut m) = self.0.sockets.lock()
                        && let Some(n) = m.get_mut(&self.1)
                    {
                        *n -= 1;
                        if *n == 0 {
                            m.remove(&self.1);
                        }
                    }
                }
            }
            let _held = Held(gate, who);
            let accept = {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .encode(sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()))
            };
            let mut s = &conn;
            s.write_all(
                format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            // The sign-in must come within the 30 s; session lifts the limit after it.
            session(r, conn.try_clone().map_err(|e| e.to_string())?, gate, who)
        }
        _ => respond(&conn, "404 Not Found", "text/plain", b"not here"),
    }
}

// ----- WebSocket frames -----------------------------------------------------------------------------

enum Frame {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

fn read_frame(r: &mut impl Read, w: &Mutex<TcpStream>) -> Result<Frame, String> {
    let mut message = Vec::new();
    let mut kind = 0u8;
    loop {
        let mut h = [0u8; 2];
        r.read_exact(&mut h).map_err(|e| e.to_string())?;
        let (fin, op, masked) = (h[0] & 0x80 != 0, h[0] & 0x0f, h[1] & 0x80 != 0);
        let mut len = (h[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            r.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            r.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u64::from_be_bytes(b);
        }
        if len > 1 << 20 || message.len() as u64 + len > 4 << 20 {
            return Err("message too large".into());
        }
        if !masked {
            return Err("unmasked frame from a browser".into());
        }
        if op >= 8 && (len > 125 || !fin) {
            return Err("bad control frame".into());
        }
        if (op == 0) != (kind != 0) && op < 8 {
            return Err("unexpected continuation".into());
        }
        let mut mask = [0u8; 4];
        r.read_exact(&mut mask).map_err(|e| e.to_string())?;
        let mut data = vec![0u8; len as usize];
        r.read_exact(&mut data).map_err(|e| e.to_string())?;
        for (i, b) in data.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        match op {
            0x8 => return Ok(Frame::Close),
            0x9 => send(w, 0xA, &data)?,
            0xA => {}
            0x0 | 0x1 | 0x2 => {
                if op != 0 {
                    kind = op;
                }
                message.extend_from_slice(&data);
                if fin {
                    return Ok(if kind == 1 {
                        Frame::Text(String::from_utf8_lossy(&message).into_owned())
                    } else {
                        Frame::Binary(message)
                    });
                }
            }
            _ => return Err("unknown frame".into()),
        }
    }
}

fn send(w: &Mutex<TcpStream>, op: u8, data: &[u8]) -> Result<(), String> {
    let mut head = vec![0x80 | op];
    match data.len() {
        n if n < 126 => head.push(n as u8),
        n if n < 65536 => {
            head.push(126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    let mut s = w.lock().map_err(|e| e.to_string())?;
    s.write_all(&head).and_then(|_| s.write_all(data)).map_err(|e| e.to_string())
}

fn send_json(w: &Mutex<TcpStream>, v: serde_json::Value) -> Result<(), String> {
    send(w, 0x1, v.to_string().as_bytes())
}

// ----- A session -------------------------------------------------------------------------------------

/// The hosts a signed-in browser is offered: this computer, and the hosts of its ~/.ssh/config.
fn hosts() -> Vec<String> {
    let mut out = vec![HERE.to_string()];
    out.extend(crate::hosts::ssh_config_hosts());
    out.dedup();
    out
}

fn equal(a: &str, b: &str) -> bool {
    use sha2::Digest;
    let (x, y) = (sha2::Sha256::digest(a.as_bytes()), sha2::Sha256::digest(b.as_bytes()));
    x.iter().zip(y.iter()).fold(0u8, |acc, (p, q)| acc | (p ^ q)) == 0
}

/// How long a sign-in a browser remembers lasts.
const TOKEN_DAYS: u64 = 30;

/// A remembered sign-in: random bytes, when it ends, and a MAC over both keyed by the gateway's
/// password. Nothing is stored here, and a new password ends every one of them.
fn token_new(password: &str) -> String {
    use base64::Engine;
    use hmac::Mac;
    let mut r = [0u8; 16];
    let _ = getrandom::getrandom(&mut r);
    let until = crate::sys::now_secs() + TOKEN_DAYS * 86_400;
    let mut body = r.to_vec();
    body.extend_from_slice(&until.to_le_bytes());
    let mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(password.as_bytes()).expect("any key length").chain_update(&body).finalize().into_bytes();
    body.extend_from_slice(&mac[..16]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body)
}

fn token_ok(password: &str, token: &str) -> bool {
    use base64::Engine;
    use hmac::Mac;
    let Ok(b) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token.trim()) else { return false };
    if b.len() != 40 {
        return false;
    }
    let until = u64::from_le_bytes(b[16..24].try_into().expect("8 bytes"));
    let mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(password.as_bytes()).expect("any key length").chain_update(&b[..24]);
    until > crate::sys::now_secs() && mac.verify_truncated_left(&b[24..]).is_ok()
}

/// What a browser opens: its name, the agent for the screen and files (reached at once for an
/// ID, when first wanted otherwise), and where its shell runs.
struct Open {
    name: String,
    agent: crate::transport::Target,
    lazy: bool,
    /// A program in a terminal here (`ssh HOST`, or this computer's shell, when empty) with what
    /// to add to its environment; None: the agent's own shell.
    shell: Option<(Vec<String>, Vec<(String, String)>)>,
}

/// An SSH host typed in the browser (`user@host`, a port, a password) or one of the gateway's
/// own: reached from this computer, its shell by `ssh` here and its screen and files by the
/// agent over ssh.
fn ssh_open(host: &str, port: Option<u16>, password: Option<String>) -> Open {
    let mut argv = vec!["ssh".to_string(), "-o".into(), "StrictHostKeyChecking=accept-new".into()];
    if let Some(p) = port {
        argv.extend(["-p".into(), p.to_string()]);
    }
    let mut env = Vec::new();
    if let Some(pw) = &password {
        // ssh asks this program for the password; a wrong one is asked once, not three times.
        argv.extend(["-o".into(), "NumberOfPasswordPrompts=1".into()]);
        env = crate::vault::askpass_env(host, port, pw);
    }
    argv.push(host.to_string());
    let agent = crate::transport::Target::Ssh {
        host: host.to_string(),
        port,
        args: Vec::new(),
        remote: crate::options::REMOTE.into(),
        capture: "auto".into(),
        password,
        unattended: true,
    };
    Open { name: host.to_string(), agent, lazy: true, shell: Some((argv, env)) }
}

fn session(mut r: BufReader<TcpStream>, w: TcpStream, gate: &Gate, who: IpAddr) -> Result<(), String> {
    let w = Arc::new(Mutex::new(w));
    // Signing in: five wrong tries from one address wait a quarter hour; each wrong one waits too.
    let Frame::Text(first) = read_frame(&mut r, &w)? else { return Err("expected a sign-in".into()) };
    // Signed in or not, it is a page now: it may stay quiet as long as it is open. On both
    // handles: on Windows a clone keeps its own timeout.
    let _ = r.get_ref().set_read_timeout(None);
    if let Ok(w) = w.lock() {
        let _ = w.set_read_timeout(None);
    }
    let v: serde_json::Value = serde_json::from_str(&first).unwrap_or_default();
    let blocked = gate
        .failures
        .lock()
        .map_err(|e| e.to_string())?
        .get(&who)
        .is_some_and(|(n, at)| *n >= 5 && at.elapsed() < Duration::from_secs(900));
    let failed = || -> Result<(), String> {
        std::thread::sleep(Duration::from_secs(2));
        let mut f = gate.failures.lock().map_err(|e| e.to_string())?;
        // Addresses quiet for a quarter of an hour are forgotten, so the list stays short.
        f.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(900));
        let e = f.entry(who).or_insert((0, Instant::now()));
        *e = (if e.1.elapsed() > Duration::from_secs(900) { 1 } else { e.0 + 1 }, Instant::now());
        Ok(())
    };
    let size = |k: &str, d: u64, lo: u64, hi: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d).clamp(lo, hi) as u16;
    let view = (size("w", 1280, 64, 7680), size("h", 720, 64, 4320));
    let term = (size("cols", 80, 2, 500), size("rows", 24, 1, 500));
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    // A computer by its remote access ID: its own password is the sign-in, checked by the computer.
    if v.get("id").is_some() {
        let id: String = text("id").chars().filter(|c| c.is_ascii_digit()).collect();
        if blocked {
            return send_json(&w, serde_json::json!({ "error": "Too many failed tries; try again later." }));
        }
        if !crate::access::valid_id(&id) {
            return send_json(&w, serde_json::json!({ "error": "An ID is 9 digits." }));
        }
        let agent = match &gate.local {
            Some(capture) => crate::transport::Target::Local { capture: capture.clone() },
            None => {
                let addr = crate::net::Address { id: id.clone(), relay: gate.relay.clone() };
                crate::transport::Target::Direct { addr, password: text("password") }
            }
        };
        let open = Open { name: format!("ID {}", crate::access::spaced(&id)), agent, lazy: false, shell: None };
        // Only a password the computer refused counts against this address: an offline
        // computer or a relay that is down is nobody's guess.
        match client(r, w, open, view, term)? {
            Opened::Yes => drop(gate.failures.lock().map_err(|e| e.to_string())?.remove(&who)),
            Opened::WrongPassword => failed()?,
            Opened::No => {}
        }
        return Ok(());
    }
    // Everything else runs from this computer, for whoever holds the gateway's password (or a
    // sign-in it gave): its own shells and hosts, and SSH to any host.
    if gate.id_only {
        return send_json(&w, serde_json::json!({ "error": "This gateway opens computers by ID only." }));
    }
    let signed = v.get("gateway").cloned().unwrap_or_default();
    let field = |k: &str| signed.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let ok = !blocked && (equal(&field("password"), &gate.password) || (!field("token").is_empty() && token_ok(&gate.password, &field("token"))));
    if !ok {
        if !blocked {
            failed()?;
        }
        let text = if blocked {
            "Too many wrong passwords; try again later."
        } else if field("password").is_empty() {
            "Signed out: sign in to this gateway again."
        } else {
            "Wrong password for this gateway."
        };
        return send_json(&w, serde_json::json!({ "error": text, "signin": true }));
    }
    gate.failures.lock().map_err(|e| e.to_string())?.remove(&who);
    let list = hosts();
    // Only signing in: the hosts, and a sign-in to remember when asked for.
    if v.get("open").is_none() {
        let token = (signed.get("remember").and_then(|x| x.as_bool()) == Some(true)).then(|| token_new(&gate.password));
        return send_json(&w, serde_json::json!({ "signedIn": { "hosts": list, "token": token, "days": TOKEN_DAYS } }));
    }
    let o = v.get("open").cloned().unwrap_or_default();
    let host = o.get("host").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let open = if host == HERE {
        Open {
            name: HERE.into(),
            agent: crate::transport::Target::Local { capture: gate.local.clone().unwrap_or_else(|| "auto".into()) },
            lazy: true,
            shell: Some((Vec::new(), Vec::new())),
        }
    } else {
        // Anything ssh takes as a destination, never an option.
        if host.is_empty() || host.starts_with('-') || host.contains(char::is_whitespace) {
            return send_json(&w, serde_json::json!({ "error": "Enter a host, for example user@192.168.1.20" }));
        }
        let port = o.get("port").and_then(|x| x.as_u64()).filter(|p| (1..=65535).contains(p)).map(|p| p as u16);
        let password = o.get("password").and_then(|x| x.as_str()).filter(|p| !p.is_empty()).map(str::to_string);
        ssh_open(&host, port, password)
    };
    client(r, w, open, view, term).map(|_| ())
}

/// What the browser sends while it shows a screen.
enum Input {
    Agent(crate::proto::ToAgent),
    View(u16, u16),
}

/// A download or an upload as large as a browser is let to take (it holds a download whole).
const MAX_TRANSFER: u64 = 2 << 30;

/// What the browser sends to a computer it opened.
enum Ask {
    Agent(crate::proto::ToAgent),
    View(u16, u16),
    /// Which view shows: the picture is only made while it is the screen; the files want the
    /// agent too.
    Show(String),
    /// A new shell, after the last one ended.
    Shell(u16, u16),
    Upload { id: u32, size: u64, dir: String, name: String },
    Chunk(u32, Vec<u8>),
    UploadEnd(u32),
    /// How the picture is sent: JPEG quality and the least time between pictures.
    Quality(u8, u64),
    Gone,
}

/// One message of a browser's session: JSON in text frames, typed bytes in binary ones.
fn ask(frame: Frame) -> Option<Ask> {
    use crate::proto::{FileOp, ToAgent};
    let v: serde_json::Value = match frame {
        Frame::Close => return Some(Ask::Gone),
        Frame::Binary(b) => {
            return match b.split_first() {
                Some((1, data)) => Some(Ask::Agent(ToAgent::TermInput { id: 0, data: data.to_vec() })),
                Some((4, rest)) if rest.len() >= 4 => {
                    Some(Ask::Chunk(u32::from_le_bytes(rest[..4].try_into().ok()?), rest[4..].to_vec()))
                }
                _ => None,
            };
        }
        Frame::Text(t) => serde_json::from_str(&t).ok()?,
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64());
    let pair = |k: &str| -> Option<(u64, u64)> {
        let a = v.get(k)?.as_array()?;
        Some((a.first()?.as_u64()?, a.get(1)?.as_u64()?))
    };
    if let Some(view) = s("show") {
        return Some(Ask::Show(view));
    }
    if let Some((c, r)) = pair("resize") {
        return Some(Ask::Agent(ToAgent::TermResize { id: 0, cols: c.clamp(2, 500) as u16, rows: r.clamp(1, 500) as u16 }));
    }
    if let Some((c, r)) = pair("shell") {
        return Some(Ask::Shell(c.clamp(2, 500) as u16, r.clamp(1, 500) as u16));
    }
    if let Some(path) = s("list") {
        return Some(Ask::Agent(ToAgent::ListDir { path }));
    }
    if let Some(path) = s("download") {
        let tag = n("tag").unwrap_or(0) as u32;
        return Some(Ask::Agent(ToAgent::Fetch { tag, path }));
    }
    if let Some(id) = n("cancel") {
        let tag = n("tag").unwrap_or(0) as u32;
        return Some(Ask::Agent(ToAgent::FetchCancel { id: id as u32, tag }));
    }
    if let Some(a) = v.get("op").and_then(|x| x.as_array()) {
        let text = |i: usize| a.get(i).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let op = match text(0).as_str() {
            "mkdir" => FileOp::Mkdir,
            "rename" => FileOp::Rename,
            "delete" => FileOp::Delete,
            _ => return None,
        };
        return Some(Ask::Agent(ToAgent::FileOp { op, path: text(1), arg: text(2) }));
    }
    if let Some(t) = s("clipboard") {
        return Some(Ask::Agent(ToAgent::Clipboard(t)));
    }
    if v.get("sas").is_some() {
        return Some(Ask::Agent(ToAgent::SecureAttention));
    }
    if let Some(m) = n("monitor") {
        return Some(Ask::Agent(ToAgent::SelectMonitor(m.min(255) as u8)));
    }
    if v.get("refresh").is_some() {
        return Some(Ask::Agent(ToAgent::Refresh));
    }
    if let Some(q) = s("quality") {
        return Some(match q.as_str() {
            "best" => Ask::Quality(90, 60),
            "fast" => Ask::Quality(60, 150),
            _ => Ask::Quality(80, 80),
        });
    }
    if let Some(u) = v.get("upload") {
        let id = u.get("id")?.as_u64()? as u32;
        let size = u.get("size")?.as_u64()?;
        let text = |k: &str| u.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        return Some(Ask::Upload { id, size, dir: text("dir"), name: text("name") });
    }
    if let Some(id) = n("uploadEnd") {
        return Some(Ask::UploadEnd(id as u32));
    }
    match browser_input(&v)? {
        Input::Agent(a) => Some(Ask::Agent(a)),
        Input::View(w, h) => Some(Ask::View(w, h)),
    }
}

/// What the picture is asked to be: the browser's view, all colours, 15 pictures a second at most.
fn settings((vw, vh): (u16, u16)) -> crate::proto::Settings {
    crate::proto::Settings { view_w: vw, view_h: vh, scale: 100, mode: crate::color::Mode::True, fps: 15 }
}

/// Reaches the agent, with no picture until the screen shows.
fn reach(target: crate::transport::Target, view: (u16, u16)) -> Result<(crate::session::Session, mpsc::Receiver<crate::session::Ev>), String> {
    use crate::proto::ToAgent;
    let (mut sess, _tx, rx) = crate::session::Session::open(&target, true)?;
    sess.send(ToAgent::Pause(true)).map_err(|e| e.to_string())?;
    sess.send(ToAgent::Settings(settings(view))).map_err(|e| e.to_string())?;
    Ok((sess, rx))
}

/// How opening a computer went, for the count of wrong tries.
enum Opened {
    Yes,
    WrongPassword,
    No,
}

/// The agent of a session, or where it stands.
enum Agent {
    Up(crate::session::Session, mpsc::Receiver<crate::session::Ev>),
    Connecting(mpsc::Receiver<Result<(crate::session::Session, mpsc::Receiver<crate::session::Ev>), String>>),
    /// Not wanted yet (an SSH host's screen and files are reached when first shown).
    Idle,
    Failed(String),
}

/// A computer in the browser as in the app: a shell first, its screen (made only while shown) and
/// its files. Says whether it was reached, refused the password, or could not be reached.
fn client(
    mut r: BufReader<TcpStream>,
    w: Arc<Mutex<TcpStream>>,
    open: Open,
    view: (u16, u16),
    (cols, rows): (u16, u16),
) -> Result<Opened, String> {
    use crate::proto::{ToAgent, ToClient};
    use crate::session::Ev;
    let io = |e: std::io::Error| e.to_string();
    let _ = send_json(&w, serde_json::json!({ "status": format!("Connecting to {}\u{2026}", open.name) }));
    let start = reach;
    let mut agent = if open.lazy {
        Agent::Idle
    } else {
        match start(open.agent.clone(), view) {
            Ok((s, rx)) => Agent::Up(s, rx),
            Err(e) => {
                let refused = e.to_lowercase().contains("wrong password");
                send_json(&w, serde_json::json!({ "error": plain(&e) }))?;
                return Ok(if refused { Opened::WrongPassword } else { Opened::No });
            }
        }
    };
    // The shell: the agent's, or a program in a terminal here.
    let mut shell = 1u32;
    let mut pty: Option<crate::pty::Local> = None;
    let spawn = |cols: u16, rows: u16| -> Result<crate::pty::Local, String> {
        let (argv, env) = open.shell.as_ref().expect("a shell here");
        crate::pty::Local::spawn_with(argv, env, cols, rows, Box::new(|| {}))
    };
    match (&open.shell, &mut agent) {
        (Some(_), _) => match spawn(cols, rows) {
            Ok(p) => pty = Some(p),
            Err(e) => return send_json(&w, serde_json::json!({ "error": e })).map(|_| Opened::No),
        },
        (None, Agent::Up(sess, _)) => sess.send(ToAgent::TermOpen { id: shell, cols, rows }).map_err(io)?,
        (None, _) => {}
    }
    let desk = match &agent {
        Agent::Up(sess, _) => serde_json::json!([sess.desk.0, sess.desk.1]),
        _ => serde_json::Value::Null,
    };
    send_json(&w, serde_json::json!({ "ready": { "host": open.name, "screen": desk } }))?;
    // The browser's messages on their own thread; a bounded queue so a fast sender waits.
    let (in_tx, input) = mpsc::sync_channel(32);
    let w_in = w.clone();
    std::thread::spawn(move || {
        loop {
            let msg = match read_frame(&mut r, &w_in) {
                Ok(f) => ask(f),
                Err(_) => Some(Ask::Gone),
            };
            if let Some(m) = msg {
                let gone = matches!(m, Ask::Gone);
                if in_tx.send(m).is_err() || gone {
                    return;
                }
            }
        }
    });
    let mut fb = crate::session::Framebuffer::default();
    let (mut dirty, mut sent, mut shown) = (false, Instant::now(), false);
    let (mut jpeg_q, mut gap) = (80u8, Duration::from_millis(80));
    let mut view = view;
    // Asks waiting for the agent to be reached.
    let mut waiting: Vec<ToAgent> = Vec::new();
    // Downloads under way: bytes passed on to the browser and bytes acknowledged to the agent.
    let mut downloads: HashMap<u32, (u64, u64)> = HashMap::new();
    // Uploads under way: declared size and bytes passed on.
    let mut uploads: HashMap<u32, (u64, u64)> = HashMap::new();
    let end = 'run: loop {
        // The agent's messages, or a short wait when there is no agent to hear from.
        let mut msgs = Vec::new();
        match &mut agent {
            Agent::Up(_, rx) => match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(m) => {
                    msgs.push(m);
                    msgs.extend(rx.try_iter().take(256));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => msgs.push(Ev::Closed("the session ended".into())),
            },
            Agent::Connecting(c) => match c.try_recv() {
                Ok(Ok((mut sess, rx))) => {
                    // Reached: what was asked meanwhile goes now.
                    let _ = send_json(&w, serde_json::json!({ "agent": { "screen": [sess.desk.0, sess.desk.1] } }));
                    if shown {
                        let _ = sess.send(ToAgent::Pause(false));
                    }
                    for m in waiting.drain(..) {
                        sess.send(m).map_err(io)?;
                    }
                    agent = Agent::Up(sess, rx);
                }
                Ok(Err(e)) => {
                    let text = plain(&e);
                    let _ = send_json(&w, serde_json::json!({ "agentError": text }));
                    waiting.clear();
                    agent = Agent::Failed(text);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            },
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
        for m in msgs {
            let m = match m {
                Ev::Net(m) => m,
                Ev::Closed(e) => {
                    // An ID's agent is the whole session; another's only its screen and files.
                    if open.shell.is_none() {
                        break 'run plain(&e);
                    }
                    let text = plain(&e);
                    let _ = send_json(&w, serde_json::json!({ "agentError": text }));
                    agent = Agent::Failed(text);
                    break;
                }
                #[allow(unreachable_patterns)]
                _ => continue,
            };
            let Agent::Up(sess, _) = &mut agent else { break };
            match m {
                ToClient::Frame(f) => {
                    fb.apply(&f).map_err(io)?;
                    sess.send(ToAgent::Ack(f.id)).map_err(io)?;
                    if std::mem::take(&mut fb.need_key) {
                        sess.send(ToAgent::Refresh).map_err(io)?;
                    }
                    dirty = true;
                }
                ToClient::Hello { w: dw, h: dh, .. } => {
                    let _ = send_json(&w, serde_json::json!({ "screen": [dw, dh] }));
                }
                ToClient::Error(e) => break 'run plain(&e),
                ToClient::TermOutput { id, data } if id == shell && pty.is_none() => {
                    if send(&w, 0x2, &[&[1u8][..], &data].concat()).is_err() {
                        break 'run String::new();
                    }
                }
                ToClient::TermExit { id, text } if id == shell && pty.is_none() => {
                    let _ = send_json(&w, serde_json::json!({ "exit": text }));
                }
                ToClient::DirList { path, entries, error } => {
                    let list: Vec<_> = entries
                        .iter()
                        .map(|e| serde_json::json!({ "name": e.name, "dir": e.dir, "size": e.size, "mtime": e.mtime }))
                        .collect();
                    let _ = send_json(&w, serde_json::json!({ "dir": { "path": path, "entries": list, "error": error } }));
                }
                ToClient::OpDone { ok, text } => {
                    let _ = send_json(&w, serde_json::json!({ "opDone": { "ok": ok, "text": text } }));
                }
                ToClient::FetchStart { id, size, tag, name } => {
                    if size > MAX_TRANSFER {
                        sess.send(ToAgent::FetchCancel { id, tag: 0 }).map_err(io)?;
                        let text = "Larger than 2 GB: too large for a browser download.";
                        let _ = send_json(&w, serde_json::json!({ "fetched": { "id": id, "ok": false, "name": name, "text": text, "tag": tag } }));
                    } else {
                        downloads.insert(id, (0, 0));
                        let _ = send_json(&w, serde_json::json!({ "fetch": { "id": id, "size": size, "name": name, "tag": tag } }));
                    }
                }
                ToClient::FetchData { id, data } => {
                    let Some(d) = downloads.get_mut(&id) else { continue };
                    // Passed on before acknowledged: the browser's pace limits the agent's.
                    if send(&w, 0x2, &[&[3u8][..], &id.to_le_bytes(), &data].concat()).is_err() {
                        break 'run String::new();
                    }
                    d.0 += data.len() as u64;
                    if d.0 - d.1 >= crate::files::REPORT {
                        d.1 = d.0;
                        sess.send(ToAgent::FetchAck { id, received: d.0 }).map_err(io)?;
                    }
                }
                ToClient::FetchEnd { id, ok, text } => {
                    downloads.remove(&id);
                    let _ = send_json(&w, serde_json::json!({ "fetched": { "id": id, "ok": ok, "text": text } }));
                }
                ToClient::FetchDone { tag } => {
                    let _ = send_json(&w, serde_json::json!({ "fetchDone": tag }));
                }
                ToClient::FileProgress { id, received } => {
                    let _ = send_json(&w, serde_json::json!({ "progress": [id, received] }));
                }
                ToClient::FileDone { id, ok, text } => {
                    uploads.remove(&id);
                    let _ = send_json(&w, serde_json::json!({ "uploaded": { "id": id, "ok": ok, "text": text } }));
                }
                ToClient::Clipboard(t) => {
                    let _ = send_json(&w, serde_json::json!({ "clipboard": t }));
                }
                ToClient::Monitors { list, current } => {
                    let list: Vec<_> = list.iter().map(|m| serde_json::json!({ "name": m.name, "w": m.w, "h": m.h, "primary": m.primary })).collect();
                    let _ = send_json(&w, serde_json::json!({ "monitors": { "list": list, "current": current } }));
                }
                ToClient::Notice(t) => {
                    let _ = send_json(&w, serde_json::json!({ "notice": t }));
                }
                _ => {}
            }
        }
        // The shell here: its output, and its end.
        if let Some(p) = pty.as_mut() {
            let (data, ended) = p.drain();
            if !data.is_empty() && send(&w, 0x2, &[&[1u8][..], &data].concat()).is_err() {
                break 'run String::new();
            }
            if let Some(text) = ended {
                pty = None;
                let _ = send_json(&w, serde_json::json!({ "exit": text }));
            }
        }
        while let Ok(m) = input.try_recv() {
            let to_agent: Option<ToAgent> = match m {
                Ask::Agent(ToAgent::TermInput { data, .. }) => match pty.as_mut() {
                    Some(p) => {
                        p.write(&data);
                        None
                    }
                    None if open.shell.is_some() => None,
                    None => Some(ToAgent::TermInput { id: shell, data }),
                },
                Ask::Agent(ToAgent::TermResize { cols, rows, .. }) => match pty.as_ref() {
                    Some(p) => {
                        p.resize(cols, rows);
                        None
                    }
                    None if open.shell.is_some() => None,
                    None => Some(ToAgent::TermResize { id: shell, cols, rows }),
                },
                Ask::Shell(c, r) => {
                    if open.shell.is_some() {
                        match spawn(c, r) {
                            Ok(p) => pty = Some(p),
                            Err(e) => drop(send_json(&w, serde_json::json!({ "exit": e }))),
                        }
                        None
                    } else {
                        if let Agent::Up(sess, _) = &mut agent {
                            let _ = sess.send(ToAgent::TermClose { id: shell });
                        }
                        shell += 1;
                        Some(ToAgent::TermOpen { id: shell, cols: c, rows: r })
                    }
                }
                // Pointer and keys reach the screen only while it shows.
                Ask::Agent(
                    ToAgent::Pointer { .. }
                    | ToAgent::Button { .. }
                    | ToAgent::Scroll { .. }
                    | ToAgent::HScroll { .. }
                    | ToAgent::KeyState { .. },
                ) if !shown => None,
                Ask::Agent(a) => Some(a),
                Ask::View(vw, vh) => {
                    view = (vw, vh);
                    Some(ToAgent::Settings(settings(view)))
                }
                Ask::Quality(q, ms) => {
                    (jpeg_q, gap, dirty) = (q, Duration::from_millis(ms), fb.w > 0);
                    None
                }
                Ask::Show(v) => {
                    let on = v == "screen";
                    // The screen or the files: an agent not reached yet is reached now.
                    if v != "terminal" && matches!(agent, Agent::Idle) {
                        let _ = send_json(&w, serde_json::json!({ "status": "Connecting for the screen and files\u{2026}" }));
                        let (tx, rx) = mpsc::channel();
                        let (target, at) = (open.agent.clone(), view);
                        std::thread::spawn(move || drop(tx.send(start(target, at))));
                        agent = Agent::Connecting(rx);
                    }
                    if on == shown {
                        None
                    } else {
                        (shown, dirty) = (on, on && fb.w > 0);
                        if let Agent::Up(sess, _) = &mut agent {
                            sess.send(ToAgent::Pause(!on)).map_err(io)?;
                        }
                        on.then_some(ToAgent::Refresh)
                    }
                }
                Ask::Upload { id, size, dir, name } => {
                    if size > MAX_TRANSFER || uploads.len() >= 8 || uploads.contains_key(&id) {
                        let text = if size > MAX_TRANSFER { "Larger than 2 GB: too large to send from a browser." } else { "Too many uploads at once." };
                        let _ = send_json(&w, serde_json::json!({ "uploaded": { "id": id, "ok": false, "text": text } }));
                        None
                    } else {
                        uploads.insert(id, (size, 0));
                        Some(ToAgent::FileStart { id, size, dir, name })
                    }
                }
                // Never more than the size it announced.
                Ask::Chunk(id, data) => match uploads.get_mut(&id) {
                    Some(u) if u.1 + data.len() as u64 <= u.0 => {
                        u.1 += data.len() as u64;
                        Some(ToAgent::FileData { id, data })
                    }
                    _ => None,
                },
                Ask::UploadEnd(id) if uploads.contains_key(&id) => Some(ToAgent::FileEnd { id }),
                Ask::UploadEnd(_) => None,
                Ask::Gone => break 'run String::new(),
            };
            let Some(m) = to_agent else { continue };
            match &mut agent {
                Agent::Up(sess, _) => sess.send(m).map_err(io)?,
                Agent::Connecting(_) => waiting.push(m),
                // Files asked for before any view wanted them reach for the agent too.
                Agent::Idle => {
                    let (tx, rx) = mpsc::channel();
                    let (target, at) = (open.agent.clone(), view);
                    std::thread::spawn(move || drop(tx.send(start(target, at))));
                    agent = Agent::Connecting(rx);
                    waiting.push(m);
                }
                Agent::Failed(why) => {
                    let _ = send_json(&w, serde_json::json!({ "agentError": why.clone() }));
                }
            }
        }
        if shown && dirty && fb.w > 0 && sent.elapsed() >= gap {
            let mut jpeg = vec![2u8];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, jpeg_q)
                .encode(&fb.rgb, fb.w as u32, fb.h as u32, image::ExtendedColorType::Rgb8)
                .map_err(|e| e.to_string())?;
            if send(&w, 0x2, &jpeg).is_err() {
                break String::new();
            }
            (dirty, sent) = (false, Instant::now());
        }
    };
    if let Agent::Up(mut sess, _) = agent {
        sess.close();
    }
    // A shell here hangs up as it goes.
    drop(pty);
    if !end.is_empty() {
        let _ = send_json(&w, serde_json::json!({ "end": end }));
    }
    if let Ok(s) = w.lock() {
        let _ = s.shutdown(std::net::Shutdown::Both);
    }
    Ok(Opened::Yes)
}

/// Why a connection failed, in the words of someone who only typed an ID and a password.
fn plain(e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("offline") || l.contains("no agent with that id") {
        "That computer is not online: its remote access is off, or it has no network.".into()
    } else if l.contains("wrong password") || l.contains("permission") {
        "Wrong password for that computer.".into()
    } else if l.contains("busy") || l.contains("try again") {
        "That computer is busy; try again in a moment.".into()
    } else {
        e.to_string()
    }
}

/// One message of the browser's: {"move":[x,y]}, {"button":[b,down]}, {"wheel":[dx,dy]},
/// {"key":[keysym,down]}, {"view":[w,h]}; points in the picture's pixels, wheel steps down/right
/// positive.
fn browser_input(v: &serde_json::Value) -> Option<Input> {
    use crate::proto::ToAgent;
    let pair = |k: &str| -> Option<(i64, i64)> {
        let a = v.get(k)?.as_array()?;
        let n = |i: usize| a.get(i).and_then(|x| x.as_i64().or_else(|| x.as_bool().map(i64::from)));
        Some((n(0)?, n(1)?))
    };
    if let Some((x, y)) = pair("move") {
        return Some(Input::Agent(ToAgent::Pointer { x: x.clamp(0, 65535) as u16, y: y.clamp(0, 65535) as u16 }));
    }
    if let Some((b, down)) = pair("button") {
        return Some(Input::Agent(ToAgent::Button { button: b.clamp(1, 9) as u8, pressed: down != 0 }));
    }
    if let Some((dx, dy)) = pair("wheel") {
        return Some(Input::Agent(if dy != 0 {
            ToAgent::Scroll { steps: dy.clamp(-10, 10) as i8 }
        } else {
            ToAgent::HScroll { steps: dx.clamp(-10, 10) as i8 }
        }));
    }
    if let Some((k, down)) = pair("key") {
        return Some(Input::Agent(ToAgent::KeyState { keysym: k.clamp(0, u32::MAX as i64) as u32, down: down != 0 }));
    }
    if let Some((vw, vh)) = pair("view") {
        return Some(Input::View(vw.clamp(64, 7680) as u16, vh.clamp(64, 4320) as u16));
    }
    None
}

// ----- SHA-1, for the WebSocket handshake only ----------------------------------------------------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request line longer than any browser's is refused instead of read on and on.
    #[test]
    fn long_request_lines_are_refused() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let _ = s.write_all(format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(20_000)).as_bytes());
            let _ = s.write_all(b"GET /fine HTTP/1.1\r\nHost: x\r\n\r\n");
        });
        let (conn, _) = l.accept().unwrap();
        let mut r = BufReader::new(conn);
        assert_eq!(read_request(&mut r).err().as_deref(), Some("request line too long"));
        sender.join().unwrap();
    }

    #[test]
    fn browser_input_messages() {
        use crate::proto::ToAgent;
        let i = |t: &str| browser_input(&serde_json::from_str(t).unwrap());
        assert!(matches!(i(r#"{"move":[10,20]}"#), Some(Input::Agent(ToAgent::Pointer { x: 10, y: 20 }))));
        assert!(matches!(i(r#"{"button":[3,1]}"#), Some(Input::Agent(ToAgent::Button { button: 3, pressed: true }))));
        assert!(matches!(i(r#"{"wheel":[0,-2]}"#), Some(Input::Agent(ToAgent::Scroll { steps: -2 }))));
        assert!(matches!(i(r#"{"wheel":[1,0]}"#), Some(Input::Agent(ToAgent::HScroll { steps: 1 }))));
        assert!(matches!(i(r#"{"key":[65293,0]}"#), Some(Input::Agent(ToAgent::KeyState { keysym: 0xff0d, down: false }))));
        assert!(matches!(i(r#"{"view":[10,99999]}"#), Some(Input::View(64, 4320))));
        assert!(i(r#"{"other":1}"#).is_none());
    }

    #[test]
    fn handshake_hash() {
        // RFC 6455's own example.
        use base64::Engine;
        let accept = base64::engine::general_purpose::STANDARD
            .encode(sha1(b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11"));
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert!(equal("a", "a") && !equal("a", "b"));
    }

    /// A browser's whole way through: page, wrong password refused, signed in, a shell opened on
    /// this computer, a command typed and its output back.
    #[test]
    fn browser_session() {
        let dir = std::env::temp_dir().join(format!("sshremote-web-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pw = dir.join("pw");
        std::fs::write(&pw, "secret-word").unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        // A relay nobody listens on: an ID connection fails there and says so.
        let args: Vec<String> = ["--listen", &format!("127.0.0.1:{port}"), "--password-file", pw.to_str().unwrap(), "--relay", "127.0.0.1:1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        std::thread::spawn(move || run(&args));
        std::thread::sleep(Duration::from_millis(300));
        let addr = format!("127.0.0.1:{port}");
        let mut page = TcpStream::connect(&addr).unwrap();
        page.write_all(b"GET /web/ HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut body = String::new();
        page.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 200") && body.contains("xterm.js"));
        // The page's script comes as a file, which the page's policy lets run.
        let mut script = TcpStream::connect(&addr).unwrap();
        script.write_all(b"GET /web/app.js HTTP/1.1
Host: x

").unwrap();
        let mut js = String::new();
        script.read_to_string(&mut js).unwrap();
        assert!(js.starts_with("HTTP/1.1 200") && js.contains("class Client") && !body.contains("<script>"));

        let open = || {
            let mut s = TcpStream::connect(&addr).unwrap();
            s.write_all(format!("GET /web/ws HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                r.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            (s, r)
        };
        let put = |s: &mut TcpStream, op: u8, data: &[u8]| s.write_all(&masked(true, op, data)).unwrap();
        let get = |r: &mut BufReader<TcpStream>| -> (u8, Vec<u8>) {
            let mut h = [0u8; 2];
            r.read_exact(&mut h).unwrap();
            let mut len = (h[1] & 0x7f) as usize;
            if len == 126 {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).unwrap();
                len = u16::from_be_bytes(b) as usize;
            }
            let mut d = vec![0u8; len];
            r.read_exact(&mut d).unwrap();
            (h[0] & 0x0f, d)
        };
        // By ID: the computer's own password is the sign-in, no gateway password needed.
        let (mut s, mut r) = open();
        put(&mut s, 1, br#"{"id":"12 34","password":"x"}"#);
        assert!(String::from_utf8(get(&mut r).1).unwrap().contains("9 digits"));
        // A relay that is down is said each time, never taken for wrong passwords that lock
        // the browser out.
        for _ in 0..7 {
            let (mut s, mut r) = open();
            put(&mut s, 1, br#"{"id":"123 456 789","password":"x","w":800,"h":600}"#);
            let mut said = String::new();
            while !said.contains("error") {
                said = String::from_utf8(get(&mut r).1).unwrap();
            }
            assert!(!said.contains("Too many"), "{said}");
        }

        // Everything else needs the gateway's password, or a sign-in it gave.
        let (mut s, mut r) = open();
        put(&mut s, 1, br#"{"gateway":{"password":"wrong"}}"#);
        assert!(String::from_utf8(get(&mut r).1).unwrap().contains("Wrong password"));
        let (mut s, mut r) = open();
        put(&mut s, 1, br#"{"open":{"host":"x@y"},"gateway":{"token":"forged"}}"#);
        assert!(String::from_utf8(get(&mut r).1).unwrap().contains("sign in"));

        let (mut s, mut r) = open();
        put(&mut s, 1, br#"{"gateway":{"password":"secret-word","remember":true}}"#);
        let signed: serde_json::Value = serde_json::from_slice(&get(&mut r).1).unwrap();
        assert!(signed["signedIn"]["hosts"].as_array().unwrap().iter().any(|h| h == HERE), "{signed}");
        let token = signed["signedIn"]["token"].as_str().expect("a sign-in to remember").to_string();

        // This computer's shell with the sign-in remembered: the client's frames, type 1 the
        // terminal's bytes.
        let (mut s, mut r) = open();
        let first = serde_json::json!({ "gateway": { "token": token }, "open": { "host": HERE }, "cols": 80, "rows": 24 });
        put(&mut s, 1, first.to_string().as_bytes());
        put(&mut s, 2, b"\x01echo web-42\r");
        let mut seen = Vec::new();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && String::from_utf8_lossy(&seen).matches("web-42").count() < 2
        {
            let (op, d) = get(&mut r);
            if op == 2 && d.first() == Some(&1) {
                seen.extend(&d[1..]);
            }
        }
        // Typed, then printed.
        assert!(String::from_utf8_lossy(&seen).matches("web-42").count() >= 2, "{}", String::from_utf8_lossy(&seen));
        // Closing the tab hangs up its shell at once, rather than leave it for the gateway's life.
        #[cfg(unix)]
        {
            put(&mut s, 2, b"\x01echo PID=$$=\r");
            let mut out = String::new();
            let pid = loop {
                let (op, d) = get(&mut r);
                if op == 2 && d.first() == Some(&1) {
                    out.push_str(&String::from_utf8_lossy(&d[1..]));
                }
                // The echo of the command shows "$$"; the shell's answer shows digits.
                if let Some(p) = out.split("PID=").filter_map(|t| t.split('=').next()?.parse::<u32>().ok()).next() {
                    break p;
                }
            };
            put(&mut s, 8, b"");
            let gone = (0..30).any(|_| {
                std::thread::sleep(Duration::from_millis(100));
                !std::path::Path::new(&format!("/proc/{pid}")).exists()
                    || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|st| st.contains(") Z "))
            });
            assert!(gone, "the shell {pid} outlived its tab");
        }
        #[cfg(not(unix))]
        put(&mut s, 8, b"");
    }

    /// A browser frame, masked, with whichever length field its size needs.
    fn masked(fin: bool, op: u8, data: &[u8]) -> Vec<u8> {
        let mut f = vec![if fin { 0x80 } else { 0 } | op];
        match data.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n < 65536 => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(&[9, 8, 7, 6]);
        f.extend(data.iter().enumerate().map(|(i, b)| b ^ [9, 8, 7, 6][i % 4]));
        f
    }

    /// A remembered sign-in is the gateway's to give: it ends when it says, and with a new
    /// password, and cannot be made up or changed.
    #[test]
    fn sign_ins_to_remember() {
        let t = token_new("pw");
        assert!(token_ok("pw", &t));
        assert!(!token_ok("new pw", &t), "a new password ends it");
        assert!(!token_ok("pw", "forged") && !token_ok("pw", ""));
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut b = b64.decode(&t).unwrap();
        b[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(!token_ok("pw", &b64.encode(&b)), "its end cannot be moved");
    }

    /// A host typed in the browser is only ever ssh's destination, and its password goes in the
    /// environment, never on the command line.
    #[test]
    fn ssh_from_the_browser() {
        let o = ssh_open("dev@build", Some(2222), Some("s3cret".into()));
        let (argv, env) = o.shell.unwrap();
        assert_eq!(argv.last().unwrap(), "dev@build");
        assert!(argv.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(!argv.iter().any(|a| a.contains("s3cret")));
        assert!(env.iter().any(|(k, v)| k == crate::vault::ASKPASS_SECRET && v == "s3cret"));
        assert!(o.lazy && matches!(o.agent, crate::transport::Target::Ssh { port: Some(2222), .. }));
    }

    #[test]
    fn frames_of_every_length_and_in_pieces() {
        let sink = TcpListener::bind("127.0.0.1:0").unwrap();
        let w = Mutex::new(TcpStream::connect(sink.local_addr().unwrap()).unwrap());
        let big: Vec<u8> = (0..70_000u32).map(|i| i as u8).collect();
        let mut wire = masked(true, 2, &big[..300]);
        wire.extend(masked(true, 2, &big));
        wire.extend(masked(false, 1, b"{\"a\":"));
        wire.extend(masked(true, 9, b"ping"));
        wire.extend(masked(true, 0, b"1}"));
        let mut r = &wire[..];
        assert!(matches!(read_frame(&mut r, &w), Ok(Frame::Binary(b)) if b == big[..300]));
        assert!(matches!(read_frame(&mut r, &w), Ok(Frame::Binary(b)) if b == big));
        assert!(matches!(read_frame(&mut r, &w), Ok(Frame::Text(t)) if t == "{\"a\":1}"));
        // Unmasked, a stray continuation and a long control frame are refused.
        assert!(read_frame(&mut &[0x82u8, 1, 0][..], &w).is_err());
        assert!(read_frame(&mut &masked(true, 0, b"x")[..], &w).is_err());
        assert!(read_frame(&mut &masked(true, 9, &[0; 200])[..], &w).is_err());
    }

    #[test]
    fn id_session_messages() {
        use crate::proto::ToAgent;
        let a = |t: &str| ask(Frame::Text(t.into()));
        assert!(matches!(a(r#"{"show":"screen"}"#), Some(Ask::Show(v)) if v == "screen"));
        assert!(matches!(a(r#"{"show":"files"}"#), Some(Ask::Show(v)) if v == "files"));
        assert!(matches!(a(r#"{"op":["rename","/a/b","c"]}"#), Some(Ask::Agent(ToAgent::FileOp { path, arg, .. })) if path == "/a/b" && arg == "c"));
        assert!(a(r#"{"op":["chmod","/a"]}"#).is_none());
        assert!(matches!(a(r#"{"download":"/d","tag":2147483649}"#), Some(Ask::Agent(ToAgent::Fetch { tag: 0x8000_0001, .. }))));
        assert!(matches!(a(r#"{"clipboard":"hi"}"#), Some(Ask::Agent(ToAgent::Clipboard(t))) if t == "hi"));
        assert!(matches!(a(r#"{"sas":1}"#), Some(Ask::Agent(ToAgent::SecureAttention))));
        assert!(matches!(a(r#"{"quality":"fast"}"#), Some(Ask::Quality(60, 150))));
        assert!(matches!(a(r#"{"list":""}"#), Some(Ask::Agent(ToAgent::ListDir { .. }))));
        assert!(matches!(a(r#"{"resize":[100,30]}"#), Some(Ask::Agent(ToAgent::TermResize { cols: 100, rows: 30, .. }))));
        assert!(matches!(a(r#"{"upload":{"id":3,"size":9,"dir":"/t","name":"a"}}"#), Some(Ask::Upload { id: 3, size: 9, .. })));
        assert!(matches!(a(r#"{"move":[1,2]}"#), Some(Ask::Agent(ToAgent::Pointer { x: 1, y: 2 }))));
        assert!(matches!(ask(Frame::Binary(vec![1, b'l', b's'])), Some(Ask::Agent(ToAgent::TermInput { data, .. })) if data == b"ls"));
        assert!(matches!(ask(Frame::Binary(vec![4, 7, 0, 0, 0, 1])), Some(Ask::Chunk(7, d)) if d == [1]));
        assert!(ask(Frame::Binary(vec![4, 7])).is_none() && ask(Frame::Binary(vec![9])).is_none());
    }

    /// A computer opened by ID, end to end over the socket: shell, screen, folder, download and
    /// upload, with an agent on this computer instead of the relay. Needs the built program:
    /// `cargo build --release` then `SSHREMOTE_EXE=$PWD/target/release/sshremote.exe cargo test
    /// --release id_client -- --ignored`.
    #[test]
    #[ignore]
    fn id_client_end_to_end() {
        let dir = std::env::temp_dir().join(format!("sshremote-web-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let dir_s = dir.display().to_string();
        let dir_s = dir_s.strip_prefix(r"\\?\").unwrap_or(&dir_s).to_string();
        std::fs::write(dir.join("pw"), "secret-word").unwrap();
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(dir.join("down.bin"), &payload).unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let args: Vec<String> =
            ["--listen", &addr, "--password-file", dir.join("pw").to_str().unwrap(), "--local-agent", "synthetic"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        std::thread::spawn(move || run(&args));
        std::thread::sleep(Duration::from_millis(300));

        let mut s = TcpStream::connect(&addr).unwrap();
        s.write_all(format!("GET /ws HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        while line != "\r\n" {
            line.clear();
            r.read_line(&mut line).unwrap();
        }
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let get = |r: &mut BufReader<TcpStream>| -> (u8, Vec<u8>) {
            let mut h = [0u8; 2];
            r.read_exact(&mut h).unwrap();
            let mut len = (h[1] & 0x7f) as u64;
            if len == 126 {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).unwrap();
                len = u16::from_be_bytes(b) as u64;
            } else if len == 127 {
                let mut b = [0u8; 8];
                r.read_exact(&mut b).unwrap();
                len = u64::from_be_bytes(b);
            }
            let mut d = vec![0u8; len as usize];
            r.read_exact(&mut d).unwrap();
            (h[0] & 0x0f, d)
        };
        let json = |s: &mut TcpStream, v: serde_json::Value| s.write_all(&masked(true, 1, v.to_string().as_bytes())).unwrap();
        // Reads until `want` takes a frame; terminal bytes seen on the way are kept.
        let mut shell = Vec::new();
        let mut until = |r: &mut BufReader<TcpStream>, what: &str, want: &mut dyn FnMut(u8, &[u8]) -> bool| {
            let start = Instant::now();
            loop {
                assert!(start.elapsed() < Duration::from_secs(30), "no {what}; terminal: {}", String::from_utf8_lossy(&shell));
                let (op, d) = get(r);
                if op == 2 && d.first() == Some(&1) {
                    shell.extend_from_slice(&d[1..]);
                }
                if want(op, &d) {
                    return d;
                }
            }
        };
        let text = |op: u8, d: &[u8], key: &str| op == 1 && serde_json::from_slice::<serde_json::Value>(d).is_ok_and(|v| v.get(key).is_some());

        json(&mut s, serde_json::json!({ "id": "123 456 789", "password": "x", "w": 640, "h": 480, "cols": 80, "rows": 24 }));
        until(&mut r, "ready", &mut |op, d| text(op, d, "ready"));

        // The shell: typed, then printed.
        s.write_all(&masked(true, 2, b"\x01echo e2e-77\r")).unwrap();
        let mut seen = Vec::new();
        until(&mut r, "echo", &mut |op, d| {
            if op == 2 && d.first() == Some(&1) {
                seen.extend_from_slice(&d[1..]);
            }
            String::from_utf8_lossy(&seen).matches("e2e-77").count() >= 2
        });

        // The screen: a JPEG once it shows.
        json(&mut s, serde_json::json!({ "show": "screen" }));
        let jpeg = until(&mut r, "picture", &mut |op, d| op == 2 && d.first() == Some(&2));
        assert_eq!(&jpeg[1..3], &[0xFF, 0xD8]);
        json(&mut s, serde_json::json!({ "show": "files" }));

        // The home folder, then the test's own.
        json(&mut s, serde_json::json!({ "list": "" }));
        let home = until(&mut r, "home folder", &mut |op, d| text(op, d, "dir"));
        let home: serde_json::Value = serde_json::from_slice(&home).unwrap();
        assert!(!home["dir"]["entries"].as_array().unwrap().is_empty(), "{home}");
        json(&mut s, serde_json::json!({ "list": dir_s }));
        let list = until(&mut r, "test folder", &mut |op, d| text(op, d, "dir"));
        assert!(String::from_utf8_lossy(&list).contains("down.bin"));

        // A download, byte for byte.
        let path = dir.join("down.bin").display().to_string();
        json(&mut s, serde_json::json!({ "download": path.strip_prefix(r"\\?\").unwrap_or(&path) }));
        let mut got = Vec::new();
        let end = until(&mut r, "download", &mut |op, d| {
            if op == 2 && d.first() == Some(&3) {
                got.extend_from_slice(&d[5..]);
            }
            text(op, d, "fetched")
        });
        assert!(String::from_utf8_lossy(&end).contains("\"ok\":true"), "{}", String::from_utf8_lossy(&end));
        assert!(got == payload, "downloaded {} bytes of {}", got.len(), payload.len());

        // An upload in chunks, byte for byte on disk.
        let up: Vec<u8> = payload.iter().rev().copied().collect();
        json(&mut s, serde_json::json!({ "upload": { "id": 7, "size": up.len(), "dir": dir_s, "name": "up.bin" } }));
        for c in up.chunks(64 << 10) {
            let mut f = vec![4u8];
            f.extend_from_slice(&7u32.to_le_bytes());
            f.extend_from_slice(c);
            s.write_all(&masked(true, 2, &f)).unwrap();
        }
        json(&mut s, serde_json::json!({ "uploadEnd": 7 }));
        let done = until(&mut r, "upload", &mut |op, d| text(op, d, "uploaded"));
        assert!(String::from_utf8_lossy(&done).contains("\"ok\":true"), "{}", String::from_utf8_lossy(&done));
        assert!(std::fs::read(dir.join("up.bin")).unwrap() == up);

        s.write_all(&masked(true, 8, b"")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
