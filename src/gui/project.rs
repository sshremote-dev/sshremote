//! Projects: the tabs, splits and what each pane runs, kept by name to open again; and the last
//! session, kept as the window works so the next start can open it. No passwords are kept: they
//! are asked for again.

use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(d) = TEST_DIR.with(|d| d.borrow().clone()) {
        return Some(d);
    }
    crate::hosts::config_dir()
}

#[cfg(test)]
thread_local!(pub(super) static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) });

fn projects() -> Option<PathBuf> {
    dir().map(|d| d.join("projects"))
}

/// A name that is also a safe file name.
fn file_name(name: &str) -> Option<String> {
    let n = name.trim();
    let ok = !n.is_empty() && n.len() <= 80 && n.chars().all(|c| c.is_alphanumeric() || " -_.()".contains(c)) && !n.starts_with('.');
    ok.then(|| format!("{n}.json"))
}

/// The saved projects by name, the newest first.
pub fn list() -> Vec<String> {
    let Some(d) = projects() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(d) else { return Vec::new() };
    let mut out: Vec<(std::time::SystemTime, String)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().strip_suffix(".json")?.to_string();
            Some((e.metadata().and_then(|m| m.modified()).ok()?, name))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, n)| n).collect()
}

pub fn save(name: &str, layout: &serde_json::Value) -> Result<(), String> {
    let f = file_name(name).ok_or("A project's name is letters, digits, spaces and - _ . ( )")?;
    let d = projects().ok_or("no settings folder")?;
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join(f), serde_json::to_string_pretty(layout).unwrap_or_default()).map_err(|e| e.to_string())
}

pub fn load(name: &str) -> Result<serde_json::Value, String> {
    let f = file_name(name).ok_or("no such project")?;
    let text = std::fs::read_to_string(projects().ok_or("no settings folder")?.join(f)).map_err(|e| format!("The project {name}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("The project {name} could not be read: {e}"))
}

pub fn remove(name: &str) -> Result<(), String> {
    let f = file_name(name).ok_or("no such project")?;
    std::fs::remove_file(projects().ok_or("no settings folder")?.join(f)).map_err(|e| e.to_string())
}

/// What a saved layout holds, for a list: "3 tabs: ubuntu, PowerShell, mac".
pub fn summary(layout: &serde_json::Value) -> String {
    let tabs = layout.get("tabs").and_then(|t| t.as_array()).map_or(0, Vec::len);
    let mut names: Vec<&str> = layout
        .get("panes")
        .and_then(|p| p.as_object())
        .map(|p| p.values().filter_map(|v| v.get("label").and_then(|l| l.as_str())).collect())
        .unwrap_or_default();
    names.dedup();
    let more = if names.len() > 4 { ", ..." } else { "" };
    names.truncate(4);
    format!("{tabs} tab{}: {}{more}", if tabs == 1 { "" } else { "s" }, names.join(", "))
}

fn last_file() -> Option<PathBuf> {
    dir().map(|d| d.join("session.json"))
}

/// Keeps the open tabs as the last session; none open forgets it.
pub fn keep_last(layout: Option<&serde_json::Value>) {
    let Some(f) = last_file() else { return };
    match layout.filter(|l| l.get("tabs").and_then(|t| t.as_array()).is_some_and(|t| !t.is_empty())) {
        Some(l) => {
            if let Some(d) = f.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(f, serde_json::to_string_pretty(l).unwrap_or_default());
        }
        None => {
            let _ = std::fs::remove_file(f);
        }
    }
}

/// The last session's tabs, when they are to open at start.
pub fn last() -> Option<serde_json::Value> {
    if !reopen() {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(last_file()?).ok()?).ok()
}

/// Whether the last session's tabs open at start (on unless turned off).
pub fn reopen() -> bool {
    crate::hosts::setting("reopen_tabs").as_deref() != Some("off")
}

pub fn set_reopen(on: bool) {
    crate::hosts::set_setting("reopen_tabs", if on { "on" } else { "off" });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_are_kept_by_name() {
        let d = std::env::temp_dir().join(format!("sshremote-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        TEST_DIR.with(|t| *t.borrow_mut() = Some(d.clone()));
        let layout = serde_json::json!({ "tabs": [{ "root": { "pane": 1 }, "active": 1 }], "panes": { "1": { "label": "ubuntu" } } });
        save("Web site (dev)", &layout).unwrap();
        assert!(save("../evil", &layout).is_err() && save("", &layout).is_err());
        assert_eq!(list(), vec!["Web site (dev)".to_string()]);
        assert_eq!(load("Web site (dev)").unwrap(), layout);
        assert_eq!(summary(&layout), "1 tab: ubuntu");
        keep_last(Some(&layout));
        assert!(d.join("session.json").is_file());
        keep_last(Some(&serde_json::json!({ "tabs": [] })));
        assert!(!d.join("session.json").exists(), "no tabs forgets the session");
        remove("Web site (dev)").unwrap();
        assert!(list().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
