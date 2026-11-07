//! Hosts the user has reached before or named in ~/.ssh/config, offered by both
//! viewers; ssh itself applies each alias's user, port and key.

use std::io::{BufRead, Write};

/// Concrete `Host` names in ~/.ssh/config (patterns with wildcards or negation are skipped).
pub fn ssh_config_hosts() -> Vec<String> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    let Some(path) = home.map(|h| std::path::Path::new(&h).join(".ssh").join("config")) else { return Vec::new() };
    parse_ssh_hosts(&std::fs::read_to_string(path).unwrap_or_default())
}

fn parse_ssh_hosts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else { continue };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for name in rest.split_whitespace() {
            if !name.contains(['*', '?', '!']) && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

pub(crate) fn config_dir() -> Option<std::path::PathBuf> {
    // Found once: it does not move while the program runs (tests that change HOME run alone).
    #[cfg(not(test))]
    {
        static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        DIR.get_or_init(find_config_dir).clone()
    }
    #[cfg(test)]
    find_config_dir()
}

fn find_config_dir() -> Option<std::path::PathBuf> {
    let base: Option<std::path::PathBuf> = if cfg!(windows) {
        std::env::var_os("APPDATA").map(Into::into)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(Into::into)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
    };
    let base = base?;
    let dir = base.join("sshremote");
    // Hosts, favorites and settings kept under the program's former name move over once.
    let old = base.join("desk");
    if !dir.exists() && old.join("recent.txt").exists() {
        let _ = std::fs::rename(&old, &dir);
    }
    Some(dir)
}

/// How many recent hosts are remembered.
pub const RECENT_MAX: usize = 12;

/// Recent connections as `host port` lines; passwords are never stored.
pub fn load_recent() -> Vec<(String, String)> {
    let Some(p) = config_dir().map(|d| d.join("recent.txt")) else { return Vec::new() };
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
        })
        .collect()
}

pub fn save_recent(list: &[(String, String)]) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let text: String = list.iter().map(|(h, p)| format!("{h} {p}\n")).collect();
        let _ = std::fs::write(d.join("recent.txt"), text);
    }
}

/// Terminal counterpart of the connect screen: a numbered list, or any host typed in.
pub fn pick() -> Option<String> {
    let recent: Vec<String> = load_recent().into_iter().map(|(h, _)| h).collect();
    let mut all = recent.clone();
    for h in ssh_config_hosts() {
        if !all.contains(&h) {
            all.push(h);
        }
    }
    let mut out = std::io::stderr();
    let _ = writeln!(out, "sshremote: choose a host");
    for (i, h) in all.iter().enumerate() {
        let tag = if recent.contains(h) { "recent" } else { "~/.ssh/config" };
        let _ = writeln!(out, "  {:>2}  {h:<32} {tag}", i + 1);
    }
    let _ = write!(out, "number, or user@host / sshremote://host: ");
    let _ = out.flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let line = line.trim();
    match line.parse::<usize>() {
        Ok(n) if (1..=all.len()).contains(&n) => Some(all[n - 1].clone()),
        _ if !line.is_empty() => Some(line.to_string()),
        _ => None,
    }
}

fn read_lines(name: &str) -> Vec<String> {
    let Some(p) = config_dir().map(|d| d.join(name)) else { return Vec::new() };
    std::fs::read_to_string(p).unwrap_or_default().lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
}

fn write_lines(name: &str, lines: &[String]) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(name), lines.iter().map(|l| format!("{l}\n")).collect::<String>());
    }
}

/// Hosts marked with a star on the connect screen.
pub fn favorites() -> Vec<String> {
    read_lines("favorites.txt")
}

pub fn set_favorite(host: &str, on: bool) {
    let mut list = favorites();
    list.retain(|h| h != host);
    if on {
        list.insert(0, host.to_string());
    }
    write_lines("favorites.txt", &list);
}

/// When each host was last connected, in seconds since 1970.
pub fn last_seen() -> std::collections::HashMap<String, u64> {
    read_lines("seen.txt")
        .iter()
        .filter_map(|l| {
            let (h, t) = l.rsplit_once(' ')?;
            Some((h.to_string(), t.parse().ok()?))
        })
        .collect()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn mark_seen(host: &str) {
    let mut seen = last_seen();
    seen.insert(host.to_string(), now());
    let mut lines: Vec<(String, u64)> = seen.into_iter().collect();
    lines.sort_by_key(|l| std::cmp::Reverse(l.1));
    lines.truncate(64);
    write_lines("seen.txt", &lines.iter().map(|(h, t)| format!("{h} {t}")).collect::<Vec<_>>());
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(when: u64) -> String {
    let d = now().saturating_sub(when);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86_400 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

/// A small persisted preference such as the theme.
pub fn setting(key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    with_settings(|lines| lines.iter().find_map(|l| l.strip_prefix(&prefix).map(str::to_string)))
}

/// Sets a preference; the file is written at once, so a crash loses nothing.
pub fn set_setting(key: &str, value: &str) {
    let prefix = format!("{key}=");
    with_settings(|lines| {
        lines.retain(|l| !l.starts_with(&prefix));
        lines.push(format!("{prefix}{value}"));
        write_lines("settings.txt", lines);
    });
}

/// The settings, read from disk once and kept: windows ask for them every frame.
static SETTINGS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

fn with_settings<R>(f: impl FnOnce(&mut Vec<String>) -> R) -> R {
    let mut cache = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    f(cache.get_or_insert_with(|| read_lines("settings.txt")))
}

/// Reads the settings from disk again (after a restore wrote them).
pub fn reload_settings() {
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Puts `host` first in the recent list.
pub fn remember(host: &str) {
    let mut list = load_recent();
    list.retain(|(h, _)| h != host);
    list.insert(0, (host.to_string(), String::new()));
    list.truncate(RECENT_MAX);
    save_recent(&list);
    mark_seen(host);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_hosts_skip_patterns() {
        let cfg = "Host ubuntu\n  HostName 192.168.40.10\nHost *.corp !bad web1 web2\nhost=db\nMatch all\n";
        assert_eq!(parse_ssh_hosts(cfg), ["ubuntu", "web1", "web2", "db"]);
    }
}
