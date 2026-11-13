//! A web page as a screen (`browser:URL`): an installed Chromium-family browser (Chrome, Edge,
//! Chromium) run with no window, its page streamed as pictures by DevTools' screencast and the
//! viewer's mouse and keys given to it, so a web app (a translator, a chat, a mail) opens in a
//! pane as a remote screen does, with no window of its own on the desktop. Each site keeps its own
//! profile (and so its sign-ins) under the settings folder.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Capture, Image, Input};
use crate::xk;

/// What the reader thread shares: replies awaited, the newest picture, why it ended.
struct Shared {
    replies: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
    frame: Mutex<Option<Image>>,
    error: Mutex<Option<String>>,
    /// A page our page opened (a link to a new tab): shown in its place once it has an address.
    opened: Mutex<Vec<String>>,
}

pub struct Browser {
    ws: Arc<Mutex<TcpStream>>,
    ids: Arc<AtomicU64>,
    shared: Arc<Shared>,
    session: String,
    target: String,
    size: (usize, usize),
    child: Option<std::process::Child>,
    /// What is held now: the pointer, its buttons (DevTools' mask), the modifier keys (its bits).
    at: (f64, f64),
    buttons: u32,
    mods: u32,
    /// The last press, to tell a double click: when, which button, where, and how many so far.
    last_press: Option<(Instant, u8, (f64, f64), u32)>,
}

/// The browser to run: SSHREMOTE_BROWSER, else Chrome, Edge or Chromium where they install.
fn find_browser() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SSHREMOTE_BROWSER").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    let mut list: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for base in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(b) = std::env::var_os(base).map(PathBuf::from) {
                list.push(b.join(r"Google\Chrome\Application\chrome.exe"));
                list.push(b.join(r"Microsoft\Edge\Application\msedge.exe"));
                list.push(b.join(r"Chromium\Application\chrome.exe"));
            }
        }
    } else {
        let path = std::env::var_os("PATH").unwrap_or_default();
        for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "microsoft-edge-stable"] {
            for dir in std::env::split_paths(&path) {
                list.push(dir.join(name));
            }
            list.push(PathBuf::from("/snap/bin").join(name));
        }
    }
    list.into_iter().find(|p| p.is_file())
}

/// The folder of a site's profile: its sign-ins and settings, apart from the user's own browser.
fn profile_dir(url: &str) -> Result<PathBuf, String> {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("site");
    let name: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let dir = crate::hosts::config_dir().ok_or("no settings folder")?.join("browser").join(if name.is_empty() { "site".into() } else { name });
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// The DevTools port and path of the browser running on `dir`, as it wrote them there.
fn active_port(dir: &std::path::Path) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(dir.join("DevToolsActivePort")).ok()?;
    let mut lines = text.lines();
    let port = lines.next()?.trim().parse().ok()?;
    Some((port, lines.next()?.trim().to_string()))
}

fn ws_connect(port: u16, path: &str) -> std::io::Result<TcpStream> {
    use base64::Engine;
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_nodelay(true)?;
    let key = base64::engine::general_purpose::STANDARD.encode(crate::sys::random(16));
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")?;
    // The answer's head, byte by byte: what follows it is the first frame.
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 || s.read(&mut b)? == 0 {
            return Err(std::io::Error::other("DevTools closed the connection"));
        }
        head.push(b[0]);
    }
    if !String::from_utf8_lossy(&head).lines().next().is_some_and(|l| l.contains(" 101")) {
        return Err(std::io::Error::other("DevTools refused the connection"));
    }
    Ok(s)
}

/// Sends one text message, masked as a client's must be.
fn ws_send(s: &mut TcpStream, text: &str) -> std::io::Result<()> {
    let data = text.as_bytes();
    let mut frame = vec![0x81u8];
    match data.len() {
        n if n < 126 => frame.push(0x80 | n as u8),
        n if n < 65536 => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    let mask = crate::sys::random(4);
    frame.extend_from_slice(&mask);
    frame.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    s.write_all(&frame)
}

/// The next whole text message; pings are answered on the way.
fn ws_read(r: &mut TcpStream, w: &Mutex<TcpStream>) -> std::io::Result<String> {
    let mut message = Vec::new();
    loop {
        let mut h = [0u8; 2];
        r.read_exact(&mut h)?;
        let (fin, op) = (h[0] & 0x80 != 0, h[0] & 0x0f);
        let mut len = (h[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            len = u64::from_be_bytes(b);
        }
        if len > 64 << 20 {
            return Err(std::io::Error::other("an oversized DevTools message"));
        }
        let mask = if h[1] & 0x80 != 0 {
            let mut m = [0u8; 4];
            r.read_exact(&mut m)?;
            Some(m)
        } else {
            None
        };
        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload)?;
        if let Some(m) = mask {
            payload.iter_mut().enumerate().for_each(|(i, b)| *b ^= m[i % 4]);
        }
        match op {
            0x8 => return Err(std::io::Error::other("the browser closed DevTools")),
            0x9 => {
                // A pong carries the ping's payload back.
                let mut s = w.lock().unwrap_or_else(|e| e.into_inner());
                let mut f = vec![0x8au8, 0x80 | payload.len().min(125) as u8];
                let m = crate::sys::random(4);
                f.extend_from_slice(&m);
                f.extend(payload.iter().take(125).enumerate().map(|(i, b)| b ^ m[i % 4]));
                s.write_all(&f)?;
            }
            0xa => {}
            _ => {
                message.extend_from_slice(&payload);
                if fin {
                    return Ok(String::from_utf8_lossy(&message).into_owned());
                }
            }
        }
    }
}

/// A JPEG frame as the screen's pixels: BGRx rows.
fn decode(data: &str) -> Option<Image> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let rgba = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let mut out = rgba.into_raw();
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some(Image { w, h, stride: w * 4, data: out })
}

impl Browser {
    pub fn open(url: &str) -> Result<Browser, String> {
        // An address with a scheme (https:, http:, data:, file:) as it is; a bare name is a site.
        let scheme = url.split_once(':').is_some_and(|(s, rest)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()) && !rest.starts_with(|c: char| c.is_ascii_digit()));
        let url = if scheme { url.to_string() } else { format!("https://{url}") };
        let dir = profile_dir(&url)?;
        // The browser already running on this profile (the same site in another pane) takes this
        // page too; else one starts.
        let mut child = None;
        let running = active_port(&dir).and_then(|(port, path)| ws_connect(port, &path).ok());
        let stream = match running {
            Some(s) => s,
            None => {
                let _ = std::fs::remove_file(dir.join("DevToolsActivePort"));
                let exe = find_browser().ok_or("this needs Chrome, Edge or Chromium installed (or SSHREMOTE_BROWSER naming one)")?;
                let mut c = std::process::Command::new(exe);
                c.arg("--headless=new")
                    .arg("--remote-debugging-port=0")
                    .arg(format!("--user-data-dir={}", dir.display()))
                    .args(["--no-first-run", "--no-default-browser-check", "--disable-background-networking", "--window-size=1280,800", "about:blank"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                crate::sys::quiet(&mut c);
                let p = c.spawn().map_err(|e| format!("the browser did not start: {e}"))?;
                crate::sys::tie(&p);
                child = Some(p);
                let started = Instant::now();
                loop {
                    if let Some((port, path)) = active_port(&dir)
                        && let Ok(s) = ws_connect(port, &path)
                    {
                        break s;
                    }
                    if started.elapsed() > Duration::from_secs(20) {
                        return Err("the browser did not open DevTools in time".into());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        let reader = stream.try_clone().map_err(|e| e.to_string())?;
        let ws = Arc::new(Mutex::new(stream));
        let shared = Arc::new(Shared { replies: Mutex::new(HashMap::new()), frame: Mutex::new(None), error: Mutex::new(None), opened: Mutex::new(Vec::new()) });
        let ids = Arc::new(AtomicU64::new(1));
        let mut b = Browser {
            ws: ws.clone(),
            ids: ids.clone(),
            shared: shared.clone(),
            session: String::new(),
            target: String::new(),
            size: (1280, 800),
            child,
            at: (0.0, 0.0),
            buttons: 0,
            mods: 0,
            last_press: None,
        };
        let session_cell = Arc::new(Mutex::new(String::new()));
        let target_cell = Arc::new(Mutex::new(String::new()));
        {
            let (shared, ws, ids, session_cell, target_cell) = (shared.clone(), ws.clone(), ids.clone(), session_cell.clone(), target_cell.clone());
            let mut reader = reader;
            std::thread::spawn(move || {
                let r = read_loop(&mut reader, &ws, &shared, &ids, &session_cell, &target_cell);
                let why = r.err().map_or_else(|| "the page closed".to_string(), |e| e.to_string());
                shared.error.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert(why);
                // Whoever waits for a reply hears the end too.
                shared.replies.lock().unwrap_or_else(|e| e.into_inner()).clear();
            });
        }
        let created = b.call("Target.createTarget", json!({ "url": url }), false)?;
        b.target = created["targetId"].as_str().unwrap_or_default().to_string();
        *target_cell.lock().unwrap_or_else(|e| e.into_inner()) = b.target.clone();
        let _ = b.call("Target.setDiscoverTargets", json!({ "discover": true }), false);
        let attached = b.call("Target.attachToTarget", json!({ "targetId": b.target, "flatten": true }), false)?;
        b.session = attached["sessionId"].as_str().unwrap_or_default().to_string();
        *session_cell.lock().unwrap_or_else(|e| e.into_inner()) = b.session.clone();
        b.call("Page.enable", json!({}), true)?;
        b.resize(1280, 800)?;
        Ok(b)
    }

    /// A command, waited for: its result, or the browser's error.
    fn call(&self, method: &str, params: Value, on_page: bool) -> Result<Value, String> {
        let id = self.ids.fetch_add(1, Ordering::AcqRel);
        let (tx, rx) = mpsc::channel();
        self.shared.replies.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        self.post(id, method, params, on_page).map_err(|e| e.to_string())?;
        let v = rx.recv_timeout(Duration::from_secs(15)).map_err(|_| self.ended().unwrap_or_else(|| format!("the browser did not answer {method}")))?;
        match v.get("error") {
            Some(e) => Err(e["message"].as_str().unwrap_or("the browser refused it").to_string()),
            None => Ok(v["result"].clone()),
        }
    }

    /// A command not waited for (input, as it comes).
    fn tell(&self, method: &str, params: Value) -> Result<(), String> {
        let id = self.ids.fetch_add(1, Ordering::AcqRel);
        self.post(id, method, params, true).map_err(|e| e.to_string())
    }

    fn post(&self, id: u64, method: &str, params: Value, on_page: bool) -> std::io::Result<()> {
        let mut m = json!({ "id": id, "method": method, "params": params });
        if on_page {
            m["sessionId"] = json!(self.session);
        }
        if std::env::var_os("SSHREMOTE_CDP_DEBUG").is_some() {
            eprintln!("cdp> {}", &m.to_string()[..m.to_string().len().min(300)]);
        }
        ws_send(&mut self.ws.lock().unwrap_or_else(|e| e.into_inner()), &m.to_string())
    }

    fn ended(&self) -> Option<String> {
        self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The page laid out at this size, and its pictures sent at it.
    fn resize(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.size = (w, h);
        self.call("Emulation.setDeviceMetricsOverride", json!({ "width": w, "height": h, "deviceScaleFactor": 1, "mobile": false }), true)?;
        let _ = self.call("Page.stopScreencast", json!({}), true);
        self.call("Page.startScreencast", json!({ "format": "jpeg", "quality": 92, "maxWidth": w, "maxHeight": h, "everyNthFrame": 1 }), true)?;
        Ok(())
    }

    fn mouse(&self, kind: &str, button: &str, clicks: u32) -> Result<(), String> {
        self.tell(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": self.at.0, "y": self.at.1, "button": button, "buttons": self.buttons, "clickCount": clicks, "modifiers": self.mods }),
        )
    }

    /// A browser's own shortcuts, which a page does not get: Alt+Left and Alt+Right go back and
    /// forward, F5 reloads. Whether it was one.
    fn shortcut(&self, keysym: u32, mods: u32) -> Result<bool, String> {
        let script = match (keysym, mods) {
            (xk::LEFT, 1) => "history.back()",
            (xk::RIGHT, 1) => "history.forward()",
            (k, 0) if k == xk::F1 + 4 => {
                self.tell("Page.reload", json!({}))?;
                return Ok(true);
            }
            _ => return Ok(false),
        };
        self.tell("Runtime.evaluate", json!({ "expression": script }))?;
        Ok(true)
    }

    /// A key down or up: a character's (typed, or with Ctrl a shortcut), or a named key's.
    fn key(&self, keysym: u32, down: bool, mods: u32) -> Result<(), String> {
        if down && self.shortcut(keysym, mods)? {
            return Ok(());
        }
        if !down && matches!((keysym, mods), (xk::LEFT | xk::RIGHT, 1)) || (!down && keysym == xk::F1 + 4 && mods == 0) {
            return Ok(());
        }
        let ch = if xk::is_unicode(keysym) { char::from_u32(keysym & 0x00ff_ffff) } else if (0x20..0x100).contains(&keysym) { char::from_u32(keysym) } else { None };
        let kind = if down { "keyDown" } else { "keyUp" };
        if let Some(c) = ch {
            // The viewer sends keys as they are on the keyboard; Shift makes the character, as a
            // US layout does (an input method's characters come whole).
            let c = if mods & 8 != 0 && mods & (1 | 2 | 4) == 0 { shifted(c) } else { c };
            let upper = c.to_ascii_uppercase();
            let vk = if upper.is_ascii_alphanumeric() { upper as u32 } else if c == ' ' { 32 } else { 0 };
            let code = if upper.is_ascii_alphabetic() { format!("Key{upper}") } else if c.is_ascii_digit() { format!("Digit{c}") } else if c == ' ' { "Space".into() } else { String::new() };
            // With Ctrl, Alt or Meta held a letter is a shortcut, not text.
            let typing = mods & (1 | 2 | 4) == 0;
            // Pasting brings this computer's clipboard into the page (the page's own is apart).
            if down && mods & 2 != 0 && mods & 1 == 0 && upper == 'V' {
                if let Some(text) = arboard::Clipboard::new().ok().and_then(|mut b| b.get_text().ok()).filter(|t| !t.is_empty()) {
                    return self.tell("Input.insertText", json!({ "text": text }));
                }
                return Ok(());
            }
            let mut p = json!({ "type": kind, "key": c.to_string(), "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk, "modifiers": mods });
            if down && typing {
                p["text"] = json!(c.to_string());
                p["unmodifiedText"] = json!(c.to_string());
            } else if down {
                p["type"] = json!("rawKeyDown");
                // Editing shortcuts, as the browser's own menus run them on every system.
                let command = match (upper, mods & 8 != 0) {
                    ('A', false) => Some("selectAll"),
                    ('C', false) => Some("copy"),
                    ('X', false) => Some("cut"),
                    ('Z', false) => Some("undo"),
                    ('Z', true) | ('Y', false) => Some("redo"),
                    _ => None,
                };
                if mods & 2 != 0
                    && let Some(cmd) = command
                {
                    p["commands"] = json!([cmd]);
                    // What is copied goes on this computer's clipboard too.
                    if matches!(cmd, "copy" | "cut")
                        && let Ok(v) = self.call(
                            "Runtime.evaluate",
                            json!({ "expression": "(() => { const e = document.activeElement; if (e && (e.tagName === 'INPUT' || e.tagName === 'TEXTAREA')) return e.value.substring(e.selectionStart, e.selectionEnd); return String(getSelection()); })()", "returnByValue": true }),
                            true,
                        )
                        && let Some(text) = v["result"]["value"].as_str().filter(|t| !t.is_empty())
                        && let Ok(mut b) = arboard::Clipboard::new()
                    {
                        let _ = b.set_text(text.to_string());
                    }
                }
            }
            return self.tell("Input.dispatchKeyEvent", p);
        }
        let (key, vk, text): (&str, u32, &str) = match keysym {
            xk::BACKSPACE => ("Backspace", 8, ""),
            xk::TAB => ("Tab", 9, ""),
            xk::RETURN => ("Enter", 13, "\r"),
            xk::ESCAPE => ("Escape", 27, ""),
            xk::DELETE => ("Delete", 46, ""),
            xk::INSERT => ("Insert", 45, ""),
            xk::HOME => ("Home", 36, ""),
            xk::END => ("End", 35, ""),
            xk::PAGE_UP => ("PageUp", 33, ""),
            xk::PAGE_DOWN => ("PageDown", 34, ""),
            xk::LEFT => ("ArrowLeft", 37, ""),
            xk::UP => ("ArrowUp", 38, ""),
            xk::RIGHT => ("ArrowRight", 39, ""),
            xk::DOWN => ("ArrowDown", 40, ""),
            k if (xk::F1..xk::F1 + 12).contains(&k) => {
                let n = k - xk::F1 + 1;
                let name = format!("F{n}");
                return self.tell("Input.dispatchKeyEvent", json!({ "type": if down { "rawKeyDown" } else { "keyUp" }, "key": name, "code": name, "windowsVirtualKeyCode": 111 + n, "modifiers": mods }));
            }
            _ => return Ok(()),
        };
        let mut p = json!({ "type": if down { "rawKeyDown" } else { "keyUp" }, "key": key, "code": key, "windowsVirtualKeyCode": vk, "modifiers": mods });
        if down && !text.is_empty() && mods & (2 | 4) == 0 {
            p["type"] = json!("keyDown");
            p["text"] = json!(text);
        }
        self.tell("Input.dispatchKeyEvent", p)
    }
}

/// A key's character with Shift held, on a US keyboard.
fn shifted(c: char) -> char {
    const PAIRS: &[(char, char)] = &[
        ('1', '!'), ('2', '@'), ('3', '#'), ('4', '$'), ('5', '%'), ('6', '^'), ('7', '&'), ('8', '*'), ('9', '('), ('0', ')'),
        ('-', '_'), ('=', '+'), ('[', '{'), (']', '}'), ('\\', '|'), (';', ':'), ('\'', '"'), (',', '<'), ('.', '>'), ('/', '?'), ('`', '~'),
    ];
    if c.is_ascii_lowercase() {
        return c.to_ascii_uppercase();
    }
    PAIRS.iter().find(|(a, _)| *a == c).map_or(c, |(_, b)| *b)
}

/// DevTools' bit for a modifier key, and its name there.
fn modifier(keysym: u32) -> Option<(u32, &'static str, u32)> {
    Some(match keysym {
        xk::ALT_L | xk::ALT_R => (1, "Alt", 18),
        xk::CONTROL_L | xk::CONTROL_R => (2, "Control", 17),
        xk::SUPER_L | xk::SUPER_R => (4, "Meta", 91),
        xk::SHIFT_L | xk::SHIFT_R => (8, "Shift", 16),
        _ => return None,
    })
}

/// The viewer's modifier bits (`proto::MOD_*`) as DevTools' (Alt 1, Ctrl 2, Meta 4, Shift 8).
fn devtools_mods(m: u8) -> u32 {
    use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
    (if m & MOD_ALT != 0 { 1 } else { 0 }) | (if m & MOD_CTRL != 0 { 2 } else { 0 }) | (if m & MOD_SUPER != 0 { 4 } else { 0 }) | (if m & MOD_SHIFT != 0 { 8 } else { 0 })
}

/// DevTools' messages, as they come: replies to whoever waits, frames kept (and acknowledged, so
/// the next is sent), pages our page opens shown in its place.
fn read_loop(r: &mut TcpStream, ws: &Mutex<TcpStream>, shared: &Shared, ids: &AtomicU64, session: &Mutex<String>, target: &Mutex<String>) -> std::io::Result<()> {
    loop {
        let text = ws_read(r, ws)?;
        if std::env::var_os("SSHREMOTE_CDP_DEBUG").is_some() {
            eprintln!("cdp< {}", &text[..text.len().min(300)]);
        }
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            if let Some(tx) = shared.replies.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
                let _ = tx.send(v);
            }
            continue;
        }
        let ours = session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let p = &v["params"];
        match v["method"].as_str().unwrap_or("") {
            "Page.screencastFrame" if v["sessionId"].as_str() == Some(ours.as_str()) => {
                let ack = json!({ "id": ids.fetch_add(1, Ordering::AcqRel), "method": "Page.screencastFrameAck", "params": { "sessionId": p["sessionId"] }, "sessionId": ours });
                ws_send(&mut ws.lock().unwrap_or_else(|e| e.into_inner()), &ack.to_string())?;
                if let Some(img) = p["data"].as_str().and_then(decode) {
                    *shared.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(img);
                }
            }
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                let info = &p["targetInfo"];
                let mine = target.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let (id, url) = (info["targetId"].as_str().unwrap_or(""), info["url"].as_str().unwrap_or(""));
                if info["type"] == "page" && info["openerId"].as_str() == Some(mine.as_str()) && id != mine && !url.is_empty() && url != "about:blank" {
                    // A link that opens a new tab: the page goes there instead, and the tab closes.
                    let navigate = json!({ "id": ids.fetch_add(1, Ordering::AcqRel), "method": "Page.navigate", "params": { "url": url }, "sessionId": ours });
                    let close = json!({ "id": ids.fetch_add(1, Ordering::AcqRel), "method": "Target.closeTarget", "params": { "targetId": id } });
                    let mut s = ws.lock().unwrap_or_else(|e| e.into_inner());
                    ws_send(&mut s, &navigate.to_string())?;
                    ws_send(&mut s, &close.to_string())?;
                    shared.opened.lock().unwrap_or_else(|e| e.into_inner()).push(url.to_string());
                }
            }
            "Target.targetDestroyed" if p["targetId"].as_str() == Some(target.lock().unwrap_or_else(|e| e.into_inner()).as_str()) => {
                return Err(std::io::Error::other("the page was closed"));
            }
            "Target.targetCrashed" if p["targetId"].as_str() == Some(target.lock().unwrap_or_else(|e| e.into_inner()).as_str()) => {
                return Err(std::io::Error::other("the page crashed"));
            }
            "Inspector.detached" => return Err(std::io::Error::other("the browser let go of the page")),
            _ => {}
        }
    }
}

impl Capture for Browser {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.size
    }

    fn resizable(&self) -> bool {
        true
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        let (w, h) = (w.clamp(200, 3840), h.clamp(150, 2160));
        if (w, h) != self.size {
            self.resize(w, h)?;
        }
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        if let Some(e) = self.ended() {
            return Err(e);
        }
        Ok(self.shared.frame.lock().unwrap_or_else(|e| e.into_inner()).take())
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        if let Some(e) = self.ended() {
            return Err(e);
        }
        match ev {
            Input::Move { x, y } => {
                self.at = (x, y);
                self.mouse("mouseMoved", "none", 0)
            }
            Input::Button { button, pressed } => {
                let (name, bit) = match button {
                    1 => ("left", 1),
                    2 => ("middle", 4),
                    3 => ("right", 2),
                    8 => ("back", 8),
                    9 => ("forward", 16),
                    _ => return Ok(()),
                };
                if pressed {
                    let now = Instant::now();
                    let clicks = match self.last_press {
                        Some((t, b, at, n)) if b == button && now - t < Duration::from_millis(500) && (at.0 - self.at.0).abs() < 5.0 && (at.1 - self.at.1).abs() < 5.0 => n + 1,
                        _ => 1,
                    };
                    self.last_press = Some((now, button, self.at, clicks));
                    self.buttons |= bit;
                    self.mouse("mousePressed", name, clicks)
                } else {
                    self.buttons &= !bit;
                    let clicks = self.last_press.map_or(1, |p| p.3);
                    self.mouse("mouseReleased", name, clicks)
                }
            }
            Input::Scroll { steps } | Input::HScroll { steps } => {
                let horizontal = matches!(ev, Input::HScroll { .. });
                let d = steps as f64 * 100.0;
                self.tell(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseWheel", "x": self.at.0, "y": self.at.1, "deltaX": if horizontal { d } else { 0.0 }, "deltaY": if horizontal { 0.0 } else { d }, "modifiers": self.mods }),
                )
            }
            Input::KeyState { keysym, down } => {
                if let Some((bit, name, vk)) = modifier(keysym) {
                    if down { self.mods |= bit } else { self.mods &= !bit }
                    return self.tell("Input.dispatchKeyEvent", json!({ "type": if down { "rawKeyDown" } else { "keyUp" }, "key": name, "code": name, "windowsVirtualKeyCode": vk, "modifiers": self.mods }));
                }
                self.key(keysym, down, self.mods)
            }
            Input::Key { keysym, mods } => {
                let mods = self.mods | devtools_mods(mods);
                self.key(keysym, true, mods)?;
                self.key(keysym, false, mods)
            }
            Input::SecureAttention => Ok(()),
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // This page goes; the browser with it when it started it (another pane's stays).
        let _ = self.call("Target.closeTarget", json!({ "targetId": self.target }), false);
        if let Some(mut c) = self.child.take() {
            let _ = self.call("Browser.close", json!({}), false);
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && c.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page in a real browser (Chrome, Edge or Chromium installed): it draws, takes typing and
    /// follows a resize. cargo test -- --ignored a_web_page_is_a_screen
    #[test]
    #[ignore]
    fn a_web_page_is_a_screen() {
        let page = "data:text/html,<input id=i autofocus style='font-size:40px'><script>document.title='ready'</script>";
        let mut b = Browser::open(page).expect("a browser");
        let started = Instant::now();
        let mut got = None;
        while got.is_none() && started.elapsed() < Duration::from_secs(15) {
            got = b.grab().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
        let img = got.expect("a picture of the page");
        assert_eq!((img.w, img.h), (1280, 800));
        // Letters, and what an input method gives (Korean) as characters.
        for c in "hi 한글".chars() {
            b.input(Input::Key { keysym: xk::of_char(c), mods: 0 }).unwrap();
        }
        let typed = b.call("Runtime.evaluate", json!({ "expression": "document.getElementById('i').value", "returnByValue": true }), true).unwrap();
        assert_eq!(typed["result"]["value"], "hi 한글");
        // Shift held makes capitals and the symbols over the digits.
        b.input(Input::Key { keysym: 'a' as u32, mods: crate::proto::MOD_CTRL }).unwrap();
        b.input(Input::KeyState { keysym: xk::SHIFT_L, down: true }).unwrap();
        for c in "h1".chars() {
            b.input(Input::KeyState { keysym: xk::of_char(c), down: true }).unwrap();
            b.input(Input::KeyState { keysym: xk::of_char(c), down: false }).unwrap();
        }
        b.input(Input::KeyState { keysym: xk::SHIFT_L, down: false }).unwrap();
        let typed = b.call("Runtime.evaluate", json!({ "expression": "document.getElementById('i').value", "returnByValue": true }), true).unwrap();
        assert_eq!(typed["result"]["value"], "H!");
        // Ctrl+A then Backspace: a shortcut, then a named key.
        b.input(Input::Key { keysym: 'a' as u32, mods: crate::proto::MOD_CTRL }).unwrap();
        b.input(Input::Key { keysym: xk::BACKSPACE, mods: 0 }).unwrap();
        let typed = b.call("Runtime.evaluate", json!({ "expression": "document.getElementById('i').value", "returnByValue": true }), true).unwrap();
        assert_eq!(typed["result"]["value"], "");
        b.set_target(800, 600).unwrap();
        let started = Instant::now();
        let mut resized = None;
        while started.elapsed() < Duration::from_secs(10) {
            if let Some(i) = b.grab().unwrap()
                && (i.w, i.h) == (800, 600)
            {
                resized = Some(i);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(resized.is_some(), "a picture at the new size");
        // A tall page scrolls under the wheel, and a click lands where it is.
        b.call("Runtime.evaluate", json!({ "expression": "document.body.style.height='5000px'; document.onclick = e => document.title = 'clicked ' + e.clientX + ' ' + e.clientY;" }), true).unwrap();
        b.input(Input::Move { x: 300.0, y: 300.0 }).unwrap();
        b.input(Input::Scroll { steps: 3 }).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let y = b.call("Runtime.evaluate", json!({ "expression": "window.scrollY", "returnByValue": true }), true).unwrap();
        assert!(y["result"]["value"].as_f64().unwrap_or(0.0) > 100.0, "scrolled down: {y}");
        b.input(Input::Button { button: 1, pressed: true }).unwrap();
        b.input(Input::Button { button: 1, pressed: false }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let t = b.call("Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true }), true).unwrap();
        assert_eq!(t["result"]["value"], "clicked 300 300");
    }
}
