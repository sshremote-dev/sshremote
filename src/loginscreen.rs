//! Linux: remote access at the login screen and on a locked screen. A system service runs a copy
//! of this program as root, kept where only root can change it, with the settings of the person
//! who set it up (the same ID and password). It answers while that person's desktop is not open
//! on the screen, showing what the display hardware shows; their own service answers otherwise.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const UNIT: &str = "sshremote-login.service";
const UNIT_FILE: &str = "/etc/systemd/system/sshremote-login.service";
/// Root's copy of the program.
pub const EXE: &str = "/usr/local/lib/sshremote/sshremote";
/// The person whose desktop this serves around, and whose settings it uses.
pub const OWNER_VAR: &str = "SSHREMOTE_CONSOLE_OWNER";
/// Their own program, whose version root's copy follows.
const OWNER_EXE_VAR: &str = "SSHREMOTE_OWNER_EXE";

pub fn installed() -> bool {
    Path::new(UNIT_FILE).exists()
}

fn is_root() -> bool {
    // SAFETY: geteuid cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// The unit for `uid`, whose settings are under `config` (their XDG_CONFIG_HOME).
fn unit(uid: u32, config: &Path, owner_exe: &Path) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!(
        "[Unit]\nDescription=SSH Remote: remote access at the login screen and on a locked screen\nAfter=network-online.target systemd-logind.service\nWants=network-online.target\n\n[Service]\nExecStart={EXE} remote-access run --service --login-screen\nEnvironment={}\nEnvironment={}\nEnvironment={OWNER_VAR}={uid}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=graphical.target\n",
        quote(&format!("XDG_CONFIG_HOME={}", config.display())),
        quote(&format!("{OWNER_EXE_VAR}={}", owner_exe.display())),
    )
}

/// Puts the service in place (`on`) or takes it away, as root: directly when this runs as root,
/// else through the system's question for an administrator's password (pkexec, sudo in a
/// terminal).
pub fn set(on: bool) -> Result<(), String> {
    if !is_root() {
        return ask_root(on);
    }
    if !on {
        let _ = Command::new("systemctl").args(["disable", "--now", UNIT]).status();
        let _ = std::fs::remove_file(UNIT_FILE);
        let _ = std::fs::remove_file(EXE);
        let _ = Command::new("systemctl").arg("daemon-reload").status();
        return Ok(());
    }
    let uid: u32 = std::env::var(OWNER_VAR).ok().and_then(|u| u.parse().ok()).ok_or("whose settings to use was not said")?;
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).ok_or("where the settings are was not said")?;
    if uid == 0 {
        return Err("set it up as the person whose desktop it serves, not as root".into());
    }
    // A copy only root can change: what root runs must not be swappable by anyone else.
    let here = std::env::current_exe().map_err(|e| e.to_string())?;
    if !Path::new(EXE).exists() || std::fs::canonicalize(&here).ok() != std::fs::canonicalize(EXE).ok() {
        install_copy(&here)?;
    }
    let owner_exe = std::env::var_os(OWNER_EXE_VAR).map(PathBuf::from).unwrap_or_else(|| here.clone());
    std::fs::write(UNIT_FILE, unit(uid, &config, &owner_exe)).map_err(|e| format!("{UNIT_FILE}: {e}"))?;
    let ok = |args: &[&str]| {
        Command::new("systemctl").args(args).status().map_err(|e| e.to_string()).and_then(|s| if s.success() { Ok(()) } else { Err(format!("systemctl {}", args.join(" "))) })
    };
    ok(&["daemon-reload"])?;
    ok(&["enable", UNIT])?;
    ok(&["restart", UNIT])
}

/// Copies `from` to root's place for it, through a temporary name so a running copy keeps its file.
fn install_copy(from: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = Path::new(EXE).parent().ok_or("bad path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".sshremote-{}", std::process::id()));
    std::fs::copy(from, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, EXE).map_err(|e| format!("{EXE}: {e}"))
}

/// Runs this same command as root, telling it whose settings to take.
fn ask_root(on: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or("no home folder")?;
    let args = [
        "env".to_string(),
        format!("{OWNER_VAR}={uid}"),
        format!("XDG_CONFIG_HOME={}", config.display()),
        format!("{OWNER_EXE_VAR}={}", exe.display()),
        exe.display().to_string(),
        "remote-access".into(),
        "login-window".into(),
        if on { "on" } else { "off" }.into(),
    ];
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin());
    // pkexec asks in the desktop's own dialog; in a terminal sudo asks there.
    let tries: Vec<(&str, Vec<String>)> = if terminal {
        vec![("sudo", args.to_vec()), ("pkexec", args.to_vec())]
    } else {
        vec![("pkexec", args.to_vec())]
    };
    let mut why = String::from("neither pkexec nor sudo is installed");
    for (tool, a) in tries {
        match Command::new(tool).args(&a).status() {
            Ok(s) if s.success() => return Ok(()),
            // 126/127: the administrator's password was not given.
            Ok(s) => {
                why = match s.code() {
                    Some(126) | Some(127) if tool == "pkexec" => crate::lang::fill(n!("{} was not allowed: no administrator's password was given"), &[&tool]),
                    _ => crate::lang::fill(n!("{} did not finish ({})"), &[&tool, &s]),
                };
                break;
            }
            Err(_) => continue,
        }
    }
    Err(why)
}

/// This program's version, findable in its file by `version_in` without running it.
#[used]
static VERSION_MARK: &str = concat!("\u{1}sshremote-version ", env!("CARGO_PKG_VERSION"), "\u{1}");

/// The version of the program in `file`, read from its bytes.
fn version_in(file: &Path) -> Option<String> {
    let data = std::fs::read(file).ok()?;
    // Made here, so this needle is not itself a copy of the mark.
    let needle: Vec<u8> = [&[1u8][..], b"sshremote-version "].concat();
    let at = data.windows(needle.len()).position(|w| w == needle.as_slice())? + needle.len();
    let end = data[at..].iter().take(32).position(|&b| b == 1)?;
    let v = std::str::from_utf8(&data[at..at + end]).ok()?;
    v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-').then(|| v.to_string())
}

/// Root's copy follows the person's program to a newer release: the release is downloaded and
/// checked against sshremote.dev's signature, as any update; their file itself is never run here.
/// Returns whether this copy was replaced (and should start again).
pub fn follow() -> bool {
    let _ = &VERSION_MARK;
    let Some(owner_exe) = std::env::var_os(OWNER_EXE_VAR).map(PathBuf::from) else { return false };
    let Some(theirs) = version_in(&owner_exe) else { return false };
    let ours = env!("CARGO_PKG_VERSION");
    if !crate::update::newer(&theirs, ours) {
        return false;
    }
    match crate::update::check() {
        Ok(Some(r)) if !crate::update::newer(&r.version, &theirs) => match crate::update::install(&r) {
            Ok(_) => {
                eprintln!("remote access at the login screen: updated to {}", r.version);
                true
            }
            Err(e) => {
                eprintln!("remote access at the login screen: the update to {} failed: {e}", r.version);
                false
            }
        },
        _ => false,
    }
}
