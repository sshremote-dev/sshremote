//! The programs installed on this computer, to open one in a pane: by name from the list, by a
//! command line, or a file (or an address) with the program that opens it. Each starts as the
//! person signed in at the screen, on their desktop; the agent then shows that program alone.

/// One installed program: how to start it (`id`), its name, and a line about it.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub detail: String,
}

/// What to start, as a viewer asks for it (`ToAgent::Launch`).
pub const APP: u8 = 0;
pub const COMMAND: u8 = 1;
pub const OPEN: u8 = 2;

/// The installed programs, by name.
pub fn list() -> Vec<App> {
    let mut all = imp::list();
    all.sort_by_key(|a| a.name.to_lowercase());
    all.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name) && a.detail == b.detail);
    all
}

/// Starts `text` as `kind` says; the name to look for its window by (empty when unknown).
pub fn launch(kind: u8, text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to start".into());
    }
    match kind {
        APP => imp::start_app(text),
        COMMAND => imp::run(text).map(|_| program_of(text)),
        OPEN => imp::open(text).map(|_| String::new()),
        _ => Err("unknown way to start a program".into()),
    }
}

/// The program a command line starts, by its file name without the folder or extension.
pub fn program_of(command: &str) -> String {
    let first = command.trim().strip_prefix('"').map_or_else(|| command.split_whitespace().next().unwrap_or(""), |r| r.split('"').next().unwrap_or(""));
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
    base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base).to_string()
}

#[cfg(target_os = "linux")]
mod imp {
    use super::App;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    /// Where desktop entries are: the user's own first (they hide or replace the system's).
    fn dirs() -> Vec<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
        let mut out = vec![data_home.join("applications"), home.join(".local/share/flatpak/exports/share/applications")];
        let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        out.extend(dirs.split(':').map(|d| Path::new(d).join("applications")));
        out.push("/var/lib/flatpak/exports/share/applications".into());
        out.push("/var/lib/snapd/desktop/applications".into());
        out
    }

    /// The `[Desktop Entry]` group's keys, the plain ones only (no translations).
    fn entry(text: &str) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        let mut inside = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                inside = line == "[Desktop Entry]";
            } else if inside && let Some((k, v)) = line.split_once('=') {
                map.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
            }
        }
        map
    }

    /// Every desktop entry file, by its id (the path under `applications`, '/' made '-'), the
    /// first found winning.
    fn files() -> Vec<(String, PathBuf)> {
        files_in(&dirs())
    }

    fn files_in(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for dir in dirs {
            let mut stack = vec![dir.clone()];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x == "desktop") {
                        let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().replace('/', "-");
                        if seen.insert(rel.clone()) {
                            out.push((rel, p));
                        }
                    }
                }
            }
        }
        out
    }

    fn shown(e: &std::collections::HashMap<String, String>) -> bool {
        // Entries for some desktops only (their settings panels), as the desktop's menu shows them.
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
        let ours = |list: &String| list.to_lowercase().split(';').any(|d| !d.is_empty() && desktop.split(':').any(|c| c == d));
        e.get("Type").is_some_and(|t| t == "Application")
            && !e.get("NoDisplay").is_some_and(|v| v == "true")
            && !e.get("Hidden").is_some_and(|v| v == "true")
            && e.get("Exec").is_some_and(|x| !x.is_empty())
            && e.get("OnlyShowIn").is_none_or(ours)
            && !e.get("NotShowIn").is_some_and(ours)
    }

    pub fn list() -> Vec<App> {
        list_in(&dirs())
    }

    pub(super) fn list_in(dirs: &[PathBuf]) -> Vec<App> {
        files_in(dirs)
            .into_iter()
            .filter_map(|(id, path)| {
                let e = entry(&std::fs::read_to_string(&path).ok()?);
                if !shown(&e) {
                    return None;
                }
                let name = e.get("Name")?.clone();
                let detail = e.get("Comment").or_else(|| e.get("GenericName")).cloned().unwrap_or_default();
                Some(App { id, name, detail })
            })
            .collect()
    }

    /// Its own process group, no terminal, nothing inherited: it outlives the session.
    fn detached(mut cmd: Command) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        // Reaped on a thread of its own, so it leaves no zombie behind.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    fn on_path(program: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
    }

    /// The Exec line without its field codes (%f, %U...), as a shell line.
    pub(super) fn exec_line(exec: &str) -> String {
        let mut out = String::new();
        let mut it = exec.chars();
        while let Some(c) = it.next() {
            if c == '%' {
                if it.next() == Some('%') {
                    out.push('%');
                }
            } else {
                out.push(c);
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    pub fn start_app(id: &str) -> Result<String, String> {
        let (_, path) = files().into_iter().find(|(i, _)| i == id).ok_or_else(|| format!("{id} is not installed here"))?;
        let e = entry(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?);
        let name = e.get("Name").cloned().unwrap_or_default();
        // The desktop's own launcher where there is one (it knows D-Bus activation and the rest).
        if on_path("gio") {
            let mut cmd = Command::new("gio");
            cmd.arg("launch").arg(&path);
            detached(cmd)?;
        } else {
            let line = exec_line(e.get("Exec").ok_or("it has no command")?);
            let mut cmd = Command::new("sh");
            cmd.args(["-c", &line]);
            detached(cmd)?;
        }
        Ok(name)
    }

    pub fn run(command: &str) -> Result<(), String> {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        detached(cmd)
    }

    pub fn open(what: &str) -> Result<(), String> {
        let what = match what.strip_prefix("~/") {
            Some(rest) => std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or_else(|_| what.to_string()),
            None => what.to_string(),
        };
        let looks_like_address = what.contains("://");
        if !looks_like_address && !Path::new(&what).exists() {
            return Err(format!("{what} is not there"));
        }
        let mut cmd = if on_path("gio") {
            let mut c = Command::new("gio");
            c.arg("open");
            c
        } else {
            Command::new("xdg-open")
        };
        cmd.arg(&what);
        detached(cmd)
    }
}

#[cfg(windows)]
mod imp {
    use super::App;
    use std::path::{Path, PathBuf};

    /// The Start menu's programs: everyone's, then the person's at the screen (an agent run as
    /// SYSTEM looks in theirs, never in its own).
    fn roots() -> Vec<PathBuf> {
        let mut out = Vec::new();
        let program_data = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| r"C:\ProgramData".into());
        out.push(program_data.join(r"Microsoft\Windows\Start Menu\Programs"));
        let profile = match crate::console_user::acting() {
            crate::console_user::Acting::User(u) => Some(u.profile.clone()),
            crate::console_user::Acting::Myself => std::env::var_os("USERPROFILE").map(PathBuf::from),
            crate::console_user::Acting::Nobody => None,
        };
        if let Some(p) = profile {
            out.push(p.join(r"AppData\Roaming\Microsoft\Windows\Start Menu\Programs"));
        }
        out
    }

    fn shortcuts() -> Vec<(PathBuf, PathBuf)> {
        let mut out = Vec::new();
        for root in roots() {
            let mut stack = vec![root.clone()];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                        out.push((root.clone(), p));
                    }
                }
            }
        }
        out
    }

    /// Uninstallers and help files are not programs to open in a pane.
    fn wanted(name: &str) -> bool {
        let n = name.to_lowercase();
        !["uninstall", "remove ", "readme", "release notes", "help", "documentation", "website"].iter().any(|w| n.contains(w))
    }

    /// Windows' own list of what the Start menu shows (Store apps too: Calculator, Notepad,
    /// Settings), read by PowerShell as the person at the screen; the shortcuts where it fails.
    pub fn list() -> Vec<App> {
        let apps = start_apps();
        if !apps.is_empty() {
            return apps;
        }
        lnk_list()
    }

    fn start_apps() -> Vec<App> {
        const PS: &str = "powershell.exe -NoProfile -NonInteractive -Command \"[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-StartApps | ForEach-Object { $_.Name + [char]9 + $_.AppID }\"";
        let out = match crate::console_user::acting() {
            crate::console_user::Acting::User(u) => crate::console_user::spawn_piped(&u, PS).ok().and_then(|(_, stdin, mut stdout)| {
                drop(stdin);
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut stdout, &mut buf).ok().map(|_| buf)
            }),
            crate::console_user::Acting::Nobody => None,
            crate::console_user::Acting::Myself => {
                use std::os::windows::process::CommandExt;
                std::process::Command::new("powershell.exe")
                    .raw_arg(&PS["powershell.exe ".len()..])
                    .creation_flags(0x0800_0000)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .ok()
                    .map(|o| o.stdout)
            }
        };
        let text = String::from_utf8_lossy(&out.unwrap_or_default()).into_owned();
        text.lines()
            .filter_map(|l| {
                let (name, id) = l.trim_end_matches('\r').split_once('\t')?;
                (!name.is_empty() && !id.is_empty() && wanted(name)).then(|| App { id: format!("shell:AppsFolder\\{id}"), name: name.to_string(), detail: String::new() })
            })
            .collect()
    }

    fn lnk_list() -> Vec<App> {
        shortcuts()
            .into_iter()
            .filter_map(|(root, p)| {
                let name = p.file_stem()?.to_string_lossy().into_owned();
                if !wanted(&name) {
                    return None;
                }
                let folder = p.parent().and_then(|f| f.strip_prefix(&root).ok()).map(|f| f.display().to_string()).unwrap_or_default();
                Some(App { id: p.display().to_string(), name, detail: folder })
            })
            .collect()
    }

    /// `cmd /c start "" LINE`, hidden, as the person at the screen: the shell opens a shortcut,
    /// a document or an address with its program, and a command in a window of its own.
    fn start(line: &str) -> Result<(), String> {
        let command = format!("cmd.exe /d /c start \"\" {line}");
        match crate::console_user::acting() {
            crate::console_user::Acting::User(u) => crate::console_user::spawn_piped(&u, &command).map(drop),
            crate::console_user::Acting::Nobody => Err(crate::console_user::NOBODY.into()),
            crate::console_user::Acting::Myself => {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                std::process::Command::new("cmd.exe")
                    .raw_arg(format!("/d /c start \"\" {line}"))
                    .creation_flags(CREATE_NO_WINDOW)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map(drop)
                    .map_err(|e| e.to_string())
            }
        }
    }

    fn quoted(s: &str) -> String {
        format!("\"{}\"", s.replace('"', ""))
    }

    pub fn start_app(id: &str) -> Result<String, String> {
        // What the Start menu lists (Get-StartApps), started as Explorer starts it.
        if id.starts_with("shell:AppsFolder\\") {
            let name = start_apps().into_iter().find(|a| a.id == id).map(|a| a.name).ok_or_else(|| format!("{id} is not in the Start menu"))?;
            start(&quoted(id))?;
            return Ok(name);
        }
        // Only a shortcut the list gave: a viewer cannot name any file this way.
        let (_, p) = shortcuts().into_iter().find(|(_, p)| p.display().to_string() == id).ok_or_else(|| format!("{id} is not in the Start menu"))?;
        start(&quoted(&p.display().to_string()))?;
        Ok(p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
    }

    pub fn run(command: &str) -> Result<(), String> {
        start(command)
    }

    pub fn open(what: &str) -> Result<(), String> {
        if !what.contains("://") && !Path::new(what).exists() {
            return Err(format!("{what} is not there"));
        }
        start(&quoted(what))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::App;
    pub fn list() -> Vec<App> {
        Vec::new()
    }
    pub fn start_app(_: &str) -> Result<String, String> {
        Err("programs are not started here on this system".into())
    }
    pub fn run(_: &str) -> Result<(), String> {
        Err("programs are not started here on this system".into())
    }
    pub fn open(_: &str) -> Result<(), String> {
        Err("programs are not started here on this system".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The programs installed here (nothing is started): cargo test -- --ignored --nocapture programs_here
    #[test]
    #[ignore]
    fn programs_here_are_listed() {
        let all = list();
        println!("{} programs: {}", all.len(), all.iter().take(25).map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "));
        assert!(!all.is_empty());
    }

    #[test]
    fn a_command_names_its_program() {
        assert_eq!(program_of("gnome-calculator --mode=basic"), "gnome-calculator");
        assert_eq!(program_of("/usr/bin/gedit /tmp/a.txt"), "gedit");
        assert_eq!(program_of(r#""C:\Program Files\App\app.exe" -x"#), "app");
        assert_eq!(program_of("notepad.exe"), "notepad");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_entries_are_read_and_launched_by_their_id() {
        let dir = std::env::temp_dir().join(format!("sshremote-apps-{}", std::process::id()));
        let apps = dir.join("applications/sub");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join("calc.desktop"), "[Desktop Entry]\nType=Application\nName=Calc\nName[ko]=계산기\nComment=Sums\nExec=true %U\n[Desktop Action x]\nName=Other\n").unwrap();
        std::fs::write(apps.join("hidden.desktop"), "[Desktop Entry]\nType=Application\nName=Hidden\nNoDisplay=true\nExec=true\n").unwrap();
        let list = imp::list_in(&[dir.join("applications")]);
        let calc = list.iter().find(|a| a.name == "Calc").expect("listed");
        assert_eq!((calc.id.as_str(), calc.detail.as_str()), ("sub-calc.desktop", "Sums"));
        assert!(!list.iter().any(|a| a.name == "Hidden" || a.name == "Other"));
        assert_eq!(imp::exec_line("foo --x %U %% %f"), "foo --x %");
        assert!(launch(APP, "sub-nothing.desktop").is_err());
        assert!(launch(OPEN, "/no/such/file").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
