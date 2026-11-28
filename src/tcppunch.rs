//! A straight TCP path through the routers, beside the UDP one (`udp`): each side asks its relay
//! where a port of its own is seen from (`seen`, that line held open so its router keeps the way
//! it made for it), both connect to each other's places from that same port at once (a
//! simultaneous open, which most home routers let through as the answer to their own), and the
//! first connection on which both show they know the offer's key carries the stream from then on.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

/// How long the two sides keep trying.
pub const LIMIT: Duration = Duration::from_secs(8);

/// What each side says first on a connection made this way.
const HELLO: &[u8; 4] = b"SRT1";

/// One side's port for the path: where it is seen from and its own addresses with it.
pub struct Punch {
    local: SocketAddr,
    /// The line to the relay that saw it, held open while the path is tried.
    _line: TcpStream,
    pub mine: Vec<SocketAddr>,
}

/// A TCP socket on `at` that other sockets may share (to connect from the same port).
fn bound(at: SocketAddr) -> io::Result<Socket> {
    let s = Socket::new(Domain::for_address(at), Type::STREAM, Some(Protocol::TCP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.bind(&at.into())?;
    Ok(s)
}

impl Punch {
    /// A port of its own, as `relay` sees it (IPv4); None where the relay cannot say (an older
    /// one, or reached through a web server) and nothing on this network is worth trying either.
    pub fn open(relay: &str) -> Option<Punch> {
        let (host, port) = crate::access::relay_addr(relay);
        let server = (host.as_str(), port).to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)?;
        let s = bound(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)).ok()?;
        let local = s.local_addr().ok()?.as_socket()?;
        s.connect_timeout(&server.into(), Duration::from_secs(3)).ok()?;
        let mut line: TcpStream = s.into();
        line.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        writeln!(line, "seen").ok()?;
        let seen = crate::access::line(&mut line).ok()?;
        let mut mine = Vec::new();
        if let Ok(a) = seen.parse::<SocketAddr>()
            && crate::udp::usable(a.ip())
        {
            mine.push(a);
        }
        // A relay on this computer (tests, a gateway beside it): this computer's own address.
        if server.ip().is_loopback() {
            mine.push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), local.port()));
        }
        for ip in crate::udp::local_ips(Some(server)).into_iter().filter(|ip| ip.is_ipv4()) {
            let a = SocketAddr::new(ip, local.port());
            if !mine.contains(&a) {
                mine.push(a);
            }
        }
        mine.truncate(8);
        (!mine.is_empty()).then_some(Punch { local, _line: line, mine })
    }

    /// Connects from this port to each of `peers` at once, again and again until `until`, and
    /// takes connections that come to it too; `check` runs on each that connects (a thread each)
    /// and the first it passes is returned.
    pub fn race(&self, peers: &[SocketAddr], until: Instant, check: Arc<dyn Fn(&mut TcpStream) -> bool + Send + Sync>) -> Option<TcpStream> {
        let (tx, rx) = mpsc::channel::<TcpStream>();
        let done = Arc::new(AtomicBool::new(false));
        let hand = {
            let done = done.clone();
            move |mut s: TcpStream, tx: &mpsc::Sender<TcpStream>| {
                let (tx, done, check) = (tx.clone(), done.clone(), check.clone());
                std::thread::spawn(move || {
                    let _ = s.set_nodelay(true);
                    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
                    if !done.load(Ordering::Acquire) && check(&mut s) {
                        let _ = s.set_read_timeout(None);
                        let _ = tx.send(s);
                    }
                });
            }
        };
        if let Ok(l) = bound(self.local).and_then(|s| s.listen(8).map(|()| s)) {
            let l: std::net::TcpListener = l.into();
            let (tx, done, hand) = (tx.clone(), done.clone(), hand.clone());
            std::thread::spawn(move || {
                let _ = l.set_nonblocking(true);
                while Instant::now() < until && !done.load(Ordering::Acquire) {
                    match l.accept() {
                        Ok((s, _)) => {
                            let _ = s.set_nonblocking(false);
                            hand(s, &tx);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(30)),
                    }
                }
            });
        }
        for &p in peers.iter().filter(|p| p.is_ipv4() && p.port() != 0).take(8) {
            let (tx, done, hand, local) = (tx.clone(), done.clone(), hand.clone(), self.local);
            std::thread::spawn(move || {
                while Instant::now() < until && !done.load(Ordering::Acquire) {
                    let Ok(s) = bound(local) else { return };
                    match s.connect_timeout(&p.into(), Duration::from_millis(900)) {
                        Ok(()) => return hand(s.into(), &tx),
                        Err(_) => std::thread::sleep(Duration::from_millis(150)),
                    }
                }
            });
        }
        drop(tx);
        let got = rx.recv_timeout(until.saturating_duration_since(Instant::now()) + Duration::from_secs(3)).ok();
        done.store(true, Ordering::Release);
        got
    }
}

fn mac(key: &[u8; 32], side: &[u8], tag: &[u8; 8]) -> hmac::Hmac<sha2::Sha256> {
    use hmac::Mac;
    let mut m = <hmac::Hmac<sha2::Sha256> as Mac>::new_from_slice(key).expect("any key length");
    m.update(b"sshremote tcp ");
    m.update(side);
    m.update(tag);
    m
}

/// The viewer's side of a connection: it shows it knows the key, and so must the agent.
pub fn viewer_check(tag: [u8; 8], key: [u8; 32]) -> Arc<dyn Fn(&mut TcpStream) -> bool + Send + Sync> {
    Arc::new(move |s: &mut TcpStream| {
        use hmac::Mac;
        let mut hello = HELLO.to_vec();
        hello.extend_from_slice(&tag);
        hello.extend_from_slice(&mac(&key, b"viewer", &tag).finalize().into_bytes());
        let mut answer = [0u8; 36];
        s.write_all(&hello).is_ok() && s.read_exact(&mut answer).is_ok() && answer[..4] == HELLO[..] && mac(&key, b"agent", &tag).verify_slice(&answer[4..]).is_ok()
    })
}

/// The agent's side: the viewer's proof checked, and only the first connection that has one
/// answered (the viewer takes the one answered, so both use the same).
pub fn agent_check(tag: [u8; 8], key: [u8; 32]) -> Arc<dyn Fn(&mut TcpStream) -> bool + Send + Sync> {
    let answered = AtomicBool::new(false);
    Arc::new(move |s: &mut TcpStream| {
        use hmac::Mac;
        let mut hello = [0u8; 44];
        if s.read_exact(&mut hello).is_err() || hello[..4] != HELLO[..] || hello[4..12] != tag || mac(&key, b"viewer", &tag).verify_slice(&hello[12..]).is_err() {
            return false;
        }
        if answered.swap(true, Ordering::AcqRel) {
            return false;
        }
        let mut answer = HELLO.to_vec();
        answer.extend_from_slice(&mac(&key, b"agent", &tag).finalize().into_bytes());
        s.write_all(&answer).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two ports on this computer find each other by connecting at once; the agent answers only
    /// a viewer that knows the key, and the one connection both took carries bytes both ways.
    #[test]
    fn two_sides_meet_on_one_connection_that_knows_the_key() {
        const TAG: [u8; 8] = [7; 8];
        const KEY: [u8; 32] = [9; 32];
        let side = || {
            let s = bound(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)).unwrap();
            let local = s.local_addr().unwrap().as_socket().unwrap();
            // A line held open, as the relay's would be.
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let line = TcpStream::connect(l.local_addr().unwrap()).unwrap();
            (Punch { local, _line: line, mine: vec![local] }, l)
        };
        let run = |viewer_key: [u8; 32]| {
            let ((a, _la), (b, _lb)) = (side(), side());
            let (to_a, to_b) = (a.mine.clone(), b.mine.clone());
            let until = Instant::now() + Duration::from_secs(2);
            let agent = std::thread::spawn(move || a.race(&to_b, until, agent_check(TAG, KEY)));
            let viewer = b.race(&to_a, until, viewer_check(TAG, viewer_key));
            (agent.join().unwrap(), viewer)
        };
        let (agent, viewer) = run([1; 32]);
        assert!(agent.is_none() && viewer.is_none(), "a viewer without the key is not answered");
        let (agent, viewer) = run(KEY);
        let (mut a, mut v) = (agent.expect("the agent took one"), viewer.expect("the viewer took one"));
        v.write_all(b"ping").unwrap();
        let mut b = [0u8; 4];
        a.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ping");
        a.write_all(b"pong").unwrap();
        v.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"pong", "the same connection on both sides");
    }
}
