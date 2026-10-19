//! Pieces shared by macOS and Linux: interface lookup and unprivileged
//! ICMP echo (both kernels allow `SOCK_DGRAM` + `IPPROTO_ICMP`).

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use super::PingError;

/// Name of the interface that holds `local`.
pub fn interface_name_for(local: IpAddr) -> Option<String> {
    let mut found = None;
    for_each_ifaddr(|ifa| {
        if found.is_some() || ifa.ifa_addr.is_null() {
            return;
        }
        // SAFETY: ifa_addr is non-null and points at a sockaddr whose
        // family tells us its concrete type.
        let addr = unsafe { sockaddr_ip(ifa.ifa_addr) };
        if addr == Some(local) {
            // SAFETY: ifa_name is a NUL-terminated C string owned by the list.
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) };
            found = Some(name.to_string_lossy().into_owned());
        }
    });
    found
}

/// Visit every entry of `getifaddrs`.
pub fn for_each_ifaddr(mut f: impl FnMut(&libc::ifaddrs)) {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we free below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return;
    }
    let mut p = head;
    while !p.is_null() {
        // SAFETY: p walks the list returned by getifaddrs.
        let ifa = unsafe { &*p };
        f(ifa);
        p = ifa.ifa_next;
    }
    // SAFETY: head came from getifaddrs.
    unsafe { libc::freeifaddrs(head) };
}

/// # Safety
/// `sa` must point at a valid sockaddr.
pub unsafe fn sockaddr_ip(sa: *const libc::sockaddr) -> Option<IpAddr> {
    // SAFETY: guaranteed by the caller; the family selects the layout.
    unsafe {
        match i32::from((*sa).sa_family) {
            libc::AF_INET => {
                let sin = &*(sa as *const libc::sockaddr_in);
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                    sin.sin_addr.s_addr,
                ))))
            }
            libc::AF_INET6 => {
                let sin6 = &*(sa as *const libc::sockaddr_in6);
                Some(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)))
            }
            _ => None,
        }
    }
}

/// UTC offset via localtime_r (reads the system zone database).
pub fn utc_offset(unix_secs: i64) -> Option<i32> {
    let t: libc::time_t = unix_secs as libc::time_t;
    // SAFETY: zeroed tm is a valid out-parameter for localtime_r.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: valid pointers to a time_t and a tm.
    let r = unsafe { libc::localtime_r(&t, &mut tm) };
    if r.is_null() {
        return None;
    }
    Some(tm.tm_gmtoff as i32)
}

/// Turn SIGTERM, SIGINT and SIGHUP into a normal `exit(0)` so `atexit`
/// cleanup runs. The signals are blocked in every thread (call this before
/// spawning any) and received by one thread with `sigwait`.
pub fn exit_on_termination_signals() {
    // SAFETY: plain signal-set manipulation on a zeroed sigset_t.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, sig);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    };
    let spawned = std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let mut sig: libc::c_int = 0;
            // SAFETY: waiting on the signals blocked above.
            unsafe { libc::sigwait(&set, &mut sig) };
            std::process::exit(0);
        });
    if spawned.is_err() {
        // Without the thread, restore default handling.
        // SAFETY: unblocking the same set.
        unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) };
    }
}

struct Fd(i32);
impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor.
        unsafe { libc::close(self.0) };
    }
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for pair in data.chunks(2) {
        let word = if pair.len() == 2 {
            u16::from_be_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], 0])
        };
        sum += u32::from(word);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// One ICMPv4 echo request/reply over an unprivileged datagram socket.
pub fn ping(target: IpAddr, seq: u16, timeout: Duration) -> Result<Duration, PingError> {
    let IpAddr::V4(v4) = target else {
        return Err(PingError::Unsupported);
    };
    // SAFETY: plain socket(2) call.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EPROTONOSUPPORT) => {
                Err(PingError::Unsupported)
            }
            _ => Err(PingError::Failed(e.to_string())),
        };
    }
    let fd = Fd(fd);
    let tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 20_000,
    };
    // SAFETY: setting a timeval option on our socket.
    unsafe {
        libc::setsockopt(
            fd.0,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&tv as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }
    let ident = (std::process::id() & 0xFFFF) as u16;
    let nonce: u64 = (std::process::id() as u64) << 32 | u64::from(seq) << 8 | 0xA5;
    let mut pkt = [0u8; 24];
    pkt[0] = 8; // echo request
    pkt[4..6].copy_from_slice(&ident.to_be_bytes());
    pkt[6..8].copy_from_slice(&seq.to_be_bytes());
    pkt[8..16].copy_from_slice(&nonce.to_be_bytes());
    let ck = checksum(&pkt);
    pkt[2..4].copy_from_slice(&ck.to_be_bytes());

    // SAFETY: zeroed sockaddr_in is a valid starting value.
    let mut sin: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sin.sin_family = libc::AF_INET as libc::sa_family_t;
    sin.sin_addr = libc::in_addr {
        s_addr: u32::from(v4).to_be(),
    };
    #[cfg(target_os = "macos")]
    {
        sin.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    }
    let t0 = Instant::now();
    // SAFETY: sending our buffer to a valid sockaddr_in.
    let sent = unsafe {
        libc::sendto(
            fd.0,
            pkt.as_ptr().cast(),
            pkt.len(),
            0,
            (&sin as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if sent < 0 {
        return Err(PingError::Failed(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let mut buf = [0u8; 512];
    loop {
        if t0.elapsed() > timeout {
            return Err(PingError::Timeout);
        }
        // SAFETY: receiving into our buffer.
        let n = unsafe { libc::recv(fd.0, buf.as_mut_ptr().cast(), buf.len(), 0) };
        let now = Instant::now();
        if n < 0 {
            continue;
        }
        let n = n as usize;
        // macOS includes the IPv4 header in datagram ICMP reads; Linux not.
        let icmp = if n >= 20 && buf[0] >> 4 == 4 {
            let ihl = usize::from(buf[0] & 0x0F) * 4;
            &buf[ihl.min(n)..n]
        } else {
            &buf[..n]
        };
        if icmp.len() >= 16
            && icmp[0] == 0
            && icmp[6..8] == seq.to_be_bytes()
            && icmp[8..16] == nonce.to_be_bytes()
        {
            return Ok(now - t0);
        }
    }
}

/// Classify an interface from its BSD/Linux name when nothing better exists.
pub fn kind_from_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let kind = if n.starts_with("lo") {
        "Loopback"
    } else if n.starts_with("utun")
        || n.starts_with("tun")
        || n.starts_with("ipsec")
        || n.starts_with("wg")
        || n.starts_with("ppp")
    {
        "Tunnel"
    } else if n.starts_with("pdp_ip") || n.starts_with("wwan") {
        "Cellular"
    } else if n.starts_with("bridge") || n.starts_with("br") {
        "Bridge"
    } else if n.starts_with("wl") {
        "Wi-Fi"
    } else if n.starts_with("en") || n.starts_with("eth") {
        "Ethernet"
    } else {
        return None;
    };
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icmp_checksum() {
        // RFC 1071 example words.
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(checksum(&data), !0xddf2u16);
    }

    #[test]
    fn loopback_has_an_interface() {
        assert!(interface_name_for(IpAddr::V4(Ipv4Addr::LOCALHOST)).is_some());
    }
}
