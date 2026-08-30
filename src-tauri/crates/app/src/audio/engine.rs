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

/// Commands accepted by the decoder thread.
#[allow(dead_code)]
pub enum AudioCmd {
    LoadAndPlay {
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
                track_id, start_ms, ..
            } => write!(
                f,
                "LoadAndPlay {{ track_id: {track_id}, start_ms: {start_ms} }}"
            ),
            AudioCmd::LoadRemoteFileAndPlay {
                track_id, start_ms, ..
            } => write!(
                f,
                "LoadRemoteFileAndPlay {{ track_id: {track_id}, start_ms: {start_ms} }}"
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
            AudioCmd::LoadUrlAndPlay { url, track_id, .. } => {
                write!(f, "LoadUrlAndPlay {{ track_id: {track_id}, url: {url} }}")
            }
            AudioCmd::SwapProducer(_) => write!(f, "SwapProducer(<producer>)"),
            AudioCmd::Shutdown => write!(f, "Shutdown"),
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
    /// Windows-only opt-in: WASAPI Exclusive Mode preference. Read
    /// at boot from `profile_setting['audio.wasapi_exclusive']`,
    /// flipped by `set_wasapi_exclusive`. Used by `set_output_device`
    /// to preserve the mode across hot-swaps.
    wasapi_exclusive: std::sync::atomic::AtomicBool,
    /// Whether the current output stream is actually running in
    /// WASAPI Exclusive Mode. This can differ from the preference
    /// when init falls back to cpal shared mode.
    wasapi_exclusive_active: std::sync::atomic::AtomicBool,
    /// Debounce guard for [`Self::try_rebuild_after_device_error`]
    /// (#175). Windows session resets and USB DAC flaps fire the
    /// cpal `DeviceNotAvailable` callback on a random thread; the
    /// callback schedules a rebuild via tokio, and a quick double
    /// flap would otherwise queue two concurrent rebuilds that
    /// each interrupt the same track.
    rebuild_in_progress: std::sync::atomic::AtomicBool,
    /// Session-only kill switch for WASAPI Exclusive after a flap storm
    /// (#322). Once tripped, every rebuild / hot-swap stays on cpal
    /// shared regardless of the `wasapi_exclusive` preference, so a
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
    /// ([`Self::set_output_device`], [`Self::set_wasapi_exclusive`],
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
    fn into_command(self, position_ms: u64) -> AudioCmd {
        match self.source {
            RadioResumeSource::Url {
                url,
                ext_hint,
                replay_gain,
                duration_ms,
                seekable_file,
            } => AudioCmd::LoadUrlAndPlay {
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
    /// `wasapi_exclusive` is the persisted opt-in for Windows
    /// Exclusive Mode (silently no-op on Linux/macOS). On a failing
    /// init the engine falls back to cpal shared mode automatically;
    /// see [`spawn_output_with_mode`] for the contract.
    pub fn new_with_device(
        app: AppHandle,
        device_name: Option<String>,
        wasapi_exclusive: bool,
    ) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = unbounded::<AudioCmd>();
        let shared = Arc::new(SharedPlayback::new());

        // Analytics channel: decoder pushes `AnalyticsMsg`s at EOF, the
        // tokio `analytics_task` consumes them to write `play_event`
        // rows and self-send the next `LoadAndPlay`.
        let (analytics_tx, analytics_rx) = unbounded_channel::<AnalyticsMsg>();

        let (output, decoder, wasapi_exclusive_active) = match spawn_output_with_mode(
            shared.clone(),
            app.clone(),
            device_name,
            wasapi_exclusive,
            None,
        ) {
            Ok((producer, handle)) => {
                let active = handle.wasapi_exclusive;
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
            wasapi_exclusive: std::sync::atomic::AtomicBool::new(wasapi_exclusive),
            wasapi_exclusive_active: std::sync::atomic::AtomicBool::new(wasapi_exclusive_active),
            rebuild_in_progress: std::sync::atomic::AtomicBool::new(false),
            exclusive_suppressed: std::sync::atomic::AtomicBool::new(false),
            exclusive_flaps: Mutex::new(FlapWindow::default()),
            rebuild_gate: Mutex::new(RebuildGate::default()),
            radio_resume: Mutex::new(None),
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
        let pref_exclusive = self.wasapi_exclusive.load(Ordering::Relaxed);

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
                    "WASAPI exclusive disabled for this session after repeated device \
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
        self.force_rebuild_output(pinned, exclusive)
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
            self.wasapi_exclusive_active
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
        let exclusive_available = self.wasapi_exclusive.load(Ordering::Relaxed);
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
        let pref_exclusive = self.wasapi_exclusive.load(Ordering::Relaxed);
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
                    self.wasapi_exclusive_active
                        .store(handle.wasapi_exclusive, Ordering::Release);
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
                self.wasapi_exclusive_active
                    .store(handle.wasapi_exclusive, Ordering::Release);
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
    /// (device_name, exclusive) tuple, bypassing the same-device
    /// no-op check. Shared by [`Self::try_rebuild_after_device_error`]
    /// and (in the future) any other "rebuild without changing the
    /// user preference" path.
    fn force_rebuild_output(&self, device_name: Option<String>, exclusive: bool) -> AppResult<()> {
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
        // spawn can still roll back.
        let pre_release = guard.as_ref().is_some_and(|h| h.wasapi_exclusive);
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
        self.wasapi_exclusive_active.store(
            guard.as_ref().map(|h| h.wasapi_exclusive).unwrap_or(false),
            std::sync::atomic::Ordering::Release,
        );
        // Settings' exclusive-mode toggle only re-reads its state on
        // mount and after a manual click (issue #405) — this is the
        // one signal that tells it a rebuild just happened behind its
        // back, whether that landed in exclusive or fell back to shared.
        let _ = self.app.emit("player:audio-mode-changed", ());

        // Resume best-effort. Same async pattern as
        // `set_output_device` and `set_wasapi_exclusive` — pull the
        // track row off the synchronous path so a slow DB doesn't
        // hold the audio recovery up. Radio sessions resume by
        // re-dispatching the cached `LoadUrlAndPlay` instead of
        // looking up a (non-existent) `track` row.
        if was_playing {
            if track_id < 0 {
                if let Some(state) = self.snapshot_radio_resume() {
                    let _ = self.cmd_tx.send(state.into_command(position_ms));
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
                        // set_output_device and set_wasapi_exclusive.
                        let replay_gain =
                            crate::commands::player::fetch_replay_gain(&pool, track_id).await;
                        let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
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

        // Step 2 — open the new output thread first. The old one is
        // still running, which is fine: PipeWire / PulseAudio / ALSA
        // dmix all support multiple concurrent streams, and the two
        // streams target different devices anyway. If this fails we
        // return immediately without disturbing the working stream.
        let (producer, handle) = spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device_name,
            self.wasapi_exclusive
                .load(std::sync::atomic::Ordering::Relaxed),
            None,
        )?;

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
            if was_playing {
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
            // Mirror force_rebuild_output / set_wasapi_exclusive: if the
            // SwapProducer send failed the closure had already run
            // `guard.take()`, so no output thread remains — the exclusive
            // flag must stop claiming one and Settings must re-read (#405).
            // No-ops when an earlier Stop send failed before guard.take(),
            // where the old stream is still installed and the flag accurate.
            self.publish_output_lost_if_gone(&guard);
            return Err(err);
        }

        *guard = Some(handle);
        self.wasapi_exclusive_active.store(
            guard.as_ref().map(|h| h.wasapi_exclusive).unwrap_or(false),
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
            if track_id < 0 {
                if let Some(state) = self.snapshot_radio_resume() {
                    let _ = self.cmd_tx.send(state.into_command(position_ms));
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

    /// Flip the WASAPI Exclusive Mode preference and re-open the
    /// output stream using the new mode. No-ops on non-Windows.
    /// Re-uses the active device name so the user keeps their pick.
    pub fn set_wasapi_exclusive(&self, enabled: bool) -> AppResult<()> {
        let previous = self
            .wasapi_exclusive
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
        // `wasapi_exclusive_active` kept reporting the old mode: the toggle
        // sat latched on the very mode the user was trying to leave, with a
        // restart as the only way out. Release the old exclusive stream
        // FIRST so the new open finds a free device.
        let pre_release = guard.as_ref().is_some_and(|h| h.wasapi_exclusive);
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
                    self.wasapi_exclusive
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
                    // `wasapi_exclusive`. Pass `active` explicitly: the
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
                    self.wasapi_exclusive
                        .store(previous, std::sync::atomic::Ordering::Relaxed);
                }
                return Err(err);
            }
        };
        let active_mode = handle.wasapi_exclusive;

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
        self.wasapi_exclusive_active
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
            if track_id < 0 {
                if let Some(state) = self.snapshot_radio_resume() {
                    let _ = self.cmd_tx.send(state.into_command(position_ms));
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

    /// Whether the current output stream is actually running in
    /// WASAPI Exclusive Mode. Always `false` on Linux / macOS and
    /// also `false` after a Windows fallback to cpal shared mode.
    pub fn wasapi_exclusive(&self) -> bool {
        self.wasapi_exclusive_active
            .load(std::sync::atomic::Ordering::Acquire)
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
        match snap.into_command(4_321) {
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
        match snap.into_command(7_654) {
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
