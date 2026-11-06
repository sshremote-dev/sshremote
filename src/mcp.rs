//! `desk mcp HOST`: a Model Context Protocol server on stdio that lets an AI
//! agent see and operate a remote desktop over the same ssh (or desk://) link.
//!
//! Built to be cheap in tokens: the picture stream is paused and a screenshot is
//! taken only when asked, scaled to `max_width` (1024 by default), optionally a
//! region and in grey. Coordinates given to the action tools are in the last
//! screenshot's pixels and are mapped back to the screen here, so the model
//! never has to rescale anything.

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::color::Mode;
use crate::proto::{Settings, ToAgent, ToClient};
use crate::session::{Ev, Options, Session};

/// How the last screenshot maps to the screen: screen = origin + image / scale.
#[derive(Clone, Copy)]
struct View {
    ox: f64,
    oy: f64,
    scale: f64,
}

struct Server {
    sess: Session,
    rx: mpsc::Receiver<Ev>,
    screen: (u32, u32),
    view: View,
    clipboard: String,
    monitors: Vec<crate::proto::MonitorInfo>,
    windows: Option<Result<Vec<crate::capture::WindowInfo>, String>>,
    sharing: Option<String>,
}

fn tools() -> Value {
    let xy = json!({"x": {"type": "integer"}, "y": {"type": "integer"}});
    let props = |extra: Value| {
        let mut p = xy.clone();
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        p
    };
    json!([
        {"name": "screenshot",
         "description": "Capture the remote screen as a PNG. Coordinates for the other tools refer to the pixels of the most recent screenshot. Keep max_width small and use region to zoom in; that saves tokens.",
         "inputSchema": {"type": "object", "properties": {
            "max_width": {"type": "integer", "description": "Scale down to at most this width (default 1024, 0 = full size)."},
            "region": {"type": "array", "items": {"type": "integer"}, "minItems": 4, "maxItems": 4,
                       "description": "[x, y, width, height] of the area to capture, in coordinates of the previous screenshot."},
            "grey": {"type": "boolean", "description": "Greyscale, enough for reading text."}}}},
        {"name": "click", "description": "Move the pointer to (x, y) and click.",
         "inputSchema": {"type": "object", "required": ["x", "y"], "properties": props(json!({
            "button": {"type": "string", "enum": ["left", "right", "middle"]},
            "double": {"type": "boolean"}}))}},
        {"name": "move", "description": "Move the pointer to (x, y) without clicking (for hover menus).",
         "inputSchema": {"type": "object", "required": ["x", "y"], "properties": xy}},
        {"name": "drag", "description": "Press the left button at (x, y), move to (to_x, to_y) and release.",
         "inputSchema": {"type": "object", "required": ["x", "y", "to_x", "to_y"], "properties": props(json!({
            "to_x": {"type": "integer"}, "to_y": {"type": "integer"}}))}},
        {"name": "type", "description": "Type text (any language) into the focused field.",
         "inputSchema": {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}}},
        {"name": "key", "description": "Press a key or combination, e.g. \"enter\", \"ctrl+c\", \"alt+tab\", \"win+r\", \"ctrl+shift+esc\", \"f5\".",
         "inputSchema": {"type": "object", "required": ["keys"], "properties": {"keys": {"type": "string"}}}},
        {"name": "scroll", "description": "Scroll at (x, y): dy > 0 scrolls down, dx > 0 right, in wheel steps.",
         "inputSchema": {"type": "object", "required": ["x", "y"], "properties": props(json!({
            "dy": {"type": "integer"}, "dx": {"type": "integer"}}))}},
        {"name": "wait", "description": "Wait for the screen to settle, in milliseconds (at most 10000).",
         "inputSchema": {"type": "object", "required": ["ms"], "properties": {"ms": {"type": "integer"}}}},
        {"name": "monitors", "description": "List the remote monitors and which one is shown.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "select_monitor", "description": "Show another monitor (index from monitors).",
         "inputSchema": {"type": "object", "required": ["index"], "properties": {"index": {"type": "integer"}}}},
        {"name": "windows", "description": "List the remote windows that can be shared on their own (id, program, title, size).",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "share", "description": "Limit the picture, clicks and keys to one window or program, which makes screenshots smaller and cheaper. target: \"screen\", \"active\" (the focused window), \"active-app\" (every window of its program), a window id from windows, or \"app:<window id>\" for every window of that program.",
         "inputSchema": {"type": "object", "required": ["target"], "properties": {"target": {"type": "string"}}}},
        {"name": "clipboard_get", "description": "Text last copied on the remote host.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "clipboard_set", "description": "Put text on the remote clipboard.",
         "inputSchema": {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}}}
    ])
}

/// Keysym for a key name in a combination such as "ctrl+shift+t".
fn key_name(k: &str) -> Option<u32> {
    let l = k.to_ascii_lowercase();
    Some(match l.as_str() {
        "ctrl" | "control" => 0xffe3,
        "shift" => 0xffe1,
        "alt" | "option" => 0xffe9,
        "win" | "super" | "meta" | "cmd" | "windows" => 0xffeb,
        "enter" | "return" => 0xff0d,
        "tab" => 0xff09,
        "esc" | "escape" => 0xff1b,
        "backspace" => 0xff08,
        "delete" | "del" => 0xffff,
        "insert" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" | "page_up" => 0xff55,
        "pagedown" | "page_down" => 0xff56,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "space" => ' ' as u32,
        "printscreen" | "print" => 0xff61,
        "capslock" => 0xffe5,
        "menu" | "apps" => 0xff67,
        "plus" => '+' as u32,
        "minus" => '-' as u32,
        _ => {
            if let Some(n) = l.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=12).contains(n)) {
                return Some(0xffbd + n);
            }
            let mut it = l.chars();
            let c = it.next()?;
            if it.next().is_some() {
                return None;
            }
            c as u32
        }
    })
}

fn text_result(t: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": t.into()}]})
}

fn error_result(t: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": t.into()}], "isError": true})
}

impl Server {
    fn send(&mut self, m: ToAgent) -> Result<(), String> {
        self.sess.send(m).map_err(|e| format!("connection lost: {e}"))
    }

    /// Handles queued messages; frames are only acknowledged, the stream being paused.
    fn drain(&mut self, until: Option<Instant>) -> Result<Option<Vec<u8>>, String> {
        loop {
            let ev = match until {
                Some(t) => match self.rx.recv_timeout(t.saturating_duration_since(Instant::now())) {
                    Ok(e) => e,
                    Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                    Err(_) => return Err("connection closed".into()),
                },
                None => match self.rx.try_recv() {
                    Ok(e) => e,
                    Err(_) => return Ok(None),
                },
            };
            match ev {
                Ev::Net(ToClient::Frame(f)) => self.send(ToAgent::Ack(f.id))?,
                Ev::Net(ToClient::Snapshot(png)) => return Ok(Some(png)),
                Ev::Net(ToClient::Clipboard(t)) => self.clipboard = t,
                Ev::Net(ToClient::Monitors { list, .. }) => self.monitors = list,
                Ev::Net(ToClient::WindowList { list, error }) => {
                    self.windows = Some(if error.is_empty() { Ok(list) } else { Err(error) });
                }
                Ev::Net(ToClient::Sharing { text, .. }) => self.sharing = Some(text),
                Ev::Net(ToClient::Hello { w, h, .. }) => self.screen = (w as u32, h as u32),
                Ev::Net(ToClient::Notice(t)) if until.is_some() => return Err(t),
                Ev::Net(ToClient::Error(e)) | Ev::Closed(e) => return Err(e),
                _ => {}
            }
        }
    }

    fn point(&self, args: &Value, kx: &str, ky: &str) -> Result<(u16, u16), String> {
        let get = |k: &str| args.get(k).and_then(Value::as_f64).ok_or(format!("missing {k}"));
        let (x, y) = (get(kx)?, get(ky)?);
        let sx = (self.view.ox + x / self.view.scale).clamp(0.0, self.screen.0 as f64 - 1.0);
        let sy = (self.view.oy + y / self.view.scale).clamp(0.0, self.screen.1 as f64 - 1.0);
        Ok((sx.round() as u16, sy.round() as u16))
    }

    fn pointer(&mut self, p: (u16, u16)) -> Result<(), String> {
        self.send(ToAgent::Pointer { x: p.0, y: p.1 })
    }

    fn screenshot(&mut self, args: &Value) -> Result<Value, String> {
        self.drain(None)?;
        self.send(ToAgent::Snapshot)?;
        let png = self.drain(Some(Instant::now() + Duration::from_secs(15)))?.ok_or("no screenshot within 15 s")?;
        let (w, h, rgb) = crate::shot::rgb_from_png(&png)?;
        self.screen = (w as u32, h as u32);
        // A region is given in the previous screenshot's coordinates.
        let (ox, oy, w, h, rgb) = match args.get("region").and_then(Value::as_array) {
            Some(r) if r.len() == 4 => {
                let v: Vec<f64> = r.iter().map(|n| n.as_f64().unwrap_or(0.0)).collect();
                let (x0, y0) = (self.view.ox + v[0] / self.view.scale, self.view.oy + v[1] / self.view.scale);
                let (cw, ch) = (v[2] / self.view.scale, v[3] / self.view.scale);
                let (cw2, ch2, px) =
                    crate::shot::crop(w, h, &rgb, (x0 as usize, y0 as usize, cw as usize, ch as usize));
                (x0.max(0.0).min(w as f64 - 1.0).floor(), y0.max(0.0).min(h as f64 - 1.0).floor(), cw2, ch2, px)
            }
            _ => (0.0, 0.0, w, h, rgb),
        };
        let max_w = args.get("max_width").and_then(Value::as_u64).unwrap_or(1024) as usize;
        let (sw, sh, small) = crate::shot::shrink(w, h, &rgb, max_w);
        let grey = args.get("grey").and_then(Value::as_bool).unwrap_or(false);
        let out = crate::shot::png_from_rgb(sw, sh, &small, grey)?;
        self.view = View { ox, oy, scale: sw as f64 / w as f64 };
        let note = format!(
            "Screen {}x{}; this image is {sw}x{sh} showing x {ox:.0}..{:.0}, y {oy:.0}..{:.0}. Use this image's coordinates.",
            self.screen.0,
            self.screen.1,
            ox + w as f64,
            oy + h as f64
        );
        Ok(json!({"content": [
            {"type": "image", "data": crate::shot::base64(&out), "mimeType": "image/png"},
            {"type": "text", "text": note}
        ]}))
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        let short = Duration::from_millis(60);
        match name {
            "screenshot" => self.screenshot(args),
            "click" => {
                let p = self.point(args, "x", "y")?;
                let b = match args.get("button").and_then(Value::as_str).unwrap_or("left") {
                    "right" => 3,
                    "middle" => 2,
                    _ => 1,
                };
                self.pointer(p)?;
                let times = if args.get("double").and_then(Value::as_bool).unwrap_or(false) { 2 } else { 1 };
                for _ in 0..times {
                    self.send(ToAgent::Button { button: b, pressed: true })?;
                    self.send(ToAgent::Button { button: b, pressed: false })?;
                }
                Ok(text_result(format!("clicked at screen {},{}", p.0, p.1)))
            }
            "move" => {
                let p = self.point(args, "x", "y")?;
                self.pointer(p)?;
                Ok(text_result("moved"))
            }
            "drag" => {
                let (a, b) = (self.point(args, "x", "y")?, self.point(args, "to_x", "to_y")?);
                self.pointer(a)?;
                self.send(ToAgent::Button { button: 1, pressed: true })?;
                std::thread::sleep(short);
                // Intermediate points so drag-and-drop targets notice the motion.
                for i in 1..=8 {
                    let t = i as f64 / 8.0;
                    let x = a.0 as f64 + (b.0 as f64 - a.0 as f64) * t;
                    let y = a.1 as f64 + (b.1 as f64 - a.1 as f64) * t;
                    self.pointer((x as u16, y as u16))?;
                    std::thread::sleep(Duration::from_millis(15));
                }
                self.send(ToAgent::Button { button: 1, pressed: false })?;
                Ok(text_result("dragged"))
            }
            "type" => {
                let text = args.get("text").and_then(Value::as_str).ok_or("missing text")?;
                for c in text.chars() {
                    let sym = match c {
                        '\n' => 0xff0d,
                        '\t' => 0xff09,
                        c if (c as u32) < 0x100 => c as u32,
                        c => 0x0100_0000 | c as u32,
                    };
                    self.send(ToAgent::Key { keysym: sym, mods: 0 })?;
                }
                Ok(text_result(format!("typed {} characters", text.chars().count())))
            }
            "key" => {
                let combo = args.get("keys").and_then(Value::as_str).ok_or("missing keys")?;
                let syms: Vec<u32> = combo
                    .split('+')
                    .map(|k| key_name(k.trim()).ok_or(format!("unknown key {k:?}")))
                    .collect::<Result<_, _>>()?;
                for &k in &syms {
                    self.send(ToAgent::KeyState { keysym: k, down: true })?;
                }
                for &k in syms.iter().rev() {
                    self.send(ToAgent::KeyState { keysym: k, down: false })?;
                }
                Ok(text_result(format!("pressed {combo}")))
            }
            "scroll" => {
                let p = self.point(args, "x", "y")?;
                self.pointer(p)?;
                let dy = args.get("dy").and_then(Value::as_i64).unwrap_or(0).clamp(-50, 50);
                let dx = args.get("dx").and_then(Value::as_i64).unwrap_or(0).clamp(-50, 50);
                if dy != 0 {
                    self.send(ToAgent::Scroll { steps: dy as i8 })?;
                }
                if dx != 0 {
                    self.send(ToAgent::HScroll { steps: dx as i8 })?;
                }
                Ok(text_result("scrolled"))
            }
            "wait" => {
                let ms = args.get("ms").and_then(Value::as_u64).unwrap_or(500).min(10_000);
                std::thread::sleep(Duration::from_millis(ms));
                self.drain(None)?;
                Ok(text_result(format!("waited {ms} ms")))
            }
            "monitors" => {
                self.drain(None)?;
                let list: Vec<String> = self
                    .monitors
                    .iter()
                    .enumerate()
                    .map(|(i, m)| format!("{i}: {} {}x{}{}", m.name, m.w, m.h, if m.primary { " primary" } else { "" }))
                    .collect();
                Ok(text_result(if list.is_empty() { "one screen".to_string() } else { list.join("\n") }))
            }
            "select_monitor" => {
                let i = args.get("index").and_then(Value::as_u64).ok_or("missing index")?;
                self.send(ToAgent::SelectMonitor(i as u8))?;
                self.drain(Some(Instant::now() + Duration::from_millis(1500)))?;
                self.view = View { ox: 0.0, oy: 0.0, scale: 1.0 };
                Ok(text_result(format!("showing monitor {i}; take a screenshot")))
            }
            "windows" => {
                self.windows = None;
                self.send(ToAgent::ListWindows)?;
                let until = Instant::now() + Duration::from_secs(5);
                while self.windows.is_none() && Instant::now() < until {
                    self.drain(Some(Instant::now() + Duration::from_millis(100)))?;
                }
                match self.windows.take() {
                    Some(Ok(list)) => Ok(text_result(
                        list.iter()
                            .map(|w| format!("{}  {}  \"{}\"  {}x{}", w.id, w.app, w.title, w.w, w.h))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )),
                    Some(Err(e)) => Ok(error_result(e)),
                    None => Ok(error_result("no window list within 5 s")),
                }
            }
            "share" => {
                let t = args.get("target").and_then(Value::as_str).ok_or("missing target")?.trim();
                let (kind, id) = match t {
                    "screen" => (0, 0),
                    "active" => (3, 0),
                    "active-app" => (4, 0),
                    _ => match t.strip_prefix("app:") {
                        Some(id) => (2, id.trim().parse::<u64>().map_err(|_| "bad window id")?),
                        None => (1, t.parse::<u64>().map_err(|_| "target is screen, active, a window id or app:<id>")?),
                    },
                };
                self.sharing = None;
                self.send(ToAgent::Share { kind, id })?;
                let until = Instant::now() + Duration::from_secs(5);
                while self.sharing.is_none() && Instant::now() < until {
                    self.drain(Some(Instant::now() + Duration::from_millis(100)))?;
                }
                self.view = View { ox: 0.0, oy: 0.0, scale: 1.0 };
                match self.sharing.clone() {
                    Some(text) => Ok(text_result(format!("sharing {text}; take a screenshot"))),
                    None => Ok(error_result("the remote host did not confirm")),
                }
            }
            "clipboard_get" => {
                self.drain(None)?;
                Ok(text_result(self.clipboard.clone()))
            }
            "clipboard_set" => {
                let text = args.get("text").and_then(Value::as_str).ok_or("missing text")?.to_string();
                self.send(ToAgent::Clipboard(text))?;
                Ok(text_result("clipboard set"))
            }
            _ => Err(format!("unknown tool {name}")),
        }
    }
}

/// Runs the server until stdin closes.
pub fn run(opt: &Options) -> Result<(), String> {
    let (mut sess, _tx, rx) = Session::open(&opt.target()?, true)?;
    let desk = sess.desk;
    // Full resolution in true colour, but no stream: screenshots are taken on request.
    let s = Settings { view_w: desk.0, view_h: desk.1, scale: 100, mode: Mode::True, fps: 5 };
    sess.send(ToAgent::Settings(s)).map_err(|e| e.to_string())?;
    sess.send(ToAgent::Pause(true)).map_err(|e| e.to_string())?;
    let mut srv = Server {
        sess,
        rx,
        screen: (desk.0 as u32, desk.1 as u32),
        view: View { ox: 0.0, oy: 0.0, scale: 1.0 },
        clipboard: String::new(),
        monitors: Vec::new(),
        windows: None,
        sharing: None,
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = req.get("id").cloned() else { continue }; // notifications need no answer
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "desk", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Remote desktop control. Take a screenshot first; give coordinates in the latest screenshot's pixels. Prefer a small max_width and a region to read details."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(srv.call(name, &args).unwrap_or_else(error_result))
            }
            _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
        };
        let reply = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        writeln!(stdout, "{reply}").and_then(|_| stdout.flush()).map_err(|e| e.to_string())?;
    }
    srv.sess.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_name("Ctrl"), Some(0xffe3));
        assert_eq!(key_name("win"), Some(0xffeb));
        assert_eq!(key_name("F5"), Some(0xffc2));
        assert_eq!(key_name("a"), Some('a' as u32));
        assert_eq!(key_name("bogus"), None);
    }

    #[test]
    fn tool_schemas_are_objects() {
        for t in tools().as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        }
    }
}
