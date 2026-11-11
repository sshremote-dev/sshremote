//! The extension host: runs the installed extensions that keep a program running beside the app
//! ("activation": "startup") and talks with each over its stdin and stdout, one JSON-RPC 2.0
//! message per line, as language servers and MCP servers do. See docs/extensions.md.
//!
//! The app tells an extension what happens (initialize, command, action, event, shutdown); the
//! extension asks the app for things (a status bar item, a panel, a terminal, a paste, a notice).
//! Each request is checked against the permissions its manifest declares.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value, json};

/// The protocol version the app speaks.
pub const API: u64 = 2;

/// A status bar item: its text, what hovering tells, and the command a click runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub ext: String,
    pub id: String,
    pub text: String,
    pub tooltip: String,
    pub command: Option<String>,
}

/// A panel in the side bar: Markdown whose `sshremote:` links come back as actions.
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub ext: String,
    pub id: String,
    pub title: String,
    pub markdown: String,
    /// Bumped on each change, so the view prepares the document again only then.
    pub rev: u64,
}

/// A question an extension asked the user: the answer goes back as the reply to its request.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub ext: String,
    pub name: String,
    id: Value,
    pub title: String,
    pub prompt: String,
    pub fields: Vec<Field>,
    /// The button that sends the answers.
    pub submit: String,
    /// A form (answered with every field by id) rather than one question (answered with its value).
    form: bool,
}

/// One field of a form: "text", "secret", "number", "bool" or "choice" (one of `choices`).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub value: String,
    pub choices: Vec<String>,
}

fn field_from(v: &Value) -> Option<Field> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let kind = match s("type").as_str() {
        "" => "text".to_string(),
        k @ ("text" | "secret" | "number" | "bool" | "choice") => k.to_string(),
        _ => return None,
    };
    let value = match v.get("value") {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(x) => x.to_string(),
    };
    let choices: Vec<String> = v.get("choices").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let id = s("id");
    (!id.is_empty() && (kind != "choice" || !choices.is_empty())).then(|| Field { label: if s("label").is_empty() { id.clone() } else { s("label") }, id, kind, value, choices })
}

/// What an extension asks of the workspace.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// A terminal running `argv` (empty: the login shell), in a new tab or beside the active pane.
    OpenTerminal { argv: Vec<String>, label: String, split: Option<bool> },
    /// Text typed into the active terminal as a paste: it is seen before Enter runs it.
    Paste(String),
}

struct Running {
    id: String,
    name: String,
    permissions: Vec<String>,
    stdin: ChildStdin,
    child: Child,
    run: u64,
}

#[derive(Default)]
struct State {
    running: Vec<Running>,
    items: Vec<Item>,
    panels: Vec<Panel>,
    requests: Vec<Request>,
    notices: Vec<String>,
    /// Questions extensions asked, answered in turn.
    asks: Vec<Ask>,
    log: Vec<String>,
    /// Messages by extension and run, so one from a program already replaced is told apart.
    rx: Option<Receiver<(String, u64, Value)>>,
    /// What each extension started from looked like (times and sizes), to notice a new version
    /// installed from outside, and when that was last looked at.
    stamps: std::collections::HashMap<String, String>,
    looked: Option<std::time::Instant>,
    tx: Option<Sender<(String, u64, Value)>>,
    runs: u64,
    next: u64,
    rev: u64,
    /// Wakes the window when a message arrives.
    waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

#[cfg(not(test))]
static STATE: std::sync::Mutex<Option<State>> = std::sync::Mutex::new(None);

#[cfg(not(test))]
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

// Tests run side by side: each keeps its own host.
#[cfg(test)]
thread_local!(static STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) });

#[cfg(test)]
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(s.borrow_mut().get_or_insert_with(State::default)))
}

/// Starts every installed extension that runs beside the app; `waker` repaints the window.
pub fn start(waker: impl Fn() + Send + Sync + 'static) {
    with(|s| s.waker = Some(std::sync::Arc::new(waker)));
    for m in crate::ext::installed() {
        if m.activation.as_deref() == Some("startup") {
            if let Err(e) = launch(&m) {
                with(|s| s.notices.push(format!("{}: {e}", m.name)));
            }
        }
    }
}

/// Starts one extension's program and says hello.
fn launch(m: &crate::ext::Manifest) -> Result<(), String> {
    let here = crate::ext::platform();
    let file = m.programs.iter().find(|(p, _)| *p == here).map(|(_, f)| f.clone()).ok_or("no program for this computer")?;
    let dir = crate::ext::folder(&m.id).ok_or("not installed")?;
    // A copy runs, so the installed file is never held open: installing over it works while the
    // app runs (Windows refuses to replace a running program).
    let program = run_copy(&m.id, &dir.join(&file))?;
    let mut cmd = Command::new(&program);
    // Not in its own folder either, which Windows would then keep from being replaced.
    cmd.current_dir(program.parent().unwrap_or(&dir)).env("SSHREMOTE_EXTENSION_DIR", &dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up for an extension.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", file))?;
    let stdout = child.stdout.take().ok_or("no output")?;
    let stderr = child.stderr.take().ok_or("no errors")?;
    let stdin = child.stdin.take().ok_or("no input")?;
    let (tx, waker) = with(|s| {
        if s.tx.is_none() {
            let (tx, rx) = channel();
            s.tx = Some(tx);
            s.rx = Some(rx);
        }
        (s.tx.clone().expect("set"), s.waker.clone())
    });
    let id = m.id.clone();
    let run = with(|s| {
        s.runs += 1;
        s.runs
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let msg = serde_json::from_str::<Value>(&line).unwrap_or_else(|e| json!({ "method": "log", "params": { "text": format!("not JSON-RPC ({e}): {line}") } }));
            if tx.send((id.clone(), run, msg)).is_err() {
                break;
            }
            if let Some(w) = &waker {
                w();
            }
        }
        let _ = tx.send((id.clone(), run, json!({ "method": "exit" })));
        if let Some(w) = &waker {
            w();
        }
    });
    let id = m.id.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            with(|s| keep_log(s, format!("{id}: {line}")));
        }
    });
    with(|s| {
        s.stamps.insert(m.id.clone(), stamp(m));
        s.running.retain(|r| r.id != m.id);
        s.running.push(Running { id: m.id.clone(), name: m.name.clone(), permissions: m.permissions.clone(), stdin, child, run });
    });
    let params = json!({
        "api": API,
        "app": env!("CARGO_PKG_VERSION"),
        "platform": here,
        "extension": { "id": m.id, "version": m.version, "folder": dir.to_string_lossy() },
        "permissions": m.permissions,
        "settings": crate::ext::settings_json(m),
    });
    request(&m.id, "initialize", params);
    Ok(())
}

/// The program copied out of the extension's folder, once for each version of it.
fn run_copy(id: &str, program: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let meta = std::fs::metadata(program).map_err(|e| format!("{}: {e}", program.display()))?;
    let when = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis());
    let name = program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = std::env::temp_dir().join("sshremote-extensions").join(id);
    let copy = dir.join(format!("{when}-{}-{name}", meta.len()));
    if !copy.is_file() {
        // Earlier versions' copies go, unless one still runs.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::copy(program, &copy).map_err(|e| format!("{}: {e}", copy.display()))?;
    }
    Ok(copy)
}

/// What an installed extension looks like on disk: its manifest's and programs' times and sizes.
fn stamp(m: &crate::ext::Manifest) -> String {
    let Some(dir) = crate::ext::folder(&m.id) else { return String::new() };
    std::iter::once("extension.json".to_string())
        .chain(m.programs.iter().map(|(_, f)| f.clone()))
        .filter_map(|f| {
            let meta = std::fs::metadata(dir.join(&f)).ok()?;
            let t = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
            Some(format!("{f}:{t}:{}", meta.len()))
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Every two seconds: extensions installed, updated or removed from outside (the command line,
/// `sshremote ext dev`) start, start again or stop.
fn look_for_changes() {
    let due = with(|s| {
        let due = s.waker.is_some() && s.looked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(2));
        if due {
            s.looked = Some(std::time::Instant::now());
        }
        due
    });
    if !due {
        return;
    }
    let installed: Vec<crate::ext::Manifest> = crate::ext::installed().into_iter().filter(|m| m.activation.as_deref() == Some("startup")).collect();
    let running: Vec<String> = with(|s| s.running.iter().map(|r| r.id.clone()).collect());
    for id in &running {
        if !installed.iter().any(|m| &m.id == id) {
            stop(id);
        }
    }
    for m in &installed {
        let (known, was) = with(|s| (s.stamps.contains_key(&m.id), s.stamps.get(&m.id).cloned()));
        let now = stamp(m);
        if !known || was.as_deref() != Some(now.as_str()) {
            with(|s| s.stamps.insert(m.id.clone(), now));
            if known || !running.contains(&m.id) {
                reload(&m.id);
            }
        }
    }
}

/// A panel's declared list as Markdown: one row for each item, its label, detail and action links.
pub(crate) fn items_markdown(list: &[Value]) -> String {
    let cell = |v: Option<&Value>| v.and_then(|x| x.as_str()).unwrap_or("").replace('|', "\\|").replace('\n', " ");
    let mut md = String::from("\n\n| | | |\n|---|---|---|\n");
    for it in list {
        let label = cell(it.get("label"));
        let label = if it.get("bold").and_then(|b| b.as_bool()) == Some(true) && !label.is_empty() { format!("**{label}**") } else { label };
        let actions: Vec<String> = it
            .get("actions")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let title = cell(a.get("title"));
                let to = a.get("action")?.as_str()?;
                (!title.is_empty() && !to.contains([' ', ')', '|'])).then(|| format!("[{title}](sshremote:{to})"))
            })
            .collect();
        md.push_str(&format!("| {label} | {} | {} |\n", cell(it.get("detail")), actions.join(" ")));
    }
    md
}

fn keep_log(s: &mut State, line: String) {
    s.log.push(line);
    let n = s.log.len();
    if n > 500 {
        s.log.drain(..n - 500);
    }
}

/// Sends a request (it has an id; the answer is logged) or a notification to one extension.
fn send(ext: &str, method: &str, params: Value, id: Option<u64>) {
    #[cfg(test)]
    SENT.with(|v| v.borrow_mut().push((ext.to_string(), method.to_string(), params.clone())));
    with(|s| {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if let Some(id) = id {
            msg["id"] = json!(id);
        }
        let line = format!("{msg}\n");
        let mut dead = None;
        if let Some(r) = s.running.iter_mut().find(|r| r.id == ext) {
            if r.stdin.write_all(line.as_bytes()).and_then(|()| r.stdin.flush()).is_err() {
                dead = Some(r.id.clone());
            }
        }
        if let Some(d) = dead {
            keep_log(s, format!("{d}: stopped answering"));
        }
    });
}

fn request(ext: &str, method: &str, params: Value) {
    let id = with(|s| {
        s.next += 1;
        s.next
    });
    send(ext, method, params, Some(id));
}

fn reply(ext: &str, id: &Value, result: Result<Value, String>) {
    #[cfg(test)]
    SENT.with(|v| v.borrow_mut().push((ext.to_string(), "reply".to_string(), json!({ "id": id, "result": result.clone().ok(), "error": result.clone().err() }))));
    let msg = match result {
        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32001, "message": e } }),
    };
    with(|s| {
        if let Some(r) = s.running.iter_mut().find(|r| r.id == ext) {
            let _ = r.stdin.write_all(format!("{msg}\n").as_bytes()).and_then(|()| r.stdin.flush());
        }
    });
}

/// What the extensions said since the last frame: applied to the status bar and panels, and
/// what they ask of the workspace kept for it to take.
pub fn pump() {
    look_for_changes();
    let msgs: Vec<(String, u64, Value)> = with(|s| s.rx.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default());
    for (ext, run, msg) in msgs {
        // From a program stopped since (an update replaced it): nothing it says counts now.
        if !with(|s| s.running.iter().any(|r| r.id == ext && r.run == run)) {
            continue;
        }
        handle(&ext, &msg);
    }
}

fn handle(ext: &str, msg: &Value) {
    let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
        // An answer to one of ours: only errors are worth keeping.
        if let Some(e) = msg.get("error") {
            with(|s| keep_log(s, format!("{ext}: {e}")));
        }
        return;
    };
    let p = msg.get("params").cloned().unwrap_or(Value::Null);
    let str_of = |k: &str| p.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let allowed = |perm: &str| with(|s| s.running.iter().any(|r| r.id == ext && r.permissions.iter().any(|x| x == perm)));
    let result: Result<Value, String> = match method {
        "status.set" => with(|s| {
            let item = Item { ext: ext.into(), id: str_of("id"), text: str_of("text"), tooltip: str_of("tooltip"), command: p.get("command").and_then(|c| c.as_str()).map(str::to_string) };
            match s.items.iter_mut().find(|i| i.ext == ext && i.id == item.id) {
                Some(i) => *i = item,
                None => s.items.push(item),
            }
            Ok(Value::Null)
        }),
        "status.remove" => with(|s| {
            s.items.retain(|i| !(i.ext == ext && i.id == str_of("id")));
            Ok(Value::Null)
        }),
        "panel.show" => with(|s| {
            s.rev += 1;
            let mut markdown = str_of("markdown");
            if let Some(list) = p.get("items").and_then(|x| x.as_array()) {
                markdown.push_str(&items_markdown(list));
            }
            let panel = Panel { ext: ext.into(), id: str_of("id"), title: str_of("title"), markdown, rev: s.rev };
            match s.panels.iter_mut().find(|x| x.ext == ext && x.id == panel.id) {
                Some(x) => *x = panel,
                None => s.panels.push(panel),
            }
            Ok(Value::Null)
        }),
        "panel.close" => with(|s| {
            s.panels.retain(|x| !(x.ext == ext && x.id == str_of("id")));
            Ok(Value::Null)
        }),
        "terminal.open" if allowed("workspace.open") => {
            let argv = p.get("argv").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let split = match p.get("split").and_then(|x| x.as_str()) {
                Some("right") => Some(false),
                Some("down") => Some(true),
                _ => None,
            };
            with(|s| s.requests.push(Request::OpenTerminal { argv, label: str_of("label"), split }));
            Ok(Value::Null)
        }
        "terminal.paste" if allowed("terminal.write") => {
            with(|s| s.requests.push(Request::Paste(str_of("text"))));
            Ok(Value::Null)
        }
        "terminal.open" | "terminal.paste" => {
            let need = if method == "terminal.open" { "workspace.open" } else { "terminal.write" };
            Err(format!("{method} needs the \"{need}\" permission in extension.json"))
        }
        "input.ask" | "input.form" => {
            let Some(id) = msg.get("id").cloned() else { return };
            let form = method == "input.form";
            let fields: Vec<Field> = if form {
                p.get("fields").and_then(|f| f.as_array()).map(|a| a.iter().filter_map(field_from).collect()).unwrap_or_default()
            } else {
                let kind = if p.get("secret").and_then(|x| x.as_bool()).unwrap_or(false) { "secret" } else { "text" };
                vec![Field { id: "value".into(), label: String::new(), kind: kind.into(), value: str_of("value"), choices: Vec::new() }]
            };
            if fields.is_empty() {
                reply(ext, &id, Err("a form needs fields".into()));
                return;
            }
            with(|s| {
                let name = s.running.iter().find(|r| r.id == ext).map_or(ext.to_string(), |r| r.name.clone());
                let submit = if str_of("submit").is_empty() { "OK".to_string() } else { str_of("submit") };
                s.asks.push(Ask { ext: ext.into(), name, id, title: str_of("title"), prompt: str_of("prompt"), fields, submit, form });
            });
            // Answered when the user is done.
            return;
        }
        "notify" => with(|s| {
            let name = s.running.iter().find(|r| r.id == ext).map_or(ext.to_string(), |r| r.name.clone());
            s.notices.push(format!("{name}: {}", str_of("text")));
            Ok(Value::Null)
        }),
        "log" => with(|s| {
            keep_log(s, format!("{ext}: {}", str_of("text")));
            Ok(Value::Null)
        }),
        "exit" => {
            forget(ext);
            return;
        }
        other => Err(format!("unknown method {other}")),
    };
    if let Some(id) = msg.get("id") {
        reply(ext, id, result);
    } else if let Err(e) = result {
        with(|s| keep_log(s, format!("{ext}: {e}")));
    }
}

/// An extension that ended: its items, panels and questions go with it.
fn forget(ext: &str) {
    with(|s| {
        s.asks.retain(|a| a.ext != ext);
        s.running.retain(|r| r.id != ext);
        s.items.retain(|i| i.ext != ext);
        s.panels.retain(|p| p.ext != ext);
    });
}

/// Runs a command an extension contributed; the selected text goes along when it may read it.
pub fn command(ext: &str, id: &str, selection: &str) {
    let may = with(|s| s.running.iter().any(|r| r.id == ext && r.permissions.iter().any(|p| p == "terminal.selection")));
    let mut params = json!({ "id": id });
    if may {
        params["selection"] = json!(selection);
    }
    request(ext, "command", params);
}

/// A `sshremote:` link clicked in a panel.
pub fn action(ext: &str, panel: &str, url: &str) {
    let (name, query) = url.trim_start_matches("sshremote:").split_once('?').unwrap_or((url.trim_start_matches("sshremote:"), ""));
    let args: serde_json::Map<String, Value> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (unescape(k), json!(unescape(v)))
        })
        .collect();
    request(ext, "action", json!({ "panel": panel, "action": name, "args": args }));
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Tells the extensions that may watch the workspace what happened in it.
pub fn event(name: &str, data: Value) {
    let ids: Vec<String> = with(|s| s.running.iter().filter(|r| r.permissions.iter().any(|p| p == "workspace.read")).map(|r| r.id.clone()).collect());
    for id in ids {
        send(&id, "event", json!({ "name": name, "data": data }), None);
    }
}

/// The commands of running extensions: (extension, command id, label).
pub fn commands() -> Vec<(String, String, String)> {
    let running: Vec<String> = with(|s| s.running.iter().map(|r| r.id.clone()).collect());
    crate::ext::installed()
        .into_iter()
        .filter(|m| running.contains(&m.id))
        .flat_map(|m| {
            let name = m.name.clone();
            m.commands.into_iter().map(move |(id, label)| (m.id.clone(), id, format!("{name}: {label}")))
        })
        .collect()
}

pub fn items() -> Vec<Item> {
    with(|s| s.items.clone())
}

pub fn panels() -> Vec<Panel> {
    with(|s| s.panels.clone())
}

/// The question to show now, if any.
pub fn ask() -> Option<Ask> {
    with(|s| s.asks.first().cloned())
}

/// The answer to the question shown: the text, or None when it was cancelled.
pub fn answer(values: Option<Vec<String>>) {
    let Some(a) = with(|s| (!s.asks.is_empty()).then(|| s.asks.remove(0))) else { return };
    let result = values
        .map(|v| {
            let typed = |f: &Field, x: &str| match f.kind.as_str() {
                "bool" => json!(x == "true"),
                "number" => x.trim().parse::<f64>().map_or(json!(x), |n| json!(n)),
                _ => json!(x),
            };
            if a.form {
                let map: serde_json::Map<String, Value> = a.fields.iter().zip(&v).map(|(f, x)| (f.id.clone(), typed(f, x))).collect();
                json!({ "values": map })
            } else {
                json!({ "value": v.first().cloned().unwrap_or_default() })
            }
        })
        .ok_or_else(|| "cancelled".to_string());
    reply(&a.ext, &a.id, result);
}

/// Tells a running extension its settings changed.
pub fn settings_changed(ext: &str) {
    if let Some(m) = crate::ext::installed().into_iter().find(|m| m.id == ext)
        && running(ext)
    {
        send(ext, "settings", crate::ext::settings_json(&m), None);
    }
}

/// The keys of running extensions' commands: (modifiers, key, extension, command).
pub fn keys() -> Vec<(u8, String, String, String)> {
    let running: Vec<String> = with(|s| s.running.iter().map(|r| r.id.clone()).collect());
    crate::ext::installed()
        .into_iter()
        .filter(|m| running.contains(&m.id))
        .flat_map(|m| {
            let id = m.id.clone();
            m.keys.into_iter().filter_map(move |(cmd, k)| crate::ext::parse_key(&k).map(|(mods, key)| (mods, key, id.clone(), cmd)))
        })
        .collect()
}

pub fn take_requests() -> Vec<Request> {
    with(|s| std::mem::take(&mut s.requests))
}

pub fn take_notices() -> Vec<String> {
    with(|s| std::mem::take(&mut s.notices))
}

pub fn log() -> Vec<String> {
    with(|s| s.log.clone())
}

/// Whether an extension's program is running.
pub fn running(ext: &str) -> bool {
    with(|s| s.running.iter().any(|r| r.id == ext))
}

/// Stops one extension: asked to shut down, then ended if it lingers.
pub fn stop(ext: &str) {
    send(ext, "shutdown", Value::Null, None);
    let r = with(|s| {
        let i = s.running.iter().position(|r| r.id == ext)?;
        Some(s.running.remove(i))
    });
    if let Some(mut r) = r {
        drop(r.stdin);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(800);
        while std::time::Instant::now() < deadline && matches!(r.child.try_wait(), Ok(None)) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = r.child.kill();
        let _ = r.child.wait();
    }
    forget(ext);
}

/// Stops them all (the app is closing).
pub fn stop_all() {
    let ids: Vec<String> = with(|s| s.running.iter().map(|r| r.id.clone()).collect());
    for id in ids {
        stop(&id);
    }
}

/// An extension was installed, updated or removed: run it again as it now is.
pub fn reload(ext: &str) {
    let was = running(ext);
    if was {
        stop(ext);
    }
    if with(|s| s.waker.is_none()) {
        return;
    }
    if let Some(m) = crate::ext::installed().into_iter().find(|m| m.id == ext && m.activation.as_deref() == Some("startup")) {
        if let Err(e) = launch(&m) {
            with(|s| s.notices.push(format!("{}: {e}", m.name)));
        }
    }
}

#[cfg(test)]
thread_local!(static SENT: std::cell::RefCell<Vec<(String, String, Value)>> = const { std::cell::RefCell::new(Vec::new()) });

/// For tests: what was sent to extensions on this thread, and messages as if from one.
#[cfg(test)]
pub fn test_sent() -> Vec<(String, String, Value)> {
    SENT.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

#[cfg(test)]
pub fn test_message(ext: &str, msg: Value) {
    handle(ext, &msg);
}

#[cfg(test)]
pub fn test_request(r: Request) {
    with(|s| s.requests.push(r));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_checked_against_permissions() {
        with(|s| {
            s.running.clear();
            s.items.clear();
        });
        // Not running: no permissions at all.
        handle("x.y", &json!({ "method": "terminal.open", "params": { "argv": ["ls"] } }));
        assert!(take_requests().is_empty(), "nothing opens without workspace.open");
        handle("x.y", &json!({ "method": "status.set", "params": { "id": "a", "text": "3 tunnels", "command": "show" } }));
        handle("x.y", &json!({ "method": "status.set", "params": { "id": "a", "text": "4 tunnels" } }));
        let mine: Vec<Item> = items().into_iter().filter(|i| i.ext == "x.y").collect();
        assert_eq!(mine.len(), 1, "one item, changed in place");
        assert_eq!(mine[0].text, "4 tunnels");
        handle("x.y", &json!({ "method": "panel.show", "params": { "id": "p", "title": "T", "markdown": "# hi" } }));
        assert!(panels().iter().any(|p| p.ext == "x.y" && p.markdown == "# hi"));
        handle("x.y", &json!({ "method": "panel.show", "params": { "id": "l", "items": [
            { "label": "db|1", "detail": "up", "bold": true, "actions": [{ "title": "Stop", "action": "stop?host=db" }, { "title": "Bad", "action": "x y" }] }
        ] } }));
        let l = panels().into_iter().find(|p| p.id == "l").unwrap().markdown;
        assert!(l.contains("| **db\\|1** | up | [Stop](sshremote:stop?host=db) |"), "{l}");
        handle("x.y", &json!({ "method": "exit" }));
        assert!(items().iter().all(|i| i.ext != "x.y") && panels().iter().all(|p| p.ext != "x.y"), "gone with it");
        assert_eq!(unescape("a%20b+c%2Fd"), "a b c/d");
        // A question waits for its answer, which goes back as the reply.
        test_sent();
        handle("x.q", &json!({ "id": 9, "method": "input.ask", "params": { "title": "Port", "prompt": "Which port?", "value": "5432" } }));
        assert_eq!(ask().map(|a| (a.title, a.fields[0].value.clone())), Some(("Port".into(), "5432".into())));
        answer(Some(vec!["6543".into()]));
        assert!(ask().is_none());
        let sent = test_sent();
        assert!(sent.iter().any(|(e, m, p)| e == "x.q" && m == "reply" && p["id"] == 9 && p["result"]["value"] == "6543"), "{sent:?}");
        // A form: its fields typed in the answer, by id; a bad field is left out.
        handle("x.q", &json!({ "id": 10, "method": "input.form", "params": { "title": "Forward", "fields": [
            { "id": "host", "type": "choice", "choices": ["db", "web"] },
            { "id": "port", "type": "number", "value": 8080 },
            { "id": "tls", "type": "bool" },
            { "id": "odd", "type": "colour" }
        ] } }));
        let a = ask().unwrap();
        assert_eq!(a.fields.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["host", "port", "tls"]);
        assert_eq!(a.fields[1].value, "8080");
        answer(Some(vec!["web".into(), "9090".into(), "true".into()]));
        let sent = test_sent();
        assert!(sent.iter().any(|(_, m, p)| m == "reply" && p["id"] == 10 && p["result"]["values"] == json!({ "host": "web", "port": 9090.0, "tls": true })), "{sent:?}");
        handle("x.q", &json!({ "id": 11, "method": "input.form", "params": { "fields": [] } }));
        assert!(ask().is_none() && test_sent().iter().any(|(_, m, p)| m == "reply" && p["id"] == 11 && p["error"].is_string()), "an empty form is refused");
    }
}
