//! Purpose: Suggest addresses another machine could use to reach this one.
//! Role: Feeds the setup step on the local Access page; never used for authorization.
//! Invariants: Lists IPv4 addresses of interfaces that are up, skipping loopback and
//!   link-local ones, then this machine's name. Best effort: an empty list is fine.

use serde::Serialize;
use std::net::Ipv4Addr;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct Candidate {
    pub host: String,
    /// What kind of network the address is on: vm, local, vpn, public, or name.
    pub kind: &'static str,
    /// The network interface the address is on, which tells two of a kind apart,
    /// such as two VM networks. The machine's name has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
}

pub(super) fn candidates() -> Vec<Candidate> {
    let mut found: Vec<Candidate> = interfaces()
        .into_iter()
        .map(|(interface, ip)| Candidate {
            host: ip.to_string(),
            kind: kind_of(&interface, ip),
            interface: Some(interface),
        })
        .collect();
    found.sort_by_key(|candidate| rank(candidate.kind));
    let mut seen = std::collections::HashSet::new();
    found.retain(|candidate| seen.insert(candidate.host.clone()));
    if let Some(name) = super::activity::hostname() {
        found.push(Candidate {
            host: name,
            kind: "name",
            interface: None,
        });
    }
    found
}

/// Guesses the network from the interface name, then from the address range.
fn kind_of(interface: &str, ip: Ipv4Addr) -> &'static str {
    const VM: [&str; 5] = ["bridge", "vmnet", "vboxnet", "virbr", "vEthernet"];
    const VPN: [&str; 4] = ["utun", "tailscale", "wg", "tun"];
    if VM.iter().any(|prefix| interface.starts_with(prefix)) {
        "vm"
    } else if VPN.iter().any(|prefix| interface.starts_with(prefix)) {
        "vpn"
    } else if ip.is_private() {
        "local"
    } else {
        "public"
    }
}

fn rank(kind: &str) -> u8 {
    match kind {
        "local" => 0,
        "vm" => 1,
        "vpn" => 2,
        _ => 3,
    }
}

#[cfg(unix)]
fn interfaces() -> Vec<(String, Ipv4Addr)> {
    let mut found = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list that freeifaddrs releases below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return found;
    }
    let mut cursor = head;
    while !cursor.is_null() {
        // SAFETY: `cursor` points into the list from getifaddrs, valid until freeifaddrs.
        let entry = unsafe { &*cursor };
        cursor = entry.ifa_next;
        let flags = entry.ifa_flags as libc::c_int;
        if flags & libc::IFF_UP == 0 || flags & libc::IFF_LOOPBACK != 0 || entry.ifa_addr.is_null()
        {
            continue;
        }
        // SAFETY: `ifa_addr` is non-null and points to a sockaddr of the family it names.
        if libc::c_int::from(unsafe { (*entry.ifa_addr).sa_family }) != libc::AF_INET {
            continue;
        }
        // SAFETY: an AF_INET address is a sockaddr_in.
        let raw = unsafe {
            (*(entry.ifa_addr as *const libc::sockaddr_in))
                .sin_addr
                .s_addr
        };
        let ip = Ipv4Addr::from(u32::from_be(raw));
        if ip.is_link_local() {
            continue;
        }
        // SAFETY: `ifa_name` is a NUL-terminated string owned by the same list.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) };
        found.push((name.to_string_lossy().into_owned(), ip));
    }
    // SAFETY: `head` came from a successful getifaddrs call and is freed once.
    unsafe { libc::freeifaddrs(head) };
    found
}

#[cfg(not(unix))]
fn interfaces() -> Vec<(String, Ipv4Addr)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_interface_names_then_ranges() {
        let local = Ipv4Addr::new(192, 168, 1, 20);
        assert_eq!(kind_of("en0", local), "local");
        assert_eq!(kind_of("bridge100", Ipv4Addr::new(192, 168, 64, 1)), "vm");
        assert_eq!(kind_of("utun4", Ipv4Addr::new(100, 101, 3, 4)), "vpn");
        assert_eq!(kind_of("eth0", Ipv4Addr::new(203, 0, 113, 7)), "public");
    }
}
