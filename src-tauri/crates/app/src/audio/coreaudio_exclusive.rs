//! macOS-only CoreAudio hog-mode output backend (#495 / #497 for DoP,
//! then ordinary PCM).
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
//! **Ordinary PCM** goes through [`open_and_run_pcm`] instead, and the
//! difference is what it does *not* do: it takes hog mode and leaves the
//! device's physical format exactly as it found it. Re-clocking a device
//! the whole machine shares is a price only a marker cadence justifies;
//! for PCM the decoder meets the device instead. A failed open there
//! falls back to cpal shared mode.
//!
//! Same SPSC ring contract as the other backends (`Producer<f32>` →
//! `Consumer<f32>`, words carried as `f32` bit patterns).

use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::macos_helpers::{
    audio_unit_from_device_id_uninitialized, find_matching_physical_format, get_default_device_id,
    get_device_id_from_name, get_hogging_pid, set_device_physical_stream_format, toggle_hog_mode,
    AliveListener,
};
use coreaudio::audio_unit::render_callback::{data, Args};
use coreaudio::audio_unit::{Element, SampleFormat, Scope, StreamFormat};
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use objc2_core_audio::{
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioStreamPropertyPhysicalFormat, AudioDeviceID, AudioObjectGetPropertyData,
    AudioObjectPropertyAddress,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;

use super::output::{DopFormat, OutputHandle, RING_CAPACITY};
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

/// How often the parked output thread wakes to look at its device. The
/// loss itself arrives by push (see [`AliveListener`]) — this only sets
/// how fast a sleeping thread notices the flag, so it can stay lazy.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Spawn the CoreAudio DoP output thread. Mirrors the other exclusive
/// backends' contract: returns the decoder-side `Producer<f32>` and an
/// [`OutputHandle`], or an error (device busy / hog denied / format
/// unsupported) surfaced synchronously so the caller can fall back to
/// DSD → PCM.
pub fn spawn_coreaudio_exclusive_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
    dop: Option<DopFormat>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<Option<String>>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join: JoinHandle<()> = std::thread::Builder::new()
        .name(match dop {
            Some(_) => "waveflow-coreaudio-dop".into(),
            None => "waveflow-coreaudio-exclusive".to_string(),
        })
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
        .map_err(|e| AppError::Audio(format!("spawn coreaudio exclusive thread: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(opened_device)) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                device_name,
                opened_device,
                // Hog mode means the system stops mixing anything
                // else into this device — the same ownership WASAPI
                // and ALSA report. It used to say `false` here, which
                // had the pipeline panel deny a grab that had happened.
                exclusive: true,
                dop,
            },
        )),
        Ok(Err(err)) => {
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "coreaudio exclusive thread died before reporting init result".into(),
        )),
    }
}

fn output_thread_main(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<Option<String>>>,
    app: AppHandle,
    device_name: Option<String>,
    dop: Option<DopFormat>,
) {
    // `resolve_device` hands back the name only when the pin matched; a
    // fallback to the default output reports `None`, which the picker
    // shows as unknown rather than as the pin (#612).
    let (device_id, opened_device) = match resolve_device(&device_name) {
        Some(resolved) => resolved,
        None => {
            let _ = init_tx.send(Err(AppError::Audio(
                "coreaudio: no output device found".into(),
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
    let result = match dop {
        Some(dop) => open_and_run(
            &shared,
            consumer,
            &shutdown_rx,
            &init_tx,
            device_id,
            opened_device,
            dop,
        ),
        None => open_and_run_pcm(
            &shared,
            consumer,
            &shutdown_rx,
            &init_tx,
            device_id,
            opened_device,
        ),
    };

    release_hog(device_id);

    match result {
        Ok(ExitReason::Shutdown) => tracing::debug!("coreaudio exclusive output thread exiting"),
        // Same contract as the WASAPI / ALSA backends (#405): a device
        // that vanishes mid-stream must be reported, or the engine keeps
        // a handle to a thread that no longer feeds anything and the UI
        // stays stuck showing "DSD natif".
        Ok(ExitReason::DeviceLost(reason)) => {
            tracing::warn!(
                %reason,
                "coreaudio exclusive output thread lost the device; requesting rebuild"
            );
            super::output::notify_device_lost(
                &app,
                &shared,
                format!("audio device error: {reason}"),
            );
            super::output::schedule_device_rebuild(&app, super::output::RebuildTarget::Resolve);
        }
        Err(err) => tracing::warn!(%err, "coreaudio exclusive output thread stopped on error"),
    }
}

/// Why the render loop returned — same distinction as the other exclusive
/// backends: a clean shutdown must NOT trigger a recovery, a device loss
/// must.
enum ExitReason {
    Shutdown,
    DeviceLost(String),
}

/// The stateful body, factored out so `output_thread_main` can always run
/// [`release_hog`] afterwards regardless of how this returns. If it errors
/// before `init_tx` was signalled, it reports the failure so the spawn
/// call can fall back; after a successful start it parks until shutdown.
fn open_and_run(
    shared: &Arc<SharedPlayback>,
    mut consumer: Consumer<f32>,
    shutdown_rx: &Receiver<()>,
    init_tx: &Sender<AppResult<Option<String>>>,
    device_id: AudioDeviceID,
    opened_device: Option<String>,
    dop: DopFormat,
) -> AppResult<ExitReason> {
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
    // Remember what the device was set to *before* touching it. Pinning
    // the physical format is a change to the device, not to our stream:
    // without this, quitting a DoP track leaves every other app on the
    // machine talking to a DAC clocked at 352.8 kHz. The guard puts it
    // back on every exit path, including the `?`s below.
    //
    // A read failure aborts the whole DoP open rather than proceeding
    // with nothing to restore: we'd be re-clocking the user's device
    // with no way to undo it. Failing here is the ordinary fail-soft
    // path — the engine falls back to DSD → PCM, same as for any other
    // refused DoP negotiation.
    let previous_format = read_physical_stream_format(device_id).map_err(|e| {
        AppError::Audio(format!(
            "coreaudio: can't read the device's current physical format ({e}) — refusing to \
             re-clock it with no way back"
        ))
    })?;
    set_device_physical_stream_format(device_id, asbd)
        .map_err(|e| AppError::Audio(format!("coreaudio set physical format: {e}")))?;
    // Declared after the AudioUnit would be wrong: locals drop in reverse
    // order, and the unit has to stop before the device is re-clocked.
    let _format_guard = PhysicalFormatGuard {
        device_id,
        previous: Some(previous_format),
    };

    // Build the output AudioUnit bound to the device (uninitialized so we
    // can set the client format before init).
    let mut audio_unit = audio_unit_from_device_id_uninitialized(device_id, false)
        .map_err(|e| AppError::Audio(format!("coreaudio audio unit: {e}")))?;
    audio_unit
        .set_stream_format(stream_format, Scope::Input, Element::Output)
        .map_err(|e| AppError::Audio(format!("coreaudio set stream format: {e}")))?;

    // Render callback: bit-exact DoP, no DSP. Runs on CoreAudio's
    // realtime thread — only ring pops + atomic ops, no alloc/lock.
    let mut marker_phase: u64 = 0;
    let cb_shared = shared.clone();
    audio_unit
        .set_render_callback(move |args: Args<data::Interleaved<i32>>| {
            let Args {
                data: data::Interleaved {
                    buffer, channels, ..
                },
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
                    &mut marker_phase,
                    buffer,
                );
            } else {
                let written = super::dop_pack::fill_dop_period_i32(
                    channels,
                    num_frames,
                    &mut consumer,
                    &mut marker_phase,
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
    let _ = init_tx.send(Ok(opened_device));

    let exit = park_until_the_device_goes(shutdown_rx, device_id);

    // Stop + drop the unit here (frees the render callback + its captured
    // ring consumer) before the caller releases hog mode.
    let _ = audio_unit.stop();
    drop(audio_unit);
    Ok(exit)
}

/// The ordinary-PCM half: hog the device and feed it `f32` at whatever
/// rate it is already running.
///
/// Deliberately lighter than [`open_and_run`]. DoP has to pin the
/// device's *physical* format, because a marker cadence that gets
/// resampled is noise; ordinary PCM does not, and pinning it would
/// re-clock a device the rest of the machine shares for no gain — our
/// decoder can meet the device instead. So this takes the rate and the
/// channel count the device already has, publishes them, and lets the
/// resampler do the meeting. What exclusive buys here is hog mode: the
/// system stops mixing anything else in.
fn open_and_run_pcm(
    shared: &Arc<SharedPlayback>,
    mut consumer: Consumer<f32>,
    shutdown_rx: &Receiver<()>,
    init_tx: &Sender<AppResult<Option<String>>>,
    device_id: AudioDeviceID,
    opened_device: Option<String>,
) -> AppResult<ExitReason> {
    // Read, don't set: this is the device's own current format, and the
    // whole point of the PCM path is that we leave it alone.
    let current = read_physical_stream_format(device_id).map_err(|e| {
        AppError::Audio(format!(
            "coreaudio: can't read the device's current physical format ({e})"
        ))
    })?;

    // A device reporting neither is one we can't describe to the
    // decoder. Falling back to invented numbers would publish a rate the
    // hardware isn't running at, and every track would play at the wrong
    // speed rather than not at all.
    let sample_rate = current.mSampleRate;
    if !(sample_rate.is_finite() && sample_rate > 0.0) {
        return Err(AppError::Audio(format!(
            "coreaudio: device reports a {sample_rate} Hz format"
        )));
    }
    let channels = current.mChannelsPerFrame;
    if channels == 0 {
        return Err(AppError::Audio(
            "coreaudio: device reports zero channels".into(),
        ));
    }

    let stream_format = StreamFormat {
        sample_rate,
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
        channels,
    };

    let mut audio_unit = audio_unit_from_device_id_uninitialized(device_id, false)
        .map_err(|e| AppError::Audio(format!("coreaudio audio unit: {e}")))?;
    audio_unit
        .set_stream_format(stream_format, Scope::Input, Element::Output)
        .map_err(|e| AppError::Audio(format!("coreaudio set stream format: {e}")))?;

    // Runs on CoreAudio's realtime thread: ring pops and atomics only,
    // no allocation and no locking.
    let cb_shared = shared.clone();
    audio_unit
        .set_render_callback(move |args: Args<data::Interleaved<f32>>| {
            let Args {
                data: data::Interleaved {
                    buffer, channels, ..
                },
                num_frames,
                ..
            } = args;
            // Never trust the two to agree: a shorter buffer than the
            // frame count implies would panic on the slice, on the one
            // thread that must not.
            let wanted = num_frames.saturating_mul(channels).min(buffer.len());
            let buffer = &mut buffer[..wanted];

            let paused = cb_shared.paused_output.load(Ordering::Acquire);
            let draining = cb_shared.drain_silent.load(Ordering::Acquire);
            if paused || draining {
                if draining {
                    while consumer.pop().is_ok() {}
                }
                buffer.fill(0.0);
            } else {
                let written =
                    super::output::fill_pcm_period(&cb_shared, &mut consumer, buffer, channels);
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

    // Published as integers because that is what the rest of the engine
    // speaks; CoreAudio carries the rate as a float, and a device on a
    // fractional rate would be rounded here rather than silently
    // mismatched later.
    shared
        .sample_rate
        .store(sample_rate.round() as u32, Ordering::Release);
    shared.channels.store(channels as u16, Ordering::Release);

    audio_unit
        .start()
        .map_err(|e| AppError::Audio(format!("coreaudio start: {e}")))?;

    tracing::info!(
        device_id,
        sample_rate,
        channels,
        "coreaudio exclusive stream opened"
    );

    let _ = init_tx.send(Ok(opened_device));

    let exit = park_until_the_device_goes(shutdown_rx, device_id);

    // Stop + drop the unit here (frees the render callback and the ring
    // consumer it captured) before the caller releases hog mode.
    let _ = audio_unit.stop();
    drop(audio_unit);
    Ok(exit)
}

/// Park until the engine asks us to stop, or the device goes away.
///
/// CoreAudio doesn't error into our thread when the DAC is unplugged: it
/// simply stops pulling the render callback, which would leave us parked
/// forever on a dead output. `AliveListener` subscribes to the device's
/// `IsAlive` property, so the loss arrives as a flag flip and the wait
/// below only decides how soon we look at it.
///
/// The listener registers a pointer to itself, so it must not move once
/// registered — it stays a local of this frame, and its `Drop`
/// unregisters.
fn park_until_the_device_goes(shutdown_rx: &Receiver<()>, device_id: AudioDeviceID) -> ExitReason {
    let mut alive = AliveListener::new(device_id);
    let watching = match alive.register() {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(
                %err,
                "coreaudio: no device-alive listener; falling back to polling the device object"
            );
            false
        }
    };

    loop {
        match shutdown_rx.recv_timeout(DEVICE_POLL_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break ExitReason::Shutdown,
            Err(RecvTimeoutError::Timeout) => {
                let gone = if watching {
                    !alive.is_alive()
                } else {
                    // No listener: a property query on a removed device
                    // fails, which is the next best signal.
                    get_hogging_pid(device_id).is_err()
                };
                if gone {
                    break ExitReason::DeviceLost("coreaudio device is no longer alive".into());
                }
            }
        }
    }
}

/// Puts the device's physical stream format back when the DoP stream
/// goes away — whether that's a clean stop, a device loss, or a `?` on
/// one of the AudioUnit calls.
///
/// Restoring is best-effort and never fatal: the stream is already gone
/// by the time this runs, and a device that refuses the old format
/// (unplugged, taken by someone else) is not something we can act on.
struct PhysicalFormatGuard {
    device_id: AudioDeviceID,
    previous: Option<AudioStreamBasicDescription>,
}

impl Drop for PhysicalFormatGuard {
    fn drop(&mut self) {
        let Some(previous) = self.previous.take() else {
            return;
        };
        match set_device_physical_stream_format(self.device_id, previous) {
            Ok(()) => tracing::debug!("coreaudio: physical format restored"),
            Err(err) => {
                tracing::warn!(%err, "coreaudio: couldn't restore the device's physical format")
            }
        }
    }
}

/// Read the device's *current* physical stream format.
///
/// `coreaudio-rs` 0.14.2 can set this property and can list the formats a
/// device supports, but never hands back the one in force — its setter
/// reads it internally and drops it. So the only way to restore what we
/// found is to make the same HAL call ourselves.
fn read_physical_stream_format(
    device_id: AudioDeviceID,
) -> Result<AudioStreamBasicDescription, String> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioStreamPropertyPhysicalFormat,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut asbd = std::mem::MaybeUninit::<AudioStreamBasicDescription>::zeroed();
    // `ioDataSize` is in *and* out — CoreAudio writes the size it actually
    // produced back through this pointer, so it has to come from a unique
    // borrow. (Upstream `coreaudio-rs` hands it a `&`; writing through a
    // pointer derived from a shared borrow is UB whether or not it
    // currently miscompiles.)
    let mut data_size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;

    // SAFETY: `address` and `data_size` outlive the call; the destination
    // is a correctly sized and aligned ASBD slot, which CoreAudio fills
    // entirely when it reports success. `assume_init` only runs on a
    // zero status, and the slot is zeroed either way.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut data_size),
            NonNull::from(&mut asbd).cast(),
        )
    };
    if status != 0 {
        return Err(format!(
            "AudioObjectGetPropertyData(kAudioStreamPropertyPhysicalFormat) failed: {status}"
        ));
    }
    Ok(unsafe { asbd.assume_init() })
}

/// Resolve the persisted device name to a CoreAudio device id, plus the
/// name of what actually opened: `Some(pin)` when the pinned name
/// matched, `None` when we fell through to the default output, which we
/// have no name for here.
///
/// The picker needs that distinction — ticking the pin after a silent
/// fallback described a device playing nothing (#612).
fn resolve_device(device_name: &Option<String>) -> Option<(AudioDeviceID, Option<String>)> {
    if let Some(name) = device_name.as_deref().filter(|n| !n.is_empty()) {
        if let Some(id) = get_device_id_from_name(name, false) {
            return Some((id, Some(name.to_string())));
        }
        tracing::warn!(
            requested = name,
            "coreaudio: requested device not found, using default output"
        );
    }
    get_default_device_id(false).map(|id| (id, None))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Runtime smoke probe against the real default output device: proves
    /// the CoreAudio FFI actually executes (compile ≠ runs) and reports
    /// which DoP rates the device can negotiate in 32-bit. Read-only — it
    /// never takes hog mode. Ignored by default (needs macOS + a device);
    /// run with:
    ///   cargo test -p waveflow coreaudio_dop_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn coreaudio_dop_probe() {
        let dev = get_default_device_id(false).expect("a default output device");
        let hog = get_hogging_pid(dev).unwrap_or(-99);
        println!("default output device id = {dev}, current hog pid = {hog}");

        // The property read the restore path depends on. Printing it
        // proves the raw HAL call actually executes and returns a
        // plausible description, not just that it compiles.
        match read_physical_stream_format(dev) {
            Ok(asbd) => println!(
                "current physical format: {} Hz, {} bits, {} ch, format id {:#x}",
                asbd.mSampleRate, asbd.mBitsPerChannel, asbd.mChannelsPerFrame, asbd.mFormatID
            ),
            Err(err) => panic!("reading the physical format failed: {err}"),
        }

        // The alive listener the device-loss path depends on.
        let mut alive = AliveListener::new(dev);
        alive.register().expect("registering the alive listener");
        assert!(alive.is_alive(), "a present device reports alive");
        println!("alive listener registered, device reports alive");
        for rate in [176_400u32, 352_800, 705_600] {
            let sf = StreamFormat {
                sample_rate: rate as f64,
                sample_format: SampleFormat::I32,
                flags: LinearPcmFlags::IS_SIGNED_INTEGER | LinearPcmFlags::IS_PACKED,
                channels: 2,
            };
            let ok = find_matching_physical_format(dev, sf).is_some();
            println!(
                "DoP {rate} Hz / S32 int / 2ch: {}",
                if ok { "SUPPORTED" } else { "not available" }
            );
        }
    }
}
