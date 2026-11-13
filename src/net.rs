//! Connections without ssh, through a relay by remote access ID. Both ends prove they know
//! the same password with SPAKE2 (an eavesdropper or the relay learns nothing it could guess
//! offline), then every byte is sealed with ChaCha20-Poly1305 under keys derived from that
//! exchange. The relay's lines are described in `access`.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use spake2::{Ed25519Group, Identity, Password, Spake2};

pub const RELAY_PORT: u16 = 7071;
const MAX_FRAME: usize = 1 << 20;
/// What one write puts in a sealed frame at most; a larger write goes out in several.
const WRITE_FRAME: usize = 64 << 10;
/// The whole handshake, from the first byte to the password proven, takes at most this long:
/// a caller that trickles bytes cannot hold the agent's one password check.
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);

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
    /// Takes as much of `data` as fits in the frame being filled, so no frame is larger than the
    /// other side accepts (`write_all` loops for the rest).
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let take = data.len().min(WRITE_FRAME - self.buf.len());
        self.buf.extend_from_slice(&data[..take]);
        if self.buf.len() >= WRITE_FRAME {
            self.emit()?;
        }
        Ok(take)
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
    /// The largest frame taken: the confirmation's size until the password is proven.
    limit: usize,
}

impl SealedReader {
    /// The next frame as it came, still sealed; None at the end of the stream.
    fn sealed_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut len = [0u8; 4];
        match self.inner.read_exact(&mut len) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            r => r?,
        }
        let len = u32::from_le_bytes(len) as usize;
        if len > self.limit + 16 {
            return Err(io::Error::other("oversized frame"));
        }
        let mut ct = vec![0u8; len];
        self.inner.read_exact(&mut ct)?;
        Ok(Some(ct))
    }

    /// Opens a frame `sealed_frame` read, the next in order.
    fn open(&mut self, ct: &[u8]) -> io::Result<Vec<u8>> {
        let nonce = self.seal.nonce();
        self.seal.cipher.decrypt(&nonce, ct).map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "wrong password or tampered stream"))
    }
}

impl Read for SealedReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            let Some(ct) = self.sealed_frame()? else { return Ok(0) };
            self.plain = self.open(&ct)?;
            self.at = 0;
        }
        let n = out.len().min(self.plain.len() - self.at);
        out[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

const CONFIRM: &[u8] = b"sshremote/1 ready";

/// Decides whether the viewer's proof of the password is checked now: given the check, it runs it
/// in its own time (its turn, a delay) and says whether the password was right; an error turns
/// the call away unchecked.
pub type Judge<'a> = &'a dyn Fn(&mut dyn FnMut() -> bool) -> io::Result<bool>;

/// How long a viewer waits for the computer's verdict on its password: the computer may make a
/// guess wait its turn and a delay first.
const VERDICT_LIMIT: Duration = Duration::from_secs(75);

/// SPAKE2 on `stream`, then a sealed confirmation each way, so a wrong password fails here. On
/// a failure the connection is shut at once, so the other side hears it without waiting.
fn handshake(stream: TcpStream, password: &str, viewer: bool, judge: Judge) -> io::Result<(SealedReader, SealedWriter)> {
    let closer = stream.try_clone()?;
    let result = handshake_on(stream, password, viewer, judge);
    if result.is_err() {
        let _ = closer.shutdown(std::net::Shutdown::Both);
    }
    result
}

fn handshake_on(stream: TcpStream, password: &str, viewer: bool, judge: Judge) -> io::Result<(SealedReader, SealedWriter)> {
    let (a, b) = (Identity::new(b"sshremote viewer"), Identity::new(b"sshremote agent"));
    let pw = Password::new(password.trim().as_bytes());
    let (state, msg) = if viewer {
        Spake2::<Ed25519Group>::start_a(&pw, &a, &b)
    } else {
        Spake2::<Ed25519Group>::start_b(&pw, &a, &b)
    };
    let mut s = stream;
    s.set_nodelay(true)?;
    // Past the deadline the connection is shut, which ends any read still waiting on it. The
    // network's part has the deadline; the wait for a verdict is not the peer's to drag out.
    let started = std::time::Instant::now();
    let deadline = Arc::new(AtomicU64::new(HANDSHAKE_LIMIT.as_millis() as u64));
    let done = Arc::new(AtomicBool::new(false));
    if let Ok(watch) = s.try_clone() {
        let (done, deadline) = (done.clone(), deadline.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                if started.elapsed().as_millis() as u64 > deadline.load(Ordering::Acquire) {
                    let _ = watch.shutdown(std::net::Shutdown::Both);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }
    let extend = |d: Duration| deadline.store(started.elapsed().as_millis() as u64 + d.as_millis() as u64, Ordering::Release);
    let late = || started.elapsed().as_millis() as u64 >= deadline.load(Ordering::Acquire);
    struct Done(Arc<AtomicBool>);
    impl Drop for Done {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _done = Done(done);
    s.write_all(&(msg.len() as u16).to_le_bytes())?;
    s.write_all(&msg)?;
    let mut len = [0u8; 2];
    if let Err(e) = s.read_exact(&mut len) {
        // A computer that will not check a password now hangs up before the exchange.
        return Err(if viewer && e.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(io::ErrorKind::ConnectionRefused, "the computer turned the call away: it is busy, or it stopped taking passwords after too many wrong ones")
        } else {
            e
        });
    }
    // Both sides' messages are the same size: anything else is not this program.
    if u16::from_le_bytes(len) as usize != msg.len() {
        return Err(io::Error::other("bad key exchange"));
    }
    let mut theirs = vec![0u8; msg.len()];
    s.read_exact(&mut theirs)?;
    let shared = state.finish(&theirs).map_err(|_| io::Error::other("bad key exchange"))?;
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, &shared);
    let (mut v2a, mut a2v) = ([0u8; 32], [0u8; 32]);
    hk.expand(b"viewer to agent", &mut v2a).map_err(|_| io::Error::other("hkdf"))?;
    hk.expand(b"agent to viewer", &mut a2v).map_err(|_| io::Error::other("hkdf"))?;
    let (send, recv) = if viewer { (v2a, a2v) } else { (a2v, v2a) };
    let mut w = SealedWriter { inner: s.try_clone()?, seal: Sealer::new(&send), buf: Vec::new() };
    let mut r = SealedReader { inner: s, seal: Sealer::new(&recv), plain: Vec::new(), at: 0, limit: CONFIRM.len() };
    // The viewer proves the password first: the agent answers only a right one, so a guess
    // cannot be checked against the agent's answer and dropped before it counts as wrong.
    let mut got = [0u8; CONFIRM.len()];
    if viewer {
        w.write_all(CONFIRM)?;
        w.flush()?;
        extend(VERDICT_LIMIT);
        // A computer that did not take the password closes the connection without a word.
        if let Err(e) = r.read_exact(&mut got) {
            return Err(match e.kind() {
                _ if late() => io::Error::new(io::ErrorKind::TimedOut, "the computer did not answer the password in time"),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => {
                    io::Error::new(io::ErrorKind::PermissionDenied, "wrong password")
                }
                _ => e,
            });
        }
    } else {
        // The proof is read as it comes; checking it waits on the judge, with no deadline
        // meanwhile (the network's part is over).
        let ct = r.sealed_frame()?.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        deadline.store(u64::MAX, Ordering::Release);
        let right = judge(&mut || r.open(&ct).is_ok_and(|p| p == CONFIRM))?;
        extend(HANDSHAKE_LIMIT);
        if !right {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
        }
        got.copy_from_slice(CONFIRM);
        w.write_all(CONFIRM)?;
        w.flush()?;
    }
    if got != CONFIRM {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
    }
    r.limit = MAX_FRAME;
    Ok((r, w))
}

/// The agent's side of the handshake, for remote access by ID; `judge` checks the viewer's proof
/// of the password when it allows.
pub fn handshake_agent(stream: TcpStream, password: &str, judge: Judge) -> io::Result<(SealedReader, SealedWriter)> {
    handshake(stream, password, false, judge)
}

/// A judge that checks at once.
fn now(check: &mut dyn FnMut() -> bool) -> io::Result<bool> {
    Ok(check())
}

/// The port a computer with remote access answers on directly, on its own network.
pub const DIRECT_PORT: u16 = 7073;

/// Viewer side: straight to the computer at one of `direct` (addresses it was last found at on
/// this network), without the relay; None when none answers in a moment as that ID.
fn connect_direct(addr: &Address, password: &str, direct: &[String]) -> Result<Option<(SealedReader, SealedWriter)>, String> {
    use std::net::ToSocketAddrs;
    for d in direct {
        let Some(to) = d.to_socket_addrs().ok().and_then(|mut a| a.next()) else { continue };
        let Ok(mut s) = TcpStream::connect_timeout(&to, Duration::from_millis(700)) else { continue };
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        if writeln!(s, "direct {}", addr.id).is_err() || !crate::access::line(&mut s).is_ok_and(|l| l == "ok") {
            continue;
        }
        let _ = s.set_read_timeout(Some(VERDICT_LIMIT + Duration::from_secs(15)));
        // A wrong password is as wrong here as through the relay: no second try there.
        let pair = match handshake(s, password, true, &now) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(e.to_string()),
            Err(_) => continue,
        };
        if let Ok(t) = pair.1.stream() {
            let _ = t.set_read_timeout(Some(Duration::from_secs(120)));
        }
        return Ok(Some(pair));
    }
    Ok(None)
}

/// The computer's side of a direct connection: the viewer names the ID it wants; anything else
/// is turned away before a password is looked at.
pub fn accept_direct(s: &mut TcpStream, id: &str) -> bool {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let asked = crate::access::line(s).ok();
    let ok = asked.as_deref().and_then(|l| l.strip_prefix("direct ")) == Some(id);
    ok && writeln!(s, "ok").is_ok()
}

/// Viewer side: reaches the ID directly when it is on this network at an address it was found
/// at, else through its relay, and runs the handshake.
pub fn connect_via(addr: &Address, password: &str, direct: &[String]) -> Result<(SealedReader, SealedWriter), String> {
    if let Some(pair) = connect_direct(addr, password, direct)? {
        return Ok(pair);
    }
    connect(addr, password)
}

/// Viewer side: reaches the ID through its relay and runs the handshake.
pub fn connect(addr: &Address, password: &str) -> Result<(SealedReader, SealedWriter), String> {
    // Dialled as remote access does: port 80 when the relay's own port is shut.
    let mut s = crate::access::dial(&addr.relay).map_err(|e| e.to_string())?;
    // A relay that never answers does not hold the viewer.
    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
    writeln!(s, "viewer {}", addr.id).map_err(|e| e.to_string())?;
    match crate::access::line(&mut s).map_err(|e| e.to_string())?.as_str() {
        "ok" => {}
        other => return Err(format!("relay: {other}")),
    }
    // The agent may make a wrong guess wait its turn and a delay, but not forever.
    let _ = s.set_read_timeout(Some(VERDICT_LIMIT + Duration::from_secs(15)));
    let pair = handshake(s, password, true, &now).map_err(|e| e.to_string())?;
    // The agent answers a ping every second: two minutes of nothing is a dead path (a network
    // change, a NAT mapping dropped), and the viewer reconnects instead of freezing.
    if let Ok(t) = pair.1.stream() {
        let _ = t.set_read_timeout(Some(Duration::from_secs(120)));
    }
    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// A write larger than a frame goes out in several, and arrives whole.
    #[test]
    fn a_large_write_arrives_whole() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let agent = thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let (mut r, _w) = handshake_agent(s, "secret password", &now).unwrap();
            let mut got = vec![0u8; 3 << 19];
            r.read_exact(&mut got).map(|()| got)
        });
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_r, mut w) = handshake(s, "secret password", true, &now).unwrap();
        let sent: Vec<u8> = (0..3usize << 19).map(|i| (i % 251) as u8).collect();
        w.write_all(&sent).unwrap();
        w.flush().unwrap();
        assert_eq!(agent.join().unwrap().unwrap(), sent);
    }

    /// Before the password is proven only exact sizes are taken: a peer declaring a longer key
    /// exchange is refused at once instead of being waited for.
    #[test]
    fn a_wrong_size_key_exchange_is_refused() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let agent = thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            handshake_agent(s, "secret password", &now).map(|_| ())
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(&256u16.to_le_bytes()).unwrap();
        s.write_all(&[0u8; 8]).unwrap();
        let started = std::time::Instant::now();
        assert!(agent.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(2), "refused, not waited for");
    }

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
            let (_r, mut w) = handshake(s, pw_agent, false, &now)?;
            w.write_all(b"hello over the sealed stream")?;
            w.flush()
        });
        let viewer = (|| {
            let s = TcpStream::connect(("127.0.0.1", port))?;
            let (mut r, _w) = handshake(s, pw_viewer, true, &now)?;
            let mut buf = [0u8; 28];
            r.read_exact(&mut buf)?;
            Ok(String::from_utf8_lossy(&buf).into_owned())
        })();
        (viewer, agent.join().unwrap())
    }

    /// A viewer that stops halfway through the exchange holds nothing: only checking a proof
    /// takes the agent's turn, so the next viewer gets in at once.
    #[test]
    fn a_stalled_viewer_does_not_hold_the_turn() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let turn = Arc::new(std::sync::Mutex::new(()));
        let agent = {
            let turn = turn.clone();
            thread::spawn(move || {
                let mut done = Vec::new();
                for _ in 0..2 {
                    let (s, _) = l.accept().unwrap();
                    let turn = turn.clone();
                    done.push(thread::spawn(move || {
                        let judge = |check: &mut dyn FnMut() -> bool| -> io::Result<bool> {
                            let _t = turn.lock().unwrap();
                            Ok(check())
                        };
                        handshake_agent(s, "123456789", &judge).map(|_| ())
                    }));
                }
                done
            })
        };
        // The first sends a key exchange message's length and nothing more.
        let mut stall = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stall.write_all(&[32, 0]).unwrap();
        let started = std::time::Instant::now();
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert!(handshake(s, "123456789", true, &now).is_ok());
        assert!(started.elapsed() < Duration::from_secs(3), "in at once: {:?}", started.elapsed());
        drop(stall);
        for h in agent.join().unwrap() {
            let _ = h.join();
        }
    }

    #[test]
    fn matching_passwords_open_a_sealed_stream() {
        let (v, a) = pair("123456789", "123456789");
        assert_eq!(v.unwrap(), "hello over the sealed stream");
        assert!(a.is_ok());
    }

    /// Straight to a computer on this network: in with its ID and password; a wrong password
    /// is an answer (no second try through the relay); another ID is no answer at all.
    #[test]
    fn a_computer_on_this_network_is_reached_straight() {
        let serve = |pw: &'static str| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let at = l.local_addr().unwrap().to_string();
            thread::spawn(move || {
                let (mut s, _) = l.accept().unwrap();
                if accept_direct(&mut s, "482913075") {
                    let _ = handshake_agent(s, pw, &now);
                }
            });
            at
        };
        let addr = Address { id: "482913075".into(), relay: "relay.example".into() };
        let at = serve("right one");
        assert!(matches!(connect_direct(&addr, "right one", &[at]), Ok(Some(_))));
        let at = serve("right one");
        assert!(connect_direct(&addr, "wrong one", &[at]).is_err());
        let other = Address { id: "111222333".into(), relay: "relay.example".into() };
        let at = serve("right one");
        assert!(matches!(connect_direct(&other, "right one", &[at]), Ok(None)));
        // Nobody there: no answer, quickly.
        assert!(matches!(connect_direct(&addr, "right one", &["127.0.0.1:1".into()]), Ok(None)));
    }

    #[test]
    fn a_wrong_password_is_refused() {
        let (v, a) = pair("123456789", "987654321");
        // The viewer hears only that the connection closed: it says why in words that lead to
        // asking the password again.
        let v = v.unwrap_err();
        assert_eq!((v.kind(), v.to_string().as_str()), (io::ErrorKind::PermissionDenied, "wrong password"));
        // The agent sees the wrong proof itself, so the guess counts against the next one.
        assert_eq!(a.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}
