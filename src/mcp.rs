//! `sshremote mcp HOST`: a Model Context Protocol server on stdio that lets an AI
//! agent see and operate a remote desktop over the same ssh (or sshremote://) link.
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

use crate::xk;
use crate::color::Mode;
use crate::proto::{
    Settings, ToAgent, ToClient, UI_CHECKED, UI_DISABLED, UI_EXPANDED, UI_FOCUSED, UI_SELECTED, UiAction, UiElem,
};
use crate::session::{Ev, Options, Session};

/// How the last screenshot maps to the screen: screen = origin + image / scale.
#[derive(Clone, Copy)]
struct View {
    ox: f64,
    oy: f64,
    scale: f64,
}

struct Server {
    /// Where the host is; it is reached on the first tool call, and again after the link is lost.
    target: crate::transport::Target,
    link: Option<(Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>)>,
    screen: (u32, u32),
    /// The host said its screen's size again since this was cleared.
    resized: bool,
    view: View,
    clipboard: String,
    monitors: Vec<crate::proto::MonitorInfo>,
    /// The shown screen's resolutions, the one it has and its own (`Modes`).
    modes: crate::proto::ScreenModes,
    /// The host has said its monitors and resolutions on this link (just after its hello).
    heard: bool,
    windows: Option<Result<Vec<crate::capture::WindowInfo>, String>>,
    /// The host's installed programs, and what the last `launch` did.
    apps: Option<Result<Vec<(String, String, String)>, String>>,
    launched: Option<(bool, String)>,
    sharing: Option<String>,
    /// The last accessibility listing or action result.
    ui: Option<ToClient>,
    /// Window ids of the latest listing, by the numbers shown for focus_window.
    ui_windows: Vec<u64>,
}

/// Full resolution in true colour, but no stream: screenshots are taken on request.
fn full(desk: (u16, u16)) -> Settings {
    Settings { view_w: desk.0, view_h: desk.1, scale: 100, mode: Mode::True, fps: 5, formats: crate::proto::FORMATS }
}

fn tools() -> Value {
    let mut all = all_tools();
    // Programs come with the Programs extension.
    if !crate::ext::programs_enabled()
        && let Some(list) = all.as_array_mut()
    {
        list.retain(|t| !matches!(t.get("name").and_then(Value::as_str), Some("apps" | "launch")));
    }
    all
}

fn all_tools() -> Value {
    let xy = json!({"x": {"type": "integer"}, "y": {"type": "integer"}});
    let props = |extra: Value| {
        let mut p = xy.clone();
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        p
    };
    json!([
        {"name": "state",
         "description": "START HERE. The window in front on the SSH host as text: its title, the other open windows, and its controls numbered as [id] role \"name\" =value @x,y (position in the latest screenshot, when known), *focused, -disabled, +checked. Costs far fewer tokens than a screenshot; operate the controls with click_element and type_into, and call state again to see the result.",
         "inputSchema": {"type": "object", "properties": {
            "text": {"type": "boolean", "description": "Also list plain text such as labels and headings (default false)."},
            "max": {"type": "integer", "description": "At most this many elements (default 120)."}}}},
        {"name": "click_element", "description": "Click a control by its id from the latest state (double or right click on request).",
         "inputSchema": {"type": "object", "required": ["id"], "properties": {
            "id": {"type": "integer"},
            "button": {"type": "string", "enum": ["left", "right"]},
            "double": {"type": "boolean"}}}},
        {"name": "type_into", "description": "Put text into a control by its id from the latest state: replaces its content unless append is true; enter presses Enter afterwards.",
         "inputSchema": {"type": "object", "required": ["id", "text"], "properties": {
            "id": {"type": "integer"}, "text": {"type": "string"},
            "append": {"type": "boolean"}, "enter": {"type": "boolean"}}}},
        {"name": "focus_window", "description": "Bring another window from the latest state's window list to the front.",
         "inputSchema": {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}}},
        {"name": "screenshot",
         "description": "Capture the SSH host's screen as a PNG, only when state is not enough (pictures, charts, canvas apps, checking a layout). Coordinates for the other tools refer to the pixels of the most recent screenshot. Keep max_width small and use region to zoom in; that saves tokens.",
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
        {"name": "drag", "description": "Press the left button at (x, y), move to (to_x, to_y), hold a moment there and release (drag-and-drop works between windows).",
         "inputSchema": {"type": "object", "required": ["x", "y", "to_x", "to_y"], "properties": props(json!({
            "to_x": {"type": "integer"}, "to_y": {"type": "integer"},
            "steps": {"type": "integer", "description": "Points along the way (default 20)."},
            "step_ms": {"type": "integer", "description": "Milliseconds between them (default 25)."}}))}},
        {"name": "type", "description": "Type text (any language) into the focused field.",
         "inputSchema": {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}}},
        {"name": "key", "description": "Press a key or combination, e.g. \"enter\", \"ctrl+c\", \"alt+tab\", \"win+r\", \"ctrl+shift+esc\", \"f5\", \"ctrl+grave\" (punctuation by name: grave, comma, period, slash, backslash, semicolon, quote, equal, minus, plus, bracketleft, bracketright).",
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
        {"name": "resolution", "description": "The shown screen's own resolution (its display mode, not the picture's size): without arguments, the ones it can be set to; with width and height, set it to one (0 and 0: back to the one it had). It is set back when the connection ends.",
         "inputSchema": {"type": "object", "properties": {"width": {"type": "integer"}, "height": {"type": "integer"}}}},
        {"name": "windows", "description": "List the remote windows that can be shared on their own (id, program, title, size).",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "apps", "description": "List the programs installed on the remote computer (id, name, about), optionally only those whose name has `search`, to start one with launch.",
         "inputSchema": {"type": "object", "properties": {"search": {"type": "string"}}}},
        {"name": "launch", "description": "Start a program on the remote computer and show it alone (screenshots, clicks and keys then go to it): app (an id from apps), command (a command line), or open (a file path or an address, opened with its program). Waits up to 25 s for its window.",
         "inputSchema": {"type": "object", "properties": {"app": {"type": "string"}, "command": {"type": "string"}, "open": {"type": "string"}}}},
        {"name": "share", "description": "Limit the picture, clicks and keys to one window or program, which makes screenshots smaller and cheaper. target: \"screen\", \"active\" (the focused window), \"active-app\" (every window of its program), a window id from windows, or \"app:<window id>\" for every window of that program.",
         "inputSchema": {"type": "object", "required": ["target"], "properties": {"target": {"type": "string"}}}},
        {"name": "clipboard_get", "description": "Text last copied on the SSH host.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "clipboard_set", "description": "Put text on the remote clipboard.",
         "inputSchema": {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}}}
    ])
}

/// Keysym for a key name in a combination such as "ctrl+shift+t".
fn key_name(k: &str) -> Option<u32> {
    let l = k.to_ascii_lowercase();
    Some(match l.as_str() {
        "ctrl" | "control" => xk::CONTROL_L,
        "shift" => xk::SHIFT_L,
        "alt" | "option" => xk::ALT_L,
        "win" | "super" | "meta" | "cmd" | "windows" => xk::SUPER_L,
        "enter" | "return" => xk::RETURN,
        "tab" => xk::TAB,
        "esc" | "escape" => xk::ESCAPE,
        "backspace" => xk::BACKSPACE,
        "delete" | "del" => xk::DELETE,
        "insert" => xk::INSERT,
        "home" => xk::HOME,
        "end" => xk::END,
        "pageup" | "page_up" => xk::PAGE_UP,
        "pagedown" | "page_down" => xk::PAGE_DOWN,
        "left" => xk::LEFT,
        "up" => xk::UP,
        "right" => xk::RIGHT,
        "down" => xk::DOWN,
        "space" => ' ' as u32,
        "printscreen" | "print" => xk::PRINT,
        "capslock" => xk::CAPS_LOCK,
        "menu" | "apps" => xk::MENU,
        "plus" => '+' as u32,
        "minus" => '-' as u32,
        // Punctuation by name, as editors write shortcuts (VS Code's terminal is ctrl+backquote).
        "grave" | "backquote" | "backtick" => '`' as u32,
        "comma" => ',' as u32,
        "period" | "dot" => '.' as u32,
        "slash" => '/' as u32,
        "backslash" => '\\' as u32,
        "semicolon" => ';' as u32,
        "quote" | "apostrophe" => '\'' as u32,
        "equal" | "equals" => '=' as u32,
        "bracketleft" => '[' as u32,
        "bracketright" => ']' as u32,
        _ => {
            if let Some(k) = l.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).and_then(xk::f) {
                return Some(k);
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
    /// Sends an accessibility request and waits for its answer.
    fn ui_request(&mut self, m: ToAgent) -> Result<ToClient, String> {
        self.drain(None)?;
        self.ui = None;
        self.send(m)?;
        let until = Instant::now() + Duration::from_secs(15);
        while self.ui.is_none() && Instant::now() < until {
            self.drain(Some(Instant::now() + Duration::from_millis(50)))?;
        }
        self.ui.take().ok_or_else(|| "the SSH host did not answer within 15 s".to_string())
    }

    fn ui_act(&mut self, target: u64, act: UiAction, text: String) -> Result<Value, String> {
        match self.ui_request(ToAgent::UiAct { target, act, text })? {
            ToClient::UiDone { ok: true, text } => Ok(text_result(format!("{text}; call state to see the result"))),
            ToClient::UiDone { ok: false, text } => Ok(error_result(text)),
            _ => Ok(error_result("unexpected answer")),
        }
    }

    /// The listing as compact text, positions given in the latest screenshot's pixels.
    fn format_state(
        &mut self,
        title: &str,
        windows: &[(u64, String)],
        elems: &[UiElem],
        more: u32,
        note: &str,
    ) -> String {
        let mut s = format!("Window: \"{title}\"\n");
        for e in elems {
            s.push_str(&format!("[{}] {}", e.id, e.role));
            if !e.name.is_empty() {
                s.push_str(&format!(" \"{}\"", e.name));
            }
            if !e.value.is_empty() {
                s.push_str(&format!(" ={}", e.value));
            }
            if e.x >= 0 && e.y >= 0 {
                let x = (e.x as f64 - self.view.ox) * self.view.scale;
                let y = (e.y as f64 - self.view.oy) * self.view.scale;
                s.push_str(&format!(" @{x:.0},{y:.0}"));
            }
            for (bit, mark) in [
                (UI_FOCUSED, " *focused"),
                (UI_DISABLED, " -disabled"),
                (UI_CHECKED, " +checked"),
                (UI_SELECTED, " +selected"),
                (UI_EXPANDED, " +expanded"),
            ] {
                if e.flags & bit != 0 {
                    s.push_str(mark);
                }
            }
            s.push('\n');
        }
        if elems.is_empty() {
            s.push_str("(no controls listed: this window publishes no accessibility information; use screenshot)\n");
        }
        if more > 0 {
            s.push_str(&format!("... {more} more (raise max)\n"));
        }
        // Windows are numbered here; focus_window takes these numbers.
        self.ui_windows = windows.iter().map(|w| w.0).collect();
        if !windows.is_empty() {
            s.push_str("Other windows (focus_window id):\n");
            for (i, (_, t)) in windows.iter().enumerate() {
                s.push_str(&format!("  {} {t}\n", i + 1));
            }
        }
        if !note.is_empty() {
            s.push_str(&format!("Note: {note}\n"));
        }
        s
    }

    /// The link to the host: made when first needed, and made again after it was lost.
    fn link(&mut self) -> Result<&mut (Session, mpsc::Sender<Ev>, mpsc::Receiver<Ev>), String> {
        if self.link.is_none() {
            let (mut sess, tx, rx) = Session::open(&self.target, true)?;
            let desk = sess.desk;
            sess.send(ToAgent::Settings(full(desk))).map_err(|e| e.to_string())?;
            sess.send(ToAgent::Pause(true)).map_err(|e| e.to_string())?;
            self.screen = (desk.0 as u32, desk.1 as u32);
            self.view = View { ox: 0.0, oy: 0.0, scale: 1.0 };
            self.heard = false;
            self.link = Some((sess, tx, rx));
        }
        Ok(self.link.as_mut().expect("made above"))
    }

    fn send(&mut self, m: ToAgent) -> Result<(), String> {
        let r = self.link()?.0.send(m);
        r.map_err(|e| {
            self.link = None;
            format!("connection lost (it is made again on the next call): {e}")
        })
    }

    /// Handles queued messages; frames are only acknowledged, the stream being paused.
    fn drain(&mut self, until: Option<Instant>) -> Result<Option<Vec<u8>>, String> {
        loop {
            let rx = &self.link()?.2;
            let got = match until {
                Some(t) => rx.recv_timeout(t.saturating_duration_since(Instant::now())).map_err(|e| e == mpsc::RecvTimeoutError::Disconnected),
                None => rx.try_recv().map_err(|e| e == mpsc::TryRecvError::Disconnected),
            };
            let ev = match got {
                Ok(e) => e,
                Err(false) => return Ok(None),
                // A lost link is said so, not taken for "nothing new"; the next call makes it again.
                Err(true) => {
                    self.link = None;
                    return Err("connection closed (it is made again on the next call)".into());
                }
            };
            match ev {
                Ev::Net(ToClient::Frame(f)) => self.send(ToAgent::Ack(f.id))?,
                Ev::Net(ToClient::Snapshot(png)) => return Ok(Some(png)),
                Ev::Net(ToClient::Clipboard(t)) => self.clipboard = t,
                Ev::Net(ToClient::Monitors { list, .. }) => self.monitors = list,
                Ev::Net(ToClient::Modes { list, current, own }) => {
                    self.modes = (list, current, own);
                    self.heard = true;
                }
                Ev::Net(ToClient::WindowList { list, error }) => {
                    self.windows = Some(if error.is_empty() { Ok(list) } else { Err(error) });
                }
                Ev::Net(ToClient::Sharing { text, .. }) => self.sharing = Some(text),
                Ev::Net(ToClient::AppList { list, error }) => self.apps = Some(if list.is_empty() && !error.is_empty() { Err(error) } else { Ok(list) }),
                Ev::Net(ToClient::Launched { ok, text }) => self.launched = Some((ok, text)),
                Ev::Net(m @ (ToClient::UiTree { .. } | ToClient::UiDone { .. })) => self.ui = Some(m),
                // The screen took another size (its resolution, another monitor): pictures follow it.
                Ev::Net(ToClient::Hello { w, h, .. }) => {
                    self.screen = (w as u32, h as u32);
                    self.resized = true;
                    self.send(ToAgent::Settings(full((w, h))))?;
                }
                Ev::Net(ToClient::Notice(t)) if until.is_some() => return Err(t),
                Ev::Net(ToClient::Error(e)) | Ev::Closed(e) => {
                    self.link = None;
                    return Err(e);
                }
                _ => {}
            }
        }
    }

    /// What came so far; just after the link is made, also what the host says after its hello
    /// (its monitors, then their resolutions), which a slow link brings a moment later.
    fn settle(&mut self) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            self.drain(None)?;
            if self.heard || Instant::now() >= until {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn point(&self, args: &Value, kx: &str, ky: &str) -> Result<(u16, u16), String> {
        let get = |k: &str| args.get(k).and_then(Value::as_f64).ok_or(format!("missing {k}"));
        let (x, y) = (get(kx)?, get(ky)?);
        // Inside the screen; one not known yet (0x0) gives the corner rather than a panic.
        let sx = (self.view.ox + x / self.view.scale).min(self.screen.0 as f64 - 1.0).max(0.0);
        let sy = (self.view.oy + y / self.view.scale).min(self.screen.1 as f64 - 1.0).max(0.0);
        Ok((sx.round() as u16, sy.round() as u16))
    }

    fn pointer(&mut self, p: (u16, u16)) -> Result<(), String> {
        self.send(ToAgent::Pointer { x: p.0, y: p.1 })
    }

    fn screenshot(&mut self, args: &Value) -> Result<Value, String> {
        self.drain(None)?;
        let mut png = Vec::new();
        for _ in 0..2 {
            self.resized = false;
            self.send(ToAgent::Snapshot)?;
            png = self.drain(Some(Instant::now() + Duration::from_secs(15)))?.ok_or("no screenshot within 15 s")?;
            // Taken at the size the screen had before: once more at its new one.
            if !self.resized {
                break;
            }
        }
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
        // Every tool works on the host: reached first, so its screen size is known.
        self.link()?;
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
                // As a hand drags: a pause after the press, steps along the way (`steps`, each
                // `step_ms` apart), so a drag-and-drop source starts its drag before the pointer
                // leaves it, then a moment over the target: a window takes a drop only once it
                // has said it will.
                let num = |k: &str, d: u64, max: u64| args.get(k).and_then(Value::as_u64).unwrap_or(d).clamp(1, max);
                let (steps, step_ms) = (num("steps", 20, 200), num("step_ms", 25, 500));
                self.pointer(a)?;
                self.send(ToAgent::Button { button: 1, pressed: true })?;
                std::thread::sleep(short.max(Duration::from_millis(150)));
                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    let x = a.0 as f64 + (b.0 as f64 - a.0 as f64) * t;
                    let y = a.1 as f64 + (b.1 as f64 - a.1 as f64) * t;
                    self.pointer((x as u16, y as u16))?;
                    std::thread::sleep(Duration::from_millis(step_ms));
                }
                std::thread::sleep(Duration::from_millis(400));
                self.send(ToAgent::Button { button: 1, pressed: false })?;
                Ok(text_result("dragged"))
            }
            "type" => {
                let text = args.get("text").and_then(Value::as_str).ok_or("missing text")?;
                for c in text.chars() {
                    let sym = match c {
                        '\n' => xk::RETURN,
                        '\t' => xk::TAB,
                        c if (c as u32) < 0x100 => c as u32,
                        c => xk::unicode(c as u32),
                    };
                    self.send(ToAgent::Key { keysym: sym, mods: 0 })?;
                }
                Ok(text_result(format!("typed {} characters", text.chars().count())))
            }
            "key" => {
                let combo = args.get("keys").and_then(Value::as_str).ok_or("missing keys")?;
                let syms: Vec<u32> = combo
                    .split('+')
                    .map(|k| {
                        key_name(k.trim()).ok_or(format!(
                            "unknown key {k:?}: use a character, f1..f24, or a name such as enter, tab, esc, backspace, delete, home, end, pageup, left, space, grave, comma, slash"
                        ))
                    })
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
                self.settle()?;
                let list: Vec<String> = self
                    .monitors
                    .iter()
                    .enumerate()
                    .map(|(i, m)| format!("{i}: {} {}x{}{}", m.name, m.w, m.h, if m.primary { " primary" } else { "" }))
                    .collect();
                Ok(text_result(if list.is_empty() { "one screen".to_string() } else { list.join("\n") }))
            }
            "resolution" => {
                let num = |k: &str| args.get(k).and_then(Value::as_u64).map(|v| v.min(u16::MAX as u64) as u16);
                if let (Some(w), Some(h)) = (num("width"), num("height")) {
                    // Why it was not set comes as a notice, which ends the wait as an error.
                    self.send(ToAgent::SetMode { w, h })?;
                    self.drain(Some(Instant::now() + Duration::from_millis(2500)))?;
                    self.view = View { ox: 0.0, oy: 0.0, scale: 1.0 };
                } else {
                    self.settle()?;
                }
                let (list, current, own) = &self.modes;
                if list.is_empty() {
                    return Ok(text_result("the resolution of this screen cannot be set from here".to_string()));
                }
                let all: Vec<String> = list.iter().map(|&(w, h)| format!("{w}x{h}{}", if (w, h) == *own { " (its own)" } else { "" })).collect();
                Ok(text_result(format!("now {}x{}; it can be set to: {}", current.0, current.1, all.join(", "))))
            }
            "select_monitor" => {
                let i = args.get("index").and_then(Value::as_u64).ok_or("missing index")?;
                self.send(ToAgent::SelectMonitor(i as u8))?;
                self.drain(Some(Instant::now() + Duration::from_millis(1500)))?;
                self.view = View { ox: 0.0, oy: 0.0, scale: 1.0 };
                Ok(text_result(format!("showing monitor {i}; take a screenshot")))
            }
            "state" => {
                let text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
                let max = args.get("max").and_then(Value::as_u64).unwrap_or(120).clamp(1, 2000) as u16;
                match self.ui_request(ToAgent::UiState { text, max })? {
                    ToClient::UiTree { title, windows, elems, more, note } => {
                        Ok(text_result(self.format_state(&title, &windows, &elems, more, &note)))
                    }
                    _ => Ok(error_result("unexpected answer")),
                }
            }
            "click_element" | "type_into" | "focus_window" => {
                let id = args.get("id").and_then(Value::as_u64).ok_or("missing id")?;
                let enter = name == "type_into" && args.get("enter").and_then(Value::as_bool).unwrap_or(false);
                let (act, text) = match name {
                    "focus_window" => {
                        let target = self.ui_windows.get((id as usize).wrapping_sub(1)).copied().unwrap_or(id);
                        return self.ui_act(target, UiAction::FocusWindow, String::new());
                    }
                    "type_into" => {
                        let text = args.get("text").and_then(Value::as_str).ok_or("missing text")?.to_string();
                        let append = args.get("append").and_then(Value::as_bool).unwrap_or(false);
                        (if append { UiAction::Type } else { UiAction::SetText }, text)
                    }
                    _ => {
                        let right = args.get("button").and_then(Value::as_str) == Some("right");
                        let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
                        let act = if right {
                            UiAction::RightClick
                        } else if double {
                            UiAction::DoubleClick
                        } else {
                            UiAction::Click
                        };
                        (act, String::new())
                    }
                };
                let done = self.ui_act(id, act, text)?;
                // Enter is a key pressed in the field, not a line break put in its text (setting
                // a field's value would only add one).
                if enter && done.get("isError").and_then(Value::as_bool) != Some(true) {
                    let _ = self.ui_request(ToAgent::UiAct { target: id, act: UiAction::Focus, text: String::new() });
                    self.send(ToAgent::Key { keysym: crate::xk::RETURN, mods: 0 })?;
                }
                Ok(done)
            }
            "apps" | "launch" if !crate::ext::programs_enabled() => Ok(error_result("programs open with the Programs extension: sshremote ext install dev.sshremote.programs")),
            "apps" => {
                self.apps = None;
                self.send(ToAgent::Apps)?;
                let until = Instant::now() + Duration::from_secs(10);
                while self.apps.is_none() && Instant::now() < until {
                    self.drain(Some(Instant::now() + Duration::from_millis(100)))?;
                }
                let search = args.get("search").and_then(Value::as_str).unwrap_or("").to_lowercase();
                match self.apps.take() {
                    Some(Ok(list)) => Ok(text_result(
                        list.iter()
                            .filter(|(_, name, _)| name.to_lowercase().contains(&search))
                            .map(|(id, name, about)| format!("{id}  {name}{}", if about.is_empty() { String::new() } else { format!("  ({about})") }))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )),
                    Some(Err(e)) => Ok(error_result(e)),
                    None => Ok(error_result("no list of programs within 10 s")),
                }
            }
            "launch" => {
                let pick = |k: &str| args.get(k).and_then(Value::as_str).filter(|t| !t.trim().is_empty()).map(str::to_string);
                let (kind, text) = if let Some(t) = pick("app") {
                    (crate::apps::APP, t)
                } else if let Some(t) = pick("command") {
                    (crate::apps::COMMAND, t)
                } else if let Some(t) = pick("open") {
                    (crate::apps::OPEN, t)
                } else {
                    return Err("give app, command or open".into());
                };
                self.launched = None;
                self.send(ToAgent::Launch { kind, text })?;
                let until = Instant::now() + Duration::from_secs(25);
                while self.launched.is_none() && Instant::now() < until {
                    self.drain(Some(Instant::now() + Duration::from_millis(100)))?;
                }
                self.view = View { ox: 0.0, oy: 0.0, scale: 1.0 };
                match self.launched.take() {
                    Some((true, t)) => Ok(text_result(format!("{t}; take a screenshot"))),
                    Some((false, e)) => Ok(error_result(e)),
                    None => Ok(error_result("it did not start within 25 s")),
                }
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
                use crate::capture::Share;
                let share = match t {
                    "screen" => Share::Screen,
                    "active" => Share::ActiveWindow,
                    "active-app" => Share::ActiveApp,
                    "pick" => {
                        self.send(ToAgent::PickOnRemote)?;
                        return Ok(text_result(
                            "the sharing dialog is open on the SSH host's screen; take a screenshot, click a window and Share there",
                        ));
                    }
                    _ => match t.strip_prefix("app:") {
                        Some(id) => Share::App(id.trim().parse::<u64>().map_err(|_| "bad window id")?),
                        None => Share::Window(t.parse::<u64>().map_err(|_| "target is screen, active, a window id or app:<id>")?),
                    },
                };
                self.sharing = None;
                self.send(ToAgent::Share(share))?;
                let until = Instant::now() + Duration::from_secs(5);
                while self.sharing.is_none() && Instant::now() < until {
                    self.drain(Some(Instant::now() + Duration::from_millis(100)))?;
                }
                self.view = View { ox: 0.0, oy: 0.0, scale: 1.0 };
                match self.sharing.clone() {
                    Some(text) => Ok(text_result(format!("sharing {text}; take a screenshot"))),
                    None => Ok(error_result("the SSH host did not confirm")),
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
    // The host is reached on the first tool call: the client gets its answer to initialize at once,
    // even when the host is away.
    let mut srv = Server {
        target: opt.target()?,
        link: None,
        screen: (0, 0),
        resized: false,
        view: View { ox: 0.0, oy: 0.0, scale: 1.0 },
        clipboard: String::new(),
        monitors: Vec::new(),
        modes: (Vec::new(), (0, 0), (0, 0)),
        heard: false,
        windows: None,
        apps: None,
        launched: None,
        sharing: None,
        ui: None,
        ui_windows: Vec::new(),
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            let err =
                serde_json::json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}});
            println!("{err}");
            continue;
        };
        let Some(id) = req.get("id").cloned() else { continue }; // notifications need no answer
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "sshremote", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Desktop control. Start with state, which lists the window in front and its numbered controls as text, and act with click_element, type_into and focus_window. Take a screenshot only for what has no text (pictures, canvases, games); its coordinates are in that screenshot's pixels, and a small max_width or a region keeps it cheap."
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
    if let Some((mut sess, _, _)) = srv.link.take() {
        sess.close();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_name("Ctrl"), Some(xk::CONTROL_L));
        assert_eq!(key_name("win"), Some(xk::SUPER_L));
        assert_eq!(key_name("F5"), Some(xk::F1 + 4));
        assert_eq!(key_name("a"), Some('a' as u32));
        assert_eq!(key_name("bogus"), None);
        assert_eq!(key_name("grave"), Some('`' as u32), "VS Code's terminal: ctrl+grave");
        assert_eq!(key_name("backslash"), Some('\\' as u32));
        assert_eq!(key_name("quote"), Some('\'' as u32));
    }

    #[test]
    fn tool_schemas_are_objects() {
        for t in tools().as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        }
    }
}
