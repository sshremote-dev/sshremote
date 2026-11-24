//! Computers on the same network: those with remote access on answer a broadcast with their ID,
//! name and hardware address (never a password), so they can be added without typing the ID; and
//! a computer that sleeps is woken by its hardware address (Wake-on-LAN).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// The UDP port finders ask on.
pub const PORT: u16 = 7072;
const ASK: &[u8] = b"sshremote-find 1";

/// A computer that answered.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub id: String,
    pub relay: String,
    pub name: String,
    pub ip: IpAddr,
    /// Its hardware address, for waking it later; empty when it did not say.
    pub mac: String,
    /// The port it takes viewers on straight, if it does.
    pub port: Option<u16>,
}

impl Found {
    /// How it is reached by its ID.
    pub fn address(&self) -> String {
        format!("sshremote://{}@{}", self.id, self.relay)
    }

    /// Where it is reached straight, if it is.
    pub fn direct(&self) -> String {
        self.port.map(|p| std::net::SocketAddr::new(self.ip, p).to_string()).unwrap_or_default()
    }
}

/// Notes where the computers in the list that answered are now (their straight address and
/// network card), so the next connection to one goes straight. Returns those that answered.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn refresh_known(wait: Duration) -> Vec<Found> {
    let found = find(wait);
    for c in crate::computers::all() {
        if let Some(f) = c.id().and_then(|id| found.iter().find(|f| f.id == id)) {
            let (direct, mac) = (f.direct(), f.mac.clone());
            if c.direct != direct || (!mac.is_empty() && c.mac != mac) {
                crate::computers::update(&c.address, |k| {
                    k.direct = direct;
                    if !mac.is_empty() {
                        k.mac = mac;
                    }
                });
            }
        }
    }
    found
}

/// Answers finders on this network while `keep` gives this computer's ID and relay; at most a
/// few answers a second, so it cannot be used to flood anyone.
#[cfg(feature = "remote")]
pub fn answer(keep: impl Fn() -> Option<(String, String, bool)>) {
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)) else { return };
    let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
    let name = crate::here::look().name;
    let mac = macs().into_iter().next().unwrap_or_default();
    let mut buf = [0u8; 64];
    let (mut window, mut told) = (Instant::now(), 0);
    while let Some((id, relay, direct)) = keep() {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        if &buf[..n] != ASK {
            continue;
        }
        if window.elapsed() > Duration::from_secs(1) {
            (window, told) = (Instant::now(), 0);
        }
        told += 1;
        if told > 10 {
            continue;
        }
        // The port it takes viewers straight on, when it does.
        let port = direct.then_some(crate::net::DIRECT_PORT);
        let reply = serde_json::json!({ "sshremote": 1, "id": id, "relay": relay, "name": name, "mac": mac, "port": port });
        let _ = sock.send_to(reply.to_string().as_bytes(), from);
    }
}

/// Asks this network, waits `wait` for answers, and returns those who answered, each once.
pub fn find(wait: Duration) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return out };
    let _ = sock.set_broadcast(true);
    for to in broadcasts() {
        let _ = sock.send_to(ASK, SocketAddr::from((to, PORT)));
    }
    let until = Instant::now() + wait;
    let mut buf = [0u8; 1024];
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let _ = sock.set_read_timeout(Some(left));
        let Ok((n, from)) = sock.recv_from(&mut buf) else { break };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) else { continue };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let id = s("id");
        if v.get("sshremote").is_none() || !id.bytes().all(|b| b.is_ascii_digit()) || id.is_empty() || out.iter().any(|f| f.id == id) {
            continue;
        }
        let port = v.get("port").and_then(|p| p.as_u64()).and_then(|p| u16::try_from(p).ok());
        out.push(Found { id, relay: s("relay"), name: s("name"), ip: from.ip(), mac: s("mac"), port });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// A hardware address as six bytes, from "aa:bb:cc:dd:ee:ff" or "AA-BB-...".
pub fn parse_mac(mac: &str) -> Option<[u8; 6]> {
    let parts: Vec<u8> = mac.trim().split([':', '-']).map(|p| u8::from_str_radix(p, 16)).collect::<Result<_, _>>().ok()?;
    parts.try_into().ok()
}

/// Wakes the computer whose network card has `mac`, if it is set to wake that way: the magic
/// packet (six 0xff, then the address sixteen times) to every broadcast address here.
pub fn wake(mac: &str) -> Result<(), String> {
    let m = parse_mac(mac).ok_or("That is not a hardware address (such as 3c:52:82:1a:2b:3c).")?;
    let mut packet = vec![0xffu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&m);
    }
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| e.to_string())?;
    sock.set_broadcast(true).map_err(|e| e.to_string())?;
    let mut sent = false;
    for to in broadcasts() {
        for port in [9, 7] {
            sent |= sock.send_to(&packet, SocketAddr::from((to, port))).is_ok();
        }
    }
    if sent { Ok(()) } else { Err("no network to send it on".into()) }
}

/// Where a broadcast reaches this network: each interface's own, and the general one.
fn broadcasts() -> Vec<Ipv4Addr> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut out = vec![Ipv4Addr::BROADCAST];
    #[cfg(unix)]
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) == 0 {
            let mut at = list;
            while !at.is_null() {
                let i = &*at;
                let up = i.ifa_flags & libc::IFF_UP as u32 != 0 && i.ifa_flags & libc::IFF_LOOPBACK as u32 == 0;
                if up && !i.ifa_addr.is_null() && !i.ifa_netmask.is_null() && (*i.ifa_addr).sa_family as i32 == libc::AF_INET {
                    let a = u32::from_be((*(i.ifa_addr as *const libc::sockaddr_in)).sin_addr.s_addr);
                    let m = u32::from_be((*(i.ifa_netmask as *const libc::sockaddr_in)).sin_addr.s_addr);
                    let b = Ipv4Addr::from(a | !m);
                    if !out.contains(&b) {
                        out.push(b);
                    }
                }
                at = i.ifa_next;
            }
            libc::freeifaddrs(list);
        }
    }
    out
}

/// This computer's hardware addresses, of its real network cards that are connected.
pub fn macs() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir("/sys/class/net") else { return out };
        let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for n in names {
            let inner = ["lo", "docker", "br-", "veth", "virbr", "lxc", "cni", "podman", "tailscale", "tun", "wg"].iter().any(|p| n.starts_with(p));
            let base = std::path::Path::new("/sys/class/net").join(&n);
            let up = std::fs::read_to_string(base.join("operstate")).is_ok_and(|s| s.trim() == "up");
            let mac = std::fs::read_to_string(base.join("address")).unwrap_or_default().trim().to_string();
            if !inner && up && parse_mac(&mac).is_some_and(|m| m != [0; 6]) {
                out.push(mac);
            }
        }
        out
    }
    #[cfg(windows)]
    {
        // getmac's CSV: connection, adapter, physical address, transport (a disconnected card's
        // is "Media disconnected").
        let Ok(o) = crate::sys::quiet(std::process::Command::new("getmac").args(["/fo", "csv", "/nh", "/v"])).output() else { return Vec::new() };
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split("\",\"").map(|x| x.trim_matches('"')).collect();
                let (adapter, mac, transport) = (f.get(1)?, f.get(2)?, f.get(3)?);
                let virt = ["Virtual", "VPN", "Hyper-V", "Loopback", "Bluetooth", "TAP"].iter().any(|v| adapter.contains(v));
                (!virt && transport.starts_with("\\Device") && parse_mac(mac).is_some()).then(|| mac.replace('-', ":").to_lowercase())
            })
            .collect()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_addresses_read_both_ways() {
        assert_eq!(parse_mac("3c:52:82:1A:2b:3c"), Some([0x3c, 0x52, 0x82, 0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_mac("3C-52-82-1A-2B-3C"), Some([0x3c, 0x52, 0x82, 0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_mac("3c:52:82"), None);
        assert_eq!(parse_mac("zz:52:82:1a:2b:3c"), None);
    }

    /// A finder here finds an answerer here, by the general broadcast.
    #[cfg(feature = "remote")]
    #[test]
    fn a_computer_answering_is_found() {
        let on = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let keep = on.clone();
        let t = std::thread::spawn(move || answer(|| keep.load(std::sync::atomic::Ordering::Relaxed).then(|| ("482913075".into(), "relay.example".into(), true))));
        std::thread::sleep(Duration::from_millis(200));
        let found = find(Duration::from_millis(800));
        on.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = t.join();
        // A machine whose firewall or network drops broadcasts finds nobody: then there is nothing
        // to check. Other computers on the network may answer too: this one is looked for.
        if !found.is_empty() {
            let f = found.iter().find(|f| f.id == "482913075").expect("this computer's answer among those found");
            assert_eq!(f.address(), "sshremote://482913075@relay.example");
        }
    }
}
