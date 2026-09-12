//! Audio engine handle — the single `Arc<AudioEngine>` managed by Tauri.
//!
//! At this checkpoint the engine is a no-op: it holds the shared state and
//! a command channel but the decoder thread and cpal output are stubbed.
//! Subsequent checkpoints flesh out the output stream (checkpoint 2),
//! decoder loop (checkpoint 4) and command wiring (checkpoint 9).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use rtrb::Producer;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::unbounded_channel;

use crate::error::{AppError, AppResult};

use super::analytics::{analytics_task, AnalyticsMsg};
use super::decoder::spawn_decoder_thread;
use super::output::{spawn_output_with_mode, DopFormat, OutputHandle};
use super::replay_gain::TrackGain;
use super::state::SharedPlayback;

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
    /// Play a track from the active remote queue using its confirmed local
    /// reconciliation link. The negative `track_id` deliberately preserves
    /// the remote queue/UI semantics; `fallback_url`, when available, lets
    /// the decoder fall back to the server if the local file can no longer
    /// be opened between selection and playback.
    LoadRemoteFileAndPlay {
        /// Ordering token for this load — see [`LoadIntent`].
        intent: LoadIntent,
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
            AudioCmd::SetNextTrack { track_id, .. } => {
                write!(f, "SetNextTrack {{ track_id: {track_id} }}")
            }
            AudioCmd::LoadUrlAndPlay {
                url,
                track_id,
                intent,
                ..
            } => {
                write!(
                    f,
                    "LoadUrlAndPlay {{ track_id: {track_id}, url: {url}, intent: {} }}",
                    intent.get()
                )
            }
            AudioCmd::SwapProducer(_) => write!(f, "SwapProducer(<producer>)"),
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

/// A WASAPI-exclusive "flap storm" (#322) is this many `DeviceNotAvailable`
/// rebuilds within [`EXCLUSIVE_FLAP_WINDOW`]. Some onboard codecs (Realtek)
/// reset the device right after an exclusive grab, so every re-engage
/// re-triggers the reset and the engine thrashes forever.
const EXCLUSIVE_FLAP_THRESHOLD: u32 = 3;
/// Time window over which [`EXCLUSIVE_FLAP_THRESHOLD`] flaps trip the
/// session-level exclusive suppression.
const EXCLUSIVE_FLAP_WINDOW: Duration = Duration::from_secs(12);

/// Rolling counter of exclusive-mode device flaps, behind
/// [`AudioEngine::try_rebuild_after_device_error`].
#[derive(Default)]
struct FlapWindow {
    count: u32,
    window_start: Option<Instant>,
}

impl FlapWindow {
    /// Record one flap at `now` and report whether it trips the storm
    /// threshold ([`EXCLUSIVE_FLAP_THRESHOLD`] flaps within
    /// [`EXCLUSIVE_FLAP_WINDOW`]). A flap outside the current window
    /// restarts the count at 1. Clock is injected so the logic is
    /// deterministic under test.
    fn record(&mut self, now: Instant) -> bool {
        let within_window = self
            .window_start
            .is_some_and(|start| now.duration_since(start) <= EXCLUSIVE_FLAP_WINDOW);
        if within_window {
            self.count += 1;
        } else {
            self.window_start = Some(now);
            self.count = 1;
        }
        self.count >= EXCLUSIVE_FLAP_THRESHOLD
    }
}

/// Quiet period after a rebuild attempt finishes during which a fresh
/// `DeviceNotAvailable` does NOT arm another rebuild (#365).
///
/// On a DAC that resets its audio session every time a client grabs it in
/// exclusive mode, our own re-open is what produces the next error — so
/// reacting to it re-arms the loop. Each pass would then open exclusive
/// while the previous pass's stream was still settling, and the second
/// open collides with `AUDCLNT_E_DEVICE_IN_USE (0x8889000A)`, whose shared
/// fallback then fails too, leaving the engine and the Settings toggle
/// inconsistent (the #355 report).
///
/// 2 s is long enough to cover such a self-inflicted reset and short
/// enough that a genuine unplug still recovers promptly.
const REBUILD_SETTLE_WINDOW: Duration = Duration::from_secs(2);

/// Single-owner gate for the automatic post-`DeviceNotAvailable` rebuild
/// (#365).
///
/// The `rebuild_in_progress` debounce only covers a rebuild *call that is
/// currently executing*. It cannot coalesce the real-world pattern, because
/// the recovery is **deferred**: the cpal error callback schedules the
/// rebuild 300 ms later, so consecutive errors each queue their own task
/// and the passes never overlap — one finishes before the next begins, so
/// every one of them sees a free debounce slot.
///
/// This gate closes that hole by tracking the whole arm → delay → run
/// lifecycle plus a settle window afterwards, so exactly one rebuild runs
/// per burst of device errors.
///
/// Clock is injected so the logic is deterministic under test, same as
/// [`FlapWindow`].
#[derive(Default)]
struct RebuildGate {
    /// A deferred rebuild has been scheduled and hasn't finished yet.
    armed: bool,
    /// When the last rebuild attempt finished, successful or not.
    last_finished: Option<Instant>,
}

impl RebuildGate {
    /// Decide whether a `DeviceNotAvailable` should schedule a rebuild.
    /// Returns `true` at most once per burst: `false` while one is already
    /// armed, and `false` inside the settle window after the last one.
    fn try_arm(&mut self, now: Instant, settle: Duration) -> bool {
        if self.armed {
            return false;
        }
        if self
            .last_finished
            .is_some_and(|t| now.duration_since(t) < settle)
        {
            return false;
        }
        self.armed = true;
        true
    }

    /// Mark the armed rebuild as finished and start the settle window.
    /// Must run on EVERY exit path of the rebuild, otherwise `armed`
    /// stays latched and no later device error can ever recover.
    fn finish(&mut self, now: Instant) {
        self.armed = false;
        self.last_finished = Some(now);
    }

    /// Drop the settle window entirely so the very next [`try_arm`]
    /// succeeds. Used when a deliberate output change we suppressed for
    /// (via [`AudioEngine::begin_deliberate_output_change`]) turns out
    /// to have installed no stream at all: the recovery we then schedule
    /// must not be swallowed by the window we opened for a swap that
    /// never happened (#405).
    fn reopen(&mut self) {
        self.armed = false;
        self.last_finished = None;
    }
}

/// Handle stored in Tauri state. Cloning an `Arc<AudioEngine>` is cheap.
///
/// The cpal `Stream` is NOT stored here — it lives on a dedicated output
/// thread (see [`spawn_output_thread`]) so the `!Send` platform handles
/// never cross a thread boundary. The engine retains the join / shutdown
/// handle inside `output`, plus the decoder thread's join handle inside
/// `decoder`. Neither thread is exposed to Tauri command code, which
/// only sees `cmd_tx` and `shared`.
pub struct AudioEngine {
    cmd_tx: Sender<AudioCmd>,
    pub(crate) shared: Arc<SharedPlayback>,
    output: Mutex<Option<OutputHandle>>,
    decoder: Mutex<Option<JoinHandle<()>>>,
    /// AppHandle clone so we can rebuild the cpal output thread from
    /// `set_output_device` without plumbing the handle through every
    /// Tauri command call site.
    app: AppHandle,
    /// Opt-in: own the output device outright rather than share it
    /// with the system mixer — WASAPI Exclusive Mode on Windows, a raw
    /// `hw:` device on Linux, hog mode on macOS. Read at boot from
    /// `profile_setting['audio.exclusive_output']`, flipped by
    /// `set_exclusive_output`. Used by `set_output_device` to preserve
    /// the mode across hot-swaps.
    exclusive_output: std::sync::atomic::AtomicBool,
    /// Whether the current output stream really owns its device. This
    /// can differ from the preference when init falls back to cpal
    /// shared mode.
    exclusive_output_active: std::sync::atomic::AtomicBool,
    /// Debounce guard for [`Self::try_rebuild_after_device_error`]
    /// (#175). Windows session resets and USB DAC flaps fire the
    /// cpal `DeviceNotAvailable` callback on a random thread; the
    /// callback schedules a rebuild via tokio, and a quick double
    /// flap would otherwise queue two concurrent rebuilds that
    /// each interrupt the same track.
    rebuild_in_progress: std::sync::atomic::AtomicBool,
    /// One resume at a time (#609). Two Play events landing together — a
    /// double tap on the OS overlay, a client sending `play` twice — both
    /// read `Idle` and both spawn `player_actions::resume_last`, which
    /// awaits the database before sending its `LoadAndPlay`. The second
    /// would restart the track the first just started.
    resume_in_flight: std::sync::atomic::AtomicBool,
    /// Session-only kill switch for exclusive output after a flap storm
    /// (#322). Once tripped, every rebuild / hot-swap stays on cpal
    /// shared regardless of the `exclusive_output` preference, so a
    /// device that resets on every exclusive grab (Realtek onboard)
    /// stops thrashing and playback survives. Reset when the user
    /// re-toggles exclusive or picks a device. Does NOT touch the
    /// persisted preference — a fresh launch tries exclusive again.
    exclusive_suppressed: std::sync::atomic::AtomicBool,
    /// Rolling flap counter feeding [`Self::try_rebuild_after_device_error`].
    exclusive_flaps: Mutex<FlapWindow>,
    /// Single-owner gate for the deferred post-`DeviceNotAvailable`
    /// rebuild (#365). `rebuild_in_progress` above only guards a rebuild
    /// that is *currently executing*; because the recovery is scheduled
    /// 300 ms after the error, consecutive errors queue separate tasks
    /// that never overlap, so each one finds the debounce slot free.
    /// This gate spans the whole arm → delay → run lifecycle plus a
    /// settle window, so one burst of device errors yields one rebuild.
    rebuild_gate: Mutex<RebuildGate>,
    /// Last non-library source captured at the boundary of [`Self::send`]
    /// (#230). The three output-rebuild paths
    /// ([`Self::set_output_device`], [`Self::set_exclusive_output`],
    /// [`Self::force_rebuild_output`]) snapshot
    /// `shared.current_track_id`; for radio and remote queues that id is a
    /// negative sentinel from
    /// [`crate::commands::player::next_radio_track_id`] with no
    /// matching `track` row, so a plain `WHERE id = ?` resume
    /// returns nothing and the rebuild silently drops the user
    /// off the stream. Holding the originating URL or reconciled-file payload
    /// lets those paths re-dispatch the same source instead. Cleared on the next
    /// [`AudioCmd::LoadAndPlay`] so a local-track switch doesn't
    /// resurrect the dead radio session on a later rebuild.
    radio_resume: Mutex<Option<RadioResumeState>>,
    /// Monotonic source of [`LoadIntent`]s (#622). Handed out by
    /// [`Self::next_load_intent`] at the start of every playback intent, so
    /// the decoder can tell a late preparation from a newer selection.
    load_intents: std::sync::atomic::AtomicU64,
}

/// Snapshot of an active non-library source, retained by the engine so output
/// rebuilds can resume either the server URL or a reconciled local file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RadioResumeState {
    pub source: RadioResumeSource,
    pub track_id: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artwork_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RadioResumeSource {
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
        path: PathBuf,
        duration_ms: u64,
        fallback_url: Option<String>,
        replay_gain: TrackGain,
    },
}

impl RadioResumeState {
    /// Rebuild the command that resumes this session, stamped with the
    /// `intent` of the rebuild asking for it (#622).
    fn into_command(self, position_ms: u64, intent: LoadIntent) -> AudioCmd {
        match self.source {
            RadioResumeSource::Url {
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
            RadioResumeSource::RemoteFile {
                path,
                duration_ms,
                fallback_url,
                replay_gain,
            } => AudioCmd::LoadRemoteFileAndPlay {
                intent,
                // Radio resume never restores a cache entry.
                discard_on_failure: false,
                path,
                start_ms: position_ms,
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

impl AudioEngine {
    /// Construct the engine, spawn the cpal output thread, then spawn
    /// the decoder thread with the producer side of the ring. Failures
    /// to open the device are logged but non-fatal — the engine still
    /// spins up and commands will error until the stream comes back.
    ///
    /// Takes an `AppHandle` so the decoder thread can emit Tauri events
    /// (`player:state`, `player:position`, `player:track-ended`,
    /// `player:error`) without routing through tokio.
    pub fn new(app: AppHandle) -> Arc<Self> {
        Self::new_with_device(app, None, false)
    }

    /// Like [`Self::new`] but opens a specific output device. Used at
    /// startup once the persisted `audio.output_device` profile setting
    /// is known. `None` means "use the OS default".
    ///
    /// `exclusive_output` is the persisted opt-in for owning the
    /// device (silently no-op where no exclusive PCM backend exists).
    /// On a failing init the engine falls back to cpal shared mode
    /// automatically; see [`spawn_output_with_mode`] for the contract.
    pub fn new_with_device(
        app: AppHandle,
        device_name: Option<String>,
        exclusive_output: bool,
    ) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = unbounded::<AudioCmd>();
        let shared = Arc::new(SharedPlayback::new());

        // Analytics channel: decoder pushes `AnalyticsMsg`s at EOF, the
        // tokio `analytics_task` consumes them to write `play_event`
        // rows and self-send the next `LoadAndPlay`.
        let (analytics_tx, analytics_rx) = unbounded_channel::<AnalyticsMsg>();

        let (output, decoder, exclusive_output_active) = match spawn_output_with_mode(
            shared.clone(),
            app.clone(),
            device_name,
            exclusive_output,
            None,
        ) {
            Ok((producer, handle)) => {
                let active = handle.exclusive;
                // `spawn_output_thread` returns only after the cpal
                // stream has opened, so `shared.sample_rate` /
                // `shared.channels` are already populated by the time
                // the decoder thread spawns.
                match spawn_decoder_thread(
                    cmd_rx,
                    producer,
                    shared.clone(),
                    app.clone(),
                    analytics_tx,
                ) {
                    Ok(join) => (Some(handle), Some(join), active),
                    Err(err) => {
                        tracing::error!(?err, "failed to spawn decoder thread");
                        handle.stop();
                        (None, None, false)
                    }
                }
            }
            Err(err) => {
                tracing::warn!(?err, "failed to open audio output at startup");
                (None, None, false)
            }
        };

        // Spawn the analytics task inside Tauri's runtime.
        tauri::async_runtime::spawn(analytics_task(analytics_rx, cmd_tx.clone(), app.clone()));

        Arc::new(Self {
            cmd_tx,
            shared,
            output: Mutex::new(output),
            decoder: Mutex::new(decoder),
            app,
            exclusive_output: std::sync::atomic::AtomicBool::new(exclusive_output),
            exclusive_output_active: std::sync::atomic::AtomicBool::new(exclusive_output_active),
            rebuild_in_progress: std::sync::atomic::AtomicBool::new(false),
            resume_in_flight: std::sync::atomic::AtomicBool::new(false),
            exclusive_suppressed: std::sync::atomic::AtomicBool::new(false),
            exclusive_flaps: Mutex::new(FlapWindow::default()),
            rebuild_gate: Mutex::new(RebuildGate::default()),
            radio_resume: Mutex::new(None),
            load_intents: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Send a command to the decoder. Returns `AppError::Audio` if the
    /// channel is disconnected (decoder thread has exited).
    ///
    /// Side-effect: maintains the [`Self::radio_resume`] snapshot at
    /// the boundary. A `LoadUrlAndPlay` overwrites the previous radio
    /// session; a `LoadAndPlay` clears it (the user moved to a local
    /// track — resurrecting the dead radio URL on a future output
    /// rebuild would be wrong). Other variants don't touch the
    /// snapshot. Capture happens before the channel send so a failed
    /// send still leaves the snapshot consistent with what the user
    /// asked for.
    /// Claim the next playback intent (#622).
    ///
    /// Call this **at the start of the intent** — before the profile
    /// snapshot, the queue read and the ReplayGain lookup — and carry the
    /// result into the load command. Taking it just before the send would
    /// stamp the order in which preparations *finished*, which is the very
    /// order that plays the wrong track.
    ///
    /// Starts at 1, so the decoder's "nothing seen yet" high-water mark of
    /// 0 accepts the first load of the session.
    pub fn next_load_intent(&self) -> LoadIntent {
        LoadIntent(
            self.load_intents
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                + 1,
        )
    }

    pub fn send(&self, cmd: AudioCmd) -> AppResult<()> {
        apply_radio_resume_update(&self.radio_resume, &cmd);
        self.cmd_tx
            .send(cmd)
            .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))
    }

    /// Cheap clone of the last Web Radio session captured by
    /// [`Self::send`]. Used by the three output-rebuild paths to
    /// decide between re-dispatching `LoadUrlAndPlay` (radio) or
    /// the SQLite-keyed `LoadAndPlay` (local track). `None` means
    /// no radio session has run on this engine, or a local track
    /// has played since.
    fn snapshot_radio_resume(&self) -> Option<RadioResumeState> {
        self.radio_resume.lock().ok().and_then(|g| g.clone())
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

    /// Borrow the shared atomic state — used by commands that need to read
    /// current position / volume / state without hitting the decoder.
    pub fn shared(&self) -> &Arc<SharedPlayback> {
        &self.shared
    }

    /// Send `Stop` and await the decoder thread's transition back to
    /// `PlayerState::Idle`. The decoder publishes the new state
    /// AFTER it drops the active stream (and therefore the
    /// underlying `File` / `HttpMediaSource` handle), so once this
    /// returns we know the audio side is no longer holding any file
    /// open under the data dir.
    ///
    /// Polls `shared.state` every 10 ms via `tokio::time::sleep` so
    /// the wait yields to the runtime instead of pinning a worker
    /// thread. Falls back to the timeout if the decoder is stuck or
    /// already dead (channel closed) — the caller can choose to
    /// surface or swallow the error depending on whether it's a
    /// hard requirement or best-effort.
    pub async fn stop_and_wait(&self, timeout: Duration) -> AppResult<()> {
        use std::sync::atomic::Ordering;
        use std::time::Instant;

        use crate::audio::state::PlayerState;

        // Channel-closed (decoder dead) → nothing left holding files,
        // treat as already-stopped. Any other send error propagates.
        match self.send(AudioCmd::Stop) {
            Ok(()) => {}
            Err(_) => return Ok(()),
        }

        let deadline = Instant::now() + timeout;
        let idle_marker = PlayerState::Idle as u8;
        while self.shared.state.load(Ordering::Acquire) != idle_marker {
            if Instant::now() >= deadline {
                return Err(AppError::Audio(
                    "audio engine did not reach Idle within timeout".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    /// Name of the cpal device feeding the current output thread, or
    /// `None` if it's tracking the OS default. Returned to the
    /// frontend so the device picker can highlight the active row.
    pub fn current_output_device(&self) -> Option<String> {
        self.output
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|h| h.device_name.clone()))
    }

    /// The device the output is really driving, when the backend could
    /// name it — as opposed to [`Self::current_output_device`], which is
    /// the user's pin and survives a fallback.
    ///
    /// The picker flags its active row from this one: a pinned device
    /// that is no longer enumerated is replaced by the default endpoint
    /// on open, and ticking the pin then described a device playing
    /// nothing (#612).
    /// Returned as `(opened, pinned)` under a **single** lock.
    ///
    /// Reading the two names through separate accessors let a rebuild
    /// swap the handle in between, pairing the old stream's opened name
    /// with the new stream's pin — the picker would then tick one row
    /// and mark another as pinned, each describing a different stream.
    pub fn current_output_devices(&self) -> (Option<String>, Option<String>) {
        self.output
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|h| (h.opened_device.clone(), h.device_name.clone()))
            })
            .unwrap_or((None, None))
    }

    /// Re-open the output on the device it is already pinned to.
    ///
    /// [`Self::set_output_device`] short-circuits when the request
    /// equals the current pin. That is right for a pick, but it left the
    /// user no way back when the stream was bound to an endpoint that is
    /// no longer the one they want — and the picker disabled its active
    /// row, so "choose another device, then choose this one again" was
    /// the only cure (#612). This goes straight to the rebuild path that
    /// device-error recovery already uses.
    /// Carries the same two protections every other deliberate rebuild
    /// has, because `force_rebuild_output` owns neither: its recovery
    /// caller computes them.
    pub fn reopen_output_device(&self) -> AppResult<()> {
        // Honour the #322 session kill switch. A flap storm that gave up
        // on exclusive for this session must not be undone by a click
        // that only asks for the same device again — this is not the
        // fresh chance a re-toggle or a device pick is, so the
        // suppression also isn't reset here.
        let exclusive = self
            .exclusive_output
            .load(std::sync::atomic::Ordering::Relaxed)
            && !self
                .exclusive_suppressed
                .load(std::sync::atomic::Ordering::Relaxed);
        // Replacing the stream disturbs the outgoing one on purpose, and
        // that self-inflicted device error would otherwise schedule a
        // recovery rebuild fighting this one — the #405 echo. Reopen the
        // window when nothing got installed, so a genuine error still
        // reaches the recovery path.
        self.begin_deliberate_output_change();
        // The device is resolved by the rebuild, under the same lock it
        // swaps the handle with. Reading it here first would leave a
        // window where a device pick installs and persists another one,
        // after which this would reinstall the stale device and leave the
        // stream disagreeing with the saved preference.
        let result = self.force_rebuild_output(RebuildDevice::Pinned, exclusive);
        if result.is_err() {
            self.cancel_deliberate_output_change();
        }
        result
    }

    /// True when the active output is currently carrying a native DSD
    /// stream via DoP (#495). Reflects what actually engaged — a DAC
    /// that refused the DoP format leaves this `false` even with the
    /// opt-in on. Drives the "DSD natif" pill in the pipeline popover.
    pub fn current_output_is_dop(&self) -> bool {
        self.output
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|h| h.dop.is_some()))
            .unwrap_or(false)
    }

    /// Hot-swap the cpal output device without restarting the decoder
    /// or the analytics task.
    ///
    /// Strategy (reordered so a failing device doesn't leave us with
    /// no audio at all):
    /// 1. snapshot the currently loaded track + its position;
    /// 2. open the new output thread first — if cpal can't open the
    ///    device (broken HDMI sink, exclusive-mode conflict, …), bail
    ///    out before touching the old one so the user keeps hearing
    ///    audio through whatever was already working;
    /// 3. send `Stop` so the decoder unwinds out of `play_track` and
    ///    parks at the top-level command loop;
    /// 4. tear the old output thread down (releases the cpal device);
    /// 5. send `SwapProducer` so the decoder picks up the new ring;
    /// 6. send `LoadAndPlay` with the saved position so playback
    ///    resumes at the same spot through the new device.
    ///
    /// Recover from a mid-stream `cpal::StreamError::DeviceNotAvailable`
    /// (#175). The cpal error callback fires on a random thread when
    /// Windows resets its audio session, a USB DAC unplugs, or a
    /// Bluetooth source flaps — without an automatic rebuild the user
    /// is stuck on a paused stream until they touch the device menu.
    ///
    /// Rebuilds with the SAME pinned device + WASAPI Exclusive
    /// preference the engine was using before the error. Re-querying
    /// the OS default here would be wrong: the default is what gets
    /// SWAPPED when Windows decides to reset the session, so the user
    /// would silently land on a different output (different sample
    /// rate, different channel count) every time the original device
    /// flapped.
    ///
    /// Debounced via `rebuild_in_progress`: a quick double-flap (the
    /// pattern seen in the original bug report — three open/close
    /// cycles in 14 seconds) only triggers one rebuild attempt
    /// instead of stacking three concurrent SwapProducer cmds onto
    /// the decoder.
    pub(super) fn try_rebuild_after_device_error(
        &self,
        target: super::output::RebuildTarget,
    ) -> AppResult<()> {
        use std::sync::atomic::Ordering;

        // Release the #365 gate on EVERY exit path below (including the
        // early `return`s and any panic), otherwise `armed` stays latched
        // and no later device error could ever schedule a recovery again.
        // Declared first so it drops last, after `force_rebuild_output`
        // has released the `output` lock — the settle window should start
        // when the rebuild is really over.
        struct GateGuard<'a>(&'a Mutex<RebuildGate>);
        impl Drop for GateGuard<'_> {
            fn drop(&mut self) {
                self.0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .finish(Instant::now());
            }
        }
        let _gate = GateGuard(&self.rebuild_gate);

        // Acquire the debounce slot. `swap(true)` returns the
        // previous value, so a `true` here means somebody else is
        // already rebuilding — bail without disturbing them.
        if self.rebuild_in_progress.swap(true, Ordering::AcqRel) {
            tracing::debug!("device-error rebuild already in flight; skipping concurrent trigger");
            return Ok(());
        }

        // RAII guard so the debounce slot clears even if rebuild
        // panics or returns Err mid-flight — otherwise a single
        // failure would lock out every subsequent retry.
        struct ResetGuard<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for ResetGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = ResetGuard(&self.rebuild_in_progress);

        // `Resolve` reads the device off the live (dead) handle still
        // parked in `self.output` — right for the cpal / exclusive
        // device-loss callers. `Device` is passed when the caller already
        // released the handle, so a self-resolve would see `None` and
        // reopen the OS default instead of the user's pick (#405).
        let pinned = match target {
            super::output::RebuildTarget::Resolve => self.current_output_device(),
            super::output::RebuildTarget::Device(device) => device,
        };
        let pref_exclusive = self.exclusive_output.load(Ordering::Relaxed);

        // #322: an exclusive-mode flap storm. A device that resets on every
        // exclusive grab fires DeviceNotAvailable ~300 ms after each
        // re-engage, so re-opening exclusive here just re-arms the loop.
        // Count flaps and, past the threshold, give up on exclusive for the
        // rest of the session so the device settles on shared mode.
        let mut exclusive = pref_exclusive && !self.exclusive_suppressed.load(Ordering::Relaxed);
        if exclusive {
            let tripped = self
                .exclusive_flaps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(Instant::now());
            if tripped {
                self.exclusive_suppressed.store(true, Ordering::Relaxed);
                exclusive = false;
                tracing::warn!(
                    "Exclusive output disabled for this session after repeated device \
                     flaps; staying on shared mode. Re-enable it in Settings to retry."
                );
            }
        }

        tracing::info!(
            device = pinned.as_deref().unwrap_or("<os-default>"),
            exclusive,
            "rebuilding cpal output after DeviceNotAvailable"
        );

        // Force-rebuild path: bypasses set_output_device's no-op
        // shortcut for "same device" because the device is the
        // same — we just need a fresh stream after the OS reset.
        self.force_rebuild_output(RebuildDevice::Explicit(pinned), exclusive)
    }

    /// Ask permission to schedule a deferred rebuild after a cpal
    /// `DeviceNotAvailable` (#365). Returns `true` for the caller that
    /// should own the recovery, `false` for everyone else.
    ///
    /// Called from the deferred recovery task (after its 300 ms backoff),
    /// not from the cpal error callback itself — the engine may not be
    /// registered in Tauri state yet at error time. A burst of errors
    /// therefore queues several tasks, but only the first to wake arms;
    /// the rest bail here, so the burst still collapses to one rebuild.
    ///
    /// A `false` means either a rebuild is already armed / running, or
    /// we're inside [`REBUILD_SETTLE_WINDOW`] after the last one — on a
    /// DAC that resets on every exclusive grab, that second case is our
    /// own re-open echoing back, and reacting to it is exactly what
    /// re-arms the cascade.
    ///
    /// The matching release lives in `try_rebuild_after_device_error`'s
    /// RAII guard, so a scheduled-but-failed rebuild still reopens the
    /// gate.
    pub fn try_arm_device_rebuild(&self) -> bool {
        self.rebuild_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_arm(Instant::now(), REBUILD_SETTLE_WINDOW)
    }

    /// Publish "no output thread at all" when a rebuild bailed out after
    /// the old handle was already released.
    ///
    /// The exclusive flag must not keep claiming a stream that no longer
    /// exists — a toggle describing one is the exact shape of #405 — and
    /// the event is what makes Settings re-read. No-ops when `guard`
    /// still holds a handle, i.e. the failure happened before the old
    /// stream was given up and the flag is still accurate.
    fn publish_output_lost_if_gone(&self, guard: &Option<OutputHandle>) {
        if guard.is_none() {
            self.exclusive_output_active
                .store(false, std::sync::atomic::Ordering::Release);
            let _ = self.app.emit("player:audio-mode-changed", ());
        }
    }

    /// Open the #365 settle window around an output change WE are making
    /// on purpose (a mode toggle, a device switch).
    ///
    /// Replacing a stream makes the outgoing one fire
    /// `DeviceNotAvailable` — seizing the endpoint in exclusive mode
    /// kicks the shared client off it, and tearing a stream down can do
    /// the same. That error is a consequence of the change, not a device
    /// failure, but the cpal callback can't tell the difference and
    /// schedules a recovery rebuild that fights the mode the user just
    /// picked (visible in the #405 report as a second exclusive open
    /// ~300 ms after the first). Marking the gate finished starts the
    /// same quiet period a real rebuild would, so those echoes are
    /// ignored.
    fn begin_deliberate_output_change(&self) {
        self.rebuild_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish(Instant::now());
    }

    /// Undo a [`begin_deliberate_output_change`] when the change ended up
    /// installing no stream (the spawn failed after the old handle was
    /// already released). The settle window was there to absorb the echo
    /// error from a successful swap; with no swap there is no echo, and
    /// leaving the window open would suppress the recovery rebuild we're
    /// about to schedule — the exact silent-no-output tail of #405.
    fn cancel_deliberate_output_change(&self) {
        self.rebuild_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reopen();
    }

    /// Clear the #322 session-level exclusive suppression and its flap
    /// counter. Called when the user explicitly re-toggles WASAPI exclusive
    /// or switches output device — both are a fresh chance for exclusive to
    /// work, so a prior flap storm shouldn't keep it pinned to shared.
    fn reset_exclusive_suppression(&self) {
        self.exclusive_suppressed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut flaps) = self.exclusive_flaps.lock() {
            *flaps = FlapWindow::default();
        }
    }

    /// Reconcile the output format with the track the decoder is about to
    /// play, for native DSD via DoP (#495). Unlike [`Self::force_rebuild_output`]
    /// this hands the fresh ring producer **directly back to the caller**
    /// (the decoder thread, mid-load) instead of pushing it through the
    /// `SwapProducer` channel — the decoder is about to write the new
    /// track's samples to it and there's no old stream to keep feeding.
    ///
    /// - `dop = Some(fmt)`: open the exclusive output at that exact DoP
    ///   rate / channels. If the DAC refuses, transparently fall back to a
    ///   normal PCM output so the caller can play the DSD → PCM path.
    /// - `dop = None`: restore the normal (preference-driven) PCM output
    ///   if the previous track left the device in DoP mode.
    ///
    /// Returns `(producer, dop_engaged)`. `producer` is `Some` only when a
    /// rebuild actually happened — an unchanged format returns `None` so
    /// an ordinary PCM-to-PCM track costs nothing. `dop_engaged` tells the
    /// caller whether to open the stream as DoP or DSD → PCM.
    pub(crate) fn switch_output_for_track(
        &self,
        dop: Option<DopFormat>,
    ) -> AppResult<(Option<rtrb::Producer<f32>>, bool)> {
        use std::sync::atomic::Ordering;

        // DoP needs exclusive device access. On Windows that's the
        // separate WASAPI Exclusive opt-in, so DoP rides it — if the user
        // hasn't enabled exclusive, drop the DoP request up-front rather
        // than tear the output down and rebuild it just to fall back to
        // PCM on every DSD track. On Linux / macOS the DoP toggle itself
        // engages the exclusive path (raw `hw:` / hog mode), so nothing
        // else gates it. On any other platform DoP can't run at all.
        #[cfg(target_os = "windows")]
        let exclusive_available = self.exclusive_output.load(Ordering::Relaxed);
        // Linux (raw `hw:`) and macOS (CoreAudio hog mode) engage the
        // exclusive path from the DoP toggle itself — no separate opt-in.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let exclusive_available = true;
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let exclusive_available = false;
        let dop = if dop.is_some() && exclusive_available {
            dop
        } else {
            None
        };

        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        let has_output = guard.is_some();
        // Compared as a whole `DopFormat`, not just the rate: an
        // exclusive DoP stream is opened for a fixed interleave, so two
        // consecutive DSD tracks at the same DoP rate but different
        // channel counts still need a re-open (reusing the stereo stream
        // for a multichannel one would tear frames across channels).
        let current_dop = guard.as_ref().and_then(|h| h.dop);
        let want_dop = dop;

        // Already in the right shape: nothing to do. An ordinary PCM track
        // following another PCM track lands here and pays nothing.
        if has_output && current_dop == want_dop {
            return Ok((None, dop.is_some()));
        }

        // Capture the pinned device + exclusive preference before dropping
        // the old handle. A DoP (exclusive) open can't proceed while the
        // previous exclusive client still holds the device, so release it
        // first (#322 reasoning) — this path always replaces the stream.
        let device = guard.as_ref().and_then(|h| h.device_name.clone());
        let pref_exclusive = self.exclusive_output.load(Ordering::Relaxed);
        if let Some(old) = guard.take() {
            old.stop();
        }

        // Try the requested DoP format first.
        if let Some(dop_fmt) = dop {
            match spawn_output_with_mode(
                self.shared.clone(),
                self.app.clone(),
                device.clone(),
                pref_exclusive,
                Some(dop_fmt),
            ) {
                Ok((producer, handle)) => {
                    self.exclusive_output_active
                        .store(handle.exclusive, Ordering::Release);
                    *guard = Some(handle);
                    let _ = self.app.emit("player:audio-mode-changed", ());
                    tracing::info!(
                        rate = dop_fmt.sample_rate,
                        channels = dop_fmt.channels,
                        "DoP output engaged"
                    );
                    return Ok((Some(producer), true));
                }
                Err(err) => {
                    tracing::warn!(
                        %err,
                        rate = dop_fmt.sample_rate,
                        "DoP output refused by device; falling back to DSD -> PCM"
                    );
                    // Fall through to a normal PCM open.
                }
            }
        }

        // Normal PCM output: DoP wasn't requested, or was refused.
        match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device,
            pref_exclusive,
            None,
        ) {
            Ok((producer, handle)) => {
                self.exclusive_output_active
                    .store(handle.exclusive, Ordering::Release);
                *guard = Some(handle);
                let _ = self.app.emit("player:audio-mode-changed", ());
                Ok((Some(producer), false))
            }
            Err(err) => {
                // No output at all now — surface the loss like the other
                // rebuild paths so the UI doesn't think playback is live.
                self.publish_output_lost_if_gone(&guard);
                Err(err)
            }
        }
    }

    /// Internal helper: rebuild the output stream against the given
    /// ([`RebuildDevice`], exclusive) pair, bypassing the same-device
    /// no-op check. Shared by [`Self::try_rebuild_after_device_error`]
    /// and [`Self::reopen_output_device`].
    fn force_rebuild_output(&self, device: RebuildDevice, exclusive: bool) -> AppResult<()> {
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        // Resolve the target under the very lock this is about to swap
        // the handle with, so nothing can move it in between (#612).
        let device_name = match device {
            RebuildDevice::Pinned => guard.as_ref().and_then(|h| h.device_name.clone()),
            RebuildDevice::Explicit(name) => name,
        };

        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        let resume = rebuild_resume(
            self.shared.state(),
            self.shared
                .paused_output
                .load(std::sync::atomic::Ordering::Acquire),
            track_id > 0,
        );
        // Both a session that was playing and one the user paused hold a
        // track the rebuild has to interrupt; only what follows differs.
        let was_playing = resume != RebuildResume::Nothing;
        let position_ms = self.shared.current_position_ms();

        // #322: a WASAPI *exclusive* client locks the device entirely — no
        // other client, exclusive OR shared, can open it until that client
        // is released. So when the OLD stream is exclusive, spawning the new
        // one first fails: a new exclusive open hits AUDCLNT_E_DEVICE_IN_USE
        // (0x8889000A), and even the shared fallback hits "device no longer
        // available" (both lines seen in the #322 report). Release the old
        // exclusive stream FIRST, whatever mode we're rebuilding into. This
        // path only runs after a DeviceNotAvailable error, so the old stream
        // is already dead — there's no working state to roll back to. When
        // the old stream is shared we keep the spawn-first order so a failed
        // spawn can still roll back — except where entering exclusive can't
        // evict it, see [`must_release_before_reopening`].
        let pre_release =
            must_release_before_reopening(guard.as_ref().map(|h| h.exclusive), exclusive);
        if pre_release {
            if was_playing {
                self.cmd_tx
                    .send(AudioCmd::Stop)
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            if let Some(old) = guard.take() {
                old.stop();
            }
        }

        let (producer, handle) = match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device_name,
            exclusive,
            None,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // `pre_release` (an old exclusive stream) already took the
                // handle above, so this is the no-output-at-all case.
                self.publish_output_lost_if_gone(&guard);
                return Err(err);
            }
        };

        // The freshly-spawned `handle` owns a live cpal output
        // thread. If either send below fails (decoder dead, channel
        // closed mid-recovery), `handle` would otherwise be dropped
        // without `stop()` being called — the cpal Stream lives on
        // a `!Send` thread that can't be reaped from Drop, so we'd
        // leak the thread until process exit. Same pattern as
        // `set_output_device`'s error rollback.
        let send_result = (|| {
            if was_playing && !pre_release {
                self.cmd_tx
                    .send(AudioCmd::Stop)
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            // No-op when `pre_release` already took the old handle above.
            if let Some(old) = guard.take() {
                old.stop();
            }
            self.cmd_tx
                .send(AudioCmd::SwapProducer(producer))
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            Ok::<(), AppError>(())
        })();
        if let Err(err) = send_result {
            handle.stop();
            self.publish_output_lost_if_gone(&guard);
            return Err(err);
        }
        *guard = Some(handle);
        self.exclusive_output_active.store(
            guard.as_ref().map(|h| h.exclusive).unwrap_or(false),
            std::sync::atomic::Ordering::Release,
        );
        // Settings' exclusive-mode toggle only re-reads its state on
        // mount and after a manual click (issue #405) — this is the
        // one signal that tells it a rebuild just happened behind its
        // back, whether that landed in exclusive or fell back to shared.
        let _ = self.app.emit("player:audio-mode-changed", ());

        // Resume best-effort. Same async pattern as
        // `set_output_device` and `set_exclusive_output` — pull the
        // track row off the synchronous path so a slow DB doesn't
        // hold the audio recovery up. Radio sessions resume by
        // re-dispatching the cached `LoadUrlAndPlay` instead of
        // looking up a (non-existent) `track` row.
        if resume == RebuildResume::StayPaused {
            // The user had paused (#611), so the track is not picked back
            // up. The Stop above did unload it, though, and the decoder
            // ignores a Resume with nothing loaded: leaving `Paused` on
            // screen would make play a dead button. Land where a launch
            // leaves the player instead — idle, with the resume point
            // saved — so play goes through `resume_last`.
            super::decoder::transition_state(
                &self.shared,
                &self.app,
                super::state::PlayerState::Idle,
                Some(track_id),
            );
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
            if let Some(profile_id) = profile_id {
                let app = self.app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<crate::state::AppState>();
                    let saved = match state.require_profile_pool_for(Some(profile_id)).await {
                        Ok(pool) => {
                            crate::queue::persist_resume_point(&pool, track_id, position_ms).await
                        }
                        Err(err) => Err(err),
                    };
                    if let Err(err) = saved {
                        tracing::warn!(%err, "device-error rebuild: paused resume point not saved");
                    }
                });
            }
        }
        if resume == RebuildResume::Play {
            // One intent for this resume, taken before either branch
            // prepares anything (#622). The local-track branch reads the
            // row back out of SQLite, and that read must not land on top of
            // a selection the user made while it was in flight.
            let intent = self.next_load_intent();
            if track_id < 0 {
                if let Some(state) = self.snapshot_radio_resume() {
                    let _ = self.cmd_tx.send(state.into_command(position_ms, intent));
                }
            } else if track_id > 0 {
                let app = self.app.clone();
                let cmd_tx = self.cmd_tx.clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager as _;
                    let state = app.state::<crate::state::AppState>();
                    let pool = match state.require_profile_pool().await {
                        Ok(p) => p,
                        Err(err) => {
                            tracing::warn!(%err, "device-error rebuild: no profile pool, skipping resume");
                            return;
                        }
                    };
                    let row: Option<(String, i64)> =
                        sqlx::query_as("SELECT file_path, duration_ms FROM track WHERE id = ?")
                            .bind(track_id)
                            .fetch_optional(&*pool)
                            .await
                            .ok()
                            .flatten();
                    if let Some((file_path, duration_ms)) = row {
                        // Fetch ReplayGain at resume time so a user who
                        // enabled the toggle keeps their analysed gain
                        // across an unintended device flap — matches
                        // set_output_device and set_exclusive_output.
                        let replay_gain =
                            crate::commands::player::fetch_replay_gain(&pool, track_id).await;
                        let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                            intent,
                            path: std::path::PathBuf::from(file_path),
                            start_ms: position_ms,
                            track_id,
                            // `duration_ms` is stored as `i64` in SQLite
                            // (no `u64` column type). Saturate to 0 before
                            // casting so a corrupted negative row can't
                            // wrap into a huge `u64` and confuse the
                            // decoder's end-of-track guard.
                            duration_ms: duration_ms.max(0) as u64,
                            source_type: "device-rebuild".into(),
                            source_id: None,
                            replay_gain,
                        });
                    }
                });
            }
        }

        Ok(())
    }

    /// `device_name = None` means "follow the OS default". Picking the
    /// already-active device is a no-op so spamming the menu doesn't
    /// glitch playback.
    ///
    /// **Threading note:** opening / tearing down a cpal stream on
    /// Linux ALSA can probe device hardware and block for hundreds of
    /// ms. Callers reaching into this from a tokio task should wrap
    /// the call in `tokio::task::spawn_blocking`.
    pub fn set_output_device(&self, device_name: Option<String>) -> AppResult<()> {
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        // Same device? Nothing to do. Compare both sides as `Option<&str>`
        // so an empty-string DB read can't masquerade as a real change.
        let current = guard.as_ref().and_then(|h| h.device_name.as_deref());
        let requested = device_name.as_deref();
        if current == requested {
            return Ok(());
        }

        // A different device may support exclusive fine — clear any #322
        // flap-storm suppression tied to the previous device.
        self.reset_exclusive_suppression();

        // Snapshot what's playing so we can resume on the new device.
        let was_playing = matches!(
            self.shared.state(),
            super::state::PlayerState::Playing | super::state::PlayerState::Paused
        );
        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        let position_ms = self.shared.current_position_ms();

        // Step 2 — release the old stream first when the new one cannot be
        // opened alongside it, then open the replacement.
        //
        // Spawn-first is the order we want for a device *switch*: the two
        // streams target different endpoints, so a failed open costs
        // nothing and the stream the user is listening to survives. PipeWire,
        // PulseAudio and ALSA dmix all take concurrent streams, and Windows
        // has no quarrel with two clients on two endpoints.
        //
        // A vanished device breaks that premise, and #604 is what it looks
        // like. When the name we were asked for is no longer enumerated,
        // `pick_device` falls back to the **default** endpoint — which can
        // be the one the old stream is holding exclusively. The open is then
        // refused with `AUDCLNT_E_DEVICE_IN_USE` against ourselves, we drop
        // to shared mode, and nothing tells the user why.
        //
        // Measured on Windows 11 rather than assumed: an exclusive client
        // does block a second exclusive open of the same endpoint, a
        // *shared* client does not block one at all, and the block clears
        // about 19 ms after the client is dropped. So the conflict is real,
        // it is ours, and releasing first is enough to clear it.
        let entering_exclusive = self
            .exclusive_output
            .load(std::sync::atomic::Ordering::Relaxed);
        let pre_release =
            must_release_before_reopening(guard.as_ref().map(|h| h.exclusive), entering_exclusive);
        // Stop is sent here rather than twice: the step below skips its own
        // send when this one already happened.
        let stopped_early = pre_release && was_playing;
        // The device to put back if the replacement will not open at all.
        // Only a pre-release needs one: spawn-first leaves the old stream
        // installed when the new one fails.
        let mut previous_device = None;
        if pre_release {
            if was_playing {
                self.cmd_tx
                    .send(AudioCmd::Stop)
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            if let Some(old) = guard.take() {
                previous_device = Some(old.device_name.clone());
                old.stop();
            }
        }

        // Set when the replacement failed and the previous device came back
        // instead: playback carries on there, and the caller still gets the
        // error, so the new choice is not persisted.
        let mut switch_error = None;
        let (producer, handle) = match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device_name,
            entering_exclusive,
            None,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // Releasing first gave up the rollback that spawn-first gets
                // for free, so buy it back by reopening the previous device.
                // A target that will not open at all, such as a headset still
                // listed but already gone, must not cost the user the output
                // they were listening to.
                //
                // Keyed on the release rather than on comparing endpoints: a
                // name is no identity (ALSA reaches one card as `default`,
                // `plughw:0,0` and `hw:0,0`), and a wrong "different" verdict
                // would put #604 back.
                let reopened = previous_device.clone().and_then(|previous| {
                    spawn_output_with_mode(
                        self.shared.clone(),
                        self.app.clone(),
                        previous,
                        entering_exclusive,
                        None,
                    )
                    .inspect_err(|reopen_err| {
                        tracing::warn!(
                            %reopen_err,
                            "set_output_device: the previous device did not reopen either"
                        );
                    })
                    .ok()
                });
                match reopened {
                    Some(pair) => {
                        tracing::warn!(
                            %err,
                            "set_output_device: the new device did not open, back on the previous one"
                        );
                        switch_error = Some(err);
                        pair
                    }
                    None => {
                        // No output at all now, and the toggle must stop
                        // claiming one (#405). Without a pre-release the old
                        // stream is still installed and still playing, and
                        // this no-ops.
                        self.publish_output_lost_if_gone(&guard);
                        // Same recovery as `set_exclusive_output`'s: retry the
                        // previous device once the OS has settled, instead of
                        // leaving the engine with no output until the user
                        // picks again. Passed explicitly because the release
                        // emptied `self.output`, so a self-resolve would
                        // reopen the OS default instead of that device (#405).
                        if let Some(previous) = previous_device {
                            super::output::schedule_device_rebuild(
                                &self.app,
                                super::output::RebuildTarget::Device(previous),
                            );
                        }
                        return Err(err);
                    }
                }
            }
        };

        // Step 3 — interrupt any current playback. The decoder will
        // walk back out of `play_track` and start polling for fresh
        // commands at the top level. The crossbeam channel is FIFO,
        // so the SwapProducer we send next won't be picked up before
        // Stop is processed.
        //
        // If either Stop or SwapProducer fails to send, the decoder
        // has died (engine teardown / crash). Tear the freshly opened
        // output back down so it doesn't outlive the engine.
        let send_result = (|| {
            if was_playing && !stopped_early {
                self.cmd_tx
                    .send(AudioCmd::Stop)
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            // Step 4 — drop the old output thread (releases the
            // device). Done before SwapProducer so the decoder
            // doesn't briefly hold two ring producers; doing this
            // here also keeps the failure path tidy.
            if let Some(old) = guard.take() {
                old.stop();
            }
            // Step 5 — hand the fresh producer over to the decoder.
            self.cmd_tx
                .send(AudioCmd::SwapProducer(producer))
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            Ok::<(), AppError>(())
        })();
        if let Err(err) = send_result {
            handle.stop();
            // Mirror force_rebuild_output / set_exclusive_output: if the
            // SwapProducer send failed the closure had already run
            // `guard.take()`, so no output thread remains — the exclusive
            // flag must stop claiming one and Settings must re-read (#405).
            // No-ops when an earlier Stop send failed before guard.take(),
            // where the old stream is still installed and the flag accurate.
            self.publish_output_lost_if_gone(&guard);
            return Err(err);
        }

        *guard = Some(handle);
        self.exclusive_output_active.store(
            guard.as_ref().map(|h| h.exclusive).unwrap_or(false),
            std::sync::atomic::Ordering::Release,
        );
        // See force_rebuild_output's comment (issue #405) — a device
        // switch can also flip the actually-engaged exclusive mode.
        let _ = self.app.emit("player:audio-mode-changed", ());

        // Step 6 — resume the previous track if we were playing one.
        // Radio (negative sentinel id) re-dispatches the cached
        // `LoadUrlAndPlay`; local tracks (positive id) hit the
        // SQLite-keyed async resume.
        if was_playing {
            // One intent for this resume, taken before either branch
            // prepares anything (#622). The local-track branch reads the
            // row back out of SQLite, and that read must not land on top of
            // a selection the user made while it was in flight.
            let intent = self.next_load_intent();
            if track_id < 0 {
                if let Some(state) = self.snapshot_radio_resume() {
                    let _ = self.cmd_tx.send(state.into_command(position_ms, intent));
                }
            } else if track_id > 0 {
                // Best-effort: pull file path + RG from the active profile
                // so the decoder gets everything it needs.
                let app = self.app.clone();
                let cmd_tx = self.cmd_tx.clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager as _;
                    let state = app.state::<crate::state::AppState>();
                    let pool = match state.require_profile_pool().await {
                        Ok(p) => p,
                        Err(err) => {
                            tracing::warn!(%err, "set_output_device: no profile pool, skipping resume");
                            return;
                        }
                    };
                    let row: Option<(String, i64)> =
                        sqlx::query_as("SELECT file_path, duration_ms FROM track WHERE id = ?")
                            .bind(track_id)
                            .fetch_optional(&*pool)
                            .await
                            .ok()
                            .flatten();
                    let Some((file_path, duration_ms)) = row else {
                        return;
                    };
                    let replay_gain =
                        crate::commands::player::fetch_replay_gain(&pool, track_id).await;
                    let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                        intent,
                        path: std::path::PathBuf::from(file_path),
                        start_ms: position_ms,
                        track_id,
                        duration_ms: duration_ms.max(0) as u64,
                        source_type: "manual".into(),
                        source_id: None,
                        replay_gain,
                    });
                });
            }
        }

        // Back on the previous device after a failed switch: playback is
        // restored, but the switch itself did not happen.
        match switch_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Flip the WASAPI Exclusive Mode preference and re-open the
    /// output stream using the new mode. No-ops on non-Windows.
    /// Re-uses the active device name so the user keeps their pick.
    pub fn set_exclusive_output(&self, enabled: bool) -> AppResult<()> {
        let previous = self
            .exclusive_output
            .swap(enabled, std::sync::atomic::Ordering::Relaxed);
        if previous == enabled {
            return Ok(());
        }
        // Reuse `set_output_device` with the active device name — the
        // current/requested equality check inside it would short-circuit
        // a same-device call, so go straight to the rebuild path by
        // temporarily yielding `None` would change the device picker
        // semantics. Instead, the engine's existing teardown path is
        // what we need: snapshot the device, drop the handle, rebuild.
        let active = self.current_output_device();
        // `set_output_device` early-exits when current == requested.
        // Bypass that by toggling to `None` then back if needed —
        // simpler: drop the handle and rebuild via the helper.
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;
        let was_playing = matches!(
            self.shared.state(),
            super::state::PlayerState::Playing | super::state::PlayerState::Paused
        );
        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        let position_ms = self.shared.current_position_ms();

        // #405 — the stuck Settings toggle. Leaving exclusive mode re-opens
        // the SAME endpoint in shared mode, and a WASAPI exclusive client
        // owns its endpoint outright: no other client, shared OR exclusive,
        // can open it until that client is released (the #322 lesson, which
        // `force_rebuild_output` already applies). Spawning first — the
        // order that's correct for a device *switch*, where the two streams
        // target different endpoints — therefore fails every time here. The
        // command returned `Err`, so the preference was never persisted and
        // `exclusive_output_active` kept reporting the old mode: the toggle
        // sat latched on the very mode the user was trying to leave, with a
        // restart as the only way out. Release the old exclusive stream
        // FIRST so the new open finds a free device — and on macOS the
        // other direction needs it too, see
        // [`must_release_before_reopening`].
        let pre_release =
            must_release_before_reopening(guard.as_ref().map(|h| h.exclusive), enabled);
        if pre_release {
            if was_playing {
                if let Err(e) = self.cmd_tx.send(AudioCmd::Stop) {
                    // Nothing has been torn down yet — the old exclusive
                    // stream is still installed and running. Roll the
                    // preference swap back so the in-memory pref keeps
                    // matching the live stream; without this a later
                    // device-error rebuild would read the new pref and
                    // silently flip to the mode this toggle failed to
                    // apply. (The spawn / send_result failure paths below
                    // deliberately keep the new pref instead, because by
                    // then the old stream is already gone.)
                    self.exclusive_output
                        .store(previous, std::sync::atomic::Ordering::Relaxed);
                    return Err(AppError::Audio(format!(
                        "audio command channel closed: {e}"
                    )));
                }
            }
            if let Some(old) = guard.take() {
                old.stop();
            }
        }

        // An explicit user toggle clears any #322 flap-storm suppression so
        // re-enabling exclusive actually retries it (and disabling resets
        // the counter for next time). Deferred until after the pre-release
        // teardown so a failed Stop send above — which leaves the old stream
        // intact and rolls the preference back — doesn't also wipe the
        // suppression state that still describes that surviving stream.
        self.reset_exclusive_suppression();

        // Both directions of this toggle disturb the outgoing stream on
        // purpose — don't let the resulting device error trigger a
        // recovery rebuild that undoes the switch.
        self.begin_deliberate_output_change();

        let (producer, handle) = match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            active.clone(),
            enabled,
            None,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // No replacement stream was installed, so the settle
                // window we opened has no swap-echo to absorb. Reopen the
                // gate either way, so it can't suppress a genuine device
                // error — on the surviving old stream (shared → exclusive
                // where even the shared fallback failed, `guard` still
                // holds it) or on the recovery we schedule below.
                self.cancel_deliberate_output_change();
                if guard.is_none() {
                    // `pre_release` already released the old exclusive
                    // handle, so there is no output thread at all. Tell
                    // Settings the flag is stale (#405), then schedule a
                    // rebuild that re-opens in the mode now recorded in
                    // `exclusive_output`. Pass `active` explicitly: the
                    // teardown emptied `self.output`, so a self-resolve
                    // would reopen the OS default instead of the user's
                    // device.
                    self.publish_output_lost_if_gone(&guard);
                    super::output::schedule_device_rebuild(
                        &self.app,
                        super::output::RebuildTarget::Device(active),
                    );
                } else {
                    // shared → exclusive where even the shared fallback
                    // failed: the old shared stream is untouched and still
                    // running. Roll the preference back to match it — same
                    // as the failed-Stop path — otherwise a later
                    // device-error rebuild would read the new pref and flip
                    // to the exclusive mode this toggle never applied.
                    self.exclusive_output
                        .store(previous, std::sync::atomic::Ordering::Relaxed);
                }
                return Err(err);
            }
        };
        let active_mode = handle.exclusive;

        // Group the whole hand-off so ANY failing step still runs the
        // `handle.stop()` below. `handle` owns a live output thread on a
        // `!Send` thread that can't be reaped from Drop, so an early
        // return here would leak it until process exit — and when the new
        // stream is the exclusive one, that leaked thread keeps the device
        // locked while `guard` still points at the old stream. Same shape
        // as `force_rebuild_output`.
        let send_result = (|| {
            if was_playing && !pre_release {
                self.cmd_tx
                    .send(AudioCmd::Stop)
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            // No-op when `pre_release` already took the old handle above.
            if let Some(old) = guard.take() {
                old.stop();
            }
            self.cmd_tx
                .send(AudioCmd::SwapProducer(producer))
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            Ok::<(), AppError>(())
        })();
        if let Err(err) = send_result {
            handle.stop();
            // A failing `Stop` send bails before the old handle is taken,
            // so `guard` still describes a live stream and the flag stays
            // accurate; the later steps leave no output at all. The helper
            // tells those two apart and only publishes for the second.
            self.publish_output_lost_if_gone(&guard);
            return Err(err);
        }
        *guard = Some(handle);
        self.exclusive_output_active
            .store(active_mode, std::sync::atomic::Ordering::Release);
        // Redundant with the caller's own re-read after a manual toggle
        // (ExclusiveModeCard.tsx), but kept for consistency with the
        // other two write sites (issue #405) in case this is ever
        // called from somewhere that doesn't already refresh itself.
        let _ = self.app.emit("player:audio-mode-changed", ());

        // Radio sessions re-dispatch `LoadUrlAndPlay` directly so
        // the WASAPI flip doesn't drop the user off the stream.
        // Local tracks hit the existing SQLite-keyed async resume.
        if was_playing {
            // One intent for this resume, taken before either branch
            // prepares anything (#622). The local-track branch reads the
            // row back out of SQLite, and that read must not land on top of
            // a selection the user made while it was in flight.
            let intent = self.next_load_intent();
            if track_id < 0 {
                if let Some(state) = self.snapshot_radio_resume() {
                    let _ = self.cmd_tx.send(state.into_command(position_ms, intent));
                }
            } else if track_id > 0 {
                let app = self.app.clone();
                let cmd_tx = self.cmd_tx.clone();
                // Resolve track metadata async — same pattern as
                // `set_output_device`. Off the synchronous path so a slow
                // DB doesn't block the setting toggle.
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager as _;
                    let state = app.state::<crate::state::AppState>();
                    let pool = match state.require_profile_pool().await {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    let row: Option<(String, i64)> =
                        sqlx::query_as("SELECT file_path, duration_ms FROM track WHERE id = ?")
                            .bind(track_id)
                            .fetch_optional(&*pool)
                            .await
                            .ok()
                            .flatten();
                    let Some((file_path, duration_ms)) = row else {
                        return;
                    };
                    let replay_gain =
                        crate::commands::player::fetch_replay_gain(&pool, track_id).await;
                    let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                        intent,
                        path: std::path::PathBuf::from(file_path),
                        start_ms: position_ms,
                        track_id,
                        duration_ms: duration_ms.max(0) as u64,
                        source_type: "manual".into(),
                        source_id: None,
                        replay_gain,
                    });
                });
            }
        }

        Ok(())
    }

    /// Whether the current output stream really owns its device —
    /// `false` after a fallback to cpal shared mode, and on any
    /// platform with no exclusive PCM backend.
    pub fn exclusive_output(&self) -> bool {
        self.exclusive_output_active
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Releases the slot [`AudioEngine::begin_resume`] took, when dropped.
pub struct ResumeGuard<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for ResumeGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Whether the old output has to be released *before* the new one is
/// opened, rather than the other way round. `old_is_exclusive` is `None`
/// when there is no stream installed at all.
///
/// Spawn-first is the order we want wherever it works: a failed open then
/// costs nothing, because the stream the user is listening to is still
/// installed and still playing. Two situations take that away.
///
/// - **The old stream is exclusive**, on any platform. It owns the device
///   outright and nothing — shared or exclusive — can open that device
///   until it lets go (#322, then #405 for the other direction).
/// - **We are entering exclusive on macOS.** Hog mode is recorded as a
///   *pid*, and the client it would have to evict here is our own cpal
///   stream, in this very process — so it evicts nothing. On Linux the
///   reservation protocol makes the sound server hand the card over, and
///   on Windows a shared client is simply no obstacle (below); macOS has
///   neither. The new AudioUnit then comes up on a device our old one is
///   still driving and renders nothing: no sound, and a position counter
///   frozen where it stood.
///
///   Measured on a MacBook Air, and only on the toggle. Armed before
///   launch the same code opens on an idle device and plays, which is
///   what made this look like a backend fault rather than an ordering
///   one.
///
/// A **shared** stream is deliberately not a reason to release first, and
/// that is measured rather than assumed. On Windows 11, `Initialize` in
/// exclusive mode succeeds while one of our own shared clients is open and
/// running on the same endpoint; only an *exclusive* client draws
/// `AUDCLNT_E_DEVICE_IN_USE`, and it stops doing so about 19 ms after that
/// client is dropped. Keeping spawn-first here is what preserves the
/// rollback for the ordinary case.
///
/// Releasing first is safe wherever it applies because
/// [`spawn_output_with_mode`] falls back to shared mode on its own, so the
/// caller still comes back holding a stream.
fn must_release_before_reopening(old_is_exclusive: Option<bool>, entering_exclusive: bool) -> bool {
    match old_is_exclusive {
        None => false,
        Some(true) => true,
        Some(false) => cfg!(target_os = "macos") && entering_exclusive,
    }
}

/// Which device a forced rebuild targets.
///
/// `Pinned` exists so a caller doesn't have to read the name first. That
/// read takes the output lock and gives it back, and a device pick
/// landing in the gap installs *and persists* another device — after
/// which the rebuild would reinstall the stale one, leaving the stream
/// disagreeing with the saved preference. Resolving inside the rebuild's
/// own acquisition closes the window (#612).
enum RebuildDevice {
    /// Whatever the installed handle is pinned to, read under the
    /// rebuild's lock.
    Pinned,
    /// A target the caller computed deliberately — the device-error
    /// recovery resolves its own and has to keep it.
    Explicit(Option<String>),
}

/// What a device-error rebuild does with the session it interrupts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RebuildResume {
    /// Nothing was loaded: reopen the output and stop there.
    Nothing,
    /// The session was playing: pick the track back up where it was.
    Play,
    /// The user had paused a library track: keep it that way (#611).
    StayPaused,
}

/// Decide [`RebuildResume`] from what the rebuild finds.
///
/// The state alone cannot tell the two sessions apart:
/// [`super::output::notify_device_lost`] has already turned a playing one
/// into `Paused` by the time the rebuild runs, so both arrive as `Paused`,
/// and resuming both started music the user had paused on whatever device
/// the system fell back to. `paused_output` does tell them apart. On the
/// playback path only the decoder's own `Pause` raises it, and nothing on
/// the device-loss path touches it (shutdown and `reset_app` raise it too,
/// but neither is followed by a rebuild).
///
/// Only a library track stays paused. What keeps it resumable is the
/// persisted resume point `resume_last` loads, and that point is always a
/// library track's: a radio station or a server track parked the same way
/// would come back as the last library track instead. Those keep being
/// picked back up, as they were before #611.
fn rebuild_resume(
    state: super::state::PlayerState,
    paused_output: bool,
    library_track: bool,
) -> RebuildResume {
    use super::state::PlayerState;
    match state {
        PlayerState::Playing | PlayerState::Paused if paused_output && library_track => {
            RebuildResume::StayPaused
        }
        PlayerState::Playing | PlayerState::Paused => RebuildResume::Play,
        PlayerState::Idle | PlayerState::Loading | PlayerState::Ended => RebuildResume::Nothing,
    }
}

/// Update the [`AudioEngine::radio_resume`] snapshot in place
/// according to the command about to be sent. Lifted out of the
/// `send` method as a free function so the lifecycle invariant
/// can be unit-tested without standing up a Tauri [`AppHandle`]
/// (which the engine itself owns).
fn apply_radio_resume_update(snapshot: &Mutex<Option<RadioResumeState>>, cmd: &AudioCmd) {
    match cmd {
        AudioCmd::LoadUrlAndPlay {
            // The rebuild that resumes this session mints its own (#622).
            intent: _,
            url,
            ext_hint,
            track_id,
            title,
            artist,
            artwork_url,
            replay_gain,
            // A cache target belongs to one open response and does not
            // survive into a new one. Everything else about the stream does:
            // flattening it to "radio" here is what would bring a finite
            // server track back forward-only and ICY-parsed, just because
            // the audio device changed.
            cache: _,
            duration_ms,
            seekable_file,
        } => {
            if let Ok(mut guard) = snapshot.lock() {
                *guard = Some(RadioResumeState {
                    source: RadioResumeSource::Url {
                        url: url.clone(),
                        ext_hint: ext_hint.clone(),
                        replay_gain: *replay_gain,
                        duration_ms: *duration_ms,
                        seekable_file: *seekable_file,
                    },
                    track_id: *track_id,
                    title: title.clone(),
                    artist: artist.clone(),
                    artwork_url: artwork_url.clone(),
                });
            }
        }
        AudioCmd::LoadRemoteFileAndPlay {
            discard_on_failure: false,
            path,
            duration_ms,
            fallback_url,
            replay_gain,
            track_id,
            title,
            artist,
            artwork_url,
            ..
        } => {
            if let Ok(mut guard) = snapshot.lock() {
                *guard = Some(RadioResumeState {
                    source: RadioResumeSource::RemoteFile {
                        path: path.clone(),
                        duration_ms: *duration_ms,
                        fallback_url: fallback_url.clone(),
                        replay_gain: *replay_gain,
                    },
                    track_id: *track_id,
                    title: title.clone(),
                    artist: artist.clone(),
                    artwork_url: artwork_url.clone(),
                });
            }
        }
        AudioCmd::LoadAndPlay { .. } => {
            if let Ok(mut guard) = snapshot.lock() {
                *guard = None;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod reopen_order_tests {
    use super::must_release_before_reopening;

    #[test]
    fn nothing_to_release_when_no_stream_is_installed() {
        assert!(!must_release_before_reopening(None, true));
        assert!(!must_release_before_reopening(None, false));
    }

    #[test]
    fn an_exclusive_stream_is_always_released_first() {
        // It owns the device outright: no open of any kind succeeds until
        // it lets go. True in both directions and on every platform.
        assert!(must_release_before_reopening(Some(true), true));
        assert!(must_release_before_reopening(Some(true), false));
    }

    #[test]
    fn entering_exclusive_over_a_shared_stream_depends_on_the_platform() {
        // A shared client blocks nothing on Windows (measured: an
        // exclusive `Initialize` succeeds alongside one) and Linux asks
        // the sound server for the card, so both keep the spawn-first
        // order and the rollback it buys. macOS records hog mode against
        // a pid and would be asked to evict this very process, so it
        // cannot.
        assert_eq!(
            must_release_before_reopening(Some(false), true),
            cfg!(target_os = "macos")
        );
    }

    #[test]
    fn shared_to_shared_has_nothing_to_reorder() {
        assert!(!must_release_before_reopening(Some(false), false));
    }
}

#[cfg(test)]
mod resume_guard_tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::ResumeGuard;

    #[test]
    fn the_slot_is_taken_once_and_released_on_drop() {
        let slot = AtomicBool::new(false);
        // The first Play claims it.
        assert!(!slot.swap(true, Ordering::AcqRel));
        {
            let _guard = ResumeGuard(&slot);
            // A second Play landing while the first resume is still
            // awaiting the database finds the slot taken and backs off.
            assert!(slot.swap(true, Ordering::AcqRel));
        }
        // Released on drop, including the `?` paths inside `resume_last`:
        // a leaked slot would leave Play dead for the whole session.
        assert!(!slot.load(Ordering::Acquire));
    }
}

#[cfg(test)]
mod rebuild_resume_tests {
    use super::super::state::PlayerState;
    use super::{rebuild_resume, RebuildResume};

    #[test]
    fn a_session_that_was_playing_picks_back_up() {
        // By the time the rebuild runs, `notify_device_lost` has already
        // parked a playing session as `Paused` without raising
        // `paused_output`.
        assert_eq!(
            rebuild_resume(PlayerState::Paused, false, true),
            RebuildResume::Play
        );
        assert_eq!(
            rebuild_resume(PlayerState::Playing, false, true),
            RebuildResume::Play
        );
    }

    #[test]
    fn a_session_the_user_paused_stays_paused() {
        // #611: resuming this one started the music on whatever device the
        // system fell back to.
        assert_eq!(
            rebuild_resume(PlayerState::Paused, true, true),
            RebuildResume::StayPaused
        );
    }

    #[test]
    fn a_paused_radio_or_server_track_is_still_picked_back_up() {
        // Parked idle it would come back as the last library track: the
        // resume point `resume_last` loads is never a radio station's.
        assert_eq!(
            rebuild_resume(PlayerState::Paused, true, false),
            RebuildResume::Play
        );
    }

    #[test]
    fn nothing_loaded_means_nothing_to_resume() {
        for state in [PlayerState::Idle, PlayerState::Loading, PlayerState::Ended] {
            assert_eq!(rebuild_resume(state, false, true), RebuildResume::Nothing);
        }
    }
}

#[cfg(test)]
mod flap_window_tests {
    use super::*;

    // Threshold is 3 within a 12 s window — see the consts under test.

    #[test]
    fn trips_on_the_threshold_flap_within_window() {
        let mut w = FlapWindow::default();
        let t = Instant::now();
        assert!(!w.record(t)); // 1
        assert!(!w.record(t + Duration::from_secs(2))); // 2
        assert!(w.record(t + Duration::from_secs(4))); // 3 → trip
    }

    #[test]
    fn resets_when_a_flap_lands_outside_the_window() {
        let mut w = FlapWindow::default();
        let t = Instant::now();
        assert!(!w.record(t)); // 1
        assert!(!w.record(t + Duration::from_secs(1))); // 2
                                                        // Past the window → counter restarts, so this does NOT trip.
        let after = t + EXCLUSIVE_FLAP_WINDOW + Duration::from_secs(1);
        assert!(!w.record(after)); // 1 (fresh window)
        assert!(!w.record(after + Duration::from_secs(1))); // 2
        assert!(w.record(after + Duration::from_secs(2))); // 3 → trip
    }

    #[test]
    fn never_trips_for_spaced_out_flaps() {
        let mut w = FlapWindow::default();
        let mut t = Instant::now();
        // Each flap sits just past the previous window, so the count keeps
        // restarting at 1 and never reaches the threshold.
        for _ in 0..6 {
            assert!(!w.record(t));
            t += EXCLUSIVE_FLAP_WINDOW + Duration::from_secs(1);
        }
    }
}

#[cfg(test)]
mod rebuild_gate_tests {
    use super::*;

    const SETTLE: Duration = REBUILD_SETTLE_WINDOW;

    #[test]
    fn arms_the_first_device_error() {
        let mut g = RebuildGate::default();
        assert!(g.try_arm(Instant::now(), SETTLE));
    }

    #[test]
    fn refuses_a_second_arm_while_one_is_still_armed() {
        let mut g = RebuildGate::default();
        let t = Instant::now();
        assert!(g.try_arm(t, SETTLE));
        // The deferred rebuild hasn't run yet: every further error in the
        // burst must be swallowed, however far apart they land.
        assert!(!g.try_arm(t + Duration::from_millis(10), SETTLE));
        assert!(!g.try_arm(t + Duration::from_secs(30), SETTLE));
    }

    #[test]
    fn refuses_a_rearm_inside_the_settle_window() {
        let mut g = RebuildGate::default();
        let t = Instant::now();
        assert!(g.try_arm(t, SETTLE));
        g.finish(t + Duration::from_millis(300));
        // This is the #365 cascade: our own exclusive re-open makes the
        // DAC reset, and the resulting error must NOT arm another pass.
        assert!(!g.try_arm(t + Duration::from_millis(340), SETTLE));
    }

    #[test]
    fn arms_again_once_the_settle_window_has_passed() {
        let mut g = RebuildGate::default();
        let t = Instant::now();
        assert!(g.try_arm(t, SETTLE));
        let finished = t + Duration::from_millis(300);
        g.finish(finished);
        assert!(!g.try_arm(finished + SETTLE - Duration::from_millis(1), SETTLE));
        // A genuine later failure (real unplug) still recovers.
        assert!(g.try_arm(finished + SETTLE, SETTLE));
    }

    #[test]
    fn finish_reopens_the_gate_even_when_the_rebuild_failed() {
        let mut g = RebuildGate::default();
        let t = Instant::now();
        assert!(g.try_arm(t, SETTLE));
        // `finish` runs from the RAII guard on every exit path, including
        // the error ones — otherwise `armed` would latch forever and the
        // engine could never recover from a later device loss.
        g.finish(t);
        assert!(g.try_arm(t + SETTLE, SETTLE));
    }

    #[test]
    fn reopen_lets_a_rebuild_arm_inside_the_settle_window() {
        let mut g = RebuildGate::default();
        let t = Instant::now();
        // A deliberate output change opens the settle window...
        g.finish(t);
        // ...but the spawn failed and installed nothing, so we reopen.
        g.reopen();
        // The scheduled recovery lands 300 ms later — well inside what
        // would have been the 2 s window — and MUST now arm, otherwise
        // the engine is left with no output thread at all (#405).
        assert!(g.try_arm(t + Duration::from_millis(300), SETTLE));
    }

    #[test]
    fn collapses_a_full_cascade_into_a_single_rebuild() {
        // Replays the reporter's pattern: an error every ~300 ms, each one
        // queueing its own deferred recovery task. Mirrors the real call
        // order — a task arms only after its 300 ms backoff, so the gate
        // is consulted at `error + 300 ms`, not at error time.
        let mut g = RebuildGate::default();
        let start = Instant::now();
        let mut armed = 0;
        for i in 0..10 {
            let error_at = start + Duration::from_millis(300 * i);
            let arms_at = error_at + Duration::from_millis(300);
            if g.try_arm(arms_at, SETTLE) {
                armed += 1;
                // The rebuild itself takes a few tens of ms.
                g.finish(arms_at + Duration::from_millis(40));
            }
        }
        // 10 errors over 2.7 s collapse to the initial rebuild plus one
        // retry after the settle window — not one per error.
        assert_eq!(armed, 2);
    }
}

#[cfg(test)]
mod radio_resume_tests {
    use super::*;
    use std::path::PathBuf;

    fn url_cmd(url: &str, track_id: i64) -> AudioCmd {
        AudioCmd::LoadUrlAndPlay {
            intent: LoadIntent::from_raw(1),
            url: url.to_string(),
            ext_hint: Some("mp3".to_string()),
            track_id,
            title: Some("Test stream".to_string()),
            artist: Some("Test artist".to_string()),
            artwork_url: Some("https://example.invalid/art.jpg".to_string()),
            cache: None,
            duration_ms: None,
            seekable_file: false,
            replay_gain: TrackGain::default(),
        }
    }

    fn local_cmd(track_id: i64) -> AudioCmd {
        AudioCmd::LoadAndPlay {
            intent: LoadIntent::from_raw(1),
            path: PathBuf::from("/dev/null"),
            start_ms: 0,
            track_id,
            duration_ms: 1000,
            source_type: "test".into(),
            source_id: None,
            replay_gain: TrackGain::default(),
        }
    }

    fn remote_local_cmd(fallback_url: Option<&str>, track_id: i64) -> AudioCmd {
        AudioCmd::LoadRemoteFileAndPlay {
            intent: LoadIntent::from_raw(1),
            path: PathBuf::from("/dev/null"),
            start_ms: 0,
            track_id,
            duration_ms: 1000,
            title: Some("Remote track".to_string()),
            artist: Some("Remote artist".to_string()),
            artwork_url: None,
            fallback_url: fallback_url.map(str::to_string),
            discard_on_failure: false,
            replay_gain: TrackGain {
                gain_db: Some(-4.0),
                peak: None,
                peak_unverified: false,
            },
        }
    }

    #[test]
    fn load_url_writes_snapshot_verbatim() {
        let lock: Mutex<Option<RadioResumeState>> = Mutex::new(None);
        apply_radio_resume_update(&lock, &url_cmd("https://radio.invalid/live", -1));
        let snap = lock.lock().unwrap().clone().expect("snapshot stored");
        assert_eq!(snap.track_id, -1);
        assert_eq!(snap.title.as_deref(), Some("Test stream"));
        match snap.source {
            RadioResumeSource::Url { url, ext_hint, .. } => {
                assert_eq!(url, "https://radio.invalid/live");
                assert_eq!(ext_hint.as_deref(), Some("mp3"));
            }
            RadioResumeSource::RemoteFile { .. } => panic!("expected URL resume"),
        }
    }

    #[test]
    fn load_url_overwrites_previous_radio_session() {
        let lock: Mutex<Option<RadioResumeState>> = Mutex::new(None);
        apply_radio_resume_update(&lock, &url_cmd("https://first.invalid/", -1));
        apply_radio_resume_update(&lock, &url_cmd("https://second.invalid/", -2));
        let snap = lock.lock().unwrap().clone().expect("snapshot present");
        assert_eq!(snap.track_id, -2);
        assert!(matches!(
            snap.source,
            RadioResumeSource::Url { ref url, .. } if url == "https://second.invalid/"
        ));
    }

    #[test]
    fn load_and_play_clears_snapshot() {
        let lock: Mutex<Option<RadioResumeState>> = Mutex::new(None);
        apply_radio_resume_update(&lock, &url_cmd("https://radio.invalid/", -1));
        assert!(lock.lock().unwrap().is_some());
        apply_radio_resume_update(&lock, &local_cmd(42));
        assert!(
            lock.lock().unwrap().is_none(),
            "local-track LoadAndPlay must wipe the radio resume cache",
        );
    }

    #[test]
    fn online_device_change_reemits_remote_local_command_with_fallback() {
        let lock: Mutex<Option<RadioResumeState>> = Mutex::new(None);
        apply_radio_resume_update(
            &lock,
            &remote_local_cmd(Some("https://server.invalid/stream"), -3),
        );
        let snap = lock.lock().unwrap().clone().expect("snapshot stored");
        match snap.into_command(4_321, LoadIntent::from_raw(9)) {
            AudioCmd::LoadRemoteFileAndPlay {
                path,
                start_ms,
                track_id,
                duration_ms,
                fallback_url,
                replay_gain,
                ..
            } => {
                assert_eq!(path, PathBuf::from("/dev/null"));
                assert_eq!(start_ms, 4_321);
                assert_eq!(track_id, -3);
                assert_eq!(duration_ms, 1_000);
                assert_eq!(
                    fallback_url.as_deref(),
                    Some("https://server.invalid/stream")
                );
                assert_eq!(replay_gain.gain_db, Some(-4.0));
            }
            _ => panic!("device change must resume the reconciled local file"),
        }
    }

    #[test]
    fn offline_device_change_reemits_remote_local_command_without_fallback() {
        let lock: Mutex<Option<RadioResumeState>> = Mutex::new(None);
        apply_radio_resume_update(&lock, &url_cmd("https://radio.invalid/", -1));
        apply_radio_resume_update(&lock, &remote_local_cmd(None, -3));
        let snap = lock.lock().unwrap().clone().expect("local snapshot stored");
        match snap.into_command(7_654, LoadIntent::from_raw(9)) {
            AudioCmd::LoadRemoteFileAndPlay {
                path,
                start_ms,
                fallback_url,
                ..
            } => {
                assert_eq!(path, PathBuf::from("/dev/null"));
                assert_eq!(start_ms, 7_654);
                assert_eq!(fallback_url, None);
            }
            _ => panic!("offline device change must still resume locally"),
        }
    }

    #[test]
    fn unrelated_cmds_leave_snapshot_untouched() {
        let lock: Mutex<Option<RadioResumeState>> = Mutex::new(None);
        apply_radio_resume_update(&lock, &url_cmd("https://radio.invalid/", -1));
        let baseline = lock.lock().unwrap().clone();
        for cmd in [
            AudioCmd::Pause,
            AudioCmd::Resume,
            AudioCmd::Stop,
            AudioCmd::Seek(123),
            AudioCmd::SetVolume(0.5),
            AudioCmd::SetMono(true),
        ] {
            apply_radio_resume_update(&lock, &cmd);
        }
        let after = lock.lock().unwrap().clone();
        assert_eq!(
            baseline, after,
            "non-Load* commands must not touch the snapshot"
        );
    }
}
