//! Projects: a window's tabs kept under a name, to open again as they were: each pane's computer
//! and what it showed (terminal, screen, files, a web app), and how the panes divide a tab. Never
//! a password: one is taken from the password store, or asked for with the computer named, as
//! when it is opened by hand. The tabs open when the app is closed are kept as the last session.
//!
//! Kept in `projects.json` in the settings folder:
//!   [ { "name": "Release", "saved": 1794600000, "tabs": [ TREE, ... ] } ]
//! where a TREE is { "split": "beside" | "below", "ratio": 0.5, "a": TREE, "b": TREE } or one
//! pane: { "host": ADDRESS, "port": "", "mode": "terminal" | "screen" | "files" },
//! { "here": MODE }, { "shell": PROFILE }, or { "web": URL, "label": NAME }.

use serde_json::{Value, json};

use super::workspace::PaneMode;

/// What a pane opens.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    /// A computer: an SSH host (`user@host`, an alias) or `sshremote://ID@RELAY`.
    Host { address: String, port: String, mode: PaneMode },
    /// This computer's screen or files.
    Here { mode: PaneMode },
    /// A shell of this computer, by its profile's name (PowerShell, bash...).
    Shell { profile: String },
    /// A web app's page.
    Web { url: String, label: String },
}

/// How a tab's panes divide it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tree {
    Leaf(Item),
    Split { below: bool, ratio: f32, a: Box<Tree>, b: Box<Tree> },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Project {
    pub name: String,
    /// When it was kept, in seconds since 1970.
    pub saved: u64,
    pub tabs: Vec<Tree>,
}

/// The name the tabs of the window closed last are kept under.
pub(crate) const LAST: &str = "Last session";

impl Tree {
    pub(crate) fn panes(&self) -> usize {
        match self {
            Tree::Leaf(_) => 1,
            Tree::Split { a, b, .. } => a.panes() + b.panes(),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Tree::Split { below, ratio, a, b } => json!({ "split": if *below { "below" } else { "beside" }, "ratio": ratio, "a": a.to_json(), "b": b.to_json() }),
            Tree::Leaf(Item::Host { address, port, mode }) => json!({ "host": address, "port": port, "mode": mode.key() }),
            Tree::Leaf(Item::Here { mode }) => json!({ "here": mode.key() }),
            Tree::Leaf(Item::Shell { profile }) => json!({ "shell": profile }),
            Tree::Leaf(Item::Web { url, label }) => json!({ "web": url, "label": label }),
        }
    }

    fn from_json(v: &Value) -> Option<Tree> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        if let Some(split) = s("split") {
            let ratio = v.get("ratio").and_then(Value::as_f64).unwrap_or(0.5).clamp(0.05, 0.95) as f32;
            return Some(Tree::Split { below: split == "below", ratio, a: Box::new(Tree::from_json(v.get("a")?)?), b: Box::new(Tree::from_json(v.get("b")?)?) });
        }
        let mode = |k: &str| s(k).and_then(|m| PaneMode::from_key(&m));
        Some(Tree::Leaf(if let Some(address) = s("host").filter(|a| !a.trim().is_empty()) {
            Item::Host { address, port: s("port").unwrap_or_default(), mode: mode("mode").unwrap_or(PaneMode::Terminal) }
        } else if s("here").is_some() {
            Item::Here { mode: mode("here")? }
        } else if let Some(profile) = s("shell") {
            Item::Shell { profile }
        } else {
            let url = s("web").filter(|u| u.starts_with("https://") || u.starts_with("http://"))?;
            Item::Web { url, label: s("label").unwrap_or_default() }
        }))
    }
}

fn file() -> Option<std::path::PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("projects.json"))
}

fn read_from(text: &str) -> Vec<Project> {
    let v: Value = serde_json::from_str(text).unwrap_or_default();
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let name = p.get("name")?.as_str()?.trim().to_string();
                    let tabs: Vec<Tree> = p.get("tabs")?.as_array()?.iter().filter_map(Tree::from_json).collect();
                    (!name.is_empty() && !tabs.is_empty()).then(|| Project { name, saved: p.get("saved").and_then(Value::as_u64).unwrap_or(0), tabs })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The projects kept, the last session first, then the newest.
pub(crate) fn list() -> Vec<Project> {
    let mut all = file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| read_from(&t)).unwrap_or_default();
    all.sort_by_key(|p| (p.name != LAST, std::cmp::Reverse(p.saved)));
    all
}

pub(crate) fn get(name: &str) -> Option<Project> {
    list().into_iter().find(|p| p.name == name)
}

fn write(all: &[Project]) -> Result<(), String> {
    let f = file().ok_or("no settings folder")?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let v: Vec<Value> = all.iter().map(|p| json!({ "name": p.name, "saved": p.saved, "tabs": p.tabs.iter().map(Tree::to_json).collect::<Vec<_>>() })).collect();
    let tmp = f.with_extension("json.new");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &f).map_err(|e| e.to_string())
}

/// Keeps `tabs` under `name`, in place of one of that name.
pub(crate) fn save(name: &str, tabs: Vec<Tree>) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A project needs a name".into());
    }
    if tabs.is_empty() {
        return Err("There are no tabs to keep".into());
    }
    let mut all = list();
    all.retain(|p| p.name != name);
    all.push(Project { name: name.to_string(), saved: crate::sys::now_secs(), tabs });
    write(&all)
}

pub(crate) fn remove(name: &str) -> Result<(), String> {
    let mut all = list();
    all.retain(|p| p.name != name);
    write(&all)
}

/// Whether the last session's tabs open with the app (Settings, Connections).
pub(crate) fn reopen_at_start() -> bool {
    crate::hosts::setting("reopen_last").as_deref() == Some("yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_read_back_as_it_was_kept() {
        let tab = Tree::Split {
            below: false,
            ratio: 0.4,
            a: Box::new(Tree::Leaf(Item::Host { address: "dev@build".into(), port: "2222".into(), mode: PaneMode::Terminal })),
            b: Box::new(Tree::Split {
                below: true,
                ratio: 0.5,
                a: Box::new(Tree::Leaf(Item::Web { url: "https://translate.google.com".into(), label: "Translate".into() })),
                b: Box::new(Tree::Leaf(Item::Host { address: "sshremote://482913075@relay.sshremote.dev".into(), port: String::new(), mode: PaneMode::Screen })),
            }),
        };
        let others = vec![Tree::Leaf(Item::Here { mode: PaneMode::Files }), Tree::Leaf(Item::Shell { profile: "PowerShell".into() })];
        let mut tabs = vec![tab];
        tabs.extend(others);
        let text = serde_json::to_string(&vec![json!({ "name": "Release", "saved": 5, "tabs": tabs.iter().map(Tree::to_json).collect::<Vec<_>>() })]).unwrap();
        let back = read_from(&text);
        assert_eq!(back, vec![Project { name: "Release".into(), saved: 5, tabs: tabs.clone() }]);
        assert_eq!(back[0].tabs[0].panes(), 3);
        // A pane this build cannot read is left out, not the whole project.
        let odd = r#"[{"name":"X","tabs":[{"web":"ftp://x"},{"here":"screen"}]}]"#;
        assert_eq!(read_from(odd)[0].tabs, vec![Tree::Leaf(Item::Here { mode: PaneMode::Screen })]);
    }
}
