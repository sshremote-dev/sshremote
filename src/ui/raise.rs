//! Bringing a window forward by its title under Wayland, where only the compositor may and some
//! have a way to be asked: Sway (swaymsg), Hyprland (hyprctl) and KDE Plasma (KWin's window search
//! on D-Bus, as KRunner uses it).
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// None when no compositor here can be asked; else whether the window came forward.
pub fn by_title(title: &str) -> Option<Result<(), String>> {
    let run = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_default();
    if let Some(sock) = std::env::var_os("SWAYSOCK").map(PathBuf::from).or_else(|| newest(&run, |n| n.starts_with("sway-ipc.") && n.ends_with(".sock"))) {
        return Some(sway(&sock, title));
    }
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().filter(|s| !s.is_empty()).or_else(|| Some(newest(&run.join("hypr"), |_| true)?.file_name()?.to_string_lossy().into_owned()));
    if let Some(sig) = sig {
        return Some(hyprland(&sig, title));
    }
    kwin(title)
}

/// The newest entry of `dir` whose name passes `keep`.
fn newest(dir: &Path, keep: impl Fn(&str) -> bool) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| keep(&e.file_name().to_string_lossy())).max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()).map(|e| e.path())
}

fn json(cmd: &mut Command) -> Option<Value> {
    cmd.output().ok().and_then(|o| serde_json::from_slice(&o.stdout).ok())
}

const MISSING: &str = "no window by that title; call state again";

/// The id of the window titled `title` in Sway's tree (a program's window has a process; a
/// workspace or an output named alike has not).
fn sway_window(v: &Value, title: &str) -> Option<u64> {
    if v.get("pid").is_some() && v.get("name").and_then(Value::as_str) == Some(title) {
        return v.get("id")?.as_u64();
    }
    ["nodes", "floating_nodes"].iter().filter_map(|k| v.get(*k)?.as_array()).flatten().find_map(|n| sway_window(n, title))
}

/// The address of the window titled `title` among Hyprland's clients.
fn hypr_window<'a>(clients: &'a Value, title: &str) -> Option<&'a str> {
    clients.as_array()?.iter().find(|c| c.get("title").and_then(Value::as_str) == Some(title))?.get("address")?.as_str()
}

fn sway(sock: &Path, title: &str) -> Result<(), String> {
    let tree = json(Command::new("swaymsg").arg("-s").arg(sock).args(["-t", "get_tree"])).ok_or("Sway did not answer")?;
    let id = sway_window(&tree, title).ok_or(MISSING)?;
    let said = json(Command::new("swaymsg").arg("-s").arg(sock).arg(format!("[con_id={id}] focus"))).ok_or("Sway did not answer")?;
    match said.get(0).and_then(|r| r.get("success")).and_then(Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(said.get(0).and_then(|r| r.get("error")).and_then(Value::as_str).unwrap_or("Sway would not focus it").to_string()),
    }
}

fn hyprland(sig: &str, title: &str) -> Result<(), String> {
    let hyprctl = || {
        let mut c = Command::new("hyprctl");
        c.env("HYPRLAND_INSTANCE_SIGNATURE", sig);
        c
    };
    let clients = json(hyprctl().args(["-j", "clients"])).ok_or("Hyprland did not answer")?;
    let address = hypr_window(&clients, title).ok_or(MISSING)?;
    let out = hyprctl().args(["dispatch", "focuswindow", &format!("address:{address}")]).output().map_err(|e| e.to_string())?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if said == "ok" { Ok(()) } else { Err(format!("Hyprland: {said}")) }
}

/// KWin's window search (what KRunner shows of open windows): a match whose text is the title,
/// run to activate it.
fn kwin(title: &str) -> Option<Result<(), String>> {
    type Match = (String, String, String, i32, f64, std::collections::HashMap<String, zbus::OwnedVariant>);
    zbus::block_on(async {
        let conn = zbus::connection::Builder::session().method_timeout(std::time::Duration::from_secs(2)).build().await.ok()?;
        let runner = zbus::Proxy::new(&conn, "org.kde.KWin", "/WindowsRunner", "org.kde.krunner1").await.ok()?;
        let found: Vec<Match> = runner.call("Match", &(title,)).await.ok()?;
        let Some(m) = found.iter().find(|m| m.1 == title) else { return Some(Err(MISSING.to_string())) };
        Some(runner.call::<_, _, ()>("Run", &(m.0.as_str(), "")).await.map_err(|e| e.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_found_by_its_title_and_not_a_workspace_named_alike() {
        let tree = serde_json::json!({"id": 1, "name": "root", "nodes": [
            {"id": 2, "name": "mail", "type": "workspace", "nodes": [{"id": 7, "name": "editor", "pid": 40}],
             "floating_nodes": [{"id": 9, "name": "mail", "pid": 41}]}
        ]});
        assert_eq!(sway_window(&tree, "mail"), Some(9));
        assert_eq!(sway_window(&tree, "editor"), Some(7));
        assert_eq!(sway_window(&tree, "root"), None);
        let clients = serde_json::json!([{"title": "editor", "address": "0xa1"}, {"title": "mail", "address": "0xb2"}]);
        assert_eq!(hypr_window(&clients, "mail"), Some("0xb2"));
        assert_eq!(hypr_window(&clients, "mai"), None);
    }
}
