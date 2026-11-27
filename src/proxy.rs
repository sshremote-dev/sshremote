//! Connections out through a proxy, for networks that let nothing else out: SOCKS5 (with a name
//! and password or without) or an HTTP proxy's CONNECT. Set as a URL, `socks5://host:1080` or
//! `http://name:password@host:8080`; computers on this network are reached without it.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Socks5,
    Http,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proxy {
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    /// A name and password it wants, if any.
    pub login: Option<(String, String)>,
}

/// The password as shown.
const HIDDEN: &str = "***";

/// The longest the proxy may take to open the way.
const OPEN_LIMIT: Duration = Duration::from_secs(15);

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

impl Proxy {
    /// `socks5://[name:password@]host[:port]` (`socks5h://` too; port 1080 unless given) or
    /// `http://[name:password@]host[:port]` (port 8080); the name and password may be escaped
    /// as in a URL (%40 for @).
    pub fn parse(url: &str) -> Result<Proxy, String> {
        let url = url.trim();
        let (scheme, rest) = url.split_once("://").ok_or(n!("a proxy is written socks5://host:port or http://host:port"))?;
        let (kind, default_port) = match scheme.to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" | "socks" => (Kind::Socks5, 1080),
            "http" => (Kind::Http, 8080),
            "https" => return Err(n!("a proxy spoken to over TLS (https://) is not supported: use its http:// or socks5:// address").into()),
            other => return Err(crate::lang::fill(n!("{}:// is no kind of proxy known here: socks5:// or http://"), &[&other])),
        };
        let rest = rest.trim_end_matches('/');
        let (login, place) = match rest.rsplit_once('@') {
            Some((l, p)) => {
                let (name, pw) = l.split_once(':').unwrap_or((l, ""));
                (Some((unescape(name), unescape(pw))), p)
            }
            None => (None, rest),
        };
        let (host, port) = match place.strip_prefix('[') {
            // [v6]:port
            Some(v6) => {
                let (h, after) = v6.split_once(']').ok_or(n!("an IPv6 address needs its closing ]"))?;
                if !after.is_empty() && !after.starts_with(':') {
                    return Err(crate::lang::fill(n!("{} after the IPv6 address is no port"), &[&after]));
                }
                (h.to_string(), after.strip_prefix(':').map(|p| p.parse::<u16>().map_err(|_| crate::lang::fill(n!("{} is no port"), &[&p]))).transpose()?)
            }
            None => match place.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p.parse::<u16>().map_err(|_| crate::lang::fill(n!("{} is no port"), &[&p]))?)),
                None => (place.to_string(), None),
            },
        };
        if host.is_empty() {
            return Err(n!("the proxy's address is missing").into());
        }
        Ok(Proxy { kind, host, port: port.unwrap_or(default_port), login })
    }

    /// The URL again, as curl takes it (names resolved by the proxy, as here).
    pub fn url(&self) -> String {
        self.written(true)
    }

    /// The same, the password left out, to show.
    pub fn shown(&self) -> String {
        self.written(false)
    }

    /// Typed back as shown (the password as ***), the password it had `before` stays.
    pub fn keeping(mut self, before: Option<&Proxy>) -> Proxy {
        if let (Some((name, pw)), Some(Proxy { login: Some((was_name, was_pw)), .. })) = (&mut self.login, before)
            && pw == HIDDEN
            && name == was_name
        {
            *pw = was_pw.clone();
        }
        self
    }

    fn written(&self, whole: bool) -> String {
        let scheme = match (self.kind, whole) {
            (Kind::Socks5, true) => "socks5h",
            (Kind::Socks5, false) => "socks5",
            (Kind::Http, _) => "http",
        };
        let login = match &self.login {
            Some((n, p)) if whole || p.is_empty() => format!("{}:{}@", escape(n), escape(p)),
            Some((n, _)) => format!("{}:{HIDDEN}@", escape(n)),
            None => String::new(),
        };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        format!("{scheme}://{login}{host}:{}", self.port)
    }

    /// A connection to `host`:`port` through the proxy; the proxy looks the name up.
    pub fn connect(&self, host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
        let mut last = io::Error::other(n!("the proxy's address has no address"));
        let mut s = None;
        for a in std::net::ToSocketAddrs::to_socket_addrs(&(self.host.as_str(), self.port))? {
            match TcpStream::connect_timeout(&a, timeout) {
                Ok(c) => {
                    s = Some(c);
                    break;
                }
                Err(e) => last = e,
            }
        }
        let mut s = s.ok_or_else(|| io::Error::new(last.kind(), crate::lang::fill(n!("the proxy {}:{} does not answer: {}"), &[&self.host, &self.port, &last])))?;
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(OPEN_LIMIT))?;
        s.set_write_timeout(Some(OPEN_LIMIT))?;
        match self.kind {
            Kind::Socks5 => self.socks5(&mut s, host, port)?,
            Kind::Http => self.http(&mut s, host, port)?,
        }
        s.set_read_timeout(None)?;
        s.set_write_timeout(None)?;
        Ok(s)
    }

    fn socks5(&self, s: &mut TcpStream, host: &str, port: u16) -> io::Result<()> {
        let refused = |why: String| io::Error::new(io::ErrorKind::PermissionDenied, why);
        s.write_all(if self.login.is_some() { &[5, 2, 0, 2] } else { &[5, 1, 0] })?;
        let mut r = [0u8; 2];
        s.read_exact(&mut r)?;
        if r[0] != 5 {
            return Err(io::Error::other(crate::lang::fill(n!("{}:{} is no SOCKS5 proxy"), &[&self.host, &self.port])));
        }
        match (r[1], &self.login) {
            (0, _) => {}
            (2, Some((name, pw))) => {
                if name.len() > 255 || pw.len() > 255 {
                    return Err(io::Error::other(n!("the proxy's name or password is too long for SOCKS5")));
                }
                let mut m = vec![1, name.len() as u8];
                m.extend_from_slice(name.as_bytes());
                m.push(pw.len() as u8);
                m.extend_from_slice(pw.as_bytes());
                s.write_all(&m)?;
                s.read_exact(&mut r)?;
                if r[1] != 0 {
                    return Err(refused(n!("the proxy did not take its name and password").into()));
                }
            }
            (2, None) => return Err(refused(n!("the proxy wants a name and password: socks5://name:password@host:port").into())),
            _ => return Err(refused(n!("the proxy takes no way of signing in this program knows").into())),
        }
        let mut m = vec![5, 1, 0];
        match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(a)) => {
                m.push(1);
                m.extend_from_slice(&a.octets());
            }
            Ok(std::net::IpAddr::V6(a)) => {
                m.push(4);
                m.extend_from_slice(&a.octets());
            }
            Err(_) if host.len() <= 255 => {
                m.extend_from_slice(&[3, host.len() as u8]);
                m.extend_from_slice(host.as_bytes());
            }
            Err(_) => return Err(io::Error::other(n!("the name is too long for SOCKS5"))),
        }
        m.extend_from_slice(&port.to_be_bytes());
        s.write_all(&m)?;
        let mut head = [0u8; 4];
        s.read_exact(&mut head)?;
        if head[1] != 0 {
            let why = match head[1] {
                2 => n!("the proxy does not let it through ({}:{})"),
                3 => n!("the proxy finds no way to that network ({}:{})"),
                4 => n!("the proxy cannot reach that computer ({}:{})"),
                5 => n!("the proxy was refused by that computer ({}:{})"),
                6 => n!("the proxy timed out on the way ({}:{})"),
                7 | 8 => n!("the proxy does not take such a request ({}:{})"),
                _ => n!("the proxy failed ({}:{})"),
            };
            return Err(refused(crate::lang::fill(why, &[&host, &port])));
        }
        // The address it bound, read past.
        let rest = match head[3] {
            1 => 4,
            4 => 16,
            3 => {
                let mut n = [0u8; 1];
                s.read_exact(&mut n)?;
                n[0] as usize
            }
            _ => return Err(io::Error::other(n!("the proxy's answer is not SOCKS5"))),
        };
        let mut skip = vec![0u8; rest + 2];
        s.read_exact(&mut skip)
    }

    fn http(&self, s: &mut TcpStream, host: &str, port: u16) -> io::Result<()> {
        use base64::Engine;
        let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
        let mut ask = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some((name, pw)) = &self.login {
            let both = base64::engine::general_purpose::STANDARD.encode(format!("{name}:{pw}"));
            ask.push_str(&format!("Proxy-Authorization: Basic {both}\r\n"));
        }
        ask.push_str("\r\n");
        s.write_all(ask.as_bytes())?;
        // Byte by byte up to the empty line: what follows is the tunnel's.
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if s.read(&mut b)? == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, n!("the proxy hung up")));
            }
            head.push(b[0]);
            if head.len() > 16 << 10 {
                return Err(io::Error::other(n!("the proxy's answer goes on and on")));
            }
        }
        let text = String::from_utf8_lossy(&head);
        let status = text.lines().next().unwrap_or("");
        let code = status.split_whitespace().nth(1).unwrap_or("");
        match code {
            c if c.starts_with('2') => Ok(()),
            "407" if self.login.is_some() => Err(io::Error::new(io::ErrorKind::PermissionDenied, n!("the proxy did not take its name and password"))),
            "407" => Err(io::Error::new(io::ErrorKind::PermissionDenied, n!("the proxy wants a name and password: http://name:password@host:port"))),
            _ => Err(io::Error::new(io::ErrorKind::PermissionDenied, crate::lang::fill(n!("the proxy did not open the way to {}: {}"), &[&target, &status.trim()]))),
        }
    }
}

/// The proxy set for this program, if any: the person's own setting, else the one remote access
/// keeps (which a service run as SYSTEM reads: it has no person's settings). One that cannot be
/// read is an error, never a way around it.
pub fn current() -> Result<Option<Proxy>, String> {
    let set = crate::hosts::setting_now("proxy").filter(|p| !p.trim().is_empty());
    #[cfg(feature = "remote")]
    let set = set.or_else(|| crate::access::stored().ok().flatten().map(|c| c.proxy).filter(|p| !p.trim().is_empty()));
    set.map(|s| Proxy::parse(&s).map_err(|e| crate::lang::fill(n!("the proxy set cannot be used ({})"), &[&e]))).transpose()
}

/// Whether nothing is to go through a proxy (one that cannot be read counts as set).
pub fn none() -> bool {
    matches!(current(), Ok(None))
}

/// Whether `host` is this computer, reached without the proxy.
fn here(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|a| a.is_loopback())
}

/// A connection to `host`:`port`: through the proxy when one is set (this computer itself
/// straight), else straight.
pub fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    if !here(host)
        && let Some(p) = current().map_err(io::Error::other)?
    {
        return p.connect(host, port, timeout);
    }
    let mut last = io::Error::other("no address");
    for a in std::net::ToSocketAddrs::to_socket_addrs(&(host, port))? {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn proxies_are_read_from_their_urls() {
        let p = Proxy::parse("socks5://proxy.example").unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port, p.login.is_none()), (Kind::Socks5, "proxy.example", 1080, true));
        let p = Proxy::parse(" http://me%40work:p%3Ass@10.0.0.1:3128/ ").unwrap();
        assert_eq!((p.kind, p.port), (Kind::Http, 3128));
        assert_eq!(p.login, Some(("me@work".into(), "p:ss".into())));
        assert_eq!(p.url(), "http://me%40work:p%3Ass@10.0.0.1:3128");
        assert_eq!(p.shown(), "http://me%40work:***@10.0.0.1:3128");
        // Shown in Settings without its password, and used again as it was.
        assert_eq!(Proxy::parse(&p.shown()).unwrap().keeping(Some(&p)), p);
        assert_eq!(Proxy::parse("http://you:***@10.0.0.1:3128").unwrap().keeping(Some(&p)).login, Some(("you".into(), "***".into())));
        assert!(Proxy::parse("socks5://[::1]x").is_err());
        assert_eq!(Proxy::parse("socks5h://[::1]:9050").unwrap().url(), "socks5h://[::1]:9050");
        assert!(here("localhost") && here("127.0.0.1") && here("[::1]") && !here("sshremote.dev") && !here("192.168.1.2"));
        assert!(Proxy::parse("https://p:443").is_err() && Proxy::parse("ftp://p").is_err() && Proxy::parse("p:1080").is_err() && Proxy::parse("http://p:x").is_err());
    }

    /// Something to reach, answering what it is sent in capitals.
    fn shouter() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut b = [0u8; 64];
                let n = c.read(&mut b).unwrap_or(0);
                let _ = c.write_all(&b[..n].to_ascii_uppercase());
            }
        });
        port
    }

    /// A SOCKS5 proxy that wants `login`, or an HTTP one, for one connection: it checks what it
    /// is told and joins the two.
    fn proxy(kind: Kind, login: Option<(&'static str, &'static str)>) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let byte = |c: &mut TcpStream, n: usize| {
                let mut b = vec![0u8; n];
                c.read_exact(&mut b).unwrap();
                b
            };
            let target = match kind {
                Kind::Socks5 => {
                    let head = byte(&mut c, 2);
                    let methods = byte(&mut c, head[1] as usize);
                    if let Some((name, pw)) = login {
                        assert!(methods.contains(&2));
                        c.write_all(&[5, 2]).unwrap();
                        let v = byte(&mut c, 2);
                        let n = String::from_utf8(byte(&mut c, v[1] as usize)).unwrap();
                        let l = byte(&mut c, 1);
                        let p = String::from_utf8(byte(&mut c, l[0] as usize)).unwrap();
                        let ok = n == name && p == pw;
                        c.write_all(&[1, if ok { 0 } else { 1 }]).unwrap();
                        if !ok {
                            return;
                        }
                    } else {
                        c.write_all(&[5, 0]).unwrap();
                    }
                    let req = byte(&mut c, 5);
                    assert_eq!(&req[..4], &[5, 1, 0, 3]);
                    let host = String::from_utf8(byte(&mut c, req[4] as usize)).unwrap();
                    let port = u16::from_be_bytes(byte(&mut c, 2).try_into().unwrap());
                    c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
                    (host, port)
                }
                Kind::Http => {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        head.extend(byte(&mut c, 1));
                    }
                    let text = String::from_utf8(head).unwrap();
                    let wanted = login.map(|(n, p)| {
                        use base64::Engine;
                        format!("Proxy-Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{n}:{p}")))
                    });
                    if wanted.is_some_and(|w| !text.contains(&w)) {
                        c.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").unwrap();
                        return;
                    }
                    let target = text.split_whitespace().nth(1).unwrap().to_string();
                    let (h, p) = target.rsplit_once(':').unwrap();
                    // The tunnel's first bytes come with the answer, as proxies do.
                    c.write_all(b"HTTP/1.1 200 Connection established\r\nVia: test\r\n\r\n").unwrap();
                    (h.to_string(), p.parse().unwrap())
                }
            };
            let mut u = TcpStream::connect((target.0.as_str(), target.1)).unwrap();
            let mut b = [0u8; 64];
            let n = c.read(&mut b).unwrap();
            u.write_all(&b[..n]).unwrap();
            let n = u.read(&mut b).unwrap();
            c.write_all(&b[..n]).unwrap();
        });
        port
    }

    fn through(p: &Proxy, port: u16) -> io::Result<String> {
        let mut s = p.connect("localhost", port, Duration::from_secs(5))?;
        s.write_all(b"hello")?;
        let mut b = [0u8; 5];
        s.read_exact(&mut b)?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    #[test]
    fn a_connection_goes_through_either_kind_of_proxy() {
        let to = shouter();
        for kind in [Kind::Socks5, Kind::Http] {
            let p = Proxy { kind, host: "127.0.0.1".into(), port: proxy(kind, None), login: None };
            assert_eq!(through(&p, to).unwrap(), "HELLO", "{kind:?}");
            let login = Some(("me", "s3cret"));
            let p = Proxy { kind, host: "127.0.0.1".into(), port: proxy(kind, login), login: Some(("me".into(), "s3cret".into())) };
            assert_eq!(through(&p, to).unwrap(), "HELLO", "{kind:?} with a name and password");
            let p = Proxy { kind, host: "127.0.0.1".into(), port: proxy(kind, login), login: Some(("me".into(), "wrong".into())) };
            let e = through(&p, to).unwrap_err().to_string();
            assert!(e.contains("did not take its name and password"), "{kind:?}: {e}");
        }
        let p = Proxy { kind: Kind::Http, host: "127.0.0.1".into(), port: proxy(Kind::Http, Some(("me", "x"))), login: None };
        assert!(through(&p, to).unwrap_err().to_string().contains("wants a name and password"));
    }
}
