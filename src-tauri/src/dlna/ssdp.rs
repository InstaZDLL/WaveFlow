//! SSDP (Simple Service Discovery Protocol) announcer + responder.
//!
//! UPnP discovery happens over UDP multicast on `239.255.255.250:1900`.
//! Two flows:
//!
//! 1. **NOTIFY ssdp:alive** — we periodically broadcast NOTIFY frames
//!    so controllers that joined the network after we started learn
//!    we exist. The cycle is anchored to the device's `CACHE-CONTROL:
//!    max-age=1800` advertisement; sending one notification per
//!    `max-age / 4` (~7 minutes) is the conservative recommendation
//!    in the UPnP arch spec.
//!
//! 2. **M-SEARCH responder** — controllers that started before us
//!    issue `M-SEARCH * HTTP/1.1` queries to enumerate devices. We
//!    listen on the multicast group, parse the `ST:` (Search Target)
//!    and reply with a unicast HTTP/1.1 200 OK pointing at our
//!    `/description.xml`.
//!
//! ## Search targets answered
//!
//! - `ssdp:all` — everything
//! - `upnp:rootdevice`
//! - `urn:schemas-upnp-org:device:MediaServer:1`
//! - `urn:schemas-upnp-org:service:ContentDirectory:1`
//! - `urn:schemas-upnp-org:service:ConnectionManager:1`
//! - the device UUID itself (`uuid:<v5>`)
//!
//! ## Lifecycle
//!
//! [`spawn`] returns a [`SsdpHandle`] whose `Drop` aborts the
//! background tasks. The DLNA worker holds the handle in its
//! [`WorkerState`](super::WorkerState); a Stop or Reconfigure replaces
//! the handle, terminating the previous announcer cleanly.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::dlna::description::device_uuid;

/// Standard SSDP multicast endpoint. Same on every UPnP network.
const SSDP_ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);

/// `CACHE-CONTROL: max-age=` value advertised in NOTIFY/MSEARCH
/// replies. 1800 s (30 min) is the value most reference clients use.
const CACHE_MAX_AGE: u64 = 1800;

/// Drop guard for the running announcer + responder tasks.
pub struct SsdpHandle {
    announcer: JoinHandle<()>,
    responder: JoinHandle<()>,
}

impl Drop for SsdpHandle {
    fn drop(&mut self) {
        self.announcer.abort();
        self.responder.abort();
    }
}

/// Spawn the announcer + M-SEARCH responder tasks.
///
/// `lan_ip` is the address advertised in `LOCATION:` headers — the
/// caller has already resolved the host's primary LAN interface so
/// controllers can reach `/description.xml`.
///
/// `port` is the HTTP port the description lives on (already bound
/// by the [`http`](super::http) layer).
pub fn spawn(server_name: String, lan_ip: String, port: u16) -> std::io::Result<SsdpHandle> {
    let uuid = device_uuid(&server_name).to_string();
    let location = format!("http://{lan_ip}:{port}/description.xml");
    let server_header = format!(
        "WaveFlow/0.1 UPnP/1.0 WaveFlowMediaServer/1.0",
    );

    let shared = Arc::new(SsdpShared {
        uuid,
        location,
        server: server_header,
    });

    let bind_ip: Ipv4Addr = lan_ip.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);
    let mcast_socket = build_multicast_socket(bind_ip)?;

    let announcer = tokio::spawn(announce_loop(shared.clone()));
    let responder = tokio::spawn(respond_loop(shared, mcast_socket));

    Ok(SsdpHandle { announcer, responder })
}

#[derive(Debug)]
struct SsdpShared {
    uuid: String,
    location: String,
    server: String,
}

/// Build a UDP socket joined to the SSDP multicast group. We use
/// socket2 because tokio's `UdpSocket::join_multicast_v4` works but
/// doesn't expose the `SO_REUSEADDR` / `SO_REUSEPORT` flags we need
/// for the responder to coexist with other UPnP services running on
/// the same machine (e.g. Windows Media Player).
fn build_multicast_socket(interface: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // SO_REUSEADDR is enough for our use case: it lets the SSDP
    // socket coexist with other UPnP services already bound to
    // 239.255.255.250:1900 (Windows Media Player, miniDLNA, ...).
    // SO_REUSEPORT (Linux/macOS-only, requires the socket2 `all`
    // feature) is not needed — we never share the multicast
    // membership with another in-process socket.
    sock.set_reuse_address(true)?;
    sock.set_nonblocking(true)?;
    let bind: SocketAddr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 1900).into();
    sock.bind(&bind.into())?;
    sock.join_multicast_v4(SSDP_ADDR.ip(), &interface)?;
    sock.set_multicast_loop_v4(false)?;
    sock.set_multicast_ttl_v4(4)?;

    let std_sock: std::net::UdpSocket = sock.into();
    UdpSocket::from_std(std_sock)
}

/// Periodic NOTIFY ssdp:alive broadcaster. Sends one batch of three
/// NOTIFY frames (rootdevice, MediaServer:1, the uuid) every
/// `CACHE_MAX_AGE / 4` seconds.
async fn announce_loop(shared: Arc<SsdpShared>) {
    let interval = Duration::from_secs(CACHE_MAX_AGE / 4);
    // First batch goes out immediately so controllers learn about us
    // without waiting the full interval.
    loop {
        if let Err(err) = send_alive_batch(&shared).await {
            tracing::warn!(?err, "SSDP NOTIFY alive failed");
        }
        tokio::time::sleep(interval).await;
    }
}

async fn send_alive_batch(shared: &SsdpShared) -> std::io::Result<()> {
    let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)).await?;
    socket.set_multicast_ttl_v4(4)?;
    for nt in NOTIFICATION_TARGETS {
        let nt_str = expand_target(nt, &shared.uuid);
        let usn = build_usn(&nt_str, &shared.uuid);
        let frame = format!(
            "NOTIFY * HTTP/1.1\r\n\
             HOST: 239.255.255.250:1900\r\n\
             CACHE-CONTROL: max-age={CACHE_MAX_AGE}\r\n\
             LOCATION: {location}\r\n\
             NT: {nt_str}\r\n\
             NTS: ssdp:alive\r\n\
             SERVER: {server}\r\n\
             USN: {usn}\r\n\r\n",
            location = shared.location,
            server = shared.server,
        );
        socket.send_to(frame.as_bytes(), SSDP_ADDR).await?;
    }
    Ok(())
}

/// Listen for M-SEARCH datagrams and reply with unicast HTTP/1.1 200.
async fn respond_loop(shared: Arc<SsdpShared>, socket: UdpSocket) {
    let mut buf = [0u8; 2048];
    loop {
        let (len, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(err) => {
                tracing::debug!(?err, "SSDP recv failed");
                continue;
            }
        };
        let msg = match std::str::from_utf8(&buf[..len]) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // We're only interested in M-SEARCH probes — ignore other
        // devices' NOTIFY frames received via the multicast group.
        if !msg.starts_with("M-SEARCH") {
            continue;
        }
        let st = match parse_header(msg, "ST") {
            Some(s) => s,
            None => continue,
        };
        let targets = matching_targets(&st, &shared.uuid);
        for target in targets {
            let usn = build_usn(&target, &shared.uuid);
            let reply = format!(
                "HTTP/1.1 200 OK\r\n\
                 CACHE-CONTROL: max-age={CACHE_MAX_AGE}\r\n\
                 EXT:\r\n\
                 LOCATION: {location}\r\n\
                 SERVER: {server}\r\n\
                 ST: {target}\r\n\
                 USN: {usn}\r\n\r\n",
                location = shared.location,
                server = shared.server,
            );
            if let Err(err) = socket.send_to(reply.as_bytes(), peer).await {
                tracing::debug!(?err, "SSDP unicast reply failed");
            }
        }
    }
}

/// Notification targets we advertise in NOTIFY ssdp:alive batches.
/// `{uuid}` is replaced with the actual device UUID at send time.
const NOTIFICATION_TARGETS: &[&str] = &[
    "upnp:rootdevice",
    "{uuid}",
    "urn:schemas-upnp-org:device:MediaServer:1",
    "urn:schemas-upnp-org:service:ContentDirectory:1",
    "urn:schemas-upnp-org:service:ConnectionManager:1",
];

fn expand_target(template: &str, uuid: &str) -> String {
    if template == "{uuid}" {
        format!("uuid:{uuid}")
    } else {
        template.to_string()
    }
}

/// Build a `USN` value matching the search target. UPnP rules:
///   - rootdevice → `uuid:<u>::upnp:rootdevice`
///   - device/service URN → `uuid:<u>::<urn>`
///   - the bare uuid → `uuid:<u>`
fn build_usn(target: &str, uuid: &str) -> String {
    if target == format!("uuid:{uuid}") {
        target.to_string()
    } else {
        format!("uuid:{uuid}::{target}")
    }
}

/// Match a controller's `ST:` against everything we serve. Returns
/// the targets we should reply with — `ssdp:all` expands to the full
/// notification list, anything else returns the matching specific
/// target so each reply carries the right `ST` echo.
fn matching_targets(st: &str, uuid: &str) -> Vec<String> {
    let st = st.trim();
    if st == "ssdp:all" {
        return NOTIFICATION_TARGETS
            .iter()
            .map(|t| expand_target(t, uuid))
            .collect();
    }
    let expanded: Vec<String> = NOTIFICATION_TARGETS
        .iter()
        .map(|t| expand_target(t, uuid))
        .collect();
    expanded.into_iter().filter(|t| t == st).collect()
}

/// Tiny header extractor — case-insensitive on the key, trims the
/// value. Avoids pulling in a full HTTP parser for a handful of
/// six-line frames.
fn parse_header(msg: &str, name: &str) -> Option<String> {
    for line in msg.lines() {
        // Skip the request-line ("M-SEARCH * HTTP/1.1") and any
        // malformed line without a colon. Using `?` here would
        // short-circuit the whole loop on the very first line.
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(name) {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_header_is_case_insensitive() {
        let msg = "M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\nHOST: x\r\n\r\n";
        assert_eq!(parse_header(msg, "st"), Some("ssdp:all".into()));
        assert_eq!(parse_header(msg, "ST"), Some("ssdp:all".into()));
        assert!(parse_header(msg, "missing").is_none());
    }

    #[test]
    fn matching_targets_ssdp_all_expands_to_full_list() {
        let out = matching_targets("ssdp:all", "abc");
        assert_eq!(out.len(), NOTIFICATION_TARGETS.len());
        assert!(out.iter().any(|t| t == "uuid:abc"));
    }

    #[test]
    fn matching_targets_specific_st_returns_single_match() {
        let out = matching_targets(
            "urn:schemas-upnp-org:service:ContentDirectory:1",
            "abc",
        );
        assert_eq!(
            out,
            vec!["urn:schemas-upnp-org:service:ContentDirectory:1".to_string()]
        );
    }

    #[test]
    fn matching_targets_unknown_st_returns_empty() {
        assert!(matching_targets("urn:foo:bar", "abc").is_empty());
    }

    #[test]
    fn build_usn_handles_uuid_only_target() {
        assert_eq!(build_usn("uuid:abc", "abc"), "uuid:abc");
        assert_eq!(
            build_usn("upnp:rootdevice", "abc"),
            "uuid:abc::upnp:rootdevice"
        );
    }
}
