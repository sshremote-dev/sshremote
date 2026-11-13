//! Hosts the user has reached before or named in ~/.ssh/config, offered by both
//! viewers; ssh itself applies each alias's user, port and key.


/// Concrete `Host` names in ~/.ssh/config and the files it includes (patterns with wildcards or
/// negation are skipped).
pub fn ssh_config_hosts() -> Vec<String> {
    let dir = crate::sys::ssh_dir();
    parse_ssh_hosts(&with_includes(&dir.join("config"), &dir, 0))
}

/// A config file's text with the files its `Include` lines name put in their place, as ssh
/// reads them: `~` for the home folder, other relative paths under ~/.ssh, `*` and `?` in file
/// names; at most eight deep, so files that include each other end.
fn with_includes(file: &std::path::Path, ssh: &std::path::Path, depth: usize) -> String {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    if depth >= 8 {
        return text;
    }
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim();
        let Some((key, rest)) = t.split_once(|c: char| c.is_whitespace() || c == '=') else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        if !key.eq_ignore_ascii_case("include") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
        for pattern in rest.split_whitespace().map(|p| p.trim_matches('"')) {
            let path = match pattern.strip_prefix("~/") {
                Some(r) => ssh.parent().map_or_else(|| ssh.join(r), |home| home.join(r)),
                None if std::path::Path::new(pattern).is_absolute() => std::path::PathBuf::from(pattern),
                None => ssh.join(pattern),
            };
            let (folder, name) = (path.parent().map(|p| p.to_path_buf()).unwrap_or_default(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            let mut files: Vec<std::path::PathBuf> = if name.contains(['*', '?']) {
                std::fs::read_dir(&folder)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| wildcard(&name, &e.file_name().to_string_lossy()))
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect()
            } else {
                vec![path]
            };
            files.sort();
            for f in files {
                out.push_str(&with_includes(&f, ssh, depth + 1));
                out.push('\n');
            }
        }
    }
    out
}

/// Whether `name` matches `pattern` with `*` (any run) and `?` (any one character).
fn wildcard(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // The classic two-pointer match, going back to the last `*` on a mismatch.
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

/// Services that answer ssh for git only: an alias for one of them is there for git, never a
/// computer to open a terminal or a screen on.
const GIT_HOSTS: [&str; 8] = [
    "github.com",
    "ssh.github.com",
    "gitlab.com",
    "altssh.gitlab.com",
    "bitbucket.org",
    "altssh.bitbucket.org",
    "codeberg.org",
    "ssh.dev.azure.com",
];

/// The `Host` aliases of an ssh config without patterns, and without those for git services
/// (by their name or their `HostName`).
fn parse_ssh_hosts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut git: Vec<String> = Vec::new();
    // The aliases of the `Host` block being read.
    let mut block: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else { continue };
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
        if key.eq_ignore_ascii_case("host") {
            block = rest.split_whitespace().filter(|n| !n.contains(['*', '?', '!'])).map(String::from).collect();
            for name in &block {
                if !out.contains(name) {
                    out.push(name.clone());
                }
                if GIT_HOSTS.contains(&name.to_lowercase().as_str()) {
                    git.push(name.clone());
                }
            }
        } else if key.eq_ignore_ascii_case("match") {
            block.clear();
        } else if key.eq_ignore_ascii_case("hostname") && GIT_HOSTS.contains(&rest.trim().to_lowercase().as_str()) {
            git.extend(block.iter().cloned());
        }
    }
    out.retain(|n| !git.contains(n));
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

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(when: u64) -> String {
    let d = crate::sys::now_secs().saturating_sub(when);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86_400 => format!("{} h ago", d / 3600),
        86_400..172_800 => "yesterday".into(),
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

/// Forgets the preferences whose line starts with `prefix` ("key=" for one key).
pub fn remove_settings(prefix: &str) {
    with_settings(|lines| {
        let before = lines.len();
        lines.retain(|l| !l.starts_with(prefix));
        if lines.len() != before {
            write_lines("settings.txt", lines);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Hosts in files the config includes are found: by a relative path, `~`, and a pattern;
    /// files that include each other end.
    #[test]
    fn included_files_bring_their_hosts() {
        let home = std::env::temp_dir().join(format!("sshremote-include-{}", std::process::id()));
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(ssh.join("config.d")).unwrap();
        std::fs::write(ssh.join("config"), "Include config.d/*.conf\ninclude ~/.ssh/extra\nHost main\n  HostName 10.0.0.1\n").unwrap();
        std::fs::write(ssh.join("config.d").join("a.conf"), "Host alpha\n  HostName 10.0.0.2\n").unwrap();
        std::fs::write(ssh.join("config.d").join("b.conf"), "Host beta\n").unwrap();
        std::fs::write(ssh.join("config.d").join("skip.txt"), "Host nope\n").unwrap();
        std::fs::write(ssh.join("extra"), "Host gamma\nInclude extra\n").unwrap();
        let hosts = super::parse_ssh_hosts(&super::with_includes(&ssh.join("config"), &ssh, 0));
        assert_eq!(hosts, ["alpha", "beta", "gamma", "main"]);
        assert!(super::wildcard("*.conf", "a.conf") && !super::wildcard("*.conf", "a.txt") && super::wildcard("h?st", "host"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ssh_config_hosts_skip_patterns() {
        let cfg = "Host ubuntu\n  HostName 192.168.40.10\nHost *.corp !bad web1 web2\nhost=db\nMatch all\n";
        assert_eq!(parse_ssh_hosts(cfg), ["ubuntu", "web1", "web2", "db"]);
    }

    /// Aliases for git services are for git, not computers to open: by name or by HostName.
    #[test]
    fn ssh_config_hosts_leave_out_git_services() {
        let cfg = "Host github.com
  User git
Host github-443
  HostName ssh.github.com
  Port 443
Host work
  HostName = 10.0.0.5
Host gl
  hostname GitLab.com
";
        assert_eq!(parse_ssh_hosts(cfg), ["work"]);
    }
}
