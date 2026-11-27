//! Which peers may connect (RFC-010 Decision 1, the address table).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// What kind of address a peer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// RFC 1918: `10/8`, `172.16/12`, `192.168/16`.
    Private,
    /// `169.254/16`, `fe80::/10`.
    LinkLocal,
    /// `127/8`, `::1`: two instances on one machine.
    Loopback,
    /// IPv6 unique local addresses, `fc00::/7` (RFC 4193). Tailscale's IPv6
    /// prefix, `fd7a:115c:a1e0::/48`, is one of them.
    UniqueLocal,
    /// `100.64.0.0/10`, the shared address space of RFC 6598, from which
    /// Tailscale numbers its devices, and carriers their customers.
    Shared,
    /// Everything else, the unspecified and multicast addresses included.
    Public,
}

/// The kind of `ip`. An IPv4 address carried in IPv6 (`::ffff:a.b.c.d`, how a
/// dual-stack socket reports an IPv4 peer) is judged as the IPv4 address.
pub fn kind(ip: IpAddr) -> Kind {
    match ip {
        IpAddr::V4(v4) => kind_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => kind_v4(v4),
            None => kind_v6(v6),
        },
    }
}

fn kind_v4(ip: Ipv4Addr) -> Kind {
    let [a, b, ..] = ip.octets();
    if ip.is_loopback() {
        Kind::Loopback
    } else if ip.is_private() {
        Kind::Private
    } else if ip.is_link_local() {
        Kind::LinkLocal
    } else if a == 100 && (b & 0b1100_0000) == 0b0100_0000 {
        // `Ipv4Addr::is_shared` is not stable; this is its definition.
        Kind::Shared
    } else {
        Kind::Public
    }
}

fn kind_v6(ip: Ipv6Addr) -> Kind {
    let first = ip.segments()[0];
    if ip.is_loopback() {
        Kind::Loopback
    } else if first & 0xffc0 == 0xfe80 {
        Kind::LinkLocal
    } else if first & 0xfe00 == 0xfc00 {
        Kind::UniqueLocal
    } else {
        Kind::Public
    }
}

/// The rule a listener (and a guest about to connect) applies to the other
/// side's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerFilter {
    /// `100.64.0.0/10` is accepted: the user turned on _Allow private VPN
    /// addresses_, **and** this machine has an address of its own in that
    /// block. The block is also a carrier's, for its other customers, so
    /// neither condition alone is enough.
    shared_allowed: bool,
}

impl PeerFilter {
    /// The filter for a machine whose own addresses are `local`, with the
    /// user's switch `allow_private_vpn`.
    pub fn new(allow_private_vpn: bool, local: impl IntoIterator<Item = IpAddr>) -> Self {
        let on_overlay = local.into_iter().any(|ip| kind(ip) == Kind::Shared);
        Self {
            shared_allowed: allow_private_vpn && on_overlay,
        }
    }

    /// The filter for this machine now, from its interfaces. An interface list
    /// that cannot be read counts as no address in the shared block.
    pub fn for_this_machine(allow_private_vpn: bool) -> Self {
        let local = if_addrs::get_if_addrs()
            .map(|interfaces| interfaces.into_iter().map(|i| i.ip()).collect::<Vec<_>>())
            .unwrap_or_default();
        Self::new(allow_private_vpn, local)
    }

    /// Whether a peer at `ip` may connect.
    pub fn admits(&self, ip: IpAddr) -> bool {
        match kind(ip) {
            Kind::Private | Kind::LinkLocal | Kind::Loopback | Kind::UniqueLocal => true,
            Kind::Shared => self.shared_allowed,
            Kind::Public => false,
        }
    }
}
