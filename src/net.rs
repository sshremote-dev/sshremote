//! Connections without ssh, through a relay by remote access ID. Both ends prove they know
//! the same password with SPAKE2 (an eavesdropper or the relay learns nothing it could guess
//! offline), then every byte is sealed with ChaCha20-Poly1305 under keys derived from that
//! exchange. The relay's lines are described in `access`.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use spake2::{Ed25519Group, Identity, Password, Spake2};

pub const RELAY_PORT: u16 = 7071;
const MAX_FRAME: usize = 1 << 20;

/// Where a viewer finds an agent: `sshremote://ID@relay[:port]`, or `sshremote://ID` at the default relay.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    pub id: String,
    /// As remote access keeps it: `host` or `host:port`, dialled by [`crate::access::dial`].
    pub relay: String,
}

impl Address {
    pub fn parse(s: &str) -> Option<Address> {
        let rest = s.strip_prefix("sshremote://")?.trim_end_matches('/');
        let (id, relay) = rest.split_once('@').unwrap_or((rest, crate::access::DEFAULT_RELAY));
        let (host, _) = crate::access::relay_addr(relay);
        (crate::access::valid_id(id) && !host.is_empty()).then(|| Address { id: id.into(), relay: relay.into() })
    }
}

/// One direction of the sealed stream: frames of `len u32 | ciphertext`, counter nonces.
struct Sealer {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl Sealer {
    fn new(key: &[u8]) -> Sealer {
        Sealer { cipher: ChaCha20Poly1305::new(Key::from_slice(key)), counter: 0 }
    }

    fn nonce(&mut self) -> Nonce {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter += 1;
        *Nonce::from_slice(&n)
    }
}

pub struct SealedWriter {
    inner: TcpStream,
    seal: Sealer,
    buf: Vec<u8>,
}

impl SealedWriter {
    fn emit(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let nonce = self.seal.nonce();
        let ct = self.seal.cipher.encrypt(&nonce, self.buf.as_slice()).map_err(|_| io::Error::other("seal"))?;
        self.buf.clear();
        self.inner.write_all(&(ct.len() as u32).to_le_bytes())?;
        self.inner.write_all(&ct)
    }
}

impl SealedWriter {
    /// The connection underneath, to set its timeouts or end it.
    pub fn stream(&self) -> io::Result<TcpStream> {
        self.inner.try_clone()
    }
}

impl Write for SealedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= 64 << 10 {
            self.emit()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit()?;
        self.inner.flush()
    }
}

pub struct SealedReader {
    inner: TcpStream,
    seal: Sealer,
    plain: Vec<u8>,
    at: usize,
}

impl Read for SealedReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            let mut len = [0u8; 4];
            match self.inner.read_exact(&mut len) {
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                r => r?,
            }
            let len = u32::from_le_bytes(len) as usize;
            if len > MAX_FRAME + 16 {
                return Err(io::Error::other("oversized frame"));
            }
            let mut ct = vec![0u8; len];
            self.inner.read_exact(&mut ct)?;
            let nonce = self.seal.nonce();
            self.plain =
                self.seal.cipher.decrypt(&nonce, ct.as_slice()).map_err(|_| {
                    io::Error::new(io::ErrorKind::PermissionDenied, "wrong password or tampered stream")
                })?;
            self.at = 0;
        }
        let n = out.len().min(self.plain.len() - self.at);
        out[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

const CONFIRM: &[u8] = b"sshremote/1 ready";

/// SPAKE2 on `stream`, then a sealed confirmation each way, so a wrong password fails here.
fn handshake(stream: TcpStream, password: &str, viewer: bool) -> io::Result<(SealedReader, SealedWriter)> {
    let (a, b) = (Identity::new(b"sshremote viewer"), Identity::new(b"sshremote agent"));
    let pw = Password::new(password.trim().as_bytes());
    let (state, msg) = if viewer {
        Spake2::<Ed25519Group>::start_a(&pw, &a, &b)
    } else {
        Spake2::<Ed25519Group>::start_b(&pw, &a, &b)
    };
    let mut s = stream;
    s.set_nodelay(true)?;
    s.write_all(&(msg.len() as u16).to_le_bytes())?;
    s.write_all(&msg)?;
    let mut len = [0u8; 2];
    s.read_exact(&mut len)?;
    let mut theirs = vec![0u8; (u16::from_le_bytes(len) as usize).min(256)];
    s.read_exact(&mut theirs)?;
    let shared = state.finish(&theirs).map_err(|_| io::Error::other("bad key exchange"))?;
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, &shared);
    let (mut v2a, mut a2v) = ([0u8; 32], [0u8; 32]);
    hk.expand(b"viewer to agent", &mut v2a).map_err(|_| io::Error::other("hkdf"))?;
    hk.expand(b"agent to viewer", &mut a2v).map_err(|_| io::Error::other("hkdf"))?;
    let (send, recv) = if viewer { (v2a, a2v) } else { (a2v, v2a) };
    let mut w = SealedWriter { inner: s.try_clone()?, seal: Sealer::new(&send), buf: Vec::new() };
    let mut r = SealedReader { inner: s, seal: Sealer::new(&recv), plain: Vec::new(), at: 0 };
    // The viewer proves the password first: the agent answers only a right one, so a guess
    // cannot be checked against the agent's answer and dropped before it counts as wrong.
    let mut got = [0u8; CONFIRM.len()];
    if viewer {
        w.write_all(CONFIRM)?;
        w.flush()?;
        r.read_exact(&mut got)?;
    } else {
        r.read_exact(&mut got)?;
        if got != CONFIRM {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
        }
        w.write_all(CONFIRM)?;
        w.flush()?;
    }
    if got != CONFIRM {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
    }
    Ok((r, w))
}

/// The agent's side of the handshake, for remote access by ID.
pub fn handshake_agent(stream: TcpStream, password: &str) -> io::Result<(SealedReader, SealedWriter)> {
    handshake(stream, password, false)
}

/// Viewer side: reaches the ID through its relay and runs the handshake.
pub fn connect(addr: &Address, password: &str) -> Result<(SealedReader, SealedWriter), String> {
    // Dialled as remote access does: port 80 when the relay's own port is shut.
    let mut s = crate::access::dial(&addr.relay).map_err(|e| e.to_string())?;
    writeln!(s, "viewer {}", addr.id).map_err(|e| e.to_string())?;
    match crate::access::line(&mut s).map_err(|e| e.to_string())?.as_str() {
        "ok" => {}
        other => return Err(format!("relay: {other}")),
    }
    // The agent may make a wrong guess wait its turn, but not forever.
    let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
    let pair = handshake(s, password, true).map_err(|e| e.to_string())?;
    if let Ok(t) = pair.1.stream() {
        let _ = t.set_read_timeout(None);
    }
    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn addresses() {
        assert_eq!(
            Address::parse("sshremote://123456789"),
            Some(Address { id: "123456789".into(), relay: crate::access::DEFAULT_RELAY.into() })
        );
        assert_eq!(
            Address::parse("sshremote://123456789@relay.example:9000"),
            Some(Address { id: "123456789".into(), relay: "relay.example:9000".into() })
        );
        assert_eq!(Address::parse("sshremote://10.0.0.5"), None);
        assert_eq!(Address::parse("user@host"), None);
    }

    fn pair(pw_agent: &'static str, pw_viewer: &'static str) -> (io::Result<String>, io::Result<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let agent = thread::spawn(move || -> io::Result<()> {
            let (s, _) = l.accept()?;
            let (_r, mut w) = handshake(s, pw_agent, false)?;
            w.write_all(b"hello over the sealed stream")?;
            w.flush()
        });
        let viewer = (|| {
            let s = TcpStream::connect(("127.0.0.1", port))?;
            let (mut r, _w) = handshake(s, pw_viewer, true)?;
            let mut buf = [0u8; 28];
            r.read_exact(&mut buf)?;
            Ok(String::from_utf8_lossy(&buf).into_owned())
        })();
        (viewer, agent.join().unwrap())
    }

    #[test]
    fn matching_passwords_open_a_sealed_stream() {
        let (v, a) = pair("123456789", "123456789");
        assert_eq!(v.unwrap(), "hello over the sealed stream");
        assert!(a.is_ok());
    }

    #[test]
    fn a_wrong_password_is_refused() {
        let (v, a) = pair("123456789", "987654321");
        assert!(v.is_err());
        // The agent sees the wrong proof itself, so the guess counts against the next one.
        assert_eq!(a.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}
