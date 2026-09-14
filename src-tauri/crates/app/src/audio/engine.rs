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
use super::output::{spawn_output_with_mode, OutputHandle, RequestedFormat};
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
                // Redacted, not printed raw: a remote-queue URL is an
                // HMAC-signed streaming ticket and the signature rides in
                // its query string. This `Debug` is what every `?cmd` /
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
pub(super) const REBUILD_SETTLE_WINDOW: Duration = Duration::from_secs(2);

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

/// Releases the [`RebuildGate`] on **every** exit path of a rebuild,
/// including the early returns and a panic.
///
/// Held by both paths that arm the gate — the device-error recovery and
/// the default-device follow (#627). Without it a rebuild that bailed
/// out would leave `armed` latched and no later device event could ever
/// schedule anything again. Declare it FIRST in the function so it drops
/// last, after the `output` lock has been released: the settle window
/// should start when the rebuild is really over.
struct RebuildGateGuard<'a>(&'a Mutex<RebuildGate>);

impl Drop for RebuildGateGuard<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish(Instant::now());
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
    output: Mutex<OutputSlot>,
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
    /// Serializes the *publish* half of a dispatch (#632): the claim,
    /// the queue cursor, the events, and the command that carries the
    /// load. See [`Self::lock_publish`].
    publish: tokio::sync::Mutex<()>,
    /// Orders [`Self::pause_pending`] against the channel it describes.
    ///
    /// The two are separate synchronisation domains — an atomic and a
    /// queue — so without this, two producers sending a `Pause` and a
    /// `Resume` at the same instant can leave the flag saying one thing
    /// while the decoder receives the other order, and a rebuild reading
    /// the flag then decides against what the user actually asked for.
    ///
    /// A leaf lock held across two statements and nothing else: no await
    /// inside, no other lock taken under it. It is deliberately **not**
    /// [`Self::publish`], which is an async mutex a producer already
    /// holds when it calls [`Self::send`] — taking that one here would
    /// deadlock on the spot.
    dispatch: Mutex<()>,
    /// #617: park playback rather than carry it onto another endpoint
    /// when the one it was playing on goes away. Seeded at boot from
    /// `profile_setting['audio.pause_on_device_loss']`, default on.
    ///
    /// It only ever applies to a rebuild that *moved*: a device that
    /// flaps and comes back is reopened and picked up where it was, which
    /// is the automatic recovery #175 exists for. What it stops is the
    /// reported case — headphones unplugged, Windows falls back to the
    /// built-in speakers, and the music carries on out loud.
    pause_on_device_loss: std::sync::atomic::AtomicBool,
    /// When a device loss was last reported, so the default-device follow
    /// can stay out of the recovery's way (#617).
    ///
    /// Unplugging a device raises both signals at once: the stream breaks
    /// (`DeviceNotAvailable`) *and* the system default moves. They are not
    /// the same event — a device that DISAPPEARS is not a default that
    /// CHANGES — and whichever rebuild arms first would otherwise decide
    /// what happens, so the pause would land or not land on a coin toss.
    /// The loss owns the recovery for a short window; the follow stands
    /// down.
    last_device_loss: Mutex<Option<Instant>>,
    /// A session a rebuild parked instead of resuming: the user had
    /// paused it (#611), or its endpoint went away and the preference
    /// above says not to follow (#617).
    ///
    /// This is what makes Play work afterwards for a radio stream or a
    /// server track: the persisted resume point is always a *library*
    /// track's, so parking one of those and pressing Play used to bring
    /// back the last library track instead.
    parked_resume: Mutex<Option<ParkedSession>>,
    /// The last thing we told the user about the output (#597), so a
    /// state that persists is announced once instead of at every track.
    /// `None` means "nothing to say", which is also what clears it.
    last_output_notice: Mutex<Option<PlaybackNotice>>,
    /// A pause the user has asked for that the decoder has not acted on
    /// yet (#611).
    ///
    /// `SharedPlayback::paused_output` is the decoder's answer, and it
    /// lags by design: it is raised when the `Pause` is *processed*,
    /// which can be a decode cycle or a ring-poll interval after it was
    /// sent. A rebuild reading it inside that gap decides `Play` for a
    /// session the user has just paused — and its resume then clears the
    /// flag and starts the music, so the pause disappears. This is the
    /// same question asked one step earlier, on the side where the
    /// answer is already known.
    ///
    /// Maintained at the [`Self::send`] boundary, which every `Pause`,
    /// `Resume` and user-facing load passes through. The two paths that
    /// bypass it hold the channel directly — the auto-advance and a
    /// rebuild's own resume — and neither can run against a paused
    /// session: a paused track never ends, and a rebuild only resumes
    /// when it has just decided the session was not paused.
    pause_pending: std::sync::atomic::AtomicBool,
    /// The rate the last output open was *asked* for, `0` for "nothing
    /// in particular" (#600).
    ///
    /// The per-track re-open compares against this rather than against
    /// the rate the stream actually runs at, and the difference is the
    /// whole guard: a device that refused 96 kHz once refuses it every
    /// time, so comparing against the running rate would tear the output
    /// down and rebuild it for every single track, forever.
    last_requested_rate: std::sync::atomic::AtomicU32,
}

/// A session a rebuild parked — the load, and where to pick it up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParkedSession {
    load: LastLoad,
    start_ms: u64,
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
    pub(crate) fn into_command(self, intent: LoadIntent) -> AudioCmd {
        let start_ms = self.start_ms;
        self.load.into_command(intent, start_ms)
    }
}

/// How long a device loss owns the recovery, keeping the default-device
/// follow off the same rebuild (#617). Comfortably longer than the
/// recovery's own 300 ms backoff, short enough that a default change a
/// few seconds later is still followed normally.
const DEVICE_LOSS_OWNERSHIP: Duration = Duration::from_secs(3);

/// Why the output is being rebuilt. It changes exactly one decision:
/// whether landing on a *different* endpoint should park playback
/// instead of resuming it (#617).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RebuildCause {
    /// The stream we were playing on failed — the device may be gone.
    DeviceLost,
    /// We are moving on purpose: the user asked for a reopen, or the
    /// system's default changed while nothing was pinned (#627). Neither
    /// is a device disappearing, and neither pauses anything.
    Deliberate,
}

/// Whether a rebuild put the audio on a different endpoint than the one
/// it was on.
///
/// Unknown on either side means "not moved", deliberately: the names
/// come from different backends (cpal's display name, WASAPI's friendly
/// name, and hog mode can name nothing at all), and the cost of a wrong
/// "moved" is playback stopping for a user who asked for none of this.
/// A wrong "not moved" only costs the pause #617 adds, leaving the
/// behaviour that shipped before it.
fn endpoint_moved(before: Option<&str>, after: Option<&str>) -> bool {
    matches!((before, after), (Some(before), Some(after)) if before != after)
}

/// The output slot: the installed stream, and the device the user picked.
///
/// Both under **one** lock on purpose. The pin has to outlive the handle
/// (#630) — `self.handle` is empty whenever a spawn failed after the old
/// stream was released, which is the state `publish_output_lost_if_gone`
/// exists for — and every question worth asking pairs the two: what is
/// playing *and* what was asked for. Keeping the pin in its own lock would
/// let a reader match one against a stale view of the other, which is the
/// defect #628 had to fix in the picker.
struct OutputSlot {
    handle: Option<OutputHandle>,
    /// The user's device choice, `None` for "the OS default".
    ///
    /// Seeded at construction from `profile_setting['audio.output_device']`
    /// and updated by [`AudioEngine::set_output_device`] when a pick
    /// succeeds. Before #630 the pin lived only inside `OutputHandle`, so
    /// with no stream installed the engine answered "nothing pinned": a
    /// forced reopen targeted the OS default instead of the user's device,
    /// the picker could not flag the pinned row it had just failed to open,
    /// and MPD reported no device at all.
    pinned: Option<String>,
}

impl OutputSlot {
    /// The device the user is pinned to: the live handle's name while a
    /// stream is installed, the seeded pin otherwise.
    ///
    /// The handle comes first because it is the one `set_output_device`
    /// and the rebuilds keep in step with the stream; `pinned` is the
    /// answer for the window where there is no stream at all.
    fn pinned_device(&self) -> Option<String> {
        self.handle
            .as_ref()
            .and_then(|h| h.device_name.clone())
            .or_else(|| self.pinned.clone())
    }
}

/// What the output really is right now (#597).
///
/// Computed by the engine rather than by the UI, so the badge in the
/// player, the notice the user gets when something silently degrades and
/// the Settings card cannot drift apart — they all read the same rule.
/// Serialised in kebab-case, the shape the frontend switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMode {
    /// The system mixer is in the path, which is the normal case.
    Shared,
    /// The stream owns the device: WASAPI Exclusive, a raw ALSA `hw:`,
    /// CoreAudio hog mode.
    Exclusive,
    /// Native DSD is reaching the DAC over DoP (#495). It implies
    /// exclusive, and says more, so it is reported instead.
    Dop,
    /// Exclusive was asked for and the device would not give it. Nothing
    /// failed — playback is fine, in shared mode — but the user chose
    /// otherwise and nothing told them (#597).
    ExclusiveRefused,
}

/// The rule behind [`AudioEngine::output_mode`], pure so it can be
/// tested without a sound card.
///
/// DoP first: on Linux and macOS the DoP toggle engages the exclusive
/// path by itself, so `requested_exclusive` can be false while the DAC
/// is being handed native DSD.
fn output_mode_of(
    requested_exclusive: bool,
    engaged_exclusive: bool,
    engaged_dop: bool,
) -> OutputMode {
    if engaged_dop {
        OutputMode::Dop
    } else if engaged_exclusive {
        OutputMode::Exclusive
    } else if requested_exclusive {
        OutputMode::ExclusiveRefused
    } else {
        OutputMode::Shared
    }
}

/// Something the user should know about playback that is **not** a
/// failure (#597).
///
/// Deliberately a different register from `player:error`: losing the
/// device is a fault, falling back to shared mode is normal operation
/// that happens to contradict a choice the user made. Announced once per
/// transition — a DAC that refuses exclusive refuses it at every track,
/// and saying so every time is nagging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackNotice {
    /// Exclusive was requested, the device opened shared.
    ExclusiveRefused,
    /// A DSD track was offered over DoP and the DAC would not take it;
    /// it is being converted to PCM instead.
    DopRefused,
    /// The output device went away and playback was parked rather than
    /// moved onto whatever the system fell back to (#617).
    PausedDeviceLost,
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
        path: PathBuf,
        duration_ms: u64,
        fallback_url: Option<String>,
        replay_gain: TrackGain,
    },
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
    fn into_command(self, intent: LoadIntent, start_ms: u64) -> AudioCmd {
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
                path,
                duration_ms,
                fallback_url,
                replay_gain,
            } => AudioCmd::LoadRemoteFileAndPlay {
                intent,
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
struct LivePosition {
    /// The intent of the load the decoder had accepted at that moment.
    intent: Option<LoadIntent>,
    /// The track the decoder had actually started at that moment.
    track_id: i64,
    position_ms: u64,
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
fn resume_start_ms(load: &LastLoad, live: LivePosition) -> u64 {
    if live.intent == Some(load.intent) && live.track_id == load.track_id {
        live.position_ms
    } else {
        load.start_ms
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
        // Kept before `device_name` is handed to the spawn: this is the
        // persisted pin, and it must survive an open that fails (#630).
        let pinned = device_name.clone();
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
            output: Mutex::new(OutputSlot {
                handle: output,
                pinned,
            }),
            decoder: Mutex::new(decoder),
            app,
            exclusive_output: std::sync::atomic::AtomicBool::new(exclusive_output),
            exclusive_output_active: std::sync::atomic::AtomicBool::new(exclusive_output_active),
            rebuild_in_progress: std::sync::atomic::AtomicBool::new(false),
            resume_in_flight: std::sync::atomic::AtomicBool::new(false),
            exclusive_suppressed: std::sync::atomic::AtomicBool::new(false),
            exclusive_flaps: Mutex::new(FlapWindow::default()),
            rebuild_gate: Mutex::new(RebuildGate::default()),
            publish: tokio::sync::Mutex::new(()),
            dispatch: Mutex::new(()),
            // On unless the profile says otherwise: what people expect
            // from headphones, and the reported defect is the other
            // behaviour. Seeded properly in `lib.rs` once the profile
            // pool is up.
            pause_on_device_loss: std::sync::atomic::AtomicBool::new(true),
            last_device_loss: Mutex::new(None),
            parked_resume: Mutex::new(None),
            last_output_notice: Mutex::new(None),
            pause_pending: std::sync::atomic::AtomicBool::new(false),
            last_requested_rate: std::sync::atomic::AtomicU32::new(0),
        })
    }

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
        LoadIntent::claim(&self.shared)
    }

    /// Whether a newer load has claimed the dispatch since `intent` did.
    ///
    /// The read-only sibling of [`Self::claim_dispatch`], for the caller
    /// that has already claimed and needs to know whether it was overtaken
    /// **after** that — a rollback, typically. Claiming again would
    /// succeed on its own mark and answer nothing.
    pub fn load_intent_superseded(&self, intent: LoadIntent) -> bool {
        intent.get()
            < self
                .shared
                .newest_load_intent
                .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Claim the right to dispatch `intent`, or report that a newer load
    /// has already taken it (#632).
    ///
    /// Every load path publishes on its way to the decoder — it relabels
    /// the player bar, and the queue-moving ones write
    /// `queue.current_index` before they send. Since #622 the decoder drops
    /// a load older than one already delivered, and those side effects
    /// survived it: the UI named one track while another played, and the
    /// cursor sat on a row nothing was playing, so the next auto-advance
    /// stepped from the wrong place.
    ///
    /// So the arbitration moves one step earlier. This is a
    /// compare-and-set on the same high-water mark the decoder reads: it
    /// succeeds only for an intent at least as new as the newest already
    /// claimed, and it publishes that claim in the same operation. **Call
    /// it immediately before the first side effect** — the queue write,
    /// the emit, the state change — and give up entirely when it returns
    /// `false`.
    ///
    /// It does not replace the decoder's own check. Two claims can succeed
    /// in order and still reach the channel out of order, and only the
    /// decoder sees what was actually delivered. What this removes is the
    /// window that lasts as long as a database read.
    ///
    /// It also does not, on its own, order what comes *after* it: see
    /// [`Self::lock_publish`], which every caller holds across the claim
    /// and the publish that follows.
    pub fn claim_dispatch(&self, intent: LoadIntent) -> bool {
        self.shared.try_claim_load(intent.get())
    }

    /// Send a command to the decoder. Returns `AppError::Audio` if the
    /// channel is disconnected (decoder thread has exited).
    ///
    /// Not the only way in: the analytics task's auto-advance and the
    /// output rebuilds hold their own clone of the channel. Anything that
    /// has to see *every* load therefore belongs on the receiving side —
    /// which is where [`LastLoad`] is recorded.
    pub fn send(&self, cmd: AudioCmd) -> AppResult<()> {
        // One critical section for both writes — see [`Self::dispatch`].
        // The user's intent is recorded before the decoder has acted on
        // it (see [`Self::pause_pending`]), and it has to reach the flag
        // in the order it reaches the channel.
        let _dispatch = self
            .dispatch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = self
            .pause_pending
            .load(std::sync::atomic::Ordering::Acquire);
        self.pause_pending.store(
            pause_pending_after(&cmd, previous),
            std::sync::atomic::Ordering::Release,
        );
        self.cmd_tx.send(cmd).map_err(|e| {
            // Nothing was queued, so the intent this recorded is one the
            // decoder will never see either.
            self.pause_pending
                .store(previous, std::sync::atomic::Ordering::Release);
            AppError::Audio(format!("audio command channel closed: {e}"))
        })
    }

    /// Take the publish lock: hold it across [`Self::claim_dispatch`]
    /// and everything that claim authorizes — the queue cursor, the
    /// `track:changed` / `queue:changed` emits, and the `AudioCmd` that
    /// carries the load.
    ///
    /// The claim cannot order those by itself, for two reasons that both
    /// bite:
    ///
    /// - `queue::commit_index` is a **database write**, so it is an
    ///   await. An older producer that claimed first can be overtaken
    ///   during it, and its cursor write then lands *after* the newer
    ///   one's — leaving the cursor (and then the label) on a track the
    ///   decoder is about to drop, which is the whole defect #632 is
    ///   about.
    /// - the runtime is **multi-threaded**. Even with no await between
    ///   the claim and the send, two producers run on two workers, so
    ///   "this task does not yield" buys nothing against the other one.
    ///
    /// Under the lock, claim order and publish order are the same order,
    /// and the claim keeps its own job: lock acquisition can invert
    /// intent order — a newer selection may well get the lock first —
    /// and the claim is what makes the older one then give up.
    ///
    /// Deliberately **not** held across the preparation that precedes
    /// the claim: reading the track, fetching ReplayGain, filling a
    /// queue. Those awaits are the reason the claim exists in the first
    /// place, and serializing them would make a Next wait behind a
    /// ten-thousand-row queue fill instead of superseding it.
    pub async fn lock_publish(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.publish.lock().await
    }

    /// What the output really is right now (#597) — what the badge in
    /// the player shows, and what the Settings card means by "engaged".
    ///
    /// One acquisition for both halves of the answer: read separately, a
    /// rebuild landing in between could pair the old stream's exclusive
    /// flag with the new stream's DoP one.
    pub fn output_mode(&self) -> OutputMode {
        let (engaged_exclusive, engaged_dop) = self
            .output
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .handle
                    .as_ref()
                    .map(|handle| (handle.exclusive, handle.dop.is_some()))
            })
            .unwrap_or((false, false));
        output_mode_of(
            self.exclusive_output
                .load(std::sync::atomic::Ordering::Relaxed),
            engaged_exclusive,
            engaged_dop,
        )
    }

    /// Record what the output actually opened as, and tell the user when
    /// that is not what they asked for (#405, #597).
    ///
    /// The single place `exclusive_output_active` is written on a
    /// successful open, because three things have to stay in step: the
    /// flag the Settings card reads, the `player:audio-mode-changed`
    /// event that makes it re-read — a rebuild can flip the engaged mode
    /// behind its back — and the notice that says so in words.
    ///
    /// `handle` is `None` only when a caller has already given the stream
    /// up, which the loss path reports separately.
    fn publish_output_mode(
        &self,
        requested: Option<RequestedFormat>,
        dop_requested: bool,
        requested_exclusive: bool,
        handle: Option<&OutputHandle>,
    ) {
        let engaged_exclusive = handle.is_some_and(|handle| handle.exclusive);
        let engaged_dop = handle.is_some_and(|handle| handle.dop.is_some());
        // What **this** open asked for, which is what the next track
        // compares against (#600) — see `last_requested_rate`. Recorded
        // here rather than at each call site so an open that asks for
        // nothing in particular, like a device switch, clears it: leaving
        // a stale rate there would make the next track think its rate was
        // already installed.
        //
        // Deliberately not `dop_requested`, which is a separate argument
        // for exactly this reason: the PCM open that follows a *refused*
        // DoP asked for no rate at all. Recording the DoP rate there left
        // every following track disagreeing with it, and rebuilding the
        // output — an audible gap per track — to install nothing new.
        self.last_requested_rate.store(
            requested.map_or(0, |r| r.sample_rate),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.exclusive_output_active
            .store(engaged_exclusive, std::sync::atomic::Ordering::Release);
        // Settings' exclusive-mode toggle only re-reads its state on
        // mount and after a manual click (issue #405) — this is the one
        // signal that tells it a rebuild just happened behind its back,
        // whether that landed in exclusive or fell back to shared.
        let _ = self.app.emit("player:audio-mode-changed", ());

        // What to say about it, in the order of what the user loses
        // most: a DSD stream converted to PCM, then an exclusive path
        // they asked for and did not get.
        let notice = if dop_requested && !engaged_dop {
            Some(PlaybackNotice::DopRefused)
        } else if requested_exclusive && !engaged_exclusive && handle.is_some() {
            Some(PlaybackNotice::ExclusiveRefused)
        } else {
            None
        };
        self.announce_output_notice(notice);
    }

    /// Emit `notice` only when it changes what the user was last told
    /// (#597). `None` clears the memory without saying anything, so a
    /// device that starts accepting exclusive again can be reported when
    /// it next refuses.
    fn announce_output_notice(&self, notice: Option<PlaybackNotice>) {
        let changed = match self.last_output_notice.lock() {
            Ok(mut guard) => {
                let changed = *guard != notice;
                *guard = notice;
                changed
            }
            // A poisoned lock costs a repeat, never a silence.
            Err(_) => true,
        };
        if let (true, Some(notice)) = (changed, notice) {
            self.emit_playback_notice(notice);
        }
    }

    /// Tell the UI about `notice`, unconditionally. For the one-shot
    /// events that are not a persisting state — see
    /// [`Self::announce_output_notice`] for the ones that are.
    pub(super) fn emit_playback_notice(&self, notice: PlaybackNotice) {
        tracing::info!(?notice, "playback notice");
        let _ = self
            .app
            .emit("player:notice", serde_json::json!({ "kind": notice }));
    }

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
    /// [`super::output::notify_device_lost`], i.e. by every backend, and
    /// at the moment of the error rather than after the recovery's
    /// backoff — that is what makes it beat the default-change
    /// notification the same unplug raises.
    pub(super) fn note_device_loss(&self) {
        if let Ok(mut guard) = self.last_device_loss.lock() {
            *guard = Some(Instant::now());
        }
    }

    /// Whether a device loss is still being recovered — see
    /// [`DEVICE_LOSS_OWNERSHIP`].
    fn device_loss_in_flight(&self) -> bool {
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
    fn paused_for_rebuild(&self) -> bool {
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
    fn live_position(&self) -> LivePosition {
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
    fn resume_after_rebuild(&self, live: LivePosition) {
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
    fn park_session(&self, live: LivePosition, track_id: i64) {
        if let Some(load) = self.shared.last_load() {
            let start_ms = resume_start_ms(&load, live);
            if let Ok(mut guard) = self.parked_resume.lock() {
                *guard = Some(ParkedSession { load, start_ms });
            }
        }
        super::decoder::transition_state(
            &self.shared,
            &self.app,
            super::state::PlayerState::Idle,
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
            .and_then(|guard| guard.pinned_device())
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
            .map(|guard| {
                (
                    guard.handle.as_ref().and_then(|h| h.opened_device.clone()),
                    guard.pinned_device(),
                )
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
        // Deliberate: the user asked for this device, so landing on it
        // is the point and nothing here pauses (#617).
        let result =
            self.force_rebuild_output(RebuildDevice::Pinned, exclusive, RebuildCause::Deliberate);
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
            .and_then(|guard| guard.handle.as_ref().map(|h| h.dop.is_some()))
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

        // Release the #365 gate on EVERY exit path below, including the
        // early `return`s and any panic.
        let _gate = RebuildGateGuard(&self.rebuild_gate);

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
        //
        // The one caller that can land somewhere else than it asked for:
        // when the pinned endpoint is really gone, the backend falls back
        // to the default, and that is the move #617 refuses to follow the
        // music onto.
        self.force_rebuild_output(
            RebuildDevice::Explicit(pinned),
            exclusive,
            RebuildCause::DeviceLost,
        )
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

    /// Move an unpinned output onto the system's new default device
    /// (#627).
    ///
    /// Called (after a backoff) by
    /// [`super::output::schedule_default_device_follow`] when the
    /// platform listener in [`super::default_device`] reports that the
    /// system default moved. Four questions, in this order:
    ///
    /// 1. **is anything pinned?** A user who picked a device asked for
    ///    that device; following the system would undo the pin. The
    ///    answer here is advisory — a pick landing between this read and
    ///    the rebuild would slip through it — so the rebuild asks again
    ///    under the lock that installs pins, and that answer is the
    ///    binding one. This early read only keeps the common case from
    ///    doing any work.
    /// 2. **would the move change anything?** Interrupting playback to
    ///    reopen the endpoint we are already on is a gap in the music
    ///    for no reason, and a default that disappeared entirely is
    ///    nothing to follow. See [`should_follow_default`].
    /// 3. **exclusive?** The preference, minus the #322 session
    ///    suppression — exactly what [`Self::reopen_output_device`]
    ///    computes. The suppression is honoured but NOT reset: this is
    ///    the system's decision, not the fresh chance a re-toggle or a
    ///    device pick is. No flap is recorded either; a default change
    ///    is not a device resetting under us, and counting it would let
    ///    a few legitimate device switches disable exclusive for the
    ///    session.
    /// 4. **is a rebuild already under way?** Only then is the #365 gate
    ///    armed — and a `false` there is reported as
    ///    [`FollowOutcome::Deferred`] rather than swallowed, because the
    ///    system default and the open device would otherwise stay apart
    ///    for good.
    pub(super) fn follow_os_default_output(&self) -> AppResult<FollowOutcome> {
        use std::sync::atomic::Ordering;

        let (opened, pinned) = self.current_output_devices();
        if let Some(pinned) = pinned {
            tracing::debug!(
                device = %pinned,
                "default output device changed, but a device is pinned; staying on it"
            );
            return Ok(FollowOutcome::Settled);
        }

        // Unplugging a device raises both signals at once: the stream
        // breaks and the default moves. They are not the same event — a
        // device that disappears is not a default that changes — and
        // without this, whichever rebuild armed first would decide, so
        // #617's pause would land or not land on a coin toss. The loss
        // recovery owns it; this one stands down rather than deferring,
        // because the recovery ends on the endpoint the system fell back
        // to anyway, which is exactly what this follow would have opened.
        if self.device_loss_in_flight() {
            tracing::debug!(
                "default output device changed while a device loss is being recovered; \
                 leaving it to the recovery"
            );
            return Ok(FollowOutcome::Settled);
        }

        let new_default = super::output::os_default_output_name();
        if !should_follow_default(opened.as_deref(), new_default.as_deref()) {
            tracing::debug!(
                opened = opened.as_deref().unwrap_or("<unnamed>"),
                new_default = new_default.as_deref().unwrap_or("<none>"),
                "default output device change needs no rebuild"
            );
            return Ok(FollowOutcome::Settled);
        }

        // Only now is the #365 gate armed, and only now is its release
        // owed: everything above is a cheap read, and arming for a
        // follow that then does nothing would open a settle window a
        // real device loss gets swallowed by — dropped, and never
        // retried, which is silence until the user intervenes. Arming
        // here still collapses a burst, since every notification of the
        // same change reaches this point and only the first one arms.
        if !self.try_arm_device_rebuild() {
            // Not "ignore": a rebuild in flight, or the quiet period after
            // one, would otherwise swallow the change for good and leave
            // the stream on a device the system no longer prefers. The
            // caller comes back once the window is over, and everything
            // above is re-read then, so the retry converges on whatever
            // the default is at that point rather than on this
            // notification's idea of it.
            tracing::debug!(
                "default-device follow: a rebuild is already armed or the settle \
                 window is open; deferring this default change"
            );
            return Ok(FollowOutcome::Deferred);
        }
        let _gate = RebuildGateGuard(&self.rebuild_gate);

        let exclusive = self.exclusive_output.load(Ordering::Relaxed)
            && !self.exclusive_suppressed.load(Ordering::Relaxed);
        tracing::info!(
            from = opened.as_deref().unwrap_or("<unnamed>"),
            to = new_default.as_deref().unwrap_or("<none>"),
            exclusive,
            "following the system's new default output device"
        );
        // Deliberate: the system default moved while the device we were
        // on is still there. Following it is the whole feature (#627),
        // and #617 has nothing to say about it — see the loss check at
        // the top of this method for the case where the two coincide.
        self.force_rebuild_output(
            RebuildDevice::OsDefaultIfUnpinned,
            exclusive,
            RebuildCause::Deliberate,
        )?;
        Ok(FollowOutcome::Settled)
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
        requested: Option<RequestedFormat>,
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
        // The gate is DoP's alone: a rate request (#600) is only ever
        // made when an exclusive stream is already open, and it is a
        // preference rather than a format the platform has to support.
        let requested = match requested {
            Some(r) if r.dop && !exclusive_available => None,
            other => other,
        };
        let dop = requested.and_then(RequestedFormat::as_dop);

        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        let has_output = guard.handle.is_some();
        // Compared as a whole `DopFormat`, not just the rate: an
        // exclusive DoP stream is opened for a fixed interleave, so two
        // consecutive DSD tracks at the same DoP rate but different
        // channel counts still need a re-open (reusing the stereo stream
        // for a multichannel one would tear frames across channels).
        let current_dop = guard.handle.as_ref().and_then(|h| h.dop);
        let want_dop = dop;

        // The rate this open would ask for, compared against what the
        // last one asked for — not against what it got (#600). A device
        // that refused the track's rate has its own rate installed, and
        // comparing against that would rebuild the output on every
        // single track for as long as the preference stayed on.
        let want_rate = requested.map_or(0, |r| r.sample_rate);
        let rate_unchanged = self.last_requested_rate.load(Ordering::Relaxed) == want_rate;

        // Already in the right shape: nothing to do. An ordinary PCM track
        // following another PCM track lands here and pays nothing.
        if has_output && current_dop == want_dop && rate_unchanged {
            return Ok((None, dop.is_some()));
        }

        // Capture the pinned device + exclusive preference before dropping
        // the old handle. A DoP (exclusive) open can't proceed while the
        // previous exclusive client still holds the device, so release it
        // first (#322 reasoning) — this path always replaces the stream.
        // Resolved from the slot, so a DoP re-open while no stream is
        // installed still targets the user's device rather than the OS
        // default (#630).
        let device = guard.pinned_device();
        let pref_exclusive = self.exclusive_output.load(Ordering::Relaxed);
        if let Some(old) = guard.handle.take() {
            old.stop();
        }

        // Try the requested DoP format first.
        if let Some(dop_fmt) = dop {
            match spawn_output_with_mode(
                self.shared.clone(),
                self.app.clone(),
                device.clone(),
                pref_exclusive,
                Some(RequestedFormat::dop(dop_fmt)),
            ) {
                Ok((producer, handle)) => {
                    guard.handle = Some(handle);
                    self.publish_output_mode(
                        Some(RequestedFormat::dop(dop_fmt)),
                        true,
                        pref_exclusive,
                        guard.handle.as_ref(),
                    );
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

        // Normal PCM output: DoP wasn't requested, or was refused. A
        // rate request rides along — the backends treat it as a
        // preference and fall back to a rate the device does offer.
        let pcm_request = requested.filter(|r| !r.dop);
        match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device,
            pref_exclusive,
            pcm_request,
        ) {
            Ok((producer, handle)) => {
                guard.handle = Some(handle);
                // Two different facts, and they must not be conflated.
                // `dop.is_some()` here means a DoP open was tried and
                // refused — the track is DSD, the opt-in is on, and it is
                // about to be converted to PCM without a word (#597), so
                // the notice needs it. The *rate* this open asked for is
                // `pcm_request`, which in that case is none at all:
                // recording the DoP rate instead left every following
                // track disagreeing with the guard and rebuilding the
                // output for nothing.
                self.publish_output_mode(
                    pcm_request,
                    dop.is_some(),
                    pref_exclusive,
                    guard.handle.as_ref(),
                );
                Ok((Some(producer), false))
            }
            Err(err) => {
                // No output at all now — surface the loss like the other
                // rebuild paths so the UI doesn't think playback is live.
                self.publish_output_lost_if_gone(&guard.handle);
                Err(err)
            }
        }
    }

    /// Internal helper: rebuild the output stream against the given
    /// ([`RebuildDevice`], exclusive) pair, bypassing the same-device
    /// no-op check. Shared by [`Self::try_rebuild_after_device_error`],
    /// [`Self::reopen_output_device`] and the default-device follow.
    ///
    /// `cause` decides one thing only: whether ending up on a *different*
    /// endpoint parks playback rather than resuming it (#617).
    fn force_rebuild_output(
        &self,
        device: RebuildDevice,
        exclusive: bool,
        cause: RebuildCause,
    ) -> AppResult<()> {
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        // Resolve the target under the very lock this is about to swap
        // the handle with, so nothing can move it in between (#612).
        let device_name = match device {
            RebuildDevice::Pinned => guard.pinned_device(),
            RebuildDevice::Explicit(name) => name,
            // Decided under the very lock a successful pick installs the
            // pin with (#627), which is what makes "only when nothing is
            // pinned" true rather than probable: a device chosen between
            // the notification and here must not be undone by a rebuild
            // aimed at the system default.
            RebuildDevice::OsDefaultIfUnpinned => {
                if let Some(pinned) = guard.pinned_device() {
                    tracing::debug!(
                        device = %pinned,
                        "default-device follow abandoned: a device was pinned meanwhile"
                    );
                    return Ok(());
                }
                None
            }
        };

        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Where the audio actually was, read before the handle goes: it
        // is the only half of "did we move?" that disappears with it
        // (#617).
        let previous_endpoint = guard
            .handle
            .as_ref()
            .and_then(|handle| handle.opened_device.clone());
        // Both a session that was playing and one the user paused hold a
        // track the rebuild has to interrupt; only what follows differs.
        let was_playing = resume != RebuildResume::Nothing;
        // Read here, before the stop: opening the replacement writes its
        // own sample rate into the shared block, and a position derived
        // from the old rate's sample count against the new one is simply a
        // wrong number. What it is the position *of* is stamped onto it,
        // because the load to resume can only be read after the swap
        // (#634) — an exclusive open costs tens to hundreds of
        // milliseconds, and a track the user picks during it is what the
        // decoder will be holding by then.
        let live = self.live_position();

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
            must_release_before_reopening(guard.handle.as_ref().map(|h| h.exclusive), exclusive);
        if pre_release {
            if was_playing {
                self.cmd_tx
                    .send(AudioCmd::Stop)
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            if let Some(old) = guard.handle.take() {
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
                self.publish_output_lost_if_gone(&guard.handle);
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
            if let Some(old) = guard.handle.take() {
                old.stop();
            }
            self.cmd_tx
                .send(AudioCmd::SwapProducer(producer))
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            Ok::<(), AppError>(())
        })();
        if let Err(err) = send_result {
            handle.stop();
            self.publish_output_lost_if_gone(&guard.handle);
            return Err(err);
        }
        guard.handle = Some(handle);
        self.publish_output_mode(None, false, exclusive, guard.handle.as_ref());

        // Re-read the decision now rather than trust the one taken
        // before the open. An exclusive open costs tens to hundreds of
        // milliseconds, and a Pause the user hits during it reaches the
        // decoder first: resuming on the strength of a stale `Play`
        // would start a track they had just stopped. The `Stop` above
        // does not clear either input — it unloads the track without
        // touching the state or `paused_output` — so this reads what the
        // user last asked for.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        if resume == RebuildResume::StayPaused {
            // The user had paused it (#611): the rebuild reopens the
            // output and hands the session to Play, it does not start
            // music nobody asked to hear.
            self.park_session(live, track_id);
        }
        if resume == RebuildResume::Play {
            // #617 — the device that went away is not the device we came
            // back on, so the music would be playing somewhere the user
            // did not choose. That is the reported case: headphones off,
            // Windows falls back to the speakers, the album keeps going
            // out loud. A device that flapped and came back is a
            // different matter, and lands here as "not moved".
            let landed_on = guard
                .handle
                .as_ref()
                .and_then(|handle| handle.opened_device.as_deref());
            let moved = endpoint_moved(previous_endpoint.as_deref(), landed_on);
            if cause == RebuildCause::DeviceLost && moved && self.pause_on_device_loss() {
                tracing::info!(
                    from = previous_endpoint.as_deref().unwrap_or("<unnamed>"),
                    to = landed_on.unwrap_or("<unnamed>"),
                    "the output device went away and the fallback is another one; parking playback"
                );
                // Playback stopping on its own needs a reason on screen,
                // not just in the log (#597). Not deduplicated: this one
                // is an event, not a state, and it answers a question the
                // user is asking right now.
                self.emit_playback_notice(PlaybackNotice::PausedDeviceLost);
                self.park_session(live, track_id);
            } else {
                self.resume_after_rebuild(live);
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
        //
        // Two things about this comparison are deliberate. It reads the
        // handle rather than `pinned_device()`, so that with no stream
        // installed — an open that failed — picking the pinned device again
        // still reaches the rebuild instead of early-returning on the pin
        // (#612, #630). And it only short-circuits **while a handle
        // exists**: without one, `current` is `None`, so a user asking for
        // the OS default matched `None == None` and got a no-op in exactly
        // the state they were trying to recover from. Spamming the menu is
        // only worth a no-op when there is a stream to leave alone.
        let current = guard.handle.as_ref().and_then(|h| h.device_name.as_deref());
        let requested = device_name.as_deref();
        if guard.handle.is_some() && current == requested {
            return Ok(());
        }
        // Kept before the spawn below consumes `device_name`: this is the
        // pin to record once the pick succeeds (#630).
        let pinned_pick = device_name.clone();

        // A different device may support exclusive fine — clear any #322
        // flap-storm suppression tied to the previous device.
        self.reset_exclusive_suppression();

        // Snapshot what's playing so we can resume on the new device.
        // What this rebuild owes the session, by the same rule the
        // device-error path uses (#611): a session the user had paused
        // is parked, not started. Picking a device or flipping the mode
        // is a deliberate act on the *output*, never a request to play —
        // and this path resumed a paused track into audible playback.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Both a session that was playing and one the user paused hold a
        // track the rebuild has to interrupt; only what follows differs.
        let was_playing = resume != RebuildResume::Nothing;
        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        // Read here, before the stop: opening the replacement writes its
        // own sample rate into the shared block, and a position derived
        // from the old rate's sample count against the new one is simply a
        // wrong number. What it is the position *of* is stamped onto it,
        // because the load to resume can only be read after the swap
        // (#634) — an exclusive open costs tens to hundreds of
        // milliseconds, and a track the user picks during it is what the
        // decoder will be holding by then.
        let live = self.live_position();

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
        let pre_release = must_release_before_reopening(
            guard.handle.as_ref().map(|h| h.exclusive),
            entering_exclusive,
        );
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
            if let Some(old) = guard.handle.take() {
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
                        self.publish_output_lost_if_gone(&guard.handle);
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
            if let Some(old) = guard.handle.take() {
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
            self.publish_output_lost_if_gone(&guard.handle);
            return Err(err);
        }

        guard.handle = Some(handle);
        // The pick becomes the pin, written under the same lock as the
        // handle it belongs to (#630). This is what lets the engine still
        // name the user's device after an open that fails and leaves no
        // handle behind: the picker keeps flagging its pinned row, a forced
        // reopen targets that device instead of the OS default, and MPD
        // stops answering "no device".
        //
        // Only on a switch that actually happened. `switch_error` is set
        // when the requested device refused to open and the previous one
        // was reopened in its place: the stream, and the persisted setting
        // the command leaves alone on `Err`, both still say the old device,
        // so recording the refused one would make `pinned_device()` name a
        // device nothing is using and send the next forced reopen straight
        // back at it.
        if switch_error.is_none() {
            guard.pinned = pinned_pick;
        }
        self.publish_output_mode(None, false, entering_exclusive, guard.handle.as_ref());

        // Re-read the decision now rather than trust the one taken
        // before the open. An exclusive open costs tens to hundreds of
        // milliseconds, and a Pause the user hits during it reaches the
        // decoder first: resuming on the strength of a stale `Play`
        // would start a track they had just stopped. The `Stop` above
        // does not clear either input — it unloads the track without
        // touching the state or `paused_output` — so this reads what the
        // user last asked for.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Step 6 — put back whatever the decoder is on. Not necessarily
        // the track this method snapshotted: a pick made during the open
        // is what the decoder accepted, and resuming the older snapshot
        // instead is what left nothing playing at all (#634). A session
        // the user had paused is parked instead, so a device pick never
        // starts music (#611).
        match resume {
            RebuildResume::Play => self.resume_after_rebuild(live),
            RebuildResume::StayPaused => self.park_session(live, track_id),
            RebuildResume::Nothing => {}
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
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;
        // Resolved under the very lock this method rebuilds with (#629).
        // Reading it before taking the lock left a window in which
        // `set_output_device` could install *and persist* device B; this
        // toggle then rebuilt on A, the name it had captured, and the
        // stream ended up contradicting the saved preference until
        // something else rebuilt. Same cure as `reopen_output_device`
        // took in #628: let the rebuild resolve its own target.
        let active = guard.pinned_device();
        // What this rebuild owes the session, by the same rule the
        // device-error path uses (#611): a session the user had paused
        // is parked, not started. Picking a device or flipping the mode
        // is a deliberate act on the *output*, never a request to play —
        // and this path resumed a paused track into audible playback.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Both a session that was playing and one the user paused hold a
        // track the rebuild has to interrupt; only what follows differs.
        let was_playing = resume != RebuildResume::Nothing;
        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        // Read here, before the stop: opening the replacement writes its
        // own sample rate into the shared block, and a position derived
        // from the old rate's sample count against the new one is simply a
        // wrong number. What it is the position *of* is stamped onto it,
        // because the load to resume can only be read after the swap
        // (#634) — an exclusive open costs tens to hundreds of
        // milliseconds, and a track the user picks during it is what the
        // decoder will be holding by then.
        let live = self.live_position();

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
            must_release_before_reopening(guard.handle.as_ref().map(|h| h.exclusive), enabled);
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
            if let Some(old) = guard.handle.take() {
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
                if guard.handle.is_none() {
                    // `pre_release` already released the old exclusive
                    // handle, so there is no output thread at all. Tell
                    // Settings the flag is stale (#405), then schedule a
                    // rebuild that re-opens in the mode now recorded in
                    // `exclusive_output`. Pass `active` explicitly: the
                    // teardown emptied `self.output`, so a self-resolve
                    // would reopen the OS default instead of the user's
                    // device.
                    self.publish_output_lost_if_gone(&guard.handle);
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
            if let Some(old) = guard.handle.take() {
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
            self.publish_output_lost_if_gone(&guard.handle);
            return Err(err);
        }
        guard.handle = Some(handle);
        // The event is redundant with the caller's own re-read after a
        // manual toggle (ExclusiveModeCard.tsx), and kept for every other
        // caller. The notice is not redundant: a toggle that lands in
        // shared mode is precisely the silent degradation #597 is about.
        self.publish_output_mode(None, false, enabled, guard.handle.as_ref());

        // Re-read the decision now rather than trust the one taken
        // before the open. An exclusive open costs tens to hundreds of
        // milliseconds, and a Pause the user hits during it reaches the
        // decoder first: resuming on the strength of a stale `Play`
        // would start a track they had just stopped. The `Stop` above
        // does not clear either input — it unloads the track without
        // touching the state or `paused_output` — so this reads what the
        // user last asked for.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // The mode flip must not drop the user off what they are
        // listening to — radio included, which is why this re-dispatches
        // the decoder's own load rather than looking a `track` row up by
        // an id a stream does not have. And it must not start what they
        // had paused either: that one is parked (#611).
        match resume {
            RebuildResume::Play => self.resume_after_rebuild(live),
            RebuildResume::StayPaused => self.park_session(live, track_id),
            RebuildResume::Nothing => {}
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
    /// The OS default, but only while nothing is pinned — the
    /// default-device follow (#627). The condition is re-checked inside
    /// the rebuild's own lock, so a pin installed in the meantime wins
    /// and no rebuild happens at all.
    OsDefaultIfUnpinned,
}

/// What a default-device follow did, so its caller knows whether the
/// change was actually dealt with (#627).
pub(super) enum FollowOutcome {
    /// Dealt with: a rebuild ran, or the checks said none was needed.
    Settled,
    /// Not dealt with — the rebuild gate was busy. Nothing was changed,
    /// and the caller has to come back, or the system default and the
    /// open device stay apart until the next notification, which may
    /// never come: moving the default twice in quick succession is one
    /// notification per change, not a repeating signal.
    Deferred,
}

/// How a command leaves the pending-pause flag (#611).
///
/// A `Pause` raises it, and everything that asks for playback clears it:
/// `Resume`, and any load, because a load is a request to play — which
/// is also when the decoder clears its own `paused_output`. Every other
/// command leaves it alone; a `Stop` in particular, since the rebuilds
/// send one and it decides nothing about whether the user wants to hear
/// anything afterwards.
///
/// Pure so the lifecycle can be exercised without an engine.
fn pause_pending_after(cmd: &AudioCmd, current: bool) -> bool {
    match cmd {
        AudioCmd::Pause => true,
        AudioCmd::Resume
        | AudioCmd::LoadAndPlay { .. }
        | AudioCmd::LoadRemoteFileAndPlay { .. }
        | AudioCmd::LoadUrlAndPlay { .. } => false,
        _ => current,
    }
}

/// Whether a default-device change is worth a rebuild.
///
/// Pure so the decision can be tested without a sound card;
/// [`AudioEngine::follow_os_default_output`] adds the pin check, which
/// needs the engine's lock.
///
/// - a default the host cannot name (`None`) is nothing to follow:
///   tearing down a working stream to open "no device" would lose the
///   audio we still have;
/// - an output the backend cannot name (`None`) cannot be compared, and
///   "cannot tell" must not mean "do nothing" in the one feature whose
///   whole job is to move — CoreAudio hog mode with no pin lands here;
/// - otherwise the names decide.
///
/// The two names do not always come from the same call: the shared cpal
/// path fills `opened_device` through the same `device_display_name` the
/// default lookup uses, while WASAPI exclusive names itself through
/// `Device::get_friendlyname()`. Both read Windows' friendly name, so
/// they normally agree — and if they ever didn't, the cost is a
/// redundant rebuild on a default change that really happened, never a
/// missed one. Which is why this compares exactly and normalises
/// nothing: a smarter match could only hide a real change.
fn should_follow_default(opened: Option<&str>, new_default: Option<&str>) -> bool {
    match (opened, new_default) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(opened), Some(new_default)) => opened != new_default,
    }
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
/// **`Loading` counts as a session**, and the reason is not about the
/// resume at all — it is about the `Stop`. The decoder is inside
/// `play_track` from the moment it accepts a load, and a `SwapProducer`
/// that reaches it there is dropped on the floor: both drains fall
/// through to a catch-all, on the stated assumption that the engine
/// always sends a `Stop` first. Answering `Nothing` for a track that was
/// still loading broke that assumption, so the swap was lost and the
/// decoder kept writing into a ring whose consumer had just been torn
/// down — silence until something else rebuilt the output. Resuming is
/// then the right answer too: the load is in `last_load` and goes back
/// out under its own intent.
///
/// `Ended` and `Idle` stay outside: there is no session to interrupt,
/// the decoder is parked at the top-level loop where a `SwapProducer` is
/// handled properly, and resuming would restart a track that had
/// finished.
///
/// Until #617 only a *library* track could stay paused, because what made
/// it resumable afterwards was the persisted resume point `resume_last`
/// loads — and that point is always a library track's, so a radio station
/// or a server track parked the same way came back as the last library
/// track instead. Both are now held by
/// [`AudioEngine::park_session`](AudioEngine::park_session) itself, so a
/// session the user paused stays paused whatever it is playing.
fn rebuild_resume(state: super::state::PlayerState, paused_output: bool) -> RebuildResume {
    use super::state::PlayerState;
    match state {
        PlayerState::Playing | PlayerState::Paused | PlayerState::Loading if paused_output => {
            RebuildResume::StayPaused
        }
        PlayerState::Playing | PlayerState::Paused | PlayerState::Loading => RebuildResume::Play,
        PlayerState::Idle | PlayerState::Ended => RebuildResume::Nothing,
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
    use super::{
        endpoint_moved, pause_pending_after, rebuild_resume, AudioCmd, LoadIntent, RebuildResume,
        TrackGain,
    };

    #[test]
    fn a_session_that_was_playing_picks_back_up() {
        // By the time the rebuild runs, `notify_device_lost` has already
        // parked a playing session as `Paused` without raising
        // `paused_output`.
        assert_eq!(
            rebuild_resume(PlayerState::Paused, false),
            RebuildResume::Play
        );
        assert_eq!(
            rebuild_resume(PlayerState::Playing, false),
            RebuildResume::Play
        );
    }

    #[test]
    fn a_session_the_user_paused_stays_paused() {
        // #611: resuming this one started the music on whatever device the
        // system fell back to.
        assert_eq!(
            rebuild_resume(PlayerState::Paused, true),
            RebuildResume::StayPaused
        );
    }

    #[test]
    fn a_paused_radio_session_stays_paused_too() {
        // It did not, until #617: parking it left Play to the persisted
        // resume point, which is always a library track's, so the station
        // came back as the last local track. The park now holds the load
        // itself, so what the user paused is what Play picks up — and a
        // device flap no longer restarts a stream they had stopped.
        assert_eq!(
            rebuild_resume(PlayerState::Playing, true),
            RebuildResume::StayPaused
        );
    }

    #[test]
    fn nothing_loaded_means_nothing_to_resume() {
        // `Loading` is deliberately not here — see below.
        for state in [PlayerState::Idle, PlayerState::Ended] {
            assert_eq!(rebuild_resume(state, false), RebuildResume::Nothing);
        }
    }

    #[test]
    fn a_track_still_loading_is_still_a_session() {
        // Not about the resume: about the `Stop`. The decoder is inside
        // `play_track` from the moment it accepts a load, and a
        // `SwapProducer` that reaches it there is dropped — both drains
        // fall through to a catch-all, assuming the engine sent a `Stop`
        // first. Answering `Nothing` here broke that assumption, so the
        // rebuild swapped nothing and the decoder went on writing into a
        // ring whose consumer had just been torn down: silence until
        // something else rebuilt the output.
        assert_eq!(
            rebuild_resume(PlayerState::Loading, false),
            RebuildResume::Play
        );
        assert_eq!(
            rebuild_resume(PlayerState::Loading, true),
            RebuildResume::StayPaused
        );
    }

    #[test]
    fn a_pause_the_decoder_has_not_reached_yet_still_counts() {
        // `paused_output` is the decoder's answer and it lags: a rebuild
        // reading it in the gap decided `Play` for a session the user had
        // just paused, and its resume then cleared the flag and started
        // the music. The engine reads both, and this is the half it owns.
        assert!(pause_pending_after(&AudioCmd::Pause, false));
        assert!(pause_pending_after(&AudioCmd::Pause, true));
    }

    #[test]
    fn anything_that_asks_for_playback_clears_it() {
        assert!(!pause_pending_after(&AudioCmd::Resume, true));
        // A load is a request to play — the same moment the decoder
        // clears its own `paused_output`.
        let load = AudioCmd::LoadAndPlay {
            intent: LoadIntent::from_raw(1),
            path: std::path::PathBuf::from("/dev/null"),
            start_ms: 0,
            track_id: 42,
            duration_ms: 1000,
            source_type: "album".into(),
            source_id: None,
            replay_gain: TrackGain::default(),
        };
        assert!(!pause_pending_after(&load, true));
    }

    #[test]
    fn a_stop_decides_nothing_about_the_pause() {
        // The rebuilds send one, and it says nothing about whether the
        // user wants to hear anything afterwards. Clearing on it would
        // undo the pause this flag exists to carry.
        assert!(pause_pending_after(&AudioCmd::Stop, true));
        assert!(!pause_pending_after(&AudioCmd::Stop, false));
        assert!(pause_pending_after(&AudioCmd::Seek(1_000), true));
        assert!(pause_pending_after(&AudioCmd::SetVolume(0.5), true));
    }

    #[test]
    fn coming_back_on_the_same_endpoint_is_not_a_move() {
        // A device that flaps and comes back: the automatic recovery #175
        // exists for, and nothing #617 should pause.
        assert!(!endpoint_moved(Some("USB DAC"), Some("USB DAC")));
    }

    #[test]
    fn landing_on_another_endpoint_is_a_move() {
        // The reported case: headphones off, Windows falls back to the
        // built-in speakers.
        assert!(endpoint_moved(Some("Headphones"), Some("Speakers")));
    }

    #[test]
    fn an_endpoint_nobody_can_name_is_never_a_move() {
        // Hog mode can name nothing at all, and the two sides do not
        // always come from the same backend accessor. A wrong "moved"
        // stops the music for a user who asked for none of this; a wrong
        // "not moved" only costs the pause, leaving the behaviour that
        // shipped before #617.
        assert!(!endpoint_moved(None, Some("Speakers")));
        assert!(!endpoint_moved(Some("Headphones"), None));
        assert!(!endpoint_moved(None, None));
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
mod last_load_tests {
    use super::*;
    use std::path::PathBuf;

    fn url_cmd(url: &str, track_id: i64, intent: u64) -> AudioCmd {
        AudioCmd::LoadUrlAndPlay {
            intent: LoadIntent::from_raw(intent),
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

    fn local_cmd(track_id: i64, intent: u64, start_ms: u64) -> AudioCmd {
        AudioCmd::LoadAndPlay {
            intent: LoadIntent::from_raw(intent),
            path: PathBuf::from("/dev/null"),
            start_ms,
            track_id,
            duration_ms: 1000,
            source_type: "album".into(),
            source_id: Some(7),
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
                ..Default::default()
            },
        }
    }

    fn live(intent: u64, track_id: i64, position_ms: u64) -> LivePosition {
        LivePosition {
            intent: Some(LoadIntent::from_raw(intent)),
            track_id,
            position_ms,
        }
    }

    #[test]
    fn a_url_load_is_captured_verbatim() {
        let snap = LastLoad::capture(&url_cmd("https://radio.invalid/live", -1, 1))
            .expect("a load is captured");
        assert_eq!(snap.track_id, -1);
        assert_eq!(snap.title.as_deref(), Some("Test stream"));
        match snap.source {
            LastLoadSource::Url { url, ext_hint, .. } => {
                assert_eq!(url, "https://radio.invalid/live");
                assert_eq!(ext_hint.as_deref(), Some("mp3"));
            }
            _ => panic!("expected a URL load"),
        }
    }

    #[test]
    fn a_library_track_is_captured_too() {
        // It was not, before #634: the snapshot existed only to resume
        // radio, and a local `LoadAndPlay` wiped it. The rebuild then went
        // to the database for the track it had noted the id of — which is
        // exactly the older snapshot that left nothing playing.
        let snap = LastLoad::capture(&local_cmd(42, 3, 0)).expect("a load is captured");
        assert_eq!(snap.track_id, 42);
        assert!(matches!(
            snap.source,
            LastLoadSource::Local { ref path, .. } if path == &PathBuf::from("/dev/null")
        ));
    }

    #[test]
    fn nothing_but_a_load_is_captured() {
        // `Stop` above all: it is what a rebuild sends just before asking
        // for this snapshot back, and clearing on it would leave every
        // rebuild with nothing to resume.
        for cmd in [
            AudioCmd::Pause,
            AudioCmd::Resume,
            AudioCmd::Stop,
            AudioCmd::Seek(123),
            AudioCmd::SetVolume(0.5),
            AudioCmd::SetMono(true),
        ] {
            assert!(
                LastLoad::capture(&cmd).is_none(),
                "{cmd:?} is not a load and must leave the snapshot alone"
            );
        }
    }

    #[test]
    fn the_resume_carries_the_load_s_own_intent() {
        // The heart of #634. A fresh intent would rank this resume above a
        // pick that claimed before it and has not reached the channel yet;
        // the original is accepted again — the decoder drops what is
        // *older* than the newest it has been handed — and gives way to a
        // genuinely newer one.
        let snap = LastLoad::capture(&local_cmd(42, 9, 0)).expect("a load is captured");
        let intent = snap.intent;
        assert_eq!(intent, LoadIntent::from_raw(9));
        assert_eq!(
            snap.into_command(intent, 1_000).load_intent(),
            Some(LoadIntent::from_raw(9))
        );
    }

    #[test]
    fn a_resumed_library_track_keeps_the_source_it_came_from() {
        // Stamping "device-rebuild" on it, as the three paths used to,
        // hides an album play from every statistic that filters on the
        // source — for the sole reason that the audio device changed.
        let snap = LastLoad::capture(&local_cmd(42, 1, 0)).expect("a load is captured");
        let intent = snap.intent;
        match snap.into_command(intent, 2_500) {
            AudioCmd::LoadAndPlay {
                source_type,
                source_id,
                start_ms,
                track_id,
                ..
            } => {
                assert_eq!(source_type, "album");
                assert_eq!(source_id, Some(7));
                assert_eq!(start_ms, 2_500);
                assert_eq!(track_id, 42);
            }
            _ => panic!("a library track resumes as LoadAndPlay"),
        }
    }

    #[test]
    fn online_device_change_reemits_remote_local_command_with_fallback() {
        let snap = LastLoad::capture(&remote_local_cmd(Some("https://server.invalid/stream"), -3))
            .expect("a load is captured");
        let intent = snap.intent;
        match snap.into_command(intent, 4_321) {
            AudioCmd::LoadRemoteFileAndPlay {
                path,
                start_ms,
                track_id,
                duration_ms,
                fallback_url,
                replay_gain,
                discard_on_failure,
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
                assert!(
                    !discard_on_failure,
                    "a resume never authorises deleting the file it re-opens"
                );
            }
            _ => panic!("device change must resume the reconciled local file"),
        }
    }

    #[test]
    fn offline_device_change_reemits_remote_local_command_without_fallback() {
        let snap = LastLoad::capture(&remote_local_cmd(None, -3)).expect("a load is captured");
        let intent = snap.intent;
        match snap.into_command(intent, 7_654) {
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
    fn the_position_is_kept_for_the_load_it_belongs_to() {
        let snap = LastLoad::capture(&local_cmd(42, 5, 0)).expect("a load is captured");
        assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 90_000);
    }

    #[test]
    fn a_track_picked_during_the_rebuild_starts_at_its_own_beginning() {
        // The position was read while track 42 was playing; what the
        // decoder accepted since is track 77, at 0. Handing it 42's
        // position would drop the user into the middle of a track they
        // just picked — or past its end.
        let snap = LastLoad::capture(&local_cmd(77, 6, 0)).expect("a load is captured");
        assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 0);
    }

    #[test]
    fn re_picking_the_same_track_restarts_it() {
        // Same id, different intent: the user asked for the track again,
        // and resuming at the old position would quietly ignore that.
        let snap = LastLoad::capture(&local_cmd(42, 6, 0)).expect("a load is captured");
        assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 0);
    }

    #[test]
    fn a_load_accepted_but_not_yet_started_uses_its_own_start() {
        // The window between "the decoder accepted this load" and "the
        // decoder started it": the intent already matches, the track id
        // does not yet, and the position still belongs to the previous
        // track. Both halves have to agree before the position is trusted.
        let snap = LastLoad::capture(&local_cmd(77, 5, 30_000)).expect("a load is captured");
        assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 30_000);
    }

    #[test]
    fn a_decoder_with_no_load_on_record_pairs_with_nothing() {
        let snap = LastLoad::capture(&local_cmd(42, 5, 12_000)).expect("a load is captured");
        let live = LivePosition {
            intent: None,
            track_id: 42,
            position_ms: 90_000,
        };
        assert_eq!(resume_start_ms(&snap, live), 12_000);
    }
}

#[cfg(test)]
mod output_slot_tests {
    use super::OutputSlot;

    // Only the handle-less branch is exercised here: an `OutputHandle`
    // owns a live output thread, so it cannot be built in a unit test.
    // That branch is the one that regressed, though — the other reads the
    // handle the rebuilds keep in step with the stream.

    #[test]
    fn the_pin_survives_a_missing_handle() {
        // #630: with no stream installed — a spawn that failed after the
        // old one was released — the engine must still name the device the
        // user picked. Answering "nothing pinned" sent a forced reopen to
        // the OS default, cost the picker the pinned row it had just
        // failed to open, and made MPD report no device at all.
        let slot = OutputSlot {
            handle: None,
            pinned: Some("Speakers (USB DAC)".to_string()),
        };
        assert_eq!(slot.pinned_device().as_deref(), Some("Speakers (USB DAC)"));
    }

    #[test]
    fn no_handle_and_no_pin_means_the_os_default() {
        let slot = OutputSlot {
            handle: None,
            pinned: None,
        };
        assert!(slot.pinned_device().is_none());
    }
}

#[cfg(test)]
mod output_mode_tests {
    use super::{output_mode_of, OutputMode};

    #[test]
    fn nothing_asked_for_is_shared() {
        assert_eq!(output_mode_of(false, false, false), OutputMode::Shared);
    }

    #[test]
    fn asked_and_granted_is_exclusive() {
        assert_eq!(output_mode_of(true, true, false), OutputMode::Exclusive);
    }

    #[test]
    fn asked_and_refused_is_its_own_state() {
        // The one #597 exists for: playback is fine, in shared mode, and
        // until now the only trace of the user's choice not being honoured
        // was a `tracing::warn!` line.
        assert_eq!(
            output_mode_of(true, false, false),
            OutputMode::ExclusiveRefused
        );
    }

    #[test]
    fn dop_outranks_the_rest() {
        // On Linux and macOS the DoP toggle engages the exclusive path by
        // itself, so the exclusive preference can be off while the DAC is
        // being handed native DSD. Reporting "shared" there would be a lie
        // about the most demanding mode we have.
        assert_eq!(output_mode_of(false, true, true), OutputMode::Dop);
        assert_eq!(output_mode_of(true, true, true), OutputMode::Dop);
    }
}

#[cfg(test)]
mod default_follow_tests {
    use super::should_follow_default;

    #[test]
    fn a_new_default_is_followed() {
        assert!(should_follow_default(Some("Speakers"), Some("USB DAC")));
    }

    #[test]
    fn the_same_default_is_not_reopened() {
        // The notification fires for changes that leave the endpoint we
        // are on as the default — and a rebuild is an audible gap, so
        // "nothing to do" has to stay nothing.
        assert!(!should_follow_default(Some("USB DAC"), Some("USB DAC")));
    }

    #[test]
    fn an_unnamed_output_is_followed() {
        // CoreAudio hog mode with no pin reports no opened name (#612:
        // inventing one was the original defect). Unknown must not mean
        // "stay put" here.
        assert!(should_follow_default(None, Some("USB DAC")));
    }

    #[test]
    fn a_vanished_default_is_not_followed() {
        // Windows sends the notification with no device when the last
        // endpoint goes away. Rebuilding onto nothing would drop audio
        // that is still playing; a device we really lost arrives as
        // `DeviceNotAvailable` instead, which owns its own recovery.
        assert!(!should_follow_default(Some("USB DAC"), None));
        assert!(!should_follow_default(None, None));
    }

    #[test]
    fn the_comparison_is_exact() {
        // Both names come from the same accessor, so they are the same
        // string for the same endpoint — no normalisation to hide a
        // genuine change behind.
        assert!(should_follow_default(Some("USB DAC"), Some("USB  DAC")));
    }
}
