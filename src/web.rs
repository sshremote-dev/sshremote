//! The web gateway: `sshremote web` serves a page to browsers and, over a WebSocket, joins it to
//! a computer as the app does: its terminal, screen and files on one session. A computer by its
//! remote access ID needs only its own password (it is reached through the relay); SSH hosts and
//! this computer's own shells need the gateway's password, or a sign-in it gave the browser to
//! remember. Put it behind HTTPS (nginx, Caddy) when it faces the internet.
//!
//! On the socket, text frames carry JSON (sign-in, what to open, the views, files, the end) and
//! binary frames start with a type byte: 1 terminal bytes, 2 a JPEG picture, 3 a download chunk
//! and 4 an upload chunk (both after a u32 LE id), and 8 a part of a picture copied (as PNG: on
//! the host to the page, or on the browser's computer to the host, ended by `clipImageEnd`).

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
/// Wrong gateway passwords from all addresses together in a quarter of an hour before every
/// password try waits.
const MOST_FAILURES: u32 = 50;
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
    // Its visitors are not the person this program runs as.
    crate::transport::serve_others();
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
            // Small messages (keys, a shell's echo) go at once, not held for the last one's
            // acknowledgement (up to 40 ms on Linux, the proxy's link here too).
            let _ = conn.set_nodelay(true);
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
    /// HEAD: the headers alone, as a check of the page would ask.
    head: bool,
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
    let head = method == "HEAD";
    if method != "GET" && !head {
        return Err("only GET".into());
    }
    let mut headers = HashMap::new();
    for _ in 0..100 {
        read_line(r, &mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            return Ok(Request { path, headers, head });
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Err("too many headers".into())
}

/// A file of the page, or only its headers for HEAD.
fn page(s: &TcpStream, req: &Request, kind: &str, body: &[u8]) -> Result<(), String> {
    respond_as(s, "200 OK", kind, body, !req.head)
}

fn respond(s: &TcpStream, status: &str, kind: &str, body: &[u8]) -> Result<(), String> {
    respond_as(s, status, kind, body, true)
}

fn respond_as(mut s: &TcpStream, status: &str, kind: &str, body: &[u8], with_body: bool) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:\r\n\
         Referrer-Policy: no-referrer\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).and_then(|_| if with_body { s.write_all(body) } else { Ok(()) }).map_err(|e| e.to_string())
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
        "" | "/" | "/index.html" => {
            // A gateway for IDs only says so on the page, which then asks for an ID alone.
            let body = if gate.id_only { PAGE.replacen("<html lang=\"en\">", "<html lang=\"en\" data-id-only>", 1) } else { PAGE.to_string() };
            page(&conn, &req, "text/html; charset=utf-8", body.as_bytes())
        }
        "/app.js" => page(&conn, &req, "text/javascript", APP_JS.as_bytes()),
        "/xterm.js" => page(&conn, &req, "text/javascript", XTERM_JS.as_bytes()),
        "/addon-fit.js" => page(&conn, &req, "text/javascript", FIT_JS.as_bytes()),
        "/favicon.svg" => page(&conn, &req, "image/svg+xml", LOGO.as_bytes()),
        "/xterm.css" => page(&conn, &req, "text/css", XTERM_CSS.as_bytes()),
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
    // One write for a small frame: two let the second wait for the first's acknowledgement.
    if data.len() < 64 << 10 {
        head.extend_from_slice(data);
        return w.lock().map_err(|e| e.to_string())?.write_all(&head).map_err(|e| e.to_string());
    }
    let mut s = w.lock().map_err(|e| e.to_string())?;
    s.write_all(&head).and_then(|_| s.write_all(data)).map_err(|e| e.to_string())
}

fn send_json(w: &Mutex<TcpStream>, v: serde_json::Value) -> Result<(), String> {
    send(w, 0x1, v.to_string().as_bytes())
}

// ----- A session -------------------------------------------------------------------------------------

/// The hosts a signed-in browser is offered: this computer, and the computers it reaches by SSH
/// (its ~/.ssh/config aliases among them), favorites first.
fn hosts() -> Vec<String> {
    let mut out = vec![HERE.to_string()];
    // Web apps kept in the list are the app's panes, not hosts.
    out.extend(
        crate::computers::all().into_iter().filter(|c| c.id().is_none() && !c.address.starts_with("https://") && !c.address.starts_with("http://")).map(|c| c.address),
    );
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
                // The two-step code rides with the password (`net::CODE_SEP`).
                let code = text("code");
                let password = if code.trim().is_empty() { text("password") } else { format!("{}{}{}", text("password"), crate::net::CODE_SEP, code.trim()) };
                crate::transport::Target::Direct { addr, password }
            }
        };
        let open = Open { name: format!("ID {}", crate::computers::spaced(&id)), agent, lazy: false, shell: None };
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
    // A sign-in this gateway gave cannot be guessed: it works even while guessing is held off, so
    // someone trying passwords does not lock the owner out.
    let by_token = !field("token").is_empty() && token_ok(&gate.password, &field("token"));
    // A password try is counted before it is checked, under one lock: tries made at the same time
    // all count, and none slips past the limit.
    let mut blocked = blocked;
    // No password at all (a remembered sign-in that ran out) is no guess, and is not counted.
    let ok = by_token || !field("password").is_empty() && {
        let reserved = {
            let mut f = gate.failures.lock().map_err(|e| e.to_string())?;
            f.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(900));
            // All addresses together: one that changes its X-Real-IP is held off as well.
            let everyone: u32 = f.values().map(|(n, _)| *n).sum();
            let mine = f.get(&who).is_some_and(|(n, _)| *n >= 5);
            if mine || everyone >= MOST_FAILURES {
                false
            } else {
                let e = f.entry(who).or_insert((0, Instant::now()));
                *e = (e.0 + 1, Instant::now());
                true
            }
        };
        blocked = !reserved;
        reserved && equal(&field("password"), &gate.password)
    };
    if !ok {
        if !blocked {
            std::thread::sleep(Duration::from_secs(2));
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
        // One of the gateway's computers opens on its own port unless the browser gives another.
        let port = o.get("port").and_then(|x| x.as_u64()).filter(|p| (1..=65535).contains(p)).map(|p| p as u16).or_else(|| crate::computers::get(&host).and_then(|c| c.port));
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
    /// A part of a picture copied in the browser, for the host's clipboard; then its end.
    ClipPart(Vec<u8>),
    ClipEnd,
    /// The picture's quality: as the app's (auto, a preset, or colours, resolution and frame
    /// rate of the user's own).
    Quality(WebQuality),
    /// The browser drew the picture update of this number.
    Drawn(u32),
    /// The picture paused by the user (to save traffic), or going on.
    Pause(bool),
    Gone,
}

/// A quality asked for from the browser.
#[derive(Debug, PartialEq)]
enum WebQuality {
    Auto,
    Preset(usize),
    Custom(crate::color::Mode, u8, u8),
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
                // The browser's voice in a call: 20 ms of Opus.
                Some((7, data)) if data.len() <= 4000 => Some(Ask::Agent(ToAgent::Voice(data.to_vec()))),
                Some((8, data)) => Some(Ask::ClipPart(data.to_vec())),
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
    // A message for the person at the host: a chat window opens there.
    if let Some(t) = s("chat").filter(|t| !t.trim().is_empty()) {
        return Some(Ask::Agent(ToAgent::Chat(t)));
    }
    // The computer itself: {"host": {"act": N, "on": true}} (`hostctl`).
    if let Some(h) = v.get("host") {
        let act = h.get("act").and_then(|a| a.as_u64()).filter(|a| *a <= 5)? as u8;
        return Some(Ask::Agent(ToAgent::Host { act, on: h.get("on").and_then(|o| o.as_bool()).unwrap_or(false) }));
    }
    if v.get("sas").is_some() {
        return Some(Ask::Agent(ToAgent::SecureAttention));
    }
    if let Some(m) = n("monitor") {
        return Some(Ask::Agent(ToAgent::SelectMonitor(m.min(255) as u8)));
    }
    // The screen's resolution: {"mode": [w, h]}, [0, 0] for its own.
    if let Some([w, h]) = v.get("mode").and_then(|m| m.as_array()).and_then(|a| <[_; 2]>::try_from(a.iter().map(|x| x.as_u64().unwrap_or(0).min(u16::MAX as u64) as u16).collect::<Vec<_>>()).ok()) {
        return Some(Ask::Agent(ToAgent::SetMode { w, h }));
    }
    if v.get("refresh").is_some() {
        return Some(Ask::Agent(ToAgent::Refresh));
    }
    if let Some(q) = s("quality") {
        let level = n("level").map(|l| (l as usize).min(crate::proto::PRESETS.len() - 1));
        return Some(Ask::Quality(match q.as_str() {
            "preset" => WebQuality::Preset(level?),
            "custom" => {
                let mode = crate::color::Mode::parse(&s("mode")?)?;
                WebQuality::Custom(mode, n("scale")?.clamp(10, 100) as u8, n("fps")?.clamp(1, crate::session::MOST_FPS as u64) as u8)
            }
            // The page's older words.
            "best" => WebQuality::Preset(crate::proto::PRESETS.len() - 1),
            "fast" => WebQuality::Preset(1),
            _ => WebQuality::Auto,
        }));
    }
    if let Some(seq) = n("drawn") {
        return Some(Ask::Drawn(seq as u32));
    }
    if let Some(on) = v.get("pause").and_then(|x| x.as_bool()) {
        return Some(Ask::Pause(on));
    }
    // The host's sound, as Opus packets the browser decodes.
    // Files uploaded into a clipboard batch, to paste there: {"pasteFiles":{"batch":B,"names":[..]}}.
    if let Some(p) = v.get("pasteFiles") {
        let batch = p.get("batch")?.as_str()?.to_string();
        let names = p.get("names")?.as_array()?.iter().filter_map(|n| n.as_str().map(String::from)).collect();
        return Some(Ask::Agent(ToAgent::PasteFiles { batch, names }));
    }
    if let Some(on) = v.get("call").and_then(|x| x.as_bool()) {
        return Some(Ask::Agent(ToAgent::Call(on)));
    }
    if let Some(on) = v.get("sound").and_then(|x| x.as_bool()) {
        return Some(Ask::Agent(ToAgent::Audio(on)));
    }
    // A lossless picture of the screen, as the app saves one.
    if v.get("snapshot").is_some() {
        return Some(Ask::Agent(ToAgent::Snapshot));
    }
    // What is shared: the whole screen, a window, a program; and the windows to choose from.
    if v.get("windows").is_some() {
        return Some(Ask::Agent(ToAgent::ListWindows));
    }
    if let Some(sh) = v.get("share") {
        use crate::shared::Share;
        // Window ids go as text: they can be larger than a browser's numbers hold exactly.
        let id = || sh.get("id").and_then(|x| x.as_str()).and_then(|t| t.parse::<u64>().ok());
        let share = match sh.get("kind").and_then(|x| x.as_str())? {
            "screen" => Share::Screen,
            "window" => Share::Window(id()?),
            "app" => Share::App(id()?),
            "activeWindow" => Share::ActiveWindow,
            "activeApp" => Share::ActiveApp,
            _ => return None,
        };
        return Some(Ask::Agent(ToAgent::Share(share)));
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
    if v.get("clipImageEnd").is_some() {
        return Some(Ask::ClipEnd);
    }
    match browser_input(&v)? {
        Input::Agent(a) => Some(Ask::Agent(a)),
        Input::View(w, h) => Some(Ask::View(w, h)),
    }
}

/// What the picture is asked to be: the quality chosen, for the browser's view.
fn settings(ctl: &crate::session::Control, (vw, vh): (u16, u16)) -> crate::proto::Settings {
    crate::proto::Settings { view_w: vw, view_h: vh, ..ctl.s }
}

/// Reaches the agent, with no picture until the screen shows; what it says before it answers
/// (that the person at the screen is asked) is the browser's status meanwhile.
fn reach(
    target: crate::transport::Target,
    s: crate::proto::Settings,
    w: Arc<Mutex<TcpStream>>,
) -> Result<(crate::session::Session, mpsc::Receiver<crate::session::Ev>), String> {
    use crate::proto::ToAgent;
    let tell = |t: &str| drop(send_json(&w, serde_json::json!({ "status": t })));
    let (mut sess, _tx, rx) = crate::session::Session::open_noting(&target, true, &tell)?;
    sess.send(ToAgent::Pause(true)).map_err(|e| e.to_string())?;
    sess.send(ToAgent::Settings(s)).map_err(|e| e.to_string())?;
    Ok((sess, rx))
}

/// A rectangle of the picture: x, y, width, height.
type Area = (usize, usize, usize, usize);

/// The picture's changed parts as a few rectangles: those that touch or nearly do become one,
/// and at most `MAX_AREAS` remain (the closest merged first), each inside a `w` x `h` picture.
fn merge_areas(areas: &[Area], w: usize, h: usize) -> Vec<Area> {
    const NEAR: usize = 8;
    const MAX_AREAS: usize = 12;
    let mut out: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut sorted: Vec<Area> = areas.iter().filter(|a| a.2 > 0 && a.3 > 0 && a.0 < w && a.1 < h).copied().collect();
    sorted.sort_by_key(|a| (a.1, a.0));
    for (x, y, aw, ah) in sorted {
        let (l, t, r, b) = (x, y, (x + aw).min(w), (y + ah).min(h));
        match out.iter_mut().find(|o| l <= o.2 + NEAR && o.0 <= r + NEAR && t <= o.3 + NEAR && o.1 <= b + NEAR) {
            Some(o) => *o = (o.0.min(l), o.1.min(t), o.2.max(r), o.3.max(b)),
            None => out.push((l, t, r, b)),
        }
    }
    // Merged rectangles can now touch others: again until none do, then down to the most kept.
    loop {
        let mut merged = false;
        'outer: for i in 0..out.len() {
            for j in i + 1..out.len() {
                let (a, b) = (out[i], out[j]);
                if a.0 <= b.2 + NEAR && b.0 <= a.2 + NEAR && a.1 <= b.3 + NEAR && b.1 <= a.3 + NEAR {
                    out[i] = (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3));
                    out.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if merged {
            continue;
        }
        if out.len() <= MAX_AREAS {
            break;
        }
        let area = |r: &(usize, usize, usize, usize)| (r.2 - r.0) * (r.3 - r.1);
        let mut best = (usize::MAX, 0, 1);
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                let u = (out[i].0.min(out[j].0), out[i].1.min(out[j].1), out[i].2.max(out[j].2), out[i].3.max(out[j].3));
                let grow = area(&u).saturating_sub(area(&out[i]) + area(&out[j]));
                if grow < best.0 {
                    best = (grow, i, j);
                }
            }
        }
        let (_, i, j) = best;
        let (a, b) = (out[i], out[j]);
        out[i] = (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3));
        out.swap_remove(j);
    }
    out.into_iter().map(|(l, t, r, b)| (l, t, r - l, b - t)).collect()
}

/// One part of the picture as the browser decodes it: lossless WebP for what has few colours
/// (text, screens of the reduced colour modes) or when lossless is asked for, JPEG at `q` for
/// photographs. Returns the format (0 WebP, 1 JPEG) and the bytes.
fn encode_area(fb: &crate::session::Framebuffer, (x, y, w, h): Area, lossless: bool, q: u8) -> Result<(u8, Vec<u8>), String> {
    let mut px = Vec::with_capacity(w * h * 3);
    for row in y..y + h {
        let o = (row * fb.w + x) * 3;
        px.extend_from_slice(&fb.rgb[o..o + w * 3]);
    }
    // A sample of its colours: more than 256 reads as a photograph.
    let few = lossless || {
        let mut seen = std::collections::HashSet::new();
        let step = (w * h / 4096).max(1);
        px.chunks(3).step_by(step).all(|c| {
            seen.insert([c[0], c[1], c[2]]);
            seen.len() <= 256
        })
    };
    let mut out = Vec::new();
    if few {
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .encode(&px, w as u32, h as u32, image::ExtendedColorType::Rgb8)
            .map_err(|e| e.to_string())?;
        Ok((0, out))
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q).encode(&px, w as u32, h as u32, image::ExtendedColorType::Rgb8).map_err(|e| e.to_string())?;
        Ok((1, out))
    }
}

/// A picture update for the browser: its number and the picture's size, the moves to make (an
/// area shifted up or down, as the agent found the screen scrolled), then the changed areas.
fn picture_update(seq: u32, fb: &crate::session::Framebuffer, moves: &[crate::scroll::Move], areas: &[Area], lossless: bool, q: u8) -> Result<Vec<u8>, String> {
    let mut b = vec![4u8];
    b.extend_from_slice(&seq.to_le_bytes());
    b.extend_from_slice(&(fb.w as u16).to_le_bytes());
    b.extend_from_slice(&(fb.h as u16).to_le_bytes());
    b.push(moves.len().min(255) as u8);
    for m in moves.iter().take(255) {
        for v in [m.x, m.y, m.w, m.h] {
            b.extend_from_slice(&(v as u16).to_le_bytes());
        }
        b.extend_from_slice(&(m.dy as i16).to_le_bytes());
    }
    b.push(areas.len() as u8);
    for &a in areas {
        let (fmt, data) = encode_area(fb, a, lossless, q)?;
        for v in [a.0, a.1, a.2, a.3] {
            b.extend_from_slice(&(v as u16).to_le_bytes());
        }
        b.push(fmt);
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
    }
    Ok(b)
}

/// The quality in effect, for the page's menu and status.
fn quality_state(ctl: &crate::session::Control) -> serde_json::Value {
    serde_json::json!({
        "auto": ctl.auto.is_some(),
        "preset": ctl.preset,
        "mode": ctl.s.mode.name(),
        "scale": ctl.s.scale,
        "fps": ctl.s.fps,
        "text": ctl.describe(),
    })
}

/// The presets as the browser lists them.
fn quality_list() -> serde_json::Value {
    crate::proto::PRESETS
        .iter()
        .map(|p| serde_json::json!({ "name": p.name, "text": format!("{}, {}% size, {} fps", p.mode.label(), p.scale, p.fps) }))
        .collect()
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
    // Rung by the agent's messages, the browser's and the shell here's output, so the loop below
    // takes each at once rather than after its wait for the agent (keys waited up to 10 ms).
    let (bell, rung) = mpsc::channel::<()>();
    let hook = |s: &crate::session::Session| {
        let b = bell.clone();
        s.on_message(move || {
            let _ = b.send(());
        });
    };
    // The picture's quality, as the app's: auto until the browser asks for another.
    let mut ctl = crate::session::Control::for_preset(None, crate::color::Mode::True);
    (ctl.s.view_w, ctl.s.view_h) = view;
    // The host's pointer comes apart, to be the browser's own (it moves with no delay at all).
    ctl.s.formats |= crate::proto::FORMAT_POINTER;
    let mut agent = if open.lazy {
        Agent::Idle
    } else {
        match start(open.agent.clone(), settings(&ctl, view), w.clone()) {
            Ok((s, rx)) => {
                hook(&s);
                Agent::Up(s, rx)
            }
            Err(e) => {
                let refused = e.to_lowercase().contains("wrong password") || e.to_lowercase().contains("wrong code");
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
        let b = bell.clone();
        crate::pty::Local::spawn_with(argv, env, cols, rows, Box::new(move || {
            let _ = b.send(());
        }))
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
    // What extensions this gateway has bring their menus to the page (Programs: the host's programs).
    send_json(&w, serde_json::json!({ "ready": { "host": open.name, "screen": desk } }))?;
    send_json(&w, serde_json::json!({ "qualities": quality_list(), "quality": quality_state(&ctl) }))?;
    // The browser's messages on their own thread; a bounded queue so a fast sender waits.
    let (in_tx, input) = mpsc::sync_channel(32);
    let w_in = w.clone();
    let b = bell.clone();
    std::thread::spawn(move || {
        loop {
            let msg = match read_frame(&mut r, &w_in) {
                Ok(f) => ask(f),
                Err(_) => Some(Ask::Gone),
            };
            if let Some(m) = msg {
                let gone = matches!(m, Ask::Gone);
                let sent = in_tx.send(m).is_ok();
                let _ = b.send(());
                if !sent || gone {
                    return;
                }
            }
        }
    });
    let mut fb = crate::session::Framebuffer::default();
    // The screen shows in the browser, and the user did not pause its picture.
    let (mut shown, mut paused) = (false, false);
    // What changed of the picture since the browser was last sent it, and the moves the agent
    // made before any of that (sent as moves, not as pictures of what moved).
    let (mut damage, mut moves): (Vec<Area>, Vec<crate::scroll::Move>) = (Vec::new(), Vec::new());
    // Updates sent and not drawn yet: their numbers, and when they went. At most two wait, so a
    // slow browser gets fewer, larger updates rather than a queue of stale ones.
    let mut drawing: Vec<(u32, Instant)> = Vec::new();
    let mut seq = 0u32;
    // How long the browser takes to draw, which auto counts as part of the round trip.
    let mut draw_ms = 0.0f64;
    let mut sent_bytes = (0u64, Instant::now());
    let mut view = view;
    // Asks waiting for the agent to be reached.
    let mut waiting: Vec<ToAgent> = Vec::new();
    // Downloads under way: bytes passed on to the browser and bytes acknowledged to the agent.
    let mut downloads: HashMap<u32, (u64, u64)> = HashMap::new();
    // Uploads under way: declared size and bytes passed on.
    let mut uploads: HashMap<u32, (u64, u64)> = HashMap::new();
    // A picture copied in the browser, as its parts come; None once it is larger than the host takes.
    let mut clip_image: Option<Vec<u8>> = Some(Vec::new());
    let end = 'run: loop {
        // The agent's messages, or a short wait when there is no agent to hear from.
        let mut msgs = Vec::new();
        let wait = || {
            let _ = rung.recv_timeout(Duration::from_millis(10));
            rung.try_iter().for_each(drop);
        };
        match &mut agent {
            Agent::Up(_, rx) => {
                let first = match rx.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => {
                        wait();
                        rx.try_recv()
                    }
                    got => got,
                };
                match first {
                    Ok(m) => {
                        msgs.push(m);
                        msgs.extend(rx.try_iter().take(256));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => msgs.push(Ev::Closed("the session ended".into())),
                }
            }
            Agent::Connecting(c) => match c.try_recv() {
                Ok(Ok((mut sess, rx))) => {
                    // Reached: what was asked meanwhile goes now.
                    let _ = send_json(&w, serde_json::json!({ "agent": { "screen": [sess.desk.0, sess.desk.1] } }));
                    if shown && !paused {
                        let _ = sess.send(ToAgent::Pause(false));
                    }
                    for m in waiting.drain(..) {
                        sess.send(m).map_err(io)?;
                    }
                    hook(&sess);
                    agent = Agent::Up(sess, rx);
                }
                Ok(Err(e)) => {
                    let text = plain(&e);
                    let _ = send_json(&w, serde_json::json!({ "agentError": text }));
                    waiting.clear();
                    agent = Agent::Failed(text);
                }
                Err(_) => wait(),
            },
            _ => wait(),
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
                    let resized = fb.apply(&f).map_err(io)?;
                    sess.send(ToAgent::Ack(f.id)).map_err(io)?;
                    if std::mem::take(&mut fb.need_key) {
                        sess.send(ToAgent::Refresh).map_err(io)?;
                    }
                    ctl.frame_done(f.covered() * 2 > fb.w * fb.h);
                    if resized {
                        (damage, moves) = (vec![(0, 0, fb.w, fb.h)], Vec::new());
                    } else {
                        // A move goes as a move while nothing waits; after other changes, as the
                        // picture of where it landed.
                        // The page moves areas up and down only: one moved sideways goes as a picture.
                        if damage.is_empty() && f.moves.iter().all(|m| m.dx == 0) {
                            moves.extend(f.moves.iter().copied());
                        } else {
                            damage.extend(f.moves.iter().map(|m| (m.x, m.y, m.w, m.h)));
                        }
                        damage.extend(f.tiles.iter().chain(f.video.iter()).map(|t| (t.x as usize, t.y as usize, t.w as usize, t.h as usize)));
                    }
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
                ToClient::Modes { list, current, own } => {
                    let pair = |(a, b): (u16, u16)| [a, b];
                    let list: Vec<_> = list.iter().map(|m| pair(*m)).collect();
                    let _ = send_json(&w, serde_json::json!({ "modes": { "list": list, "current": pair(current), "own": pair(own) } }));
                }
                ToClient::Notice(t) => {
                    let _ = send_json(&w, serde_json::json!({ "notice": t }));
                }
                ToClient::HostState { can, on } => {
                    let _ = send_json(&w, serde_json::json!({ "host": { "can": can, "on": on } }));
                }
                ToClient::Allowed(bits) => {
                    let _ = send_json(&w, serde_json::json!({ "allowed": { "bits": bits, "words": crate::grant::words(bits) } }));
                }
                ToClient::Chat(t) => {
                    let _ = send_json(&w, serde_json::json!({ "chat": t }));
                }
                ToClient::WindowList { list, error } => {
                    let list: Vec<_> = list.iter().map(|w| serde_json::json!({ "id": w.id.to_string(), "title": w.title, "app": w.app, "w": w.w, "h": w.h })).collect();
                    let _ = send_json(&w, serde_json::json!({ "windows": { "list": list, "error": error } }));
                }
                ToClient::Sharing { kind, text } => {
                    let _ = send_json(&w, serde_json::json!({ "sharing": { "kind": kind, "text": text } }));
                }
                ToClient::Audio(packet) => {
                    if send(&w, 0x2, &[&[6u8][..], &packet].concat()).is_err() {
                        break 'run String::new();
                    }
                }
                ToClient::Voice(packet) => {
                    if send(&w, 0x2, &[&[7u8][..], &packet].concat()).is_err() {
                        break 'run String::new();
                    }
                }
                ToClient::CallState(state) => {
                    let _ = send_json(&w, serde_json::json!({ "call": state }));
                }
                ToClient::Snapshot(png) => {
                    if send(&w, 0x2, &[&[5u8][..], &png].concat()).is_err() {
                        break 'run String::new();
                    }
                }
                ToClient::ClipImage(png) => {
                    if send(&w, 0x2, &[&[8u8][..], &png].concat()).is_err() {
                        break 'run String::new();
                    }
                }
                // The pointer's picture as a PNG with the pixel that points, for the page to use
                // as its own pointer over the screen.
                ToClient::PointerShape { w: pw, h: ph, hot_x, hot_y, rgba } => {
                    let mut png = Vec::new();
                    let ok = image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut png), &rgba, pw as u32, ph as u32, image::ExtendedColorType::Rgba8).is_ok();
                    let head = [9u8, hot_x as u8, (hot_x >> 8) as u8, hot_y as u8, (hot_y >> 8) as u8];
                    if ok && send(&w, 0x2, &[&head[..], &png].concat()).is_err() {
                        break 'run String::new();
                    }
                }
                // Where the host has it, in the picture's pixels.
                ToClient::PointerAt(at) => {
                    let (dw, dh) = (sess.desk.0.max(1) as f64, sess.desk.1.max(1) as f64);
                    let at = at.map(|(x, y)| [(x as f64 * fb.w as f64 / dw) as i32, (y as f64 * fb.h as f64 / dh) as i32]);
                    let _ = send_json(&w, serde_json::json!({ "hostPointer": at }));
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
                    (ctl.s.view_w, ctl.s.view_h) = view;
                    Some(ToAgent::Settings(settings(&ctl, view)))
                }
                Ask::Quality(q) => {
                    match q {
                        WebQuality::Auto => drop(ctl.menu('a')),
                        WebQuality::Preset(i) => drop(ctl.menu(char::from(b'0' + i as u8))),
                        WebQuality::Custom(mode, scale, fps) => ctl.set_custom(mode, scale, fps),
                    }
                    let _ = send_json(&w, serde_json::json!({ "quality": quality_state(&ctl) }));
                    Some(ToAgent::Settings(settings(&ctl, view)))
                }
                Ask::Pause(on) => {
                    paused = on;
                    Some(ToAgent::Pause(on || !shown))
                }
                Ask::Drawn(n) => {
                    if let Some(i) = drawing.iter().position(|d| d.0 == n) {
                        let ms = drawing.remove(i).1.elapsed().as_secs_f64() * 1000.0;
                        draw_ms = draw_ms * 0.7 + ms * 0.3;
                    }
                    None
                }
                Ask::Show(v) => {
                    let on = v == "screen";
                    // The screen or the files: an agent not reached yet is reached now.
                    if v != "terminal" && matches!(agent, Agent::Idle) {
                        let _ = send_json(&w, serde_json::json!({ "status": "Connecting for the screen and files\u{2026}" }));
                        let (tx, rx) = mpsc::channel();
                        let (target, at) = (open.agent.clone(), settings(&ctl, view));
                        let (w, b) = (w.clone(), bell.clone());
                        std::thread::spawn(move || {
                            let _ = tx.send(start(target, at, w));
                            let _ = b.send(());
                        });
                        agent = Agent::Connecting(rx);
                    }
                    if on == shown {
                        None
                    } else {
                        shown = on;
                        // Shown again: the whole picture, as it is now.
                        if on && fb.w > 0 {
                            (damage, moves) = (vec![(0, 0, fb.w, fb.h)], Vec::new());
                            drawing.clear();
                        }
                        if let Agent::Up(sess, _) = &mut agent {
                            sess.send(ToAgent::Pause(!on || paused)).map_err(io)?;
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
                Ask::ClipPart(data) => {
                    clip_image = clip_image.take().filter(|b| b.len() + data.len() <= crate::clip::IMAGE_MAX).map(|mut b| {
                        b.extend_from_slice(&data);
                        b
                    });
                    None
                }
                Ask::ClipEnd => match std::mem::replace(&mut clip_image, Some(Vec::new())) {
                    Some(png) if !png.is_empty() => Some(ToAgent::ClipImage(png)),
                    Some(_) => None,
                    None => {
                        let _ = send_json(&w, serde_json::json!({ "notice": "The picture is larger than 4 MB: paste it as a file instead." }));
                        None
                    }
                },
                Ask::Gone => break 'run String::new(),
            };
            let Some(m) = to_agent else { continue };
            match &mut agent {
                Agent::Up(sess, _) => sess.send(m).map_err(io)?,
                Agent::Connecting(_) => waiting.push(m),
                // Files asked for before any view wanted them reach for the agent too.
                Agent::Idle => {
                    let (tx, rx) = mpsc::channel();
                    let (target, at) = (open.agent.clone(), settings(&ctl, view));
                    let (w, b) = (w.clone(), bell.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(start(target, at, w));
                        let _ = b.send(());
                    });
                    agent = Agent::Connecting(rx);
                    waiting.push(m);
                }
                Agent::Failed(why) => {
                    let _ = send_json(&w, serde_json::json!({ "agentError": why.clone() }));
                }
            }
        }
        // An answer lost on the way never stops the picture.
        drawing.retain(|d| d.1.elapsed() < Duration::from_secs(3));
        if shown && fb.w > 0 && drawing.len() < 2 && (!damage.is_empty() || !moves.is_empty()) {
            let areas = merge_areas(&damage, fb.w, fb.h);
            // Lossless at the best preset; photographs as JPEG otherwise, sharper at the crisp one.
            let lossless = ctl.auto.is_none() && ctl.preset == crate::proto::PRESETS[crate::proto::PRESETS.len() - 1].name;
            let q = if ctl.preset == "crisp" { 88 } else { 78 };
            seq = seq.wrapping_add(1);
            let update = picture_update(seq, &fb, &moves, &areas, lossless, q)?;
            (damage, moves) = (Vec::new(), Vec::new());
            sent_bytes.0 += update.len() as u64;
            if send(&w, 0x2, &update).is_err() {
                break String::new();
            }
            drawing.push((seq, Instant::now()));
        }
        // Once a second: what auto makes of the link (the browser's drawing counted in its round
        // trip), and the figures for the page.
        if shown && let Agent::Up(sess, _) = &mut agent {
            ctl.rtt = ctl.rtt.max(draw_ms);
            if ctl.tick(sess).map_err(io)? {
                let secs = sent_bytes.1.elapsed().as_secs_f64().max(0.001);
                let kbps = sent_bytes.0 as f64 / 1024.0 / secs;
                sent_bytes = (0, Instant::now());
                let _ = send_json(&w, serde_json::json!({ "stats": { "kbps": kbps, "fps": ctl.fps_now, "rtt": ctl.rtt }, "quality": quality_state(&ctl) }));
            }
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
    } else if l.contains("without its password") {
        "That computer lets nobody in without its password: type it.".into()
    } else if l.contains("only from some addresses") {
        "That computer takes viewers only from some addresses, and this gateway's is not one of them.".into()
    } else if l.contains("answering someone else") {
        "Someone else is asking that computer just now; try again in a few seconds.".into()
    } else if l.contains("authenticator") {
        "The password was right; that computer wants the code of its authenticator app too.".into()
    } else if l.contains("wrong code") {
        "Wrong code from the authenticator app (the password was right).".into()
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

    /// Wrong passwords tried at the same time all count: no more than five get checked, and a
    /// remembered sign-in still works while guessing is held off.
    #[test]
    fn guesses_at_once_all_count() {
        let dir = std::env::temp_dir().join(format!("sshremote-web-guess-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pw = dir.join("pw");
        std::fs::write(&pw, "secret-word").unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let args: Vec<String> = ["--listen", &format!("127.0.0.1:{port}"), "--password-file", pw.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
        std::thread::spawn(move || run(&args));
        std::thread::sleep(Duration::from_millis(300));
        let addr = format!("127.0.0.1:{port}");
        // One sign-in, its answer as text.
        let try_once = move |first: String| -> String {
            let mut s = TcpStream::connect(&addr).unwrap();
            s.write_all(format!("GET /ws HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            while line != "\r\n" {
                line.clear();
                r.read_line(&mut line).unwrap();
            }
            s.write_all(&masked(true, 1, first.as_bytes())).unwrap();
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
            String::from_utf8_lossy(&d).into_owned()
        };
        let signed: serde_json::Value = serde_json::from_str(&try_once(r#"{"gateway":{"password":"secret-word","remember":true}}"#.into())).unwrap();
        let token = signed["signedIn"]["token"].as_str().unwrap().to_string();
        let tries: Vec<_> = (0..8)
            .map(|_| {
                let t = try_once.clone();
                std::thread::spawn(move || t(r#"{"gateway":{"password":"wrong"}}"#.into()))
            })
            .collect();
        let answers: Vec<String> = tries.into_iter().map(|t| t.join().unwrap()).collect();
        let checked = answers.iter().filter(|a| a.contains("Wrong password")).count();
        assert!(checked <= 5, "only five checked: {answers:?}");
        assert!(answers.iter().filter(|a| a.contains("Too many")).count() >= 3, "{answers:?}");
        assert!(try_once(r#"{"gateway":{"password":"secret-word"}}"#.into()).contains("Too many"), "held off, even the right one");
        let by_token = try_once(serde_json::json!({ "gateway": { "token": token } }).to_string());
        assert!(by_token.contains("signedIn"), "the owner's remembered sign-in still works: {by_token}");
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

    /// Changed parts become a few rectangles: neighbours merge, and many far apart are merged
    /// down to the most kept, all inside the picture.
    #[test]
    fn changed_parts_merge_into_few_rectangles() {
        assert_eq!(merge_areas(&[(0, 0, 32, 32), (32, 0, 32, 32), (64, 0, 32, 32)], 640, 480), vec![(0, 0, 96, 32)]);
        assert_eq!(merge_areas(&[(600, 450, 100, 100)], 640, 480), vec![(600, 450, 40, 30)], "cut to the picture");
        let spread: Vec<Area> = (0..40).map(|i| ((i % 8) * 80, (i / 8) * 90, 10, 10)).collect();
        let merged = merge_areas(&spread, 640, 480);
        assert!(merged.len() <= 12, "{}", merged.len());
        for (x, y, _, _) in &spread {
            assert!(merged.iter().any(|m| m.0 <= *x && *x < m.0 + m.2 && m.1 <= *y && *y < m.1 + m.3), "every part kept");
        }
    }

    /// A picture update carries the moves, then each area as a picture the browser can decode:
    /// few colours lossless, a photograph as JPEG.
    #[test]
    fn picture_updates_carry_moves_and_areas() {
        let mut fb = crate::session::Framebuffer::default();
        (fb.rgb, fb.w, fb.h) = (vec![0; 64 * 32 * 3], 64, 32);
        // A noisy right half, a plain left one.
        for (i, px) in fb.rgb.chunks_mut(3).enumerate() {
            if i % 64 >= 32 {
                px.copy_from_slice(&[(i * 37 % 251) as u8, (i * 91 % 241) as u8, (i * 53 % 239) as u8]);
            }
        }
        let m = crate::scroll::Move { x: 0, y: 4, w: 64, h: 20, dy: -3, dx: 0 };
        let b = picture_update(9, &fb, &[m], &[(0, 0, 32, 32), (32, 0, 32, 32)], false, 80).unwrap();
        assert_eq!(b[0], 4);
        assert_eq!(u32::from_le_bytes(b[1..5].try_into().unwrap()), 9);
        assert_eq!((u16::from_le_bytes([b[5], b[6]]), u16::from_le_bytes([b[7], b[8]])), (64, 32));
        assert_eq!(b[9], 1, "one move");
        assert_eq!(i16::from_le_bytes([b[18], b[19]]), -3);
        assert_eq!(b[20], 2, "two areas");
        let fmt = |at: usize| b[at + 8];
        let len = |at: usize| u32::from_le_bytes(b[at + 9..at + 13].try_into().unwrap()) as usize;
        let first = 21;
        let second = first + 13 + len(first);
        assert_eq!((fmt(first), fmt(second)), (0, 1), "plain as WebP, noise as JPEG");
        assert_eq!(second + 13 + len(second), b.len());
        let lossless = picture_update(1, &fb, &[], &[(32, 0, 32, 32)], true, 80).unwrap();
        assert_eq!(lossless[10 + 8], 0, "lossless when asked, even for a photograph");
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
        assert!(matches!(a(r#"{"host":{"act":3,"on":true}}"#), Some(Ask::Agent(ToAgent::Host { act: 3, on: true }))));
        assert!(a(r#"{"host":{"act":9}}"#).is_none());
        assert!(matches!(a(r#"{"quality":"fast"}"#), Some(Ask::Quality(WebQuality::Preset(1)))));
        assert!(matches!(a(r#"{"quality":"auto"}"#), Some(Ask::Quality(WebQuality::Auto))));
        assert!(matches!(a(r#"{"quality":"preset","level":0}"#), Some(Ask::Quality(WebQuality::Preset(0)))));
        assert!(matches!(a(r#"{"quality":"custom","mode":"mono","scale":5,"fps":200}"#), Some(Ask::Quality(WebQuality::Custom(crate::color::Mode::Mono, 10, 120)))));
        assert!(a(r#"{"quality":"custom","mode":"sepia","scale":50,"fps":10}"#).is_none(), "an unknown colour mode is refused");
        assert!(matches!(a(r#"{"drawn":7}"#), Some(Ask::Drawn(7))));
        assert!(matches!(a(r#"{"pause":true}"#), Some(Ask::Pause(true))));
        assert!(matches!(a(r#"{"snapshot":1}"#), Some(Ask::Agent(ToAgent::Snapshot))));
        use crate::shared::Share;
        assert!(matches!(a(r#"{"windows":1}"#), Some(Ask::Agent(ToAgent::ListWindows))));
        // Programs are opened in the app's new-open view only, never from the web page.
        assert!(a(r#"{"apps":1}"#).is_none() && a(r#"{"launch":{"kind":1,"text":"xterm"}}"#).is_none());
        assert!(matches!(a(r#"{"sound":true}"#), Some(Ask::Agent(ToAgent::Audio(true)))));
        assert!(matches!(a(r#"{"call":true}"#), Some(Ask::Agent(ToAgent::Call(true)))));
        assert!(matches!(a(r#"{"pasteFiles":{"batch":"web-1","names":["a.txt"]}}"#), Some(Ask::Agent(ToAgent::PasteFiles { names, .. })) if names == ["a.txt"]));
        assert!(matches!(ask(Frame::Binary(vec![7, 1, 2])), Some(Ask::Agent(ToAgent::Voice(v))) if v == [1, 2]));
        assert!(matches!(a(r#"{"share":{"kind":"window","id":"18446744073709551000"}}"#), Some(Ask::Agent(ToAgent::Share(Share::Window(18446744073709551000))))));
        assert!(matches!(a(r#"{"share":{"kind":"activeApp"}}"#), Some(Ask::Agent(ToAgent::Share(Share::ActiveApp)))));
        assert!(a(r#"{"share":{"kind":"window"}}"#).is_none(), "a window needs its id");
        assert!(matches!(a(r#"{"list":""}"#), Some(Ask::Agent(ToAgent::ListDir { .. }))));
        assert!(matches!(a(r#"{"resize":[100,30]}"#), Some(Ask::Agent(ToAgent::TermResize { cols: 100, rows: 30, .. }))));
        assert!(matches!(a(r#"{"upload":{"id":3,"size":9,"dir":"/t","name":"a"}}"#), Some(Ask::Upload { id: 3, size: 9, .. })));
        assert!(matches!(a(r#"{"move":[1,2]}"#), Some(Ask::Agent(ToAgent::Pointer { x: 1, y: 2 }))));
        assert!(matches!(a(r#"{"clipImageEnd":true}"#), Some(Ask::ClipEnd)));
        assert!(matches!(ask(Frame::Binary(vec![8, 0x89, b'P'])), Some(Ask::ClipPart(b)) if b == [0x89, b'P']));
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

        // The screen: a picture update once it shows, the whole of it at first.
        json(&mut s, serde_json::json!({ "show": "screen" }));
        let update = until(&mut r, "picture", &mut |op, d| op == 2 && d.first() == Some(&4));
        let (w, h) = (u16::from_le_bytes([update[5], update[6]]), u16::from_le_bytes([update[7], update[8]]));
        assert!(w > 0 && h > 0 && update.len() > 20, "{w}x{h}, {} bytes", update.len());
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
