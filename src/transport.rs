//! How a viewer reaches an agent. Everything above this module only sees a
//! byte stream in each direction plus a diagnostics stream, so a future direct
//! or relayed network link only needs another `Target` and `open` arm.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};

pub use crate::options::Target;

pub struct Link {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Diagnostics such as ssh errors; drained into the session log.
    pub errors: Box<dyn Read + Send>,
    child: Option<Child>,
    /// The child is this computer's agent, which sets back what it changed (the screen's
    /// resolution) as it ends.
    local: bool,
    /// A link by ID: ended on close, so the other end stops at once (the reader thread holds a
    /// copy, which would keep it open), and asked which way it runs.
    ender: Option<crate::net::Ender>,
}

impl Link {
    pub fn close(&mut self) {
        if let Some(c) = self.child.as_mut() {
            // This computer's agent ends by itself once its input has ended, a moment later; one
            // over ssh ends when ssh does.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while self.local && std::time::Instant::now() < until && matches!(c.try_wait(), Ok(None)) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(e) = self.ender.take() {
            e.end();
        }
    }

    /// For a link by ID that asked without a password: the code to compare with the person there.
    pub fn code(&self) -> Option<String> {
        self.ender.as_ref().and_then(|e| e.code())
    }

    /// For a link by ID: where its straight path goes, or None while it runs through the relay.
    pub fn straight(&self) -> Option<Option<std::net::SocketAddr>> {
        self.ender.as_ref().map(|e| e.straight())
    }

    /// Waits for the other side to finish, as after it closed the stream.
    pub fn wait(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.wait();
        }
    }
}

/// A link let go on any path (an error on the way out included) ends its ssh or agent with it,
/// and reaps it.
impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}


/// This process connects for others (the web gateway's visitors): what this person keeps (the
/// tokens of devices computers trust, where computers were found) is not theirs to use.
static FOR_OTHERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn serve_others() {
    FOR_OTHERS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Keeps what a connection by ID showed about a computer's addresses on this network: one where
/// its password got in is proven; one found there that turned down the password the relay took
/// was someone else answering for it, so found ones are not believed for it any more.
fn note_way(c: &crate::computers::Computer, way: &crate::net::Way) {
    if let Some(at) = &way.straight {
        let mut proven: Vec<String> = c.proven.split(',').map(str::trim).filter(|d| !d.is_empty() && d != at).map(String::from).collect();
        proven.insert(0, at.clone());
        proven.truncate(4);
        let proven = proven.join(",");
        if proven != c.proven {
            crate::computers::update(&c.address, |k| k.proven = proven);
        }
    }
    if way.refused.is_some() && !c.found_refused {
        crate::computers::update(&c.address, |k| k.found_refused = true);
    }
}

pub fn open(target: &Target, hidden: bool) -> Result<Link, String> {
    if let Target::Direct { addr, password } = target {
        let own = !FOR_OTHERS.load(std::sync::atomic::Ordering::Relaxed);
        let known = crate::computers::all().into_iter().filter(|_| own).find(|c| c.id() == Some(addr.id.as_str()));
        let list = |s: &str| s.split(',').map(str::trim).filter(|d| !d.is_empty()).map(String::from).collect::<Vec<_>>();
        // Straight, before the relay: where its password got in before, then where it was last
        // found on this network unless an address found there was caught answering for it.
        // The addresses the person gave first (the port remote access takes straight when none).
        let given: Vec<String> = known.as_ref().map(|c| list(&c.straight)).unwrap_or_default().into_iter().map(|a| crate::computers::with_direct_port(&a)).collect();
        let mut proven = given;
        for p in known.as_ref().map(|c| list(&c.proven)).unwrap_or_default() {
            if !proven.contains(&p) {
                proven.push(p);
            }
        }
        let found: Vec<String> =
            known.as_ref().filter(|c| !c.found_refused).map(|c| list(&c.direct)).unwrap_or_default().into_iter().filter(|d| !proven.contains(d)).collect();
        // A device the computer trusts gives its token where the two-step code is asked.
        let trust = crate::vault::trust_name(&crate::access::address(&addr.id, &addr.relay));
        let token = own.then(|| crate::vault::get(&trust)).flatten().filter(|t| !t.trim().is_empty());
        let given = match &token {
            Some(t) => format!("{}{}{}{t}", password.split(crate::net::CODE_SEP).next().unwrap_or(""), crate::net::CODE_SEP, crate::net::TRUST_MARK),
            None => password.clone(),
        };
        let ((r, w), way) = match crate::net::connect_via(addr, &given, &proven, &found) {
            // Trusted no longer (its time is up, or it was forgotten there): the code typed
            // goes instead, else it is asked.
            Err(e) if token.is_some() && e == crate::net::NOT_TRUSTED => {
                let _ = crate::vault::delete(&trust);
                let typed = password.split_once(crate::net::CODE_SEP).is_some_and(|(_, c)| !c.trim().is_empty());
                if !typed {
                    return Err(e);
                }
                crate::net::connect_via(addr, password, &proven, &found)?
            }
            r => r?,
        };
        if let Some(c) = &known {
            note_way(c, &way);
        }
        let ender = Some(w.ender());
        return Ok(Link { reader: Box::new(r), writer: Box::new(w), errors: Box::new(std::io::empty()), child: None, local: false, ender });
    }
    let mut cmd = match target {
        Target::Direct { .. } => unreachable!(),
        Target::Local { capture } => {
            // Tests run as a test binary, which is no agent: they name the built program instead.
            #[cfg(test)]
            let exe = std::env::var_os("SSHREMOTE_EXE").map(std::path::PathBuf::from).ok_or("set SSHREMOTE_EXE")?;
            #[cfg(not(test))]
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let mut c = Command::new(exe);
            c.args(["--agent", "--capture", capture]);
            c
        }
        Target::Ssh { host, port, args, remote, capture, password, unattended } => {
            let mut c = Command::new("ssh");
            c.arg("-T");
            if let Some(p) = port {
                c.args(["-p", &p.to_string()]);
            }
            if *unattended {
                c.args(["-o", "StrictHostKeyChecking=accept-new"]);
                if password.is_none() {
                    c.args(["-o", "BatchMode=yes"]);
                }
            }
            c.args(args).arg(host).arg(format!("{remote} --capture {capture}"));
            if let Some(pw) = password {
                // ssh runs this program with the prompt as its only argument; main() prints the
                // secret, to the destination's prompt only.
                c.args(["-o", "NumberOfPasswordPrompts=1"]);
                c.envs(crate::vault::askpass_env(host, *port, pw));
            }
            c
        }
    };
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A window-mode viewer must not flash a console for ssh.
    if hidden {
        crate::sys::quiet(&mut cmd);
    }
    let mut child = cmd.spawn().map_err(|e| format!("could not start the agent: {e}"))?;
    crate::sys::tie(&child);
    Ok(Link {
        reader: Box::new(child.stdout.take().unwrap()),
        writer: Box::new(child.stdin.take().unwrap()),
        errors: Box::new(child.stderr.take().unwrap()),
        child: Some(child),
        local: matches!(target, Target::Local { .. }),
        ender: None,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::{Duration, Instant};

    /// Closing a link by ID tells the other end at once, though a reader still holds the stream.
    #[test]
    fn closing_a_link_by_id_ends_it_for_the_other_side() {
        let ((r, w), (mut far, _keep)) = crate::net::sealed_pair(false);
        let ender = Some(w.ender());
        let mut link = super::Link { reader: Box::new(r), writer: Box::new(w), errors: Box::new(std::io::empty()), child: None, local: false, ender };
        let started = Instant::now();
        link.close();
        let mut b = [0u8; 1];
        assert!(matches!(far.read(&mut b), Ok(0) | Err(_)), "the other side sees the end");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// What a connection by ID showed is kept: where its password got in is proven, the latest
    /// first; a found address caught answering for it stops found ones being believed.
    #[test]
    fn what_a_connection_by_id_showed_is_kept() {
        let address = "sshremote://918273645@relay.example";
        crate::computers::update(address, |c| c.direct = "10.0.0.66:7073".into());
        let get = || crate::computers::get(address).unwrap();
        let way = |straight: Option<&str>, refused: Option<&str>| crate::net::Way { straight: straight.map(Into::into), refused: refused.map(Into::into) };
        super::note_way(&get(), &way(Some("10.0.0.5:7073"), None));
        super::note_way(&get(), &way(Some("10.0.0.9:7073"), None));
        super::note_way(&get(), &way(Some("10.0.0.5:7073"), None));
        assert_eq!((get().proven.as_str(), get().found_refused), ("10.0.0.5:7073,10.0.0.9:7073", false));
        super::note_way(&get(), &way(None, Some("10.0.0.66:7073")));
        assert!(get().found_refused);
        crate::computers::forget(address);
    }
}
