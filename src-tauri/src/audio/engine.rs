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

use crate::error::{AppError, AppResult};

use super::decoder::spawn_decoder_thread;
use super::output::{spawn_output_thread, OutputHandle};
use super::state::SharedPlayback;

/// Commands accepted by the decoder thread. Variants are added as the
/// feature set grows — at the stub stage we only need `Shutdown` so the
/// channel type is concrete.
#[derive(Debug)]
#[allow(dead_code)]
pub enum AudioCmd {
    LoadAndPlay {
        path: PathBuf,
        start_ms: u64,
        track_id: i64,
        duration_ms: u64,
    },
    Pause,
    Resume,
    Stop,
    Seek(u64),
    SetVolume(f32),
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
    pub fn new() -> Arc<Self> {
        let (cmd_tx, cmd_rx) = unbounded::<AudioCmd>();
        let shared = Arc::new(SharedPlayback::new());

        let (output, decoder) = match spawn_output_thread(shared.clone()) {
            Ok((producer, handle)) => {
                // `spawn_output_thread` returns only after the cpal
                // stream has opened, so `shared.sample_rate` /
                // `shared.channels` are already populated by the time
                // the decoder thread spawns.
                match spawn_decoder_thread(cmd_rx, producer, shared.clone()) {
                    Ok(join) => (Some(handle), Some(join)),
                    Err(err) => {
                        tracing::error!(?err, "failed to spawn decoder thread");
                        // Tear down the output so we don't leave an
                        // orphan cpal stream running.
                        handle.stop();
                        (None, None)
                    }
                }
            }
            Err(err) => {
                tracing::warn!(?err, "failed to open audio output at startup");
                // The decoder receiver is dropped with `cmd_rx` — any
                // future `send` on `cmd_tx` will fail, which bubbles up
                // as `AppError::Audio` through the command layer.
                (None, None)
            }
        };

        Arc::new(Self {
            cmd_tx,
            shared,
            output: Mutex::new(output),
            decoder: Mutex::new(decoder),
        })
    }

    /// Send a command to the decoder. Returns `AppError::Audio` if the
    /// channel is disconnected (decoder thread has exited).
    #[allow(dead_code)]
    pub fn send(&self, cmd: AudioCmd) -> AppResult<()> {
        self.cmd_tx
            .send(cmd)
            .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))
    }

    /// Borrow the shared atomic state — used by commands that need to read
    /// current position / volume / state without hitting the decoder.
    #[allow(dead_code)]
    pub fn shared(&self) -> &Arc<SharedPlayback> {
        &self.shared
    }
}
