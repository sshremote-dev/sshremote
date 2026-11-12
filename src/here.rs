//! This computer as others reach it: its name, its addresses, the user signed in here, and whether
//! an SSH server answers here.

use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Here {
    /// Its name on the network ("office-pc").
    pub name: String,
    /// Its full name, when it has one beyond `name` ("office-pc.corp.example").
    pub dns: String,
    /// Its addresses, the one its traffic goes out from first.
    pub ips: Vec<IpAddr>,
    /// Who is signed in here, for `user@address`.
    pub user: String,
    /// An SSH server answers on port 22 here.
    pub ssh: bool,
}

/// Looks it all up: a few system calls and a short local connection, so not on a window's thread.
pub fn look() -> Here {
    let (name, dns) = names();
    let mut ips: Vec<IpAddr> = Vec::new();
    // The address traffic leaves from: a socket aimed out (nothing is sent) says which.
    for (bind, to) in [("0.0.0.0:0", "192.0.2.1:9"), ("[::]:0", "[2001:db8::1]:9")] {
        if let Some(ip) = UdpSocket::bind(bind).and_then(|s| s.connect(to).and_then(|_| s.local_addr())).ok().map(|a| a.ip())
            && !ip.is_unspecified()
            && !ips.contains(&ip)
        {
            ips.push(ip);
        }
    }
    for ip in interfaces() {
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    ips.retain(|ip| !ip.is_loopback() && !link_local(ip));
    let user = std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default();
    let ssh = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], 22)), Duration::from_millis(200)).is_ok();
    Here { name, dns, ips, user, ssh }
}

fn link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_link_local(),
        IpAddr::V6(v) => (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// The host name, and the full name when the system knows one longer than it.
#[cfg(unix)]
fn names() -> (String, String) {
    let mut buf = [0u8; 256];
    let name = if unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0 {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    } else {
        String::new()
    };
    let dns = canonical(&name).filter(|d| d.contains('.') && *d != name).unwrap_or_default();
    (name, dns)
}

/// The resolver's canonical name for `name`.
#[cfg(unix)]
fn canonical(name: &str) -> Option<String> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_flags = libc::AI_CANONNAME;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    if unsafe { libc::getaddrinfo(c.as_ptr(), std::ptr::null(), &hints, &mut res) } != 0 || res.is_null() {
        return None;
    }
    let canon = unsafe { (*res).ai_canonname };
    let out = (!canon.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(canon) }.to_string_lossy().into_owned());
    unsafe { libc::freeaddrinfo(res) };
    out
}

#[cfg(windows)]
fn names() -> (String, String) {
    use windows_sys::Win32::System::SystemInformation::{ComputerNameDnsFullyQualified, ComputerNameDnsHostname, GetComputerNameExW};
    let get = |kind| {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        (unsafe { GetComputerNameExW(kind, buf.as_mut_ptr(), &mut len) } != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    };
    let name = get(ComputerNameDnsHostname).unwrap_or_else(|| std::env::var("COMPUTERNAME").unwrap_or_default());
    let dns = get(ComputerNameDnsFullyQualified).filter(|d| d.contains('.') && !d.eq_ignore_ascii_case(&name)).unwrap_or_default();
    (name, dns)
}

/// The addresses of the network interfaces that are up.
#[cfg(unix)]
fn interfaces() -> Vec<IpAddr> {
    let mut out = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return out;
    }
    let mut at = list;
    while !at.is_null() {
        let i = unsafe { &*at };
        let up = i.ifa_flags & libc::IFF_UP as u32 != 0;
        // Containers' and virtual machines' bridges are this computer's own inner networks:
        // nobody else reaches it there (a VPN's, such as tailscale0, stays).
        let name = if i.ifa_name.is_null() { String::new() } else { unsafe { std::ffi::CStr::from_ptr(i.ifa_name) }.to_string_lossy().into_owned() };
        let inner = ["docker", "br-", "veth", "virbr", "lxc", "cni", "podman"].iter().any(|p| name.starts_with(p));
        if up && !inner && !i.ifa_addr.is_null() {
            match unsafe { (*i.ifa_addr).sa_family } as i32 {
                libc::AF_INET => {
                    let a = unsafe { &*(i.ifa_addr as *const libc::sockaddr_in) };
                    out.push(IpAddr::from(u32::from_be(a.sin_addr.s_addr).to_be_bytes()));
                }
                libc::AF_INET6 => {
                    let a = unsafe { &*(i.ifa_addr as *const libc::sockaddr_in6) };
                    out.push(IpAddr::from(a.sin6_addr.s6_addr));
                }
                _ => {}
            }
        }
        at = i.ifa_next;
    }
    unsafe { libc::freeifaddrs(list) };
    out
}

/// On Windows the address traffic leaves from is the one told (above).
#[cfg(windows)]
fn interfaces() -> Vec<IpAddr> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_computer_has_a_name_and_an_address() {
        let h = super::look();
        assert!(!h.name.is_empty());
        assert!(h.ips.iter().all(|ip| !ip.is_loopback()));
        println!("{h:?}");
    }
}
