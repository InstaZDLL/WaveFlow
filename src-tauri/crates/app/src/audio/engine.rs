//! Audio engine handle — the single `Arc<AudioEngine>` managed by Tauri.
//!
//! At this checkpoint the engine is a no-op: it holds the shared state and
//! a command channel but the decoder thread and cpal output are stubbed.
//! Subsequent checkpoints flesh out the output stream (checkpoint 2),
//! decoder loop (checkpoint 4) and command wiring (checkpoint 9).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{unbounded, Sender};
use rtrb::Producer;
use tauri::AppHandle;
use tokio::sync::mpsc::unbounded_channel;

use crate::error::{AppError, AppResult};

use super::analytics::{analytics_task, AnalyticsMsg};
use super::decoder::spawn_decoder_thread;
use super::output::{spawn_output_with_mode, OutputHandle};
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
        /// ReplayGain in dB for this track if analysis has computed it.
        /// `None` means "no gain known" (track never analyzed) — the
        /// decoder leaves the signal untouched even when the toggle is
        /// on. Lookup is done at command time so the decoder thread
        /// stays out of the SQLite path.
        replay_gain_db: Option<f64>,
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
        replay_gain_db: Option<f64>,
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
            AudioCmd::SwapProducer(_) => write!(f, "SwapProducer(<producer>)"),
            AudioCmd::Shutdown => write!(f, "Shutdown"),
        }
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
        })
    }

    /// Send a command to the decoder. Returns `AppError::Audio` if the
    /// channel is disconnected (decoder thread has exited).
    pub fn send(&self, cmd: AudioCmd) -> AppResult<()> {
        self.cmd_tx
            .send(cmd)
            .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))
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
    pub fn try_rebuild_after_device_error(&self) -> AppResult<()> {
        use std::sync::atomic::Ordering;

        // Acquire the debounce slot. `swap(true)` returns the
        // previous value, so a `true` here means somebody else is
        // already rebuilding — bail without disturbing them.
        if self.rebuild_in_progress.swap(true, Ordering::AcqRel) {
            tracing::debug!(
                "device-error rebuild already in flight; skipping concurrent trigger"
            );
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

        let pinned = self.current_output_device();
        let exclusive = self.wasapi_exclusive.load(Ordering::Relaxed);

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

    /// Internal helper: rebuild the output stream against the given
    /// (device_name, exclusive) tuple, bypassing the same-device
    /// no-op check. Shared by [`Self::try_rebuild_after_device_error`]
    /// and (in the future) any other "rebuild without changing the
    /// user preference" path.
    fn force_rebuild_output(
        &self,
        device_name: Option<String>,
        exclusive: bool,
    ) -> AppResult<()> {
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

        let (producer, handle) =
            spawn_output_with_mode(self.shared.clone(), self.app.clone(), device_name, exclusive)?;

        if was_playing {
            self.cmd_tx
                .send(AudioCmd::Stop)
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
        }
        if let Some(old) = guard.take() {
            old.stop();
        }
        self.cmd_tx
            .send(AudioCmd::SwapProducer(producer))
            .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
        *guard = Some(handle);
        self.wasapi_exclusive_active.store(
            guard.as_ref().map(|h| h.wasapi_exclusive).unwrap_or(false),
            std::sync::atomic::Ordering::Release,
        );

        // Resume best-effort. Same async pattern as
        // `set_output_device` and `set_wasapi_exclusive` — pull the
        // track row off the synchronous path so a slow DB doesn't
        // hold the audio recovery up.
        if was_playing && track_id > 0 {
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
                        .fetch_optional(&pool)
                        .await
                        .ok()
                        .flatten();
                if let Some((file_path, duration_ms)) = row {
                    let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                        path: std::path::PathBuf::from(file_path),
                        start_ms: position_ms,
                        track_id,
                        duration_ms: duration_ms as u64,
                        source_type: "device-rebuild".into(),
                        source_id: None,
                        replay_gain_db: None,
                    });
                }
            });
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
            return Err(err);
        }

        *guard = Some(handle);
        self.wasapi_exclusive_active.store(
            guard.as_ref().map(|h| h.wasapi_exclusive).unwrap_or(false),
            std::sync::atomic::Ordering::Release,
        );

        // Step 6 — resume the previous track if we were playing one.
        if was_playing && track_id > 0 {
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
                        .fetch_optional(&pool)
                        .await
                        .ok()
                        .flatten();
                let Some((file_path, duration_ms)) = row else {
                    return;
                };
                let replay_gain_db =
                    crate::commands::player::fetch_replay_gain_db(&pool, track_id).await;
                let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                    path: std::path::PathBuf::from(file_path),
                    start_ms: position_ms,
                    track_id,
                    duration_ms: duration_ms.max(0) as u64,
                    source_type: "manual".into(),
                    source_id: None,
                    replay_gain_db,
                });
            });
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

        let (producer, handle) =
            spawn_output_with_mode(self.shared.clone(), self.app.clone(), active, enabled)?;
        let active_mode = handle.wasapi_exclusive;

        if was_playing {
            self.cmd_tx
                .send(AudioCmd::Stop)
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
        }
        if let Some(old) = guard.take() {
            old.stop();
        }
        self.cmd_tx
            .send(AudioCmd::SwapProducer(producer))
            .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
        *guard = Some(handle);
        self.wasapi_exclusive_active
            .store(active_mode, std::sync::atomic::Ordering::Release);

        if was_playing && track_id > 0 {
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
                        .fetch_optional(&pool)
                        .await
                        .ok()
                        .flatten();
                let Some((file_path, duration_ms)) = row else {
                    return;
                };
                let replay_gain_db =
                    crate::commands::player::fetch_replay_gain_db(&pool, track_id).await;
                let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                    path: std::path::PathBuf::from(file_path),
                    start_ms: position_ms,
                    track_id,
                    duration_ms: duration_ms.max(0) as u64,
                    source_type: "manual".into(),
                    source_id: None,
                    replay_gain_db,
                });
            });
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
