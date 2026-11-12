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
    /// A direct link's socket: shut on close, so the other end stops at once (the reader thread
    /// holds a copy, which would keep it open).
    socket: Option<std::net::TcpStream>,
}

impl Link {
    pub fn close(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(s) = self.socket.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
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
        let (r, w) = crate::net::connect(addr, password)?;
        let socket = w.stream().ok();
        return Ok(Link { reader: Box::new(r), writer: Box::new(w), errors: Box::new(std::io::empty()), child: None, socket });
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
        socket: None,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    /// Closing a direct link tells the other end at once, though a reader still holds the socket.
    #[test]
    fn closing_a_direct_link_ends_it_for_the_other_side() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (mut far, _) = l.accept().unwrap();
        let reader = s.try_clone().unwrap();
        let mut link = super::Link {
            reader: Box::new(reader),
            writer: Box::new(s.try_clone().unwrap()),
            errors: Box::new(std::io::empty()),
            child: None,
            socket: Some(s),
        };
        link.close();
        far.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let started = Instant::now();
        let mut b = [0u8; 1];
        assert_eq!(far.read(&mut b).unwrap(), 0, "the other side sees the end");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
