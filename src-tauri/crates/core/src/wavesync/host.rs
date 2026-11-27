//! The host's half: turning requests into ops, and keeping the log.

use std::collections::{HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::entry::clean;
use super::{
    Anchor, Entry, EntryId, Op, Participant, ParticipantId, ParticipantState, Position, Refusal,
    Rights, Role, Sequenced, SessionState, Snapshot, LOG_KEPT, MAX_NAME_CHARS, MAX_PARTICIPANTS,
    MAX_UPCOMING,
};

/// A restart counts as going back to the start of the track. Earlier
/// entries have been played and are no longer readable, so there is no
/// previous track to go to.
const RESTART_MS: u64 = 0;

/// What a participant asks the host to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Add tracks the sender owns. Guests add at the end; the host anywhere.
    Add {
        entries: Vec<Entry>,
        position: Position,
    },
    /// Take an entry out: the sender's own, or any for the host. Removing
    /// what plays skips it for everyone.
    Remove {
        entry: EntryId,
    },
    /// Host only.
    Move {
        entry: EntryId,
        position: Position,
    },
    /// Play `entry`, or resume what is paused, or start the queue.
    Play {
        entry: Option<EntryId>,
    },
    Pause,
    Seek {
        position_ms: u64,
    },
    Next,
    /// Back to the start of what plays.
    Previous,
    /// The host's own player finished `entry`. Host only; ignored when
    /// `entry` no longer plays, so a late report never skips twice.
    ///
    /// Two plays of the same entry are not told apart here (it ended, then a
    /// restart arrived before the report): the app sends the report only for
    /// the load it belongs to, which its load intent names. A play counter
    /// in the report would instead stall the session whenever the report and
    /// the counter disagreed.
    Ended {
        entry: EntryId,
    },
    /// The sender's own row: listening, buffering or not following. Only the
    /// host marks anyone away, when their connection drops.
    SetState {
        state: ParticipantState,
    },
    /// Host only.
    SetRights {
        rights: Rights,
    },
    /// Host only: send a participant away for good.
    RemoveParticipant {
        participant: ParticipantId,
    },
    /// A guest leaving on purpose. The host ends the session instead.
    Leave,
    /// Host only.
    EndSession,
}

/// What a participant that reconnects is sent.
#[derive(Debug, Clone, PartialEq)]
pub enum CatchUp {
    /// It has everything.
    UpToDate,
    /// The ops it missed, in order.
    Ops(Vec<Sequenced>),
    /// Too much was missed, or what it claims to have is not what the host
    /// sent: start again from this.
    Snapshot(Snapshot),
}

/// A session, as its host holds it.
#[derive(Debug, Clone)]
pub struct Host {
    state: SessionState,
    seq: u64,
    log: VecDeque<Sequenced>,
    host: ParticipantId,
    next_participant: u32,
    /// Every entry id the session has seen, removed ones included: an id is
    /// never readable again once it has been revoked.
    seen: HashSet<EntryId>,
}

impl Host {
    /// Open a session named `name`, hosted by `host_name`, at `now_ms` of the
    /// host's clock.
    pub fn new(name: &str, host_name: &str, now_ms: u64) -> Result<Self, Refusal> {
        let name = clean(name, MAX_NAME_CHARS);
        let host_name = clean(host_name, MAX_NAME_CHARS);
        if name.is_empty() || host_name.is_empty() {
            return Err(Refusal::InvalidName);
        }
        let host = ParticipantId(1);
        let state = SessionState::new(
            name,
            Participant {
                id: host,
                name: host_name,
                role: Role::Host,
                state: ParticipantState::Listening,
            },
            now_ms,
        );
        Ok(Self {
            state,
            seq: 0,
            log: VecDeque::new(),
            host,
            next_participant: 2,
            seen: HashSet::new(),
        })
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// The last op made; 0 before the first.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The host's own participant id.
    pub fn host_id(&self) -> ParticipantId {
        self.host
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            seq: self.seq,
            state: self.state.clone(),
        }
    }

    /// Let someone in, once the code and the host's approval are settled (the
    /// app's part). They start from [`Self::snapshot`] taken after this.
    pub fn admit(&mut self, name: &str) -> Result<(ParticipantId, Vec<Sequenced>), Refusal> {
        self.live()?;
        let name = clean(name, MAX_NAME_CHARS);
        if name.is_empty() {
            return Err(Refusal::InvalidName);
        }
        if self.state.participants().len() >= MAX_PARTICIPANTS {
            return Err(Refusal::SessionFull);
        }
        let id = ParticipantId(self.next_participant);
        self.next_participant += 1;
        let ops = self.commit(vec![Op::ParticipantJoined {
            participant: Participant {
                id,
                name,
                role: Role::Guest,
                state: ParticipantState::Listening,
            },
        }]);
        Ok((id, ops))
    }

    /// A guest's connection dropped: they are away, and their entries are
    /// skipped until they come back. What plays keeps playing from what the
    /// listeners already hold; the host's player reports its end as usual.
    pub fn connection_lost(&mut self, who: ParticipantId) -> Vec<Sequenced> {
        self.set_state(who, ParticipantState::Away)
    }

    /// The same guest is back, on a new connection the app authenticated.
    pub fn reconnected(&mut self, who: ParticipantId) -> Vec<Sequenced> {
        self.set_state(who, ParticipantState::Listening)
    }

    fn set_state(&mut self, who: ParticipantId, state: ParticipantState) -> Vec<Sequenced> {
        match self.state.participant(who) {
            Some(p) if p.role == Role::Guest && p.state != state && !self.state.has_ended() => self
                .commit(vec![Op::ParticipantStateChanged {
                    participant: who,
                    state,
                }]),
            _ => Vec::new(),
        }
    }

    /// What a participant that has applied every op up to `after` is missing.
    pub fn catch_up(&self, after: u64) -> CatchUp {
        if after == self.seq {
            return CatchUp::UpToDate;
        }
        // The log holds ops `oldest..=seq`; `after` must be at most one
        // before the oldest for the rest to follow on.
        let oldest = self.log.front().map_or(self.seq + 1, |s| s.seq);
        if after > self.seq || after + 1 < oldest {
            return CatchUp::Snapshot(self.snapshot());
        }
        CatchUp::Ops(self.log.iter().filter(|s| s.seq > after).cloned().collect())
    }

    /// Answer a request from `from` at `now_ms` of the host's clock: the ops
    /// it made, possibly none, or why not.
    pub fn handle(
        &mut self,
        from: ParticipantId,
        action: Action,
        now_ms: u64,
    ) -> Result<Vec<Sequenced>, Refusal> {
        self.live()?;
        let sender = self
            .state
            .participant(from)
            .ok_or(Refusal::UnknownParticipant)?;
        let is_host = sender.role == Role::Host;
        let away = sender.state == ParticipantState::Away;
        let controls = is_host || self.state.rights().everyone_controls_playback;

        let ops = match action {
            Action::Add { entries, position } => {
                if !is_host && position != Position::End {
                    return Err(Refusal::NotAllowed);
                }
                if away {
                    return Err(Refusal::NotAllowed);
                }
                self.add(from, entries, position)?
            }
            Action::Remove { entry } => {
                let owner = self.state.queue()[self.index(entry)?].owner;
                if !is_host && owner != from {
                    return Err(Refusal::NotAllowed);
                }
                self.remove(entry, now_ms)
            }
            Action::Move { entry, position } => {
                if !is_host {
                    return Err(Refusal::NotAllowed);
                }
                let index = self.index(entry)?;
                if index < self.state.next_slot() || position == Position::After(entry) {
                    return Err(Refusal::NotAllowed);
                }
                if let Position::After(after) = position {
                    let at = self.index(after)?;
                    if at < self.state.cursor() {
                        return Err(Refusal::EntryGone);
                    }
                }
                vec![Op::EntryMoved { entry, position }]
            }
            Action::Play { entry } => {
                if !controls {
                    return Err(Refusal::NotAllowed);
                }
                self.play(entry, now_ms)?
            }
            Action::Pause => {
                if !controls {
                    return Err(Refusal::NotAllowed);
                }
                let anchor = self.state.anchor();
                if anchor.entry.is_none() || !anchor.playing {
                    Vec::new()
                } else {
                    // Not past the end: a pause that arrives after the
                    // track ended, before its end was reported, stops on its
                    // last millisecond.
                    let duration = self.state.current().map_or(0, |e| e.duration_ms);
                    self.transport(
                        self.state.cursor(),
                        anchor.entry,
                        anchor.position_at(now_ms).min(duration),
                        now_ms,
                        false,
                    )
                }
            }
            Action::Seek { position_ms } => {
                if !controls {
                    return Err(Refusal::NotAllowed);
                }
                let current = self.state.current().ok_or(Refusal::NothingToPlay)?;
                let position = position_ms.min(current.duration_ms);
                let anchor = self.state.anchor();
                self.transport(
                    self.state.cursor(),
                    anchor.entry,
                    position,
                    now_ms,
                    anchor.playing,
                )
            }
            Action::Next => {
                if !controls {
                    return Err(Refusal::NotAllowed);
                }
                self.advance(now_ms, true)
            }
            Action::Previous => {
                if !controls {
                    return Err(Refusal::NotAllowed);
                }
                let anchor = self.state.anchor();
                if anchor.entry.is_none() {
                    return Err(Refusal::NothingToPlay);
                }
                self.transport(
                    self.state.cursor(),
                    anchor.entry,
                    RESTART_MS,
                    now_ms,
                    anchor.playing,
                )
            }
            Action::Ended { entry } => {
                if !is_host {
                    return Err(Refusal::NotAllowed);
                }
                if self.state.anchor().entry != Some(entry) {
                    Vec::new()
                } else {
                    let playing = self.state.anchor().playing;
                    self.advance(now_ms, playing)
                }
            }
            Action::SetState { state } => {
                // Away is the host's to set and to clear: a message still in
                // flight from a connection the host saw drop does not bring
                // its sender back, only `reconnected` does.
                if state == ParticipantState::Away || away {
                    return Err(Refusal::NotAllowed);
                }
                if self.state.participant(from).map(|p| p.state) == Some(state) {
                    Vec::new()
                } else {
                    vec![Op::ParticipantStateChanged {
                        participant: from,
                        state,
                    }]
                }
            }
            Action::SetRights { rights } => {
                if !is_host {
                    return Err(Refusal::NotAllowed);
                }
                if rights == self.state.rights() {
                    Vec::new()
                } else {
                    vec![Op::RightsChanged { rights }]
                }
            }
            Action::RemoveParticipant { participant } => {
                if !is_host || participant == self.host {
                    return Err(Refusal::NotAllowed);
                }
                if self.state.participant(participant).is_none() {
                    return Err(Refusal::UnknownParticipant);
                }
                self.depart(participant, true)
            }
            Action::Leave => {
                if is_host {
                    return Err(Refusal::NotAllowed);
                }
                self.depart(from, false)
            }
            Action::EndSession => {
                if !is_host {
                    return Err(Refusal::NotAllowed);
                }
                vec![Op::Ended]
            }
        };
        Ok(self.commit(ops))
    }

    /// End the session from the host's side, whatever the reason (the host
    /// quit, offline mode came on).
    pub fn end(&mut self) -> Vec<Sequenced> {
        if self.state.has_ended() {
            return Vec::new();
        }
        self.commit(vec![Op::Ended])
    }

    fn live(&self) -> Result<(), Refusal> {
        if self.state.has_ended() {
            Err(Refusal::SessionEnded)
        } else {
            Ok(())
        }
    }

    fn index(&self, entry: EntryId) -> Result<usize, Refusal> {
        self.state.index_of(entry).ok_or(Refusal::EntryGone)
    }

    fn add(
        &mut self,
        owner: ParticipantId,
        entries: Vec<Entry>,
        position: Position,
    ) -> Result<Vec<Op>, Refusal> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        if self.state.upcoming().len() + entries.len() > MAX_UPCOMING {
            return Err(Refusal::QueueFull);
        }
        if let Position::After(after) = position {
            if self.index(after)? < self.state.cursor() {
                return Err(Refusal::EntryGone);
            }
        }
        let mut ids = HashSet::new();
        let mut admitted = Vec::with_capacity(entries.len());
        for entry in entries {
            if self.seen.contains(&entry.id) || !ids.insert(entry.id) {
                return Err(Refusal::DuplicateEntry);
            }
            admitted.push(entry.admitted(owner)?);
        }
        self.seen.extend(ids);
        Ok(vec![Op::EntriesAdded {
            entries: admitted,
            position,
        }])
    }

    /// Ops removing `entry`; removing what plays moves on to the next
    /// playable entry, keeping the play / pause state.
    fn remove(&self, entry: EntryId, now_ms: u64) -> Vec<Op> {
        let anchor = self.state.anchor();
        let mut ops = vec![Op::EntryRemoved { entry }];
        if anchor.entry == Some(entry) {
            let mut after = self.state.clone();
            // Infallible: the entry was just found in this same state.
            let _ = after.apply(&ops[0]);
            ops.push(next_transport(
                &after,
                after.cursor(),
                now_ms,
                anchor.playing,
            ));
        }
        ops
    }

    fn play(&self, entry: Option<EntryId>, now_ms: u64) -> Result<Vec<Op>, Refusal> {
        let anchor = self.state.anchor();
        match entry {
            Some(entry) => {
                let index = self.index(entry)?;
                if index < self.state.cursor() {
                    return Err(Refusal::EntryGone);
                }
                if !self.owner_present(&self.state.queue()[index]) {
                    return Err(Refusal::NothingToPlay);
                }
                Ok(self.transport(index, Some(entry), 0, now_ms, true))
            }
            None if anchor.entry.is_some() => {
                if anchor.playing {
                    return Ok(Vec::new());
                }
                // A paused entry whose owner has gone away or left cannot be
                // resumed: its bytes may stop at any point. Resuming moves on
                // to the next entry that can play, as the end of it would.
                let resumable = self
                    .state
                    .current()
                    .is_some_and(|current| self.owner_present(current));
                if !resumable {
                    return match self.playable_from(self.state.next_slot()) {
                        Some(_) => Ok(self.advance(now_ms, true)),
                        None => Err(Refusal::NothingToPlay),
                    };
                }
                Ok(self.transport(
                    self.state.cursor(),
                    anchor.entry,
                    anchor.position_ms,
                    now_ms,
                    true,
                ))
            }
            None => match self.playable_from(self.state.cursor()) {
                Some(index) => {
                    let id = self.state.queue()[index].id;
                    Ok(self.transport(index, Some(id), 0, now_ms, true))
                }
                None => Err(Refusal::NothingToPlay),
            },
        }
    }

    /// Move to the next playable entry after what plays, or stop at the end.
    fn advance(&self, now_ms: u64, playing: bool) -> Vec<Op> {
        vec![next_transport(
            &self.state,
            self.state.next_slot(),
            now_ms,
            playing,
        )]
    }

    fn transport(
        &self,
        cursor: usize,
        entry: Option<EntryId>,
        position_ms: u64,
        now_ms: u64,
        playing: bool,
    ) -> Vec<Op> {
        vec![Op::Transport {
            cursor,
            anchor: Anchor {
                entry,
                position_ms,
                at_ms: now_ms,
                playing: playing && entry.is_some(),
            },
        }]
    }

    fn playable_from(&self, from: usize) -> Option<usize> {
        playable_from(&self.state, from)
    }

    fn owner_present(&self, entry: &Entry) -> bool {
        owner_present(&self.state, entry)
    }

    /// Ops for `who` leaving for good: the entries they still had to play go
    /// with them; one already playing finishes from what the listeners hold.
    fn depart(&self, who: ParticipantId, removed: bool) -> Vec<Op> {
        let current = self.state.anchor().entry;
        let mut ops: Vec<Op> = self
            .state
            .upcoming()
            .iter()
            .filter(|e| e.owner == who && Some(e.id) != current)
            .map(|e| Op::EntryRemoved { entry: e.id })
            .collect();
        ops.push(Op::ParticipantLeft {
            participant: who,
            removed,
        });
        ops
    }

    /// Number `ops`, apply them and keep them in the log. They were built
    /// from the current state, so applying cannot fail; if it ever did, the
    /// op is dropped rather than broadcast, so no replica is told something
    /// the host does not hold.
    fn commit(&mut self, ops: Vec<Op>) -> Vec<Sequenced> {
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            if self.state.apply(&op).is_err() {
                debug_assert!(false, "the host built an op it cannot apply: {op:?}");
                continue;
            }
            self.seq += 1;
            let sequenced = Sequenced { seq: self.seq, op };
            self.log.push_back(sequenced.clone());
            if self.log.len() > LOG_KEPT {
                self.log.pop_front();
            }
            out.push(sequenced);
        }
        out
    }
}

/// Whether `entry`'s owner is in the session and connected.
fn owner_present(state: &SessionState, entry: &Entry) -> bool {
    state
        .participant(entry.owner)
        .is_some_and(|p| p.state != ParticipantState::Away)
}

/// The first entry at or after `from` whose owner is present.
fn playable_from(state: &SessionState, from: usize) -> Option<usize> {
    (from..state.queue().len()).find(|&i| owner_present(state, &state.queue()[i]))
}

/// The transport to the first playable entry at or after `from`, or to the
/// end of the queue with nothing playing. Entries skipped on the way count
/// as played.
fn next_transport(state: &SessionState, from: usize, now_ms: u64, playing: bool) -> Op {
    match playable_from(state, from) {
        Some(index) => Op::Transport {
            cursor: index,
            anchor: Anchor {
                entry: Some(state.queue()[index].id),
                position_ms: 0,
                at_ms: now_ms,
                playing,
            },
        },
        None => Op::Transport {
            cursor: state.queue().len(),
            anchor: Anchor::idle(now_ms),
        },
    }
}
