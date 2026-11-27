//! Port forwards through an SSH computer (the ssh that keeps one open) or through a computer
//! reached by its ID (a session with its agent, `porttunnel`).

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
            Kind::Local => n!("Here to there"),
            Kind::Remote => n!("There to here"),
            Kind::Socks => n!("SOCKS proxy"),
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
    /// What it does, in a few words, for the window: "here 8080 → localhost:80".
    pub fn label(&self) -> String {
        match self.kind {
            Kind::Local => t!("here {} → {}", self.port, self.dest),
            Kind::Remote => t!("there {} → {}", self.port, self.dest),
            Kind::Socks => t!("SOCKS on {}", self.port),
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
            return Some(n!("The port is a number from 1 to 65535."));
        }
        if self.kind == Kind::Socks {
            return None;
        }
        let ok = |s: &str| !s.is_empty() && !s.starts_with('-') && !s.chars().any(char::is_whitespace);
        match self.dest.rsplit_once(':') {
            Some((host, port)) if ok(host) && port.parse::<u16>().is_ok_and(|p| p > 0) => None,
            _ => Some(n!("Where it leads is HOST:PORT, such as localhost:5432.")),
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
        n!("the computer wants a password, which a tunnel cannot type: install your key on it (Settings > SSH keys)").into()
    } else if l.contains("address already in use") || l.contains("cannot listen") || l.contains("forwarding failed") {
        n!("that port is already taken; pick another").into()
    } else if l.contains("could not resolve") || l.contains("no such host") {
        n!("the computer's name is not known").into()
    } else if l.contains("timed out") || l.contains("connection refused") || l.contains("unreachable") {
        n!("the computer could not be reached").into()
    } else {
        why.trim().to_string()
    }
}

/// A tunnel open now: whose it is, and the ssh keeping it open with what it said.
struct Run {
    address: String,
    tunnel: Tunnel,
    child: Option<std::process::Child>,
    /// Through a computer reached by its ID instead.
    #[cfg(feature = "remote")]
    by_id: Option<crate::porttunnel::Forward>,
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
    runs.retain(|r| !(r.address == address && same(&r.tunnel, t) && r.child.is_none() && !r.through_id()));
    if runs.iter().any(|r| r.address == address && same(&r.tunnel, t)) {
        return Ok(());
    }
    if address.starts_with("sshremote://") {
        return start_by_id(&mut runs, address, t);
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
    // No console flashes up for it.
    crate::sys::quiet(&mut cmd);
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
    let mut child = cmd.spawn().map_err(|e| crate::lang::fill(n!("cannot start ssh: {}"), &[&e]))?;
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
    runs.push(Run {
        address: address.into(),
        tunnel: t.clone(),
        child: Some(child),
        #[cfg(feature = "remote")]
        by_id: None,
        said,
        since: std::time::Instant::now(),
        ended: None,
    });
    Ok(())
}

impl Run {
    fn through_id(&self) -> bool {
        #[cfg(feature = "remote")]
        return self.by_id.is_some();
        #[cfg(not(feature = "remote"))]
        false
    }
}

/// A port here leading somewhere a computer reached by its ID reaches, with its saved password.
#[cfg(feature = "remote")]
fn start_by_id(runs: &mut Vec<Run>, address: &str, t: &Tunnel) -> Result<(), String> {
    if t.kind != Kind::Local {
        return Err(n!("Through a computer reached by its ID, a tunnel goes from here to there only.").into());
    }
    let password = crate::vault::get(address).ok_or(n!("Save its password first: a tunnel has nobody to ask for it."))?;
    let fwd = crate::porttunnel::open(address, password, t.port, &t.dest)?;
    runs.push(Run {
        address: address.into(),
        tunnel: t.clone(),
        child: None,
        by_id: Some(fwd),
        said: Default::default(),
        since: std::time::Instant::now(),
        ended: None,
    });
    Ok(())
}

#[cfg(not(feature = "remote"))]
fn start_by_id(_: &mut Vec<Run>, _: &str, _: &Tunnel) -> Result<(), String> {
    // A build made without a part (`cargo build` with fewer features) says so.
    Err(n!("this sshremote was built without tunnels through a computer reached by its ID").into())
}

/// Closes `t` of the computer at `address`, and forgets why it failed if it did.
/// Where Remote Desktop listens on a Windows computer.
const RDP_DEST: &str = "localhost:3389";

/// Windows' Remote Desktop on the computer at `address`: a tunnel from a free port here to its
/// port 3389 (the one opened for it before, while it is up), and this computer's RDP client
/// opened on it once it is. Waits for the tunnel: call it from a thread of its own.
pub fn open_rdp(address: &str, port: Option<u16>) -> Result<(), String> {
    let up = RUNS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|r| r.address == address && r.tunnel.kind == Kind::Local && r.tunnel.dest == RDP_DEST).map(|r| r.tunnel.clone());
    let t = match up.filter(|t| matches!(state(address, t), State::On | State::Starting)) {
        Some(t) => t,
        None => {
            let free = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_err(|e| e.to_string())?.port();
            Tunnel { kind: Kind::Local, port: free, dest: RDP_DEST.into(), start_with_app: false }
        }
    };
    start(address, port, &t)?;
    let since = std::time::Instant::now();
    loop {
        match state(address, &t) {
            State::On => break,
            State::Failed(e) => return Err(e),
            State::Off => return Err(n!("the tunnel closed").into()),
            State::Starting if since.elapsed() > std::time::Duration::from_secs(20) => return Err(n!("the tunnel did not open in time").into()),
            State::Starting => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
    rdp_client(t.port)
}

/// This computer's RDP client, on `127.0.0.1:port`.
fn rdp_client(port: u16) -> Result<(), String> {
    let at = format!("127.0.0.1:{port}");
    #[cfg(not(target_os = "macos"))]
    let spawn = |program: &str, args: &[String]| {
        let mut c = std::process::Command::new(program);
        c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        c.spawn().map(|_| ())
    };
    #[cfg(windows)]
    return spawn("mstsc", &[format!("/v:{at}")]).map_err(|e| format!("mstsc: {e}"));
    // Microsoft's Windows App (Remote Desktop) takes rdp:// links.
    #[cfg(target_os = "macos")]
    return std::process::Command::new("open")
        .arg(format!("rdp://full%20address=s:{at}"))
        .status()
        .map_err(|e| e.to_string())
        .and_then(|s| if s.success() { Ok(()) } else { Err(n!("no RDP client found here: install Windows App from the App Store").into()) });
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let free = [format!("/v:{at}"), "/dynamic-resolution".into(), "+clipboard".into(), "/cert:tofu".into()];
        for (program, args) in [
            ("xfreerdp3", &free[..]),
            ("wlfreerdp3", &free[..]),
            ("xfreerdp", &free[..]),
            ("wlfreerdp", &free[..]),
            ("remmina", &["-c".to_string(), format!("rdp://{at}")][..]),
        ] {
            match spawn(program, args) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                r => return r.map_err(|e| format!("{program}: {e}")),
            }
        }
        Err(n!("no RDP client found here: install FreeRDP or Remmina").into())
    }
}

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
    #[cfg(feature = "remote")]
    if let Some(f) = &r.by_id {
        return match f.status() {
            crate::porttunnel::Status::Starting => State::Starting,
            crate::porttunnel::Status::On => State::On,
            crate::porttunnel::Status::Failed(why) => State::Failed(why),
        };
    }
    if let Some(c) = r.child.as_mut()
        && let Ok(Some(status)) = c.try_wait()
    {
        r.child = None;
        r.ended = Some(crate::lang::fill(n!("ssh ended ({})"), &[&status]));
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

/// Whether any tunnel is open or opening.
pub fn any_open() -> bool {
    RUNS.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().any(|r| r.through_id() || r.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None))))
}

/// Opens every tunnel set to start with the app; returns why those that could not, could not.
pub fn start_with_app() -> Vec<String> {
    let mut out = Vec::new();
    for c in crate::computers::all() {
        for t in c.tunnels.iter().filter(|t| t.start_with_app) {
            if let Err(e) = start(&c.address, c.port, t) {
                out.push(format!("{} {}: {}", crate::lang::tr_msg(&c.label()), t.label(), crate::lang::tr_msg(&e)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {

    /// Run alone with `SSHR_RDP_TO` an ssh address (an alias) that reaches a Windows computer with
    /// Remote Desktop on, by key, and a stand-in `xfreerdp3` first on PATH that writes its
    /// arguments to `$SSHR_RDP_SAID`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn remote_desktop_opens_through_a_tunnel_in_the_rdp_client() {
        let to = std::env::var("SSHR_RDP_TO").expect("SSHR_RDP_TO");
        let said = std::path::PathBuf::from(std::env::var("SSHR_RDP_SAID").expect("SSHR_RDP_SAID"));
        let _ = std::fs::remove_file(&said);
        open_rdp(&to, None).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let args = std::fs::read_to_string(&said).unwrap();
        let port: u16 = args.split_whitespace().find_map(|a| a.strip_prefix("/v:127.0.0.1:")).unwrap().parse().unwrap();
        // The tunnel leads to the computer's Remote Desktop: its first answer is a TPKT header.
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        // An X.224 connection request asking for TLS.
        s.write_all(&[3, 0, 0, 19, 14, 0xe0, 0, 0, 0, 0, 0, 1, 0, 8, 0, 1, 0, 0, 0]).unwrap();
        let mut b = [0u8; 4];
        s.read_exact(&mut b).unwrap();
        assert_eq!(b[0], 3, "a TPKT answer: {b:?}");
        // Asked again: the same tunnel.
        let up = RUNS.lock().unwrap().iter().filter(|r| r.address == to && r.tunnel.dest == RDP_DEST).count();
        open_rdp(&to, None).unwrap();
        assert_eq!(RUNS.lock().unwrap().iter().filter(|r| r.address == to && r.tunnel.dest == RDP_DEST).count(), up);
        stop_all();
    }
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
