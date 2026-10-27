//! Audio engine handle — the single `Arc<AudioEngine>` managed by Tauri.
//!
//! At this checkpoint the engine is a no-op: it holds the shared state and
//! a command channel but the decoder thread and cpal output are stubbed.
//! Subsequent checkpoints flesh out the output stream (checkpoint 2),
//! decoder loop (checkpoint 4) and command wiring (checkpoint 9).

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use tauri::AppHandle;
use tokio::sync::mpsc::unbounded_channel;

use crate::error::{AppError, AppResult};

use super::analytics::{analytics_task, AnalyticsMsg};
use super::decoder::spawn_decoder_thread;
use super::output::{spawn_output_with_mode, OutputHandle};
use super::state::SharedPlayback;

mod command;
mod notice;
mod output_switch;
mod rebuild;
mod resume;

pub use command::{AudioCmd, LoadIntent};
pub use notice::{ExclusiveRefusal, OutputMode};
pub(super) use rebuild::{FollowOutcome, REBUILD_SETTLE_WINDOW};
pub use resume::{DetachedOrigin, LastLoad};

use notice::PlaybackNotice;
use rebuild::{pause_pending_after, FlapWindow, RebuildGate};
use resume::ParkedSession;

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
    /// The exit path's way to the analytics task: see
    /// [`AudioEngine::shut_down_and_flush`].
    analytics_tx: tokio::sync::mpsc::UnboundedSender<AnalyticsMsg>,
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
                    analytics_tx.clone(),
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
            analytics_tx,
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
            // behaviour. Seeded properly in `startup/setup.rs` once the profile
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

    /// Stop the decoder and wait, at most `budget`, for the play it was in
    /// the middle of to reach the database.
    ///
    /// A play is credited when it ends, and quitting ends it: the decoder
    /// answers `Shutdown` with the same credit a skip gets (15 s or more),
    /// sent to the analytics task. The process used to exit with that
    /// message still in the channel, so the last thing listened to before
    /// quitting never reached the history or the stats. Waiting for the
    /// thread to finish proves the message is sent; the [`AnalyticsMsg::Flush`]
    /// queued behind it proves it was handled (written, or failed and
    /// logged by the analytics task).
    ///
    /// Bounded because a decoder stuck in a slow read must not hold the
    /// process open: past the budget the play is lost, as before.
    pub async fn shut_down_and_flush(&self, budget: Duration) {
        let deadline = tokio::time::Instant::now() + budget;
        // A window close may have sent it already, and then the channel is
        // closed: the thread is finishing or finished either way.
        let _ = self.cmd_tx.send(AudioCmd::Shutdown);
        let decoder = self
            .decoder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(decoder) = decoder {
            while !decoder.is_finished() {
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!("decoder still running at exit; last play not credited");
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        let done = Arc::new(tokio::sync::Notify::new());
        if self
            .analytics_tx
            .send(AnalyticsMsg::Flush(done.clone()))
            .is_err()
        {
            return;
        }
        if tokio::time::timeout_at(deadline, done.notified())
            .await
            .is_err()
        {
            tracing::warn!("analytics not drained at exit; last play may be lost");
        }
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
        let paused_after = pause_pending_after(&cmd, previous);
        self.pause_pending
            .store(paused_after, std::sync::atomic::Ordering::Release);
        // A live station that is reconnecting holds the decoder in a read
        // on an empty buffer; a command that replaces or stops it would
        // otherwise wait for the station to come back or give up. Released
        // after the send, so the command is waiting when the decoder sees
        // the stream end, and is served before that end is declared.
        let releases = matches!(
            cmd,
            AudioCmd::LoadAndPlay { .. }
                | AudioCmd::LoadRemoteFileAndPlay { .. }
                | AudioCmd::LoadUrlAndPlay { .. }
                | AudioCmd::Stop
                | AudioCmd::Shutdown
        );
        self.cmd_tx.send(cmd).map_err(|e| {
            // Nothing was queued, so the intent this recorded is one the
            // decoder will never see either.
            self.pause_pending
                .store(previous, std::sync::atomic::Ordering::Release);
            AppError::Audio(format!("audio command channel closed: {e}"))
        })?;
        if releases {
            super::live::release_starved_reader();
        }
        // A pause on a live station is time behind live, counted by its
        // buffer — the same lifecycle as the pending pause, so a load
        // counts as playing again.
        if paused_after != previous {
            super::live::note_paused(paused_after);
        }
        Ok(())
    }

    /// The `Stop` an output rebuild sends straight down the channel, past
    /// [`Self::send`]: a live station reconnecting holds the decoder in a
    /// read on an empty buffer, released once the `Stop` is queued so the
    /// rebuild does not wait for the station to come back.
    fn stop_for_rebuild(&self) -> Result<(), String> {
        self.cmd_tx
            .send(AudioCmd::Stop)
            .map_err(|e| e.to_string())?;
        super::live::release_starved_reader();
        Ok(())
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

    /// Play the output test tone.
    ///
    /// The tone needs the decoder, so a session in progress is parked
    /// first, the way a rebuild parks a paused one (#611): stopped where
    /// it was, shown as stopped, and picked back up there by Play. The
    /// position is read before the `Stop`, which resets it.
    pub fn play_test_tone(&self) -> AppResult<()> {
        use super::state::PlayerState;
        if matches!(
            self.shared.state(),
            PlayerState::Playing | PlayerState::Paused | PlayerState::Loading
        ) {
            let live = self.live_position();
            self.send(AudioCmd::Stop)?;
            self.park_session(live, live.track_id);
        }
        self.send(AudioCmd::PlayTestTone)
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
}

#[cfg(test)]
mod track_reopen_tests;

#[cfg(test)]
mod reopen_order_tests;

#[cfg(test)]
mod resume_guard_tests;

#[cfg(test)]
mod rebuild_resume_tests;

#[cfg(test)]
mod flap_window_tests;

#[cfg(test)]
mod rebuild_gate_tests;

#[cfg(test)]
mod last_load_tests;

#[cfg(test)]
mod output_slot_tests;

#[cfg(test)]
mod output_mode_tests;

#[cfg(test)]
mod default_follow_tests;
