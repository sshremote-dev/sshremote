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

fn config_dir() -> Option<std::path::PathBuf> {
    let base: Option<std::path::PathBuf> = if cfg!(windows) {
        std::env::var_os("APPDATA").map(Into::into)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(Into::into)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
    };
    base.map(|b| b.join("desk"))
}

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
    let _ = writeln!(out, "desk: choose a host");
    for (i, h) in all.iter().enumerate() {
        let tag = if recent.contains(h) { "recent" } else { "~/.ssh/config" };
        let _ = writeln!(out, "  {:>2}  {h:<32} {tag}", i + 1);
    }
    let _ = write!(out, "number, or user@host / desk://host: ");
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

/// Puts `host` first in the recent list.
pub fn remember(host: &str) {
    let mut list = load_recent();
    list.retain(|(h, _)| h != host);
    list.insert(0, (host.to_string(), String::new()));
    list.truncate(8);
    save_recent(&list);
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
