//! Port forwards through an SSH computer: what one is, and the ssh that keeps it open.

use std::sync::Mutex;

use serde_json::{Value, json};

/// Which way a tunnel goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A port here leads to `dest` as the computer sees it (`ssh -L`).
    Local,
    /// A port on the computer leads to `dest` as this one sees it (`ssh -R`).
    Remote,
    /// A SOCKS proxy here that goes out from the computer (`ssh -D`).
    Socks,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Local, Kind::Remote, Kind::Socks];

    pub fn key(self) -> &'static str {
        match self {
            Kind::Local => "local",
            Kind::Remote => "remote",
            Kind::Socks => "socks",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Local => "Here to there",
            Kind::Remote => "There to here",
            Kind::Socks => "SOCKS proxy",
        }
    }

    fn from_key(k: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|x| x.key() == k)
    }
}

/// One forward, kept with its computer.
#[derive(Clone, Debug, PartialEq)]
pub struct Tunnel {
    pub kind: Kind,
    pub port: u16,
    /// HOST:PORT it leads to; empty for SOCKS.
    pub dest: String,
    /// Started when the app starts.
    pub start_with_app: bool,
}

impl Tunnel {
    /// What it does, in a few words: "8080 → localhost:80".
    pub fn label(&self) -> String {
        match self.kind {
            Kind::Local => format!("here {} → {}", self.port, self.dest),
            Kind::Remote => format!("there {} → {}", self.port, self.dest),
            Kind::Socks => format!("SOCKS on {}", self.port),
        }
    }

    /// The ssh option that makes it.
    pub fn args(&self) -> Vec<String> {
        match self.kind {
            Kind::Local => vec!["-L".into(), format!("{}:{}", self.port, self.dest)],
            Kind::Remote => vec!["-R".into(), format!("{}:{}", self.port, self.dest)],
            Kind::Socks => vec!["-D".into(), self.port.to_string()],
        }
    }

    /// Why it cannot be made as it is, if it cannot: nothing ssh would read as an option, a port
    /// that exists, and somewhere to lead to.
    pub fn problem(&self) -> Option<&'static str> {
        if self.port == 0 {
            return Some("The port is a number from 1 to 65535.");
        }
        if self.kind == Kind::Socks {
            return None;
        }
        let ok = |s: &str| !s.is_empty() && !s.starts_with('-') && !s.chars().any(char::is_whitespace);
        match self.dest.rsplit_once(':') {
            Some((host, port)) if ok(host) && port.parse::<u16>().is_ok_and(|p| p > 0) => None,
            _ => Some("Where it leads is HOST:PORT, such as localhost:5432."),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({ "kind": self.kind.key(), "port": self.port, "dest": self.dest, "auto": self.start_with_app })
    }

    pub fn from_json(v: &Value) -> Option<Tunnel> {
        let t = Tunnel {
            kind: Kind::from_key(v.get("kind")?.as_str()?)?,
            port: v.get("port")?.as_u64()?.try_into().ok()?,
            dest: v.get("dest").and_then(Value::as_str).unwrap_or("").to_string(),
            start_with_app: v.get("auto").and_then(Value::as_bool).unwrap_or(false),
        };
        t.problem().is_none().then_some(t)
    }
}

/// What ssh said, in the words of someone who only wanted a port forwarded.
pub fn plain(why: &str) -> String {
    let l = why.to_lowercase();
    if l.contains("permission denied") {
        "the computer wants a password, which a tunnel cannot type: install your key on it (Settings > SSH keys)".into()
    } else if l.contains("address already in use") || l.contains("cannot listen") || l.contains("forwarding failed") {
        "that port is already taken; pick another".into()
    } else if l.contains("could not resolve") || l.contains("no such host") {
        "the computer's name is not known".into()
    } else if l.contains("timed out") || l.contains("connection refused") || l.contains("unreachable") {
        "the computer could not be reached".into()
    } else {
        why.trim().to_string()
    }
}

/// A tunnel open now: whose it is, and the ssh keeping it open with what it said.
struct Run {
    address: String,
    tunnel: Tunnel,
    child: Option<std::process::Child>,
    said: std::sync::Arc<Mutex<String>>,
    since: std::time::Instant,
    /// How ssh ended, once it has.
    ended: Option<String>,
}

static RUNS: Mutex<Vec<Run>> = Mutex::new(Vec::new());

/// The same forward, whatever its start setting.
fn same(a: &Tunnel, b: &Tunnel) -> bool {
    (a.kind, a.port, &a.dest) == (b.kind, b.port, &b.dest)
}

/// Where a tunnel is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Off,
    /// ssh runs, and has not yet had the time to refuse the forward.
    Starting,
    On,
    Failed(String),
}

/// Opens `t` through the computer at `address` (an alias or `user@host`, with `port`), unless it
/// is open already. Signs in with keys only: nobody is there to type a password.
pub fn start(address: &str, port: Option<u16>, t: &Tunnel) -> Result<(), String> {
    if let Some(p) = t.problem() {
        return Err(p.into());
    }
    let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    runs.retain(|r| !(r.address == address && same(&r.tunnel, t) && r.child.is_none()));
    if runs.iter().any(|r| r.address == address && same(&r.tunnel, t)) {
        return Ok(());
    }
    let mut cmd = std::process::Command::new("ssh");
    cmd.args(["-N", "-o", "ExitOnForwardFailure=yes", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=30"]);
    // Its own connection: as a shared one (ControlMaster in ~/.ssh/config) it would go on in the
    // background, out of reach of Stop.
    cmd.args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
    if let Some(p) = port {
        cmd.args(["-p", &p.to_string()]);
    }
    cmd.args(t.args()).arg("--").arg(address);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up for it.
        cmd.creation_flags(0x0800_0000);
    }
    // Ended with the app even when it does not end well (on Windows its job does that). Tunnels
    // are started from the window's thread, which lasts as long as the app.
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        let parent = libc::getpid();
        cmd.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot start ssh: {e}"))?;
    crate::sys::tie(&child);
    let said = std::sync::Arc::new(Mutex::new(String::new()));
    if let Some(mut err) = child.stderr.take() {
        let said = said.clone();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut err, &mut text);
            *said.lock().unwrap_or_else(|e| e.into_inner()) = text;
        });
    }
    runs.push(Run { address: address.into(), tunnel: t.clone(), child: Some(child), said, since: std::time::Instant::now(), ended: None });
    Ok(())
}

/// Closes `t` of the computer at `address`, and forgets why it failed if it did.
pub fn stop(address: &str, t: &Tunnel) {
    RUNS.lock().unwrap_or_else(|e| e.into_inner()).retain_mut(|r| {
        let it = r.address == address && same(&r.tunnel, t);
        if it && let Some(c) = r.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        !it
    });
}

/// Where `t` of the computer at `address` is now.
pub fn state(address: &str, t: &Tunnel) -> State {
    let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(r) = runs.iter_mut().find(|r| r.address == address && same(&r.tunnel, t)) else { return State::Off };
    if let Some(c) = r.child.as_mut()
        && let Ok(Some(status)) = c.try_wait()
    {
        r.child = None;
        r.ended = Some(format!("ssh ended ({status})"));
    }
    match (&r.child, &r.ended) {
        (Some(_), _) if r.since.elapsed() < std::time::Duration::from_secs(3) => State::Starting,
        (Some(_), _) => State::On,
        // What ssh said is read to its end on a thread of its own, shortly after it ends.
        (None, Some(ended)) => {
            let said = r.said.lock().unwrap_or_else(|e| e.into_inner());
            State::Failed(said.lines().rfind(|l| !l.trim().is_empty()).map_or_else(|| ended.clone(), plain))
        }
        (None, None) => State::Off,
    }
}

/// Closes every tunnel of the computer at `address` but those in `keep`.
pub fn stop_others(address: &str, keep: &[Tunnel]) {
    let gone: Vec<Tunnel> =
        RUNS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|r| r.address == address && !keep.iter().any(|k| same(k, &r.tunnel))).map(|r| r.tunnel.clone()).collect();
    for t in gone {
        stop(address, &t);
    }
}

/// Closes them all, as the app ends (on Windows its job ends them too; elsewhere nothing would).
pub fn stop_all() {
    for r in RUNS.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        if let Some(mut c) = r.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// How many tunnels of the computer at `address` are open or opening.
pub fn open_count(address: &str, tunnels: &[Tunnel]) -> usize {
    tunnels.iter().filter(|t| matches!(state(address, t), State::On | State::Starting)).count()
}

/// Opens every tunnel set to start with the app; returns why those that could not, could not.
pub fn start_with_app() -> Vec<String> {
    let mut out = Vec::new();
    for c in crate::computers::all() {
        for t in c.tunnels.iter().filter(|t| t.start_with_app) {
            if let Err(e) = start(&c.address, c.port, t) {
                out.push(format!("{} {}: {e}", c.label(), t.label()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnels_make_their_ssh_options_and_refuse_what_ssh_would_misread() {
        let t = Tunnel { kind: Kind::Local, port: 5432, dest: "localhost:5432".into(), start_with_app: true };
        assert_eq!(t.args(), ["-L", "5432:localhost:5432"]);
        assert_eq!(Tunnel::from_json(&t.to_json()), Some(t));
        let socks = Tunnel { kind: Kind::Socks, port: 1080, dest: String::new(), start_with_app: false };
        assert!(socks.problem().is_none() && socks.args() == ["-D", "1080"]);
        for dest in ["", "-oProxyCommand=x:1", "host", "a b:1", "host:0", "host:99999"] {
            let bad = Tunnel { kind: Kind::Remote, port: 80, dest: dest.into(), start_with_app: false };
            assert!(bad.problem().is_some(), "{dest:?}");
        }
        assert!(plain("bind [127.0.0.1]:8080: Address already in use").contains("taken"));
    }

    /// A tunnel whose ssh cannot sign in says so in plain words, and stopping it forgets it.
    #[test]
    fn a_tunnel_that_cannot_open_says_why() {
        let t = Tunnel { kind: Kind::Local, port: 47_321, dest: "localhost:1".into(), start_with_app: false };
        let address = "nobody@127.0.0.1";
        start(address, Some(1), &t).unwrap();
        let mut s = state(address, &t);
        for _ in 0..100 {
            // First "ssh ended", then what it said, once read.
            if matches!(&s, State::Failed(why) if !why.starts_with("ssh ended")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            s = state(address, &t);
        }
        let State::Failed(why) = s else { panic!("{s:?}") };
        assert!(why.contains("could not be reached"), "{why}");
        stop(address, &t);
        assert_eq!(state(address, &t), State::Off);
    }
}
