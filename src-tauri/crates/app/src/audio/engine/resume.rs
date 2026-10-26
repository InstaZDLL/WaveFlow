//! What a rebuild puts back or parks: the load the decoder accepted
//! (#634), the session parked instead of resumed (#611, #617), and the
//! one-resume-at-a-time guard (#609).

use std::path::PathBuf;
use std::time::Instant;

use super::super::replay_gain::TrackGain;
use super::command::{AudioCmd, LoadIntent};
use super::rebuild::DEVICE_LOSS_OWNERSHIP;
use super::AudioEngine;

/// A session a rebuild parked — the load, and where to pick it up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParkedSession {
    pub(super) load: LastLoad,
    pub(super) start_ms: u64,
}

impl ParkedSession {
    /// The track this park is holding, for the caller that has to decide
    /// whether it is still the right thing to resume.
    pub(crate) fn intent(&self) -> LoadIntent {
        self.load.intent
    }

    /// The command that resumes it, under a **fresh** intent: unlike a
    /// rebuild's own resume, this one is a user action, and it has to
    /// outrank anything claimed before it.
    ///
    /// A park in the track's last two seconds starts it over, as the
    /// persisted resume point does (#898). A stream carries no position
    /// worth adjusting: its resume starts where the server is.
    pub(crate) fn into_command(self, intent: LoadIntent) -> AudioCmd {
        let start_ms = match &self.load.source {
            LastLoadSource::Local { duration_ms, .. }
            | LastLoadSource::RemoteFile { duration_ms, .. } => {
                crate::queue::resume_position(self.start_ms, *duration_ms)
            }
            LastLoadSource::Url { .. } => self.start_ms,
        };
        self.load.into_command(intent, start_ms)
    }
}

/// What the decoder is playing, captured when it accepts a load (#634).
///
/// The three output-rebuild paths ([`AudioEngine::set_output_device`],
/// [`AudioEngine::set_exclusive_output`],
/// [`AudioEngine::force_rebuild_output`]) have to put back what the `Stop`
/// they send takes away, and each of them used to re-dispatch a snapshot
/// taken *before* that stop. A track the user picked in between was
/// therefore loaded, unloaded by the stop, and its only replacement — the
/// rebuild's own, older resume — was dropped on arrival by `accept_load`,
/// correctly, because the user's intent was the newer one. Nothing played,
/// while the player bar named the track the user had just picked.
///
/// So a rebuild resumes **what the decoder accepted**, payload and all,
/// rather than what was current when it started. Two properties make that
/// safe:
///
/// - it is written by the decoder at the moment of receipt, so it can
///   never name a load that was superseded on the way in, and it covers
///   the auto-advance, whose `LoadAndPlay` goes straight down the channel
///   without passing through [`AudioEngine::send`];
/// - it is re-dispatched **with its own intent**, never a fresh one. An
///   intent already delivered is accepted again — the decoder drops what
///   is *older* than the newest it has seen, and this is not older — while
///   a genuinely newer pick still supersedes it. Minting a new intent here
///   would do the opposite: it would outrank a pick that claimed before us
///   and had not reached the channel yet, which is the defect #632 closed
///   one step earlier.
#[derive(Debug, Clone, PartialEq)]
pub struct LastLoad {
    /// The intent this load carried, re-emitted as-is — see above.
    pub(crate) intent: LoadIntent,
    pub(crate) track_id: i64,
    /// Where the load itself asked to start. Used when the live position
    /// cannot be paired with this load — see [`resume_start_ms`].
    pub(crate) start_ms: u64,
    pub(crate) title: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) artwork_url: Option<String>,
    pub(crate) source: LastLoadSource,
}

/// The payload half of [`LastLoad`], one variant per load command.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LastLoadSource {
    /// A library track, keyed by its own row.
    Local {
        path: PathBuf,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
        replay_gain: TrackGain,
    },
    Url {
        url: String,
        ext_hint: Option<String>,
        /// Carried so a device flap mid-stream resumes at the same
        /// level. Empty for a live station; set when a library track
        /// is being streamed from the server after its local file
        /// failed to open.
        replay_gain: TrackGain,
        /// Carried so a finite stream comes back finite — see
        /// [`AudioCmd::LoadUrlAndPlay`].
        duration_ms: Option<u64>,
        seekable_file: bool,
    },
    RemoteFile {
        origin: DetachedOrigin,
        path: PathBuf,
        duration_ms: u64,
        fallback_url: Option<String>,
        replay_gain: TrackGain,
    },
}

/// Whose session a file played by [`AudioCmd::LoadRemoteFileAndPlay`]
/// belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachedOrigin {
    /// A track of the remote queue (RFC-005).
    Remote,
    /// A file the user opened without importing it
    /// ([`crate::external_playback`]).
    File,
}

impl LastLoad {
    /// Capture a load command, or `None` for anything that is not one.
    ///
    /// Free of the engine on purpose, so the lifecycle can be unit-tested
    /// without standing up a Tauri [`AppHandle`].
    pub(crate) fn capture(cmd: &AudioCmd) -> Option<Self> {
        match cmd {
            AudioCmd::LoadAndPlay {
                intent,
                path,
                start_ms,
                track_id,
                duration_ms,
                source_type,
                source_id,
                replay_gain,
            } => Some(Self {
                intent: *intent,
                track_id: *track_id,
                start_ms: *start_ms,
                // A library track carries none of the three: the player
                // bar reads them from the `track` row instead.
                title: None,
                artist: None,
                artwork_url: None,
                source: LastLoadSource::Local {
                    path: path.clone(),
                    duration_ms: *duration_ms,
                    source_type: source_type.clone(),
                    source_id: *source_id,
                    replay_gain: *replay_gain,
                },
            }),
            AudioCmd::LoadUrlAndPlay {
                intent,
                url,
                ext_hint,
                track_id,
                title,
                artist,
                artwork_url,
                replay_gain,
                // A cache target belongs to one open response and does not
                // survive into a new one. Everything else about the stream
                // does: flattening it to "radio" here is what would bring a
                // finite server track back forward-only and ICY-parsed, just
                // because the audio device changed.
                cache: _,
                duration_ms,
                seekable_file,
            } => Some(Self {
                intent: *intent,
                track_id: *track_id,
                // A stream starts where the server is, not where we left
                // off; the resume ignores this for `Url` anyway.
                start_ms: 0,
                title: title.clone(),
                artist: artist.clone(),
                artwork_url: artwork_url.clone(),
                source: LastLoadSource::Url {
                    url: url.clone(),
                    ext_hint: ext_hint.clone(),
                    replay_gain: *replay_gain,
                    duration_ms: *duration_ms,
                    seekable_file: *seekable_file,
                },
            }),
            AudioCmd::LoadRemoteFileAndPlay {
                intent,
                origin,
                path,
                start_ms,
                track_id,
                duration_ms,
                title,
                artist,
                artwork_url,
                fallback_url,
                // Recorded whatever it was, and re-dispatched as `false` —
                // see `into_command`.
                discard_on_failure: _,
                replay_gain,
            } => Some(Self {
                intent: *intent,
                track_id: *track_id,
                start_ms: *start_ms,
                title: title.clone(),
                artist: artist.clone(),
                artwork_url: artwork_url.clone(),
                source: LastLoadSource::RemoteFile {
                    origin: *origin,
                    path: path.clone(),
                    duration_ms: *duration_ms,
                    fallback_url: fallback_url.clone(),
                    replay_gain: *replay_gain,
                },
            }),
            _ => None,
        }
    }

    /// Rebuild the command that resumes this load at `start_ms`.
    ///
    /// The intent is the caller's to choose, and the two callers choose
    /// differently: a rebuild passes the load's **own** intent, because
    /// it is putting back what it interrupted and must give way to
    /// anything newer (#634); Play passes a **fresh** one, because a user
    /// action outranks what came before it.
    pub(super) fn into_command(self, intent: LoadIntent, start_ms: u64) -> AudioCmd {
        match self.source {
            LastLoadSource::Local {
                path,
                duration_ms,
                source_type,
                source_id,
                replay_gain,
            } => AudioCmd::LoadAndPlay {
                intent,
                path,
                start_ms,
                track_id: self.track_id,
                duration_ms,
                // The source the track really came from, not the rebuild.
                // A play credited to "device-rebuild" — which is what the
                // three paths used to stamp — hides an album or a playlist
                // play from every statistic that filters on it, for the
                // sole reason that the audio device changed underneath.
                source_type,
                source_id,
                // The gain captured with the load. Re-reading it from the
                // database mid-rebuild, as the old resume did, only ever
                // mattered because that path had to go to the database
                // anyway for the file path.
                replay_gain,
            },
            LastLoadSource::Url {
                url,
                ext_hint,
                replay_gain,
                duration_ms,
                seekable_file,
            } => AudioCmd::LoadUrlAndPlay {
                intent,
                url,
                ext_hint,
                track_id: self.track_id,
                title: self.title,
                artist: self.artist,
                artwork_url: self.artwork_url,
                replay_gain,
                // Not cached on resume: the target belongs to the response
                // that was open, and this is a new one.
                cache: None,
                // Restored, not assumed. Flattening every resumed stream to
                // "radio" would bring a finite server track back forward-only
                // and ICY-parsed, just because the device changed.
                duration_ms,
                seekable_file,
            },
            LastLoadSource::RemoteFile {
                origin,
                path,
                duration_ms,
                fallback_url,
                replay_gain,
            } => AudioCmd::LoadRemoteFileAndPlay {
                intent,
                origin,
                // Never `true` on a resume. The flag authorises deleting the
                // file when it will not decode, and that is a judgement about
                // a copy we just fetched — not about one we are re-opening
                // because the audio device moved. A stream-cache entry that
                // really is bad still falls back to `fallback_url`.
                discard_on_failure: false,
                path,
                start_ms,
                track_id: self.track_id,
                duration_ms,
                title: self.title,
                artist: self.artist,
                artwork_url: self.artwork_url,
                fallback_url,
                replay_gain,
            },
        }
    }
}

/// The playback position, together with what it was the position *of*
/// (#634). Captured by a rebuild before it stops the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LivePosition {
    /// The intent of the load the decoder had accepted at that moment.
    pub(super) intent: Option<LoadIntent>,
    /// The track the decoder had actually started at that moment.
    pub(super) track_id: i64,
    pub(super) position_ms: u64,
}

/// Where a rebuild's resume should start.
///
/// The two halves cannot be read at the same moment. The **position** has
/// to be taken before the stop: opening the replacement writes its own
/// sample rate into the shared block, and a position derived from the old
/// rate's sample count against the new rate is simply a wrong number. The
/// **load** can only be taken after the swap, since the whole point of
/// #634 is to resume what the decoder accepted in between. So they are
/// paired explicitly rather than assumed to match: the position belongs to
/// this load only if the decoder had accepted it (same intent) *and* had
/// started it (same track) when the position was read.
///
/// Otherwise the load's own `start_ms` wins, which for a freshly picked
/// track is the beginning of it. That covers both mismatches: a different
/// track picked during the rebuild, and the same track picked again —
/// where the intent differs even though the id does not, and resuming at
/// the old position would ignore the restart the user asked for.
pub(super) fn resume_start_ms(load: &LastLoad, live: LivePosition) -> u64 {
    if live.intent == Some(load.intent) && live.track_id == load.track_id {
        live.position_ms
    } else {
        load.start_ms
    }
}

impl AudioEngine {
    /// Whether a device that goes away should park playback instead of
    /// letting it move to whatever the system falls back to (#617).
    pub fn pause_on_device_loss(&self) -> bool {
        self.pause_on_device_loss
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Apply the preference. Takes effect on the next device loss;
    /// nothing is rebuilt, since the setting describes what to do when
    /// something else happens.
    pub fn set_pause_on_device_loss(&self, enabled: bool) {
        self.pause_on_device_loss
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record that a device loss was just reported (#617). Called from
    /// [`super::super::output::notify_device_lost`], i.e. by every backend, and
    /// at the moment of the error rather than after the recovery's
    /// backoff — that is what makes it beat the default-change
    /// notification the same unplug raises.
    pub(in crate::audio) fn note_device_loss(&self) {
        if let Ok(mut guard) = self.last_device_loss.lock() {
            *guard = Some(Instant::now());
        }
    }

    /// Whether a device loss is still being recovered — see
    /// [`DEVICE_LOSS_OWNERSHIP`].
    pub(super) fn device_loss_in_flight(&self) -> bool {
        self.last_device_loss
            .lock()
            .ok()
            .and_then(|guard| *guard)
            .is_some_and(|at| at.elapsed() < DEVICE_LOSS_OWNERSHIP)
    }

    /// Take the session a rebuild parked, if it is still the right thing
    /// to resume (#611, #617).
    ///
    /// A park is only valid while nothing newer has claimed the dispatch:
    /// the moment the user picks anything else, this snapshot describes a
    /// session they have moved on from, and handing it to Play would
    /// start the wrong thing.
    /// Read the parked session **without spending it** (#611, #617).
    ///
    /// Split from [`Self::clear_parked_resume`] on purpose: the caller
    /// has to claim the dispatch before it can act on this, and a claim
    /// can fail — or succeed and then be abandoned by whoever won it.
    /// Consuming the park first meant losing the session with nothing
    /// playing, and the next Play falling back to the persisted resume
    /// point, which is always a library track.
    pub(crate) fn peek_parked_resume(&self) -> Option<ParkedSession> {
        let parked = self.parked_resume.lock().ok()?.clone()?;
        if self.load_intent_superseded(parked.intent()) {
            tracing::debug!("parked session superseded by a newer load; dropping it");
            return None;
        }
        Some(parked)
    }

    /// Whether this session counts as paused for a rebuild's decision:
    /// what the decoder has acted on, or what the user has asked for and
    /// it has not reached yet (#611).
    pub(super) fn paused_for_rebuild(&self) -> bool {
        self.shared
            .paused_output
            .load(std::sync::atomic::Ordering::Acquire)
            || self
                .pause_pending
                .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The position, stamped with the load it belongs to (#634).
    ///
    /// Read by a rebuild **before** it stops the decoder — see
    /// [`resume_start_ms`] for why the two halves of a resume cannot be
    /// read at the same moment.
    pub(super) fn live_position(&self) -> LivePosition {
        LivePosition {
            intent: self.shared.last_load_intent(),
            track_id: self
                .shared
                .current_track_id
                .load(std::sync::atomic::Ordering::Acquire),
            position_ms: self.shared.current_position_ms(),
        }
    }

    /// Put back what a rebuild's `Stop` took away (#634).
    ///
    /// Called after the producer swap, with the position captured before
    /// the stop. Re-dispatches the load the decoder accepted — which is
    /// the track the user picked mid-rebuild when there was one, and the
    /// track that was already playing otherwise — carrying that load's own
    /// intent, so a pick that is newer still wins and this one gives way.
    ///
    /// Sends nothing when the decoder has no load on record: a rebuild on
    /// a player that has never loaded anything has nothing to resume.
    pub(super) fn resume_after_rebuild(&self, live: LivePosition) {
        let Some(load) = self.shared.last_load() else {
            tracing::debug!("output rebuild: no load on record, nothing to resume");
            return;
        };
        // Whatever was parked is answered by this resume, and keeping it
        // would let a later Play start a session that is already running.
        self.clear_parked_resume();
        let start_ms = resume_start_ms(&load, live);
        tracing::info!(
            track_id = load.track_id,
            start_ms,
            "resuming the decoder's current load after an output rebuild"
        );
        // Its own intent, never a fresh one — see [`LastLoad`].
        let intent = load.intent;
        let _ = self.cmd_tx.send(load.into_command(intent, start_ms));
    }

    /// Park the session a rebuild interrupted instead of resuming it:
    /// the user had paused it (#611), or its endpoint went away and the
    /// preference says not to carry the music onto another one (#617).
    ///
    /// Three things, and the set matters more than the order:
    ///
    /// - the load is kept, so Play picks *this* session back up. A radio
    ///   stream and a server track both need that: the persisted resume
    ///   point is always a library track's, so parking one of those and
    ///   pressing Play brought back the last library track instead, which
    ///   is why #611 could only ever park a library track;
    /// - the state lands on `Idle`, not `Paused`. The `Stop` this rebuild
    ///   sent unloaded the track and the decoder ignores a `Resume` with
    ///   nothing loaded, so `Paused` on screen would make Play a dead
    ///   button;
    /// - a library track's resume point is written, so the session
    ///   survives a restart too.
    pub(super) fn park_session(&self, live: LivePosition, track_id: i64) {
        if let Some(load) = self.shared.last_load() {
            let start_ms = resume_start_ms(&load, live);
            if let Ok(mut guard) = self.parked_resume.lock() {
                *guard = Some(ParkedSession { load, start_ms });
            }
        }
        super::super::decoder::transition_state(
            &self.shared,
            &self.app,
            super::super::state::PlayerState::Idle,
            Some(track_id),
        );
        // Only a library track has a row to write against; a radio
        // station or a server track is held by the park above and by
        // nothing else.
        if track_id <= 0 {
            return;
        }
        // Pin the write to the profile that was playing, captured without
        // waiting: this runs on a blocking thread, and a lock it cannot
        // take means a switch is under way. That profile must not receive
        // this track's resume point, so the write is skipped (#485).
        use tauri::Manager as _;
        let profile_id = self
            .app
            .state::<crate::state::AppState>()
            .profile
            .try_read()
            .ok()
            .and_then(|active| active.as_ref().map(|p| p.profile_id));
        let Some(profile_id) = profile_id else {
            return;
        };
        let app = self.app.clone();
        let position_ms = live.position_ms;
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::state::AppState>();
            let saved = match state.require_profile_pool_for(Some(profile_id)).await {
                Ok(pool) => crate::queue::persist_resume_point(&pool, track_id, position_ms).await,
                Err(err) => Err(err),
            };
            if let Err(err) = saved {
                tracing::warn!(%err, "output rebuild: paused resume point not saved");
            }
        });
    }

    pub(crate) fn clear_parked_resume(&self) {
        if let Ok(mut guard) = self.parked_resume.lock() {
            *guard = None;
        }
    }

    /// Claim the right to run one resume, or `None` when another is
    /// already in flight (#609).
    ///
    /// The guard clears the slot on every exit path, including the `?`
    /// returns inside [`crate::player_actions::resume_last`] and a panic.
    /// A leaked slot would leave Play dead for the rest of the session,
    /// which is worse than the double load it prevents.
    pub fn begin_resume(&self) -> Option<ResumeGuard<'_>> {
        use std::sync::atomic::Ordering;
        if self.resume_in_flight.swap(true, Ordering::AcqRel) {
            None
        } else {
            Some(ResumeGuard(&self.resume_in_flight))
        }
    }
}

/// Releases the slot [`AudioEngine::begin_resume`] took, when dropped.
pub struct ResumeGuard<'a>(pub(super) &'a std::sync::atomic::AtomicBool);

impl Drop for ResumeGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}
