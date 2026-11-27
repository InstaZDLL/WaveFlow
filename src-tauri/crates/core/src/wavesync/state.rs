//! The state every participant holds, and the ops that change it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{Entry, EntryId, ParticipantId, MAX_PLAYED_KEPT};

/// A participant as the others see them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub id: ParticipantId,
    /// The name they chose, cleaned by the host. Shown, never logged.
    pub name: String,
    pub role: Role,
    pub state: ParticipantState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Host,
    Guest,
}

/// What a participant's panel row says about them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantState {
    /// Following the host's playback.
    Listening,
    /// Following, and waiting for the bytes of what plays.
    Buffering,
    /// In the session, playing something else on their own.
    NotFollowing,
    /// The connection dropped. Their entries are skipped until they are back.
    Away,
}

/// What the session plays: an entry, where in it, at which moment of the
/// host's clock, and whether it is moving. A guest that knows the host's
/// clock knows where the host is now ([`Anchor::position_at`]).
///
/// There is no rate: speed is pinned to 1× while following (RFC-010
/// Decision 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// `None` when nothing plays.
    pub entry: Option<EntryId>,
    pub position_ms: u64,
    /// The host's clock, in milliseconds, when the entry was at
    /// `position_ms`.
    pub at_ms: u64,
    pub playing: bool,
}

impl Anchor {
    /// Nothing playing, as of `at_ms`.
    pub fn idle(at_ms: u64) -> Self {
        Self {
            entry: None,
            position_ms: 0,
            at_ms,
            playing: false,
        }
    }

    /// Where the host is at `now_ms` of its own clock. A clock read before
    /// the anchor (an estimate a little off) counts as the anchor itself.
    pub fn position_at(&self, now_ms: u64) -> u64 {
        if self.playing {
            self.position_ms
                .saturating_add(now_ms.saturating_sub(self.at_ms))
        } else {
            self.position_ms
        }
    }
}

/// What the session allows guests, beyond adding and removing their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rights {
    /// Guests may play, pause, seek and skip for everyone. Off by default:
    /// the transport is the host's (RFC-010 decision 6).
    pub everyone_controls_playback: bool,
}

/// Where entries go in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", content = "entry", rename_all = "snake_case")]
pub enum Position {
    /// After everything.
    End,
    /// Right after what plays: the next to play.
    Next,
    /// Right after this entry, which must not have been played.
    After(EntryId),
}

/// One change to the session. The host makes them; everyone applies them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    ParticipantJoined {
        participant: Participant,
    },
    /// Gone for good: they left, or the host removed them (`removed`).
    ParticipantLeft {
        participant: ParticipantId,
        removed: bool,
    },
    ParticipantStateChanged {
        participant: ParticipantId,
        state: ParticipantState,
    },
    EntriesAdded {
        entries: Vec<Entry>,
        position: Position,
    },
    /// The entry leaves the queue and stops being readable. Removing what
    /// plays leaves nothing playing; the host follows with a `Transport`.
    EntryRemoved {
        entry: EntryId,
    },
    EntryMoved {
        entry: EntryId,
        position: Position,
    },
    /// What plays now. `cursor` is the index of `anchor.entry` in the queue,
    /// or of the next entry to play when nothing does; every entry before it
    /// counts as played.
    Transport {
        cursor: usize,
        anchor: Anchor,
    },
    RightsChanged {
        rights: Rights,
    },
    Ended,
}

/// An op and its place in the session's sequence, which starts at 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequenced {
    pub seq: u64,
    #[serde(flatten)]
    pub op: Op,
}

/// The whole state at one point of the sequence: what a joining or lagging
/// participant starts from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The last op the state includes; 0 before the first.
    pub seq: u64,
    pub state: SessionState,
}

/// Why an op could not be applied. On a replica it means the replica and
/// the host disagree, and the cure is a [`Snapshot`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("unknown entry")]
    UnknownEntry,
    #[error("unknown participant")]
    UnknownParticipant,
    #[error("an entry with this id is already in the queue")]
    DuplicateEntry,
    #[error("a participant with this id is already in the session")]
    DuplicateParticipant,
    #[error("the entry has been played")]
    NotUpcoming,
    #[error("the cursor does not match the queue")]
    BadCursor,
    #[error("the session has ended")]
    Ended,
    /// The session has one host from start to end: none joins, the host never
    /// leaves (it ends the session instead).
    #[error("the session must have exactly one host")]
    HostCannotChange,
}

/// A session as every participant holds it.
///
/// The fields change only through [`SessionState::apply`], which keeps these
/// true after every op: `cursor <= queue.len()`; when an entry plays, it is
/// `queue[cursor]`, and when none does nothing moves; no two entries and no two
/// participants share an id; exactly one participant is the host.
///
/// A state read from the wire is checked against the same rules before it
/// exists ([`Self::check`], through `try_from`), so no accessor ever meets a
/// cursor past the end of its queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedState")]
pub struct SessionState {
    name: String,
    participants: Vec<Participant>,
    queue: Vec<Entry>,
    cursor: usize,
    anchor: Anchor,
    rights: Rights,
    ended: bool,
}

/// [`SessionState`] as it arrives, before [`SessionState::check`].
#[derive(Deserialize)]
struct UncheckedState {
    name: String,
    participants: Vec<Participant>,
    queue: Vec<Entry>,
    cursor: usize,
    anchor: Anchor,
    rights: Rights,
    ended: bool,
}

impl TryFrom<UncheckedState> for SessionState {
    type Error = ApplyError;

    fn try_from(raw: UncheckedState) -> Result<Self, Self::Error> {
        let state = Self {
            name: raw.name,
            participants: raw.participants,
            queue: raw.queue,
            cursor: raw.cursor,
            anchor: raw.anchor,
            rights: raw.rights,
            ended: raw.ended,
        };
        state.check()?;
        Ok(state)
    }
}

impl SessionState {
    pub(super) fn new(name: String, host: Participant, now_ms: u64) -> Self {
        Self {
            name,
            participants: vec![host],
            queue: Vec::new(),
            cursor: 0,
            anchor: Anchor::idle(now_ms),
            rights: Rights::default(),
            ended: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn participants(&self) -> &[Participant] {
        &self.participants
    }

    pub fn participant(&self, id: ParticipantId) -> Option<&Participant> {
        self.participants.iter().find(|p| p.id == id)
    }

    /// The whole queue, played entries first.
    pub fn queue(&self) -> &[Entry] {
        &self.queue
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The entries already played, still listed.
    pub fn played(&self) -> &[Entry] {
        &self.queue[..self.cursor]
    }

    /// What plays now, and what will: the readable part of the queue.
    pub fn upcoming(&self) -> &[Entry] {
        &self.queue[self.cursor..]
    }

    pub fn current(&self) -> Option<&Entry> {
        self.anchor.entry.and(self.queue.get(self.cursor))
    }

    pub fn anchor(&self) -> Anchor {
        self.anchor
    }

    pub fn rights(&self) -> Rights {
        self.rights
    }

    pub fn has_ended(&self) -> bool {
        self.ended
    }

    pub fn index_of(&self, entry: EntryId) -> Option<usize> {
        self.queue.iter().position(|e| e.id == entry)
    }

    /// The entry whose bytes may be read now, if `entry` is one: in the
    /// queue, and not passed by the cursor. Everything else — a removed
    /// entry, a played one, an id nobody minted — reads nothing.
    pub fn readable(&self, entry: EntryId) -> Option<&Entry> {
        if self.ended {
            return None;
        }
        let index = self.index_of(entry)?;
        (index >= self.cursor).then(|| &self.queue[index])
    }

    /// The index the first entry after what plays would have.
    pub fn next_slot(&self) -> usize {
        self.cursor + usize::from(self.anchor.entry.is_some())
    }

    /// Where `position` puts an entry, or why it cannot.
    pub(super) fn slot(&self, position: Position) -> Result<usize, ApplyError> {
        match position {
            Position::End => Ok(self.queue.len()),
            Position::Next => Ok(self.next_slot()),
            Position::After(after) => {
                let index = self.index_of(after).ok_or(ApplyError::UnknownEntry)?;
                if index < self.cursor {
                    return Err(ApplyError::NotUpcoming);
                }
                Ok((index + 1).max(self.next_slot()))
            }
        }
    }

    /// Apply `op`, or change nothing and say why.
    pub fn apply(&mut self, op: &Op) -> Result<(), ApplyError> {
        if self.ended {
            return Err(ApplyError::Ended);
        }
        match op {
            Op::ParticipantJoined { participant } => {
                if self.participant(participant.id).is_some() {
                    return Err(ApplyError::DuplicateParticipant);
                }
                if participant.role == Role::Host {
                    return Err(ApplyError::HostCannotChange);
                }
                self.participants.push(participant.clone());
            }
            Op::ParticipantLeft { participant, .. } => {
                let index = self
                    .participants
                    .iter()
                    .position(|p| p.id == *participant)
                    .ok_or(ApplyError::UnknownParticipant)?;
                if self.participants[index].role == Role::Host {
                    return Err(ApplyError::HostCannotChange);
                }
                self.participants.remove(index);
            }
            Op::ParticipantStateChanged { participant, state } => {
                let p = self
                    .participants
                    .iter_mut()
                    .find(|p| p.id == *participant)
                    .ok_or(ApplyError::UnknownParticipant)?;
                p.state = *state;
            }
            Op::EntriesAdded { entries, position } => {
                let mut seen: HashSet<EntryId> = self.queue.iter().map(|e| e.id).collect();
                if !entries.iter().all(|e| seen.insert(e.id)) {
                    return Err(ApplyError::DuplicateEntry);
                }
                let at = self.slot(*position)?;
                self.queue.splice(at..at, entries.iter().cloned());
            }
            Op::EntryRemoved { entry } => {
                let index = self.index_of(*entry).ok_or(ApplyError::UnknownEntry)?;
                self.queue.remove(index);
                if index < self.cursor {
                    self.cursor -= 1;
                } else if index == self.cursor && self.anchor.entry == Some(*entry) {
                    self.anchor.entry = None;
                    self.anchor.playing = false;
                    self.anchor.position_ms = 0;
                }
            }
            Op::EntryMoved { entry, position } => {
                let index = self.index_of(*entry).ok_or(ApplyError::UnknownEntry)?;
                if index < self.next_slot() {
                    return Err(ApplyError::NotUpcoming);
                }
                if *position == Position::After(*entry) {
                    return Err(ApplyError::UnknownEntry);
                }
                // Resolve the place on the queue without the entry, so that
                // "after X" means after X wherever the entry came from.
                let mut without = self.clone();
                let moved = without.queue.remove(index);
                let at = without.slot(*position)?;
                without.queue.insert(at, moved);
                self.queue = without.queue;
            }
            Op::Transport { cursor, anchor } => {
                // The cursor only moves forward: an entry it has passed was
                // revoked, and no op makes it readable again.
                if *cursor < self.cursor {
                    return Err(ApplyError::BadCursor);
                }
                self.fits(*cursor, anchor)?;
                self.cursor = *cursor;
                self.anchor = *anchor;
                if self.cursor > MAX_PLAYED_KEPT {
                    let drop = self.cursor - MAX_PLAYED_KEPT;
                    self.queue.drain(..drop);
                    self.cursor = MAX_PLAYED_KEPT;
                }
            }
            Op::RightsChanged { rights } => self.rights = *rights,
            Op::Ended => self.ended = true,
        }
        Ok(())
    }

    /// Whether `anchor` can stand at `cursor`: the cursor within the queue,
    /// the entry playing being the one under it and the position within it,
    /// and nothing moving when no entry plays.
    fn fits(&self, cursor: usize, anchor: &Anchor) -> Result<(), ApplyError> {
        if cursor > self.queue.len() {
            return Err(ApplyError::BadCursor);
        }
        match anchor.entry {
            Some(entry) => match self.queue.get(cursor) {
                Some(e) if e.id == entry && anchor.position_ms <= e.duration_ms => Ok(()),
                _ => Err(ApplyError::BadCursor),
            },
            None if anchor.playing => Err(ApplyError::BadCursor),
            None => Ok(()),
        }
    }

    /// Whether a state that arrived whole — a host's snapshot, hostile input
    /// like any other — keeps the invariants [`Self::apply`] keeps.
    pub fn check(&self) -> Result<(), ApplyError> {
        self.fits(self.cursor, &self.anchor)?;
        let mut entries = HashSet::new();
        if !self.queue.iter().all(|e| entries.insert(e.id)) {
            return Err(ApplyError::DuplicateEntry);
        }
        let mut people = HashSet::new();
        if !self.participants.iter().all(|p| people.insert(p.id)) {
            return Err(ApplyError::DuplicateParticipant);
        }
        if self
            .participants
            .iter()
            .filter(|p| p.role == Role::Host)
            .count()
            != 1
        {
            return Err(ApplyError::HostCannotChange);
        }
        Ok(())
    }
}
