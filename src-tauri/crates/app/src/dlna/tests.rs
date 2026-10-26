use super::*;

fn addr(ip: [u8; 4], prefix: u32) -> LanAddr {
    LanAddr {
        ip: Ipv4Addr::from(ip),
        netmask: Ipv4Addr::from(u32::MAX.checked_shl(32 - prefix).unwrap_or(0)),
    }
}

#[test]
fn a_physical_lan_beats_a_virtual_bridge_listed_first() {
    let wsl = interface_rank("vEthernet (WSL)", Ipv4Addr::new(172, 20, 0, 1), true, false);
    let docker = interface_rank("docker0", Ipv4Addr::new(172, 17, 0, 1), true, false);
    let wifi = interface_rank("Wi-Fi", Ipv4Addr::new(192, 168, 1, 20), true, false);
    assert!(wifi < wsl && wifi < docker);
}

#[test]
fn tunnels_down_links_and_link_local_are_demoted() {
    let lan = interface_rank("eth0", Ipv4Addr::new(10, 0, 0, 2), true, false);
    let tunnel = interface_rank("ppp0", Ipv4Addr::new(10, 8, 0, 2), true, true);
    let down = interface_rank("eth1", Ipv4Addr::new(192, 168, 2, 2), false, false);
    let apipa = interface_rank("Ethernet 2", Ipv4Addr::new(169, 254, 3, 4), true, false);
    let vpn = interface_rank("tailscale0", Ipv4Addr::new(100, 64, 0, 7), true, false);
    assert!(lan < tunnel && lan < down && lan < vpn);
    assert!(vpn < apipa);
}

#[test]
fn a_physical_public_address_beats_a_virtual_private_one() {
    let public = interface_rank("eno1", Ipv4Addr::new(130, 79, 1, 2), true, false);
    let bridge = interface_rank("virbr0", Ipv4Addr::new(192, 168, 122, 1), true, false);
    assert!(public < bridge);
}

#[test]
fn a_plain_lan_name_is_not_virtual() {
    for name in ["eth0", "enp3s0", "wlan0", "en0", "Ethernet", "Wi-Fi", "br0"] {
        assert!(!looks_virtual(name), "{name}");
    }
}

#[test]
fn the_peer_gets_the_address_on_its_subnet() {
    let addrs = [addr([192, 168, 1, 20], 24), addr([172, 20, 0, 1], 20)];
    let peer = |ip: [u8; 4]| IpAddr::V4(Ipv4Addr::from(ip));
    let wsl = Some(Ipv4Addr::new(172, 20, 0, 1));
    let lan = Some(Ipv4Addr::new(192, 168, 1, 20));
    assert_eq!(local_ip_for(&addrs, peer([172, 20, 3, 9])), wsl);
    assert_eq!(local_ip_for(&addrs, peer([192, 168, 1, 77])), lan);
    // Off every subnet (a routed peer): the preferred address.
    assert_eq!(local_ip_for(&addrs, peer([10, 1, 1, 1])), lan);
    let mapped = IpAddr::V6(Ipv4Addr::new(172, 20, 3, 9).to_ipv6_mapped());
    assert_eq!(local_ip_for(&addrs, mapped), wsl);
    assert_eq!(local_ip_for(&[], peer([10, 1, 1, 1])), None);
}

#[test]
fn a_zero_mask_claims_no_peer() {
    assert!(!addr([10, 0, 0, 1], 0).contains(Ipv4Addr::new(192, 168, 1, 1)));
    assert!(addr([10, 0, 0, 1], 32).contains(Ipv4Addr::new(10, 0, 0, 1)));
    assert!(!addr([10, 0, 0, 1], 32).contains(Ipv4Addr::new(10, 0, 0, 2)));
}
