//! Following the host: its clock, seen from a guest, and what to do with
//! the guest's player (RFC-010 Decision 4, level 1).
//!
//! The host's clock is estimated from round trips on the connection: the
//! guest sends its time, the host answers with its own, and the reply lands
//! a round trip later. Assuming the two halves of the trip took as long,
//! the host's time was read at the midpoint. Several probes are taken and
//! only the fastest are believed: a slow round trip is one where one half
//! waited, and its midpoint is wrong by up to half the wait.

use super::{Anchor, EntryId};

/// Past this, a guest playing the same entry as the host is moved to where
/// the host is. Below it, a seek would be heard more than the drift.
pub const DRIFT_MS: u64 = 400;

/// One clock probe, all in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// The guest's clock when the probe left.
    pub sent_ms: u64,
    /// The host's clock when it answered.
    pub host_ms: u64,
    /// The guest's clock when the answer arrived.
    pub received_ms: u64,
}

/// The host's clock minus the guest's, from `samples`: the median offset of
/// the half with the shortest round trips. `None` without a usable sample
/// (an answer that arrived before it was sent is not one).
pub fn estimate_offset(samples: &[Sample]) -> Option<i64> {
    let mut usable: Vec<(u64, i64)> = samples
        .iter()
        .filter(|s| s.received_ms >= s.sent_ms)
        .map(|s| {
            let rtt = s.received_ms - s.sent_ms;
            let midpoint = i128::from(s.sent_ms) + i128::from(rtt / 2);
            let offset = (i128::from(s.host_ms) - midpoint)
                .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
            (rtt, offset)
        })
        .collect();
    if usable.is_empty() {
        return None;
    }
    usable.sort_by_key(|&(rtt, _)| rtt);
    usable.truncate(usable.len().div_ceil(2));
    let mut offsets: Vec<i64> = usable.into_iter().map(|(_, offset)| offset).collect();
    offsets.sort_unstable();
    Some(offsets[offsets.len() / 2])
}

/// The host's clock at the guest's `local_ms`, given the offset.
pub fn host_now(local_ms: u64, offset: i64) -> u64 {
    local_ms.saturating_add_signed(offset)
}

/// What the guest's player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub entry: Option<EntryId>,
    pub position_ms: u64,
    pub playing: bool,
    /// Waiting for bytes: its position is not moving because it cannot, and
    /// seeking it again would only restart the wait.
    pub buffering: bool,
}

/// What to do with the guest's player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    /// Leave it alone.
    InSync,
    /// Open `entry` at `position_ms`, playing or paused.
    Load {
        entry: EntryId,
        position_ms: u64,
        playing: bool,
    },
    /// Move within the same entry.
    Seek { position_ms: u64 },
    /// Go to `position_ms` and play.
    Resume { position_ms: u64 },
    /// Pause at `position_ms`.
    Pause { position_ms: u64 },
    /// Nothing plays in the session.
    Stop,
}

/// Compare the guest's player with the host's `anchor` at `host_now_ms`.
pub fn follow(anchor: &Anchor, host_now_ms: u64, local: &Local) -> Follow {
    let Some(entry) = anchor.entry else {
        return if local.entry.is_some() {
            Follow::Stop
        } else {
            Follow::InSync
        };
    };
    let target = anchor.position_at(host_now_ms);
    if local.entry != Some(entry) {
        return Follow::Load {
            entry,
            position_ms: target,
            playing: anchor.playing,
        };
    }
    match (anchor.playing, local.playing) {
        (true, false) if !local.buffering => Follow::Resume {
            position_ms: target,
        },
        (false, true) => Follow::Pause {
            position_ms: target,
        },
        _ if local.buffering => Follow::InSync,
        _ if local.position_ms.abs_diff(target) > DRIFT_MS => Follow::Seek {
            position_ms: target,
        },
        _ => Follow::InSync,
    }
}
