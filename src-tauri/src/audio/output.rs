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
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::error::{AppError, AppResult};

use super::state::{PlayerState, SharedPlayback};

/// Description of one available output device, returned to the
/// frontend by [`list_output_devices`]. The `id` field is the cpal
/// device name — there is no stable platform-independent ID, so we
/// match by name when the user picks one.
#[derive(Debug, Clone, Serialize)]
pub struct OutputDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Enumerate every output device available on the default audio host.
/// The OS default is flagged so the UI can highlight it.
///
/// On Linux we deliberately **avoid** `cpal::HostTrait::output_devices()`
/// here: cpal's ALSA backend calls `snd_pcm_open()` on every card to
/// build its iterator, which probes hardware (HDMI sinks, Bluetooth
/// profiles, …) and can take 1-2 seconds plus spam stderr with
/// `pcm_dmix` / `pcm_route` warnings. That's the source of the
/// 2-second freeze the user sees when opening the device menu —
/// webkit2gtk renders a black frame while waiting on the IPC. Instead
/// we read the ALSA hint database (`snd_device_name_hint("pcm")`),
/// which is just a config parse and finishes in a few ms with no
/// probing. cpal stays in charge of actually opening the device when
/// the user picks one — we just don't need it for the listing step.
pub fn list_output_devices() -> AppResult<Vec<OutputDeviceInfo>> {
    #[cfg(target_os = "linux")]
    {
        list_output_devices_alsa_hints()
    }
    #[cfg(not(target_os = "linux"))]
    {
        list_output_devices_cpal()
    }
}

/// Fallback enumeration via cpal — used on non-Linux platforms where
/// the host's enumeration is fast enough not to need a workaround.
#[cfg(not(target_os = "linux"))]
fn list_output_devices_cpal() -> AppResult<Vec<OutputDeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    let devices = host
        .output_devices()
        .map_err(|e| AppError::Audio(format!("enumerate output devices: {e}")))?;
    let mut out = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        let is_default = default_name.as_deref().is_some_and(|n| n == name);
        out.push(OutputDeviceInfo {
            id: name.clone(),
            name,
            is_default,
        });
    }
    Ok(out)
}

/// Linux-only fast enumeration via ALSA's hint API. Same data as
/// `aplay -L` exposes — config-level info, no PCM probing — so it
/// returns instantly even on systems with many HDMI cards.
///
/// We still need cpal for the *default device's name* so we can flag
/// the right row, but `default_output_device()` is a single-device
/// lookup that doesn't iterate.
#[cfg(target_os = "linux")]
fn list_output_devices_alsa_hints() -> AppResult<Vec<OutputDeviceInfo>> {
    use std::collections::HashSet;
    use std::ffi::CString;

    // cpal's `default_output_device` opens just the "default" alias
    // (one open, fast) and reports its resolved name. We use that to
    // mark which hint row should carry `is_default = true`. Wrapped
    // in `silence_alsa_stderr` to swallow any tangential probe noise.
    let default_name = silence_alsa_stderr(|| {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| d.name().ok())
    });

    let pcm = CString::new("pcm")
        .map_err(|e| AppError::Audio(format!("CString: {e}")))?;
    let iter = alsa::device_name::HintIter::new(None, pcm.as_c_str())
        .map_err(|e| AppError::Audio(format!("ALSA HintIter: {e}")))?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for hint in iter {
        // Filter to playback-capable devices. ALSA hints with no
        // `direction` field can be either, so we keep them.
        let direction_ok = matches!(
            hint.direction,
            None | Some(alsa::Direction::Playback)
        );
        if !direction_ok {
            continue;
        }
        let Some(name) = hint.name else { continue };
        // `null` is ALSA's bit bucket — useless to the user.
        if name == "null" {
            continue;
        }
        // ALSA reports the same hint multiple times in some configs
        // (once per profile). Dedupe by name.
        if !seen.insert(name.clone()) {
            continue;
        }
        let display = hint
            .desc
            .map(|d| d.replace('\n', ", "))
            .unwrap_or_else(|| name.clone());
        let is_default = default_name.as_deref().is_some_and(|d| d == name);
        out.push(OutputDeviceInfo {
            id: name,
            name: display,
            is_default,
        });
    }
    Ok(out)
}

/// Run the closure while ALSA library error messages are redirected
/// to /dev/null. On Linux, cpal's enumeration probes every PCM card
/// and ALSA helpfully prints `pcm_dmix` / `pcm_route` warnings for
/// cards that aren't currently usable (HDMI sinks with no monitor
/// attached, Bluetooth profiles in the wrong state, …). The warnings
/// are noise — failure to open during a probe is expected — so we
/// hide them while we're enumerating.
///
/// On non-Linux platforms this is a passthrough.
#[cfg(target_os = "linux")]
fn silence_alsa_stderr<R, F: FnOnce() -> R>(f: F) -> R {
    use std::os::unix::io::AsRawFd;

    // Open /dev/null + dup the current stderr (fd 2) so we can put it
    // back. If anything fails, just run `f` with stderr untouched —
    // we'd rather show the spam than skip enumeration.
    let dev_null = match std::fs::OpenOptions::new().write(true).open("/dev/null") {
        Ok(f) => f,
        Err(_) => return f(),
    };
    let saved = unsafe { libc::dup(2) };
    if saved < 0 {
        return f();
    }
    if unsafe { libc::dup2(dev_null.as_raw_fd(), 2) } < 0 {
        unsafe { libc::close(saved) };
        return f();
    }
    let result = f();
    // Restore stderr — best-effort. If `dup2` fails here we can't do
    // much, but the OS will reclaim the fd at process exit.
    unsafe {
        libc::dup2(saved, 2);
        libc::close(saved);
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn silence_alsa_stderr<R, F: FnOnce() -> R>(f: F) -> R {
    f()
}

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
    /// Resolved device name actually used by this output thread —
    /// `None` means the OS default device. Saved so a hot-swap can
    /// no-op when the user picks the same device again.
    pub device_name: Option<String>,
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
    device_name: Option<String>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<()>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join = std::thread::Builder::new()
        .name("waveflow-audio-output".into())
        .spawn(move || {
            output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
            )
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
                device_name,
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
    device_name: Option<String>,
) {
    let stream = match build_stream(shared.clone(), consumer, app.clone(), device_name) {
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
    device_name: Option<String>,
) -> AppResult<Stream> {
    silence_alsa_stderr(|| build_stream_inner(shared, consumer, app, device_name))
}

fn build_stream_inner(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    app: AppHandle,
    device_name: Option<String>,
) -> AppResult<Stream> {
    let host = cpal::default_host();
    // If a specific device was requested, look it up by name. If the
    // user picked a device that since vanished (USB DAC unplugged
    // between sessions), fall back to the OS default rather than
    // erroring out — the alternative is a silent app on next launch.
    let device = match device_name.as_deref() {
        Some(name) => {
            let mut found = None;
            if let Ok(iter) = host.output_devices() {
                for d in iter {
                    if d.name().ok().as_deref() == Some(name) {
                        found = Some(d);
                        break;
                    }
                }
            }
            match found {
                Some(d) => d,
                None => {
                    tracing::warn!(
                        device = %name,
                        "requested output device not found, falling back to default"
                    );
                    host.default_output_device().ok_or_else(|| {
                        AppError::Audio("no default audio output device".into())
                    })?
                }
            }
        }
        None => host
            .default_output_device()
            .ok_or_else(|| AppError::Audio("no default audio output device".into()))?,
    };

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
        if let Some(controls) =
            err_app.try_state::<crate::media_controls::MediaControlsHandle>()
        {
            controls.update_playback(PlayerState::Paused, err_shared.current_position_ms());
        }
    };

    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [T], _info: &cpal::OutputCallbackInfo| {
                // Hard pause: while paused, the decoder stops pushing
                // into the ring, and we stop draining it so the next
                // `resume` picks right back up where it left off. We
                // just write silence into the device buffer — users
                // hear the pause within ~WASAPI's internal latency
                // (~50-200 ms) instead of the full ring length.
                if shared.paused_output.load(Ordering::Acquire) {
                    for slot in out.iter_mut() {
                        *slot = T::from_sample(0.0_f32);
                    }
                    return;
                }

                // Drain-silent mode: drop whatever's left in the
                // ring as fast as possible and write silence. Used
                // during a track switch so the tail of the old
                // track never reaches the device. We still
                // `consumer.pop()` so `producer.slots()` reflects
                // the drop (the decoder's spin-wait needs this).
                if shared.drain_silent.load(Ordering::Acquire) {
                    for slot in out.iter_mut() {
                        let _ = consumer.pop();
                        *slot = T::from_sample(0.0_f32);
                    }
                    return;
                }

                // Read atomic flags once per buffer — cheap relaxed
                // loads that avoid ~5k redundant ops per callback.
                let volume = shared.volume();
                let normalize = shared.normalize_enabled.load(Ordering::Relaxed);
                let mono = shared.mono_enabled.load(Ordering::Relaxed);
                let channels = shared.channels.load(Ordering::Relaxed).max(1) as usize;
                // Normalization applies a −3 dB gain reduction (× 0.707)
                // to prevent clipping on loud source material.
                let norm_gain: f32 = if normalize { 0.707 } else { 1.0 };

                let mut written: u64 = 0;

                if mono && channels >= 2 {
                    // Mono downmix: read `channels` samples at a time,
                    // average them, and write the same value to every
                    // output channel. This loop processes one frame (all
                    // channels) per iteration. If the ring underruns
                    // mid-frame we still write silence for the remaining
                    // channels so the device buffer stays aligned.
                    for frame in out.chunks_mut(channels) {
                        let mut sum: f32 = 0.0;
                        let mut got: usize = 0;
                        for _ in 0..channels {
                            match consumer.pop() {
                                Ok(s) => {
                                    sum += s;
                                    got += 1;
                                }
                                Err(_) => {}
                            }
                        }
                        if got > 0 {
                            written += got as u64;
                            let mono_sample = (sum / channels as f32) * volume * norm_gain;
                            for slot in frame.iter_mut() {
                                *slot = T::from_sample(mono_sample);
                            }
                        } else {
                            for slot in frame.iter_mut() {
                                *slot = T::from_sample(0.0_f32);
                            }
                        }
                    }
                } else {
                    // Normal stereo/multi-channel path.
                    for slot in out.iter_mut() {
                        let sample = match consumer.pop() {
                            Ok(s) => {
                                written += 1;
                                s
                            }
                            Err(_) => 0.0,
                        };
                        *slot = T::from_sample(sample * volume * norm_gain);
                    }
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
