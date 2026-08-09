//! Linux-only ALSA hardware-exclusive DoP output backend (#495).
//!
//! The Linux equivalent of [`super::wasapi_exclusive`]: it opens the DAC
//! as a **raw `hw:` device** (never `default` / `plughw:` / a Pulse or
//! PipeWire alias), which bypasses the system mixer + resampler and gives
//! us exclusive, bit-perfect access — the only way a DoP marker cadence
//! survives to the DAC. The stream is opened at the exact DoP rate
//! (`dsd_rate / 16`) in **`S32_LE`**, and each 24-bit DoP word is placed
//! MSB-justified in the 32-bit sample (marker in the top byte) via
//! [`super::dop_pack::fill_dop_period_i32`].
//!
//! If the DAC won't accept `S32_LE` at the DoP rate, or another client
//! already holds the `hw:` device, the open fails and the engine falls
//! back to the ordinary DSD → PCM path (through cpal shared).
//!
//! Same SPSC ring contract as the other backends (`Producer<f32>` →
//! `Consumer<f32>`, words carried as `f32` bit patterns), so the decoder
//! doesn't know which backend is draining the ring.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

use alsa::pcm::{Access, Format, HwParams, State, PCM};
use alsa::{Direction, ValueOr};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;

use super::output::{DopFormat, OutputHandle, RING_CAPACITY};
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

/// Spawn the ALSA DoP output thread. Mirrors
/// [`super::wasapi_exclusive::spawn_exclusive_output_thread`]'s contract:
/// returns the decoder-side `Producer<f32>` and an [`OutputHandle`], or an
/// error (device busy / format unsupported / no such device) surfaced
/// synchronously so the caller can fall back to DSD → PCM.
pub fn spawn_alsa_dop_output_thread(
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
        .name("waveflow-alsa-dop".into())
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
        .map_err(|e| AppError::Audio(format!("spawn alsa dop thread: {e}")))?;

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
            "alsa dop thread died before reporting init result".into(),
        )),
    }
}

/// Why the render loop returned — same distinction as the WASAPI backend:
/// a clean shutdown must NOT trigger a recovery, a device loss must.
enum ExitReason {
    Shutdown,
    DeviceLost(String),
}

fn output_thread_main(
    shared: Arc<SharedPlayback>,
    mut consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<()>>,
    app: AppHandle,
    device_name: Option<String>,
    dop: DopFormat,
) {
    let dev = resolve_hw_device(&device_name);
    let channels = dop.channels as usize;

    let (pcm, period_frames) = match open_pcm(&dev, dop) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(%err, device = %dev, "alsa dop init failed");
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    // `io_i32` borrows the PCM, so it lives in this frame alongside it.
    let io = match pcm.io_i32() {
        Ok(io) => io,
        Err(err) => {
            let _ = init_tx.send(Err(AppError::Audio(format!("alsa io_i32: {err}"))));
            return;
        }
    };

    shared.sample_rate.store(dop.sample_rate, Ordering::Release);
    shared.channels.store(dop.channels, Ordering::Release);
    let _ = init_tx.send(Ok(()));

    tracing::info!(
        device = %dev,
        sample_rate = dop.sample_rate,
        channels = dop.channels,
        period_frames,
        "alsa dop stream opened"
    );

    let mut buf: Vec<i32> = vec![0; period_frames * channels];
    let mut silence_phase: u64 = 0;

    let exit = loop {
        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }

        if shared.paused_output.load(Ordering::Acquire) {
            super::dop_pack::render_dop_silence_i32(channels, period_frames, &mut silence_phase, &mut buf);
        } else if shared.drain_silent.load(Ordering::Acquire) {
            while consumer.pop().is_ok() {}
            super::dop_pack::render_dop_silence_i32(channels, period_frames, &mut silence_phase, &mut buf);
        } else {
            let written = super::dop_pack::fill_dop_period_i32(
                channels,
                period_frames,
                &mut consumer,
                &mut silence_phase,
                &mut buf,
            );
            if written > 0 {
                shared.samples_played.fetch_add(written, Ordering::Relaxed);
            }
        }

        // Blocking write (the PCM was opened blocking). On XRUN / suspend
        // `try_recover` re-prepares the device; a hard failure is a device
        // loss.
        if let Err(err) = io.writei(&buf) {
            if shutdown_rx.try_recv().is_ok() {
                break ExitReason::Shutdown;
            }
            if let Err(rec) = pcm.try_recover(err, true) {
                tracing::warn!(?rec, "alsa dop write failed and recovery failed");
                break ExitReason::DeviceLost(format!("alsa write failed: {rec}"));
            }
            // Recovered — re-prepare if needed and continue.
            if pcm.state() == State::Setup {
                let _ = pcm.prepare();
            }
        }

        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }
    };

    // `io` borrows `pcm`; both drop here at end of scope. Dropping the
    // PCM calls `snd_pcm_close`, which stops the stream and releases the
    // exclusive `hw:` handle for the next opener.
    drop(io);

    match exit {
        ExitReason::Shutdown => {
            tracing::debug!("alsa dop output thread exiting");
        }
        ExitReason::DeviceLost(reason) => {
            tracing::warn!(%reason, "alsa dop output thread lost the device; requesting rebuild");
            super::output::notify_device_lost(&app, &shared, format!("audio device error: {reason}"));
            super::output::schedule_device_rebuild(&app, super::output::RebuildTarget::Resolve);
        }
    }
}

/// Map the persisted output name to a raw `hw:` device so we get
/// exclusive hardware access. A `default` / `plughw:` / `sysdefault:` /
/// Pulse alias would route through the mixer and resample the DoP marker
/// into noise, so we rewrite to `hw:` (keeping the `CARD=` selector when
/// present). An empty / unknown name falls back to the first card.
fn resolve_hw_device(name: &Option<String>) -> String {
    match name.as_deref().filter(|n| !n.is_empty()) {
        Some(n) if n.starts_with("hw:") => n.to_string(),
        Some(n) => {
            if let Some(idx) = n.find("CARD=") {
                format!("hw:{}", &n[idx..])
            } else {
                "hw:0,0".to_string()
            }
        }
        None => "hw:0,0".to_string(),
    }
}

/// Open the `hw:` device at the exact DoP format. Returns the PCM plus its
/// negotiated period size (frames). Any rejection (busy device, rate /
/// format unsupported) is an error → the caller falls back to DSD → PCM.
fn open_pcm(dev: &str, dop: DopFormat) -> AppResult<(PCM, usize)> {
    let pcm = PCM::new(dev, Direction::Playback, false)
        .map_err(|e| AppError::Audio(format!("alsa open {dev}: {e}")))?;

    {
        let hwp = HwParams::any(&pcm).map_err(|e| AppError::Audio(format!("alsa hwparams: {e}")))?;
        hwp.set_channels(dop.channels as u32)
            .map_err(|e| AppError::Audio(format!("alsa set_channels: {e}")))?;
        // DoP demands the exact rate — no resampling. `Nearest` lets ALSA
        // pick, then we verify below and bail if it deviated.
        hwp.set_rate(dop.sample_rate, ValueOr::Nearest)
            .map_err(|e| AppError::Audio(format!("alsa set_rate: {e}")))?;
        hwp.set_format(Format::S32LE)
            .map_err(|e| AppError::Audio(format!("alsa set_format S32_LE: {e}")))?;
        hwp.set_access(Access::RWInterleaved)
            .map_err(|e| AppError::Audio(format!("alsa set_access: {e}")))?;
        pcm.hw_params(&hwp)
            .map_err(|e| AppError::Audio(format!("alsa hw_params: {e}")))?;
    }

    let hwp = pcm
        .hw_params_current()
        .map_err(|e| AppError::Audio(format!("alsa hw_params_current: {e}")))?;
    let actual_rate = hwp
        .get_rate()
        .map_err(|e| AppError::Audio(format!("alsa get_rate: {e}")))?;
    if actual_rate != dop.sample_rate {
        return Err(AppError::Audio(format!(
            "alsa gave {actual_rate} Hz, DoP needs exactly {} Hz — device can't do this DoP rate",
            dop.sample_rate
        )));
    }
    let period_frames = hwp
        .get_period_size()
        .map_err(|e| AppError::Audio(format!("alsa get_period_size: {e}")))? as usize;
    if period_frames == 0 {
        return Err(AppError::Audio("alsa reported a zero period size".into()));
    }

    pcm.prepare()
        .map_err(|e| AppError::Audio(format!("alsa prepare: {e}")))?;

    Ok((pcm, period_frames))
}
