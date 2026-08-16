//! In-memory remote play queue (RFC-005 sync_v2).
//!
//! A remote playlist plays by streaming each track from the bound server
//! over HTTP — the same single-URL path Web Radio uses — rather than from
//! the local `queue_item` table, whose rows are library row ids joined to
//! the `track` table. A projected remote track has no such row (the
//! projection deliberately never lands in local tables, RFC-005
//! Decision 1), so its queue lives here instead: an ordered list of
//! server track ids plus the metadata needed to drive the PlayerBar,
//! held entirely in memory and rebuilt from the projection each time the
//! user starts a playlist.
//!
//! ## Why this is a separate structure, not a variant of `QueueTrack`
//!
//! [`crate::queue::QueueTrack`] is `FromRow` of a multi-join query over
//! `track` / `album` / `artist` / `artwork`, consumed by the local
//! player, MPD, the media-key surface and the PlayerBar payload. Teaching
//! it about rows that have no local identity would ripple through every
//! one of those. Keeping the remote queue parallel — like the radio
//! session, which is also id-less and in-memory — leaves the local queue
//! path untouched.
//!
//! ## The discriminator
//!
//! Playback tells a remote session apart from a library track by the
//! sign of the engine's `current_track_id` (negative = URL stream), and
//! a remote session apart from a *plain radio* stream by
//! [`RemotePlayback::is_active`]. The session is cleared the instant a
//! library track becomes current (see `emit_track_changed`) or a radio
//! stream is started (`player_play_url`), so a stale session can never
//! hijack the next advance.
//!
//! This module holds only plain data (no dependency on the `sync_v2`-gated
//! [`crate::remote`] tree) so it can be compiled unconditionally and read
//! from the always-present control seams. The orchestration that mints
//! tickets and drives the engine lives in [`crate::remote::playback`],
//! which is gated.

// Most of this module's surface is consumed only by the `sync_v2`-gated
// `remote::playback` orchestration and the always-present control seams
// under `#[cfg(feature = "sync_v2")]`. In a stock build those callers are
// compiled out, so all but `clear()` reads as dead code even though it is
// live wherever the feature is on (and under `cfg(test)`). Same treatment,
// same reason, as `queue.rs` and `remote/mod.rs`.
#![allow(dead_code)]

use std::sync::Mutex;

use crate::queue::{Direction, RepeatMode};

/// One entry of a remote play queue: a server track id plus the metadata
/// the PlayerBar, the queue panel and the seekbar read. Cloned out of the
/// lock before any await.
#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artwork_hash: Option<String>,
    pub duration_ms: Option<i64>,
}

/// An ordered remote play queue and its cursor.
#[derive(Debug, Clone)]
pub struct RemoteQueue {
    pub entries: Vec<RemoteEntry>,
    pub index: usize,
}

/// Process-wide handle on the active remote play queue, if any. Held on
/// [`crate::state::AppState`]. A `std::sync::Mutex` (not tokio) so the
/// synchronous control seams — `emit_track_changed`, the player commands
/// — can clear or probe it without an await; the guard is never held
/// across one.
#[derive(Default)]
pub struct RemotePlayback {
    inner: Mutex<Option<RemoteQueue>>,
}

impl RemotePlayback {
    /// Drop any active remote session. Called whenever a library track or
    /// a radio stream becomes current, so the next advance can't act on a
    /// queue the user has moved on from. A no-op when none is active.
    pub fn clear(&self) {
        *self.inner.lock().expect("remote_playback poisoned") = None;
    }

    /// Whether a remote queue is currently driving playback.
    pub fn is_active(&self) -> bool {
        self.inner
            .lock()
            .expect("remote_playback poisoned")
            .is_some()
    }

    /// Install a fresh queue, replacing any current one.
    pub fn set(&self, queue: RemoteQueue) {
        *self.inner.lock().expect("remote_playback poisoned") = Some(queue);
    }

    /// The entry the cursor points at, cloned out of the lock.
    pub fn current(&self) -> Option<RemoteEntry> {
        let guard = self.inner.lock().expect("remote_playback poisoned");
        guard.as_ref().and_then(|q| q.entries.get(q.index).cloned())
    }

    /// A snapshot of the whole queue and its cursor, for the queue panel.
    pub fn snapshot(&self) -> Option<(Vec<RemoteEntry>, usize)> {
        let guard = self.inner.lock().expect("remote_playback poisoned");
        guard.as_ref().map(|q| (q.entries.clone(), q.index))
    }

    /// Move the cursor to an absolute position (clamped) and return the
    /// entry there. Used when the user clicks a row in the queue panel.
    pub fn seek_to(&self, index: usize) -> Option<RemoteEntry> {
        let mut guard = self.inner.lock().expect("remote_playback poisoned");
        let queue = guard.as_mut()?;
        if queue.entries.is_empty() {
            return None;
        }
        queue.index = index.min(queue.entries.len() - 1);
        queue.entries.get(queue.index).cloned()
    }

    /// Move the cursor one step and return the entry it lands on. Returns
    /// `None` — and clears the session — when the step runs off the end
    /// with repeat off, matching [`crate::queue::advance`]'s semantics for
    /// the local queue.
    pub fn step(&self, direction: Direction, repeat: RepeatMode) -> Option<RemoteEntry> {
        let mut guard = self.inner.lock().expect("remote_playback poisoned");
        let queue = guard.as_mut()?;
        match advance_index(queue.entries.len(), queue.index, direction, repeat) {
            Some(next) => {
                queue.index = next;
                queue.entries.get(next).cloned()
            }
            None => {
                *guard = None;
                None
            }
        }
    }
}

/// Where the cursor lands after one step, or `None` to stop. Pure, so the
/// wrap / clamp / stop rules are unit-testable without a queue. Mirrors
/// [`crate::queue::advance`]:
///   - repeat-one replays the same slot in either direction;
///   - next stops (`None`) at the end with repeat off, else wraps;
///   - previous clamps at 0 with repeat off, else wraps to the end.
fn advance_index(len: usize, index: usize, direction: Direction, repeat: RepeatMode) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match (direction, repeat) {
        (_, RepeatMode::One) => Some(index),
        (Direction::Next, RepeatMode::Off) => {
            if index + 1 >= len {
                None
            } else {
                Some(index + 1)
            }
        }
        (Direction::Next, RepeatMode::All) => Some((index + 1) % len),
        (Direction::Previous, RepeatMode::Off) => Some(index.saturating_sub(1)),
        (Direction::Previous, RepeatMode::All) => {
            if index == 0 {
                Some(len - 1)
            } else {
                Some(index - 1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_off_stops_at_the_end() {
        assert_eq!(advance_index(3, 0, Direction::Next, RepeatMode::Off), Some(1));
        assert_eq!(advance_index(3, 1, Direction::Next, RepeatMode::Off), Some(2));
        assert_eq!(advance_index(3, 2, Direction::Next, RepeatMode::Off), None);
    }

    #[test]
    fn next_all_wraps() {
        assert_eq!(advance_index(3, 2, Direction::Next, RepeatMode::All), Some(0));
    }

    #[test]
    fn previous_off_clamps_at_zero() {
        assert_eq!(
            advance_index(3, 0, Direction::Previous, RepeatMode::Off),
            Some(0)
        );
        assert_eq!(
            advance_index(3, 2, Direction::Previous, RepeatMode::Off),
            Some(1)
        );
    }

    #[test]
    fn previous_all_wraps_to_end() {
        assert_eq!(
            advance_index(3, 0, Direction::Previous, RepeatMode::All),
            Some(2)
        );
    }

    #[test]
    fn repeat_one_holds_the_slot() {
        assert_eq!(advance_index(3, 1, Direction::Next, RepeatMode::One), Some(1));
        assert_eq!(
            advance_index(3, 1, Direction::Previous, RepeatMode::One),
            Some(1)
        );
    }

    #[test]
    fn an_empty_queue_never_advances() {
        assert_eq!(advance_index(0, 0, Direction::Next, RepeatMode::All), None);
    }

    #[test]
    fn step_clears_when_it_runs_off_the_end() {
        let playback = RemotePlayback::default();
        playback.set(RemoteQueue {
            entries: vec![entry("a"), entry("b")],
            index: 1,
        });
        assert!(playback.is_active());
        // Next past the last entry with repeat off ends the session.
        assert!(playback.step(Direction::Next, RepeatMode::Off).is_none());
        assert!(!playback.is_active());
    }

    #[test]
    fn step_advances_and_reports_the_landing_entry() {
        let playback = RemotePlayback::default();
        playback.set(RemoteQueue {
            entries: vec![entry("a"), entry("b"), entry("c")],
            index: 0,
        });
        let landed = playback.step(Direction::Next, RepeatMode::Off).unwrap();
        assert_eq!(landed.id, "b");
        assert_eq!(playback.current().unwrap().id, "b");
    }

    fn entry(id: &str) -> RemoteEntry {
        RemoteEntry {
            id: id.into(),
            title: None,
            artist: None,
            artwork_hash: None,
            duration_ms: None,
        }
    }
}
