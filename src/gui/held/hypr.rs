//! Hyprland, through its sockets: the program is started with an exec rule that opens its first
//! window on a special workspace of its own (never shown) and floating. A held window has no
//! border, rounding, shadow or animation, and waits on this process's special workspace while its
//! pane is not shown. Its window list is in stacking order.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::tiled::{Win, Wm};

const SPECIAL: &str = "special:sshremote-";
/// What a held window looks like, and how it looks again when let go.
const LOOK: [&str; 5] = ["no_anim 1", "border_size 0", "rounding 0", "no_shadow 1", "no_blur 1"];

pub(super) struct Hypr {
    dir: PathBuf,
}

impl Hypr {
    pub(super) fn find() -> Option<Hypr> {
        std::env::var_os("WAYLAND_DISPLAY")?;
        let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_default();
        let dir = [runtime.join("hypr").join(&sig), PathBuf::from("/tmp/hypr").join(&sig)].into_iter().find(|d| d.join(".socket.sock").exists())?;
        let hypr = Hypr { dir };
        hypr.json("version")?;
        Some(hypr)
    }

    fn ask(&self, request: &str) -> Option<String> {
        let mut s = UnixStream::connect(self.dir.join(".socket.sock")).ok()?;
        s.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
        s.write_all(request.as_bytes()).ok()?;
        let mut out = String::new();
        s.read_to_string(&mut out).ok()?;
        Some(out)
    }

    fn json(&self, what: &str) -> Option<Value> {
        serde_json::from_str(&self.ask(&format!("j/{what}"))?).ok()
    }

    /// Dispatches, in one request (none may hold a ';').
    fn batch(&self, all: &[String]) -> bool {
        let line = all.iter().map(|d| format!("dispatch {d}")).collect::<Vec<_>>().join(";");
        self.ask(&format!("[[BATCH]]{line}")).is_some_and(|r| !r.contains("err"))
    }

    fn looks(&self, id: &str, on: bool) -> Vec<String> {
        LOOK.iter()
            .map(|l| {
                let (prop, value) = l.split_once(' ').unwrap_or((l, ""));
                format!("setprop address:{id} {prop} {}", if on { value } else { "unset" })
            })
            .collect()
    }
}

/// The windows in `clients`, seen or not as `monitors` says.
pub(super) fn windows_in(clients: &Value, monitors: &Value) -> Vec<Win> {
    let shown: Vec<i64> = monitors
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| [m["activeWorkspace"]["id"].as_i64(), m["specialWorkspace"]["id"].as_i64().filter(|&id| id != 0)])
        .flatten()
        .collect();
    clients
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, c)| c["mapped"].as_bool() != Some(false))
        .map(|(i, c)| {
            let num = |v: &Value, at: usize| v.get(at).and_then(Value::as_i64).unwrap_or(0) as i32;
            let ws = &c["workspace"];
            let id = ws["id"].as_i64().unwrap_or(0);
            let name = ws["name"].as_str().unwrap_or("");
            Win {
                id: c["address"].as_str().unwrap_or("").to_string(),
                pid: c["pid"].as_i64().unwrap_or(0).max(0) as u32,
                // Moves take a workspace by its number, a special one by its name.
                space: if id > 0 { id.to_string() } else { name.to_string() },
                rect: [num(&c["at"], 0), num(&c["at"], 1), num(&c["size"], 0), num(&c["size"], 1)],
                floating: c["floating"].as_bool().unwrap_or(false),
                full: c["fullscreen"].as_i64().unwrap_or(0) > 0,
                visible: shown.contains(&id) && !c["hidden"].as_bool().unwrap_or(false),
                focused: c["focusHistoryID"].as_i64() == Some(0),
                name: format!("{} {}", c["class"].as_str().unwrap_or(""), c["title"].as_str().unwrap_or("")),
                order: Some(i),
                tag: name.strip_prefix(SPECIAL).and_then(|r| r.split('-').next()).and_then(|p| p.parse().ok()),
                look: String::new(),
            }
        })
        .collect()
}

impl Wm for Hypr {
    fn name(&self) -> &'static str {
        "Hyprland"
    }

    fn windows(&self) -> Option<Vec<Win>> {
        Some(windows_in(&self.json("clients")?, &self.json("monitors")?))
    }

    fn focused_space(&self) -> Option<String> {
        self.json("activeworkspace")?["id"].as_i64().map(|id| id.to_string())
    }

    fn events(&self) -> Option<UnixStream> {
        UnixStream::connect(self.dir.join(".socket2.sock")).ok()
    }

    fn catch_space(&self, token: u32) -> String {
        format!("{SPECIAL}{}-{token}", std::process::id())
    }

    fn launch(&self, space: &str, script: &str) -> bool {
        // The rule goes with the process started (each step after it takes its place), so its first
        // window opens there.
        self.ask(&format!("dispatch exec [workspace {space} silent; float] exec sh {script}")).is_some_and(|r| r.trim() == "ok")
    }

    fn out_of_sight(&self, w: &Win) -> bool {
        w.space.starts_with(SPECIAL)
    }

    fn ready(&self, w: &Win) {
        let mut all = self.looks(&w.id, true);
        if !w.floating {
            all.push(format!("setfloating address:{}", w.id));
        }
        if !self.out_of_sight(w) {
            all.push(format!("movetoworkspacesilent {SPECIAL}{},address:{}", std::process::id(), w.id));
        }
        self.batch(&all);
    }

    fn size(&self, id: &str, w: i32, h: i32) {
        self.batch(&[format!("resizewindowpixel exact {w} {h},address:{id}")]);
    }

    fn put(&self, w: &Win, space: &str, rect: [i32; 4]) {
        let mut all = Vec::new();
        if w.space != space {
            all.push(format!("movetoworkspacesilent {space},address:{}", w.id));
        }
        if w.rect[2] != rect[2] || w.rect[3] != rect[3] {
            all.push(format!("resizewindowpixel exact {} {},address:{}", rect[2], rect[3], w.id));
        }
        all.push(format!("movewindowpixel exact {} {},address:{}", rect[0], rect[1], w.id));
        self.batch(&all);
    }

    fn hide(&self, w: &Win) {
        // One on a workspace out of sight (with this window) is out of sight already.
        if w.visible {
            self.batch(&[format!("movetoworkspacesilent {SPECIAL}{},address:{}", std::process::id(), w.id)]);
        }
    }

    fn raise(&self, w: &Win, _: &str, _: [i32; 4]) {
        self.batch(&[format!("alterzorder top,address:{}", w.id)]);
    }

    fn focus(&self, id: &str) {
        self.batch(&[format!("focuswindow address:{id}")]);
    }

    fn over_full(&self) -> bool {
        true
    }

    fn close(&self, id: &str) {
        self.batch(&[format!("closewindow address:{id}")]);
    }

    fn free(&self, w: &Win, space: &str, was: Option<&Win>) {
        let mut all = vec![format!("movetoworkspacesilent {space},address:{}", w.id)];
        all.extend(self.looks(&w.id, false));
        match was {
            Some(v) if v.floating => {
                all.push(format!("resizewindowpixel exact {} {},address:{}", v.rect[2], v.rect[3], w.id));
                all.push(format!("movewindowpixel exact {} {},address:{}", v.rect[0], v.rect[1], w.id));
            }
            _ => all.push(format!("settiled address:{}", w.id)),
        }
        self.batch(&all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_read_in_stacking_order_and_seen_where_their_workspace_is() {
        let clients = serde_json::json!([
            {"address": "0xa", "mapped": true, "hidden": false, "at": [21, 21], "size": [613, 758], "workspace": {"id": 2, "name": "2"}, "floating": false,
             "pid": 88, "class": "foot", "title": "foot", "focusHistoryID": 0},
            {"address": "0xb", "mapped": true, "hidden": false, "at": [290, 150], "size": [700, 500], "workspace": {"id": -98, "name": "special:sshremote-55-3"},
             "floating": true, "pid": 301, "class": "org.gnome.Calculator", "title": "Calculator", "focusHistoryID": 2},
            {"address": "0xc", "mapped": true, "hidden": false, "at": [0, 0], "size": [10, 10], "workspace": {"id": 3, "name": "3"}, "floating": true,
             "pid": 302, "class": "x", "title": "y", "focusHistoryID": 1}
        ]);
        let monitors = serde_json::json!([{"activeWorkspace": {"id": 2, "name": "2"}, "specialWorkspace": {"id": 0, "name": ""}}]);
        let wins = windows_in(&clients, &monitors);
        assert_eq!(wins.len(), 3);
        assert_eq!((wins[0].space.as_str(), wins[0].rect, wins[0].visible, wins[0].focused), ("2", [21, 21, 613, 758], true, true));
        assert_eq!((wins[1].space.as_str(), wins[1].tag, wins[1].visible, wins[1].name.as_str()), ("special:sshremote-55-3", Some(55), false, "org.gnome.Calculator Calculator"));
        assert!(!wins[2].visible, "on a workspace not in front");
        assert!(wins[1].order > wins[0].order);
    }
}
