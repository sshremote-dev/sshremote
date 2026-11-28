//! A straight path to the other side over UDP, found through both sides' routers (NAT hole
//! punching), and an ordered, reliable stream on it as TCP gives: what a sealed stream moves to
//! from the relay once the path is found (`net`).
//!
//! Each side asks the relay, from its own socket, which address and port its router gave that
//! socket; the two swap those and their own network addresses inside the sealed stream, then
//! both send probes to all of the other's. A probe out opens this side's router to answers from
//! there, so once both have sent, probes get through and are answered. The viewer names the first
//! address that answered, the agent takes it, and the stream runs there. Every packet carries a
//! tag only the two sides know (it went inside the sealed stream), so a stranger's are dropped;
//! what the stream carries is sealed already.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Stream bytes in one packet: with the headers, under the 1280 bytes every path carries.
const MSS: usize = 1180;
/// Bytes written and not yet taken by the other side, and bytes come and not yet read.
const SEND_LIMIT: usize = 4 << 20;
const RECV_LIMIT: usize = 4 << 20;
const MAX_CWND: f64 = 4096.0;
const MIN_RTO: Duration = Duration::from_millis(200);
const MAX_RTO: Duration = Duration::from_secs(3);
/// Nothing heard for this long: the path is gone (both sides keep it alive more often).
const DEAD: Duration = Duration::from_secs(20);
const KEEPALIVE: Duration = Duration::from_secs(2);
/// How long both sides try before the session stays on the relay.
pub const PUNCH_LIMIT: Duration = Duration::from_secs(6);
/// How much longer the viewer waits for the answer to a path it named: the agent moves on the
/// naming, so the viewer giving up just then would leave the two on different paths.
pub const NOMINATE_GRACE: Duration = Duration::from_secs(3);
const ACK_DELAY: Duration = Duration::from_millis(3);
/// Before the timeout, the last packet is sent again this many times, each wait twice the one
/// before: a single key whose packet and first probe were both lost waited the whole timeout.
const TAIL_PROBES: u32 = 2;
/// One side waits this long before it probes (or until it hears from the other), so the other
/// side's router has seen its side send first. A probe that reaches a router before its side sent
/// anything is turned away, and some routers then keep the way shut: a Linux router that answers
/// it keeps the stray as a flow of its own and gives its side's probes another port, and some
/// routers answered so drop the mapping of the side that sent it. Long enough for the addresses
/// to cross the relay.
const OPENING: Duration = Duration::from_millis(1000);
/// When both sides' routers may turn away the other's first probes (a third try), both first
/// send IPv4 probes that die two hops out for this long: past their own router, which then
/// knows the way, but not as far as the other's.
const LOW_FOR: Duration = Duration::from_millis(600);
const LOW_TTL: u32 = 2;

/// Who probes first.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Start {
    /// This side, at once.
    Now,
    /// The other side: this one waits `OPENING`, or until it hears from it.
    Second,
    /// Both, opening their own routers only for `LOW_FOR`.
    Low,
}

/// After the first answer, how long the viewer waits for an answer on a private network (the
/// two on one network, or one router apart) before it names the path.
const SETTLE: Duration = Duration::from_millis(100);
/// What was written is still delivered this long after the stream is let go.
const LINGER: Duration = Duration::from_secs(3);
/// The relay's answer to "where do you see me from" (and the question's start).
pub const REFLECT: &[u8] = b"sshremote-addr 1 ";

const PROBE: u8 = 1;
const PROBE_ACK: u8 = 2;
const NOMINATE: u8 = 3;
const NOMINATE_ACK: u8 = 4;
const DATA: u8 = 5;
const ACK: u8 = 6;
const PING: u8 = 7;
const FIN: u8 = 8;
/// Kind, sender (0 viewer, 1 agent), the session's tag.
const HEAD: usize = 10;
const VIEWER: u8 = 0;
/// With a key both sides were given on the sealed line, every packet ends with this much of its
/// HMAC-SHA256: one that anyone else sent or changed (who saw the tag go by, a machine on the
/// network the probes reached) is not taken.
const MAC: usize = 8;

/// The check a packet ends with under `key`.
fn mac_of(key: &[u8; 32], pkt: &[u8]) -> hmac::Hmac<sha2::Sha256> {
    use hmac::Mac;
    let mut m = <hmac::Hmac<sha2::Sha256> as Mac>::new_from_slice(key).expect("any key length");
    m.update(pkt);
    m
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

struct Sent {
    data: Vec<u8>,
    at: Instant,
    again: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Idle,
    Punching,
    Up(SocketAddr),
    Done,
}

struct State {
    phase: Phase,
    tag: [u8; 8],
    /// The key packets are checked with, when both sides know them (`MAC`).
    key: Option<[u8; 32]>,
    // Punching.
    peers: Vec<SocketAddr>,
    probes: VecDeque<(u32, Instant)>,
    probe_n: u32,
    next_probe: Instant,
    started: Instant,
    hint: Option<Duration>,
    nominee: Option<SocketAddr>,
    next_nominate: Instant,
    /// Addresses that answered the viewer's probes, and when it names one.
    working: Vec<SocketAddr>,
    nominate_at: Option<Instant>,
    /// This side still waits for the other to probe first (`OPENING`).
    opening: bool,
    /// Its IPv4 probes still die two hops out until then (`Start::Low`).
    low_until: Option<Instant>,
    /// A read fails after this long without new data (`Stream::set_idle`); when data last came.
    idle: Option<Duration>,
    got: Instant,
    // Sending: packets numbered from 0, kept until the other side has them.
    seq: u32,
    queue: VecDeque<u8>,
    flight: BTreeMap<u32, Sent>,
    flight_bytes: usize,
    lost: BTreeSet<u32>,
    largest: Option<u32>,
    rack: Option<Instant>,
    cwnd: f64,
    ssthresh: f64,
    recover: u32,
    srtt: Option<Duration>,
    rttvar: Duration,
    rto: Duration,
    backoff: u32,
    progress: Instant,
    /// Tail probes sent since the last progress (`TAIL_PROBES` at most before the timeout).
    tail_probes: u32,
    window: u32,
    // Receiving.
    expect: u32,
    ahead: BTreeMap<u32, Vec<u8>>,
    ahead_bytes: usize,
    ready: VecDeque<u8>,
    owe: u32,
    ack_at: Option<Instant>,
    told: u32,
    fin: bool,
    // Both.
    heard: Instant,
    said: Instant,
    closing: Option<Instant>,
    error: Option<io::ErrorKind>,
}

impl State {
    fn new() -> State {
        let now = Instant::now();
        State {
            phase: Phase::Idle,
            tag: [0; 8],
            key: None,
            peers: Vec::new(),
            probes: VecDeque::new(),
            probe_n: 0,
            next_probe: now,
            started: now,
            hint: None,
            nominee: None,
            next_nominate: now,
            working: Vec::new(),
            nominate_at: None,
            opening: false,
            low_until: None,
            idle: None,
            got: now,
            seq: 0,
            queue: VecDeque::new(),
            flight: BTreeMap::new(),
            flight_bytes: 0,
            lost: BTreeSet::new(),
            largest: None,
            rack: None,
            cwnd: 10.0,
            ssthresh: MAX_CWND,
            recover: 0,
            srtt: None,
            rttvar: Duration::ZERO,
            rto: Duration::from_secs(1),
            backoff: 0,
            progress: now,
            tail_probes: 0,
            window: (RECV_LIMIT / MSS) as u32,
            expect: 0,
            ahead: BTreeMap::new(),
            ahead_bytes: 0,
            ready: VecDeque::new(),
            owe: 0,
            ack_at: None,
            told: (RECV_LIMIT / MSS) as u32,
            fin: false,
            heard: now,
            said: now,
            closing: None,
            error: None,
        }
    }

    fn room(&self) -> usize {
        RECV_LIMIT.saturating_sub(self.ready.len() + self.ahead_bytes)
    }

    fn rtt(&mut self, r: Duration) {
        match self.srtt {
            None => {
                self.srtt = Some(r);
                self.rttvar = r / 2;
            }
            Some(s) => {
                let d = s.abs_diff(r);
                self.rttvar = (self.rttvar * 3 + d) / 4;
                self.srtt = Some((s * 7 + r) / 8);
            }
        }
        self.rto = (self.srtt.unwrap_or(r) + (self.rttvar * 4).max(Duration::from_millis(10))).clamp(MIN_RTO, MAX_RTO);
    }
}

struct Inner {
    /// IPv4, and IPv6 where this computer has it.
    socks: Vec<UdpSocket>,
    me: u8,
    st: Mutex<State>,
    cv: Condvar,
    /// Wakes the timers' own thread when a deadline nearer than the one it sleeps to may be due.
    timer: Condvar,
    /// Tests: one packet of the stream in this many is dropped on the way out (0: none).
    #[cfg(test)]
    lossy: std::sync::atomic::AtomicU32,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn sock_for(&self, to: SocketAddr) -> Option<&UdpSocket> {
        if to.is_ipv6() { self.socks.get(1) } else { self.socks.first() }
    }

    fn send(&self, st: &mut State, kind: u8, body: &[&[u8]], to: SocketAddr) {
        let mut p = Vec::with_capacity(HEAD + body.iter().map(|b| b.len()).sum::<usize>());
        p.push(kind);
        p.push(self.me);
        p.extend_from_slice(&st.tag);
        for b in body {
            p.extend_from_slice(b);
        }
        if let Some(key) = &st.key {
            use hmac::Mac;
            let check = mac_of(key, &p).finalize().into_bytes();
            p.extend_from_slice(&check[..MAC]);
        }
        st.said = Instant::now();
        #[cfg(test)]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static DICE: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);
            let n = self.lossy.load(Ordering::Relaxed);
            if n > 0 && kind >= DATA {
                let mut x = DICE.load(Ordering::Relaxed);
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                DICE.store(x, Ordering::Relaxed);
                if x.is_multiple_of(n as u64) {
                    return;
                }
            }
        }
        if let Some(s) = self.sock_for(to) {
            let _ = s.send_to(&p, to);
        }
    }

    fn up(&self, st: &mut State, to: SocketAddr, now: Instant) {
        st.phase = Phase::Up(to);
        st.heard = now;
        st.progress = now;
        if let Some(h) = st.hint {
            st.rtt(h);
        }
        self.cv.notify_all();
    }

    /// Ends at once: the other side is told, readers see the end, writers an error.
    fn abort(&self) {
        let mut st = self.lock();
        if let Phase::Up(to) = st.phase {
            for _ in 0..2 {
                self.send(&mut st, FIN, &[], to);
            }
        }
        st.phase = Phase::Done;
        self.cv.notify_all();
        self.timer.notify_all();
    }

    fn send_ack(&self, st: &mut State) {
        let Phase::Up(to) = st.phase else { return };
        let window = (st.room() / MSS) as u32;
        let mut body = Vec::with_capacity(8 + 16 * 8);
        body.extend_from_slice(&st.expect.to_le_bytes());
        body.extend_from_slice(&window.to_le_bytes());
        // What came past a gap, as ranges, so only what is missing is sent again.
        let mut run: Option<(u32, u32)> = None;
        let mut n = 0;
        for &k in st.ahead.keys() {
            match run {
                Some((a, b)) if b == k => run = Some((a, k + 1)),
                Some((a, b)) => {
                    body.extend_from_slice(&a.to_le_bytes());
                    body.extend_from_slice(&b.to_le_bytes());
                    n += 1;
                    run = (n < 16).then_some((k, k + 1));
                    if run.is_none() {
                        break;
                    }
                }
                None => run = Some((k, k + 1)),
            }
        }
        if let Some((a, b)) = run {
            body.extend_from_slice(&a.to_le_bytes());
            body.extend_from_slice(&b.to_le_bytes());
        }
        st.owe = 0;
        st.ack_at = None;
        st.told = window;
        self.send(st, ACK, &[&body], to);
    }

    /// Sends what the window allows: what was lost first, then what is waiting.
    fn pump(&self, st: &mut State) {
        let Phase::Up(to) = st.phase else { return };
        let limit = (st.cwnd as usize).min(st.window as usize);
        let now = Instant::now();
        while st.flight.len().saturating_sub(st.lost.len()) < limit {
            if let Some(seq) = st.lost.pop_first() {
                if let Some(data) = st.flight.get(&seq).map(|s| s.data.clone()) {
                    self.send(st, DATA, &[&seq.to_le_bytes(), &data], to);
                    if let Some(s) = st.flight.get_mut(&seq) {
                        s.at = now;
                        s.again = true;
                    }
                }
                continue;
            }
            if st.queue.is_empty() {
                break;
            }
            let n = st.queue.len().min(MSS);
            let data: Vec<u8> = st.queue.drain(..n).collect();
            let seq = st.seq;
            st.seq += 1;
            if st.flight.is_empty() {
                st.progress = now;
                // A key after a quiet while: its tail probe is due long before the timers next
                // look (a lost one waited the whole timeout, 200 ms).
                self.timer.notify_all();
            }
            self.send(st, DATA, &[&seq.to_le_bytes(), &data], to);
            st.flight_bytes += data.len();
            st.flight.insert(seq, Sent { data, at: now, again: false });
        }
    }

    fn on_ack(&self, st: &mut State, body: &[u8]) {
        let (Some(cum), Some(window)) = (u32_at(body, 0), u32_at(body, 4)) else { return };
        st.window = window;
        let now = Instant::now();
        let busy = (st.flight.len().saturating_sub(st.lost.len())) as f64 * 2.0 >= st.cwnd;
        let mut acked: Vec<(u32, Sent)> = Vec::new();
        while let Some(e) = st.flight.first_entry() {
            if *e.key() >= cum {
                break;
            }
            acked.push(e.remove_entry());
        }
        for r in body[8..].as_chunks::<8>().0 {
            let (Some(a), Some(b)) = (u32_at(r, 0), u32_at(r, 4)) else { continue };
            if b <= a || b - a > 1 << 16 {
                continue;
            }
            let keys: Vec<u32> = st.flight.range(a..b).map(|(k, _)| *k).collect();
            for k in keys {
                if let Some(s) = st.flight.remove(&k) {
                    acked.push((k, s));
                }
            }
        }
        if acked.is_empty() {
            self.detect_loss(st);
            return;
        }
        let mut sample: Option<Instant> = None;
        let recovering = !st.lost.is_empty();
        for (seq, s) in &acked {
            st.flight_bytes -= s.data.len();
            st.lost.remove(seq);
            st.largest = Some(st.largest.map_or(*seq, |l| l.max(*seq)));
            st.rack = Some(st.rack.map_or(s.at, |r| r.max(s.at)));
            if !s.again {
                sample = Some(sample.map_or(s.at, |t| t.max(s.at)));
            }
        }
        st.backoff = 0;
        st.progress = now;
        st.tail_probes = 0;
        if let Some(at) = sample {
            st.rtt(now - at);
        }
        // The window grows only while it is used: from small, quickly (after a timeout too);
        // past half of where loss came, slowly, and not while recovering what was lost.
        if busy {
            for _ in 0..acked.len() {
                if st.cwnd < st.ssthresh {
                    st.cwnd += 1.0;
                } else if !recovering {
                    st.cwnd += 1.0 / st.cwnd;
                }
            }
            st.cwnd = st.cwnd.min(MAX_CWND);
        }
        self.detect_loss(st);
        self.cv.notify_all();
    }

    /// A packet is lost once one sent after it (by more than a little) arrived, or three
    /// numbered after it did; the window halves once for each such loss.
    fn detect_loss(&self, st: &mut State) {
        let Some(rack) = st.rack else { return };
        let reo = st.srtt.map_or(Duration::from_millis(5), |s| s / 8) + Duration::from_millis(1);
        let largest = st.largest.unwrap_or(0);
        // Fewer than three behind it when few are under way (early retransmit).
        let behind = (st.flight.len().saturating_sub(1) as u32).clamp(1, 3);
        let mut newly = Vec::new();
        for (&seq, s) in &st.flight {
            if st.lost.contains(&seq) {
                continue;
            }
            let by_time = s.at + reo < rack;
            let by_count = !s.again && seq.saturating_add(behind) <= largest;
            if by_time || by_count {
                newly.push(seq);
            } else if !s.again {
                // Sent in order after this one, so not lost either.
                break;
            }
        }
        for seq in newly {
            st.lost.insert(seq);
            if seq >= st.recover {
                st.ssthresh = (st.cwnd / 2.0).max(4.0);
                st.cwnd = st.ssthresh;
                st.recover = st.seq;
            }
        }
    }

    fn on_data(&self, st: &mut State, seq: u32, payload: &[u8]) {
        if st.fin {
            return;
        }
        if seq < st.expect || st.ahead.contains_key(&seq) || payload.len() > st.room() {
            // Had already, or no room: the other side hears where things stand.
            self.send_ack(st);
            return;
        }
        if seq == st.expect {
            let gap = !st.ahead.is_empty();
            st.got = Instant::now();
            st.ready.extend(payload);
            st.expect += 1;
            while let Some(p) = st.ahead.remove(&st.expect) {
                st.ahead_bytes -= p.len();
                st.ready.extend(&p);
                st.expect += 1;
            }
            self.cv.notify_all();
            st.owe += 1;
            if st.owe >= 2 || gap {
                self.send_ack(st);
            } else if st.ack_at.is_none() {
                st.ack_at = Some(Instant::now() + ACK_DELAY);
            }
        } else {
            if seq - st.expect > 2 * (RECV_LIMIT / MSS) as u32 {
                return;
            }
            st.ahead_bytes += payload.len();
            st.ahead.insert(seq, payload.to_vec());
            self.send_ack(st);
        }
    }

    /// A packet as it came: its check (when packets are checked) taken off first.
    fn receive(&self, st: &mut State, pkt: &[u8], from: SocketAddr) {
        let pkt = match &st.key {
            Some(key) => {
                use hmac::Mac;
                let Some(at) = pkt.len().checked_sub(MAC).filter(|&n| n >= HEAD) else { return };
                if mac_of(key, &pkt[..at]).verify_truncated_left(&pkt[at..]).is_err() {
                    return;
                }
                &pkt[..at]
            }
            None => pkt,
        };
        self.take(st, pkt, from);
    }

    fn take(&self, st: &mut State, pkt: &[u8], from: SocketAddr) {
        if pkt.len() < HEAD || pkt[1] == self.me || pkt[1] > 1 || pkt[2..HEAD] != st.tag {
            return;
        }
        let (kind, body) = (pkt[0], &pkt[HEAD..]);
        let now = Instant::now();
        // Heard from the other side: its router has seen it send.
        st.opening = false;
        match st.phase {
            Phase::Punching => match kind {
                PROBE if body.len() >= 4 => {
                    self.send(st, PROBE_ACK, &[&body[..4]], from);
                    // The other side as its router shows it to this one: worth probing too.
                    if !st.peers.contains(&from) && st.peers.len() < 24 {
                        st.peers.push(from);
                    }
                }
                PROBE_ACK => {
                    if let Some(id) = u32_at(body, 0)
                        && let Some(&(_, t)) = st.probes.iter().find(|p| p.0 == id)
                    {
                        st.hint = Some(now - t);
                    }
                    if self.me == VIEWER && st.nominee.is_none() && !st.working.contains(&from) {
                        st.working.push(from);
                        st.nominate_at.get_or_insert(now + SETTLE);
                        if private(from.ip()) {
                            st.nominate_at = Some(now);
                        }
                    }
                }
                NOMINATE if self.me != VIEWER => {
                    self.up(st, from, now);
                    self.send(st, NOMINATE_ACK, &[], from);
                }
                NOMINATE_ACK | DATA | ACK | PING if self.me == VIEWER && st.nominee == Some(from) => {
                    self.up(st, from, now);
                    if kind != NOMINATE_ACK {
                        self.take(st, pkt, from);
                    }
                }
                _ => {}
            },
            Phase::Up(to) if from == to => {
                st.heard = now;
                match kind {
                    DATA if body.len() >= 4 => self.on_data(st, u32_at(body, 0).unwrap_or(0), &body[4..]),
                    ACK => self.on_ack(st, body),
                    PING => self.send_ack(st),
                    FIN => {
                        st.fin = true;
                        self.cv.notify_all();
                    }
                    NOMINATE => self.send(st, NOMINATE_ACK, &[], from),
                    PROBE if body.len() >= 4 => self.send(st, PROBE_ACK, &[&body[..4]], from),
                    _ => {}
                }
                self.pump(st);
            }
            _ => {}
        }
    }

    /// Timers: probes while punching; acks, losses, keepalives and the end once up. The next
    /// time to look again, or None once done.
    fn tick(&self, st: &mut State, now: Instant) -> Option<Instant> {
        match st.phase {
            Phase::Idle => Some(now + Duration::from_millis(100)),
            Phase::Done => None,
            Phase::Punching => {
                if st.opening && now >= st.started + OPENING {
                    st.opening = false;
                    st.next_probe = now;
                }
                let grace = if st.nominee.is_some() { NOMINATE_GRACE } else { Duration::ZERO };
                if now >= st.started + PUNCH_LIMIT + grace {
                    st.phase = Phase::Done;
                    st.error = Some(io::ErrorKind::TimedOut);
                    self.cv.notify_all();
                    return None;
                }
                if !st.opening && now >= st.next_probe {
                    st.probe_n += 1;
                    let id = st.probe_n;
                    st.probes.push_back((id, now));
                    if st.probes.len() > 64 {
                        st.probes.pop_front();
                    }
                    let low = st.low_until.is_some_and(|t| now < t);
                    let v4 = &self.socks[0];
                    let ttl = v4.ttl().unwrap_or(64);
                    if low {
                        let _ = v4.set_ttl(LOW_TTL);
                    }
                    for p in st.peers.clone() {
                        self.send(st, PROBE, &[&id.to_le_bytes()], p);
                    }
                    if low {
                        let _ = v4.set_ttl(ttl);
                    }
                    let early = now - st.started < Duration::from_secs(2);
                    st.next_probe = now + Duration::from_millis(if early { 100 } else { 250 });
                }
                // The viewer names a path: one on a private network if that answered, else the
                // first that did.
                if st.nominee.is_none()
                    && let Some(at) = st.nominate_at
                    && now >= at
                {
                    st.nominee = st.working.iter().min_by_key(|a| rank(a)).copied();
                    st.next_nominate = now;
                }
                let mut next = if st.opening { st.started + OPENING } else { st.next_probe.min(st.started + PUNCH_LIMIT + grace) };
                if st.nominee.is_none()
                    && let Some(at) = st.nominate_at
                {
                    next = next.min(at);
                }
                if let Some(n) = st.nominee {
                    if now >= st.next_nominate {
                        self.send(st, NOMINATE, &[], n);
                        st.next_nominate = now + Duration::from_millis(100);
                    }
                    next = next.min(st.next_nominate);
                }
                Some(next)
            }
            Phase::Up(to) => {
                if now.duration_since(st.heard) > DEAD {
                    st.phase = Phase::Done;
                    st.error = Some(io::ErrorKind::TimedOut);
                    self.cv.notify_all();
                    return None;
                }
                if let Some(c) = st.closing
                    && ((st.flight.is_empty() && st.queue.is_empty()) || now.duration_since(c) > LINGER)
                {
                    for _ in 0..2 {
                        self.send(st, FIN, &[], to);
                    }
                    st.phase = Phase::Done;
                    self.cv.notify_all();
                    return None;
                }
                if st.ack_at.is_some_and(|t| now >= t) {
                    self.send_ack(st);
                }
                let mut next = (st.heard + DEAD).min(st.said + KEEPALIVE);
                if !st.flight.is_empty() {
                    let rto_at = st.progress + (st.rto * (1 << st.backoff)).min(MAX_RTO);
                    let pto = st.srtt.map_or(st.rto, |s| (s * 2).max(Duration::from_millis(10)) + ACK_DELAY * 2);
                    if now >= rto_at {
                        // Nothing came back in time: all of it again, from a small window.
                        let keys: Vec<u32> = st.flight.keys().copied().collect();
                        st.lost.extend(keys);
                        st.ssthresh = (st.cwnd / 2.0).max(4.0);
                        st.cwnd = 2.0;
                        st.recover = st.seq;
                        st.backoff = (st.backoff + 1).min(5);
                        st.progress = now;
                        st.tail_probes = 0;
                    } else if st.tail_probes < TAIL_PROBES && st.lost.is_empty() && now >= st.progress + pto * (1 << st.tail_probes) {
                        // The last packets may be what was lost: the last one again tells.
                        if let Some((&seq, s)) = st.flight.last_key_value() {
                            let data = s.data.clone();
                            self.send(st, DATA, &[&seq.to_le_bytes(), &data], to);
                            if let Some(s) = st.flight.get_mut(&seq) {
                                s.at = now;
                                s.again = true;
                            }
                        }
                        st.tail_probes += 1;
                    }
                    next = next.min(st.progress + (st.rto * (1 << st.backoff)).min(MAX_RTO));
                    if st.tail_probes < TAIL_PROBES {
                        next = next.min(st.progress + pto * (1 << st.tail_probes));
                    }
                }
                let stuck = st.window == 0 && !st.queue.is_empty() && now.duration_since(st.said) >= st.rto;
                if now.duration_since(st.said) >= KEEPALIVE || stuck {
                    self.send(st, PING, &[], to);
                }
                self.pump(st);
                if let Some(t) = st.ack_at {
                    next = next.min(t);
                }
                if let Some(c) = st.closing {
                    next = next.min(c + LINGER).min(now + Duration::from_millis(20));
                }
                Some(next)
            }
        }
    }

    /// Keeps the timers between packets, woken early as data goes out after a quiet while.
    fn keep_time(self: Arc<Self>) {
        let mut st = self.lock();
        loop {
            let Some(next) = self.tick(&mut st, Instant::now()) else { return };
            // At most as long as the sockets' threads sleep, so an end is seen as soon.
            let wait = next.saturating_duration_since(Instant::now()).clamp(Duration::from_millis(1), Duration::from_millis(200));
            st = self.timer.wait_timeout(st, wait).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Receives on one socket; each socket's thread also keeps the timers.
    fn run(self: Arc<Self>, which: usize) {
        let sock = &self.socks[which];
        let mut buf = vec![0u8; 2048];
        loop {
            let next = {
                let mut st = self.lock();
                match self.tick(&mut st, Instant::now()) {
                    Some(t) => t,
                    None => break,
                }
            };
            let wait = next.saturating_duration_since(Instant::now()).clamp(Duration::from_millis(1), Duration::from_millis(200));
            let _ = sock.set_read_timeout(Some(wait));
            // Errors include a port that said it was closed (Windows reports it on the next
            // read): the timers decide when the path is gone.
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                let mut st = self.lock();
                self.receive(&mut st, &buf[..n], from);
            }
        }
    }
}

/// One session's sockets, from finding the path to the stream on it.
pub struct Path {
    inner: Arc<Inner>,
    /// The relay is on this computer: the other side may be too.
    local_relay: bool,
    taken: bool,
}

impl Path {
    pub fn bind(viewer: bool) -> io::Result<Path> {
        let mut socks = vec![UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?];
        // IPv6 has no router in the way but a firewall: the same probes open it.
        socks.extend(UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok());
        socks.iter().for_each(bigger_buffers);
        let inner = Inner {
            socks,
            me: if viewer { VIEWER } else { 1 },
            st: Mutex::new(State::new()),
            cv: Condvar::new(),
            timer: Condvar::new(),
            #[cfg(test)]
            lossy: std::sync::atomic::AtomicU32::new(0),
        };
        Ok(Path { inner: Arc::new(inner), local_relay: false, taken: false })
    }

    /// Where the other side may reach these sockets: as the relay sees the IPv4 one from outside
    /// (the router's address and the port it gave), and on this computer's own networks, IPv6
    /// addresses (which no router changes) included.
    pub fn candidates(&mut self, relay: &str) -> Vec<SocketAddr> {
        let port = |s: &UdpSocket| s.local_addr().map_or(0, |a| a.port());
        let (host, rport) = crate::access::relay_addr(relay);
        let servers: Vec<SocketAddr> = (host.as_str(), rport).to_socket_addrs().map(|a| a.collect()).unwrap_or_default();
        let server = servers.iter().find(|a| a.is_ipv4()).copied();
        self.local_relay = server.is_some_and(|s| s.ip().is_loopback());
        let v4 = &self.inner.socks[0];
        let mut out = Vec::new();
        if self.local_relay {
            out.push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port(v4)));
        }
        if let Some(r) = server.and_then(|s| reflect(v4, s))
            && usable(r.ip())
        {
            out.push(r);
        }
        for ip in local_ips(server) {
            let a = SocketAddr::new(ip, port(v4));
            if !out.contains(&a) {
                out.push(a);
            }
        }
        out.truncate(8);
        if let Some(v6) = self.inner.socks.get(1) {
            if self.local_relay {
                out.push(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port(v6)));
            }
            for ip in local_ips6(servers.iter().find(|a| a.is_ipv6()).copied()) {
                out.push(SocketAddr::new(ip, port(v6)));
            }
        }
        out.truncate(12);
        out
    }

    /// Starts trying the other side's addresses, with the tag both were given, and the key
    /// every packet is checked with when both know it.
    pub fn punch(&mut self, tag: [u8; 8], key: Option<[u8; 32]>, peers: &[SocketAddr], start: Start) {
        let (local, v6) = (self.local_relay, self.inner.socks.len() > 1);
        let mut st = self.inner.lock();
        if st.phase != Phase::Idle {
            return;
        }
        st.tag = tag;
        st.key = key;
        st.peers = peers.iter().copied().filter(|p| p.port() != 0 && (p.is_ipv4() || v6) && (usable(p.ip()) || (local && p.ip().is_loopback()))).take(20).collect();
        st.phase = Phase::Punching;
        st.started = Instant::now();
        st.next_probe = st.started;
        st.opening = start == Start::Second;
        st.low_until = (start == Start::Low).then(|| st.started + LOW_FOR);
        drop(st);
        for which in 0..self.inner.socks.len() {
            let inner = self.inner.clone();
            std::thread::spawn(move || inner.run(which));
        }
        let inner = self.inner.clone();
        std::thread::spawn(move || inner.keep_time());
    }

    /// Waits for the path: where it goes, or None when none was found.
    pub fn wait(&self, limit: Duration) -> Option<SocketAddr> {
        let until = Instant::now() + limit;
        let mut st = self.inner.lock();
        loop {
            match st.phase {
                Phase::Up(to) => return Some(to),
                Phase::Done => return None,
                _ => {}
            }
            let left = until.checked_duration_since(Instant::now())?;
            st = self.inner.cv.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The stream on the path found.
    pub fn stream(mut self) -> Stream {
        self.taken = true;
        Stream(Arc::new(Handle(self.inner.clone())))
    }

    #[cfg(test)]
    fn lossy(&self, one_in: u32) {
        self.inner.lossy.store(one_in, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for Path {
    fn drop(&mut self) {
        if !self.taken {
            self.inner.abort();
        }
    }
}

struct Handle(Arc<Inner>);

/// Let go: what was written is still delivered, then the other side hears the end.
impl Drop for Handle {
    fn drop(&mut self) {
        let mut st = self.0.lock();
        if matches!(st.phase, Phase::Up(_)) {
            st.closing.get_or_insert_with(Instant::now);
        } else {
            st.phase = Phase::Done;
        }
        self.0.cv.notify_all();
    }
}

/// The stream on a straight path: a copy reads, another writes; the last one let go ends it.
#[derive(Clone)]
pub struct Stream(Arc<Handle>);

impl Stream {
    /// Ends it now for both sides.
    pub fn end(&self) {
        self.0.0.abort();
    }

    /// Ends it once what was written has arrived (or after `LINGER`).
    pub fn finish(&self) {
        let mut st = self.0.0.lock();
        if matches!(st.phase, Phase::Up(_)) {
            st.closing.get_or_insert_with(Instant::now);
        }
        self.0.0.cv.notify_all();
    }

    /// Reads fail when nothing new comes for `d`, as on a connection with a read timeout.
    pub fn set_idle(&self, d: Duration) {
        let mut st = self.0.0.lock();
        st.idle = Some(d);
        st.got = Instant::now();
    }
}

impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let inner = &self.0.0;
        let mut st = inner.lock();
        loop {
            if !st.ready.is_empty() {
                let n = out.len().min(st.ready.len());
                let (a, b) = st.ready.as_slices();
                if n <= a.len() {
                    out[..n].copy_from_slice(&a[..n]);
                } else {
                    out[..a.len()].copy_from_slice(a);
                    out[a.len()..n].copy_from_slice(&b[..n - a.len()]);
                }
                st.ready.drain(..n);
                // A window that had nearly shut opens: the sender hears at once.
                let window = (st.room() / MSS) as u32;
                if matches!(st.phase, Phase::Up(_)) && st.told < (RECV_LIMIT / MSS / 4) as u32 && window >= st.told * 2 + 8 {
                    inner.send_ack(&mut st);
                }
                return Ok(n);
            }
            if st.fin {
                return Ok(0);
            }
            if st.phase == Phase::Done {
                return match st.error {
                    Some(k) => Err(io::Error::new(k, "the straight path to the other computer was lost")),
                    None => Ok(0),
                };
            }
            st = match st.idle {
                Some(idle) => {
                    let Some(left) = idle.checked_sub(st.got.elapsed()) else {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "nothing came from the other computer"));
                    };
                    inner.cv.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0
                }
                None => inner.cv.wait(st).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }
}

impl Write for Stream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        let inner = &self.0.0;
        let mut st = inner.lock();
        loop {
            match st.phase {
                Phase::Done => {
                    return Err(io::Error::new(st.error.unwrap_or(io::ErrorKind::BrokenPipe), "the straight path to the other computer is closed"));
                }
                Phase::Up(_) if st.fin || st.closing.is_some() => return Err(io::ErrorKind::BrokenPipe.into()),
                Phase::Up(_) => {
                    let room = SEND_LIMIT.saturating_sub(st.queue.len() + st.flight_bytes);
                    if room > 0 {
                        let n = room.min(data.len());
                        st.queue.extend(&data[..n]);
                        inner.pump(&mut st);
                        return Ok(n);
                    }
                }
                _ => {}
            }
            st = inner.cv.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// An address the other side may send to: not this computer's own, nor a link's alone (an IPv6
/// one needs the link named), nor one only for documents.
pub(crate) fn usable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => !(v.is_loopback() || v.is_unspecified() || v.is_link_local() || v.is_broadcast() || v.is_multicast()),
        IpAddr::V6(v) => {
            let s = v.segments();
            let global = s[0] & 0xe000 == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8);
            let unique_local = s[0] & 0xfe00 == 0xfc00;
            global || unique_local
        }
    }
}

/// An address on a private network: a path to it does not cross the internet.
fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || (v.octets()[0] == 100 && v.octets()[1] & 0xc0 == 64),
        IpAddr::V6(v) => v.segments()[0] & 0xfe00 == 0xfc00,
    }
}

/// Which path the viewer names when several answer: on one network first, then IPv6 (no router
/// that may forget the mapping), then IPv4.
fn rank(a: &SocketAddr) -> u8 {
    match a.ip() {
        ip if private(ip) => 0,
        IpAddr::V6(_) => 1,
        IpAddr::V4(_) => 2,
    }
}

/// Asks the relay where it sees this socket from: the router's address and the port it gave.
fn reflect(sock: &UdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    let nonce = crate::sys::hex(&crate::sys::random(8));
    let mut ask = REFLECT.to_vec();
    ask.extend_from_slice(nonce.as_bytes());
    // Never smaller than the answer, so the relay cannot be used to amplify a flood.
    ask.resize(96, b' ');
    let mut buf = [0u8; 256];
    for _ in 0..3 {
        let _ = sock.send_to(&ask, server);
        let until = Instant::now() + Duration::from_millis(300);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            let _ = sock.set_read_timeout(Some(left.max(Duration::from_millis(1))));
            let Ok((n, from)) = sock.recv_from(&mut buf) else { break };
            if from.ip() == server.ip()
                && let Some(a) = parse_reflect(&buf[..n], &nonce)
            {
                return Some(a);
            }
        }
    }
    None
}

fn parse_reflect(b: &[u8], nonce: &str) -> Option<SocketAddr> {
    let t = std::str::from_utf8(b.strip_prefix(REFLECT)?).ok()?;
    let mut w = t.split_whitespace();
    (w.next()? == nonce).then_some(())?;
    w.next()?.parse().ok()
}

/// The relay's answer to a question `q` from `from`, or None for anything else.
#[cfg(test)]
pub fn reflect_answer(q: &[u8], from: SocketAddr) -> Option<Vec<u8>> {
    let rest = q.strip_prefix(REFLECT)?;
    let nonce: String = std::str::from_utf8(rest).ok()?.split_whitespace().next()?.chars().take(32).collect();
    (q.len() >= 64).then(|| format!("{}{nonce} {from}", String::from_utf8_lossy(REFLECT)).into_bytes())
}

/// This computer's own IPv4 addresses on its networks: the one it reaches the relay from first.
pub(crate) fn local_ips(toward: Option<SocketAddr>) -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Some(t) = toward
        && let Some(a) = source_toward(t)
    {
        out.push(a);
    }
    for a in interface_addrs().into_iter().filter(|a| a.is_ipv4()) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out.truncate(6);
    out
}

/// This computer's own IPv6 addresses: first the one it sends from to the internet (a temporary
/// one where those are used, which is what the other side's firewall then sees), then the rest.
fn local_ips6(toward: Option<SocketAddr>) -> Vec<IpAddr> {
    let mut out = Vec::new();
    // Choosing a source sends nothing: a documentation address stands for the internet.
    let t = toward.unwrap_or_else(|| SocketAddr::new(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1).into(), 9));
    if let Some(a) = source_toward(t) {
        out.push(a);
    }
    for a in interface_addrs().into_iter().filter(|a| a.is_ipv6()) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out.truncate(4);
    out
}

/// The address this computer would send from to `t`, if usable; found without sending.
fn source_toward(t: SocketAddr) -> Option<IpAddr> {
    let any: IpAddr = if t.is_ipv6() { Ipv6Addr::UNSPECIFIED.into() } else { Ipv4Addr::UNSPECIFIED.into() };
    let s = UdpSocket::bind((any, 0)).ok()?;
    s.connect(t).ok()?;
    let a = s.local_addr().ok()?.ip();
    usable(a).then_some(a)
}

/// The usable addresses of this computer's network cards that are up (not those of containers
/// and virtual machines it hosts).
fn interface_addrs() -> Vec<IpAddr> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut out = Vec::new();
    #[cfg(unix)]
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) == 0 {
            let mut at = list;
            while !at.is_null() {
                let i = &*at;
                let name = std::ffi::CStr::from_ptr(i.ifa_name).to_string_lossy();
                let inner = ["lo", "docker", "br-", "veth", "virbr", "lxc", "cni", "podman"].iter().any(|p| name.starts_with(p));
                let up = i.ifa_flags & libc::IFF_UP as u32 != 0;
                if up && !inner && !i.ifa_addr.is_null() {
                    let a: Option<IpAddr> = match (*i.ifa_addr).sa_family as i32 {
                        libc::AF_INET => Some(Ipv4Addr::from(u32::from_be((*(i.ifa_addr as *const libc::sockaddr_in)).sin_addr.s_addr)).into()),
                        libc::AF_INET6 => Some(Ipv6Addr::from((*(i.ifa_addr as *const libc::sockaddr_in6)).sin6_addr.s6_addr).into()),
                        _ => None,
                    };
                    if let Some(a) = a.filter(|a| usable(*a) && !out.contains(a)) {
                        out.push(a);
                    }
                }
                at = i.ifa_next;
            }
            libc::freeifaddrs(list);
        }
    }
    out
}

/// Room for a burst at full speed: the systems' defaults drop some at tens of megabytes a second.
fn bigger_buffers(sock: &UdpSocket) {
    let size: i32 = 4 << 20;
    #[cfg(unix)]
    unsafe {
        use std::os::fd::AsRawFd;
        for opt in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
            libc::setsockopt(sock.as_raw_fd(), libc::SOL_SOCKET, opt, &size as *const i32 as *const libc::c_void, 4);
        }
    }
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{SO_RCVBUF, SO_SNDBUF, SOL_SOCKET, setsockopt};
        for opt in [SO_RCVBUF, SO_SNDBUF] {
            setsockopt(sock.as_raw_socket() as usize, SOL_SOCKET, opt, &size as *const i32 as *const u8, 4);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn local(p: &Path) -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), p.inner.socks[0].local_addr().unwrap().port())
    }

    fn local6(p: &Path) -> Option<SocketAddr> {
        p.inner.socks.get(1).map(|s| SocketAddr::new(Ipv6Addr::LOCALHOST.into(), s.local_addr().unwrap().port()))
    }

    /// Whether IPv6 works on this computer at all (it can be turned off).
    fn ipv6_here() -> bool {
        let (Ok(a), Ok(b)) = (UdpSocket::bind("[::1]:0"), UdpSocket::bind("[::1]:0")) else { return false };
        let _ = b.set_read_timeout(Some(Duration::from_secs(1)));
        a.send_to(b"x", b.local_addr().unwrap()).is_ok() && b.recv_from(&mut [0u8; 4]).is_ok()
    }

    /// Both families answer: IPv6 is named, and the stream runs on it.
    #[test]
    fn ipv6_is_found_and_taken_before_ipv4() {
        if !ipv6_here() {
            return;
        }
        let (mut v, mut a) = (Path::bind(true).unwrap(), Path::bind(false).unwrap());
        let (Some(v6), Some(a6)) = (local6(&v), local6(&a)) else { return };
        v.local_relay = true;
        a.local_relay = true;
        let tag = [3u8; 8];
        a.punch(tag, None, &[local(&v), v6], Start::Second);
        v.punch(tag, None, &[local(&a), a6], Start::Now);
        assert_eq!(v.wait(Duration::from_secs(5)), Some(a6));
        assert_eq!(a.wait(Duration::from_secs(5)), Some(v6));
        let (mut vs, mut at) = (v.stream(), a.stream());
        vs.write_all(&pattern(200 << 10, 5)).unwrap();
        let mut got = vec![0u8; 200 << 10];
        at.read_exact(&mut got).unwrap();
        assert!(got == pattern(200 << 10, 5));
    }

    /// Only addresses another computer can send to are offered or tried.
    #[test]
    fn addresses_worth_trying() {
        let ok = |s: &str| usable(s.parse().unwrap());
        assert!(ok("203.0.113.7") && ok("192.168.1.20") && ok("2a01:4f8:241:134f::2") && ok("fd6a::2"));
        assert!(!ok("127.0.0.1") && !ok("169.254.3.4") && !ok("::1") && !ok("fe80::1") && !ok("2001:db8::1") && !ok("::ffff:1.2.3.4") && !ok("ff02::1"));
        let r = |s: &str| rank(&SocketAddr::new(s.parse().unwrap(), 1));
        assert!(r("192.168.1.20") < r("2a01:4f8::2") && r("fd6a::2") < r("2a01:4f8::2") && r("2a01:4f8::2") < r("203.0.113.7"));
    }

    /// Two sides on this computer find each other and agree on one path.
    fn pair(one_in: u32) -> (Stream, Stream) {
        let (mut v, mut a) = (Path::bind(true).unwrap(), Path::bind(false).unwrap());
        v.local_relay = true;
        a.local_relay = true;
        let (vl, al) = (local(&v), local(&a));
        let tag = [7u8; 8];
        // Every packet checked, as two current versions agree to.
        let key = Some([5u8; 32]);
        a.punch(tag, key, &[vl], Start::Second);
        v.punch(tag, key, &[al], Start::Now);
        assert_eq!(v.wait(Duration::from_secs(5)), Some(al));
        assert_eq!(a.wait(Duration::from_secs(5)), Some(vl));
        v.lossy(one_in);
        a.lossy(one_in);
        (v.stream(), a.stream())
    }

    fn pattern(n: usize, salt: u8) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8 ^ salt).collect()
    }

    /// Both ways at once, through a path that drops one packet in `one_in`: all of it, in order.
    fn both_ways(one_in: u32, size: usize) {
        let (v, a) = pair(one_in);
        let sender = |mut s: Stream, salt: u8| {
            thread::spawn(move || {
                let data = pattern(size, salt);
                for chunk in data.chunks(7919) {
                    s.write_all(chunk).unwrap();
                }
            })
        };
        let receiver = |mut s: Stream| {
            thread::spawn(move || {
                let mut got = vec![0u8; size];
                s.read_exact(&mut got).unwrap();
                got
            })
        };
        let (t1, t2) = (sender(v.clone(), 1), sender(a.clone(), 2));
        let (r1, r2) = (receiver(a.clone()), receiver(v.clone()));
        t1.join().unwrap();
        t2.join().unwrap();
        assert!(r1.join().unwrap() == pattern(size, 1), "viewer to agent");
        assert!(r2.join().unwrap() == pattern(size, 2), "agent to viewer");
    }

    #[test]
    fn a_stream_carries_everything_in_order() {
        both_ways(0, 3 << 20);
    }

    #[test]
    fn a_lossy_path_still_carries_everything_in_order() {
        both_ways(20, 2 << 20);
    }

    #[test]
    fn a_very_lossy_path_still_carries_everything_in_order() {
        both_ways(4, 300 << 10);
    }

    /// Let go, the other side reads what was written and then the end; ended, it reads the end.
    #[test]
    fn the_other_side_hears_the_end() {
        let (v, mut a) = pair(0);
        let mut w = v.clone();
        w.write_all(b"last words").unwrap();
        drop(w);
        drop(v);
        let mut got = Vec::new();
        a.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"last words");
        let (v, mut a) = pair(0);
        v.end();
        let mut b = [0u8; 4];
        assert_eq!(a.read(&mut b).unwrap(), 0);
        assert!(a.write_all(b"x").is_err());
        // Finished while copies are still held (a reader waiting elsewhere): what was written
        // still arrives, then the end.
        let (v, mut a) = pair(0);
        let mut w = v.clone();
        let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        w.write_all(&big).unwrap();
        v.finish();
        let mut got = Vec::new();
        a.read_to_end(&mut got).unwrap();
        assert_eq!(got, big);
    }

    /// A read with an idle limit fails once nothing new comes for that long.
    #[test]
    fn a_silent_path_times_out_a_read() {
        let (v, mut a) = pair(0);
        a.set_idle(Duration::from_millis(400));
        let started = Instant::now();
        let mut b = [0u8; 4];
        assert_eq!(a.read(&mut b).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(390));
        drop(v);
    }

    /// A stranger's packets, without the tag, are not taken.
    #[test]
    fn packets_without_the_tag_are_dropped() {
        let (v, mut a) = pair(0);
        let a_addr = match v.0.0.lock().phase {
            Phase::Up(to) => to,
            _ => unreachable!(),
        };
        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut p = vec![DATA, VIEWER];
        p.extend_from_slice(&[9u8; 8]);
        p.extend_from_slice(&0u32.to_le_bytes());
        p.extend_from_slice(b"forged");
        stranger.send_to(&p, a_addr).unwrap();
        let mut w = v.clone();
        w.write_all(b"real").unwrap();
        let mut got = [0u8; 4];
        a.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"real");
    }

    /// With a key, a packet from anyone without it (who saw the tag go by, as a machine the
    /// probes reached does) is not taken: a made-up answer to a probe names no path, where
    /// without a key it would.
    #[test]
    fn packets_without_the_key_are_not_taken() {
        for key in [None, Some([9u8; 32])] {
            let mut v = Path::bind(true).unwrap();
            v.local_relay = true;
            let forger = UdpSocket::bind("127.0.0.1:0").unwrap();
            forger.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            v.punch([4; 8], key, &[forger.local_addr().unwrap()], Start::Now);
            let mut b = [0u8; 1500];
            let (n, from) = forger.recv_from(&mut b).unwrap();
            assert_eq!((b[0], n), (PROBE, HEAD + 4 + if key.is_some() { MAC } else { 0 }));
            // Its answer as one who saw the probe would make it: the tag, the probe's number,
            // and (it has nothing better) the probe's own check.
            let mut ack = vec![PROBE_ACK, 1];
            ack.extend_from_slice(&b[2..HEAD + 4]);
            if key.is_some() {
                ack.extend_from_slice(&b[n - MAC..n]);
            }
            forger.send_to(&ack, from).unwrap();
            thread::sleep(Duration::from_millis(300));
            let taken = v.inner.lock().working.contains(&forger.local_addr().unwrap());
            assert_eq!(taken, key.is_none(), "{key:?}");
        }
    }

    /// Nobody answering: the attempt ends, in its time.
    #[test]
    fn nobody_there_is_no_path() {
        let mut v = Path::bind(true).unwrap();
        v.local_relay = true;
        let nowhere = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        v.punch([1; 8], None, &[nowhere], Start::Now);
        let started = Instant::now();
        assert_eq!(v.wait(PUNCH_LIMIT + Duration::from_secs(2)), None);
        assert!(started.elapsed() >= PUNCH_LIMIT - Duration::from_millis(100));
    }

    /// The relay's answer reads back as the address it saw.
    #[test]
    fn the_relay_says_where_it_sees_a_socket_from() {
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        let at = relay.local_addr().unwrap();
        thread::spawn(move || {
            let mut b = [0u8; 256];
            let (n, from) = relay.recv_from(&mut b).unwrap();
            let answer = reflect_answer(&b[..n], from).unwrap();
            assert!(answer.len() <= n, "never larger than the question");
            relay.send_to(&answer, from).unwrap();
        });
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert_eq!(reflect(&s, at), Some(s.local_addr().unwrap()));
    }
}
