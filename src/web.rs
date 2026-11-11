//! The web gateway: `sshremote web` serves a page to browsers and, over a WebSocket, joins it to
//! a shell on this computer or to `ssh HOST` for the hosts this computer knows (behind the
//! gateway's password), or to any computer's screen by its remote access ID and password, through
//! the relay. Put it behind HTTPS (nginx, Caddy) when it faces the internet.
//!
//! On the socket, text frames carry JSON (sign-in, host list, opening, resizing, the end) and
//! binary frames carry the terminal's bytes both ways.

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
    /// Failed sign-ins by address: how many, and when the last was.
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
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
    let gate = Arc::new(Gate { password, id_only, relay, failures: Mutex::new(HashMap::new()) });
    let server = TcpListener::bind(&listen).map_err(|e| format!("{listen}: {e}"))?;
    eprintln!("sshremote web: on http://{listen}/");
    for conn in server.incoming().flatten() {
        let gate = gate.clone();
        std::thread::spawn(move || {
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

fn read_request(r: &mut BufReader<TcpStream>) -> Result<Request, String> {
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/").to_string());
    if method != "GET" {
        return Err("only GET".into());
    }
    let mut headers = HashMap::new();
    for _ in 0..100 {
        line.clear();
        r.read_line(&mut line).map_err(|e| e.to_string())?;
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
            // On both handles: on Windows a clone keeps its own timeout.
            let _ = conn.set_read_timeout(None);
            let _ = r.get_ref().set_read_timeout(None);
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

/// The hosts a browser may open: this computer, and the hosts of this computer's ~/.ssh/config.
fn hosts() -> Vec<String> {
    let mut out = vec![HERE.to_string()];
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if let Some(text) = home.and_then(|h| std::fs::read_to_string(h.join(".ssh").join("config")).ok()) {
        for line in text.lines() {
            let mut w = line.split_whitespace();
            if w.next().is_some_and(|k| k.eq_ignore_ascii_case("host")) {
                out.extend(w.filter(|h| !h.contains(['*', '?', '!'])).map(str::to_string));
            }
        }
    }
    out.dedup();
    out
}

fn equal(a: &str, b: &str) -> bool {
    use sha2::Digest;
    let (x, y) = (sha2::Sha256::digest(a.as_bytes()), sha2::Sha256::digest(b.as_bytes()));
    x.iter().zip(y.iter()).fold(0u8, |acc, (p, q)| acc | (p ^ q)) == 0
}

fn session(mut r: BufReader<TcpStream>, w: TcpStream, gate: &Gate, who: IpAddr) -> Result<(), String> {
    let w = Arc::new(Mutex::new(w));
    // Signing in: five wrong tries from one address wait a quarter hour; each wrong one waits too.
    let Frame::Text(first) = read_frame(&mut r, &w)? else { return Err("expected a sign-in".into()) };
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
        let e = f.entry(who).or_insert((0, Instant::now()));
        *e = (if e.1.elapsed() > Duration::from_secs(900) { 1 } else { e.0 + 1 }, Instant::now());
        Ok(())
    };
    // A computer by its remote access ID: its own password is the sign-in, checked by the computer.
    if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
        let id: String = id.chars().filter(|c| c.is_ascii_digit()).collect();
        if blocked {
            return send_json(&w, serde_json::json!({ "error": "Too many failed tries; try again later." }));
        }
        if !crate::access::valid_id(&id) {
            return send_json(&w, serde_json::json!({ "error": "An ID is 9 digits." }));
        }
        let password = v.get("password").and_then(|p| p.as_str()).unwrap_or("").to_string();
        let addr = crate::net::Address { id: id.clone(), relay: gate.relay.clone() };
        let target = crate::transport::Target::Direct { addr, password };
        let size = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(1280).clamp(64, 7680) as u16;
        if screen(r, w, target, &crate::access::spaced(&id), (size("w"), size("h")))? {
            gate.failures.lock().map_err(|e| e.to_string())?.remove(&who);
        } else {
            failed()?;
        }
        return Ok(());
    }
    if gate.id_only {
        return send_json(&w, serde_json::json!({ "error": "This gateway opens computers by ID only." }));
    }
    if blocked || !equal(v.get("password").and_then(|p| p.as_str()).unwrap_or(""), &gate.password) {
        if !blocked {
            failed()?;
        }
        let text = if blocked { "Too many wrong passwords; try again later." } else { "Wrong password." };
        let _ = send_json(&w, serde_json::json!({ "error": text }));
        return Ok(());
    }
    gate.failures.lock().map_err(|e| e.to_string())?.remove(&who);
    let list = hosts();
    send_json(&w, serde_json::json!({ "hosts": list }))?;
    // Then which host, at what size.
    let Frame::Text(open) = read_frame(&mut r, &w)? else { return Err("expected a host".into()) };
    let v: serde_json::Value = serde_json::from_str(&open).unwrap_or_default();
    let host = v.get("open").and_then(|h| h.as_str()).unwrap_or("");
    if !list.iter().any(|h| h == host) {
        return send_json(&w, serde_json::json!({ "error": "That host is not offered here." }));
    }
    if v.get("screen").and_then(|x| x.as_bool()) == Some(true) {
        let size = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(1280).clamp(64, 7680) as u16;
        let target = if host == HERE {
            crate::transport::Target::Local { capture: "auto".into() }
        } else {
            crate::transport::Target::Ssh {
                host: host.to_string(),
                port: None,
                args: Vec::new(),
                remote: crate::options::REMOTE.into(),
                capture: "auto".into(),
                password: None,
                unattended: false,
            }
        };
        return screen(r, w, target, host, (size("w"), size("h"))).map(|_| ());
    }
    let size = |k: &str, d: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d).clamp(2, 500) as u16;
    let argv: Vec<String> = if host == HERE { Vec::new() } else { vec!["ssh".into(), "-tt".into(), host.to_string()] };
    let (wake_tx, wake) = mpsc::sync_channel::<()>(1);
    let term = pty_spawn(&argv, size("cols", 80), size("rows", 24), wake_tx)?;
    let term = Arc::new(Mutex::new(term));
    // Output to the browser as it comes.
    let (t2, w2) = (term.clone(), w.clone());
    let out = std::thread::spawn(move || {
        loop {
            let _ = wake.recv_timeout(Duration::from_millis(500));
            let (data, end) = match t2.lock() {
                Ok(mut t) => t.drain(),
                Err(_) => return,
            };
            if !data.is_empty() && send(&w2, 0x2, &data).is_err() {
                return;
            }
            if let Some(why) = end {
                let _ = send_json(&w2, serde_json::json!({ "end": why }));
                let _ = send(&w2, 0x8, &[]);
                if let Ok(s) = w2.lock() {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
                return;
            }
        }
    });
    // Input and resizes from the browser until it goes.
    loop {
        match read_frame(&mut r, &w) {
            Ok(Frame::Binary(data)) => term.lock().map_err(|e| e.to_string())?.write(&data),
            Ok(Frame::Text(t)) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                if let Some([c, r]) = v
                    .get("resize")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|n| n.as_u64()).collect::<Vec<_>>())
                    .as_deref()
                {
                    term.lock()
                        .map_err(|e| e.to_string())?
                        .resize((*c).clamp(2, 500) as u16, (*r).clamp(1, 500) as u16);
                }
            }
            Ok(Frame::Close) | Err(_) => break,
        }
    }
    if let Ok(s) = w.lock() {
        let _ = s.shutdown(std::net::Shutdown::Both);
    }
    // The terminal ends with the session (dropping it hangs up the shell).
    drop(term);
    let _ = out.join();
    Ok(())
}

/// What the browser sends while it shows a screen.
enum Input {
    Agent(crate::proto::ToAgent),
    View(u16, u16),
    Gone,
}

/// The host's screen in the browser: the session's frames kept here and sent on as JPEG pictures
/// (at most 12 a second, only when something changed); pointer, buttons, wheel and keys back.
/// False when the connection could not be made.
fn screen(
    mut r: BufReader<TcpStream>,
    w: Arc<Mutex<TcpStream>>,
    target: crate::transport::Target,
    host: &str,
    view: (u16, u16),
) -> Result<bool, String> {
    use crate::proto::{Settings, ToAgent, ToClient};
    use crate::session::{Ev, Session};
    let _ = send_json(&w, serde_json::json!({ "status": format!("Connecting to {host}\u{2026}") }));
    let (mut sess, _tx, rx) = match Session::open(&target, true) {
        Ok(x) => x,
        Err(e) => return send_json(&w, serde_json::json!({ "error": plain(&e) })).map(|_| false),
    };
    let settings = |(vw, vh): (u16, u16)| Settings { view_w: vw, view_h: vh, scale: 100, mode: crate::color::Mode::True, fps: 15 };
    sess.send(ToAgent::Settings(settings(view))).map_err(|e| e.to_string())?;
    send_json(&w, serde_json::json!({ "screen": [sess.desk.0, sess.desk.1] }))?;
    // The browser's input on its own thread, handed over here.
    let (in_tx, input) = mpsc::channel();
    let w_in = w.clone();
    std::thread::spawn(move || {
        loop {
            let msg = match read_frame(&mut r, &w_in) {
                Ok(Frame::Text(t)) => serde_json::from_str::<serde_json::Value>(&t).ok().and_then(|v| browser_input(&v)),
                Ok(Frame::Binary(_)) => None,
                Ok(Frame::Close) | Err(_) => Some(Input::Gone),
            };
            if let Some(m) = msg {
                let gone = matches!(m, Input::Gone);
                if in_tx.send(m).is_err() || gone {
                    return;
                }
            }
        }
    });
    let mut fb = crate::session::Framebuffer::default();
    let (mut dirty, mut sent) = (false, Instant::now() - Duration::from_secs(1));
    let end = loop {
        match rx.recv_timeout(Duration::from_millis(15)) {
            Ok(Ev::Net(ToClient::Frame(f))) => {
                fb.apply(&f).map_err(|e| e.to_string())?;
                sess.send(ToAgent::Ack(f.id)).map_err(|e| e.to_string())?;
                dirty = true;
            }
            Ok(Ev::Net(ToClient::Error(e))) | Ok(Ev::Closed(e)) => break e,
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break "the session ended".into(),
        }
        let mut gone = false;
        for m in input.try_iter() {
            match m {
                Input::Agent(a) => sess.send(a).map_err(|e| e.to_string())?,
                Input::View(vw, vh) => sess.send(ToAgent::Settings(settings((vw, vh)))).map_err(|e| e.to_string())?,
                Input::Gone => gone = true,
            }
        }
        if gone {
            break String::new();
        }
        if dirty && fb.w > 0 && sent.elapsed() >= Duration::from_millis(80) {
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 82)
                .encode(&fb.rgb, fb.w as u32, fb.h as u32, image::ExtendedColorType::Rgb8)
                .map_err(|e| e.to_string())?;
            if send(&w, 0x2, &jpeg).is_err() {
                break String::new();
            }
            (dirty, sent) = (false, Instant::now());
        }
    };
    sess.close();
    if !end.is_empty() {
        let _ = send_json(&w, serde_json::json!({ "end": end }));
    }
    if let Ok(s) = w.lock() {
        let _ = s.shutdown(std::net::Shutdown::Both);
    }
    Ok(true)
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

fn pty_spawn(argv: &[String], cols: u16, rows: u16, wake: mpsc::SyncSender<()>) -> Result<crate::pty::Local, String> {
    crate::pty::Local::spawn(
        argv,
        cols,
        rows,
        Box::new(move || {
            let _ = wake.try_send(());
        }),
    )
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
        assert!(js.starts_with("HTTP/1.1 200") && js.contains("showScreen") && !body.contains("<script>"));

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
        let put = |s: &mut TcpStream, op: u8, data: &[u8]| {
            let mut f = vec![0x80 | op, 0x80 | data.len() as u8, 1, 2, 3, 4];
            f.extend(data.iter().enumerate().map(|(i, b)| b ^ [1, 2, 3, 4][i % 4]));
            s.write_all(&f).unwrap();
        };
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
        let (mut s, mut r) = open();
        put(&mut s, 1, br#"{"id":"123 456 789","password":"x","w":800,"h":600}"#);
        let mut said = String::new();
        while !said.contains("error") {
            said = String::from_utf8(get(&mut r).1).unwrap();
        }

        let (mut s, mut r) = open();
        put(&mut s, 1, br#"{"password":"wrong"}"#);
        assert!(String::from_utf8(get(&mut r).1).unwrap().contains("Wrong password"));

        let (mut s, mut r) = open();
        put(&mut s, 1, br#"{"password":"secret-word"}"#);
        assert!(String::from_utf8(get(&mut r).1).unwrap().contains(HERE));
        put(&mut s, 1, format!(r#"{{"open":"{HERE}","cols":80,"rows":24}}"#).as_bytes());
        put(&mut s, 2, b"echo web-42\r");
        let mut seen = Vec::new();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && String::from_utf8_lossy(&seen).matches("web-42").count() < 2
        {
            let (op, d) = get(&mut r);
            if op == 2 {
                seen.extend(d);
            }
        }
        // Typed, then printed.
        assert!(String::from_utf8_lossy(&seen).matches("web-42").count() >= 2, "{}", String::from_utf8_lossy(&seen));
        put(&mut s, 8, b"");
    }
}
