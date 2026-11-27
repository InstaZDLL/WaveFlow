//! Advertising a listener on the LAN and finding the others (mDNS / DNS-SD).
//!
//! The daemon is the app's own (`mdns-sd`, a thread with no async runtime),
//! not Avahi over D-Bus: the Flatpak needs no permission beyond the network
//! it already has, and it answers on every platform the same way. It shares
//! UDP 5353 with whatever responder the system runs.
//!
//! An advertisement names an instance by random hex, never the machine or
//! the user; what it says beyond that is the caller's `txt`, which every
//! machine on the network reads.

use std::collections::BTreeMap;
use std::net::{SocketAddr, SocketAddrV6};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

#[derive(Debug, thiserror::Error)]
#[error("mDNS: {0}")]
pub struct MdnsError(String);

impl From<mdns_sd::Error> for MdnsError {
    fn from(e: mdns_sd::Error) -> Self {
        Self(e.to_string())
    }
}

/// A listener being advertised. Dropping it withdraws the advertisement.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    /// Advertise `port` under `service_type` (`_name._tcp.local.`), with
    /// `txt`. The addresses are the machine's, kept current by the daemon.
    pub fn start(service_type: &str, port: u16, txt: &[(&str, &str)]) -> Result<Self, MdnsError> {
        let instance = hex::encode(super::random::<8>());
        let host = format!("{instance}.local.");
        let properties: Vec<(&str, &str)> = txt.to_vec();
        let info = ServiceInfo::new(service_type, &instance, &host, "", port, &properties[..])?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        let daemon = ServiceDaemon::new()?;
        if let Err(e) = daemon.register(info) {
            // The daemon's thread outlives its handles: stop it, or every
            // failed start leaves one behind with its sockets.
            let _ = daemon.shutdown();
            return Err(e.into());
        }
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// A listener another machine advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its name in the advertisement; the same in a later [`Event::Lost`].
    pub fullname: String,
    /// Where it listens, IPv4 first. An IPv6 link-local address carries the
    /// interface it was heard on, without which it cannot be reached.
    pub addresses: Vec<SocketAddr>,
    /// Its text record. Anybody on the network wrote it: hostile input.
    pub txt: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Found(Found),
    Lost { fullname: String },
}

/// Looking for listeners of one type, for as long as it lives.
pub struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
}

/// Longest text value kept from somebody else's advertisement.
const MAX_TXT_VALUE: usize = 256;

impl Browser {
    pub fn start(service_type: &str) -> Result<Self, MdnsError> {
        let daemon = ServiceDaemon::new()?;
        let events = match daemon.browse(service_type) {
            Ok(events) => events,
            Err(e) => {
                let _ = daemon.shutdown();
                return Err(e.into());
            }
        };
        Ok(Self { daemon, events })
    }

    /// The next listener found or lost; `None` once the daemon has stopped.
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            match self.events.recv_async().await.ok()? {
                ServiceEvent::ServiceResolved(info) => {
                    let port = info.get_port();
                    let mut addresses: Vec<SocketAddr> = info
                        .get_addresses()
                        .iter()
                        .map(|a| match a {
                            mdns_sd::ScopedIp::V6(v6) => SocketAddr::V6(SocketAddrV6::new(
                                *v6.addr(),
                                port,
                                0,
                                v6.scope_id().index,
                            )),
                            other => SocketAddr::new(other.to_ip_addr(), port),
                        })
                        .collect();
                    addresses.sort_by_key(|a| (a.is_ipv6(), *a));
                    let txt = info
                        .get_properties()
                        .iter()
                        .filter_map(|p| {
                            let value = p.val_str();
                            (value.len() <= MAX_TXT_VALUE)
                                .then(|| (p.key().to_string(), value.to_string()))
                        })
                        .collect();
                    return Some(Event::Found(Found {
                        fullname: info.get_fullname().to_string(),
                        addresses,
                        txt,
                    }));
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    return Some(Event::Lost { fullname });
                }
                _ => continue,
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}
