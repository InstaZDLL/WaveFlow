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
use tauri::AppHandle;
use tokio::sync::mpsc::unbounded_channel;

use crate::error::{AppError, AppResult};

use super::analytics::{analytics_task, AnalyticsMsg};
use super::decoder::spawn_decoder_thread;
use super::output::{spawn_output_thread, OutputHandle};
use super::state::SharedPlayback;

/// Commands accepted by the decoder thread.
#[derive(Debug)]
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
    Shutdown,
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
        let (cmd_tx, cmd_rx) = unbounded::<AudioCmd>();
        let shared = Arc::new(SharedPlayback::new());

        // Analytics channel: decoder pushes `AnalyticsMsg`s at EOF, the
        // tokio `analytics_task` consumes them to write `play_event`
        // rows and self-send the next `LoadAndPlay`.
        let (analytics_tx, analytics_rx) = unbounded_channel::<AnalyticsMsg>();

        let (output, decoder) = match spawn_output_thread(shared.clone(), app.clone()) {
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
}
