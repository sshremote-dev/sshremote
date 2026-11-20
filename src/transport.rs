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
    /// A link by ID: ended on close, so the other end stops at once (the reader thread holds a
    /// copy, which would keep it open), and asked which way it runs.
    ender: Option<crate::net::Ender>,
}

impl Link {
    pub fn close(&mut self) {
        if let Some(c) = self.child.as_mut() {
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


pub fn open(target: &Target, hidden: bool) -> Result<Link, String> {
    if let Target::Direct { addr, password } = target {
        // Where it was last found on this network: tried first, straight, before the relay.
        let direct: Vec<String> = crate::computers::all()
            .into_iter()
            .find(|c| c.id() == Some(addr.id.as_str()))
            .map(|c| c.direct.split(',').map(str::trim).filter(|d| !d.is_empty()).map(String::from).collect())
            .unwrap_or_default();
        // A device the computer trusts gives its token where the two-step code is asked.
        let trust = crate::vault::trust_name(&crate::access::address(&addr.id, &addr.relay));
        let token = crate::vault::get(&trust).filter(|t| !t.trim().is_empty());
        let given = match &token {
            Some(t) => format!("{}{}{}{t}", password.split(crate::net::CODE_SEP).next().unwrap_or(""), crate::net::CODE_SEP, crate::net::TRUST_MARK),
            None => password.clone(),
        };
        let (r, w) = match crate::net::connect_via(addr, &given, &direct) {
            // Trusted no longer (its time is up, or it was forgotten there): the code typed
            // goes instead, else it is asked.
            Err(e) if token.is_some() && e == crate::net::NOT_TRUSTED => {
                let _ = crate::vault::delete(&trust);
                let typed = password.split_once(crate::net::CODE_SEP).is_some_and(|(_, c)| !c.trim().is_empty());
                if !typed {
                    return Err(e);
                }
                crate::net::connect_via(addr, password, &direct)?
            }
            r => r?,
        };
        let ender = Some(w.ender());
        return Ok(Link { reader: Box::new(r), writer: Box::new(w), errors: Box::new(std::io::empty()), child: None, ender });
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
    #[cfg(windows)]
    if hidden {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a window-mode viewer must not flash a console for ssh.
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = hidden;
    let mut child = cmd.spawn().map_err(|e| format!("could not start the agent: {e}"))?;
    crate::sys::tie(&child);
    Ok(Link {
        reader: Box::new(child.stdout.take().unwrap()),
        writer: Box::new(child.stdin.take().unwrap()),
        errors: Box::new(child.stderr.take().unwrap()),
        child: Some(child),
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
        let mut link = super::Link { reader: Box::new(r), writer: Box::new(w), errors: Box::new(std::io::empty()), child: None, ender };
        let started = Instant::now();
        link.close();
        let mut b = [0u8; 1];
        assert!(matches!(far.read(&mut b), Ok(0) | Err(_)), "the other side sees the end");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
