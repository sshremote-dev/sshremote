//! Sway, through its IPC socket (i3's protocol): the program is started on a workspace of its own
//! that is left again in the same command, so its first window opens there out of sight. A held
//! window is a borderless floating one, kept in the scratchpad while its pane is not shown and
//! marked (with a mark Sway does not draw) as this process's.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::tiled::{Win, Wm};

const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
const GET_TREE: u32 = 4;
const GET_VERSION: u32 = 7;
const GET_CONFIG: u32 = 9;
const SCRATCHPAD: &str = "__i3_scratch";
const CATCH: &str = "sshremote-";
const MARK: &str = "_sshremote-";

pub(super) struct Sway {
    sock: PathBuf,
}

fn send(s: &mut UnixStream, kind: u32, payload: &str) -> std::io::Result<()> {
    let mut msg = Vec::with_capacity(14 + payload.len());
    msg.extend_from_slice(b"i3-ipc");
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&kind.to_ne_bytes());
    msg.extend_from_slice(payload.as_bytes());
    s.write_all(&msg)
}

fn receive(s: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut head = [0u8; 14];
    s.read_exact(&mut head)?;
    let len = u32::from_ne_bytes([head[6], head[7], head[8], head[9]]) as usize;
    let mut body = vec![0; len];
    s.read_exact(&mut body)?;
    Ok(body)
}

/// A string as Sway's commands read it.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Sway {
    pub(super) fn find() -> Option<Sway> {
        std::env::var_os("WAYLAND_DISPLAY")?;
        let sway = Sway { sock: PathBuf::from(std::env::var_os("SWAYSOCK")?) };
        sway.ask(GET_VERSION, "")?;
        Some(sway)
    }

    fn ask(&self, kind: u32, payload: &str) -> Option<Value> {
        let mut s = UnixStream::connect(&self.sock).ok()?;
        s.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
        send(&mut s, kind, payload).ok()?;
        serde_json::from_slice(&receive(&mut s).ok()?).ok()
    }

    fn run(&self, command: &str) -> bool {
        self.ask(RUN_COMMAND, command).and_then(|v| v.as_array().map(|all| all.iter().all(|r| r["success"].as_bool() == Some(true)))).unwrap_or(false)
    }

    fn on(&self, id: &str, commands: &str) -> bool {
        self.run(&format!("[con_id={id}] {commands}"))
    }

    /// The border a window gets as it opens, as the configuration says.
    fn default_border(&self) -> String {
        let text = self.ask(GET_CONFIG, "").and_then(|v| v["config"].as_str().map(str::to_string)).unwrap_or_default();
        default_border_in(&text)
    }
}

pub(super) fn default_border_in(config: &str) -> String {
    let line = config.lines().map(str::trim).rfind(|l| l.starts_with("default_border "));
    match line.map(|l| l["default_border ".len()..].trim()) {
        Some(how) if !how.is_empty() => format!("border {how}"),
        _ => "border normal".into(),
    }
}

/// Every window in the tree, with its workspace.
pub(super) fn windows_in(tree: &Value) -> Vec<Win> {
    fn walk(n: &Value, space: &str, order: Option<usize>, out: &mut Vec<Win>) {
        let space = if n["type"] == "workspace" { n["name"].as_str().unwrap_or("") } else { space };
        if let Some(pid) = n["pid"].as_u64() {
            let num = |v: &Value, k: &str| v[k].as_i64().unwrap_or(0) as i32;
            let (r, w) = (&n["rect"], &n["window_rect"]);
            let app = n["app_id"].as_str().or_else(|| n["window_properties"]["class"].as_str()).unwrap_or("");
            let marks: Vec<&str> = n["marks"].as_array().map(|m| m.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let tag = marks.iter().find_map(|m| m.strip_prefix(MARK)).or_else(|| space.strip_prefix(CATCH)).and_then(|r| r.split('-').next()).and_then(|p| p.parse().ok());
            let border = n["border"].as_str().unwrap_or("normal");
            out.push(Win {
                id: n["id"].as_u64().unwrap_or(0).to_string(),
                pid: pid as u32,
                space: space.to_string(),
                rect: [num(r, "x") + num(w, "x"), num(r, "y") + num(w, "y"), num(w, "width"), num(w, "height")],
                floating: n["type"] == "floating_con",
                visible: n["visible"].as_bool().unwrap_or(false),
                focused: n["focused"].as_bool().unwrap_or(false),
                name: format!("{app} {}", n["name"].as_str().unwrap_or("")),
                order,
                tag,
                look: if border == "none" { "border none".into() } else { format!("border {border} {}", n["current_border_width"].as_i64().unwrap_or(2)) },
            });
        }
        for c in n["nodes"].as_array().into_iter().flatten() {
            walk(c, space, None, out);
        }
        for (i, c) in n["floating_nodes"].as_array().into_iter().flatten().enumerate() {
            walk(c, space, Some(i), out);
        }
    }
    let mut out = Vec::new();
    walk(tree, "", None, &mut out);
    out
}

impl Wm for Sway {
    fn name(&self) -> &'static str {
        "Sway"
    }

    fn windows(&self) -> Option<Vec<Win>> {
        Some(windows_in(&self.ask(GET_TREE, "")?))
    }

    fn focused_space(&self) -> Option<String> {
        let all = self.ask(GET_WORKSPACES, "")?;
        all.as_array()?.iter().find(|w| w["focused"].as_bool() == Some(true))?["name"].as_str().map(str::to_string)
    }

    fn events(&self) -> Option<UnixStream> {
        let mut s = UnixStream::connect(&self.sock).ok()?;
        send(&mut s, SUBSCRIBE, r#"["window","workspace"]"#).ok()?;
        Some(s)
    }

    fn catch_space(&self, token: u32) -> String {
        format!("{CATCH}{}-{token}", std::process::id())
    }

    fn launch(&self, space: &str, script: &str) -> bool {
        // Sway opens a program's first window on the workspace in front when it was started; it
        // never draws the one gone to and left within a command.
        let Some(front) = self.focused_space() else { return false };
        self.run(&format!("workspace {}; exec sh {script}; workspace {}", quoted(space), quoted(&front)))
    }

    fn out_of_sight(&self, w: &Win) -> bool {
        w.space == SCRATCHPAD || w.space.starts_with(CATCH)
    }

    fn ready(&self, w: &Win) {
        // Into the scratchpad from the workspace in front: from another, Sway would bring that one
        // to the front.
        let Some(front) = self.focused_space() else { return };
        // A mark names one window only: this process's, and the window's own.
        self.on(&w.id, &format!("mark --add {MARK}{}-{}, border none, floating enable, move to workspace {}, move scratchpad", std::process::id(), w.id, quoted(&front)));
    }

    fn size(&self, id: &str, w: i32, h: i32) {
        self.on(id, &format!("resize set width {w} px height {h} px"));
    }

    fn put(&self, w: &Win, space: &str, rect: [i32; 4]) {
        let mut how = Vec::new();
        if w.space != space {
            how.push(format!("move to workspace {}", quoted(space)));
        }
        // Sized first: a floating window keeps its middle where it is when sized.
        if w.rect[2] != rect[2] || w.rect[3] != rect[3] {
            how.push(format!("resize set width {} px height {} px", rect[2], rect[3]));
        }
        how.push(format!("move absolute position {} {}", rect[0], rect[1]));
        self.on(&w.id, &how.join(", "));
    }

    fn hide(&self, w: &Win) {
        // As in `ready`; one on a workspace out of sight is out of sight already.
        let Some(front) = self.focused_space() else { return };
        if w.space == front {
            self.on(&w.id, "move scratchpad");
        } else if w.visible {
            self.on(&w.id, &format!("move to workspace {}, move scratchpad", quoted(&front)));
        }
    }

    fn raise(&self, w: &Win, space: &str, rect: [i32; 4]) {
        // Sway raises a floating window only as it is focused, or as it comes onto its workspace
        // (by way of the scratchpad, as in `hide`).
        if self.focused_space().as_deref() == Some(space) {
            self.on(&w.id, &format!("move scratchpad, move to workspace {}, move absolute position {} {}", quoted(space), rect[0], rect[1]));
        }
    }

    fn focus(&self, id: &str) {
        self.on(id, "focus");
    }

    fn close(&self, id: &str) {
        self.on(id, &format!("unmark {MARK}{}-{id}, kill", std::process::id()));
    }

    fn free(&self, w: &Win, space: &str, was: Option<&Win>) {
        let back = match was {
            Some(v) if v.floating => format!("move absolute position {} {}, {}", v.rect[0], v.rect[1], v.look),
            Some(v) => format!("floating disable, {}", v.look),
            None => format!("floating disable, {}", self.default_border()),
        };
        let unmark = w.tag.map(|p| format!("unmark {MARK}{p}-{}, ", w.id)).unwrap_or_default();
        self.on(&w.id, &format!("{unmark}move to workspace {}, {back}", quoted(space)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_gives_each_window_its_workspace_place_and_stack() {
        let tree = serde_json::json!({"id": 1, "type": "root", "nodes": [
            {"type": "output", "name": "__i3", "nodes": [{"type": "workspace", "name": "__i3_scratch", "nodes": [], "floating_nodes": [
                {"id": 9, "type": "floating_con", "pid": 50, "app_id": "foot", "name": "held", "marks": ["_sshremote-123-9"], "border": "none",
                 "rect": {"x": 1, "y": 2, "width": 30, "height": 40}, "window_rect": {"x": 0, "y": 0, "width": 30, "height": 40}, "visible": false}]}]},
            {"type": "output", "name": "HEADLESS-1", "nodes": [{"type": "workspace", "name": "1", "nodes": [
                {"id": 5, "type": "con", "pid": 28, "app_id": null, "window_properties": {"class": "XTerm"}, "name": "xterm", "border": "normal", "current_border_width": 2,
                 "rect": {"x": 0, "y": 25, "width": 1280, "height": 775}, "window_rect": {"x": 2, "y": 0, "width": 1276, "height": 773}, "visible": true, "focused": true}],
              "floating_nodes": [
                {"id": 7, "type": "floating_con", "pid": 60, "app_id": "a", "name": "under", "rect": {"x": 0, "y": 0, "width": 9, "height": 9}, "window_rect": {"x": 0, "y": 0, "width": 9, "height": 9}},
                {"id": 8, "type": "floating_con", "pid": 61, "app_id": "b", "name": "over", "rect": {"x": 0, "y": 0, "width": 9, "height": 9}, "window_rect": {"x": 0, "y": 0, "width": 9, "height": 9}}]},
              {"type": "workspace", "name": "sshremote-321-4", "nodes": [
                {"id": 11, "type": "con", "pid": 70, "app_id": "calc", "name": "Calc", "rect": {"x": 0, "y": 0, "width": 9, "height": 9}, "window_rect": {"x": 0, "y": 0, "width": 9, "height": 9}}]}]}]});
        let wins = windows_in(&tree);
        let by = |id: &str| wins.iter().find(|w| w.id == id).unwrap().clone();
        let held = by("9");
        assert_eq!((held.space.as_str(), held.tag, held.floating, held.look.as_str()), (SCRATCHPAD, Some(123), true, "border none"));
        let xterm = by("5");
        assert_eq!((xterm.rect, xterm.name.as_str(), xterm.focused, xterm.look.as_str()), ([2, 25, 1276, 773], "XTerm xterm", true, "border normal 2"));
        assert!(by("8").order > by("7").order, "the later one is over");
        assert_eq!((by("11").space.as_str(), by("11").tag), ("sshremote-321-4", Some(321)));
        assert_eq!(quoted(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(default_border_in("font x\ndefault_border pixel 2\n"), "border pixel 2");
        assert_eq!(default_border_in("exec foot\n"), "border normal");
    }
}
