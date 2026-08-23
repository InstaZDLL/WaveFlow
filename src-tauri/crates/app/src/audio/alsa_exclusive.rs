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
//! If the DAC won't accept `S32_LE` at the DoP rate the open fails and
//! the engine falls back to the ordinary DSD → PCM path (through cpal
//! shared).
//!
//! A device held by *another client* is a different story, and used to
//! end the same way: on a desktop the holder is PipeWire or PulseAudio,
//! which grabbed the card at login, so DoP fell back on every machine
//! that had a sound server — silently. It now asks for the card through
//! [`super::device_reservation`] and retries; the fallback is what
//! happens when that is refused, not the first thing we do.
//!
//! Same SPSC ring contract as the other backends (`Producer<f32>` →
//! `Consumer<f32>`, words carried as `f32` bit patterns), so the decoder
//! doesn't know which backend is draining the ring.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
                dop: Some(dop),
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
    let dev = match resolve_hw_device(&device_name) {
        Ok(dev) => dev,
        Err(err) => {
            tracing::warn!(%err, "alsa dop: can't map the selected output to a hw: device");
            let _ = init_tx.send(Err(err));
            return;
        }
    };
    let channels = dop.channels as usize;

    // The reservation is bound alongside the PCM and dropped with it:
    // holding a card we are no longer playing on would keep the sound
    // server locked out of it.
    let (_reservation, pcm, period_frames) = match open_pcm(&dev, dop) {
        Ok((pcm, period_frames)) => (None, pcm, period_frames),
        Err(failure) if failure.busy => {
            // Someone else owns the card. On any desktop that is the
            // sound server, and the protocol below is how you ask it to
            // step aside; before this, the answer was always to give up.
            let reservation =
                hw_card_index(&dev).and_then(super::device_reservation::Reservation::acquire);
            let Some(reservation) = reservation else {
                tracing::warn!(
                    device = %dev,
                    "alsa dop: the card is busy and could not be reserved; falling back to DSD -> PCM"
                );
                let _ = init_tx.send(Err(failure.err));
                return;
            };
            match open_after_release(&dev, dop) {
                Ok((pcm, period_frames)) => (Some(reservation), pcm, period_frames),
                Err(failure) => {
                    tracing::warn!(
                        err = %failure.err,
                        device = %dev,
                        "alsa dop: the card stayed busy after the reservation"
                    );
                    let _ = init_tx.send(Err(failure.err));
                    return;
                }
            }
        }
        Err(failure) => {
            tracing::warn!(err = %failure.err, device = %dev, "alsa dop init failed");
            let _ = init_tx.send(Err(failure.err));
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
    let mut marker_phase: u64 = 0;

    let exit = 'run: loop {
        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }

        if shared.paused_output.load(Ordering::Acquire) {
            super::dop_pack::render_dop_silence_i32(
                channels,
                period_frames,
                &mut marker_phase,
                &mut buf,
            );
        } else if shared.drain_silent.load(Ordering::Acquire) {
            while consumer.pop().is_ok() {}
            super::dop_pack::render_dop_silence_i32(
                channels,
                period_frames,
                &mut marker_phase,
                &mut buf,
            );
        } else {
            let written = super::dop_pack::fill_dop_period_i32(
                channels,
                period_frames,
                &mut consumer,
                &mut marker_phase,
                &mut buf,
            );
            if written > 0 {
                shared.samples_played.fetch_add(written, Ordering::Relaxed);
            }
        }

        // Blocking write (the PCM was opened blocking), but `writei` is
        // still allowed to accept fewer frames than we offered — on a
        // signal, or after a recovery that swallowed part of the period.
        // The tail has to be re-offered rather than dropped: a hole in
        // the stream shifts every following frame against the DoP marker
        // cadence the DAC is locked onto. Only the *unwritten* remainder
        // is re-sent, never the frames the device already took.
        let mut frames_done = 0usize;
        while frames_done < period_frames {
            match io.writei(&buf[frames_done * channels..]) {
                Ok(0) => {
                    // A blocking device that reports no progress and no
                    // error has nothing left to recover from.
                    break 'run ExitReason::DeviceLost(
                        "alsa accepted 0 frames on a blocking write".into(),
                    );
                }
                Ok(n) => frames_done += n,
                Err(err) => {
                    if shutdown_rx.try_recv().is_ok() {
                        break 'run ExitReason::Shutdown;
                    }
                    if let Err(rec) = pcm.try_recover(err, true) {
                        tracing::warn!(?rec, "alsa dop write failed and recovery failed");
                        break 'run ExitReason::DeviceLost(format!("alsa write failed: {rec}"));
                    }
                    // Recovered — re-prepare if needed, then retry the tail.
                    if pcm.state() == State::Setup {
                        let _ = pcm.prepare();
                    }
                }
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
            super::output::notify_device_lost(
                &app,
                &shared,
                format!("audio device error: {reason}"),
            );
            super::output::schedule_device_rebuild(&app, super::output::RebuildTarget::Resolve);
        }
    }
}

/// Map the persisted output name to a raw `hw:` device so we get
/// exclusive hardware access. A `default` / `plughw:` / `sysdefault:` /
/// Pulse alias would route through the mixer and resample the DoP marker
/// into noise, so we rewrite to `hw:` (keeping the `CARD=` selector when
/// present, else resolving the friendly name against the card list).
///
/// Only an *absent* selection falls back to the first card. A name we
/// can't place is an error instead: opening card 0 because the user's
/// DAC didn't parse would send a DoP stream to some other device
/// entirely — better to fail here and let the engine play this track as
/// ordinary DSD → PCM on the device the user actually picked.
fn resolve_hw_device(name: &Option<String>) -> AppResult<String> {
    let Some(n) = name.as_deref().filter(|n| !n.is_empty()) else {
        return Ok("hw:0,0".to_string());
    };
    if n.starts_with("hw:") {
        return Ok(n.to_string());
    }
    // ALSA PCM names carry the card as a `CARD=` selector
    // ("sysdefault:CARD=D50s", "front:CARD=PCH,DEV=0") — keep it and
    // swap the plugin prefix for `hw:`.
    if let Some(idx) = n.find("CARD=") {
        return Ok(format!("hw:{}", &n[idx..]));
    }
    // No selector: a friendly name, or one of the system aliases.
    if let Some(index) = find_card_index(n) {
        return Ok(format!("hw:{index},0"));
    }
    if n.eq_ignore_ascii_case("default") {
        return Ok("hw:0,0".to_string());
    }
    Err(AppError::Audio(format!(
        "alsa: no card matches the selected output '{n}' — can't open it exclusively for DoP"
    )))
}

/// Find the ALSA card index whose short or long name matches `name`.
/// Long names are the verbose HAL strings ("Topping D50s at usb-…"), so
/// they're matched by containment; short names must match exactly.
fn find_card_index(name: &str) -> Option<i32> {
    alsa::card::Iter::new().flatten().find_map(|card| {
        let short_hit = card.get_name().is_ok_and(|n| n == name);
        let long_hit = card.get_longname().is_ok_and(|n| n.contains(name));
        (short_hit || long_hit).then(|| card.get_index())
    })
}

/// The ALSA card index behind a resolved `hw:` name.
///
/// The reservation protocol is keyed on the index, not on the name, so
/// `hw:CARD=D50s` has to be resolved back through the card list.
fn hw_card_index(dev: &str) -> Option<i32> {
    let first = dev.strip_prefix("hw:")?.split(',').next()?;
    if let Ok(index) = first.parse::<i32>() {
        return Some(index);
    }
    find_card_index(first.strip_prefix("CARD=")?)
}

/// A failed open, plus the one thing the caller has to branch on: a
/// device another client is holding can be asked for, a device that
/// can't do this DoP rate cannot.
struct PcmOpenError {
    busy: bool,
    err: AppError,
}

impl From<AppError> for PcmOpenError {
    fn from(err: AppError) -> Self {
        Self { busy: false, err }
    }
}

/// Retry the open while the sound server finishes letting go.
///
/// Releasing is asynchronous on its side — it sees `NameLost`, plays
/// out what it has buffered and only then closes the device — so the
/// first open after the reservation lands still returns `EBUSY`.
fn open_after_release(dev: &str, dop: DopFormat) -> Result<(PCM, usize), PcmOpenError> {
    const STEP: Duration = Duration::from_millis(50);
    let deadline = Instant::now() + super::device_reservation::RELEASE_GRACE;
    loop {
        // Try before waiting: a server that let go promptly costs
        // nothing, and a device that simply cannot do this DoP rate
        // says so on the first attempt.
        match open_pcm(dev, dop) {
            Ok(opened) => return Ok(opened),
            Err(failure) if failure.busy && Instant::now() < deadline => {
                std::thread::sleep(STEP);
            }
            Err(failure) => return Err(failure),
        }
    }
}

/// Open the `hw:` device at the exact DoP format. Returns the PCM plus its
/// negotiated period size (frames). A rejection (busy device, rate /
/// format unsupported) is an error → the caller either reserves the
/// card and retries, or falls back to DSD → PCM.
fn open_pcm(dev: &str, dop: DopFormat) -> Result<(PCM, usize), PcmOpenError> {
    let pcm = PCM::new(dev, Direction::Playback, false).map_err(|e| PcmOpenError {
        busy: e.errno() == libc::EBUSY,
        err: AppError::Audio(format!("alsa open {dev}: {e}")),
    })?;

    {
        let hwp =
            HwParams::any(&pcm).map_err(|e| AppError::Audio(format!("alsa hwparams: {e}")))?;
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

    // Scoped: `hw_params_current` borrows the PCM, and the borrow would
    // otherwise still be live at the `Ok((pcm, …))` move below.
    let period_frames = {
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
            ))
            .into());
        }
        hwp.get_period_size()
            .map_err(|e| AppError::Audio(format!("alsa get_period_size: {e}")))? as usize
    };
    if period_frames == 0 {
        return Err(AppError::Audio("alsa reported a zero period size".into()).into());
    }

    pcm.prepare()
        .map_err(|e| AppError::Audio(format!("alsa prepare: {e}")))?;

    Ok((pcm, period_frames))
}

#[cfg(test)]
mod tests {
    use super::hw_card_index;

    #[test]
    fn a_numeric_hw_name_yields_its_index() {
        assert_eq!(hw_card_index("hw:0,0"), Some(0));
        assert_eq!(hw_card_index("hw:3"), Some(3));
    }

    #[test]
    fn a_name_we_never_resolved_to_hw_has_no_index() {
        // `resolve_hw_device` only ever hands us `hw:` names, but the
        // reservation must not invent a card index from anything else.
        assert_eq!(hw_card_index("default"), None);
        assert_eq!(hw_card_index("plughw:1,0"), None);
    }

    #[test]
    fn a_card_selector_is_looked_up_and_not_parsed() {
        // `hw:CARD=…` carries a name, not an index — parsing it as a
        // number would reserve card 0 and hand the wrong device over.
        assert_eq!(hw_card_index("hw:CARD=NoSuchCardHere,DEV=0"), None);
    }
}
