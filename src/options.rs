//! What the command line asks a viewer for: kept apart from the session so a build without
//! remote screens still reads it.

use crate::color::Mode;

/// Command that starts the agent on a host, finding it in the usual install places.
pub const REMOTE: &str = "PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:$PATH\" sshremote --agent";

#[derive(Clone)]
pub struct Options {
    pub host: Option<String>,
    pub local: bool,
    pub gui: bool,
    pub remote: String,
    pub capture: String,
    pub ssh_args: Vec<String>,
    /// Password of a remote access ID, for an `sshremote://` address.
    pub password: Option<String>,
    /// `None` is the auto preset.
    pub preset: Option<usize>,
    /// Traffic auto stays under, in KB/s; `None` follows the link's delay alone.
    pub budget: Option<f64>,
    pub mode: Option<Mode>,
    pub scale: Option<u8>,
    pub fps: Option<u8>,
    pub cell: Option<(usize, usize)>,
    /// Headless run: (seconds, view size, optional PPM dump of the final framebuffer).
    pub probe: Option<(f64, (u16, u16), Option<String>)>,
    /// Probe only: text typed one key per 500 ms after a 2 s settle, to measure per-key traffic.
    pub probe_type: String,
    /// Probe only: scroll steps sent the same way after the typed text.
    pub probe_scroll: usize,
    /// Probe only: a file uploaded at the start, to check transfers end to end.
    pub probe_send: Option<String>,
    /// Probe only: a remote file or folder downloaded at the start.
    pub probe_fetch: Option<String>,
    /// Probe only: asks for this monitor at the start.
    pub probe_monitor: Option<u8>,
    /// Plays the host's sound.
    pub sound: bool,
    /// Window mode: what the session opens with besides the screen: "terminal" or "files".
    pub open: String,
}

/// Where the agent runs.
#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "remote"), allow(dead_code))]
pub enum Target {
    /// A child process on this machine.
    Local { capture: String },
    /// `ssh -T` to a host that runs `sshremote --agent`.
    Ssh {
        /// `[user@]host` as ssh takes it.
        host: String,
        port: Option<u16>,
        /// Extra ssh arguments.
        args: Vec<String>,
        /// Command that starts the agent on the host.
        remote: String,
        capture: String,
        /// Answered to ssh's password prompt instead of asking on a terminal.
        password: Option<String>,
        /// No terminal to ask on: trust a host key the first time, fail instead of prompting.
        unattended: bool,
    },
    #[cfg(feature = "remote")]
    /// A computer reached by its remote access ID through a relay, sealed with that ID's password.
    Direct { addr: crate::net::Address, password: String },
}
