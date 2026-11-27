//! A guest's copy of the session.

use super::{ApplyError, Sequenced, SessionState, Snapshot};

/// The session as a guest holds it: the host's state, as of the last op
/// applied.
#[derive(Debug, Clone)]
pub struct Replica {
    state: SessionState,
    seq: u64,
}

/// What [`Replica::apply`] did with an op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Applied,
    /// The op was already applied (a resend after a reconnection): nothing
    /// changed.
    AlreadyHad,
}

/// Why a replica could not follow. Either way it asks the host for a
/// snapshot and starts again from it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaError {
    #[error("op {got} arrived, {expected} was expected")]
    Gap { expected: u64, got: u64 },
    #[error("the host's op does not apply here: {0}")]
    Diverged(ApplyError),
}

impl Replica {
    /// Start from what the host sent, after checking it is a state the ops
    /// could have produced.
    pub fn from_snapshot(snapshot: Snapshot) -> Result<Self, ApplyError> {
        snapshot.state.check()?;
        Ok(Self {
            state: snapshot.state,
            seq: snapshot.seq,
        })
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// The last op applied: what to send the host when reconnecting.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Apply the next op of the host's sequence. On an error nothing
    /// changed, and the replica needs a snapshot.
    pub fn apply(&mut self, sequenced: &Sequenced) -> Result<Applied, ReplicaError> {
        if sequenced.seq <= self.seq {
            return Ok(Applied::AlreadyHad);
        }
        if sequenced.seq != self.seq + 1 {
            return Err(ReplicaError::Gap {
                expected: self.seq + 1,
                got: sequenced.seq,
            });
        }
        // Applied to a copy so that a refused op leaves the replica as it
        // was: `apply` itself refuses before changing anything, but a
        // replica must not depend on that for every future op.
        let mut next = self.state.clone();
        next.apply(&sequenced.op).map_err(ReplicaError::Diverged)?;
        self.state = next;
        self.seq = sequenced.seq;
        Ok(Applied::Applied)
    }
}
