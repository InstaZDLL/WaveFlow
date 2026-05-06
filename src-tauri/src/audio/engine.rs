//! Audio engine handle — the single `Arc<AudioEngine>` managed by Tauri.
//!
//! At this checkpoint the engine is a no-op: it holds the shared state and
//! a command channel but the decoder thread and cpal output are stubbed.
//! Subsequent checkpoints flesh out the output stream (checkpoint 2),
//! decoder loop (checkpoint 4) and command wiring (checkpoint 9).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{unbounded, Sender};
use rtrb::Producer;
use tauri::AppHandle;
use tokio::sync::mpsc::unbounded_channel;

use crate::error::{AppError, AppResult};

use super::analytics::{analytics_task, AnalyticsMsg};
use super::decoder::spawn_decoder_thread;
use super::output::{spawn_output_thread, OutputHandle};
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
    shared: Arc<SharedPlayback>,
    output: Mutex<Option<OutputHandle>>,
    decoder: Mutex<Option<JoinHandle<()>>>,
    /// AppHandle clone so we can rebuild the cpal output thread from
    /// `set_output_device` without plumbing the handle through every
    /// Tauri command call site.
    app: AppHandle,
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
        Self::new_with_device(app, None)
    }

    /// Like [`Self::new`] but opens a specific output device. Used at
    /// startup once the persisted `audio.output_device` profile setting
    /// is known. `None` means "use the OS default".
    pub fn new_with_device(app: AppHandle, device_name: Option<String>) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = unbounded::<AudioCmd>();
        let shared = Arc::new(SharedPlayback::new());

        // Analytics channel: decoder pushes `AnalyticsMsg`s at EOF, the
        // tokio `analytics_task` consumes them to write `play_event`
        // rows and self-send the next `LoadAndPlay`.
        let (analytics_tx, analytics_rx) = unbounded_channel::<AnalyticsMsg>();

        let (output, decoder) =
            match spawn_output_thread(shared.clone(), app.clone(), device_name) {
                Ok((producer, handle)) => {
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
                        Ok(join) => (Some(handle), Some(join)),
                        Err(err) => {
                            tracing::error!(?err, "failed to spawn decoder thread");
                            handle.stop();
                            (None, None)
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to open audio output at startup");
                    (None, None)
                }
            };

        // Spawn the analytics task inside Tauri's runtime.
        tauri::async_runtime::spawn(analytics_task(
            analytics_rx,
            cmd_tx.clone(),
            app.clone(),
        ));

        Arc::new(Self {
            cmd_tx,
            shared,
            output: Mutex::new(output),
            decoder: Mutex::new(decoder),
            app,
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
        let (producer, handle) =
            spawn_output_thread(self.shared.clone(), self.app.clone(), device_name)?;

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
                self.cmd_tx.send(AudioCmd::Stop).map_err(|e| {
                    AppError::Audio(format!("audio command channel closed: {e}"))
                })?;
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
                let row: Option<(String, i64)> = sqlx::query_as(
                    "SELECT file_path, duration_ms FROM track WHERE id = ?",
                )
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
}
