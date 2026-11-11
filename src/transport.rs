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
}

impl Link {
    pub fn close(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Waits for the other side to finish, as after it closed the stream.
    pub fn wait(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.wait();
        }
    }
}

pub use crate::vault::ASKPASS_SECRET;

pub fn open(target: &Target, hidden: bool) -> Result<Link, String> {
    if let Target::Direct { addr, password } = target {
        let (r, w) = crate::net::connect(addr, password)?;
        return Ok(Link { reader: Box::new(r), writer: Box::new(w), errors: Box::new(std::io::empty()), child: None });
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
                // ssh runs this program with the prompt as its only argument; main() prints the secret.
                c.env("SSH_ASKPASS", std::env::current_exe().map_err(|e| e.to_string())?);
                c.env("SSH_ASKPASS_REQUIRE", "force");
                c.env(ASKPASS_SECRET, pw);
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
    })
}
