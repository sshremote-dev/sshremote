//! Wire messages. Client to agent is plain framing; agent to client is the same
//! framing inside one long-lived zstd stream that is flushed after each message.

use std::io::{self, Read, Write};

use crate::color::Mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub view_w: u16,
    pub view_h: u16,
    pub scale: u8,
    pub mode: Mode,
    pub fps: u8,
}

pub struct Preset {
    pub name: &'static str,
    pub scale: u8,
    pub mode: Mode,
    pub fps: u8,
}

pub const PRESETS: [Preset; 5] = [
    Preset { name: "P0 glance", scale: 50, mode: Mode::Mono, fps: 5 },
    Preset { name: "P1 lean", scale: 75, mode: Mode::Gray16, fps: 10 },
    Preset { name: "P2 balanced", scale: 100, mode: Mode::Color256, fps: 20 },
    Preset { name: "P3 crisp", scale: 100, mode: Mode::True, fps: 30 },
    Preset { name: "P4 exact", scale: 100, mode: Mode::True, fps: 60 },
];

pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_SUPER: u8 = 8;

#[derive(Debug, PartialEq)]
pub enum ToAgent {
    Settings(Settings),
    Ack(u32),
    Key {
        keysym: u32,
        mods: u8,
    },
    Pointer {
        x: u16,
        y: u16,
    },
    Button {
        button: u8,
        pressed: bool,
    },
    Scroll {
        steps: i8,
    },
    Refresh,
    /// Echoed back as `Pong` behind any queued frames, so its round trip shows queueing delay.
    Ping(u32),
}

#[derive(Debug, PartialEq)]
pub struct Tile {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub struct Frame {
    pub id: u32,
    pub w: u16,
    pub h: u16,
    pub mode: Mode,
    pub tiles: Vec<Tile>,
}

#[derive(Debug, PartialEq)]
pub enum ToClient {
    Hello { w: u16, h: u16, backend: String },
    Frame(Frame),
    Error(String),
    Pong(u32),
}

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed message")
}

struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let s = self.d.get(self.p..self.p + n).ok_or_else(bad)?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn mode(&mut self) -> io::Result<Mode> {
        Mode::from_u8(self.u8()?).ok_or_else(bad)
    }
    fn rest(&mut self) -> &'a [u8] {
        let s = &self.d[self.p..];
        self.p = self.d.len();
        s
    }
}

pub fn write_msg(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = [kind, 0, 0, 0, 0];
    head[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&head)?;
    w.write_all(payload)
}

/// Returns `None` on a clean end of stream.
pub fn read_msg(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        r => r?,
    }
    let len = u32::from_le_bytes(head[1..].try_into().unwrap()) as usize;
    if len > 256 << 20 {
        return Err(bad());
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some((head[0], payload)))
}

impl ToAgent {
    pub fn encode(&self) -> (u8, Vec<u8>) {
        let mut b = Vec::new();
        let kind = match self {
            ToAgent::Settings(s) => {
                b.extend_from_slice(&s.view_w.to_le_bytes());
                b.extend_from_slice(&s.view_h.to_le_bytes());
                b.extend_from_slice(&[s.scale, s.mode.to_u8(), s.fps]);
                1
            }
            ToAgent::Ack(id) => {
                b.extend_from_slice(&id.to_le_bytes());
                2
            }
            ToAgent::Key { keysym, mods } => {
                b.extend_from_slice(&keysym.to_le_bytes());
                b.push(*mods);
                3
            }
            ToAgent::Pointer { x, y } => {
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
                4
            }
            ToAgent::Button { button, pressed } => {
                b.extend_from_slice(&[*button, *pressed as u8]);
                5
            }
            ToAgent::Scroll { steps } => {
                b.push(*steps as u8);
                6
            }
            ToAgent::Refresh => 7,
            ToAgent::Ping(t) => {
                b.extend_from_slice(&t.to_le_bytes());
                8
            }
        };
        (kind, b)
    }

    pub fn decode(kind: u8, d: &[u8]) -> io::Result<ToAgent> {
        let mut c = Cur { d, p: 0 };
        Ok(match kind {
            1 => ToAgent::Settings(Settings {
                view_w: c.u16()?,
                view_h: c.u16()?,
                scale: c.u8()?,
                mode: c.mode()?,
                fps: c.u8()?,
            }),
            2 => ToAgent::Ack(c.u32()?),
            3 => ToAgent::Key { keysym: c.u32()?, mods: c.u8()? },
            4 => ToAgent::Pointer { x: c.u16()?, y: c.u16()? },
            5 => ToAgent::Button { button: c.u8()?, pressed: c.u8()? != 0 },
            6 => ToAgent::Scroll { steps: c.u8()? as i8 },
            7 => ToAgent::Refresh,
            8 => ToAgent::Ping(c.u32()?),
            _ => return Err(bad()),
        })
    }
}

impl ToClient {
    pub fn encode(&self) -> (u8, Vec<u8>) {
        let mut b = Vec::new();
        let kind = match self {
            ToClient::Hello { w, h, backend } => {
                b.extend_from_slice(&w.to_le_bytes());
                b.extend_from_slice(&h.to_le_bytes());
                b.extend_from_slice(backend.as_bytes());
                101
            }
            ToClient::Frame(f) => {
                b.extend_from_slice(&f.id.to_le_bytes());
                b.extend_from_slice(&f.w.to_le_bytes());
                b.extend_from_slice(&f.h.to_le_bytes());
                b.push(f.mode.to_u8());
                b.extend_from_slice(&(f.tiles.len() as u32).to_le_bytes());
                for t in &f.tiles {
                    for v in [t.x, t.y, t.w, t.h] {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                    b.extend_from_slice(&(t.data.len() as u32).to_le_bytes());
                    b.extend_from_slice(&t.data);
                }
                102
            }
            ToClient::Error(e) => {
                b.extend_from_slice(e.as_bytes());
                103
            }
            ToClient::Pong(t) => {
                b.extend_from_slice(&t.to_le_bytes());
                104
            }
        };
        (kind, b)
    }

    pub fn decode(kind: u8, d: &[u8]) -> io::Result<ToClient> {
        let mut c = Cur { d, p: 0 };
        Ok(match kind {
            101 => {
                ToClient::Hello { w: c.u16()?, h: c.u16()?, backend: String::from_utf8_lossy(c.rest()).into_owned() }
            }
            102 => {
                let (id, w, h, mode) = (c.u32()?, c.u16()?, c.u16()?, c.mode()?);
                let n = c.u32()? as usize;
                let mut tiles = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    let (x, y, tw, th) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
                    let len = c.u32()? as usize;
                    let data = c.take(len)?.to_vec();
                    if x as usize + tw as usize > w as usize || y as usize + th as usize > h as usize {
                        return Err(bad());
                    }
                    tiles.push(Tile { x, y, w: tw, h: th, data });
                }
                ToClient::Frame(Frame { id, w, h, mode, tiles })
            }
            103 => ToClient::Error(String::from_utf8_lossy(c.rest()).into_owned()),
            104 => ToClient::Pong(c.u32()?),
            _ => return Err(bad()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let msgs = [
            ToAgent::Settings(Settings { view_w: 1900, view_h: 1000, scale: 75, mode: Mode::Gray16, fps: 10 }),
            ToAgent::Ack(7),
            ToAgent::Key { keysym: 0xff0d, mods: MOD_CTRL },
            ToAgent::Pointer { x: 3, y: 4 },
            ToAgent::Button { button: 1, pressed: true },
            ToAgent::Scroll { steps: -2 },
            ToAgent::Refresh,
            ToAgent::Ping(5),
        ];
        for m in msgs {
            let (k, p) = m.encode();
            assert_eq!(ToAgent::decode(k, &p).unwrap(), m);
        }
        let f = ToClient::Frame(Frame {
            id: 9,
            w: 64,
            h: 32,
            mode: Mode::Mono,
            tiles: vec![Tile { x: 1, y: 2, w: 3, h: 4, data: vec![1, 2] }],
        });
        let mut wire = Vec::new();
        let (k, p) = f.encode();
        write_msg(&mut wire, k, &p).unwrap();
        let (k, p) = read_msg(&mut wire.as_slice()).unwrap().unwrap();
        assert_eq!(ToClient::decode(k, &p).unwrap(), f);
    }
}
