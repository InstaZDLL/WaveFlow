//! Windows-only WASAPI Exclusive Mode output backend.
//!
//! Bit-perfect audiophile output: the application owns the device's
//! mix layer outright. The Windows audio engine doesn't mix our
//! samples with system sounds / other applications, doesn't apply its
//! own resampler, and doesn't insert any DSP between us and the DAC.
//!
//! Trade-offs vs the default cpal shared-mode backend:
//!
//! - Only one application can hold the device exclusive at a time.
//!   If Spotify (or any other player) already grabbed it, our init
//!   fails and the engine falls back to shared mode.
//! - System sounds (notifications, Discord) are silenced for the
//!   duration — by design.
//! - Some USB DACs ship buggy drivers that misbehave in
//!   event-driven exclusive mode; if init fails with the OS-reported
//!   error, we surface it through `player:error` so the user can
//!   toggle the feature off without restarting.
//!
//! Scope: we initialize exclusive at the device's **mix-format
//! sample rate** (i.e. the default rate Windows reports for the
//! device, typically the rate the audiophile picked in the Windows
//! Sound control panel). The decoder's existing rubato resampler
//! still converts every source track to that rate — so this is
//! "bypass the OS mixer" rather than "honor the source rate
//! exactly". Per-track sample-rate switching is a future phase.
//!
//! Same SPSC ring contract as the cpal backend (`Producer<f32>` →
//! `Consumer<f32>`), so the decoder thread doesn't know which
//! backend is driving the device.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;
use wasapi::{
    get_default_device, AudioClient, AudioRenderClient, Device, DeviceCollection, Direction,
    Handle, SampleType, ShareMode, StreamMode, WaveFormat,
};

use super::output::{OutputHandle, RING_CAPACITY};
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

/// Spawn the WASAPI Exclusive output thread. Returns the
/// decoder-side `Producer<f32>` (hand it to the decoder) and an
/// [`OutputHandle`] the engine keeps around for teardown.
///
/// Mirrors `output::spawn_output_thread`'s signature so the engine can
/// pick a backend without leaking the type into every layer.
///
/// Init is synchronous: if the device can't be opened in exclusive
/// mode (busy, unsupported format, no such device), the error is
/// surfaced before this function returns so the caller can fall back
/// to the cpal shared backend.
pub fn spawn_exclusive_output_thread(
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
    let join: JoinHandle<()> = std::thread::Builder::new()
        .name("waveflow-wasapi-exclusive".into())
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
        .map_err(|e| AppError::Audio(format!("spawn wasapi exclusive thread: {e}")))?;

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
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "wasapi exclusive thread died before reporting init result".into(),
        )),
    }
}

/// Thread body. Owns the COM apartment + the wasapi handles.
fn output_thread_main(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<()>>,
    app: AppHandle,
    device_name: Option<String>,
) {
    // COM init for this thread. MTA is the right choice for an audio
    // worker that doesn't touch UI. Any HRESULT other than S_OK /
    // S_FALSE / RPC_E_CHANGED_MODE is a hard error.
    let hr = wasapi::initialize_mta();
    if let Err(err) = hr.ok() {
        let _ = init_tx.send(Err(AppError::Audio(format!(
            "CoInitializeEx(MTA) failed: {err:?}"
        ))));
        return;
    }

    let session = match open_exclusive_session(&device_name, &shared) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(?err, "wasapi exclusive init failed");
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    // Signal a successful init back to the caller so the engine can
    // proceed to spawn the decoder thread.
    let _ = init_tx.send(Ok(()));

    tracing::info!(
        sample_rate = session.sample_rate,
        channels = session.channels,
        buffer_frames = session.buffer_frames,
        "wasapi exclusive stream opened"
    );

    run_event_loop(session, consumer, shutdown_rx, &shared, &app);

    tracing::debug!("wasapi exclusive output thread exiting");
}

/// Bundle of everything the event loop needs after a successful init.
/// Kept private so callers can't misuse the wasapi handles outside
/// the thread that owns the COM apartment.
struct ExclusiveSession {
    client: AudioClient,
    render: AudioRenderClient,
    event: Handle,
    sample_rate: u32,
    channels: u16,
    buffer_frames: u32,
}

/// Resolve the device, build a candidate WaveFormat, verify it's
/// supported in exclusive mode, then initialize the client. On any
/// fatal mismatch we return Err so the engine falls back to shared
/// mode rather than launching a broken stream.
fn open_exclusive_session(
    device_name: &Option<String>,
    shared: &Arc<SharedPlayback>,
) -> AppResult<ExclusiveSession> {
    // 1. Resolve device — explicit name first, OS default as fallback.
    let device = pick_device(device_name)?;

    // 2. Audio client.
    let mut client = device
        .get_iaudioclient()
        .map_err(|e| AppError::Audio(format!("get IAudioClient: {e:?}")))?;

    // 3. Build WaveFormat. We anchor on the device's mix format so
    //    the chosen sample rate matches what the user picked in the
    //    Windows Sound control panel. Exclusive mode rejects formats
    //    the device hardware can't run natively — `is_supported`
    //    tells us before we commit.
    let mix_format = client
        .get_mixformat()
        .map_err(|e| AppError::Audio(format!("get_mixformat: {e:?}")))?;
    let sample_rate = mix_format.get_samplespersec() as usize;
    let channels = mix_format.get_nchannels() as usize;

    // 32-bit float stereo (downmix happens upstream in the decoder
    // when the source is multichannel). Float is the most widely
    // supported exclusive-mode format on consumer DACs.
    let desired = WaveFormat::new(32, 32, &SampleType::Float, sample_rate, channels, None);

    // 4. Check the format is supported in exclusive mode. The wasapi
    //    crate returns `Ok(None)` here on success — Err on rejection.
    //    Surface the rejection so the engine falls back to shared.
    client
        .is_supported(&desired, &ShareMode::Exclusive)
        .map_err(|e| {
            AppError::Audio(format!(
                "device does not support 32-bit float in exclusive mode: {e:?}"
            ))
        })?;

    // 5. Pick the minimum supported device period for low latency.
    //    `get_device_period` returns (default, min) in 100 ns units.
    let (default_period, min_period) = client
        .get_device_period()
        .map_err(|e| AppError::Audio(format!("get_device_period: {e:?}")))?;
    let period_hns = min_period.max(default_period);
    let mode = StreamMode::EventsExclusive { period_hns };

    // 6. Initialize. `AUDCLNT_E_DEVICE_IN_USE` here means another
    //    application already owns the device exclusively — we surface
    //    the error so the engine can fall back gracefully.
    client
        .initialize_client(&desired, &Direction::Render, &mode)
        .map_err(|e| AppError::Audio(format!("initialize_client exclusive: {e:?}")))?;

    // 7. Register the event handle for the event-driven loop.
    let event = client
        .set_get_eventhandle()
        .map_err(|e| AppError::Audio(format!("set_get_eventhandle: {e:?}")))?;

    let buffer_frames = client
        .get_buffer_size()
        .map_err(|e| AppError::Audio(format!("get_buffer_size: {e:?}")))?;

    let render = client
        .get_audiorenderclient()
        .map_err(|e| AppError::Audio(format!("get_audiorenderclient: {e:?}")))?;

    // 8. Pre-fill the buffer with silence so the device starts cleanly
    //    — exclusive mode is strict about the first pass.
    let silent_bytes = vec![0u8; (buffer_frames as usize) * channels * std::mem::size_of::<f32>()];
    render
        .write_to_device(buffer_frames as usize, &silent_bytes, None)
        .map_err(|e| AppError::Audio(format!("prefill render buffer: {e:?}")))?;

    client
        .start_stream()
        .map_err(|e| AppError::Audio(format!("start_stream: {e:?}")))?;

    // 9. Stamp the chosen format into SharedPlayback so the decoder
    //    knows the target sample rate / channel count for its resampler.
    shared
        .sample_rate
        .store(sample_rate as u32, Ordering::Release);
    shared.channels.store(channels as u16, Ordering::Release);

    Ok(ExclusiveSession {
        client,
        render,
        event,
        sample_rate: sample_rate as u32,
        channels: channels as u16,
        buffer_frames,
    })
}

fn pick_device(device_name: &Option<String>) -> AppResult<Device> {
    if let Some(name) = device_name.as_deref().filter(|n| !n.is_empty()) {
        // Iterate the render collection until we find a friendlyname
        // match. The Linux ALSA story has the same pattern — name
        // strings are how we pin a specific endpoint across reboots.
        let coll = DeviceCollection::new(&Direction::Render)
            .map_err(|e| AppError::Audio(format!("enumerate render devices: {e:?}")))?;
        if let Ok(dev) = coll.get_device_with_name(name) {
            return Ok(dev);
        }
        tracing::warn!(
            requested = name,
            "wasapi exclusive: requested device not found, falling back to default"
        );
    }
    get_default_device(&Direction::Render)
        .map_err(|e| AppError::Audio(format!("default render device: {e:?}")))
}

/// Drain the SPSC ring into the wasapi render client on each buffer
/// event. The thread blocks on the OS event handle so it costs zero
/// CPU while idle.
///
/// Mirrors the cpal callback's logic for `paused_output`,
/// `drain_silent`, `volume`, `normalize_enabled`, and `mono_enabled`
/// so the user-facing behaviour is identical regardless of backend.
fn run_event_loop(
    session: ExclusiveSession,
    mut consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    shared: &Arc<SharedPlayback>,
    _app: &AppHandle,
) {
    let ExclusiveSession {
        client,
        render,
        event,
        channels,
        buffer_frames,
        ..
    } = session;

    let channels = channels as usize;
    let need_frames = buffer_frames as usize;
    let need_samples = need_frames * channels;
    let frame_bytes = channels * std::mem::size_of::<f32>();
    let buffer_bytes = need_frames * frame_bytes;

    // Two reusable scratch buffers: one for the f32 samples we apply
    // gain/mono/normalize to, one for the little-endian byte image
    // we hand to WASAPI. Pre-allocated so the hot path never touches
    // the allocator.
    let mut samples: Vec<f32> = vec![0.0; need_samples];
    let mut bytes_scratch: Vec<u8> = vec![0u8; buffer_bytes];
    let silent_buf = vec![0u8; buffer_bytes];

    // 2 s timeout is generous — for a 10 ms device period the event
    // should fire every 10 ms. A timeout means the device went away
    // (USB unplug, hibernate). We then exit so the engine can decide
    // whether to re-init or fall back.
    const EVENT_TIMEOUT_MS: u32 = 2000;

    loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }

        match event.wait_for_event(EVENT_TIMEOUT_MS) {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(?err, "wasapi exclusive wait_for_event failed");
                break;
            }
        }

        // Hard pause: write silence so the device underrun doesn't
        // click and the user-perceived pause is instant (instead of
        // waiting for the ~1 s pre-buffer to drain).
        let bytes: &[u8] = if shared.paused_output.load(Ordering::Acquire) {
            &silent_buf
        } else if shared.drain_silent.load(Ordering::Acquire) {
            // Drain-silent mode: drop whatever's queued and emit
            // silence. Pop the entire ring so the decoder's
            // spin-wait on a fresh `producer.slots() == RING_CAPACITY`
            // completes within one event period.
            while consumer.pop().is_ok() {}
            &silent_buf
        } else {
            let volume = shared.volume();
            let normalize = shared.normalize_enabled.load(Ordering::Relaxed);
            let mono = shared.mono_enabled.load(Ordering::Relaxed);
            let norm_gain: f32 = if normalize { 0.707 } else { 1.0 };

            if mono && channels >= 2 {
                // Mono downmix: average all channels per frame.
                let mut i = 0;
                while i + channels <= need_samples {
                    let mut sum = 0.0_f32;
                    let mut got = 0usize;
                    for slot in &mut samples[i..i + channels] {
                        match consumer.pop() {
                            Ok(s) => {
                                sum += s;
                                got += 1;
                                *slot = 0.0; // placeholder, overwritten below
                            }
                            Err(_) => *slot = 0.0,
                        }
                    }
                    let v = if got > 0 {
                        (sum / channels as f32) * volume * norm_gain
                    } else {
                        0.0
                    };
                    for slot in &mut samples[i..i + channels] {
                        *slot = v;
                    }
                    i += channels;
                }
            } else {
                // Normal multi-channel path.
                for slot in samples.iter_mut() {
                    *slot = match consumer.pop() {
                        Ok(s) => s * volume * norm_gain,
                        Err(_) => 0.0,
                    };
                }
            }

            // Pack f32 samples into the little-endian byte buffer
            // wasapi expects (KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, 32 bps).
            for (sample, chunk) in samples
                .iter()
                .zip(bytes_scratch.chunks_exact_mut(std::mem::size_of::<f32>()))
            {
                chunk.copy_from_slice(&sample.to_le_bytes());
            }
            &bytes_scratch
        };

        if let Err(err) = render.write_to_device(need_frames, bytes, None) {
            tracing::warn!(?err, "wasapi write_to_device failed");
            break;
        }

        // Quick non-blocking shutdown check after every buffer so a
        // user-initiated stop takes effect within one device period
        // (~10 ms) rather than waiting up to EVENT_TIMEOUT_MS.
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
    }

    // Stop the stream so the next exclusive opener doesn't fight us
    // for the device. Errors are non-fatal — we're tearing down anyway.
    let _ = client.stop_stream();
    let _ = event; // released with the function frame
                   // Wait briefly so a slow `stop_stream` settles before COM
                   // uninit; not strictly required, but tidier under tracing.
    std::thread::sleep(Duration::from_millis(5));
    wasapi::deinitialize();
}
