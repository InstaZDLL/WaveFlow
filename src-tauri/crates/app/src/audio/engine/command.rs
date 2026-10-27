//! The commands the decoder thread accepts, and the intent every load
//! carries (#622).

use std::path::PathBuf;

use rtrb::Producer;

use super::super::replay_gain::TrackGain;
use super::super::state::SharedPlayback;
use super::resume::DetachedOrigin;

/// Sequence number identifying one playback intent (#622).
///
/// Every path that hands a track to the decoder prepares it
/// asynchronously first — a profile snapshot, queue reads, a ReplayGain
/// lookup — so two loads started close together reach the decoder in the
/// order their *preparation* finished, not the order they were asked for.
/// Pressing Play from a stopped player and then Next a few tens of
/// milliseconds later left whichever prepared slower in charge, and the
/// wrong track played.
///
/// Take one with [`AudioEngine::next_load_intent`] **when the intent
/// starts** — before the first await, not just before the send — and carry
/// it into the load command. The decoder drops any load older than the
/// newest one it has already been handed, so a preparation that finishes
/// late is discarded instead of overwriting a newer selection.
///
/// The field is private on purpose: a load command cannot be built without
/// asking the engine for an intent first. Seventeen sites send loads, and
/// serialising only the ones a bug report happens to name would leave the
/// rest reordering exactly as before while reading as though ordering were
/// guaranteed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoadIntent(u64);

impl LoadIntent {
    /// The raw sequence number, for logs and for the decoder's
    /// `AtomicU64` high-water mark.
    pub(crate) fn get(self) -> u64 {
        self.0
    }

    /// Claim the next intent from the engine's shared counter.
    ///
    /// Lives here rather than on [`SharedPlayback`] so the tuple field
    /// stays private to this module: a load command still cannot be built
    /// without asking for an intent. Takes the shared state because the
    /// decoder thread has that and no engine handle — it claims the
    /// auto-advance's intent at the moment a track ends.
    pub(crate) fn claim(shared: &SharedPlayback) -> Self {
        Self(
            shared
                .load_intents
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                + 1,
        )
    }

    /// Build an intent from a raw number — tests only. Production code
    /// goes through [`AudioEngine::next_load_intent`] so every intent on
    /// one engine comes from the same monotonic counter.
    #[cfg(test)]
    pub(crate) fn from_raw(n: u64) -> Self {
        Self(n)
    }
}

/// Commands accepted by the decoder thread.
#[allow(dead_code)]
pub enum AudioCmd {
    LoadAndPlay {
        /// Ordering token for this load — see [`LoadIntent`].
        intent: LoadIntent,
        path: PathBuf,
        start_ms: u64,
        track_id: i64,
        duration_ms: u64,
        /// Identifies where the queue this track came from originated,
        /// so the analytics task can stamp the matching `play_event`
        /// row with the same source for later filtering.
        source_type: String,
        source_id: Option<i64>,
        /// What is known about this track's loudness: the gain in dB
        /// on the ReplayGain 2.0 scale and the linear peak, from the
        /// file's own tags when it has them and from `track_analysis`
        /// otherwise. `TrackGain::default()` carries neither, and the
        /// decoder then applies the user's fallback gain instead of
        /// leaving the signal untouched. Lookup is done at command
        /// time so the decoder thread stays out of the SQLite path.
        replay_gain: TrackGain,
    },
    /// Play a file that is not a library track under a negative
    /// `track_id`: a track of the active remote queue through its confirmed
    /// local reconciliation link, or a file the user opened without
    /// importing it ([`DetachedOrigin`]). The negative id keeps the queue
    /// and UI semantics of that session; `fallback_url`, when available,
    /// lets the decoder fall back to the server if the local file can no
    /// longer be opened between selection and playback.
    LoadRemoteFileAndPlay {
        /// Ordering token for this load — see [`LoadIntent`].
        intent: LoadIntent,
        /// Whose session the file belongs to: what the decoder announces it
        /// as, and which queue its end advances.
        origin: DetachedOrigin,
        path: PathBuf,
        start_ms: u64,
        track_id: i64,
        duration_ms: u64,
        title: Option<String>,
        artist: Option<String>,
        artwork_url: Option<String>,
        fallback_url: Option<String>,
        /// Whether `path` is a reproducible copy that should be discarded if
        /// it will not open or decode.
        ///
        /// True for a stream-cache entry, which can always be fetched again;
        /// false for a reconciled file from the user's own library, which is
        /// theirs and is never ours to delete. Without the distinction a bad
        /// cache entry would fall back to the server on every single play
        /// instead of once.
        discard_on_failure: bool,
        replay_gain: TrackGain,
    },
    Pause,
    Resume,
    Stop,
    Seek(u64),
    SetVolume(f32),
    SetNormalize(bool),
    SetMono(bool),
    /// Update the crossfade window length (ms). 0 disables crossfade.
    SetCrossfade(u32),
    /// Toggle whether the decoder applies the per-track ReplayGain
    /// factor when pushing samples to the ring.
    SetReplayGain(bool),
    /// Toggle gapless playback (sample-accurate hand-off between
    /// consecutive queued tracks when no crossfade is configured).
    SetGapless(bool),
    /// Update the playback speed multiplier. Pushed live so the
    /// decoder rebuilds the active stream's resampler against the
    /// new effective source rate (`actual_rate * speed`).
    SetSpeed(f32),
    /// Left / right balance in percent, `-100` (left) to `100` (right).
    /// Clamped on the way in.
    SetBalance(i32),
    /// Stereo width in percent, `0` (mono) to `200`; `100` is the
    /// source as it is. Clamped on the way in.
    SetStereoWidth(i32),
    /// Hand the decoder thread the next track to prefetch for
    /// crossfade. Sent by the analytics task in response to a
    /// `PrefetchNext` request from the decoder.
    SetNextTrack {
        path: PathBuf,
        track_id: i64,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
        replay_gain: TrackGain,
    },
    /// Play a live HTTP audio stream (Web Radio). The decoder opens
    /// the URL in a blocking client (safe because the decoder thread
    /// is non-tokio), wraps the response in a `HttpMediaSource`, and
    /// reuses the symphonia probe + decode path.
    ///
    /// Distinct from `LoadAndPlay` because:
    /// - `track_id` is a negative sentinel (no library row to write a
    ///   `play_event` against),
    /// - `duration_ms = 0` suppresses end-of-track guards / prefetch /
    ///   auto-advance — the stream runs until the user hits Stop,
    /// - `title` / `artist` / `artwork_url` ride along the command so
    ///   the OS media overlay + Discord RPC + UI can be populated
    ///   without a DB lookup.
    LoadUrlAndPlay {
        /// Ordering token for this load — see [`LoadIntent`].
        intent: LoadIntent,
        url: String,
        /// File-extension hint forwarded to the symphonia probe (e.g.
        /// "mp3", "aac"). Many Icecast streams need this to probe
        /// cleanly because the first bytes aren't an unambiguous
        /// magic — derive from the server's Content-Type when known.
        ext_hint: Option<String>,
        /// Sentinel track id — negative, unique per active radio
        /// session so `current_track_id` reads can still distinguish
        /// streams from one another.
        track_id: i64,
        title: Option<String>,
        artist: Option<String>,
        artwork_url: Option<String>,
        /// Where to cache the bytes this stream reads, when it is a finite
        /// remote-queue track worth keeping. `None` for radio, which is
        /// endless and therefore never complete.
        cache: Option<crate::audio::stream_cache::CacheTarget>,
        /// Total length, when the caller knows it.
        ///
        /// The decoder used to take this from the remote session alone, which
        /// leaves it unknown for a finite stream reached any other way — a
        /// library track falling back to the server has a duration in its own
        /// row, and dropping it costs the seekbar its total.
        duration_ms: Option<u64>,
        /// Force a finite, range-capable open even with no remote session
        /// running.
        ///
        /// The decoder used to answer "file or radio?" from the *global*
        /// remote-session state, which is right for the remote queue and
        /// wrong for anyone else: a library track whose file vanished and
        /// falls back to the server has no session, and would have been
        /// opened with ICY — forward-only, and parsing metadata blocks out
        /// of a FLAC. The question belongs to the command.
        seekable_file: bool,
        /// Loudness metadata for the stream. `TrackGain::default()`
        /// for a live radio station — nothing knows anything about it
        /// — but a library track that fell back to streaming from the
        /// server is still the same recording, so it carries the gain
        /// the local file would have used. Without this the fallback
        /// is an audible level jump.
        replay_gain: TrackGain,
    },
    /// Hand the decoder thread a fresh ring producer after the output
    /// thread was rebuilt on a different cpal device. The decoder
    /// drops its old producer (the consumer is already gone with the
    /// previous output thread) and pushes subsequent samples through
    /// the new one. Always preceded by a `Stop` so the decoder picks
    /// it up from the top-level idle loop, not mid-`play_track`.
    SwapProducer(Producer<f32>),
    /// Play the output test tone ([`super::super::test_tone`]) from the idle
    /// loop. Not a load: it carries no intent, is never recorded as the
    /// last load, and a rebuild never replays it.
    PlayTestTone,
    Shutdown,
}

// `rtrb::Producer` doesn't implement `Debug`, so the auto-derive
// would refuse to compile once `SwapProducer` was added. Hand-rolled
// Debug just prints the variant name + key scalar fields; nothing in
// the audio path actually relies on this output.
impl std::fmt::Debug for AudioCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioCmd::LoadAndPlay {
                track_id,
                start_ms,
                intent,
                ..
            } => write!(
                f,
                "LoadAndPlay {{ track_id: {track_id}, start_ms: {start_ms}, intent: {} }}",
                intent.get()
            ),
            AudioCmd::LoadRemoteFileAndPlay {
                track_id,
                start_ms,
                intent,
                ..
            } => write!(
                f,
                "LoadRemoteFileAndPlay {{ track_id: {track_id}, start_ms: {start_ms}, intent: {} }}",
                intent.get()
            ),
            AudioCmd::Pause => write!(f, "Pause"),
            AudioCmd::Resume => write!(f, "Resume"),
            AudioCmd::Stop => write!(f, "Stop"),
            AudioCmd::Seek(ms) => write!(f, "Seek({ms})"),
            AudioCmd::SetVolume(v) => write!(f, "SetVolume({v})"),
            AudioCmd::SetNormalize(v) => write!(f, "SetNormalize({v})"),
            AudioCmd::SetMono(v) => write!(f, "SetMono({v})"),
            AudioCmd::SetCrossfade(v) => write!(f, "SetCrossfade({v})"),
            AudioCmd::SetReplayGain(v) => write!(f, "SetReplayGain({v})"),
            AudioCmd::SetGapless(v) => write!(f, "SetGapless({v})"),
            AudioCmd::SetSpeed(v) => write!(f, "SetSpeed({v})"),
            AudioCmd::SetBalance(v) => write!(f, "SetBalance({v})"),
            AudioCmd::SetStereoWidth(v) => write!(f, "SetStereoWidth({v})"),
            AudioCmd::SetNextTrack { track_id, .. } => {
                write!(f, "SetNextTrack {{ track_id: {track_id} }}")
            }
            AudioCmd::LoadUrlAndPlay {
                url,
                track_id,
                intent,
                ..
            } => {
                // Redacted, not printed raw: a remote-queue URL carries a
                // bearer stream ticket in its path, and a radio URL may
                // carry userinfo or a `?key=`. This `Debug` is what every `?cmd` /
                // `?err` logger reaches, so the redaction belongs here
                // rather than at each call site.
                write!(
                    f,
                    "LoadUrlAndPlay {{ track_id: {track_id}, url: {}, intent: {} }}",
                    crate::audio::http_source::redact_url(url),
                    intent.get()
                )
            }
            AudioCmd::SwapProducer(_) => write!(f, "SwapProducer(<producer>)"),
            AudioCmd::PlayTestTone => write!(f, "PlayTestTone"),
            AudioCmd::Shutdown => write!(f, "Shutdown"),
        }
    }
}

impl AudioCmd {
    /// The ordering token of a load command, `None` for everything else.
    ///
    /// Only the three loads are arbitrated: they are the commands that
    /// *replace* what is playing. Pause, Seek, a volume change and the
    /// rest act on whatever is current, so they are always meant for the
    /// state they find. `SetNextTrack` is deliberately absent too — it
    /// arms the gapless prefetch rather than taking over, and dropping one
    /// would cost a gapless hand-off to prevent nothing audible.
    pub(crate) fn load_intent(&self) -> Option<LoadIntent> {
        match self {
            AudioCmd::LoadAndPlay { intent, .. }
            | AudioCmd::LoadRemoteFileAndPlay { intent, .. }
            | AudioCmd::LoadUrlAndPlay { intent, .. } => Some(*intent),
            _ => None,
        }
    }
}
