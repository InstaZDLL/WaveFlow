//! macOS-only CoreAudio hog-mode DoP output backend (#495 / #497).
//!
//! The macOS equivalent of [`super::wasapi_exclusive`] / [`super::alsa_exclusive`]:
//! it takes **hog mode** on the output device (exclusive access — the
//! system stops mixing anything else into it), forces the device's
//! **physical stream format** to the exact DoP rate (`dsd_rate / 16`) in
//! 32-bit signed integer, and feeds an `AudioUnit` render callback with
//! DoP words MSB-justified in each 32-bit sample (marker in the top byte)
//! via [`super::dop_pack::fill_dop_period_i32`] — the same wire shape as
//! the Linux `S32_LE` path.
//!
//! Bit-perfect note: because we pin the device's *physical* rate to the
//! DoP rate, CoreAudio never resamples. The AudioUnit may still adjust
//! endianness between our native-LE client buffer and the device format,
//! but that conversion is **value-preserving** (it's samples, not raw
//! bytes), so the marker stays in the sample's most-significant byte and
//! the DoP stream survives intact.
//!
//! If hog mode is unavailable (another app holds the device) or the DAC
//! won't accept the DoP rate in 32-bit, the open fails and the engine
//! falls back to the ordinary DSD → PCM path.
//!
//! Same SPSC ring contract as the other backends (`Producer<f32>` →
//! `Consumer<f32>`, words carried as `f32` bit patterns).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::macos_helpers::{
    audio_unit_from_device_id_uninitialized, find_matching_physical_format, get_default_device_id,
    get_device_id_from_name, get_hogging_pid, set_device_physical_stream_format, toggle_hog_mode,
};
use coreaudio::audio_unit::render_callback::{data, Args};
use coreaudio::audio_unit::{Element, SampleFormat, Scope, StreamFormat};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;

use super::output::{DopFormat, OutputHandle, RING_CAPACITY};
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

/// CoreAudio device handle. `coreaudio-rs` takes/returns the
/// `objc2_core_audio::AudioDeviceID` type, which is a transparent alias
/// for `u32` (`AudioObjectID`), so we alias it locally rather than pull
/// in `objc2-core-audio` just for the name.
type AudioDeviceID = u32;

/// Spawn the CoreAudio DoP output thread. Mirrors the other exclusive
/// backends' contract: returns the decoder-side `Producer<f32>` and an
/// [`OutputHandle`], or an error (device busy / hog denied / format
/// unsupported) surfaced synchronously so the caller can fall back to
/// DSD → PCM.
pub fn spawn_coreaudio_dop_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
    dop: DopFormat,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<()>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join: JoinHandle<()> = std::thread::Builder::new()
        .name("waveflow-coreaudio-dop".into())
        .spawn(move || {
            output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
                dop,
            )
        })
        .map_err(|e| AppError::Audio(format!("spawn coreaudio dop thread: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(())) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                device_name,
                wasapi_exclusive: false,
                dop_rate: Some(dop.sample_rate),
            },
        )),
        Ok(Err(err)) => {
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "coreaudio dop thread died before reporting init result".into(),
        )),
    }
}

fn output_thread_main(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<()>>,
    _app: AppHandle,
    device_name: Option<String>,
    dop: DopFormat,
) {
    let device_id = match resolve_device(&device_name) {
        Some(id) => id,
        None => {
            let _ = init_tx.send(Err(AppError::Audio(
                "coreaudio: no output device found for DoP".into(),
            )));
            return;
        }
    };

    // Take exclusive (hog) access before touching the format, so the
    // physical-format change sticks and nothing else gets mixed in.
    if let Err(err) = acquire_hog(device_id) {
        let _ = init_tx.send(Err(err));
        return;
    }

    // Everything past hog acquisition must release it on the way out.
    let result = open_and_run(&shared, consumer, &shutdown_rx, &init_tx, device_id, dop);

    release_hog(device_id);

    match result {
        Ok(()) => tracing::debug!("coreaudio dop output thread exiting"),
        Err(err) => tracing::warn!(%err, "coreaudio dop output thread stopped on error"),
    }
}

/// The stateful body, factored out so `output_thread_main` can always run
/// [`release_hog`] afterwards regardless of how this returns. If it errors
/// before `init_tx` was signalled, it reports the failure so the spawn
/// call can fall back; after a successful start it parks until shutdown.
fn open_and_run(
    shared: &Arc<SharedPlayback>,
    mut consumer: Consumer<f32>,
    shutdown_rx: &Receiver<()>,
    init_tx: &Sender<AppResult<()>>,
    device_id: AudioDeviceID,
    dop: DopFormat,
) -> AppResult<()> {
    let channels = dop.channels as usize;

    // The DoP client format: interleaved 32-bit signed int at the exact
    // DoP rate. Marker rides MSB-justified inside each sample.
    let stream_format = StreamFormat {
        sample_rate: dop.sample_rate as f64,
        sample_format: SampleFormat::I32,
        flags: LinearPcmFlags::IS_SIGNED_INTEGER | LinearPcmFlags::IS_PACKED,
        channels: dop.channels as u32,
    };

    // Pin the device's physical format to a supported one matching the
    // DoP rate + 32-bit int — this is what prevents any resampling.
    let asbd = find_matching_physical_format(device_id, stream_format).ok_or_else(|| {
        AppError::Audio(format!(
            "coreaudio: DAC has no 32-bit physical format at {} Hz — can't do this DoP rate",
            dop.sample_rate
        ))
    })?;
    set_device_physical_stream_format(device_id, asbd)
        .map_err(|e| AppError::Audio(format!("coreaudio set physical format: {e}")))?;

    // Build the output AudioUnit bound to the device (uninitialized so we
    // can set the client format before init).
    let mut audio_unit = audio_unit_from_device_id_uninitialized(device_id, false)
        .map_err(|e| AppError::Audio(format!("coreaudio audio unit: {e}")))?;
    audio_unit
        .set_stream_format(stream_format, Scope::Input, Element::Output)
        .map_err(|e| AppError::Audio(format!("coreaudio set stream format: {e}")))?;

    // Render callback: bit-exact DoP, no DSP. Runs on CoreAudio's
    // realtime thread — only ring pops + atomic ops, no alloc/lock.
    let mut silence_phase: u64 = 0;
    let cb_shared = shared.clone();
    audio_unit
        .set_render_callback(move |args: Args<data::Interleaved<i32>>| {
            let Args {
                data: data::Interleaved { buffer, channels, .. },
                num_frames,
                ..
            } = args;
            let paused = cb_shared.paused_output.load(Ordering::Acquire);
            let draining = cb_shared.drain_silent.load(Ordering::Acquire);
            if paused || draining {
                if draining {
                    while consumer.pop().is_ok() {}
                }
                super::dop_pack::render_dop_silence_i32(
                    channels,
                    num_frames,
                    &mut silence_phase,
                    buffer,
                );
            } else {
                let written = super::dop_pack::fill_dop_period_i32(
                    channels,
                    num_frames,
                    &mut consumer,
                    &mut silence_phase,
                    buffer,
                );
                if written > 0 {
                    cb_shared
                        .samples_played
                        .fetch_add(written, Ordering::Relaxed);
                }
            }
            Ok(())
        })
        .map_err(|e| AppError::Audio(format!("coreaudio set render callback: {e}")))?;

    audio_unit
        .initialize()
        .map_err(|e| AppError::Audio(format!("coreaudio initialize: {e}")))?;

    shared.sample_rate.store(dop.sample_rate, Ordering::Release);
    shared.channels.store(dop.channels, Ordering::Release);

    audio_unit
        .start()
        .map_err(|e| AppError::Audio(format!("coreaudio start: {e}")))?;

    tracing::info!(
        device_id,
        sample_rate = dop.sample_rate,
        channels,
        "coreaudio dop stream opened"
    );

    // Init succeeded — tell the spawn call, then park until teardown.
    let _ = init_tx.send(Ok(()));
    let _ = shutdown_rx.recv();

    // Stop + drop the unit here (frees the render callback + its captured
    // ring consumer) before the caller releases hog mode.
    let _ = audio_unit.stop();
    drop(audio_unit);
    Ok(())
}

/// Resolve the persisted device name to a CoreAudio device id, or the
/// default output when unset / not found.
fn resolve_device(device_name: &Option<String>) -> Option<AudioDeviceID> {
    if let Some(name) = device_name.as_deref().filter(|n| !n.is_empty()) {
        if let Some(id) = get_device_id_from_name(name, false) {
            return Some(id);
        }
        tracing::warn!(
            requested = name,
            "coreaudio: requested device not found, using default output"
        );
    }
    get_default_device_id(false)
}

/// Take hog mode (exclusive access). Errors if another process already
/// owns it or the toggle didn't land on us — the caller then falls back
/// to DSD → PCM instead of fighting for the device.
fn acquire_hog(device_id: AudioDeviceID) -> AppResult<()> {
    let our_pid = std::process::id() as i32;
    // If it's already ours (shouldn't happen on a fresh open), don't
    // toggle it off. Otherwise take it and verify we won.
    match get_hogging_pid(device_id) {
        Ok(pid) if pid == our_pid => Ok(()),
        _ => {
            let owner = toggle_hog_mode(device_id)
                .map_err(|e| AppError::Audio(format!("coreaudio hog mode: {e}")))?;
            if owner == our_pid {
                Ok(())
            } else {
                Err(AppError::Audio(format!(
                    "coreaudio: device is held exclusively by another process (pid {owner})"
                )))
            }
        }
    }
}

/// Release hog mode if we still own it. Best-effort — logged, never fatal.
fn release_hog(device_id: AudioDeviceID) {
    let our_pid = std::process::id() as i32;
    if let Ok(pid) = get_hogging_pid(device_id) {
        if pid == our_pid {
            if let Err(err) = toggle_hog_mode(device_id) {
                tracing::warn!(%err, "coreaudio: failed to release hog mode");
            }
        }
    }
}
