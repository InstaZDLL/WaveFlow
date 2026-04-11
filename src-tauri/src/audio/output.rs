//! cpal output stream, hosted on a dedicated thread.
//!
//! On Windows `cpal::Stream` is `!Send` (WASAPI / COM handles don't
//! cross thread boundaries). To keep the Stream alive for the engine's
//! lifetime without forcing it into Tauri's managed state (which demands
//! `Send + Sync`), we spawn an "output thread" that:
//!
//! 1. creates the cpal device + stream locally (so the `!Send` value
//!    never leaves its origin thread),
//! 2. calls `stream.play()`,
//! 3. parks on a shutdown channel until the engine tears down.
//!
//! The decoder-side `Producer<f32>` is `Send` and is handed back to the
//! caller along with the shutdown sender and the thread's join handle.
//!
//! The audio callback itself MUST NOT take locks, allocate, or block —
//! it only reads from `rtrb::Consumer` and mutates atomics in
//! [`SharedPlayback`].

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::error::{AppError, AppResult};

use super::state::{PlayerState, SharedPlayback};

/// Capacity of the SPSC sample ring, in f32 samples. At 48 kHz stereo
/// this is ~1 second of audio, which gives the decoder thread plenty of
/// headroom while keeping latency low.
pub const RING_CAPACITY: usize = 96_000;

/// Handle retained by the engine so it can tear the output thread down
/// cleanly on shutdown or device switch. Separate from the decoder-side
/// `Producer` which is handed off independently — see the tuple returned
/// from [`spawn_output_thread`].
pub struct OutputHandle {
    pub shutdown_tx: Sender<()>,
    pub join: JoinHandle<()>,
}

impl OutputHandle {
    /// Signal the output thread to drop its Stream and exit, then wait
    /// for it. Called from `AudioEngine::shutdown` and `Drop`.
    pub fn stop(self) {
        // Ignore the send error — the receiver may already be gone if
        // the stream errored out on its own.
        let _ = self.shutdown_tx.send(());
        let _ = self.join.join();
    }
}

/// Spawn the dedicated output thread. Returns the decoder-side
/// `Producer<f32>` (hand it to the decoder) and an [`OutputHandle`] the
/// engine keeps around for teardown.
///
/// The thread is named `waveflow-audio-output` so it's easy to spot in
/// profilers / `perf top`. Any error during Stream construction is
/// surfaced via an init-result channel before this function returns, so
/// the caller learns synchronously whether playback is usable.
///
/// Takes an [`AppHandle`] so the cpal error callback can emit
/// `player:error` + `player:state` events on device loss (headphones
/// unplugged mid-playback).
pub fn spawn_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<()>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let join = std::thread::Builder::new()
        .name("waveflow-audio-output".into())
        .spawn(move || {
            output_thread_main(thread_shared, consumer, shutdown_rx, init_tx, thread_app)
        })
        .map_err(|e| AppError::Audio(format!("spawn output thread: {e}")))?;

    // Block until the thread reports whether the Stream opened cleanly.
    // Any failure here means we never reached `stream.play()`.
    match init_rx.recv() {
        Ok(Ok(())) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
            },
        )),
        Ok(Err(err)) => {
            // The thread already exited; join it so we don't leak.
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "output thread died before reporting init result".into(),
        )),
    }
}

/// Thread body. Owns the `!Send` `cpal::Stream` locally, so nothing
/// crosses a thread boundary.
fn output_thread_main(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<()>>,
    app: AppHandle,
) {
    let stream = match build_stream(shared.clone(), consumer, app.clone()) {
        Ok(s) => s,
        Err(err) => {
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    if let Err(err) = stream.play().map_err(|e| AppError::Audio(format!("stream play: {e}"))) {
        let _ = init_tx.send(Err(err));
        return;
    }

    // Signal successful initialization.
    let _ = init_tx.send(Ok(()));

    // Park until the engine says shutdown. The Stream runs its callback
    // on its own (WASAPI-managed) thread on Windows, so we just need to
    // keep the Stream alive here.
    let _ = shutdown_rx.recv();
    drop(stream);
    tracing::debug!("audio output thread exiting");
}

/// Build the cpal `Stream`. Called only from inside the output thread.
fn build_stream(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    app: AppHandle,
) -> AppResult<Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AppError::Audio("no default audio output device".into()))?;

    let default_cfg = device
        .default_output_config()
        .map_err(|e| AppError::Audio(format!("default output config: {e}")))?;

    let sample_format = default_cfg.sample_format();
    let channels = default_cfg.channels();
    let sample_rate = default_cfg.sample_rate().0;
    let config: StreamConfig = default_cfg.into();

    // Stamp the device config into the shared state so the decoder and
    // position helper can compute timing without re-querying cpal.
    shared.sample_rate.store(sample_rate, Ordering::Release);
    shared.channels.store(channels, Ordering::Release);

    tracing::info!(
        sample_rate,
        channels,
        ?sample_format,
        "cpal output stream opened"
    );

    match sample_format {
        SampleFormat::F32 => open_stream::<f32>(&device, &config, consumer, shared, app),
        SampleFormat::I16 => open_stream::<i16>(&device, &config, consumer, shared, app),
        SampleFormat::U16 => open_stream::<u16>(&device, &config, consumer, shared, app),
        other => Err(AppError::Audio(format!(
            "unsupported sample format: {other:?}"
        ))),
    }
}

/// Generic stream builder parameterized by the device's native sample
/// format. We always decode into `f32` internally and let cpal convert
/// to whatever the device wants at the last second via `FromSample`.
fn open_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: Consumer<f32>,
    shared: Arc<SharedPlayback>,
    app: AppHandle,
) -> AppResult<Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static,
{
    // On device loss (headphones unplugged, sound server restart)
    // cpal fires this callback on a random thread. We flip state to
    // Paused, emit player:state + player:error so the UI can surface
    // the problem, and keep the Stream itself untouched — it's about
    // to be dropped by cpal anyway.
    let err_shared = shared.clone();
    let err_app = app.clone();
    let err_fn = move |err: cpal::StreamError| {
        tracing::warn!(?err, "cpal stream error");
        err_shared.set_state(PlayerState::Paused);
        let _ = err_app.emit(
            "player:state",
            json!({ "state": "paused", "track_id": null }),
        );
        let _ = err_app.emit(
            "player:error",
            json!({ "message": format!("audio device error: {err}") }),
        );
    };

    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [T], _info: &cpal::OutputCallbackInfo| {
                // Drain one sample per output slot. Buffer underrun is
                // normal when idle / paused / between tracks: we write
                // silence for those slots and let the consumer catch up.
                let mut written: u64 = 0;
                for slot in out.iter_mut() {
                    let sample = match consumer.pop() {
                        Ok(s) => {
                            written += 1;
                            s
                        }
                        Err(_) => 0.0,
                    };
                    *slot = T::from_sample(sample);
                }
                if written > 0 {
                    shared
                        .samples_played
                        .fetch_add(written, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| AppError::Audio(format!("build_output_stream: {e}")))?;

    Ok(stream)
}
