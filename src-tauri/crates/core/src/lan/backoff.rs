//! Wrong codes slow their sender down; they never lock everyone out
//! (RFC-010 Decision 5).

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

/// The wait after a first wrong code; it doubles with each one after.
pub const FIRST_WAIT_MS: u64 = 1_000;
/// The longest wait.
pub const MAX_WAIT_MS: u64 = 5 * 60 * 1_000;
/// Senders remembered at most. Past it, the one whose wait ended first is
/// forgotten: a machine cycling addresses only buys its next attempt sooner,
/// at the price of filling this.
const MAX_SENDERS: usize = 1_024;

/// Wrong codes per sender, a sender being one address (an IPv4 address
/// carried in IPv6 counts as the IPv4 one).
///
/// Not an IPv6 /64: on a LAN every machine shares the network's /64, so one
/// person typing wrong codes would make every IPv6 guest wait. A machine can
/// pick new addresses to get fresh attempts; each still costs an exchange on
/// a 2⁴⁰ code with at most a few dozen at once, and every one is counted on
/// the host's screen.
#[derive(Debug, Default)]
pub struct Backoff {
    senders: HashMap<Sender, Strike>,
    /// Senders with an exchange under way: a second one from the same sender
    /// waits for the first to be settled, or two at once would each pass the
    /// check before either failed.
    in_flight: HashSet<Sender>,
    total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Sender {
    V4([u8; 4]),
    V6([u8; 16]),
}

#[derive(Debug, Clone, Copy)]
struct Strike {
    failures: u32,
    until_ms: u64,
}

impl Sender {
    fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Sender::V4(v4.octets()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Sender::V4(v4.octets()),
                None => Sender::V6(v6.octets()),
            },
        }
    }
}

impl Backoff {
    /// `Err(ms)` while `ip` must still wait that long.
    pub fn check(&self, ip: IpAddr, now_ms: u64) -> Result<(), u64> {
        match self.senders.get(&Sender::of(ip)) {
            Some(strike) if strike.until_ms > now_ms => Err(strike.until_ms - now_ms),
            _ => Ok(()),
        }
    }

    /// Start an exchange for `ip`: `Err(ms)` while it must wait, or while it
    /// already has one under way. Every `Ok` is settled by exactly one of
    /// [`Self::failed`] or [`Self::succeeded`].
    pub fn begin(&mut self, ip: IpAddr, now_ms: u64) -> Result<(), u64> {
        self.check(ip, now_ms)?;
        if !self.in_flight.insert(Sender::of(ip)) {
            return Err(FIRST_WAIT_MS);
        }
        Ok(())
    }

    /// Record a wrong code from `ip`; returns how many wrong codes the
    /// listener has seen in all, for the host's notice.
    pub fn failed(&mut self, ip: IpAddr, now_ms: u64) -> u32 {
        let sender = Sender::of(ip);
        self.in_flight.remove(&sender);
        if !self.senders.contains_key(&sender) && self.senders.len() >= MAX_SENDERS {
            if let Some(oldest) = self
                .senders
                .iter()
                .min_by_key(|(_, s)| s.until_ms)
                .map(|(k, _)| *k)
            {
                self.senders.remove(&oldest);
            }
        }
        let strike = self.senders.entry(sender).or_insert(Strike {
            failures: 0,
            until_ms: 0,
        });
        strike.failures = strike.failures.saturating_add(1);
        let wait = FIRST_WAIT_MS
            .saturating_mul(1u64 << (strike.failures - 1).min(20))
            .min(MAX_WAIT_MS);
        strike.until_ms = now_ms.saturating_add(wait);
        self.total = self.total.saturating_add(1);
        self.total
    }

    /// `ip` got in: its wrong codes are forgotten (the total is not).
    pub fn succeeded(&mut self, ip: IpAddr) {
        let sender = Sender::of(ip);
        self.in_flight.remove(&sender);
        self.senders.remove(&sender);
    }

    /// Wrong codes since the listener started.
    pub fn total(&self) -> u32 {
        self.total
    }
}
