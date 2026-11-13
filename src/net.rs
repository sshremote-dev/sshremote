//! Connections without ssh, through a relay by remote access ID. Both ends prove they know
//! the same password with SPAKE2 (an eavesdropper or the relay learns nothing it could guess
//! offline), then every byte is sealed with ChaCha20-Poly1305 under keys derived from that
//! exchange. The relay's lines are described in `access`.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
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

/// A frame whose length has this bit is the two sides' own (finding a straight path, moving to
/// it), not the stream's: sent only to a side that said it knows them (an empty frame, which
/// older versions pass over).
const CONTROL: u32 = 1 << 31;
const MOST_CONTROL: usize = 4096;
/// How long a reader told the other side moved waits for this side's end of the path.
const MOVE_WAIT: Duration = Duration::from_secs(15);

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

/// What sealed frames travel on: the connection they started on (the relay's, or one straight
/// on this network), and from a "move" on, a straight path found through the routers.
enum Wire {
    Tcp(TcpStream),
    Udp(crate::udp::Stream),
}

impl Read for Wire {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Tcp(s) => s.read(b),
            Wire::Udp(s) => s.read(b),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Wire::Tcp(s) => s.write(b),
            Wire::Udp(s) => s.write(b),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// This side looks for a straight path: as the viewer or the agent, asking `relay` (`host` or
/// `host:port`, UDP on the relay's port) where its router shows it.
#[derive(Clone, Debug)]
pub struct Punch {
    pub viewer: bool,
    pub relay: String,
    /// The viewer's: the ID it called, remembered if its straight path is lost.
    pub id: String,
}

/// IDs whose straight path was lost, and when: the relay only for a while (a network that cuts
/// UDP after a time would otherwise end every session that moved).
static LOST: Mutex<Vec<(String, std::time::Instant)>> = Mutex::new(Vec::new());
const LOST_FOR: Duration = Duration::from_secs(600);

fn lost_lately(id: &str) -> bool {
    let mut l = guard(&LOST);
    l.retain(|(_, t)| t.elapsed() < LOST_FOR);
    l.iter().any(|(i, _)| i == id)
}

struct Out {
    seal: Sealer,
    wire: Wire,
}

impl Out {
    fn frame(&mut self, plain: &[u8], control: bool) -> io::Result<()> {
        let nonce = self.seal.nonce();
        let ct = self.seal.cipher.encrypt(&nonce, plain).map_err(|_| io::Error::other("seal"))?;
        let mut f = Vec::with_capacity(4 + ct.len());
        f.extend_from_slice(&(ct.len() as u32 | if control { CONTROL } else { 0 }).to_le_bytes());
        f.extend_from_slice(&ct);
        self.wire.write_all(&f)
    }
}

const MOVED_OUT: u8 = 1;
const MOVED_IN: u8 = 2;

/// What the two halves of a sealed stream share: the sending side (control frames go out
/// between the stream's), the first connection, and the straight path once there is one.
struct Line {
    out: Mutex<Out>,
    tcp: TcpStream,
    udp: Mutex<Option<crate::udp::Stream>>,
    udp_ready: Condvar,
    straight: Mutex<Option<SocketAddr>>,
    moved: AtomicU8,
    punch: Option<Punch>,
    /// The viewer's wait for the agent's addresses.
    answer: Mutex<Option<mpsc::Sender<Vec<SocketAddr>>>>,
    /// Attempts at a straight path so far.
    tried: AtomicU8,
    ended: AtomicBool,
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How a side starts probing, given which the offer named first.
fn start(first: &str, viewer: bool) -> crate::udp::Start {
    use crate::udp::Start;
    match first {
        "both" => Start::Low,
        f if (f == "viewer") == viewer => Start::Now,
        _ => Start::Second,
    }
}

fn addresses(list: &[SocketAddr]) -> String {
    list.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ")
}

impl Line {
    fn frame(&self, plain: &[u8], control: bool) -> io::Result<()> {
        guard(&self.out).frame(plain, control)
    }

    fn moved(&self, bit: u8) {
        // Both ways on the straight path: the relay's connection is no longer needed.
        if self.moved.fetch_or(bit, Ordering::AcqRel) | bit == MOVED_OUT | MOVED_IN {
            let _ = self.tcp.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Sends from here on over `path`: a "move" on the old wire marks where.
    fn go_straight(&self, path: crate::udp::Stream, to: SocketAddr) {
        if self.ended.load(Ordering::Acquire) {
            path.end();
            return;
        }
        *guard(&self.udp) = Some(path.clone());
        self.udp_ready.notify_all();
        *guard(&self.straight) = Some(to);
        {
            let mut out = guard(&self.out);
            if out.frame(b"move", true).is_err() {
                return;
            }
            out.wire = Wire::Udp(path);
        }
        self.moved(MOVED_OUT);
    }

    fn wait_udp(&self) -> Option<crate::udp::Stream> {
        let deadline = std::time::Instant::now() + MOVE_WAIT;
        let mut u = guard(&self.udp);
        loop {
            if let Some(s) = u.as_ref() {
                return Some(s.clone());
            }
            let left = deadline.checked_duration_since(std::time::Instant::now())?;
            u = self.udp_ready.wait_timeout(u, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The agent said it can look for a straight path: the viewer starts, if it looks for one.
    /// Three times at most, each on new ports (a router that turned the first probes away may
    /// keep that way shut): the viewer probes first, then the agent, then both, each first
    /// opening only its own router.
    fn agent_can(self: &Arc<Self>) {
        let Some(p) = self.punch.clone().filter(|p| p.viewer) else { return };
        if self.tried.fetch_add(1, Ordering::AcqRel) > 0 {
            return;
        }
        let line = self.clone();
        std::thread::spawn(move || {
            for first in ["viewer", "agent", "both"] {
                if line.ended.load(Ordering::Acquire) {
                    return;
                }
                let (tx, rx) = mpsc::channel();
                *guard(&line.answer) = Some(tx);
                let Ok(mut path) = crate::udp::Path::bind(true) else { return };
                let mine = path.candidates(&p.relay);
                let tag: [u8; 8] = crate::sys::random(8).try_into().unwrap_or([0; 8]);
                if line.frame(format!("offer {} {first} {}", crate::sys::hex(&tag), addresses(&mine)).as_bytes(), true).is_err() {
                    return;
                }
                let Ok(peers) = rx.recv_timeout(MOVE_WAIT) else { return };
                path.punch(tag, &peers, start(first, true));
                if let Some(to) = path.wait(crate::udp::PUNCH_LIMIT + Duration::from_secs(1)) {
                    line.go_straight(path.stream(), to);
                    return;
                }
            }
        });
    }

    /// The viewer offered its addresses: the agent answers with its own and tries them.
    fn offered(self: &Arc<Self>, tag: [u8; 8], first: &str, peers: Vec<SocketAddr>) {
        let Some(p) = self.punch.clone().filter(|p| !p.viewer) else { return };
        if self.tried.fetch_add(1, Ordering::AcqRel) >= 3 || self.moved.load(Ordering::Acquire) != 0 {
            return;
        }
        let line = self.clone();
        let start = start(first, false);
        std::thread::spawn(move || {
            let Ok(mut path) = crate::udp::Path::bind(false) else { return };
            let mine = path.candidates(&p.relay);
            if line.frame(format!("answer {}", addresses(&mine)).as_bytes(), true).is_err() {
                return;
            }
            path.punch(tag, &peers, start);
            if let Some(to) = path.wait(crate::udp::PUNCH_LIMIT + Duration::from_secs(1)) {
                line.go_straight(path.stream(), to);
            }
        });
    }
}

/// Ends a sealed stream from anywhere (the reader and writer being busy elsewhere), whichever
/// way it runs; and says which way that is.
#[derive(Clone)]
pub struct Ender(Arc<Line>);

impl Ender {
    pub fn end(&self) {
        self.0.ended.store(true, Ordering::Release);
        let _ = self.0.tcp.shutdown(std::net::Shutdown::Both);
        if let Some(u) = guard(&self.0.udp).as_ref() {
            u.end();
        }
    }

    /// How long the first connection may be silent before a read on it fails.
    pub fn set_idle_limit(&self, d: Duration) {
        let _ = self.0.tcp.set_read_timeout(Some(d));
    }

    /// Where the straight path goes, once the stream runs on one.
    pub fn straight(&self) -> Option<SocketAddr> {
        *guard(&self.0.straight)
    }
}

pub struct SealedWriter {
    line: Arc<Line>,
    buf: Vec<u8>,
}

impl SealedWriter {
    fn emit(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let r = self.line.frame(&self.buf, false);
        self.buf.clear();
        r
    }

    /// Ends the stream from elsewhere, and tells which way it runs.
    pub fn ender(&self) -> Ender {
        Ender(self.line.clone())
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
        self.emit()
    }
}

pub struct SealedReader {
    line: Arc<Line>,
    wire: Wire,
    seal: Sealer,
    plain: Vec<u8>,
    at: usize,
    /// The largest frame taken: the confirmation's size until the password is proven.
    limit: usize,
}

impl SealedReader {
    /// The next frame as it came, still sealed, and whether it is a control frame; None at the
    /// end of the stream.
    fn sealed_frame(&mut self) -> io::Result<Option<(bool, Vec<u8>)>> {
        let mut len = [0u8; 4];
        match self.wire.read_exact(&mut len) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            r => r?,
        }
        let raw = u32::from_le_bytes(len);
        let control = raw & CONTROL != 0;
        let len = (raw & !CONTROL) as usize;
        if len > self.limit + 16 || (control && len > MOST_CONTROL + 16) {
            return Err(io::Error::other("oversized frame"));
        }
        let mut ct = vec![0u8; len];
        self.wire.read_exact(&mut ct)?;
        Ok(Some((control, ct)))
    }

    /// The next frame of the stream itself, still sealed (the password check reads it so).
    fn stream_frame(&mut self) -> io::Result<Vec<u8>> {
        match self.sealed_frame()? {
            Some((false, ct)) => Ok(ct),
            Some((true, _)) => Err(io::Error::other("bad key exchange")),
            None => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }

    /// Opens a frame `sealed_frame` read, the next in order.
    fn open(&mut self, ct: &[u8]) -> io::Result<Vec<u8>> {
        let nonce = self.seal.nonce();
        self.seal.cipher.decrypt(&nonce, ct).map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "wrong password or tampered stream"))
    }

    /// The other side's word about the straight path.
    fn control(&mut self, msg: &[u8]) -> io::Result<()> {
        let text = String::from_utf8_lossy(msg);
        let mut words = text.split_whitespace();
        let peers = |w: std::str::SplitWhitespace| w.filter_map(|a| a.parse::<SocketAddr>().ok()).take(20).collect::<Vec<_>>();
        match words.next() {
            // From here on its frames come on the straight path.
            Some("move") => {
                let path = self.line.wait_udp().ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "the straight path to the other computer did not come up"))?;
                self.wire = Wire::Udp(path);
                self.line.moved(MOVED_IN);
            }
            // offer TAG FIRST ADDRESS...: FIRST names the side that probes first.
            Some("offer") => {
                let tag = words.next().filter(|t| t.len() == 16 && t.bytes().all(|b| b.is_ascii_hexdigit())).and_then(|t| (0..8).map(|i| u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).ok()).collect::<Option<Vec<u8>>>());
                let first = words.next().unwrap_or("viewer").to_string();
                if let Some(tag) = tag.and_then(|t| <[u8; 8]>::try_from(t).ok()) {
                    self.line.offered(tag, &first, peers(words));
                }
            }
            Some("answer") => {
                if let Some(tx) = guard(&self.line.answer).take() {
                    let _ = tx.send(peers(words));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Read for SealedReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            let frame = match self.sealed_frame() {
                Err(e) if matches!(self.wire, Wire::Udp(_)) && e.kind() == io::ErrorKind::TimedOut => {
                    if let Some(p) = self.line.punch.as_ref().filter(|p| p.viewer) {
                        guard(&LOST).push((p.id.clone(), std::time::Instant::now()));
                    }
                    return Err(e);
                }
                r => r?,
            };
            let Some((control, ct)) = frame else { return Ok(0) };
            let plain = self.open(&ct)?;
            if control {
                self.control(&plain)?;
                continue;
            }
            // Empty: the agent can look for a straight path (an older viewer passes over it).
            if plain.is_empty() {
                self.line.agent_can();
                continue;
            }
            self.plain = plain;
            self.at = 0;
        }
        let n = out.len().min(self.plain.len() - self.at);
        out[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

const CONFIRM: &[u8] = b"sshremote/1 ready";
/// Said instead of CONFIRM by a computer that also wants the code of an authenticator app.
const CODE_PLEASE: &[u8] = b"sshremote/1 code?";
/// Joins the two-step code to the password a viewer is given: "password", SEP, "123456".
pub const CODE_SEP: char = '\u{1f}';
/// What a viewer is told when the computer wants a code it was not given.
pub const CODE_WANTED: &str = "the computer asks for the code of its authenticator app, with the password";

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
    handshake_2fa(stream, password, viewer, judge, None, None)
}

/// `punch`: this side looks for a straight path once in (the agent says so to the viewer).
fn handshake_2fa(stream: TcpStream, password: &str, viewer: bool, judge: Judge, totp: Option<&str>, punch: Option<Punch>) -> io::Result<(SealedReader, SealedWriter)> {
    let side = if viewer { Side::Viewer(password) } else { Side::Agent { permanent: Some(password), one_time: None } };
    handshake_side(stream, side, judge, totp, punch).map(|(r, w, _)| (r, w))
}

/// Which side of the exchange, with what it knows: the viewer the password it was given, the
/// agent the passwords it takes.
enum Side<'a> {
    Viewer(&'a str),
    Agent { permanent: Option<&'a str>, one_time: Option<&'a str> },
}

fn handshake_side(stream: TcpStream, side: Side, judge: Judge, totp: Option<&str>, punch: Option<Punch>) -> io::Result<(SealedReader, SealedWriter, bool)> {
    let closer = stream.try_clone()?;
    let result = handshake_on(stream, side, judge, totp, punch);
    if result.is_err() {
        let _ = closer.shutdown(std::net::Shutdown::Both);
    }
    result
}

/// The bit of the key exchange's length that says the viewer gives a one-time password.
const ONE_TIME: u16 = 0x8000;
/// What one-time passwords are made of: no letters or digits that read alike.
const ONE_TIME_SET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// A one-time password, as remote access makes them: two groups of four ("k7qp-2mz9"), in any case.
pub fn looks_one_time(p: &str) -> bool {
    crate::vault::one_time_shape(p)
}

/// A new one-time password.
pub fn new_one_time() -> String {
    let r = crate::sys::random(8);
    let c: String = r.iter().map(|x| ONE_TIME_SET[*x as usize % ONE_TIME_SET.len()] as char).collect();
    format!("{}-{}", &c[..4], &c[4..])
}

fn handshake_on(stream: TcpStream, side: Side, judge: Judge, totp: Option<&str>, punch: Option<Punch>) -> io::Result<(SealedReader, SealedWriter, bool)> {
    let viewer = matches!(side, Side::Viewer(_));
    // A viewer may be given the two-step code with the password.
    let (password, code) = match side {
        Side::Viewer(p) => match p.split_once(CODE_SEP) {
            Some((p, c)) => (p, Some(c.trim())),
            None => (p, None),
        },
        Side::Agent { .. } => ("", None),
    };
    let (a, b) = (Identity::new(b"sshremote viewer"), Identity::new(b"sshremote agent"));
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
    // The viewer speaks first, saying whether it gives a one-time password; the agent answers
    // with the password that says (older agents speak at once: the viewer's turn comes first
    // either way).
    let (state, msg, theirs, one_time_used) = match side {
        Side::Viewer(_) => {
            let once = looks_one_time(password);
            let given = if once { password.trim().to_ascii_lowercase() } else { password.trim().to_string() };
            let (state, msg) = Spake2::<Ed25519Group>::start_a(&Password::new(given.as_bytes()), &a, &b);
            let flag = if once { ONE_TIME } else { 0 };
            s.write_all(&(msg.len() as u16 | flag).to_le_bytes())?;
            s.write_all(&msg)?;
            let mut len = [0u8; 2];
            if let Err(e) = s.read_exact(&mut len) {
                // A computer that will not check a password now hangs up before the exchange.
                return Err(if e.kind() == io::ErrorKind::UnexpectedEof {
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
            (state, msg, theirs, once)
        }
        Side::Agent { permanent, one_time } => {
            let mut len = [0u8; 2];
            s.read_exact(&mut len)?;
            let len = u16::from_le_bytes(len);
            let once = len & ONE_TIME != 0;
            // A password this computer does not take (one-time passwords off, or only they on)
            // is one nobody knows: the guess is checked, and counted, as any wrong one.
            let unknown = crate::sys::hex(&crate::sys::random(24));
            let pw = if once { one_time.or(permanent) } else { permanent }.unwrap_or(&unknown);
            let (state, msg) = Spake2::<Ed25519Group>::start_b(&Password::new(pw.trim().as_bytes()), &a, &b);
            if (len & !ONE_TIME) as usize != msg.len() {
                return Err(io::Error::other("bad key exchange"));
            }
            let mut theirs = vec![0u8; msg.len()];
            s.read_exact(&mut theirs)?;
            s.write_all(&(msg.len() as u16).to_le_bytes())?;
            s.write_all(&msg)?;
            (state, msg, theirs, once && one_time.is_some())
        }
    };
    let _ = &msg;
    let shared = state.finish(&theirs).map_err(|_| io::Error::other("bad key exchange"))?;
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, &shared);
    let (mut v2a, mut a2v) = ([0u8; 32], [0u8; 32]);
    hk.expand(b"viewer to agent", &mut v2a).map_err(|_| io::Error::other("hkdf"))?;
    hk.expand(b"agent to viewer", &mut a2v).map_err(|_| io::Error::other("hkdf"))?;
    let (send, recv) = if viewer { (v2a, a2v) } else { (a2v, v2a) };
    let line = Arc::new(Line {
        out: Mutex::new(Out { seal: Sealer::new(&send), wire: Wire::Tcp(s.try_clone()?) }),
        tcp: s.try_clone()?,
        udp: Mutex::new(None),
        udp_ready: Condvar::new(),
        straight: Mutex::new(None),
        moved: AtomicU8::new(0),
        punch,
        answer: Mutex::new(None),
        tried: AtomicU8::new(0),
        ended: AtomicBool::new(false),
    });
    let mut w = SealedWriter { line: line.clone(), buf: Vec::new() };
    let mut r = SealedReader { line, wire: Wire::Tcp(s), seal: Sealer::new(&recv), plain: Vec::new(), at: 0, limit: CONFIRM.len() };
    // The viewer proves the password first: the agent answers only a right one, so a guess
    // cannot be checked against the agent's answer and dropped before it counts as wrong.
    let mut got = [0u8; CONFIRM.len()];
    if viewer {
        w.write_all(CONFIRM)?;
        w.flush()?;
        extend(VERDICT_LIMIT);
        // A computer that did not take the password closes the connection without a word.
        let refused = |e: io::Error, what: &str| match e.kind() {
            _ if late() => io::Error::new(io::ErrorKind::TimedOut, "the computer did not answer the password in time"),
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => io::Error::new(io::ErrorKind::PermissionDenied, what.to_string()),
            _ => e,
        };
        r.read_exact(&mut got).map_err(|e| refused(e, "wrong password"))?;
        // The password was right; the computer wants the code too.
        if got == CODE_PLEASE {
            let Some(code) = code.filter(|c| !c.is_empty()) else {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, CODE_WANTED));
            };
            w.write_all(code.as_bytes())?;
            w.flush()?;
            extend(VERDICT_LIMIT);
            r.read_exact(&mut got).map_err(|e| refused(e, "wrong code (the password was right)"))?;
        }
    } else {
        // The proof is read as it comes; checking it waits on the judge, with no deadline
        // meanwhile (the network's part is over).
        let ct = r.stream_frame()?;
        deadline.store(u64::MAX, Ordering::Release);
        let right = judge(&mut || r.open(&ct).is_ok_and(|p| p == CONFIRM))?;
        extend(HANDSHAKE_LIMIT);
        if !right {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
        }
        // Two-step: the code is checked in turn too, and a wrong one counts as a wrong password.
        if let Some(secret) = totp {
            w.write_all(CODE_PLEASE)?;
            w.flush()?;
            let ct = r.stream_frame()?;
            deadline.store(u64::MAX, Ordering::Release);
            let right = judge(&mut || r.open(&ct).is_ok_and(|c| crate::totp::check(secret, &String::from_utf8_lossy(&c))))?;
            extend(HANDSHAKE_LIMIT);
            if !right {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong code"));
            }
        }
        got.copy_from_slice(CONFIRM);
        w.write_all(CONFIRM)?;
        w.flush()?;
        // An empty frame: this side can look for a straight path, if the viewer can.
        if w.line.punch.is_some() {
            w.line.frame(&[], false)?;
        }
    }
    if got != CONFIRM {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
    }
    r.limit = MAX_FRAME;
    Ok((r, w, one_time_used))
}

/// The agent's side of the handshake, for remote access by ID; `judge` checks the viewer's proof
/// of the password when it allows.
#[cfg(test)]
pub fn handshake_agent(stream: TcpStream, password: &str, judge: Judge) -> io::Result<(SealedReader, SealedWriter)> {
    handshake(stream, password, false, judge)
}

/// `handshake_agent` that also wants the code of the authenticator app set up with `totp`.
/// `punch`: it also looks for a straight path with a viewer that does.
#[cfg(test)]
pub fn handshake_agent_2fa(stream: TcpStream, password: &str, totp: Option<&str>, judge: Judge, punch: Option<Punch>) -> io::Result<(SealedReader, SealedWriter)> {
    handshake_2fa(stream, password, false, judge, totp, punch)
}

/// The agent's side with the passwords it takes: the lasting one, a one-time one, or both (None:
/// that kind is not taken). Also says whether the viewer came in with the one-time password.
pub fn handshake_agent_passwords(
    stream: TcpStream,
    permanent: Option<&str>,
    one_time: Option<&str>,
    totp: Option<&str>,
    judge: Judge,
    punch: Option<Punch>,
) -> io::Result<(SealedReader, SealedWriter, bool)> {
    handshake_side(stream, Side::Agent { permanent, one_time }, judge, totp, punch)
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
        pair.1.ender().set_idle_limit(Duration::from_secs(120));
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
    // Once in, a straight path is looked for, unless the person chose the relay always, or the
    // relay is on this computer (the web page's gateway beside it gains nothing from one).
    let local = s.peer_addr().is_ok_and(|a| a.ip().is_loopback());
    let punch = (straight_paths() && !local && !lost_lately(&addr.id)).then(|| Punch { viewer: true, relay: addr.relay.clone(), id: addr.id.clone() });
    let pair = handshake_2fa(s, password, true, &now, None, punch).map_err(|e| e.to_string())?;
    // The agent answers a ping every second: two minutes of nothing is a dead path (a network
    // change, a NAT mapping dropped), and the viewer reconnects instead of freezing.
    pair.1.ender().set_idle_limit(Duration::from_secs(120));
    Ok(pair)
}

/// Whether this viewer looks for straight paths through the routers (on unless turned off).
pub fn straight_paths() -> bool {
    crate::hosts::setting("straight_paths").is_none_or(|v| v != "no")
}

/// Tests: a viewer's and an agent's sealed stream over one connection on this computer; with
/// `punch`, both look for a straight path, asking a relay here where they are seen from.
#[cfg(test)]
pub fn sealed_pair(punch: bool) -> ((SealedReader, SealedWriter), (SealedReader, SealedWriter)) {
    let relay = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let at = relay.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let mut b = [0u8; 256];
        while let Ok((n, from)) = relay.recv_from(&mut b) {
            if let Some(a) = crate::udp::reflect_answer(&b[..n], from) {
                let _ = relay.send_to(&a, from);
            }
        }
    });
    let p = |viewer| punch.then(|| Punch { viewer, relay: at.clone(), id: String::new() });
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let agent_punch = p(false);
    let agent = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        handshake_agent_2fa(s, "pair password", None, &now, agent_punch).unwrap()
    });
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let viewer = handshake_2fa(s, "pair password", true, &now, None, p(true)).unwrap();
    (viewer, agent.join().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Writes a numbered pattern, a piece at a time, until told to stop; then says how much.
    fn writer(mut w: SealedWriter, stop: Arc<AtomicBool>, salt: u8) -> thread::JoinHandle<usize> {
        thread::spawn(move || {
            let mut sent = 0usize;
            while !stop.load(Ordering::Acquire) {
                let piece: Vec<u8> = (sent..sent + 3001).map(|i| (i % 253) as u8 ^ salt).collect();
                if w.write_all(&piece).and_then(|_| w.flush()).is_err() {
                    break;
                }
                sent += piece.len();
                thread::sleep(Duration::from_millis(2));
            }
            sent
        })
    }

    /// Reads until the end, checking the pattern; says how much came.
    fn reader(mut r: SealedReader, salt: u8) -> thread::JoinHandle<io::Result<usize>> {
        thread::spawn(move || {
            let mut got = 0usize;
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    return Ok(got);
                }
                for (i, b) in buf[..n].iter().enumerate() {
                    assert_eq!(*b, ((got + i) % 253) as u8 ^ salt, "byte {} out of order", got + i);
                }
                got += n;
            }
        })
    }

    /// Two sides that both look for a straight path move to it while the stream runs both
    /// ways: nothing lost or out of order across the move, the first connection let go, and
    /// the end still heard.
    #[test]
    fn a_relayed_stream_moves_to_a_straight_path() {
        let ((vr, vw), (ar, aw)) = sealed_pair(true);
        let (ve, ae) = (vw.ender(), aw.ender());
        let tcp = vw.line.tcp.try_clone().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (w1, w2) = (writer(vw, stop.clone(), 1), writer(aw, stop.clone(), 2));
        let (r1, r2) = (reader(ar, 1), reader(vr, 2));
        let started = std::time::Instant::now();
        while (ve.straight().is_none() || ae.straight().is_none()) && started.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(ve.straight().is_some() && ae.straight().is_some(), "both on the straight path");
        thread::sleep(Duration::from_millis(500));
        // The first connection is shut once both moved.
        let mut b = [0u8; 1];
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(2)));
        assert!(matches!((&tcp).read(&mut b), Ok(0) | Err(_)), "the first connection let go");
        stop.store(true, Ordering::Release);
        let (sent1, sent2) = (w1.join().unwrap(), w2.join().unwrap());
        thread::sleep(Duration::from_millis(300));
        ve.end();
        assert_eq!(r1.join().unwrap().unwrap(), sent1);
        assert_eq!(r2.join().unwrap().unwrap(), sent2);
    }

    /// A side that does not look for one (an older version, or turned off) keeps the stream
    /// where it is, and nothing extra reaches it.
    #[test]
    fn without_both_sides_looking_the_stream_stays() {
        let ((vr, vw), (ar, aw)) = sealed_pair(false);
        let (ve, ae) = (vw.ender(), aw.ender());
        let stop = Arc::new(AtomicBool::new(false));
        let (w1, w2) = (writer(vw, stop.clone(), 3), writer(aw, stop.clone(), 4));
        let (r1, r2) = (reader(ar, 3), reader(vr, 4));
        thread::sleep(Duration::from_secs(2));
        assert!(ve.straight().is_none() && ae.straight().is_none());
        stop.store(true, Ordering::Release);
        let (sent1, sent2) = (w1.join().unwrap(), w2.join().unwrap());
        thread::sleep(Duration::from_millis(200));
        // Each side's end reaches the other (on Windows a socket shut here does not wake a read
        // waiting on it here).
        ae.end();
        assert_eq!(r2.join().unwrap().unwrap(), sent2);
        ve.end();
        assert_eq!(r1.join().unwrap().unwrap(), sent1);
    }

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

    /// A computer that wants a code: refused without it, or with a wrong one, though the
    /// password is right; in with both.
    #[test]
    fn two_step_wants_the_code_too() {
        let secret = crate::totp::new_secret();
        let serve = |secret: String| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let at = l.local_addr().unwrap();
            thread::spawn(move || {
                let (s, _) = l.accept().unwrap();
                let _ = handshake_agent_2fa(s, "pw for both", Some(&secret), &now, None);
            });
            at
        };
        let viewer = |pw: String, at| handshake(TcpStream::connect(at).unwrap(), &pw, true, &now).map(|_| ()).map_err(|e| e.to_string());
        assert_eq!(viewer("pw for both".into(), serve(secret.clone())), Err(CODE_WANTED.to_string()));
        assert!(viewer(format!("pw for both{CODE_SEP}000000"), serve(secret.clone())).is_err_and(|e| e.contains("wrong code")) || crate::totp::check(&secret, "000000"));
        let code = crate::totp::now(&secret);
        assert!(viewer(format!("pw for both{CODE_SEP}{code}"), serve(secret.clone())).is_ok());
        assert!(viewer(format!("wrong pw{CODE_SEP}{code}"), serve(secret)).is_err_and(|e| e.contains("wrong password")));
    }

    /// The agent takes the lasting password, the one-time one, or both, and says which came in;
    /// the viewer tells a one-time password by its form, in any case.
    #[test]
    fn one_time_passwords_get_in_only_where_taken() {
        let try_with = |lasting: Option<&'static str>, once: Option<&'static str>, given: &'static str| -> Result<bool, String> {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let agent = thread::spawn(move || {
                let (s, _) = l.accept().unwrap();
                handshake_agent_passwords(s, lasting, once, None, &now, None).map(|(_, _, used)| used)
            });
            let viewer = handshake(TcpStream::connect(("127.0.0.1", port)).unwrap(), given, true, &now).map(|_| ());
            let used = agent.join().unwrap();
            viewer.map_err(|e| e.to_string())?;
            used.map_err(|e| e.to_string())
        };
        let (lasting, once) = (Some("a lasting one"), Some("k7qp-2mz9"));
        assert_eq!(try_with(lasting, once, "a lasting one"), Ok(false));
        assert_eq!(try_with(lasting, once, "k7qp-2mz9"), Ok(true));
        assert_eq!(try_with(lasting, once, " K7QP-2MZ9 "), Ok(true), "any case, spaces around");
        assert!(try_with(lasting, None, "k7qp-2mz9").is_err(), "one-time passwords off");
        assert!(try_with(None, once, "a lasting one").is_err(), "one-time passwords alone");
        assert!(try_with(lasting, once, "k7qp-2mz8").is_err(), "a wrong one-time password");
        assert!(looks_one_time(&new_one_time()) && !looks_one_time("abcdefgh") && !looks_one_time("k7qp-2mz0"));
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
