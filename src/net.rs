//! Direct and relayed connections without ssh. Both ends prove they know the
//! same one-time password with SPAKE2 (an eavesdropper or a relay learns
//! nothing it could guess offline), then every byte is sealed with
//! ChaCha20-Poly1305 under keys derived from that exchange.
//!
//! Relay protocol: each side opens a TCP connection to the relay and sends one
//! line, `agent ID` or `viewer ID`; the relay answers `ok` once both are there
//! and from then on only copies bytes.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use spake2::{Ed25519Group, Identity, Password, Spake2};

pub const PORT: u16 = 7070;
pub const RELAY_PORT: u16 = 7071;
const MAX_FRAME: usize = 1 << 20;

/// Where a viewer finds an agent: `desk://host[:port]` or `desk://ID@relay[:port]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    pub host: String,
    pub port: u16,
    pub id: Option<String>,
}

impl Address {
    pub fn parse(s: &str) -> Option<Address> {
        let rest = s.strip_prefix("desk://")?;
        let (id, hostport) = match rest.split_once('@') {
            Some((id, hp)) => (Some(id.to_string()), hp),
            None => (None, rest),
        };
        let default = if id.is_some() { RELAY_PORT } else { PORT };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() => (h.to_string(), p.parse().ok()?),
            _ => (hostport.to_string(), default),
        };
        (!host.is_empty()).then_some(Address { host, port, id })
    }
}

fn random_digits(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("system random source");
    b.iter().map(|x| char::from(b'0' + x % 10)).collect()
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

const CONFIRM: &[u8] = b"desk/1 ready";

/// SPAKE2 on `stream`, then a sealed confirmation each way, so a wrong password fails here.
fn handshake(stream: TcpStream, password: &str, viewer: bool) -> io::Result<(SealedReader, SealedWriter)> {
    let (a, b) = (Identity::new(b"desk viewer"), Identity::new(b"desk agent"));
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
    w.write_all(CONFIRM)?;
    w.flush()?;
    let mut got = [0u8; CONFIRM.len()];
    r.read_exact(&mut got)?;
    if got != CONFIRM {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
    }
    Ok((r, w))
}

fn relay_hello(stream: &mut TcpStream, role: &str, id: &str) -> io::Result<()> {
    writeln!(stream, "{role} {id}")?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    if line.trim() == "ok" { Ok(()) } else { Err(io::Error::other(format!("relay: {}", line.trim()))) }
}

/// Viewer side: connects directly or through a relay and runs the handshake.
pub fn connect(addr: &Address, password: &str) -> Result<(SealedReader, SealedWriter), String> {
    let mut s =
        TcpStream::connect((addr.host.as_str(), addr.port)).map_err(|e| format!("{}:{}: {e}", addr.host, addr.port))?;
    if let Some(id) = &addr.id {
        relay_hello(&mut s, "viewer", id).map_err(|e| e.to_string())?;
    }
    handshake(s, password, true).map_err(|e| e.to_string())
}

/// Agent side of `desk --serve`: waits for viewers, one at a time, and serves each.
pub fn serve(listen: Option<&str>, via: Option<&str>, password: Option<&str>, backend: &str) -> Result<(), String> {
    let password = password.map(str::to_string).unwrap_or_else(|| random_digits(9));
    let id = random_digits(9);
    match via {
        Some(r) => println!(
            "desk is waiting through relay {r}\n  ID:       {id}\n  password: {password}\n  connect with: desk --gui desk://{id}@{r}"
        ),
        None => println!("desk is listening on {}\n  password: {password}", listen.unwrap_or("0.0.0.0:7070")),
    }
    let listener = match via {
        None => Some(TcpListener::bind(listen.unwrap_or("0.0.0.0:7070")).map_err(|e| e.to_string())?),
        Some(_) => None,
    };
    let mut failures = 0;
    loop {
        let stream = match (&listener, via) {
            (Some(l), _) => l.accept().map_err(|e| e.to_string())?.0,
            (None, Some(r)) => {
                let addr = Address::parse(&format!("desk://{id}@{r}")).ok_or("bad relay address")?;
                let mut s = match TcpStream::connect((addr.host.as_str(), addr.port)) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("desk: relay {r}: {e}; retrying");
                        thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                };
                if let Err(e) = relay_hello(&mut s, "agent", &id) {
                    eprintln!("desk: {e}; retrying");
                    thread::sleep(Duration::from_secs(5));
                    continue;
                }
                s
            }
            (None, None) => unreachable!(),
        };
        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        match handshake(stream, &password, false) {
            Ok((r, w)) => {
                failures = 0;
                eprintln!("desk: viewer connected from {peer}");
                if let Err(e) = crate::agent::run_on(backend, Box::new(r), Box::new(w)) {
                    eprintln!("desk: session ended: {e}");
                }
                eprintln!("desk: viewer disconnected");
            }
            Err(e) => {
                failures += 1;
                eprintln!("desk: rejected {peer}: {e}");
                // Each wrong guess costs time; many end the server so the password cannot be ground down.
                if failures >= 10 {
                    return Err("too many failed attempts; start again for a new password".into());
                }
                thread::sleep(Duration::from_secs(2 * failures));
            }
        }
    }
}

/// `desk --relay`: pairs agents and viewers by ID and copies bytes between them.
pub fn relay(listen: Option<&str>) -> Result<(), String> {
    let addr = listen.unwrap_or("0.0.0.0:7071");
    let listener = TcpListener::bind(addr).map_err(|e| e.to_string())?;
    println!("desk relay listening on {addr}");
    let waiting: Arc<Mutex<HashMap<String, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
    for s in listener.incoming().flatten() {
        let waiting = waiting.clone();
        thread::spawn(move || {
            let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
            let mut line = String::new();
            if BufReader::new(&s).read_line(&mut line).is_err() {
                return;
            }
            let _ = s.set_read_timeout(None);
            let (role, id) = match line.split_whitespace().collect::<Vec<_>>().as_slice() {
                [role @ ("agent" | "viewer"), id] if id.len() <= 32 => (role.to_string(), id.to_string()),
                _ => return,
            };
            let mut s = s;
            if role == "agent" {
                waiting.lock().unwrap().insert(id, s);
                return;
            }
            let Some(mut agent) = waiting.lock().unwrap().remove(&id) else {
                let _ = writeln!(s, "no agent with that ID");
                return;
            };
            if writeln!(agent, "ok").is_err() || writeln!(s, "ok").is_err() {
                return;
            }
            let (Ok(mut a2), Ok(mut s2)) = (agent.try_clone(), s.try_clone()) else { return };
            thread::spawn(move || {
                let _ = io::copy(&mut a2, &mut s2);
                let _ = s2.shutdown(std::net::Shutdown::Both);
            });
            let _ = io::copy(&mut s, &mut agent);
            let _ = agent.shutdown(std::net::Shutdown::Both);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(Address::parse("desk://10.0.0.5"), Some(Address { host: "10.0.0.5".into(), port: PORT, id: None }));
        assert_eq!(
            Address::parse("desk://123456789@relay.example:9000"),
            Some(Address { host: "relay.example".into(), port: 9000, id: Some("123456789".into()) })
        );
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
        let (v, _) = pair("123456789", "987654321");
        assert!(v.is_err());
    }
}
