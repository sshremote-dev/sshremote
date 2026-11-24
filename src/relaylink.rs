//! A computer's line to the relay, sealed (`access::run`): the computer says `secure E` (a fresh X25519 key),
//! the relay answers `secure E' S` (its own fresh key and its lasting one), and from then on each
//! side's words go in frames of ChaCha20-Poly1305 under keys drawn (HKDF-SHA256) from both
//! exchanges. Only the holder of S can read the computer's token or write the lines a computer
//! acts on (`from`, `call`); a computer that knows S (it keeps it from the first time, or ships
//! with the public relay's) sees through anyone in between. The same code is in sshremote-relay.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use hkdf::Hkdf;
use sha2::Sha256;

use std::io::{self, Read, Write};
use std::net::TcpStream;

/// The longest frame's words.
pub const MOST: usize = 16 << 10;

/// A key pair: the secret (kept here), and the public half to send.
pub fn keypair(secret: [u8; 32]) -> ([u8; 32], [u8; 32]) {
    (secret, MontgomeryPoint::mul_base_clamped(secret).to_bytes())
}

fn dh(secret: &[u8; 32], public: &[u8; 32]) -> Option<[u8; 32]> {
    let shared = MontgomeryPoint(*public).mul_clamped(*secret).to_bytes();
    // A point of small order makes it known to anyone: refused.
    (shared != [0; 32]).then_some(shared)
}

/// A key of 32 bytes from its 64 hex digits.
pub fn key_from_hex(s: &str) -> Option<[u8; 32]> {
    crate::sys::unhex(s)?.try_into().ok()
}

/// One direction's sealing: a key and the count of frames so far (each frame's nonce).
pub struct Way {
    aead: ChaCha20Poly1305,
    count: u64,
}

impl Way {
    fn new(key: &[u8]) -> Way {
        Way { aead: ChaCha20Poly1305::new(Key::from_slice(key)), count: 0 }
    }

    fn nonce(&mut self) -> Nonce {
        let mut n = [0u8; 12];
        n[..8].copy_from_slice(&self.count.to_le_bytes());
        self.count += 1;
        *Nonce::from_slice(&n)
    }

    /// `words` as a frame: its length (2 bytes), then them sealed.
    pub fn seal(&mut self, words: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in words.chunks(MOST).chain(words.is_empty().then_some(&[][..])) {
            let nonce = self.nonce();
            let sealed = self.aead.encrypt(&nonce, part).expect("sealing in memory");
            out.extend_from_slice(&(sealed.len() as u16).to_be_bytes());
            out.extend_from_slice(&sealed);
        }
        out
    }

    /// The next frame's words from the front of `raw`, once it is all there; an error for one
    /// that was not sealed by the other side (the line is then given up).
    pub fn open(&mut self, raw: &mut Vec<u8>) -> Result<Option<Vec<u8>>, ()> {
        if raw.len() < 2 {
            return Ok(None);
        }
        let len = u16::from_be_bytes([raw[0], raw[1]]) as usize;
        if !(16..=MOST + 16).contains(&len) {
            return Err(());
        }
        if raw.len() < 2 + len {
            return Ok(None);
        }
        let frame: Vec<u8> = raw.drain(..2 + len).skip(2).collect();
        let nonce = self.nonce();
        self.aead.decrypt(&nonce, frame.as_slice()).map(Some).map_err(|_| ())
    }
}

/// Both directions of a sealed line.
pub struct Sealed {
    pub out: Way,
    pub inn: Way,
}

/// Keys from both exchanges: the computer's fresh key with the relay's lasting one, and with
/// its fresh one. `relay`: this side is the relay.
fn sealed(dh1: [u8; 32], dh2: [u8; 32], e_agent: &[u8; 32], e_relay: &[u8; 32], s_relay: &[u8; 32], relay: bool) -> Sealed {
    let hk = Hkdf::<Sha256>::new(Some(b"sshremote relay link 1"), &[dh1, dh2].concat());
    let mut okm = [0u8; 64];
    hk.expand(&[&e_agent[..], &e_relay[..], &s_relay[..]].concat(), &mut okm).expect("64 bytes");
    let (to_relay, to_agent) = okm.split_at(32);
    if relay {
        Sealed { out: Way::new(to_agent), inn: Way::new(to_relay) }
    } else {
        Sealed { out: Way::new(to_relay), inn: Way::new(to_agent) }
    }
}

/// The relay's side (in sshremote-relay; here for the tests): the computer's fresh key `e_agent` and the relay's lasting pair; the line
/// to answer with, and the sealing.
#[cfg(test)]
pub fn answer(e_agent: &[u8; 32], lasting: &([u8; 32], [u8; 32]), fresh: [u8; 32]) -> Option<(String, Sealed)> {
    let (e_secret, e_relay) = keypair(fresh);
    let dh1 = dh(&lasting.0, e_agent)?;
    let dh2 = dh(&e_secret, e_agent)?;
    let line = format!("secure {} {}", hex(&e_relay), hex(&lasting.1));
    Some((line, sealed(dh1, dh2, e_agent, &e_relay, &lasting.1, true)))
}

/// The computer's side: its fresh pair and the relay's answer (its fresh and lasting keys).
pub fn finish(mine: &([u8; 32], [u8; 32]), e_relay: &[u8; 32], s_relay: &[u8; 32]) -> Option<Sealed> {
    let dh1 = dh(&mine.0, s_relay)?;
    let dh2 = dh(&mine.0, e_relay)?;
    Some(sealed(dh1, dh2, &mine.1, e_relay, s_relay, false))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}


/// The relay's lasting key the computer knows: the public relay's, shipped with it.
pub const PUBLIC_RELAY_KEY: &str = "be1fa6089cb9a4dcb2cdd89c2e6ae69d1fc79c5db73d41263c180db71e02a002";

/// A line to the relay, sealed or (an older relay) not: read and written as the stream it wraps.
pub struct Link {
    s: TcpStream,
    seal: Option<Sealed>,
    raw: Vec<u8>,
    words: std::collections::VecDeque<u8>,
    /// What is being written, sealed a line at a time (a line is written in pieces).
    out: Vec<u8>,
}

/// How sealing the line went.
// One per connection, handed on at once: no need to box the link.
#[allow(clippy::large_enum_variant)]
pub enum Outcome {
    /// Sealed, with the relay's lasting key.
    Sealed(Link, [u8; 32]),
    /// The relay did not take it (one before 1.3): the line is gone, another is needed.
    Older,
}

impl Link {
    pub fn plain(s: TcpStream) -> Link {
        Link { s, seal: None, raw: Vec::new(), words: Default::default(), out: Vec::new() }
    }

    /// Seals `s`: the keys exchanged, the relay's lasting key checked against `known` (when one
    /// is known it must be the one).
    pub fn seal(mut s: TcpStream, known: Option<[u8; 32]>) -> io::Result<Outcome> {
        let mut secret = [0u8; 32];
        getrandom::getrandom(&mut secret).map_err(|e| io::Error::other(e.to_string()))?;
        let mine = keypair(secret);
        writeln!(s, "secure {}", hex(&mine.1))?;
        let answer = match crate::access::line(&mut s) {
            Ok(a) => a,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Err(e),
            Err(_) => return Ok(Outcome::Older),
        };
        let words: Vec<&str> = answer.split(' ').collect();
        let (Some(e), Some(lasting)) = (words.get(1).and_then(|w| key_from_hex(w)), words.get(2).and_then(|w| key_from_hex(w))) else {
            return if words.first() == Some(&"secure") { Err(io::Error::other("a garbled answer")) } else { Ok(Outcome::Older) };
        };
        if known.is_some_and(|k| k != lasting) {
            return Err(io::Error::other("the relay showed another key than the one it is known by"));
        }
        let seal = finish(&mine, &e, &lasting).ok_or_else(|| io::Error::other("a weak key"))?;
        Ok(Outcome::Sealed(Link { s, seal: Some(seal), raw: Vec::new(), words: Default::default(), out: Vec::new() }, lasting))
    }

    pub fn set_read_timeout(&self, d: Option<std::time::Duration>) -> io::Result<()> {
        self.s.set_read_timeout(d)
    }

    pub fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        self.s.shutdown(how)
    }
}

impl Read for Link {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(seal) = &mut self.seal else { return self.s.read(buf) };
        loop {
            if !self.words.is_empty() {
                let n = buf.len().min(self.words.len());
                for (b, w) in buf.iter_mut().zip(self.words.drain(..n)) {
                    *b = w;
                }
                return Ok(n);
            }
            match seal.inn.open(&mut self.raw) {
                Ok(Some(w)) => {
                    self.words.extend(w);
                    continue;
                }
                Ok(None) => {}
                Err(()) => return Err(io::Error::new(io::ErrorKind::InvalidData, "a frame not sealed by the relay")),
            }
            let mut b = [0u8; 4096];
            let n = self.s.read(&mut b)?;
            if n == 0 {
                return Ok(0);
            }
            self.raw.extend_from_slice(&b[..n]);
        }
    }
}

impl Write for Link {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(seal) = &mut self.seal else { return self.s.write(buf) };
        self.out.extend_from_slice(buf);
        // Whole lines go, each in a frame of its own.
        if let Some(at) = self.out.iter().rposition(|b| *b == b'\n') {
            let lines: Vec<u8> = self.out.drain(..=at).collect();
            self.s.write_all(&seal.out.seal(&lines))?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(seal) = &mut self.seal
            && !self.out.is_empty()
        {
            let rest = std::mem::take(&mut self.out);
            self.s.write_all(&seal.out.seal(&rest))?;
        }
        self.s.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_seal_alike_and_a_forger_is_refused() {
        let lasting = keypair([7; 32]);
        let agent = keypair([9; 32]);
        let (line, mut relay) = answer(&agent.1, &lasting, [5; 32]).unwrap();
        let words: Vec<&str> = line.split(' ').collect();
        let (e_relay, s_relay) = (key_from_hex(words[1]).unwrap(), key_from_hex(words[2]).unwrap());
        assert_eq!(s_relay, lasting.1);
        let mut comp = finish(&agent, &e_relay, &s_relay).unwrap();
        // Several frames, in order, and a long one in parts.
        let mut raw = comp.out.seal(b"listen 123456 token");
        raw.extend(comp.out.seal(&vec![b'x'; MOST + 10]));
        assert_eq!(relay.inn.open(&mut raw).unwrap().unwrap(), b"listen 123456 token");
        assert_eq!(relay.inn.open(&mut raw).unwrap().unwrap().len(), MOST);
        assert_eq!(relay.inn.open(&mut raw).unwrap().unwrap().len(), 10);
        let mut back = relay.out.seal(b"ok\n");
        assert_eq!(comp.inn.open(&mut back).unwrap().unwrap(), b"ok\n");
        // Half a frame waits; a changed byte is refused.
        let mut part = relay.out.seal(b"ping\n");
        let rest = part.split_off(5);
        assert_eq!(comp.inn.open(&mut part).unwrap(), None);
        part.extend(rest);
        let n = part.len();
        part[n - 1] ^= 1;
        assert!(comp.inn.open(&mut part).is_err());
        // Someone else's lasting key: the computer's keys differ, nothing opens.
        let other = keypair([8; 32]);
        let mut fake = finish(&agent, &e_relay, &other.1).unwrap();
        let mut raw = fake.out.seal(b"listen");
        assert!(relay.inn.open(&mut raw).is_err());
        // A point of small order is refused.
        assert!(answer(&[0; 32], &lasting, [5; 32]).is_none());
    }
}
