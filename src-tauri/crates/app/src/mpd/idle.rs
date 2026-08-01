//! The `idle` command: change notification without polling.
//!
//! An MPD client issues `idle` and the server holds the connection open
//! until something the client cares about changes, then answers with
//! the list of changed subsystems. Without it clients fall back to
//! hammering `status` once a second, which is what makes a naive MPD
//! implementation feel like a battery drain on a phone remote.
//!
//! WaveFlow already broadcasts every relevant change as a Tauri event
//! for the frontend, so this module is a translation layer: subscribe
//! to those events once per server, fan them out to every idling
//! connection over a [`tokio::sync::broadcast`] channel.

use tokio::sync::broadcast;

/// The MPD subsystems we can actually signal.
///
/// The protocol defines more (`database`, `update`, `stored_playlist`,
/// `sticker`, `subscription`, `message`, `partition`, `neighbor`), but
/// advertising a subsystem we never fire would make a client wait
/// forever on it. Only what we emit is listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subsystem {
    /// Play state, current song, elapsed position.
    Player,
    /// Queue contents or ordering.
    Playlist,
    /// Volume.
    Mixer,
    /// repeat / random / single / consume.
    Options,
    /// Audio output device changed.
    Output,
}

impl Subsystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Playlist => "playlist",
            Self::Mixer => "mixer",
            Self::Options => "options",
            Self::Output => "output",
        }
    }

    /// Parse a subsystem name from an `idle` argument list. Unknown
    /// names return `None`; the caller drops them, which is what MPD
    /// does — an `idle` naming only unknown subsystems simply never
    /// wakes up.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "player" => Some(Self::Player),
            "playlist" => Some(Self::Playlist),
            "mixer" => Some(Self::Mixer),
            "options" => Some(Self::Options),
            "output" => Some(Self::Output),
            _ => None,
        }
    }

    /// Every subsystem, for the `commands`/`tagtypes`-style listings and
    /// for a bare `idle` with no arguments (which means "any").
    pub const ALL: [Subsystem; 5] = [
        Self::Player,
        Self::Playlist,
        Self::Mixer,
        Self::Options,
        Self::Output,
    ];
}

/// Fan-out channel. Cloned into every connection task.
///
/// Capacity is generous because a lagging receiver loses messages, and
/// a lost message means a client's `idle` misses a change and shows
/// stale state until the next one. 64 covers a burst of track changes
/// during a rapid skip without any realistic chance of overflow.
const CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Clone)]
pub struct IdleBus {
    tx: broadcast::Sender<Subsystem>,
}

impl IdleBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { tx }
    }

    /// Signal a change. Fails silently when nobody is idling, which is
    /// the common case — `broadcast::send` errors only on zero
    /// receivers and that is not a problem worth logging.
    pub fn notify(&self, subsystem: Subsystem) {
        let _ = self.tx.send(subsystem);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Subsystem> {
        self.tx.subscribe()
    }
}

impl Default for IdleBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Wait for a change in any of `wanted`, or until `cancel` fires.
///
/// Returns the subsystems that changed. An empty vec means the wait was
/// cancelled (the client sent `noidle`, or the server is shutting down)
/// and the caller should answer a bare `OK`.
///
/// After the first hit it drains whatever else is already queued, so a
/// track change that moves both `player` and `playlist` answers once
/// with both rather than waking the client twice.
pub async fn wait(
    rx: &mut broadcast::Receiver<Subsystem>,
    wanted: &[Subsystem],
    cancel: &tokio_util::sync::CancellationToken,
) -> Vec<Subsystem> {
    let mut hits: Vec<Subsystem> = Vec::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Vec::new(),
            received = rx.recv() => match received {
                Ok(subsystem) => {
                    if wanted.contains(&subsystem) && !hits.contains(&subsystem) {
                        hits.push(subsystem);
                    }
                    if hits.is_empty() {
                        continue;
                    }
                    // Coalesce the rest of the burst without blocking:
                    // `try_recv` drains what is already buffered and
                    // then stops.
                    while let Ok(extra) = rx.try_recv() {
                        if wanted.contains(&extra) && !hits.contains(&extra) {
                            hits.push(extra);
                        }
                    }
                    return hits;
                }
                // Lagged: we lost messages, so we cannot know what
                // changed. Reporting everything the client asked about
                // is the safe answer — it re-reads and converges.
                Err(broadcast::error::RecvError::Lagged(_)) => return wanted.to_vec(),
                Err(broadcast::error::RecvError::Closed) => return Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn parses_known_subsystems_and_rejects_the_rest() {
        assert_eq!(Subsystem::parse("player"), Some(Subsystem::Player));
        assert_eq!(Subsystem::parse("mixer"), Some(Subsystem::Mixer));
        // We never fire `database`, so claiming to support it would
        // leave a client idling on it forever.
        assert_eq!(Subsystem::parse("database"), None);
        assert_eq!(Subsystem::parse("nonsense"), None);
    }

    #[tokio::test]
    async fn wakes_on_a_wanted_subsystem() {
        let bus = IdleBus::new();
        let mut rx = bus.subscribe();
        bus.notify(Subsystem::Player);
        let hits = wait(&mut rx, &[Subsystem::Player], &CancellationToken::new()).await;
        assert_eq!(hits, vec![Subsystem::Player]);
    }

    #[tokio::test]
    async fn ignores_subsystems_the_client_did_not_ask_for() {
        let bus = IdleBus::new();
        let mut rx = bus.subscribe();
        bus.notify(Subsystem::Mixer);
        bus.notify(Subsystem::Player);
        // Only `player` was requested, so the mixer event must not wake
        // this client.
        let hits = wait(&mut rx, &[Subsystem::Player], &CancellationToken::new()).await;
        assert_eq!(hits, vec![Subsystem::Player]);
    }

    #[tokio::test]
    async fn coalesces_a_burst_into_one_answer() {
        let bus = IdleBus::new();
        let mut rx = bus.subscribe();
        // A track change moves both at once; the client should be woken
        // once carrying both, not twice.
        bus.notify(Subsystem::Player);
        bus.notify(Subsystem::Playlist);
        let hits = wait(
            &mut rx,
            &[Subsystem::Player, Subsystem::Playlist],
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(hits.len(), 2);
        assert!(hits.contains(&Subsystem::Player));
        assert!(hits.contains(&Subsystem::Playlist));
    }

    #[tokio::test]
    async fn cancellation_returns_empty() {
        let bus = IdleBus::new();
        let mut rx = bus.subscribe();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let hits = wait(&mut rx, &Subsystem::ALL, &cancel).await;
        assert!(hits.is_empty());
    }
}
