//! The computers the app knows, as one list: the window's home, the web gateway and the terminal
//! picker all read it. A computer is reached by SSH (an alias of ~/.ssh/config, or `user@host`) or
//! by its remote access ID (`sshremote://ID@RELAY`); its address is also the key of its password
//! in the system's password store. This computer itself is not in the list: it is always there.
//!
//! Kept in `computers.json` in the settings folder. The aliases of ~/.ssh/config are read live and
//! merged by address; the older `recent.txt`, `favorites.txt` and `seen.txt` are read into it once.

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::tunnels::Tunnel;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Computer {
    /// How it is reached: an alias, `user@host`, or `sshremote://ID@RELAY`.
    pub address: String,
    /// The SSH port, when another than 22 or the config's.
    pub port: Option<u16>,
    /// A name given to it; empty: its address is its name.
    pub name: String,
    pub favorite: bool,
    /// When it was last opened, in seconds since 1970; 0: never.
    pub last_used: u64,
    /// An alias of ~/.ssh/config, whose user, port and key are kept there.
    pub in_ssh_config: bool,
    pub tunnels: Vec<Tunnel>,
}

impl Computer {
    /// Its ID, for a computer reached by its remote access ID.
    pub fn id(&self) -> Option<&str> {
        let rest = self.address.strip_prefix("sshremote://")?;
        Some(rest.split('@').next().unwrap_or(rest))
    }

    /// What it is called in lists.
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        match self.id() {
            Some(id) => format!("ID {}", spaced(id)),
            None => self.address.clone(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "address": self.address,
            "port": self.port,
            "name": self.name,
            "favorite": self.favorite,
            "last": self.last_used,
            "tunnels": self.tunnels.iter().map(Tunnel::to_json).collect::<Vec<_>>(),
        })
    }

    fn from_json(v: &Value) -> Option<Computer> {
        let address = v.get("address")?.as_str()?.trim().to_string();
        (!address.is_empty()).then(|| Computer {
            address,
            port: v.get("port").and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).filter(|&p| p > 0),
            name: v.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            favorite: v.get("favorite").and_then(Value::as_bool).unwrap_or(false),
            last_used: v.get("last").and_then(Value::as_u64).unwrap_or(0),
            in_ssh_config: false,
            tunnels: v.get("tunnels").and_then(Value::as_array).map(|a| a.iter().filter_map(Tunnel::from_json).collect()).unwrap_or_default(),
        })
    }
}

/// "123 456 789", to read out.
pub fn spaced(id: &str) -> String {
    id.as_bytes().chunks(3).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// The relay this computer goes through for IDs.
pub fn relay() -> String {
    #[cfg(feature = "remote")]
    return crate::access::relay();
    #[cfg(not(feature = "remote"))]
    return "sshremote.dev".into();
}

/// The list as kept: read once, written on every change.
static KEPT: Mutex<Option<Vec<Computer>>> = Mutex::new(None);

#[cfg(test)]
thread_local!(static TEST_FILE: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) });

fn file() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(f) = TEST_FILE.with(|f| f.borrow().clone()) {
        return Some(f);
    }
    crate::hosts::config_dir().map(|d| d.join("computers.json"))
}

fn with<R>(f: impl FnOnce(&mut Vec<Computer>) -> R) -> R {
    // A test reads its own file each time and leaves the kept list alone.
    #[cfg(test)]
    if TEST_FILE.with(|f| f.borrow().is_some()) {
        return f(&mut read());
    }
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    f(kept.get_or_insert_with(read))
}

fn read() -> Vec<Computer> {
    let Some(path) = file() else { return Vec::new() };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(_) => from_older_files(path.parent()),
    }
}

/// The list in `computers.json`'s words.
pub fn parse(text: &str) -> Vec<Computer> {
    serde_json::from_str::<Value>(text).ok().and_then(|v| v.as_array().map(|a| a.iter().filter_map(Computer::from_json).collect())).unwrap_or_default()
}

/// Takes in `more` (a backup's): each replaces the one at its address, the others stay.
pub fn merge(more: Vec<Computer>) {
    if more.is_empty() {
        return;
    }
    with(|l| {
        for c in more {
            match l.iter_mut().find(|k| k.address == c.address) {
                Some(k) => *k = c,
                None => l.push(c),
            }
        }
        write(l);
    });
}

/// The lists the app kept before this one: recent hosts with their ports, favorites, and when
/// each was last opened.
pub fn from_older_files(dir: Option<&std::path::Path>) -> Vec<Computer> {
    let Some(dir) = dir else { return Vec::new() };
    let lines = |name: &str| -> Vec<String> {
        std::fs::read_to_string(dir.join(name)).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
    };
    let seen: std::collections::HashMap<String, u64> =
        lines("seen.txt").iter().filter_map(|l| l.rsplit_once(' ').and_then(|(h, t)| Some((h.to_string(), t.parse().ok()?)))).collect();
    let mut out: Vec<Computer> = Vec::new();
    for l in lines("recent.txt") {
        let mut it = l.split_whitespace();
        let Some(address) = it.next() else { continue };
        if out.iter().all(|c| c.address != address) {
            let port = it.next().and_then(|p| p.parse().ok()).filter(|&p: &u16| p > 0 && p != 22);
            out.push(Computer { address: address.into(), port, last_used: seen.get(address).copied().unwrap_or(0), ..Default::default() });
        }
    }
    for address in lines("favorites.txt") {
        match out.iter_mut().find(|c| c.address == address) {
            Some(c) => c.favorite = true,
            None => out.push(Computer { last_used: seen.get(&address).copied().unwrap_or(0), address, favorite: true, ..Default::default() }),
        }
    }
    out
}

fn write(list: &[Computer]) {
    let Some(path) = file() else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text = serde_json::to_string_pretty(&Value::Array(list.iter().map(Computer::to_json).collect())).unwrap_or_default();
    // Written whole, then put in place: a crash never leaves half a list.
    let part = path.with_extension("json.part");
    if std::fs::write(&part, text).is_ok() {
        let _ = std::fs::rename(&part, &path);
    }
}

/// Every computer: favorites first, then by when it was last opened, then the aliases of
/// ~/.ssh/config never opened, in the file's order.
pub fn all() -> Vec<Computer> {
    let mut list = with(|l| l.clone());
    for alias in crate::hosts::ssh_config_hosts() {
        match list.iter_mut().find(|c| c.address == alias) {
            Some(c) => c.in_ssh_config = true,
            None => list.push(Computer { address: alias, in_ssh_config: true, ..Default::default() }),
        }
    }
    // A stable sort keeps the config's order among the never opened.
    list.sort_by_key(|c| (!c.favorite, std::cmp::Reverse(c.last_used)));
    list
}

/// The computer at `address`, if the list has it.
pub fn get(address: &str) -> Option<Computer> {
    all().into_iter().find(|c| c.address == address)
}

/// Changes the computer at `address` (added first if new) and keeps the list.
pub fn update(address: &str, change: impl FnOnce(&mut Computer)) {
    let address = address.trim();
    if address.is_empty() {
        return;
    }
    with(|l| {
        let i = match l.iter().position(|c| c.address == address) {
            Some(i) => i,
            None => {
                l.push(Computer { address: address.into(), ..Default::default() });
                l.len() - 1
            }
        };
        change(&mut l[i]);
        write(l);
    });
}

/// Moves what is kept of the computer at `was` (its name, star, tunnels, when it was opened)
/// to `now`, its new address.
pub fn rename(was: &str, now: &str) {
    with(|l| {
        if l.iter().any(|c| c.address == now) {
            l.retain(|c| c.address != was);
        } else if let Some(c) = l.iter_mut().find(|c| c.address == was) {
            c.address = now.to_string();
        }
        write(l);
    });
}

/// Opened just now (with this port, when one was given).
pub fn used(address: &str, port: Option<u16>) {
    update(address, |c| {
        c.last_used = crate::sys::now_secs();
        if port.is_some() {
            c.port = port.filter(|&p| p != 22);
        }
    });
}

/// The terminal's way to choose: a numbered list of the computers, or any address typed in.
pub fn pick() -> Option<String> {
    use std::io::{BufRead, Write};
    let list = all();
    let mut out = std::io::stderr();
    let _ = writeln!(out, "sshremote: choose a computer");
    for (i, c) in list.iter().enumerate() {
        let how = if c.favorite { "favorite" } else if c.in_ssh_config && c.last_used == 0 { "~/.ssh/config" } else { "recent" };
        let _ = writeln!(out, "  {:>2}  {:<32} {how}", i + 1, c.label());
    }
    let _ = write!(out, "number, or user@host / sshremote://ID: ");
    let _ = out.flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let line = line.trim();
    match line.parse::<usize>() {
        Ok(n) if (1..=list.len()).contains(&n) => Some(list[n - 1].address.clone()),
        _ if !line.is_empty() => Some(line.to_string()),
        _ => None,
    }
}

/// Takes a computer off the list (an alias of ~/.ssh/config stays there, and so in the list).
pub fn forget(address: &str) {
    with(|l| {
        l.retain(|c| c.address != address);
        write(l);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sshremote-computers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TEST_FILE.with(|f| *f.borrow_mut() = Some(dir.join("computers.json")));
        dir
    }

    /// The older lists become the one list once: recent hosts with their ports, favorites, and
    /// when each was opened.
    #[test]
    fn the_older_lists_are_read_in() {
        let dir = scratch("older");
        std::fs::write(dir.join("recent.txt"), "dev@build 2222\nweb1 \nsshremote://123456789@relay.example \n").unwrap();
        std::fs::write(dir.join("favorites.txt"), "web1\nnas\n").unwrap();
        std::fs::write(dir.join("seen.txt"), "web1 300\ndev@build 500\n").unwrap();
        let list = with(|l| l.clone());
        let by = |a: &str| list.iter().find(|c| c.address == a).cloned().unwrap();
        assert_eq!((by("dev@build").port, by("dev@build").last_used), (Some(2222), 500));
        assert!(by("web1").favorite && by("nas").favorite && !by("dev@build").favorite);
        assert_eq!(by("sshremote://123456789@relay.example").label(), "ID 123 456 789");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opening, starring, naming and forgetting are kept, and the order is favorites first,
    /// then the last opened.
    #[test]
    fn the_list_keeps_what_is_done_to_it() {
        let dir = scratch("kept");
        used("a@one", None);
        update("a@one", |c| c.last_used = 100);
        used("b@two", Some(2200));
        update("b@two", |c| c.last_used = 200);
        update("c@three", |c| {
            c.favorite = true;
            c.name = "Third".into();
        });
        let mut order: Vec<String> = with(|l| l.clone()).into_iter().map(|c| c.address).collect();
        order.sort();
        assert_eq!(order, ["a@one", "b@two", "c@three"]);
        let mut list = with(|l| l.clone());
        list.sort_by_key(|c| (!c.favorite, std::cmp::Reverse(c.last_used)));
        assert_eq!(list.iter().map(|c| c.label()).collect::<Vec<_>>(), ["Third", "b@two", "a@one"]);
        assert_eq!(list[1].port, Some(2200));
        forget("a@one");
        assert!(with(|l| l.iter().all(|c| c.address != "a@one")));
        let text = std::fs::read_to_string(dir.join("computers.json")).unwrap();
        assert!(text.contains("Third") && !text.contains("a@one"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A computer whose address changes keeps its name and star; a backup's computers replace
    /// those at the same address and leave the others.
    #[test]
    fn renaming_and_merging_keep_what_belongs_to_each_computer() {
        let dir = scratch("moved");
        update("old@host", |c| {
            c.name = "Box".into();
            c.favorite = true;
        });
        update("other@host", |c| c.name = "Other".into());
        rename("old@host", "new@host");
        let list = with(|l| l.clone());
        let by = |a: &str| list.iter().find(|c| c.address == a).cloned();
        assert!(by("old@host").is_none());
        assert_eq!(by("new@host").map(|c| (c.name, c.favorite)), Some(("Box".into(), true)));
        merge(parse(r#"[{"address":"other@host","name":"From the backup"},{"address":"nas","favorite":true}]"#));
        let list = with(|l| l.clone());
        let names: Vec<(String, String)> = list.iter().map(|c| (c.address.clone(), c.name.clone())).collect();
        assert!(names.contains(&("other@host".into(), "From the backup".into())));
        assert!(names.contains(&("new@host".into(), "Box".into())));
        assert!(list.iter().any(|c| c.address == "nas" && c.favorite));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
