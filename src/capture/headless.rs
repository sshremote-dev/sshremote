//! A desktop of its own for a Linux computer with none running (a server with no monitor): Xvfb
//! and the first X session installed there, started as this user when a viewer asks for the
//! screen and left running, as a desktop at a screen would be, until it is signed out of. Its X
//! server takes only clients that hold its cookie (this user's own file), not every local user.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Light desktops first: they run best without a graphics card.
const LIKED: [&str; 11] = ["xfce", "mate", "lxqt", "lxde", "cinnamon2d", "cinnamon", "plasmax11", "plasma", "gnome-xorg", "ubuntu-xorg", "openbox"];

/// Its size until the viewer's own asks for another.
const SIZE: &str = "1920x1080x24";

#[derive(Debug, PartialEq)]
pub(super) struct Session {
    /// The file's name without `.desktop` (`mate`).
    name: String,
    exec: String,
    /// What `XDG_CURRENT_DESKTOP` is for it (`MATE`).
    desktop: String,
}

fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// The X session a desktop file names, if it can run here.
pub(super) fn session_of(name: &str, text: &str) -> Option<Session> {
    let entry = text.lines().skip_while(|l| l.trim() != "[Desktop Entry]").skip(1).take_while(|l| !l.trim_start().starts_with('['));
    let mut get = std::collections::HashMap::new();
    for l in entry {
        if let Some((k, v)) = l.split_once('=') {
            get.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    let yes = |k: &str| get.get(k).is_some_and(|v| v == "true");
    let exec = get.get("Exec")?.clone();
    // The display manager's own (~/.xsession, "default"), and hidden ones.
    if yes("Hidden") || yes("NoDisplay") || exec == "default" || name.contains("xsession") || exec.contains('\n') {
        return None;
    }
    let program = exec.split_whitespace().find(|w| !w.contains('=') && *w != "env")?;
    if !on_path(program) || get.get("TryExec").is_some_and(|t| !on_path(t)) {
        return None;
    }
    let desktop = get.get("DesktopNames").map(|d| d.trim_end_matches(';').replace(';', ":")).filter(|d| !d.is_empty()).unwrap_or_else(|| get.get("Name").cloned().unwrap_or_default());
    Some(Session { name: name.to_string(), exec, desktop })
}

/// The X sessions installed, the lightest first.
fn sessions() -> Vec<Session> {
    let mut found = Vec::new();
    for dir in ["/usr/share/xsessions", "/usr/local/share/xsessions"] {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            let (Some(stem), Some("desktop")) = (p.file_stem().and_then(|s| s.to_str()), p.extension().and_then(|x| x.to_str())) else { continue };
            if let Some(s) = std::fs::read_to_string(&p).ok().and_then(|t| session_of(stem, &t)) {
                found.push(s);
            }
        }
    }
    found.sort_by_key(|s| LIKED.iter().position(|l| s.name.to_lowercase() == *l).unwrap_or(LIKED.len()));
    found
}

/// Whether an X server listens as display `n`: its socket in /tmp/.X11-unix, or its abstract
/// one (all it has where that folder is missing and only root may make it).
fn listening(n: u32) -> bool {
    let name = format!("/tmp/.X11-unix/X{n}");
    Path::new(&name).exists() || std::fs::read_to_string("/proc/net/unix").is_ok_and(|t| t.lines().any(|l| l.ends_with(&format!("@{name}"))))
}

/// A display number nobody uses.
fn free_display() -> Option<u32> {
    (20..100).find(|n| !listening(*n) && !Path::new(&format!("/tmp/.X{n}-lock")).exists())
}

/// An X authority file of one cookie for display `n` (any host), only this user's to read.
fn cookie_file(n: u32, dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(format!("sshremote-desktop-{n}.xauth"));
    let field = |b: &[u8]| [&(b.len() as u16).to_be_bytes()[..], b].concat();
    // Family "wild" (65535), no address, the display's number, the cookie.
    let mut data = 65535u16.to_be_bytes().to_vec();
    data.extend(field(b""));
    data.extend(field(n.to_string().as_bytes()));
    data.extend(field(b"MIT-MAGIC-COOKIE-1"));
    data.extend(field(&crate::sys::random(16)));
    crate::sys::write_private(&path, &data)?;
    Ok(path)
}

/// Whether logind has a graphical session here, anyone's (a desktop, or a login screen at a
/// monitor): then the computer is no headless one, whatever this process sees.
fn graphical_session() -> bool {
    let Ok(out) = Command::new("loginctl").args(["list-sessions", "--no-legend"]).stderr(Stdio::null()).output() else { return false };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_whitespace().next()).any(|id| {
        Command::new("loginctl")
            .args(["show-session", id, "-p", "Type", "--value"])
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|o| matches!(String::from_utf8_lossy(&o.stdout).trim(), "x11" | "wayland" | "mir"))
    })
}

/// Whether this computer starts one now: no display is here, nor any graphical session on the
/// computer (a signed-out desktop shows its login screen, which is not this), not as root (the
/// login screen is the display hardware's), Xvfb and a desktop installed, and remote access
/// allows it (it does unless turned off).
pub(super) fn allowed() -> bool {
    // SAFETY: getuid cannot fail.
    let root = unsafe { libc::getuid() } == 0;
    let off = crate::access::stored().ok().flatten().is_some_and(|c| !c.headless);
    !root
        && !off
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
        && on_path("Xvfb")
        && on_path("dbus-run-session")
        && !sessions().is_empty()
        && !graphical_session()
}

/// Starts the desktop, and points this process at it (`DISPLAY`, `XAUTHORITY` and the desktop's
/// names). The X server goes when the session ends (its person signs out).
pub(super) fn start() -> Result<(), String> {
    let s = sessions().into_iter().next().ok_or(n!("no desktop is installed here to start (Xfce, MATE, LXQt or another X session)"))?;
    let n = free_display().ok_or("no free display number")?;
    let run = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    let auth = cookie_file(n, &run)?;
    let auth_s = auth.display().to_string();
    let script = format!(
        "Xvfb :{n} -screen 0 {SIZE} -nolisten tcp -dpi 96 -auth \"$XAUTHORITY\" +extension RANDR >/dev/null 2>&1 & x=$!; \
         i=0; while ! grep -qs '/tmp/.X11-unix/X{n}$' /proc/net/unix && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done; \
         dbus-run-session -- {} ; kill $x; rm -f \"$XAUTHORITY\"",
        s.exec
    );
    let vars = [
        ("DISPLAY", format!(":{n}")),
        ("XAUTHORITY", auth_s.clone()),
        ("XDG_SESSION_TYPE", "x11".to_string()),
        ("XDG_CURRENT_DESKTOP", s.desktop.clone()),
        ("XDG_SESSION_DESKTOP", s.name.clone()),
        ("DESKTOP_SESSION", s.name.clone()),
    ];
    // In a unit of its own where there is a user manager, so a restart of remote access's own
    // leaves it running; else in a session of its own.
    let manager = on_path("systemd-run")
        && Command::new("systemctl")
            .args(["--user", "is-system-running"])
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|o| matches!(String::from_utf8_lossy(&o.stdout).trim(), "running" | "degraded" | "starting"));
    let by_manager = manager && {
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--collect", "--quiet", &format!("--unit=sshremote-desktop-{n}"), "--description=SSH Remote: a desktop of its own"]);
        if let Some(path) = std::env::var_os("PATH") {
            c.arg(format!("--setenv=PATH={}", path.to_string_lossy()));
        }
        for (k, v) in &vars {
            c.arg(format!("--setenv={k}={v}"));
        }
        c.args(["--", "sh", "-c", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        c.status().is_ok_and(|st| st.success())
    };
    if !by_manager {
        let mut c = Command::new("sh");
        c.args(["-c", &script]).envs(vars.iter().map(|(k, v)| (*k, v.as_str()))).env_remove("WAYLAND_DISPLAY");
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid only, between fork and exec.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            })
        };
        let mut child = c.spawn().map_err(|e| format!("Xvfb: {e}"))?;
        // The shell lives on by itself; its end is reaped here while this process lasts.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    for _ in 0..100 {
        if listening(n) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !listening(n) {
        return Err(n!("the desktop of its own did not start (Xvfb)").into());
    }
    // SAFETY: the agent sets its display variables before its threads use them, as gnome::ensure_env does.
    unsafe {
        for (k, v) in &vars {
            std::env::set_var(k, v);
        }
    }
    eprintln!("sshremote: started {} on :{n} for this session (a desktop of its own)", s.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop file's session as it is started, the display manager's own and hidden ones
    /// left out.
    #[test]
    fn desktop_files_name_their_sessions() {
        let mate = "[Desktop Entry]\nName=MATE\nExec=sh\nTryExec=sh\nDesktopNames=MATE;\n[Desktop Action x]\nExec=other\n";
        assert_eq!(session_of("mate", mate), Some(Session { name: "mate".into(), exec: "sh".into(), desktop: "MATE".into() }));
        let env = "[Desktop Entry]\nName=Cinnamon\nExec=env GDK_BACKEND=x11 sh -c x\n";
        assert_eq!(session_of("cinnamon", env).map(|s| s.desktop), Some("Cinnamon".into()));
        for (name, text) in [
            ("lightdm-xsession", "[Desktop Entry]\nName=Default\nExec=default\n"),
            ("hidden", "[Desktop Entry]\nName=H\nExec=sh\nHidden=true\n"),
            ("missing", "[Desktop Entry]\nName=M\nExec=no-such-program-here\n"),
            ("tryexec", "[Desktop Entry]\nName=T\nExec=sh\nTryExec=no-such-program-here\n"),
        ] {
            assert_eq!(session_of(name, text), None, "{name}");
        }
    }

    /// The cookie file is as Xlib reads it: family, address, display, name and data, each with
    /// its length first.
    #[test]
    fn the_cookie_file_holds_one_cookie_for_its_display() {
        let dir = std::env::temp_dir().join(format!("sshr-xauth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = cookie_file(42, &dir).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[..4], &[0xff, 0xff, 0, 0]);
        assert_eq!(&b[4..8], &[0, 2, b'4', b'2']);
        assert_eq!(&b[8..28], b"\x00\x12MIT-MAGIC-COOKIE-1");
        assert_eq!((&b[28..30], b.len()), (&[0u8, 16][..], 46));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
