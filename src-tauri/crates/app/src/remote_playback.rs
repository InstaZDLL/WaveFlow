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
//! sign of the engine's `current_track_id` (negative = non-library source), and
//! a remote session apart from a *plain radio* stream by
//! [`RemotePlayback::is_active`]. The session is cleared the instant a
//! library track becomes current (see `emit_track_changed`) or a radio
//! stream is started (`player_play_url`), so a stale session can never
//! hijack the next advance.
//!
//! Clearing alone stopped being enough once loads became ordered (#622):
//! a remote start whose own load is dropped as superseded would install
//! its session *after* that clear and take the transport back. So the
//! install is guarded on both sides — `remote::playback::play_entries`
//! installs nothing when its intent is already superseded, and undoes its
//! install through [`RemotePlayback::clear_if`] when it is superseded
//! while starting. `clear_if` rather than `clear` because by then the
//! session may be a newer one someone else installed — or the same one
//! the user has navigated inside, which is why every navigation takes a
//! fresh revision too.
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

use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Server id of the primary artist, so the "About the artist" panel can
    /// link to the remote artist view and fetch its photo. `None` when the
    /// server has no primary artist for the track.
    pub artist_id: Option<String>,
    pub artwork_hash: Option<String>,
    pub duration_ms: Option<i64>,
}

/// An ordered remote play queue and its cursor.
#[derive(Debug, Clone)]
pub struct RemoteQueue {
    pub entries: Vec<RemoteEntry>,
    pub index: usize,
}

/// A one-lock snapshot of what the decoder stamps on a remote track's
/// `player:radio-metadata` emit. See [`RemotePlayback::current_stream_meta`].
#[derive(Debug, Clone, Default)]
pub struct RemoteStreamMeta {
    pub is_remote: bool,
    pub duration_ms: Option<i64>,
    pub artwork_hash: Option<String>,
    /// The track's identifier **on the server**, which the local one cannot
    /// stand in for: a remote track plays under a negative sentinel id
    /// ([`crate::commands::player`] mints a fresh one per track), so nothing
    /// downstream can name it to the server without this. Same reason
    /// `artwork_hash` is here.
    pub remote_id: Option<String>,
}

/// Process-wide handle on the active remote play queue, if any. Held on
/// [`crate::state::AppState`]. A `std::sync::Mutex` (not tokio) so the
/// synchronous control seams — `emit_track_changed`, the player commands
/// — can clear or probe it without an await; the guard is never held
/// across one.
#[derive(Default)]
pub struct RemotePlayback {
    inner: Mutex<Option<RemoteQueue>>,
    /// Names the current state of the session, so a caller can undo its
    /// own work and nobody else's — see [`Self::set`] and
    /// [`Self::clear_if`].
    ///
    /// **Every mutation that makes the session somebody else's business
    /// bumps this**, not just an install: a navigation counts too. A token
    /// that survived a `seek_to` would let a start whose load was
    /// superseded clear the session the user has since jumped inside, and
    /// nothing would be driving the queue they can hear.
    ///
    /// Written only while holding `inner`, so a reader holding that same
    /// lock sees the queue and the number naming it as one consistent
    /// pair.
    revisions: AtomicU64,
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

    /// Install a fresh queue, replacing any current one. Returns the
    /// revision naming the session it just installed, for
    /// [`Self::clear_if`].
    pub fn set(&self, queue: RemoteQueue) -> u64 {
        let mut guard = self.inner.lock().expect("remote_playback poisoned");
        let revision = self.revisions.fetch_add(1, Ordering::AcqRel) + 1;
        *guard = Some(queue);
        revision
    }

    /// Drop the session only if `revision` still names it — the rollback
    /// half of [`Self::set`].
    ///
    /// A caller that installed a session and then found out it should not
    /// have (its load was superseded, #622) cannot simply
    /// [`clear`](Self::clear): by then the session may be a newer one
    /// someone else installed, or the same one the user has navigated
    /// inside, and dropping either would strand the queue they are
    /// actually listening to. Returns whether it cleared anything.
    pub fn clear_if(&self, revision: u64) -> bool {
        let mut guard = self.inner.lock().expect("remote_playback poisoned");
        if self.revisions.load(Ordering::Acquire) != revision {
            return false;
        }
        let had = guard.is_some();
        *guard = None;
        had
    }

    /// The entry the cursor points at, cloned out of the lock.
    pub fn current(&self) -> Option<RemoteEntry> {
        let guard = self.inner.lock().expect("remote_playback poisoned");
        guard.as_ref().and_then(|q| q.entries.get(q.index).cloned())
    }

    /// The facts the decoder stamps on a remote track's
    /// `player:radio-metadata` emit — whether a session is active, and the
    /// cursor entry's duration, artwork hash and server id — read under a
    /// single lock. Separate calls could straddle a `clear()` and emit
    /// `is_remote = true` with a `None` duration/artwork (or the reverse),
    /// so they must come from one guard.
    pub fn current_stream_meta(&self) -> RemoteStreamMeta {
        let guard = self.inner.lock().expect("remote_playback poisoned");
        let entry = guard.as_ref().and_then(|q| q.entries.get(q.index));
        RemoteStreamMeta {
            is_remote: guard.is_some(),
            duration_ms: entry.and_then(|e| e.duration_ms),
            artwork_hash: entry.and_then(|e| e.artwork_hash.clone()),
            remote_id: entry.map(|e| e.id.clone()),
        }
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
        // A navigation makes the session the navigator's, so it takes a
        // fresh revision: an earlier start's rollback must no longer match.
        self.revisions.fetch_add(1, Ordering::AcqRel);
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
        // Same as `seek_to`: stepping is a navigation, and the session that
        // comes out of it is no longer the one an earlier start installed.
        self.revisions.fetch_add(1, Ordering::AcqRel);
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
fn advance_index(
    len: usize,
    index: usize,
    direction: Direction,
    repeat: RepeatMode,
) -> Option<usize> {
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
    fn clear_if_undoes_only_its_own_install() {
        let playback = RemotePlayback::default();
        let mine = playback.set(RemoteQueue {
            entries: vec![entry("a")],
            index: 0,
        });
        assert!(playback.clear_if(mine), "its own install is undone");
        assert!(!playback.is_active());
    }

    #[test]
    fn clear_if_leaves_a_session_the_user_navigated_alone() {
        // The start that installed the session is rolling back because its
        // own load was superseded — by a jump inside that very session.
        // Its queue is the one playing, so the rollback must not fire
        // (#622). Counting installs alone would have missed this: a
        // navigation does not install anything.
        let playback = RemotePlayback::default();
        let installed = playback.set(RemoteQueue {
            entries: vec![entry("a"), entry("b")],
            index: 0,
        });
        assert_eq!(playback.seek_to(1).map(|e| e.id).as_deref(), Some("b"));
        assert!(
            !playback.clear_if(installed),
            "the jump made the session the navigator's"
        );
        assert!(playback.is_active(), "the queue the user can hear survives");
        assert_eq!(playback.current().map(|e| e.id).as_deref(), Some("b"));
    }

    #[test]
    fn clear_if_leaves_a_session_that_was_stepped_alone() {
        // Same through `step`, which is the auto-advance's path.
        let playback = RemotePlayback::default();
        let installed = playback.set(RemoteQueue {
            entries: vec![entry("a"), entry("b")],
            index: 0,
        });
        assert_eq!(
            playback
                .step(Direction::Next, RepeatMode::Off)
                .map(|e| e.id)
                .as_deref(),
            Some("b")
        );
        assert!(!playback.clear_if(installed));
        assert!(playback.is_active());
    }

    #[test]
    fn clear_if_leaves_a_newer_session_alone() {
        // A remote start whose load was superseded rolls back, but by then
        // the session in place belongs to whoever came after it. Dropping
        // that one would strand the queue the user is listening to (#622).
        let playback = RemotePlayback::default();
        let stale = playback.set(RemoteQueue {
            entries: vec![entry("a")],
            index: 0,
        });
        let _newer = playback.set(RemoteQueue {
            entries: vec![entry("b")],
            index: 0,
        });
        assert!(!playback.clear_if(stale), "nothing of ours left to undo");
        assert_eq!(playback.current().map(|e| e.id).as_deref(), Some("b"));
    }

    #[test]
    fn next_off_stops_at_the_end() {
        assert_eq!(
            advance_index(3, 0, Direction::Next, RepeatMode::Off),
            Some(1)
        );
        assert_eq!(
            advance_index(3, 1, Direction::Next, RepeatMode::Off),
            Some(2)
        );
        assert_eq!(advance_index(3, 2, Direction::Next, RepeatMode::Off), None);
    }

    #[test]
    fn next_all_wraps() {
        assert_eq!(
            advance_index(3, 2, Direction::Next, RepeatMode::All),
            Some(0)
        );
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
        assert_eq!(
            advance_index(3, 1, Direction::Next, RepeatMode::One),
            Some(1)
        );
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
        let _ = playback.set(RemoteQueue {
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
        let _ = playback.set(RemoteQueue {
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
            artist_id: None,
            artwork_hash: None,
            duration_ms: None,
        }
    }
}
