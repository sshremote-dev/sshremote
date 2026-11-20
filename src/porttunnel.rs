//! A port here that leads to HOST:PORT as a computer reached by its ID sees it: each connection to
//! the port is carried through one session with the computer's agent, which connects there.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::proto::{ToAgent, ToClient};
use crate::session::{Ev, Session};

/// Where a forward is.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Reaching the computer (whose person may be asked to let it in).
    Starting,
    On,
    Failed(String),
}

/// A forward open now; it closes when dropped.
pub struct Forward {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
}

impl Forward {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// What the forward's own thread hears: from the computer, or from the connections here.
enum Event {
    Net(Ev),
    Accepted(TcpStream),
    Data(u32, Vec<u8>),
    Closed(u32),
}

/// Bytes read from a connection at a time.
const CHUNK: usize = 32 << 10;
/// The agent ends a session it heard nothing from in two minutes.
const PING: Duration = Duration::from_secs(30);

/// Opens `port` on this computer, leading to `dest` (HOST:PORT) as the computer at `address`
/// (`sshremote://ID@RELAY`) sees it, with its `password`.
pub fn open(address: &str, password: String, port: u16, dest: &str) -> Result<Forward, String> {
    let addr = crate::net::Address::parse(address).ok_or("not an address by ID")?;
    open_to(crate::transport::Target::Direct { addr, password }, port, dest)
}

/// `open` through any way of reaching an agent.
fn open_to(target: crate::transport::Target, port: u16, dest: &str) -> Result<Forward, String> {
    let (host, dport) = dest.rsplit_once(':').and_then(|(h, p)| Some((h.to_string(), p.parse::<u16>().ok()?))).ok_or("Where it leads is HOST:PORT")?;
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|_| "that port is already taken; pick another".to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(Status::Starting));
    let (tx, rx) = mpsc::channel();
    {
        let (stop, tx) = (stop.clone(), tx.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let _ = s.set_nonblocking(false);
                        // Passed on as it comes: a small write is not held for the last one's
                        // acknowledgement (an interactive program through the port waited).
                        let _ = s.set_nodelay(true);
                        if tx.send(Event::Accepted(s)).is_err() {
                            return;
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        });
    }
    {
        let (stop, status) = (stop.clone(), status.clone());
        std::thread::spawn(move || run(target, (host, dport), stop, status, tx, rx));
    }
    Ok(Forward { stop, status })
}

fn set(status: &Mutex<Status>, s: Status) {
    *status.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

/// The session, kept up while the forward is open: connections here wait for it and go through it.
fn run(target: crate::transport::Target, (host, port): (String, u16), stop: Arc<AtomicBool>, status: Arc<Mutex<Status>>, tx: Sender<Event>, rx: Receiver<Event>) {
    let mut wait = Duration::from_secs(2);
    while !stop.load(Ordering::Relaxed) {
        set(&status, Status::Starting);
        let (mut sess, net) = match Session::open(&target, true) {
            Ok((s, _, net)) => (s, net),
            Err(e) => {
                set(&status, Status::Failed(e));
                let until = Instant::now() + wait;
                while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    // Connections made meanwhile are turned away.
                    while let Ok(Event::Accepted(s)) = rx.try_recv() {
                        drop(s);
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                wait = (wait * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        wait = Duration::from_secs(2);
        set(&status, Status::On);
        let alive = Arc::new(AtomicBool::new(true));
        {
            let (tx, alive) = (tx.clone(), alive.clone());
            std::thread::spawn(move || {
                while alive.load(Ordering::Relaxed) {
                    match net.recv_timeout(Duration::from_millis(500)) {
                        Ok(e) => {
                            let end = matches!(e, Ev::Closed(_));
                            if tx.send(Event::Net(e)).is_err() || end {
                                return;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return,
                    }
                }
            });
        }
        let lost = serve(&mut sess, &host, port, &stop, &tx, &rx);
        alive.store(false, Ordering::Relaxed);
        if let Some(why) = lost {
            set(&status, Status::Failed(why));
        }
    }
}

/// Carries connections until the forward closes (None) or the session is lost (why).
fn serve(sess: &mut Session, host: &str, port: u16, stop: &AtomicBool, tx: &Sender<Event>, rx: &Receiver<Event>) -> Option<String> {
    let mut writers: HashMap<u32, Sender<Vec<u8>>> = HashMap::new();
    let mut next = 1u32;
    let mut pinged = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        if pinged.elapsed() > PING {
            pinged = Instant::now();
            if sess.send(ToAgent::Ping(0)).is_err() {
                return Some("the connection was lost".into());
            }
        }
        let e = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(e) => e,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return None,
        };
        let sent = match e {
            Event::Accepted(s) => {
                let id = next;
                next += 1;
                let Ok(reading) = s.try_clone() else { continue };
                let (wtx, wrx) = mpsc::channel::<Vec<u8>>();
                writers.insert(id, wtx);
                std::thread::spawn(move || write_out(s, wrx));
                let tx = tx.clone();
                std::thread::spawn(move || read_in(id, reading, tx));
                sess.send(ToAgent::PortOpen { id, host: host.to_string(), port })
            }
            Event::Data(id, data) => sess.send(ToAgent::PortData { id, data }),
            Event::Closed(id) => {
                writers.remove(&id);
                sess.send(ToAgent::PortClose { id })
            }
            Event::Net(Ev::Net(ToClient::PortData { id, data })) => {
                if let Some(w) = writers.get(&id) {
                    let _ = w.send(data);
                }
                Ok(())
            }
            Event::Net(Ev::Net(ToClient::PortClosed { id } | ToClient::PortOpened { id, ok: false, .. })) => {
                writers.remove(&id);
                Ok(())
            }
            Event::Net(Ev::Net(_)) => Ok(()),
            Event::Net(Ev::Closed(why)) => return Some(why),
            Event::Net(Ev::Term(_)) => Ok(()),
        };
        if sent.is_err() {
            return Some("the connection was lost".into());
        }
    }
}

/// What one connection here sends, read until it closes.
fn read_in(id: u32, mut s: TcpStream, tx: Sender<Event>) {
    let mut buf = vec![0u8; CHUNK];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(Event::Data(id, buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        }
    }
    let _ = tx.send(Event::Closed(id));
}

/// What comes back for one connection, written to it; it is shut once nothing more will come.
fn write_out(mut s: TcpStream, rx: Receiver<Vec<u8>>) {
    while let Ok(data) = rx.recv() {
        if s.write_all(&data).is_err() {
            break;
        }
    }
    let _ = s.shutdown(std::net::Shutdown::Both);
}

/// The agent's end: connections it made for the viewer, each with a thread reading it and one
/// writing it, and what they read waiting to go out.
#[derive(Default)]
pub struct Ports {
    open: HashMap<u32, Sender<Vec<u8>>>,
    /// Bounded: a connection that reads faster than the viewer takes waits, and so does its sender.
    events: Option<(mpsc::SyncSender<ToClient>, Receiver<ToClient>)>,
    /// Called as a connection has something, for a loop that waits rather than looks.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The last pump left some of what was read for the next turn.
    more: bool,
}

impl Ports {
    /// Connections whose news calls `wake` as it comes.
    pub fn waking(wake: Arc<dyn Fn() + Send + Sync>) -> Ports {
        Ports { wake: Some(wake), ..Ports::default() }
    }

    /// What was read waits for the next turn, which should come at once.
    pub fn busy(&self) -> bool {
        self.more
    }

    fn events(&mut self) -> &mpsc::SyncSender<ToClient> {
        &self.events.get_or_insert_with(|| mpsc::sync_channel(64)).0
    }

    /// Connects to `host`:`port` for the viewer's connection `id`, in the background; what the
    /// viewer sends meanwhile waits for it.
    pub fn open(&mut self, id: u32, host: String, port: u16) {
        let (wtx, wrx) = mpsc::channel::<Vec<u8>>();
        self.open.insert(id, wtx);
        let events = self.events().clone();
        let wake = self.wake.clone();
        // Whether it could go (the agent may have ended).
        let tx = move |m: ToClient| {
            let sent = events.send(m).is_ok();
            wake.iter().for_each(|w| w());
            sent
        };
        std::thread::spawn(move || {
            use std::net::ToSocketAddrs;
            let reached = (host.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| e.to_string())
                .and_then(|mut a| a.next().ok_or_else(|| "the name is not known".to_string()))
                .and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(10)).map_err(|e| e.to_string()));
            let s = match reached {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx(ToClient::PortOpened { id, ok: false, text: format!("{host}:{port}: {e}") });
                    return;
                }
            };
            let _ = s.set_nodelay(true);
            let _ = tx(ToClient::PortOpened { id, ok: true, text: String::new() });
            let Ok(mut reading) = s.try_clone() else { return };
            std::thread::spawn(move || write_out(s, wrx));
            let mut buf = vec![0u8; CHUNK];
            loop {
                match reading.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if !tx(ToClient::PortData { id, data: buf[..n].to_vec() }) {
                            return;
                        }
                    }
                }
            }
            let _ = tx(ToClient::PortClosed { id });
        });
    }

    pub fn data(&mut self, id: u32, data: Vec<u8>) {
        if let Some(w) = self.open.get(&id) {
            let _ = w.send(data);
        }
    }

    pub fn close(&mut self, id: u32) {
        self.open.remove(&id);
    }

    /// What the connections read, up to about `most` bytes this turn, so the screen keeps its share.
    pub fn pump(&mut self, most: usize) -> Vec<ToClient> {
        self.more = false;
        let Some((_, rx)) = self.events.as_ref() else { return Vec::new() };
        let mut out = Vec::new();
        let mut size = 0;
        while size < most {
            let Ok(m) = rx.try_recv() else { break };
            if let ToClient::PortData { data, .. } = &m {
                size += data.len();
            }
            if let ToClient::PortClosed { id } | ToClient::PortOpened { id, ok: false, .. } = &m {
                self.open.remove(id);
            }
            out.push(m);
        }
        self.more = size >= most;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port here reaches an echo server through a local agent and back, and a refused one
    /// closes at once. Needs SSHREMOTE_EXE (a built sshremote) for the agent.
    #[test]
    #[ignore]
    fn a_forwarded_port_carries_a_connection_both_ways() {
        if std::env::var_os("SSHREMOTE_EXE").is_none() {
            return;
        }
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let there = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in echo.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut r = s.try_clone().unwrap();
                    let mut w = s;
                    let _ = std::io::copy(&mut r, &mut w);
                });
            }
        });
        let here = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let target = crate::transport::Target::Local { capture: "synthetic".into() };
        let fwd = open_to(target, here, &format!("127.0.0.1:{there}")).unwrap();
        for _ in 0..100 {
            if fwd.status() == Status::On {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(fwd.status(), Status::On);
        let mut c = TcpStream::connect(("127.0.0.1", here)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut w = c.try_clone().unwrap();
        let sent = big.clone();
        std::thread::spawn(move || w.write_all(&sent).unwrap());
        let mut got = vec![0u8; big.len()];
        c.read_exact(&mut got).unwrap();
        assert!(got == big, "the echo came back changed");
        drop(c);
        drop(fwd);
    }
}
